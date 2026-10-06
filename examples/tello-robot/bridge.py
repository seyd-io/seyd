#!/usr/bin/env python3
"""
Tello demo robot bridge: the robot-side program that makes a Ryze Tello a
Seyd robot. Like examples/demo-robot/bridge.py for the PTZ camera, it
consumes seydd's generic UDP interfaces (docs/protocol/seydd.md) and speaks
the vendor protocol (tello.py) on the other side:

  * command channel `flight` (seydd → udp://127.0.0.1:5014): JSON
    {"roll": -100..100, "pitch": -100..100, "throttle": -100..100,
     "yaw": -100..100, "ts": ms} — stick velocities, latest-value-wins,
    held for a short window and centred when the pilot stops renewing;
    {"takeoff": true, "ts"} and {"land": true, "ts"}.
  * sensor channel `telemetry` (bridge → udp://127.0.0.1:5012): JSON at
    10 Hz with battery, height, speed, attitude, position and link state,
    plus the bridge's own video counters — what the pilot's footer shows.
  * video: the drone's Annex B pictures, re-framed as RTP (h264rtp.py) to
    seydd's `rtp://127.0.0.1:5010` video input.
  * publisher control (seydd → udp://127.0.0.1:5013):
    {"type": "recovery-request"} → ask the drone for a keyframe (its
    "start video" command answers with SPS/PPS and an IDR); the same request
    goes out when a picture arrives torn from the Wi-Fi. The torn picture is
    dropped; the delta frames after it keep flowing while the keyframe is on
    its way (--after-loss forward, a brief smear) or are held back until it
    arrives (--after-loss wait, a brief freeze);
    {"type": "video-config", "maxBitrateKbps": N, "maxGopMs": G} → the
    drone's encoder level (1–5 = 1–4 Mbps) is set to the highest at or below
    N, at most once per 2 s, and G becomes the keyframe safety net: if no IDR
    has been seen for G ms one is requested (ADR 0009 — on demand, never on
    a one-second timer).

  The drone's own link is adapted here, because Seyd cannot see it (loss is
  measured on the pilot leg, ADR 0006; PLAN.md item 24 is the proper fix).
  Every torn picture is a datagram the 2.4 GHz link dropped, and a picture
  is as many datagrams as its size: 9 at 1.5 Mbps, 6 at 1 Mbps, a keyframe
  14 against 9. So when tears climb the level steps down (fewer datagrams,
  fewer tears, keyframes that survive) and when the link is quiet it steps
  back up, never above what Seyd asked for: effective = min(requested,
  link). Thresholds: ≥ --link-down-tears torn pictures in 5 s steps down,
  ≤ --link-up-tears in 15 s steps up; one step, then hold. The range flight
  (DEMO-TELLO.md) is what this is for: 1 keyframe in 80 arrived at 1.5 Mbps.
    {"type": "session", "state": "ended", "sessions": 0} → land if airborne.

Safety, in order of what fails first:
  1. Sticks expire. A command holds the sticks for `--hold-ms`; the pilot
     renews every 100 ms while a key is held. No renewal → centred → hover.
  2. Commands must be fresh (`--max-command-age-ms`); a stale one is ignored.
  3. Nobody flying? Land. When the last session ends, or the driver's commands
     stop for `--orphan-land-s` while airborne, the drone lands itself. The
     pilot page sends its sticks once a second even when centred, as presence,
     so a hovering pilot is not mistaken for an absent one (first flight,
     2026-10-02: without that, a hands-off hover was landed after 5 s).
  4. The drone's own failsafe: it lands when the controller link is lost.
  5. `--alt-limit-m` is written to the drone before every take-off.

Nothing here is Seyd; it is what a customer writes for their own vehicle.
"""
import argparse
import asyncio
import collections
import json
import logging
import os
import socket
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from h264rtp import Relay  # noqa: E402
from tello import Tello  # noqa: E402

log = logging.getLogger('bridge')

