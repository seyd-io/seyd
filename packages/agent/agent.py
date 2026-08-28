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
    --video-url           RTSP URL to pull from instead of --video-port
    --sensor-port         Sensor UDP input port (default: 5002)
    --camera-ip           PTZ camera address for operator control

Credentials are read from the environment (CAMERA_USER / CAMERA_PASSWORD), never
from arguments — anything on the command line is visible to any local `ps`.
"""

import asyncio
import argparse
import json
import logging
import os
import socket
from urllib.parse import quote

import portmap
import qos
from camera import CameraControl
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


def resolve_video_url(url: str | None) -> str | None:
    """
    Fill in RTSP credentials from the environment if the URL has none.

    Lets the URL stay free of secrets in scripts, shell history and `ps` output
    while still producing what libavformat needs, which is credentials inline.
    A URL that already carries them is left alone.
    """
    if not url or '@' in url.split('://', 1)[-1].split('/', 1)[0]:
        return url
    user = os.environ.get('CAMERA_USER')
    password = os.environ.get('CAMERA_PASSWORD')
    if not user or not password:
        return url
    scheme, rest = url.split('://', 1)
    return f'{scheme}://{quote(user, safe="")}:{quote(password, safe="")}@{rest}'


def parse_ptz_home(raw: str | None) -> tuple | None:
    if not raw:
        return None
    try:
        elevation, azimuth, zoom = (int(p) for p in raw.split(','))
        return (elevation, azimuth, zoom)
    except ValueError:
        log.warning('ignoring malformed --ptz-home %r (want elevation,azimuth,zoom)', raw)
        return None


def parse_args():
    p = argparse.ArgumentParser(description='DARC Agent')
    p.add_argument('--robot-id',           required=True)
    p.add_argument('--signal-url',         required=True)
    p.add_argument('--webtransport-port',  type=int, default=4433)
    p.add_argument('--webtransport-host',  default=None,
                   help='Override public host for WebTransport (skips STUN)')
    p.add_argument('--video-port',         type=int, default=5000)
    p.add_argument('--video-url',          default=None,
                   help='RTSP URL to pull video from (replaces --video-port). '
                        'Credentials may be embedded, or supplied via '
                        'CAMERA_USER / CAMERA_PASSWORD')
    p.add_argument('--video-fps',          type=int, default=30,
                   help='Source frame rate; scales the backlog drop threshold')
    p.add_argument('--sensor-port',        type=int, default=5002)
    p.add_argument('--camera-ip',          default=None,
                   help='PTZ camera address for ISAPI control (enables operator '
                        'pan/tilt/zoom). Password from CAMERA_PASSWORD')
    p.add_argument('--camera-channel',     type=int, default=1,
                   help='PTZ channel on the camera (default: 1)')
    p.add_argument('--ptz-home',           default=None,
                   help='Home position as elevation,azimuth,zoom in ISAPI units '
                        '(e.g. 0,1800,10). Returned to when a pilot disconnects')
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
                  profile=qos.get(args.qos_profile),
                  video_url=resolve_video_url(args.video_url),
                  fps=args.video_fps)
    wt    = WebTransportServer()

    # ── optional PTZ camera ───────────────────────────────────────────────────
    camera = None
    if args.camera_ip:
        password = os.environ.get('CAMERA_PASSWORD')
        if not password:
            log.error('--camera-ip given but CAMERA_PASSWORD is unset — '
                      'PTZ control disabled')
        else:
            camera = CameraControl(
                host=args.camera_ip,
                user=os.environ.get('CAMERA_USER', 'admin'),
                password=password,
                channel=args.camera_channel,
                home=parse_ptz_home(args.ptz_home),
            )
            relay.on_ptz      = camera.move
            relay.on_ptz_home = camera.go_home

    def detach_sinks():
        relay.send_batch    = None
        relay.send_json     = None
        relay.pending_bytes = None
        relay.drop_pending  = None
        relay.link_stats    = None

    def capabilities() -> dict:
        # Lets the pilot show PTZ controls only where they do something. A demo
        # camera and a webcam-on-a-Mac run the same agent, and a UI that offers
        # pan/tilt on a fixed webcam teaches the operator to distrust the UI.
        return {
            'type': 'capabilities',
            'ptz':  camera is not None,
            'ptzHome': bool(camera and camera.home),
        }

    async def release_camera():
        """Stop any motion, then park. Called whenever an operator goes away."""
        if not camera:
            return
        camera.move(0, 0, 0)
        await camera.go_home()

    async def on_hello():
        """Pilot announced itself on the JSON channel — tell it what we can do."""
        if relay.send_json:
            await relay.send_json(capabilities())

    async def on_wt_connected(session):
        relay.send_batch    = session.send_datagram_batch
        relay.send_json     = session.send_json
        relay.pending_bytes = session.pending_bytes
        relay.drop_pending  = session.drop_pending
        relay.link_stats    = session.link_stats
        log.info('pilot connected via WebTransport')

    async def on_wt_disconnected():
        detach_sinks()
        # A pilot whose link drops mid-gesture has no way to send a stop, and
        # `momentary` only bounds how long that runs for — parking it is what
        # makes the demo safe to leave unattended.
        await release_camera()
        log.info('pilot disconnected from WebTransport')

    async def on_qos(profile_name: str) -> dict:
        """Apply a profile: DARC's half immediately, the publisher's best-effort."""
        profile = qos.get(profile_name)
        relay.set_profile(profile)
        # The publisher control port addresses a local process on the robot. A
        # camera pulled over RTSP is not that process and is not listening on
        # it, so firing the datagram anyway only produces a log line claiming a
        # reconfiguration that cannot have happened. Report it honestly instead:
        # the pilot then shows "(transport only)", which is the truth — the
        # camera keeps encoding whatever it was configured with out of band.
        delivered = (False if relay.video_url
                     else await publisher.send(profile.publisher_config()))
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
    relay.on_hello     = on_hello

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
        await release_camera()

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
        # The JSON path goes through signalling too. Previously it was left
        # unset here, so relay mode silently had no robot→pilot JSON at all —
        # no sensor data, no telemetry, no acks — despite PROTOTYPE.md claiming
        # sensor data worked on both paths. For this demo it is load-bearing:
        # PTZ is the entire interaction, and a client behind carrier NAT lands
        # on relay.
        relay.send_json     = signaling.send_to_pilot
        await signaling.send_to_pilot(capabilities())
        log.info('relay mode — video and JSON now flow via signal server')

    async def on_command(payload: dict):
        # Pilot→robot JSON arriving over signalling rather than the WebTransport
        # bidi stream. Same handler, so a command behaves identically on both
        # paths and neither one has a private feature set.
        await relay.handle_message(payload)

    async def on_qos_signal(profile_name: str):
        # Arrives via the signal server, so it works in relay mode too — where
        # there is no pilot→robot JSON path at all.
        await on_qos(profile_name)

    signaling.on_pilot_connected    = on_pilot_connected
    signaling.on_pilot_disconnected = on_pilot_disconnected
    signaling.on_relay_mode         = on_relay_mode
    signaling.on_punch              = on_punch
    signaling.on_qos                = on_qos_signal
    signaling.on_command            = on_command

    # Push the startup profile to the publisher so it is not left on its default.
    # Skipped for an IP camera: the publisher control port addresses a local
    # process on the robot, and a camera reached over RTSP is not listening on
    # it. Its encoder is configured out of band (see DEMO.md).
    if not relay.video_url:
        await publisher.send(relay.profile.publisher_config())

    if camera:
        await camera.start()
        await camera.go_home()

    # ── Step 6: Run ───────────────────────────────────────────────────────────
    try:
        await signaling.run()
    except KeyboardInterrupt:
        pass
    finally:
        wt.stop()
        await relay.stop()
        if camera:
            await camera.stop()


if __name__ == '__main__':
    asyncio.run(main())
