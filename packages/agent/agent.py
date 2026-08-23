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
import logging

from cert import generate_cert
from stun import get_local_ips, get_stun_address
from transport import WebTransportServer
from signaling import SignalingClient
from peer import Relay


def parse_args():
    p = argparse.ArgumentParser(description='DARC Agent')
    p.add_argument('--robot-id',           required=True)
    p.add_argument('--signal-url',         required=True)
    p.add_argument('--webtransport-port',  type=int, default=4433)
    p.add_argument('--webtransport-host',  default=None,
                   help='Override public host for WebTransport (skips STUN)')
    p.add_argument('--video-port',         type=int, default=5000)
    p.add_argument('--sensor-port',        type=int, default=5002)
    return p.parse_args()


async def main():
    args = parse_args()
    logging.basicConfig(
        level=logging.INFO,
        format='%(asctime)s %(levelname)-8s %(message)s',
        datefmt='%H:%M:%S',
    )
    log = logging.getLogger(__name__)

    wt_port = args.webtransport_port

    # ── Step 1: Gather all IPs and candidates ────────────────────────────────
    # Done BEFORE cert generation so every IP is included in the SAN extension,
    # which Chrome requires for the serverCertificateHashes verifier to accept.
    candidates: list[dict] = []
    all_ips:    list[str]  = []

    if args.webtransport_host:
        candidates.append({'url': f'https://{args.webtransport_host}:{wt_port}/darc',
                           'label': 'host-override'})
        all_ips.append(args.webtransport_host)
        log.info('WebTransport host override: %s:%d', args.webtransport_host, wt_port)
    else:
        # Host candidates — LAN IPs; win immediately when pilot is on same network
        for ip in get_local_ips():
            candidates.append({'url': f'https://{ip}:{wt_port}/darc', 'label': 'host'})
            all_ips.append(ip)
            log.info('host candidate: https://%s:%d/darc', ip, wt_port)

        # STUN — discovers the public address for this port before aioquic binds.
        # SO_REUSEADDR/REUSEPORT ensures the temp socket can bind the same port;
        # many NATs reuse the same external mapping when aioquic quickly rebinds.
        stun_result = await get_stun_address(wt_port)
        if stun_result:
            stun_ip, stun_port = stun_result
            candidates.append({'url': f'https://{stun_ip}:{stun_port}/darc', 'label': 'stun'})
            if stun_ip not in all_ips:
                all_ips.append(stun_ip)
            log.info('STUN reflexive address: %s:%d', stun_ip, stun_port)
        else:
            log.warning('STUN failed — robot may not be reachable from internet')

    if not candidates:
        log.error('no WebTransport candidates — agent will register as unreachable')

    # ── Step 2: Generate TLS cert with all IPs in SubjectAlternativeName ─────
    cert, key, fingerprint = generate_cert(all_ips)
    log.info('cert fingerprint: %s…  (SAN IPs: %s)', fingerprint[:16], ', '.join(all_ips))

    # ── Step 3: Start relay + WebTransport server ─────────────────────────────
    relay = Relay(video_port=args.video_port, sensor_port=args.sensor_port)
    wt    = WebTransportServer()

    async def on_wt_connected(session):
        relay.send_binary      = session.send_datagram
        relay.send_json        = session.send_json
        relay.flush_send_queue = session.flush_datagrams
        log.info('pilot connected via WebTransport')

    async def on_wt_disconnected():
        relay.send_binary      = None
        relay.send_json        = None
        relay.flush_send_queue = None
        log.info('pilot disconnected from WebTransport')

    wt.on_connected    = on_wt_connected
    wt.on_disconnected = on_wt_disconnected
    wt.on_message      = relay.handle_message

    await relay.start()
    await wt.start(port=wt_port, cert=cert, key=key)

    # ── Step 4: Signaling ─────────────────────────────────────────────────────
    signaling = SignalingClient(
        url=args.signal_url,
        robot_id=args.robot_id,
        cert_fingerprint=fingerprint,
        candidates=candidates,
    )

    async def on_pilot_connected(pilot_ip: str | None):
        if pilot_ip:
            wt.start_probing(pilot_ip)
        relay.send_binary      = None
        relay.send_json        = None
        relay.flush_send_queue = None

    async def on_pilot_disconnected():
        wt.stop_probing()
        relay.send_binary      = None
        relay.send_json        = None
        relay.flush_send_queue = None

    async def on_relay_mode():
        # P2P failed — pilot asked to relay video through the signal server.
        # Switch send_binary to the signaling WebSocket; flush is a no-op (TCP).
        relay.send_binary      = signaling.send_binary
        relay.flush_send_queue = None
        log.info('relay mode — video now flows via signal server')

    signaling.on_pilot_connected    = on_pilot_connected
    signaling.on_pilot_disconnected = on_pilot_disconnected
    signaling.on_relay_mode         = on_relay_mode

    # ── Step 5: Run ───────────────────────────────────────────────────────────
    try:
        await signaling.run()
    except KeyboardInterrupt:
        pass
    finally:
        wt.stop()
        await relay.stop()


if __name__ == '__main__':
    asyncio.run(main())