# Encoder level → bitrate, measured on the drone (2026-10-02): the levels are
# exact to within 1 %. Level 0 is "auto" and sat at ~4 Mbps on the desk.
ENCODER_LEVELS_KBPS = {1: 1000, 2: 1500, 3: 2000, 4: 3000, 5: 4000}


class _Udp(asyncio.DatagramProtocol):
    def __init__(self, on_msg):
        self.on_msg = on_msg

    def datagram_received(self, data, addr):
        try:
            self.on_msg(json.loads(data.decode('utf-8')))
        except (ValueError, UnicodeDecodeError):
            log.debug('non-JSON datagram from %s', addr)


def level_for_kbps(kbps: float) -> int:
    best = 1
    for level, rate in ENCODER_LEVELS_KBPS.items():
        if rate <= kbps:
            best = level
    return best


async def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--drone-ip', default=os.getenv('TELLO_IP', '192.168.10.1'))
    ap.add_argument('--drone-port', type=int, default=8889)
    ap.add_argument('--control-local-port', type=int, default=9000)
    ap.add_argument('--video-local-port', type=int, default=6038)
    ap.add_argument('--rtp', default='127.0.0.1:5010', help='seydd video input (rtp://host:port in seydd.toml)')
    ap.add_argument('--telemetry', default='127.0.0.1:5012', help='seydd sensor input')
    ap.add_argument('--control-port', type=int, default=5013, help='publisher control from seydd')
    ap.add_argument('--command-port', type=int, default=5014, help='flight commands from seydd')
    ap.add_argument('--telemetry-hz', type=float, default=10.0)
    ap.add_argument('--hold-ms', type=int, default=400, help='how long one stick command stays in force')
    ap.add_argument('--max-command-age-ms', type=int, default=1500)
    ap.add_argument('--orphan-land-s', type=float, default=5.0, help='land when airborne with no driver command for this long')
    ap.add_argument('--alt-limit-m', type=int, default=int(os.getenv('TELLO_ALT_LIMIT_M', '5')))
    ap.add_argument('--stick-scale', type=float, default=1.0, help='multiply pilot stick values (0 < s <= 1) to tame the drone indoors')
    ap.add_argument('--encoder-rate', type=int, default=4, help='initial encoder level 0–5 (0 = auto)')
    ap.add_argument('--zoom', action='store_true', help='1280x720 16:9 instead of 960x720 4:3')
    ap.add_argument('--no-takeoff', action='store_true', help='refuse take-off commands (bench testing with props off)')
    ap.add_argument('--after-loss', choices=['forward', 'wait'], default='forward',
                    help='after a torn picture: keep forwarding delta frames while the keyframe is on its way '
                         '(smear, like the vendor app), or hold them back until it arrives (freeze)')
    ap.add_argument('--link-down-tears', type=int, default=10, help='torn pictures in 5 s that step the encoder level down (0 disables link adaptation)')
    ap.add_argument('--link-up-tears', type=int, default=3, help='torn pictures in 15 s at or below which the level steps back up')
    ap.add_argument('--resume-after-loss-s', type=float, default=1.5,
                    help='with --after-loss wait: forward delta frames again if no keyframe came within this long')
    args = ap.parse_args()
    logging.basicConfig(level=os.getenv('LOG_LEVEL', 'INFO'), format='%(asctime)s %(levelname)s %(name)s: %(message)s')

    loop = asyncio.get_running_loop()
    rtp_host, rtp_port = args.rtp.split(':')
    tel_host, tel_port = args.telemetry.split(':')
    rtp_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    rtp_sock.connect((rtp_host, int(rtp_port)))
    tel_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    tel_sock.connect((tel_host, int(tel_port)))

    drone = Tello(addr=(args.drone_ip, args.drone_port), control_port=args.control_local_port,
                  video_port=args.video_local_port, alt_limit_m=args.alt_limit_m,
                  encoder_rate=args.encoder_rate, zoom=args.zoom)
    stats = {'commands': 0, 'stale': 0, 'takeoffs': 0, 'landings': 0, 'keyframe_requests': 0,
             'rate_changes': 0, 'orphan_landings': 0, 'safety_net_keyframes': 0, 'link_steps_down': 0, 'link_steps_up': 0}
    state = {'last_command_at': 0.0, 'sessions': 0, 'max_gop_ms': 10000, 'last_rate_at': 0.0,
             'pending_rate': None, 'last_kf_req': 0.0, 'no_video_since': None,
             'notice': None, 'notice_until': 0.0,
             'requested_level': args.encoder_rate or 5, 'link_level': 5, 'link_changed_at': 0.0}
    tears = collections.deque()   # monotonic times of torn pictures, last 15 s

    def want_level() -> int:
        return max(1, min(state['requested_level'], state['link_level']))

    def submit_level() -> None:
        level = want_level()
        if level == drone.encoder_rate and state['pending_rate'] is None:
            return
        idle = state['pending_rate'] is None
        state['pending_rate'] = level
        if idle:
            asyncio.ensure_future(drain_rate())

    def notice(text: str, seconds: float = 8.0) -> None:
        # What the pilot must be told: a refusal or an automatic landing is
        # otherwise a button that silently does nothing.
        log.warning(text)
        state['notice'] = text
        state['notice_until'] = time.monotonic() + seconds

    def request_keyframe(reason: str) -> None:
        # seydd already rate-limits its requests to one per 250 ms; the
        # bridge's own reasons (a torn frame, the safety net) get the same cap.
        now = time.monotonic()
        if now - state['last_kf_req'] < 0.25:
            return
        state['last_kf_req'] = now
        stats['keyframe_requests'] += 1
        log.debug('keyframe request (%s)', reason)
        drone.start_video()

    def send_rtp(pkt: bytes) -> None:
        try:
            rtp_sock.send(pkt)
        except OSError as e:   # seydd not up yet: ECONNREFUSED on a connected UDP socket
            log.debug('rtp send: %s', e)

    relay = Relay(send=send_rtp, now=time.monotonic, on_need_keyframe=lambda: request_keyframe('torn frame'),
                  resume_after_s=args.resume_after_loss_s, after_loss=args.after_loss)

    def on_video_loss(n: int) -> None:
        tears.append(time.monotonic())
        relay.mark_loss(n)

    fps_window = {'frames': 0, 'bytes': 0, 'at': time.monotonic(), 'fps': 0.0, 'kbps': 0}
    # Per picture: first→last datagram (the drone's and Wi-Fi's share) and
    # last datagram→RTP handed to the daemon (this bridge's share). Rolling,
    # for the comparison with the native Rust host (host/).
    timing = {'assembly_ms': collections.deque(maxlen=300), 'push_ms': collections.deque(maxlen=300)}

    def pct(v, p):
        if not v:
            return 0.0
        s = sorted(v)
        return round(s[round((len(s) - 1) * p)], 2)

    def on_video(picture: bytes) -> None:
        sent, _ = relay.push_picture(picture)
        if sent:
            a = drone.assembler
            timing['assembly_ms'].append((a.last_close_at - a.last_first_at) * 1000.0)
            timing['push_ms'].append((time.monotonic() - a.last_close_at) * 1000.0)
        fps_window['frames'] += 1
        fps_window['bytes'] += len(picture)

    drone.on_video = on_video
    drone.on_video_loss = on_video_loss
    drone.on_state = lambda s: log.info('drone %s', s)
    await drone.start()

    # ── commands from the pilot ──
    def on_command(m: dict) -> None:
        ts = m.get('ts')
        if isinstance(ts, (int, float)) and abs(time.time() * 1000 - ts) > args.max_command_age_ms:
            stats['stale'] += 1
            return
        state['last_command_at'] = time.monotonic()
        stats['commands'] += 1
        if m.get('land'):
            stats['landings'] += 1
            drone.land()
            return
        if m.get('takeoff'):
            if args.no_takeoff:
                notice('take-off refused: disabled on this robot (--no-takeoff)')
                return
            if not drone.connected:
                notice('take-off refused: drone not connected')
                return
            if drone.flight.flying:
                return
            if drone.flight.battery_percentage and drone.flight.battery_percentage < 15:
                notice('take-off refused: battery %d%% (needs 15%%)' % drone.flight.battery_percentage)
                return
            stats['takeoffs'] += 1
            drone.takeoff()
            return
        if any(k in m for k in ('roll', 'pitch', 'throttle', 'yaw')):
            s = max(0.05, min(1.0, args.stick_scale)) / 100.0

            def axis(k):
                v = m.get(k, 0)
                return max(-1.0, min(1.0, float(v) * s)) if isinstance(v, (int, float)) else 0.0
            drone.set_sticks(axis('roll'), axis('pitch'), axis('throttle'), axis('yaw'), hold_s=args.hold_ms / 1000.0)

    # ── publisher control from seydd ──
    async def drain_rate() -> None:
        while state['pending_rate'] is not None:
            wait = 2.0 - (time.monotonic() - state['last_rate_at'])
            if wait > 0:
                await asyncio.sleep(wait)
            level, state['pending_rate'] = state['pending_rate'], None
            if level != drone.encoder_rate:
                drone.set_video_encoder_rate(level)
                stats['rate_changes'] += 1
                log.info('encoder level -> %d (~%d kbps)', level, ENCODER_LEVELS_KBPS.get(level, 0))
            state['last_rate_at'] = time.monotonic()

    def on_control(m: dict) -> None:
        t = m.get('type')
        if t == 'recovery-request':
            request_keyframe(f"seydd {m.get('kind')}/{m.get('reason')}")
        elif t == 'video-config':
            kbps = m.get('maxBitrateKbps')
            gop = m.get('maxGopMs')
            log.info('video-config: %s kbps, gop %s ms (%s)', kbps, gop, m.get('reason'))
            if isinstance(kbps, (int, float)) and kbps > 0:
                state['requested_level'] = level_for_kbps(kbps)
                submit_level()
            if isinstance(gop, (int, float)) and gop > 0:
                state['max_gop_ms'] = int(gop)
        elif t == 'session':
            n = m.get('sessions', 0)
            state['sessions'] = n
            log.info('session %s (%s) — %s active', m.get('state'), m.get('role', '?'), n)
            if m.get('state') == 'ended' and n == 0:
                drone.centre_sticks()
                if drone.flight.flying:
                    notice('last session ended while airborne — landing')
                    stats['orphan_landings'] += 1
                    drone.land()
        elif t == 'layer':
            pass   # single layer; the drone encodes one stream

    await loop.create_datagram_endpoint(lambda: _Udp(on_command), local_addr=('127.0.0.1', args.command_port))
    await loop.create_datagram_endpoint(lambda: _Udp(on_control), local_addr=('127.0.0.1', args.control_port))
    log.info('bridge up: flight commands on :%d, publisher control on :%d, video → rtp %s, telemetry → %s, drone %s',
             args.command_port, args.control_port, args.rtp, args.telemetry, args.drone_ip)

    # ── telemetry out, and the watchdogs, on one timer ──
    async def telemetry_loop() -> None:
        period = 1.0 / args.telemetry_hz
        last_log = time.monotonic()
        while True:
            await asyncio.sleep(period)
            now = time.monotonic()
            f = drone.flight
            # orphan: airborne, a driver was here, and nothing has come for a while
            if (f.flying and state['last_command_at'] and now - state['last_command_at'] > args.orphan_land_s
                    and state['sessions'] > 0):
                notice('no pilot presence for %.0fs while airborne — landing' % (now - state['last_command_at']))
                stats['orphan_landings'] += 1
                state['last_command_at'] = 0.0
                drone.land()
            # the drone's own link: step the encoder level with the tear rate
            while tears and now - tears[0] > 15.0:
                tears.popleft()
            if args.link_down_tears > 0 and now - state['link_changed_at'] >= 5.0:
                recent5 = sum(1 for t in tears if now - t <= 5.0)
                if recent5 >= args.link_down_tears and want_level() > 1:
                    state['link_level'] = want_level() - 1
                    state['link_changed_at'] = now
                    stats['link_steps_down'] += 1
                    log.warning('drone link: %d torn pictures in 5 s — encoder level cap -> %d', recent5, state['link_level'])
                    submit_level()
                elif (len(tears) <= args.link_up_tears and state['link_level'] < state['requested_level']
                      and now - state['link_changed_at'] >= 15.0):
                    state['link_level'] += 1
                    state['link_changed_at'] = now
                    stats['link_steps_up'] += 1
                    log.info('drone link quiet (%d torn in 15 s) — encoder level cap -> %d', len(tears), state['link_level'])
                    submit_level()
            # keyframe safety net (ADR 0009): the profile's maxGopMs, not a timer of our own
            if (drone.connected and relay.last_idr_at is not None and now - relay.last_idr_at > state['max_gop_ms'] / 1000.0
                    and now - drone.last_video_rx < 1.0):
                stats['safety_net_keyframes'] += 1
                request_keyframe('safety net')
            if now - fps_window['at'] >= 1.0:
                dt = now - fps_window['at']
                fps_window['fps'] = round(fps_window['frames'] / dt, 1)
                fps_window['kbps'] = int(fps_window['bytes'] * 8 / dt / 1000)
                fps_window['frames'] = fps_window['bytes'] = 0
                fps_window['at'] = now
            yaw, pitch, roll = drone.logs.euler_deg()
            msg = {
                **({'notice': state['notice']} if state['notice'] and now < state['notice_until'] else {}),
                'drone': 'connected' if drone.connected else 'disconnected',
                'flying': f.flying,
                'battery': f.battery_percentage,
                'battery_low': bool(f.battery_low or f.battery_lower),
                'height_m': round(f.height / 10.0, 1),
                'speed_mps': round(f.ground_speed / 10.0, 1),
                'vel_mps': [round(v, 2) for v in drone.logs.vel],
                'pos_m': [round(p, 2) for p in drone.logs.pos],
                'yaw_deg': round(yaw), 'pitch_deg': round(pitch), 'roll_deg': round(roll),
                'fly_time_s': round(f.fly_time / 10.0, 1),
                'fly_mode': f.fly_mode,
                'wind': bool(f.wind_state),
                'imu_ok': bool(f.imu_state),
                'hot': bool(f.temperature_height),
                'wifi': drone.wifi_strength,
                'wifi_disturb': drone.wifi_disturb,
                'video': {'fps': fps_window['fps'], 'kbps': fps_window['kbps'], 'level': drone.encoder_rate,
                          'level_requested': state['requested_level'], 'level_link_cap': state['link_level'],
                          'tears_5s': sum(1 for t in tears if now - t <= 5.0),
                          'lost_frames': drone.assembler.lost_frames, 'idr': relay.stats['idr'],
                          'codec': relay.codec,
                          'assembly_ms_p50': pct(timing['assembly_ms'], 0.5), 'assembly_ms_p95': pct(timing['assembly_ms'], 0.95),
                          'push_ms_p95': pct(timing['push_ms'], 0.95)},
            }
            try:
                tel_sock.send(json.dumps(msg, separators=(',', ':')).encode())
            except OSError:
                pass
            if now - last_log >= 30:
                last_log = now
                idr_iv = relay.idr_intervals[-10:]
                log.info('stats %s relay=%s drone=%s frames=%d lost=%d idr_interval=%s assembly_ms p50=%.1f p95=%.1f push_ms p95=%.2f',
                         stats, relay.stats, drone.stats, drone.assembler.frames, drone.assembler.lost_frames,
                         [round(x, 1) for x in idr_iv], pct(timing['assembly_ms'], 0.5), pct(timing['assembly_ms'], 0.95), pct(timing['push_ms'], 0.95))

    try:
        await telemetry_loop()
    finally:
        if drone.flight.flying:
            drone.land()
            await asyncio.sleep(0.2)
        await drone.stop()


if __name__ == '__main__':
    try:
        sys.exit(asyncio.run(main()) or 0)
    except KeyboardInterrupt:
        pass
