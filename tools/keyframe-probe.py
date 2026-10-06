#!/usr/bin/env python3
# Copyright 2026 Anton Gravestam
# SPDX-License-Identifier: Apache-2.0
"""
Measure a publisher's keyframe behaviour: the cadence of its periodic
keyframes and how fast it answers an on-demand request. Vendor-neutral on the
video side (any RTSP or RTP source ffmpeg can open); the request itself is a
shell command you supply, because that half is always vendor-specific.

    tools/keyframe-probe.py --input "rtsp://user:pass@<ip>:554/Streaming/Channels/101" \\
        --seconds 30 --request-at 12,20 \\
        --request-cmd 'curl -s -m 5 --digest -u "$CAMERA_USER:$CAMERA_PASSWORD" -X PUT \\
                       http://<ip>/ISAPI/Streaming/channels/101/requestKeyFrame'

Prints every keyframe with its wall-clock arrival and the gap since the last
one, then a summary: periodic interval, and for each request the delay from
the moment the command returned to the first keyframe that followed. The
arrival clock is ffmpeg's `showinfo` line stamped as it is read, so it includes
one software decode (~5-10 ms at 720p) — fine for the ~100 ms scale this
measures, and constant across a comparison.

Used for the 2026-09-08 long-GOP experiment on the demo camera
(docs/latency-sources.md §12, docs/encoder-setup.md).
"""
import argparse, re, subprocess, sys, threading, time

SHOWINFO = re.compile(r'showinfo.*?\bn:\s*(\d+).*?\bpts_time:\s*([\d.]+).*?\biskey:(\d)')


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--input', required=True, help='RTSP/RTP URL or anything ffmpeg opens')
    ap.add_argument('--seconds', type=float, default=30)
    ap.add_argument('--request-at', default='', help='comma-separated seconds into the run at which to run --request-cmd')
    ap.add_argument('--request-cmd', default='', help='shell command that asks the publisher for a keyframe')
    ap.add_argument('--transport', default='tcp', help='rtsp transport (tcp|udp)')
    a = ap.parse_args()

    cmd = ['ffmpeg', '-hide_banner', '-loglevel', 'info', '-fflags', 'nobuffer', '-flags', 'low_delay']
    if a.input.startswith('rtsp'):
        cmd += ['-rtsp_transport', a.transport]
    cmd += ['-i', a.input, '-an', '-vf', 'showinfo', '-f', 'null', '-']
    proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True, bufsize=1)

    t0 = time.time()
    frames = []      # (wall, n, pts, key)
    lock = threading.Lock()

    def reader():
        for line in proc.stderr:
            m = SHOWINFO.search(line)
            if not m:
                continue
            with lock:
                frames.append((time.time(), int(m.group(1)), float(m.group(2)), m.group(3) == '1'))

    threading.Thread(target=reader, daemon=True).start()

    requests = []    # (wall_sent, wall_returned)
    req_times = [float(x) for x in a.request_at.split(',') if x.strip()] if a.request_cmd else []
    for at in sorted(req_times):
        wait = t0 + at - time.time()
        if wait > 0:
            time.sleep(wait)
        sent = time.time()
        subprocess.run(a.request_cmd, shell=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        requests.append((sent, time.time()))
        print(f't={sent - t0:6.2f}s  keyframe requested (command took {(time.time() - sent) * 1000:.0f} ms)', flush=True)
    remaining = t0 + a.seconds - time.time()
    if remaining > 0:
        time.sleep(remaining)
    proc.terminate()
    try:
        proc.wait(3)
    except subprocess.TimeoutExpired:
        proc.kill()

    with lock:
        fr = list(frames)
    if not fr:
        print('no frames decoded — check the URL and credentials', file=sys.stderr)
        return 1
    keys = [f for f in fr if f[3]]
    first_wall = fr[0][0]
    print(f'\n{len(fr)} frames in {fr[-1][0] - first_wall:.1f} s '
          f'({len(fr) / max(0.001, fr[-1][0] - first_wall):.1f} fps), {len(keys)} keyframes')
    prev = None
    for wall, n, pts, _ in keys:
        gap = '' if prev is None else f'  +{wall - prev:6.3f} s'
        print(f'  key at t={wall - t0:7.2f}s  frame {n:5d}  pts {pts:8.3f}{gap}')
        prev = wall
    if len(keys) >= 3:
        gaps = [b[0] - a_[0] for a_, b in zip(keys, keys[1:])]
        # Periodic interval = the median gap, ignoring gaps cut short by a request.
        gaps_sorted = sorted(gaps)
        print(f'  keyframe gap: median {gaps_sorted[len(gaps_sorted) // 2]:.3f} s, '
              f'min {gaps_sorted[0]:.3f} s, max {gaps_sorted[-1]:.3f} s')
    for sent, returned in requests:
        after = [k for k in keys if k[0] >= sent]
        if after:
            k = after[0]
            # Frames between the request and the keyframe: how many the encoder finished first.
            between = sum(1 for f in fr if sent <= f[0] < k[0])
            print(f'  request at t={sent - t0:.2f}s → keyframe {k[0] - sent:.3f} s after the request '
                  f'({k[0] - returned:.3f} s after the HTTP call returned, {between} frames in between)')
        else:
            print(f'  request at t={sent - t0:.2f}s → no keyframe followed within the run')
    return 0


if __name__ == '__main__':
    sys.exit(main())
