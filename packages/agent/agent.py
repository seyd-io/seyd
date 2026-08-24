"""
DARC Agent — robot-side relay daemon.

Subscribes to local UDP streams (RTP video on --video-port, sensor data on
--sensor-port) and relays them to a connected pilot over WebTransport (P2P).
Video is forwarded as QUIC datagrams (H.264 Annex B). Sensor data and commands
travel over a bidirectional JSON stream.

The signal server is used only for registration and connection handshaking.
Video and command data never pass through the signal server.

Usage:
    python agent.py --robot-id <id> --signal-url wss://<host>

Optional:
    --webtransport-port   UDP port for the WebTransport server (default: 4433)
    --webtransport-host   Override the public host sent to pilots (default: STUN)
    --video-port          RTP video input port (default: 5000)
    --sensor-port         Sensor UDP input port (default: 5002)
"""

import asyncio
import argparse
import json
import logging
import socket

import portmap
import qos
from cert import generate_cert
from stun import discover_nat, format_host, get_local_ips, is_ipv6
from transport import WebTransportServer
from signaling import SignalingClient
from peer import Relay

log = logging.getLogger(__name__)


class PublisherControl:
    """
    Outbound control channel to the robot's video publisher.

    DARC defines the message and the port; it does not implement the publisher.
    Per SPEC.md, DARC states a bitrate ceiling and a latency budget and the
    publisher decides how to meet them — so this carries targets, never
    resolutions or encoder flags.

    Best-effort UDP to localhost, fire-and-forget, never fatal: a robot whose
    publisher does not implement the interface must keep streaming on whatever
    it was already configured with.
    """

    def __init__(self, port: int, host: str = '127.0.0.1'):
        self._addr = (host, port)
        self._sock: socket.socket | None = None

    async def send(self, msg: dict) -> bool:
        if self._sock is None:
            try:
                self._sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
                self._sock.setblocking(False)
            except OSError as e:
                log.debug('publisher control socket unavailable: %s', e)
                return False
        try:
            self._sock.sendto(json.dumps(msg).encode(), self._addr)
            log.info('publisher control → %s', msg.get('type'))
            return True
        except OSError as e:
            log.debug('publisher control send failed: %s', e)
            return False


def parse_args():
    p = argparse.ArgumentParser(description='DARC Agent')
    p.add_argument('--robot-id',           required=True)
    p.add_argument('--signal-url',         required=True)
    p.add_argument('--webtransport-port',  type=int, default=4433)
    p.add_argument('--webtransport-host',  default=None,
                   help='Override public host for WebTransport (skips STUN)')
    p.add_argument('--video-port',         type=int, default=5000)
    p.add_argument('--sensor-port',        type=int, default=5002)
    p.add_argument('--no-port-mapping',    action='store_true',
                   help='Skip PCP/NAT-PMP/UPnP router port mapping')
    p.add_argument('--no-ipv6',            action='store_true',
                   help='Do not listen on or advertise IPv6')
    p.add_argument('--qos-profile',        default=qos.DEFAULT,
                   choices=sorted(qos.PROFILES),
                   help=f'Starting QoS profile (default: {qos.DEFAULT})')
    p.add_argument('--publisher-control-port', type=int, default=5003,
                   help='UDP port the video publisher listens on for config')
    return p.parse_args()


def bind_sockets(port: int, want_ipv6: bool) -> list[socket.socket]:
    """
    Bind the UDP sockets the QUIC server will use.

    These are bound here, up front, because STUN has to run on the very socket
    that later receives QUIC — binding a temporary socket and letting aioquic
    rebind the port makes the advertised reflexive address a guess. Two separate
    sockets rather than one dual-stack socket: V6ONLY keeps the IPv6 listener
    from colliding with the IPv4 one on the same port, and it keeps the IPv4
    path byte-identical to what is already known to work.
    """
    socks: list[socket.socket] = []

    s4 = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s4.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s4.bind(('0.0.0.0', port))
    s4.setblocking(False)
    socks.append(s4)

    if want_ipv6:
        try:
            s6 = socket.socket(socket.AF_INET6, socket.SOCK_DGRAM)
            s6.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            s6.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 1)
            s6.bind(('::', port))
            s6.setblocking(False)
            socks.append(s6)
        except OSError as e:
            log.info('IPv6 listener unavailable (%s) — continuing IPv4-only', e)

    return socks


