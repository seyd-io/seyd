#!/usr/bin/env python3
# Copyright 2026 Anton Gravestam
# SPDX-License-Identifier: Apache-2.0
"""A complete Seyd robot in Python, with its own encoder.

This is the shape of a real integration: the robot already has an H.264
encoder, and Seyd is handed the encoded access units. Nothing is transcoded and
nothing is decoded — Seyd relays the bytes the encoder produced.

    python3 ffmpeg_robot.py --robot-id seyd-py
    python3 ffmpeg_robot.py --signal ws://localhost:8080/ws   # against a local cloud

Or ../../../py-robot.sh, which checks the prerequisites first.

Contrast with `seydd`, which owns the RTP/RTSP plumbing for robots that publish
video on a socket. Here the customer's process owns the pipeline and calls
`push_frame` directly, which is one process and one copy fewer.

The encoder settings below live here, in the publisher, on purpose: Seyd states
transport-observable *targets* (a bitrate ceiling, a latency budget, a GOP
bound) through `on_requested_config`, and the publisher decides how to meet
them. See CLAUDE.md, "Where QoS settings live".
"""

import argparse
import json
import logging
import os
import signal
import subprocess
import sys
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from seyd import Agent, ChannelKind  # noqa: E402

log = logging.getLogger("ffmpeg-robot")

#: Mirrors sim/video-source.sh so the two sources are comparable.
PROFILES = {
    "latency": dict(w=960, h=540, fps=30, kbps=1500, gop=30, preset="ultrafast", vbv_ms=100, idr_s=10),
    "balanced": dict(w=1280, h=720, fps=30, kbps=3000, gop=30, preset="veryfast", vbv_ms=100, idr_s=10),
    "quality": dict(w=1280, h=720, fps=30, kbps=6000, gop=60, preset="veryfast", vbv_ms=200, idr_s=4),
}


def ffmpeg_command(profile: dict, device: str) -> list[str]:
    """H.264 Annex-B on stdout, one access unit per AUD.

    `aud=insert` is what makes the stream splittable without a full parser: an
    access-unit delimiter marks every AU boundary explicitly, so the reader
    below never has to interpret slice headers.
    """
    if device == "lavfi":
        source = ["-re", "-f", "lavfi", "-i",
                  f"testsrc2=size={profile['w']}x{profile['h']}:rate={profile['fps']}"]
    else:
        source = ["-f", "avfoundation", "-framerate", str(profile["fps"]),
                  "-video_size", f"{profile['w']}x{profile['h']}", "-i", device]
    bufk = profile["kbps"] * profile["vbv_ms"] // 1000
    return [
        "ffmpeg", "-hide_banner", "-loglevel", "error", "-fflags", "nobuffer",
        *source,
        "-pix_fmt", "yuv420p",
        "-c:v", "libx264",
        "-tune", "zerolatency",
        "-preset", profile["preset"],
        "-profile:v", "baseline",
        "-b:v", f"{profile['kbps']}k",
        "-maxrate", f"{profile['kbps']}k",
        "-bufsize", f"{bufk}k",
        # Periodic intra refresh instead of periodic IDRs (ADR 0009): `gop` is
        # the refresh sweep period, and an IDR is forced only every `idr_s`, the
        # profile's maxGopMs — x264 in a pipe cannot answer a recovery-request,
        # so this bounds a joining pilot's wait for its first picture.
        "-g", str(profile["gop"]), "-keyint_min", str(profile["gop"]), "-bf", "0",
        "-x264-params", "scenecut=0:intra-refresh=1",
        "-force_key_frames", f"expr:gte(t,n_forced*{profile['idr_s']})",
        "-an", "-flush_packets", "1", "-max_delay", "0",
        "-bsf:v", "h264_metadata=aud=insert",
        "-f", "h264", "pipe:1",
    ]


def access_units(stream):
    """Yield `(bytes, is_keyframe)` per access unit from an Annex-B stream.

    Splits at each access-unit delimiter (NAL type 9); an AU is a keyframe if
    it carries an IDR slice (type 5) or a parameter set (type 7).
    """
    buf = bytearray()
    pending = bytearray()
    while True:
        chunk = stream.read(4096)
        if not chunk:
            break
        pending += chunk
        # Split on start codes, keeping the last (possibly partial) NAL back.
        parts = pending.split(b"\x00\x00\x01")
        pending = bytearray(parts.pop())
        for part in parts:
            if not part and not buf:
                continue
            # `part` is the previous NAL's payload plus its trailing zeroes.
            nal_type = part[0] & 0x1F if part else 0
            if nal_type == 9 and buf:
                au = bytes(buf)
                buf.clear()
                yield au, _is_keyframe(au)
            buf += b"\x00\x00\x01" + part
    if buf:
        yield bytes(buf), _is_keyframe(bytes(buf))


