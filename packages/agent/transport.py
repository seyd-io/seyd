"""
darc-agent WebTransport server (aioquic-based).

Accepts a single WebTransport session at a time (last-connect wins).
Video frames are sent as QUIC datagrams (unreliable, lowest latency).
Sensor data and command acknowledgements travel over a JSON line-delimited
bidirectional stream initiated by the pilot.
Commands from the pilot arrive on the same stream and are dispatched via
the on_message callback.

The server does not bind its own sockets. The caller passes in sockets it has
already bound and already run STUN on, so the NAT mapping advertised to the
pilot is provably the same mapping QUIC will receive on — see stun.py. One
socket per address family: IPv4 and, where the host has a routable address,
IPv6.
"""

import asyncio
import json
import logging
import socket
from typing import Callable, Optional

from aioquic.asyncio.protocol import QuicConnectionProtocol
from aioquic.asyncio.server import QuicServer
from aioquic.h3.connection import H3Connection, H3_ALPN
from aioquic.h3.events import (
    DatagramReceived,
    HeadersReceived,
    WebTransportStreamDataReceived,
)
from aioquic.quic.configuration import QuicConfiguration
from aioquic.quic.events import QuicEvent

log = logging.getLogger(__name__)

_PATH = b'/darc'


class _Session:
    """One active WebTransport connection from a pilot."""

    def __init__(self, session_id: int, http: H3Connection, protocol: 'DARCProtocol'):
        self._session_id = session_id
        self._http = http
        self._protocol = protocol
        self._stream_id: Optional[int] = None
        self._buf = b''
        self.on_message: Optional[Callable] = None

    def pending_bytes(self) -> int:
        """
        Bytes queued inside aioquic but not yet on the wire.

        Note this grows for two different reasons: an exhausted congestion
        window, and aioquic's pacer merely spacing packets out. So a non-empty
        queue does not mean congestion — the caller must compare against a byte
        budget, not against zero.

        Fails open (returns 0, meaning "no backlog") so that a missing private
        attribute degrades to always-send rather than never-send.
        """
        try:
            return sum(len(d) for d in self._protocol._quic._datagrams_pending)
        except AttributeError:
            return 0

    def drop_pending(self):
        """
        Discard queued DATAGRAM frames.

        Only ever correct ahead of a keyframe: an IDR makes every queued chunk
        from the previous GOP irrelevant by definition. Doing this before a
        delta frame destroys a frame the pilot still needs and throws away
        uplink already spent on it.
        """
        try:
            self._protocol._quic._datagrams_pending.clear()
        except AttributeError:
            pass

    async def send_datagram_batch(self, chunks: list[bytes]):
        """
        Queue every chunk of one frame, then transmit once.

        Transmitting per chunk means a full packet-build/send pass for each
        1000-byte datagram — 40+ of them for a keyframe. At this chunk size
        nothing coalesces into a shared QUIC packet anyway, so batching is a
        pure CPU and jitter win with no change in what goes on the wire.
        """
        for data in chunks:
            self._http.send_datagram(self._session_id, data)
        self._protocol.transmit()

    def link_stats(self) -> dict:
        """Congestion window and smoothed RTT, for the telemetry channel."""
        try:
            loss = self._protocol._quic._loss
            return {'cwnd': loss.congestion_window,
                    'srtt_ms': round(loss._rtt_smoothed * 1000, 1)}
        except AttributeError:
            return {}

    async def send_json(self, msg: dict):
        if self._stream_id is None:
            return
        payload = (json.dumps(msg) + '\n').encode()
        self._protocol._quic.send_stream_data(self._stream_id, payload, end_stream=False)
        self._protocol.transmit()

    def stream_data_received(self, stream_id: int, data: bytes):
        if self._stream_id is None:
            self._stream_id = stream_id
        if stream_id != self._stream_id:
            return
        self._buf += data
        while b'\n' in self._buf:
            line, self._buf = self._buf.split(b'\n', 1)
            if not line:
                continue
            try:
                msg = json.loads(line)
            except json.JSONDecodeError:
                continue
            if self.on_message:
                asyncio.ensure_future(self.on_message(msg))


class DARCProtocol(QuicConnectionProtocol):

    def __init__(self, *args, server: 'WebTransportServer', **kwargs):
        super().__init__(*args, **kwargs)
        self._server = server
        self._http: Optional[H3Connection] = None
        self._session: Optional[_Session] = None

    def quic_event_received(self, event: QuicEvent):
        if self._http is None:
            self._http = H3Connection(self._quic, enable_webtransport=True)

        for http_event in self._http.handle_event(event):
            if isinstance(http_event, HeadersReceived):
                headers = dict(http_event.headers)
                if (headers.get(b':method') == b'CONNECT'
                        and headers.get(b':protocol') == b'webtransport'
                        and headers.get(b':path', b'').rstrip(b'/') == _PATH):
                    self._accept(http_event.stream_id)

            elif isinstance(http_event, WebTransportStreamDataReceived):
                if self._session:
                    self._session.stream_data_received(
                        http_event.stream_id, http_event.data
                    )

            elif isinstance(http_event, DatagramReceived):
                pass

    def _accept(self, stream_id: int):
        log.info('WebTransport session accepted (stream %d)', stream_id)
        self._http.send_headers(
            stream_id=stream_id,
            headers=[
                (b':status', b'200'),
                (b'sec-webtransport-http3-draft', b'draft02'),
            ],
        )
        self.transmit()

        session = _Session(session_id=stream_id, http=self._http, protocol=self)
        session.on_message = self._server.on_message
        self._session = session
        self._server._set_session(session)

    def connection_lost(self, exc):
        super().connection_lost(exc)
        if self._session and self._server._current_session is self._session:
            log.info('WebTransport session closed')
            self._server._set_session(None)
            self._session = None