async def gather_candidates(args, socks) -> tuple[list[dict], list[str], str]:
    """
    Work out every address a pilot could reach us on.

    Returns (candidates, san_ips, p2p_hint). Candidates carry a `priority` so
    the pilot can order its attempts, and `needsProbe` so it knows which ones
    depend on a NAT hole being punched first and must not be fired too early.
    """
    wt_port = args.webtransport_port
    candidates: list[dict] = []
    san_ips: list[str] = []

    def add(ip: str, port: int, label: str, priority: int, needs_probe: bool):
        url = f'https://{format_host(ip)}:{port}/darc'
        if any(c['url'] == url for c in candidates):
            return
        candidates.append({'url': url, 'label': label,
                           'priority': priority, 'needsProbe': needs_probe})
        if ip not in san_ips:
            san_ips.append(ip)
        log.info('candidate [%-8s prio %3d] %s', label, priority, url)

    if args.webtransport_host:
        add(args.webtransport_host, wt_port, 'host-override', 250, False)
        return candidates, san_ips, 'likely'

    # ── host candidates ──────────────────────────────────────────────────────
    # A LAN address wins instantly when the pilot is on the same network, and a
    # global IPv6 address has no NAT in front of it at all — both are reachable
    # without any hole punching, so the pilot can try them immediately.
    have_global_v6 = False
    for ip in get_local_ips():
        if is_ipv6(ip):
            have_global_v6 = True
            add(ip, wt_port, 'host6', 200, True)   # firewall pinhole still helps
        else:
            add(ip, wt_port, 'host', 240, False)

    # ── router port mapping ──────────────────────────────────────────────────
    # Explicitly asking the router to forward the port beats inferring a mapping
    # from STUN: it also covers port-restricted and many symmetric NATs.
    mapped = None
    if not args.no_port_mapping:
        mapped = await portmap.map_port(wt_port)
        # Only advertise it if the router's external address is actually on the
        # public internet — under CGNAT it will report one that isn't, and a
        # candidate nobody can route to just burns a slot in the pilot's race.
        if mapped and mapped.routable:
            add(mapped.external_ip, mapped.external_port, 'portmap', 220, False)
        elif mapped:
            mapped = None

    # ── server-reflexive candidate ───────────────────────────────────────────
    nat = await discover_nat(socks[0])
    if nat.reflexive:
        add(nat.reflexive[0], nat.reflexive[1], 'srflx', 150, True)
    elif nat.symmetric:
        # Advertising it would just burn a slot in the pilot's race: under a
        # symmetric NAT the external port is chosen per destination, so the one
        # STUN saw is not the one the pilot's packets would arrive on.
        log.info('skipping reflexive candidate — %s NAT', nat.nat_type)

    # How hopeful should the pilot be? This drives how long it waits before
    # giving up on P2P, so that a definitely-doomed attempt fails fast while a
    # plausible one gets the full window.
    if mapped or nat.reflexive or have_global_v6:
        hint = 'likely'
    elif candidates:
        hint = 'lan-only'   # only host candidates; works iff pilot shares the LAN
    else:
        hint = 'none'

    log.info('NAT type: %s — P2P outlook: %s', nat.nat_type, hint)
    return candidates, san_ips, hint