def _is_keyframe(au: bytes) -> bool:
    for i in range(len(au) - 3):
        if au[i] == 0 and au[i + 1] == 0 and au[i + 2] == 1:
            if (au[i + 3] & 0x1F) in (5, 7):
                return True
    return False


def make_command_handler(udp_port: int | None):
    """Log commands, and optionally forward them to a UDP port.

    Forwarding is how you bridge Seyd to robot software that is already
    listening on a socket — the same generic interface `seydd` offers. Sending
    is non-blocking, which matters: this runs on the Seyd callback thread.
    """
    sock = None
    if udp_port:
        import socket as _socket

        sock = _socket.socket(_socket.AF_INET, _socket.SOCK_DGRAM)
        sock.setblocking(False)

    def on_command(channel: int, payload: bytes) -> None:
        log.info("command on channel %d: %s", channel, payload[:120])
        if sock:
            try:
                sock.sendto(payload, ("127.0.0.1", udp_port))
            except OSError as e:
                log.debug("forward failed: %s", e)

    return on_command


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--robot-id", default="seyd-py")
    # The deployed cloud, so the example runs without a second terminal.
    ap.add_argument("--signal",
                    default="wss://seyd-signal-flj7s44j4a-ew.a.run.app/ws")
    ap.add_argument("--profile", default="latency", choices=sorted(PROFILES))
    ap.add_argument("--device", default="lavfi",
                    help="avfoundation device index, or 'lavfi' for a synthetic source")
    ap.add_argument("--credential", default="./robot.key")
    ap.add_argument("--host-override", default=None,
                    help="advertise this address only (LAN testing)")
    ap.add_argument("--command-udp", type=int, default=None, metavar="PORT",
                    help="forward pilot commands to this UDP port on localhost, "
                         "the way seydd does, for robot software already "
                         "listening on a socket")
    args = ap.parse_args()
    logging.basicConfig(level=logging.INFO,
                        format="%(asctime)s %(levelname)s %(name)s: %(message)s")

    profile = PROFILES[args.profile]
    stopping = threading.Event()

    with Agent(
        args.robot_id,
        args.signal,
        credential_path=args.credential,
        qos_profile=args.profile,
        host_override=args.host_override,
    ) as agent:
        video = agent.add_channel(ChannelKind.VIDEO, "main",
                                  codec="avc1.42001f", fps=profile["fps"])
        telemetry = agent.add_channel(ChannelKind.SENSOR, "telemetry")
        # Named `ptz` because that is what the demo pilot's controls send on.
        agent.add_channel(ChannelKind.COMMAND, "ptz")

        # Handlers run on one Seyd thread and must not block.
        agent.on_session_started = lambda sid, role, path: log.info(
            "session %s started (%s over %s)", sid, role.name.lower(), path)
        agent.on_session_ended = lambda sid, reason: log.info(
            "session %s ended (%s) — park actuators here", sid, reason)
        agent.on_command = make_command_handler(args.command_udp)
        # A real publisher reconfigures its encoder here; x264 in a pipe cannot
        # be re-rated live, so this example only reports the request.
        agent.on_requested_config = lambda cfg: log.info("requested config: %s", cfg)
        agent.on_recovery_request = lambda channel, kind, reason: log.info(
            "recovery request: %s (%s)", kind, reason)
        agent.on_signal_state = lambda state, detail: log.info(
            "signal %s%s", state.name.lower(), f": {detail}" if detail else "")

        agent.start()
        log.info("robot %s online — open the pilot at ?robot=%s",
                 args.robot_id, args.robot_id)

        def sensor_loop():
            seq = 0
            while not stopping.wait(0.1):
                agent.push_message(telemetry, {"seq": seq, "t_us": int(time.time() * 1e6)})
                seq += 1

        threading.Thread(target=sensor_loop, daemon=True).start()

        cmd = ffmpeg_command(profile, args.device)
        log.info("encoder: %s", " ".join(cmd))
        proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, bufsize=0)
        signal.signal(signal.SIGINT, lambda *_: stopping.set())

        frames = 0
        try:
            for au, keyframe in access_units(proc.stdout):
                if stopping.is_set():
                    break
                # A real integration passes the encoder's own capture timestamp;
                # raw Annex-B on a pipe carries none, so this is the read clock.
                agent.push_frame(video, au, keyframe=keyframe,
                                 capture_ts_us=int(time.time() * 1e6))
                frames += 1
                if frames % (profile["fps"] * 10) == 0:
                    log.info("%d frames pushed, %s", frames, json.dumps(agent.counters))
        finally:
            stopping.set()
            proc.terminate()
            proc.wait(timeout=5)
            log.info("stopping after %d frames: %s", frames, json.dumps(agent.counters))
    return 0


if __name__ == "__main__":
    sys.exit(main())
