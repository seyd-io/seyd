#!/usr/bin/env python3
"""
Flip the camera picture 180° — for a camera mounted upside down.

The DS-2DE2A404IWG1-E offers exactly one flip style, `CENTER`, which rotates the
image 180°. That is the right and only correction for an inverted mount: a dome
PTZ has pan and tilt axes but no roll axis, so nothing else about the picture's
orientation is adjustable from either the camera or DARC.

**This flips more than the picture.** With flip enabled the camera also inverts
its pan and tilt directions, so "drag right" still moves the view right rather
than becoming a mirror-image control. That is what makes this the correct fix
rather than rotating the canvas in the pilot: a CSS rotation would leave the
operator's controls inverted, and DARC must never decode or transform video
anyway — the picture arrives as H.264 bytes and is forwarded untouched.

Doing it on the camera also costs nothing at run time. It happens in the sensor
pipeline before encoding, so there is no extra latency and no re-encode.

Not part of DARC. This is a camera setup aid; never imported by packages/.

Usage:
    tools/camera-flip.py              # toggle
    tools/camera-flip.py --on         # force flipped (upside-down mount)
    tools/camera-flip.py --off        # force normal
    tools/camera-flip.py --status     # report without changing anything

Credentials come from .env.local (CAMERA_USER / CAMERA_PASSWORD); CAMERA_IP may
be set there too, or passed with --ip.
"""

import argparse
import os
import re
import sys
import urllib.error
import urllib.request

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

FLIP_XML = ('<?xml version="1.0" encoding="UTF-8"?>'
            '<ImageFlip version="2.0" xmlns="http://www.hikvision.com/ver20/XMLSchema">'
            '<enabled>{enabled}</enabled>'
            '{style}'
            '</ImageFlip>')

# Only sent when enabling: the device rejects a style on a disabled flip.
STYLE_EL = '<ImageFlipStyle>CENTER</ImageFlipStyle>'


def load_env():
    """Read .env.local without disturbing anything already in the environment."""
    path = os.path.join(REPO, '.env.local')
    if not os.path.exists(path):
        return
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith('#') or '=' not in line:
                continue
            key, value = line.split('=', 1)
            os.environ.setdefault(key.strip(), value.strip())


def opener_for(ip, user, password):
    mgr = urllib.request.HTTPPasswordMgrWithDefaultRealm()
    mgr.add_password(None, f'http://{ip}/', user, password)
    return urllib.request.build_opener(urllib.request.HTTPDigestAuthHandler(mgr))


def request(opener, url, body=None):
    req = urllib.request.Request(
        url,
        data=body.encode() if body else None,
        method='PUT' if body else 'GET',
        headers={'Content-Type': 'application/xml'} if body else {},
    )
    with opener.open(req, timeout=10) as resp:
        return resp.read().decode('utf-8', 'replace')


def read_flip(opener, ip):
    xml = request(opener, f'http://{ip}/ISAPI/Image/channels/1/ImageFlip')
    m = re.search(r'<enabled>([^<]*)</enabled>', xml)
    return (m.group(1).strip().lower() == 'true') if m else None


def set_flip(opener, ip, enable: bool):
    body = FLIP_XML.format(enabled='true' if enable else 'false',
                           style=STYLE_EL if enable else '')
    xml = request(opener, f'http://{ip}/ISAPI/Image/channels/1/ImageFlip', body)
    m = re.search(r'<statusString>([^<]*)</statusString>', xml)
    status = m.group(1) if m else '?'
    if status.lower() != 'ok':
        sub = re.search(r'<subStatusCode>([^<]*)</subStatusCode>', xml)
        raise RuntimeError(f'camera rejected the change: {status}'
                           + (f' ({sub.group(1)})' if sub else ''))


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    mode = ap.add_mutually_exclusive_group()
    mode.add_argument('--on', action='store_true', help='flip 180° (inverted mount)')
    mode.add_argument('--off', action='store_true', help='normal orientation')
    mode.add_argument('--status', action='store_true', help='report only')
    ap.add_argument('--ip', default=None, help='camera address (default: CAMERA_IP)')
    args = ap.parse_args()

    load_env()
    ip = args.ip or os.environ.get('CAMERA_IP')
    user = os.environ.get('CAMERA_USER', 'admin')
    password = os.environ.get('CAMERA_PASSWORD')

    if not ip:
        print('error: no camera address. Pass --ip, or set CAMERA_IP in .env.local.',
              file=sys.stderr)
        print('       Not sure of the address?  tools/find-camera.py', file=sys.stderr)
        return 2
    if not password:
        print('error: CAMERA_PASSWORD is not set (expected in .env.local).',
              file=sys.stderr)
        return 2

    opener = opener_for(ip, user, password)

    try:
        current = read_flip(opener, ip)
    except urllib.error.HTTPError as e:
        print(f'error: camera returned HTTP {e.code}'
              + (' — check CAMERA_PASSWORD' if e.code == 401 else ''), file=sys.stderr)
        return 1
    except Exception as e:
        print(f'error: could not reach camera at {ip}: {e}', file=sys.stderr)
        print('       Try: tools/find-camera.py', file=sys.stderr)
        return 1

    if current is None:
        print('error: camera did not report a flip state', file=sys.stderr)
        return 1

    def describe(flipped):
        return 'FLIPPED 180° (upside-down mount)' if flipped else 'normal'

    print(f'  camera {ip}')
    print(f'  current: {describe(current)}')

    if args.status:
        return 0

    target = True if args.on else False if args.off else (not current)
    if target == current:
        print(f'  already {describe(target)} — nothing to do')
        return 0

    try:
        set_flip(opener, ip, target)
    except Exception as e:
        print(f'error: {e}', file=sys.stderr)
        return 1

    # Read back rather than trusting the 200: this is a setting whose whole
    # purpose is visual, and reporting success for a change that did not take
    # would send someone to check their mount instead of their command.
    confirmed = read_flip(opener, ip)
    if confirmed != target:
        print(f'error: camera accepted the change but still reports '
              f'{describe(confirmed)}', file=sys.stderr)
        return 1

    print(f'  now:     {describe(confirmed)}')
    print()
    print('  Takes effect on the live stream immediately — no restart needed.')
    print('  A pilot already connected keeps decoding; the picture just turns over.')
    print('  Pan and tilt directions are inverted to match, so the controls still')
    print('  move the view the way the operator expects.')
    return 0


if __name__ == '__main__':
    sys.exit(main())