async def main():
    args = parse_args()
    logging.basicConfig(
        level=logging.INFO,
        format='%(asctime)s %(levelname)-8s %(message)s',
        datefmt='%H:%M:%S',
    )
    wt_port = args.webtransport_port

    # ── Step 1: Bind the sockets QUIC will use ───────────────────────────────
    # Everything downstream measures and advertises *these* sockets, so nothing
    # rebinds the port later and invalidates what we told the pilot.
    socks = bind_sockets(wt_port, want_ipv6=not args.no_ipv6)

    # ── Step 2: Discover every reachable address ─────────────────────────────
    candidates, all_ips, p2p_hint = await gather_candidates(args, socks)
    if not candidates:
        log.error('no WebTransport candidates — agent will register as unreachable')

    # ── Step 3: TLS cert covering all of them ────────────────────────────────
    # Generated after discovery so every advertised IP lands in the SAN
    # extension, which Chrome requires for serverCertificateHashes to verify.
    cert, key, fingerprint = generate_cert(all_ips)
    log.info('cert fingerprint: %s…  (SAN IPs: %s)', fingerprint[:16], ', '.join(all_ips))

    # ── Step 4: Start relay + WebTransport server ─────────────────────────────
    publisher = PublisherControl(args.publisher_control_port)
    relay = Relay(video_port=args.video_port, sensor_port=args.sensor_port,
                  profile=qos.get(args.qos_profile))
    wt    = WebTransportServer()

    def detach_sinks():
        relay.send_batch    = None
        relay.send_json     = None
        relay.pending_bytes = None
        relay.drop_pending  = None
        relay.link_stats    = None

    async def on_wt_connected(session):
        relay.send_batch    = session.send_datagram_batch
        relay.send_json     = session.send_json
        relay.pending_bytes = session.pending_bytes
        relay.drop_pending  = session.drop_pending
        relay.link_stats    = session.link_stats
        log.info('pilot connected via WebTransport')

    async def on_wt_disconnected():
        detach_sinks()
        log.info('pilot disconnected from WebTransport')

    async def on_qos(profile_name: str) -> dict:
        """Apply a profile: DARC's half immediately, the publisher's best-effort."""
        profile = qos.get(profile_name)
        relay.set_profile(profile)
        delivered = await publisher.send(profile.publisher_config())
        return {
            'type':      'qos-applied',
            'profile':   profile.name,
            'fec':       {'delta': profile.fec_delta_pct, 'key': profile.fec_key_pct},
            'pilot':     profile.pilot_config(),
            # 'requested', not 'applied' — the control channel is fire-and-forget
            # UDP, so claiming the publisher applied it would be a guess.
            'publisher': 'requested' if delivered else 'unavailable',
        }

    wt.on_connected    = on_wt_connected
    wt.on_disconnected = on_wt_disconnected
    wt.on_message      = relay.handle_message
    relay.on_qos       = on_qos

    await relay.start()
    await wt.start(socks, cert=cert, key=key)

    # ── Step 5: Signaling ─────────────────────────────────────────────────────
    signaling = SignalingClient(
        url=args.signal_url,
        robot_id=args.robot_id,
        cert_fingerprint=fingerprint,
        candidates=candidates,
        p2p_hint=p2p_hint,
    )

    async def on_pilot_connected(pilot_ip: str | None):
        if pilot_ip:
            wt.start_probing(pilot_ip)
        detach_sinks()

    async def on_pilot_disconnected():
        wt.stop_probing()
        detach_sinks()

    async def on_punch(pilot_ip: str | None):
        # P2P retry from a pilot already on relay. Only reopen the hole; the
        # video sinks must keep pointing at the relay until a session lands.
        if pilot_ip:
            wt.start_probing(pilot_ip)

    async def on_relay_mode():
        # P2P failed — pilot asked to relay video through the signal server.
        # Same chunk format including parity; the pilot decodes it identically.
        # There is no drop_pending on TCP, so relay latency stays unbounded under
        # sustained congestion — a documented limitation of the fallback path.
        relay.send_batch    = signaling.send_binary_batch
        relay.pending_bytes = signaling.pending_bytes
        relay.drop_pending  = None
        relay.link_stats    = None
        log.info('relay mode — video now flows via signal server')

    async def on_qos_signal(profile_name: str):
        # Arrives via the signal server, so it works in relay mode too — where
        # there is no pilot→robot JSON path at all.
        await on_qos(profile_name)

    signaling.on_pilot_connected    = on_pilot_connected
    signaling.on_pilot_disconnected = on_pilot_disconnected
    signaling.on_relay_mode         = on_relay_mode
    signaling.on_punch              = on_punch
    signaling.on_qos                = on_qos_signal

    # Push the startup profile to the publisher so it is not left on its default.
    await publisher.send(relay.profile.publisher_config())

    # ── Step 6: Run ───────────────────────────────────────────────────────────
    try:
        await signaling.run()
    except KeyboardInterrupt:
        pass
    finally:
        wt.stop()
        await relay.stop()


if __name__ == '__main__':
    asyncio.run(main())
