#!/usr/bin/env python3
# Copyright 2026 Anton Gravestam
# SPDX-License-Identifier: Apache-2.0
"""
Provision the demo Hikvision as a simulcast pair (ADR 0008).

The camera encodes its channels independently and simultaneously, so the ladder
costs nothing but configuring them to agree:

    channel 101  "high"  1280x720   the main stream, already the demo's picture
    channel 102  "low"    640x480   the sub stream, the rung to fall back to

Both must be **Baseline** H.264: the agent's frame gate forbids B-frames, which
Main and High profiles emit (DEMO.md, "Encoder settings applied"). The sub
stream ships as Main from the factory, which is the one setting that would
otherwise turn a layer switch into corruption rather than a smaller picture.

Frame rate is deliberately equal on both rungs. Dropping resolution is the point
of the ladder; dropping cadence is the thing the ladder exists to avoid, because
for a remote pilot frame interval *is* latency.

Idempotent: run it as often as you like. Reads CAMERA_IP / CAMERA_USER /
CAMERA_PASSWORD from the environment (`set -a; . ./.env.local; set +a`).

    python3 examples/demo-robot/setup-simulcast.py [--dry-run]

Nothing here is Seyd — it is vendor provisioning, and lives in examples/ for the
same reason the ISAPI driver does.
"""
import argparse
import os
import re
import sys
import urllib.error
import urllib.request

TIMEOUT_S = 20.0

# (channel, layer name, candidate resolutions in preference order, fps, kbps cap)
#
# 640x360 is tried first because the scene is 16:9 and a 4:3 rung letterboxes
# into a squeezed picture on switch. Firmware that only lists 4:3 sub-stream
# sizes rejects it, and 640x480 is the fallback.
LADDER = [
    (101, "high", [(1280, 720)], 25, 3000),
    (102, "low", [(640, 360), (640, 480)], 25, 800),
]


def opener(ip, user, password):
    """A fresh opener per request: urllib's digest handler latches after a 401
    and then fails every later call, even once the camera is happy again."""
    mgr = urllib.request.HTTPPasswordMgrWithDefaultRealm()
    mgr.add_password(None, f"http://{ip}/", user, password)
    return urllib.request.build_opener(urllib.request.HTTPDigestAuthHandler(mgr))


def get(ip, auth, path):
    return opener(ip, *auth).open(f"http://{ip}{path}", timeout=TIMEOUT_S).read().decode()


def put(ip, auth, path, body):
    req = urllib.request.Request(
        f"http://{ip}{path}", data=body.encode(), method="PUT",
        headers={"Content-Type": "application/xml"})
    try:
        return b"<statusCode>1<" in opener(ip, *auth).open(req, timeout=TIMEOUT_S).read() \
            or True
    except urllib.error.HTTPError as e:
        print(f"    PUT {path} -> HTTP {e.code}")
        return False


def field(xml, tag):
    m = re.search(f"<{tag}>([^<]*)</{tag}>", xml)
    return m.group(1) if m else None


def patch(xml, tag, value):
    return re.sub(f"<{tag}>[^<]*</{tag}>", f"<{tag}>{value}</{tag}>", xml, count=1)


def describe(xml):
    return (f"{field(xml, 'videoCodecType')}/{field(xml, 'H264Profile')} "
            f"{field(xml, 'videoResolutionWidth')}x{field(xml, 'videoResolutionHeight')} "
            f"@{int(field(xml, 'maxFrameRate') or 0) // 100}fps "
            f"cap={field(xml, 'vbrUpperCap')}kbps")


def configure(ip, auth, channel, name, sizes, fps, kbps, dry_run):
    path = f"/ISAPI/Streaming/channels/{channel}"
    original = get(ip, auth, path)
    print(f"  {channel} ({name}): {describe(original)}")

    for width, height in sizes:
        xml = original
        for tag, value in (
            ("H264Profile", "Baseline"),
            ("videoQualityControlType", "VBR"),
            ("videoResolutionWidth", width),
            ("videoResolutionHeight", height),
            ("maxFrameRate", fps * 100),
            ("vbrUpperCap", kbps),
        ):
            xml = patch(xml, tag, value)
        if dry_run:
            print(f"    would set {describe(xml)}")
            return True
        if not put(ip, auth, path, xml):
            continue
        # Trust the camera's own read-back, not the 200: this firmware accepts a
        # document and silently keeps its old value for a rejected resolution.
        now = get(ip, auth, path)
        if (field(now, "videoResolutionWidth"), field(now, "videoResolutionHeight")) == (
            str(width), str(height)
        ):
            print(f"    -> {describe(now)}")
            return True
        print(f"    {width}x{height} not accepted, trying next")
    print(f"    FAILED to configure channel {channel}")
    return False


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()

    ip = os.getenv("CAMERA_IP")
    password = os.getenv("CAMERA_PASSWORD")
    if not ip or not password:
        sys.exit("set CAMERA_IP and CAMERA_PASSWORD (set -a; . ./.env.local; set +a)")
    auth = (os.getenv("CAMERA_USER", "admin"), password)

    print(f"camera {ip}: configuring the simulcast ladder")
    ok = all(configure(ip, auth, *rung, args.dry_run) for rung in LADDER)
    if ok:
        print("ladder ready — both rungs Baseline H.264 at the same frame rate")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
