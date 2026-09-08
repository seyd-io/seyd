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
    {"type": "layer", "name": "low"|"high", "reason": "up"|"down", ...} → ask
    that layer's stream for an immediate IDR, so Seyd's switch lands at once;
    {"type": "video-config", "maxBitrateKbps": N, "maxGopMs": G,
     "suggestedFps": F, ...} → the camera's VBR upper cap for stream 101 is
    set to N (seydd's ABR lowers it under loss or latency and raises it back),
    its GOP to G converted to frames at the stream's frame rate (ADR 0009: a
    long GOP, with IDRs on demand through recovery-request — this camera has
    no intra refresh, so `preferIntraRefresh` is noted and ignored), and its
    frame-rate cap to F when the ABR is pinned at its bitrate floor, back to
    the configured baseline when F is 0; at most one change per 2 s per
    setting.

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
    ap.add_argument('--layer-channels', default='low=102,high=101',
                    help='simulcast layer name -> ISAPI stream channel, for forcing an IDR on a switch')
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
    stats = {'ptz': 0, 'stale': 0, 'keyframes': 0, 'bitrate_changes': 0, 'gop_changes': 0, 'fps_changes': 0, 'layer_switches': 0}

    def coalescer(apply, stat_key):
        """
        At most one ISAPI write per 2 s, latest value wins. The camera is far
        slower than the ABR's once-a-second decisions, so submitting every one
        of them would queue writes faster than they drain.
        """
        st = {'last': None, 'pending': None, 'at': 0.0}

        async def drain():
            while st['pending'] is not None:
                wait = 2.0 - (time.time() - st['at'])
                if wait > 0:
                    await asyncio.sleep(wait)
                value, st['pending'] = st['pending'], None
                if value == st['last']:
                    continue
                if await apply(value):
                    st['last'] = value
                    stats[stat_key] += 1
                st['at'] = time.time()

        def submit(value):
            idle = st['pending'] is None
            st['pending'] = value
            if idle:
                asyncio.ensure_future(drain())

        return submit

    submit_bitrate = coalescer(
        lambda kbps: cam.set_bitrate_cap(kbps, args.stream_channel), 'bitrate_changes')
    submit_fps = coalescer(
        lambda fps: cam.set_max_frame_rate(fps, args.stream_channel), 'fps_changes')
    submit_gop = coalescer(
        lambda frames: cam.set_gop(frames, args.stream_channel), 'gop_changes')

    # The frame rate to come back to once the link recovers. Read from the
    # camera rather than assumed, so the demo restores whatever the operator
    # actually configured.
    baseline_fps = await cam.get_max_frame_rate(args.stream_channel) or 25
    log.info('baseline frame rate: %d fps (channel %d)', baseline_fps, args.stream_channel)

    layer_channels = {}
    for pair in filter(None, (p.strip() for p in args.layer_channels.split(','))):
        name, _, ch = pair.partition('=')
        layer_channels[name] = int(ch)

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
        elif t == 'layer':
            # Seyd switches on the new layer's next keyframe. Asking the camera
            # for one now turns "next keyframe" from up to a GOP into
            # immediately, which is the whole point of simulcast over a
            # reconnect: the switch costs one frame, not a reconnection.
            name = m.get('name')
            ch = layer_channels.get(name)
            log.info('layer -> %s (%s)%s', name, m.get('reason'),
                     '' if ch else ' [no ISAPI channel mapped, switching on its own keyframe]')
            if ch:
                stats['layer_switches'] += 1
                asyncio.ensure_future(cam.request_keyframe(ch))
        elif t == 'video-config':
            kbps = m.get('maxBitrateKbps')
            fps = m.get('suggestedFps')
            gop_ms = m.get('maxGopMs')
            log.info('video-config: %s kbps, gop %s ms, fps %s (%s)%s', kbps, gop_ms, fps or '-', m.get('reason'),
                     ' [intra refresh preferred; not available on this camera, using a long GOP]'
                     if m.get('preferIntraRefresh') and m.get('reason') == 'profile' else '')
            if isinstance(kbps, (int, float)) and kbps > 0:
                submit_bitrate(int(kbps))
            # The GOP ceiling is the publisher's to meet however it can. This
            # encoder has no intra refresh, so it takes the other route ADR 0009
            # names: the longest GOP the profile allows, with IDRs supplied on
            # demand through recovery-request above. The baseline frame rate,
            # not the current cap, converts ms to frames, so a temporary fps cut
            # does not also shorten the GOP.
            if isinstance(gop_ms, (int, float)) and gop_ms > 0:
                submit_gop(max(1, round(gop_ms * baseline_fps / 1000)))
            # suggestedFps is 0 whenever the controller is not pinned at its
            # bitrate floor, which is the signal to restore full cadence. Frame
            # rate is the last rung of the ladder precisely because it is a
            # latency term for the pilot: 25 -> 15 fps stretches the interval
            # between frames from 40 ms to 67 ms, so the operator waits that
            # much longer to see the result of their own input.
            if isinstance(fps, (int, float)):
                submit_fps(int(fps) if fps > 0 else baseline_fps)

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