class WebTransportServer:
    """
    Manages an aioquic WebTransport server.

    Callbacks (all async):
      on_connected(session)  — a pilot opened a WebTransport session
      on_disconnected()      — that session closed
      on_message(msg)        — JSON command received from the pilot
    """

    def __init__(self):
        self._current_session: Optional[_Session] = None
        self._quic_servers: list[QuicServer] = []
        self._socks: list[socket.socket] = []
        self._probe_task: Optional[asyncio.Task] = None
        self.on_connected: Optional[Callable] = None
        self.on_disconnected: Optional[Callable] = None
        self.on_message: Optional[Callable] = None

    async def start(self, socks: list[socket.socket], cert, key):
        """Run a QUIC server on each already-bound socket."""
        config = QuicConfiguration(
            alpn_protocols=H3_ALPN,
            is_client=False,
            max_datagram_frame_size=65536,
        )
        config.certificate = cert
        config.private_key = key

        loop = asyncio.get_running_loop()
        self._socks = list(socks)

        for sock in socks:
            # aioquic's serve() binds for us, which we cannot use here: these
            # sockets are already bound and already carry the NAT mapping STUN
            # measured. Drive QuicServer onto the existing socket instead.
            _, server = await loop.create_datagram_endpoint(
                lambda: QuicServer(
                    configuration=config,
                    create_protocol=lambda *a, **kw: DARCProtocol(*a, server=self, **kw),
                ),
                sock=sock,
            )
            self._quic_servers.append(server)
            family = 'IPv6' if sock.family == socket.AF_INET6 else 'IPv4'
            log.info('WebTransport server listening on %s %s', family, sock.getsockname()[:2])

    def _socket_for(self, ip: str) -> Optional[socket.socket]:
        """Pick the listening socket whose address family matches `ip`."""
        want = socket.AF_INET6 if ':' in ip else socket.AF_INET
        for sock in self._socks:
            if sock.family == want:
                return sock
        return None

    def start_probing(self, ip: str, port: int = 443,
                      interval: float = 0.25, duration: float = 12.0):
        """
        Repeatedly send a small UDP packet from the WebTransport socket toward
        the pilot's IP, so our NAT creates an outbound mapping and admits the
        pilot's inbound QUIC Initial.

        What this does and does not buy us, by agent-side NAT type:

          full-cone           already reachable via the STUN candidate; probe is
                              redundant but harmless
          address-restricted  WORKS — filtering is by source *address* only, so
                              the destination port we probe is irrelevant
          port-restricted     cannot work — the NAT would only admit traffic from
                              the exact ip:port we probed, and the browser picks a
                              random ephemeral source port we have no way to learn
          symmetric / CGNAT   cannot work — the external mapping differs per
                              destination, so the advertised STUN candidate is
                              already wrong

        Hence a single destination port, not a list of guesses: for the one NAT
        type this helps, the port does not matter, and for the type where it
        would matter the port is unknowable.

        Probing repeats rather than firing once because a lone UDP packet can be
        dropped, and because the mapping has to still be alive whenever the
        pilot's Initial actually arrives — Chrome retries its QUIC handshake with
        backoff, so that can be seconds after the pilot first tried.
        """
        self.stop_probing()
        self._probe_task = asyncio.ensure_future(
            self._probe_loop(ip, port, interval, duration)
        )

    def stop_probing(self):
        if self._probe_task and not self._probe_task.done():
            self._probe_task.cancel()
        self._probe_task = None

    async def _probe_loop(self, ip: str, port: int, interval: float, duration: float):
        sock = self._socket_for(ip)
        if not sock:
            log.warning('probe: no listening socket matches %s', ip)
            return
        loop     = asyncio.get_running_loop()
        deadline = loop.time() + duration
        sent     = 0
        try:
            while loop.time() < deadline:
                try:
                    sock.sendto(b'\x00', (ip, port))
                    sent += 1
                except Exception as e:
                    log.debug('probe to %s:%d failed: %s', ip, port, e)
                await asyncio.sleep(interval)
        except asyncio.CancelledError:
            pass
        finally:
            log.info('sent %d NAT probes to %s:%d', sent, ip, port)

    def stop(self):
        self.stop_probing()
        for server in self._quic_servers:
            try:
                server.close()
            except Exception:
                pass
        self._quic_servers.clear()
        self._socks.clear()

    def _set_session(self, session: Optional[_Session]):
        self._current_session = session
        if session is not None:
            # The hole is open and in use — no reason to keep probing.
            self.stop_probing()
            if self.on_connected:
                asyncio.ensure_future(self.on_connected(session))
        else:
            if self.on_disconnected:
                asyncio.ensure_future(self.on_disconnected())
