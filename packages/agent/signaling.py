import asyncio
import json
import logging
from typing import Optional

import websockets

log = logging.getLogger(__name__)


class SignalingClient:
    """
    WebSocket client for darc-signal.

    Registers the agent as a robot, advertising all WebTransport candidates
    (host LAN IPs + STUN-reflexive public address) and the TLS fingerprint.
    The signal server relays these to connecting pilots.

    When a pilot connects, the signal server sends us their IP so we can
    punch a UDP hole in our NAT before the pilot's QUIC Initial arrives.

    Only presence/handshake messages pass through signal in phase 2.
    """

    def __init__(self, url: str, robot_id: str,
                 cert_fingerprint: str,
                 candidates: list[dict],
                 p2p_hint: str = 'likely'):
        self.url = url
        self.robot_id = robot_id
        self.cert_fingerprint = cert_fingerprint
        self.candidates = candidates  # [{url, label, priority, needsProbe}, ...]
        self.p2p_hint = p2p_hint      # 'likely' | 'lan-only' | 'none'
        self._ws = None
        self._pilot_connected = False

        self.on_pilot_connected: Optional[object] = None    # async (pilot_ip: str|None) -> None
        self.on_pilot_disconnected: Optional[object] = None # async () -> None
        self.on_relay_mode: Optional[object] = None          # async () -> None
        self.on_punch: Optional[object] = None               # async (pilot_ip: str|None) -> None
        self.on_qos: Optional[object] = None                 # async (profile: str) -> None

    async def run(self):
        backoff = 1
        while True:
            try:
                async with websockets.connect(self.url) as ws:
                    self._ws = ws
                    backoff = 1
                    await self._register()
                    async for raw in ws:
                        if isinstance(raw, bytes):
                            continue
                        await self._dispatch(json.loads(raw))
            except (asyncio.CancelledError, KeyboardInterrupt):
                raise
            except Exception as e:
                log.warning('signal disconnected: %s — reconnecting in %ds', e, backoff)
            finally:
                self._ws = None
                if self._pilot_connected:
                    self._pilot_connected = False
                    if self.on_pilot_disconnected:
                        await self.on_pilot_disconnected()
            await asyncio.sleep(backoff)
            backoff = min(backoff * 2, 30)

    async def send(self, msg: dict):
        if self._ws:
            try:
                await self._ws.send(json.dumps(msg))
            except Exception:
                pass

    async def send_binary_batch(self, chunks: list[bytes]):
        """Send one frame's chunks to the signal server (relay mode fallback)."""
        if not self._ws:
            return
        try:
            for data in chunks:
                await self._ws.send(data)
        except Exception:
            pass

    def pending_bytes(self) -> int:
        """
        Bytes buffered in the WebSocket transport.

        Fails open (0) — this reaches into a `websockets` internal, and a wrong
        guess here should degrade to always-send rather than never-send. Note
        that on TCP there is no way to discard what is already queued, so relay
        latency remains unbounded under sustained congestion regardless.
        """
        try:
            return self._ws.transport.get_write_buffer_size()
        except Exception:
            return 0

    async def _register(self):
        msg = {
            'type':            'register',
            'robotId':         self.robot_id,
            'certFingerprint': self.cert_fingerprint,
            'candidates':      self.candidates,
            'p2pHint':         self.p2p_hint,
        }
        await self.send(msg)
        labels = ', '.join(f"{c['label']}:{c['url']}" for c in self.candidates) or 'none'
        log.info('registered robot_id=%s candidates=[%s]', self.robot_id, labels)

    async def _dispatch(self, msg: dict):
        t = msg.get('type')
        if t == 'registered':
            log.info('registration confirmed by server')
        elif t == 'pilot-connected':
            pilot_ip = msg.get('pilotIp')
            log.info('pilot connecting from %s — punching NAT hole', pilot_ip or 'unknown')
            self._pilot_connected = True
            if self.on_pilot_connected:
                await self.on_pilot_connected(pilot_ip)
        elif t == 'peer-disconnected':
            log.info('pilot signaling disconnected')
            self._pilot_connected = False
            if self.on_pilot_disconnected:
                await self.on_pilot_disconnected()
        elif t == 'punch':
            # Pilot is retrying P2P while relay carries video. Punch only —
            # leaving the current video sinks alone, unlike pilot-connected.
            pilot_ip = msg.get('pilotIp')
            log.info('re-punching NAT for pilot at %s (P2P retry)', pilot_ip or 'unknown')
            if self.on_punch:
                await self.on_punch(pilot_ip)
        elif t == 'qos':
            # Routed through the signal server rather than the WebTransport
            # stream so it also reaches us in relay mode, where there is no
            # pilot→robot JSON path.
            profile = msg.get('profile')
            log.info('QoS profile requested via signalling: %s', profile)
            if self.on_qos:
                await self.on_qos(profile)
        elif t == 'relay-mode':
            log.info('relay mode requested — switching video to signal server')
            if self.on_relay_mode:
                await self.on_relay_mode()
