#!/usr/bin/env python3
"""
Demo robot bridge: the robot-side program that makes the Hikvision PTZ camera a
Seyd robot. It consumes seydd's two generic UDP interfaces
(docs/protocol/seydd.md) and speaks ISAPI to the camera:

  * command channel `ptz` (seydd → udp://127.0.0.1:5004): JSON
    {"pan": -100..100, "tilt": -100..100, "zoom": -100..100, "ts": ms}
    or {"home": true}. Velocity, latest-value-wins, momentary windows.
  * publisher control (seydd → udp://127.0.0.1:5003): JSON
    {"type": "recovery-request", ...} → ask the encoder for an IDR;
    {"type": "session", "state": "ended", "sessions": 0} → stop and park;
    {"type": "video-config", "maxBitrateKbps": N, ...} → the camera's VBR upper
    cap for stream 101 is set to N (seydd's ABR lowers it under loss or
    latency and raises it back); at most one change per 2 s.

Nothing here is Seyd; it is what a customer writes for their own actuators.
Credentials come from CAMERA_USER / CAMERA_PASSWORD in the environment.
"""
import argparse
import asyncio
import json
import logging
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from hikvision import CameraControl  # noqa: E402

log = logging.getLogger('bridge')


class _Udp(asyncio.DatagramProtocol):
    def __init__(self, on_msg):
        self.on_msg = on_msg

    def datagram_received(self, data, addr):
        try:
            self.on_msg(json.loads(data.decode('utf-8')))
        except (ValueError, UnicodeDecodeError):
            log.debug('non-JSON datagram from %s', addr)


async def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--camera-ip', default=os.getenv('CAMERA_IP', '192.168.86.237'))
    ap.add_argument('--camera-channel', type=int, default=1)
    ap.add_argument('--stream-channel', type=int, default=101)
    ap.add_argument('--ptz-home', default='0,1800,10', help='elevation,azimuth,zoom in ISAPI units')
    ap.add_argument('--command-port', type=int, default=5004)
    ap.add_argument('--control-port', type=int, default=5003)
    ap.add_argument('--max-command-age-ms', type=int, default=1500)
    args = ap.parse_args()
    logging.basicConfig(level=logging.INFO, format='%(asctime)s %(levelname)s %(name)s: %(message)s')

    user, password = os.getenv('CAMERA_USER', 'admin'), os.getenv('CAMERA_PASSWORD', '')
    if not password:
        log.error('CAMERA_PASSWORD not set (see .env.local)')
        return 2
    home = tuple(int(x) for x in args.ptz_home.split(','))
    cam = CameraControl(args.camera_ip, user, password, channel=args.camera_channel, home=home)
    await cam.start()
    loop = asyncio.get_running_loop()
    stats = {'ptz': 0, 'stale': 0, 'keyframes': 0, 'bitrate_changes': 0}
    cap = {'last': None, 'pending': None, 'at': 0.0}

    async def apply_bitrate():
        # Coalesce: at most one ISAPI write per 2 s, latest value wins.
        while cap['pending'] is not None:
            wait = 2.0 - (time.time() - cap['at'])
            if wait > 0:
                await asyncio.sleep(wait)
            kbps, cap['pending'] = cap['pending'], None
            if kbps == cap['last']:
                continue
            if await cam.set_bitrate_cap(kbps, args.stream_channel):
                cap['last'] = kbps
                stats['bitrate_changes'] += 1
            cap['at'] = time.time()

    def on_command(m):
        if m.get('home'):
            asyncio.ensure_future(cam.go_home())
            return
        ts = m.get('ts')
        if isinstance(ts, (int, float)) and abs(time.time() * 1000 - ts) > args.max_command_age_ms:
            # A command that sat in a queue somewhere is not the operator's
            # current intent; moving on it is how a camera ends up at its stop.
            stats['stale'] += 1
            return
        cam.move(int(m.get('pan', 0)), int(m.get('tilt', 0)), int(m.get('zoom', 0)))
        stats['ptz'] += 1

    def on_control(m):
        t = m.get('type')
        if t == 'recovery-request':
            stats['keyframes'] += 1
            asyncio.ensure_future(cam.request_keyframe(args.stream_channel))
        elif t == 'session':
            log.info('session %s (%s) — %s active', m.get('state'), m.get('role', '?'), m.get('sessions'))
            if m.get('state') == 'ended' and m.get('sessions', 0) == 0:
                asyncio.ensure_future(park(cam))
        elif t == 'video-config':
            kbps = m.get('maxBitrateKbps')
            log.info('video-config: %s kbps (%s)', kbps, m.get('reason'))
            if isinstance(kbps, (int, float)) and kbps > 0:
                idle = cap['pending'] is None
                cap['pending'] = int(kbps)
                if idle:
                    asyncio.ensure_future(apply_bitrate())

    await loop.create_datagram_endpoint(lambda: _Udp(on_command), local_addr=('127.0.0.1', args.command_port))
    await loop.create_datagram_endpoint(lambda: _Udp(on_control), local_addr=('127.0.0.1', args.control_port))
    log.info('bridge up: commands on :%d, publisher control on :%d, camera %s', args.command_port, args.control_port, args.camera_ip)
    try:
        while True:
            await asyncio.sleep(30)
            log.info('stats %s camera=%s', stats, cam.stats)
    finally:
        await cam.stop()


async def park(cam):
    cam.move(0, 0, 0)
    await asyncio.sleep(0.1)
    await cam.go_home()


if __name__ == '__main__':
    sys.exit(asyncio.run(main()) or 0)
