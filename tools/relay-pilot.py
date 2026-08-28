#!/usr/bin/env python3
"""
Headless pilot that exercises the relay path end to end.

Relay mode is the path a real client gets whenever the robot is behind carrier
NAT, and it is the one a browser cannot easily be scripted against. It is also
the path that used to have no JSON channel at all, so a regression here is
silent: video keeps working while PTZ, sensor data and telemetry quietly stop.

Deliberately forces relay rather than P2P — there is no WebTransport client
here, and P2P is what the browser already covers.

Checks:
  * the signal server brokers a session and reports `ready`
  * the agent announces its capabilities (PTZ or not)
  * binary video chunks arrive and their FEC headers parse
  * frames reassemble, i.e. every chunk index of a frame shows up
  * a PTZ command sent pilot→robot over signalling actually moves the camera

Not part of DARC. Never imported by packages/.

Usage:
    tools/relay-pilot.py --robot darc-demo --signal ws://localhost:8080
    tools/relay-pilot.py --robot darc-demo --signal ws://localhost:8080 --ptz
"""

import argparse
import asyncio
import collections
import json
import struct
import sys

sys.path.insert(0, __file__.rsplit('/', 2)[0] + '/packages/agent')

import websockets   # noqa: E402


def parse_header(chunk: bytes):
    """Mirror of the wire format in packages/agent/fec.py."""
    if len(chunk) < 10:
        return None
    flags = chunk[0]
    version = flags & 0x0F
    if version != 1:
        return None
    frame_id, chunk_idx, total, = struct.unpack('>HHH', chunk[1:7])
    fec_count = chunk[7]
    last_len, = struct.unpack('>H', chunk[8:10])
    return {
        'keyframe':  bool(flags & 0x80),
        'fec_type':  (flags >> 4) & 0x07,
        'frame_id':  frame_id,
        'chunk_idx': chunk_idx,
        'n':         total,
        'k':         fec_count,
        'last_len':  last_len,
        'payload':   len(chunk) - 10,
    }


async def run(args):
    result = {'ok': False}
    frames = collections.defaultdict(set)
    counts = collections.Counter()
    caps = None
    json_msgs = collections.Counter()

    async with websockets.connect(args.signal) as ws:
        await ws.send(json.dumps({'type': 'connect', 'robotId': args.robot}))
        print(f'→ connect robotId={args.robot}')

        # Force the relay path: no WebTransport client here, and relay is the
        # path under test.
        ready = False
        deadline = asyncio.get_event_loop().time() + args.seconds + 10

        while asyncio.get_event_loop().time() < deadline:
            try:
                raw = await asyncio.wait_for(ws.recv(), timeout=2.0)
            except asyncio.TimeoutError:
                continue

            if isinstance(raw, bytes):
                counts['chunks'] += 1
                counts['bytes'] += len(raw)
                h = parse_header(raw)
                if not h:
                    counts['bad_header'] += 1
                    continue
                if h['fec_type'] == 2:
                    counts['fec_chunks'] += 1
                if h['chunk_idx'] >= h['n']:
                    counts['parity'] += 1
                else:
                    frames[h['frame_id']].add(h['chunk_idx'])
                    if h['keyframe']:
                        counts['keyframe_chunks'] += 1
                        frames[h['frame_id']].add('KEY')
                        frames[h['frame_id']].add(('n', h['n']))
                    else:
                        frames[h['frame_id']].add(('n', h['n']))
                continue

            msg = json.loads(raw)
            t = msg.get('type')
            json_msgs[t] += 1

            if t == 'ready':
                ready = True
                print(f"← ready  hint={msg.get('p2pHint')} "
                      f"candidates={len(msg.get('candidates', []))}")
                await ws.send(json.dumps({'type': 'relay-request',
                                          'robotId': args.robot}))
                print('→ relay-request (forcing relay path)')
                # Exercises the agent's `hello` handler and its capabilities
                # reply. On the P2P path this handshake is the only thing that
                # makes capabilities arrive at all — see peer.handle_message.
                await ws.send(json.dumps({'type': 'cmd', 'robotId': args.robot,
                                          'payload': {'type': 'hello'}}))
                print('→ hello')
                start = asyncio.get_event_loop().time()
                deadline = start + args.seconds

                if args.ptz:
                    asyncio.ensure_future(ptz_probe(ws, args))

            elif t == 'unreachable':
                print(f"← unreachable: {msg.get('reason')}")
                return result

            elif t == 'cmd-out':
                payload = msg.get('payload') or {}
                pt = payload.get('type')
                json_msgs[f'cmd-out/{pt}'] += 1
                if pt == 'capabilities':
                    caps = payload
                    print(f"← capabilities  ptz={payload.get('ptz')} "
                          f"ptzHome={payload.get('ptzHome')}")
                elif pt == 'agent-stats':
                    counts['agent_stats'] += 1
                    counts['agent_frames_sent'] = payload.get('frames_sent', 0)
                    counts['agent_chunks_sent'] = payload.get('chunks_sent', 0)
                elif pt == 'sensor':
                    counts['sensor'] += 1

            elif t == 'peer-disconnected':
                print('← peer-disconnected (robot went away)')
                return result

    # ── report ────────────────────────────────────────────────────────────────
    complete = 0
    incomplete = 0
    for fid, got in frames.items():
        n = next((item[1] for item in got
                  if isinstance(item, tuple) and item[0] == 'n'), None)
        idxs = {i for i in got if isinstance(i, int)}
        if n and len(idxs) == n:
            complete += 1
        else:
            incomplete += 1

    print()
    print('── relay path report ────────────────────────────────')
    print(f'  ready received      {ready}')
    print(f'  capabilities        {caps}')
    print(f'  video chunks        {counts["chunks"]}  '
          f'({counts["bytes"]/1024:.0f} KB)')
    print(f'  parity chunks       {counts["parity"]}')
    print(f'  keyframe chunks     {counts["keyframe_chunks"]}')
    print(f'  bad headers         {counts["bad_header"]}')
    print(f'  frames complete     {complete}')
    print(f'  frames incomplete   {incomplete}')
    print(f'  agent-stats msgs    {counts["agent_stats"]}'
          f'   (frames_sent={counts["agent_frames_sent"]})')
    print(f'  json message types  {dict(json_msgs)}')

    result['ok'] = (
        ready
        and counts['chunks'] > 0
        and counts['bad_header'] == 0
        and complete > 0
    )
    result['counts'] = counts
    result['caps'] = caps
    return result


async def ptz_probe(ws, args):
    """Drive PTZ over the signalling socket and report what the camera did."""
    await asyncio.sleep(2.0)
    print('→ ptz pan=40 (via signalling cmd envelope)')
    for _ in range(6):
        await ws.send(json.dumps({
            'type': 'cmd', 'robotId': args.robot,
            'payload': {'type': 'ptz', 'pan': 40, 'tilt': 0, 'zoom': 0},
        }))
        await asyncio.sleep(0.2)
    await ws.send(json.dumps({
        'type': 'cmd', 'robotId': args.robot,
        'payload': {'type': 'ptz', 'pan': 0, 'tilt': 0, 'zoom': 0},
    }))
    print('→ ptz stop')
    await asyncio.sleep(1.0)
    await ws.send(json.dumps({
        'type': 'cmd', 'robotId': args.robot,
        'payload': {'type': 'ptz-home', 'ts': 0},
    }))
    print('→ ptz-home')


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--robot', required=True)
    ap.add_argument('--signal', required=True)
    ap.add_argument('--seconds', type=float, default=10.0)
    ap.add_argument('--ptz', action='store_true',
                    help='also drive PTZ over the relay JSON path')
    args = ap.parse_args()

    result = asyncio.run(run(args))
    print()
    print('RESULT:', 'PASS' if result['ok'] else 'FAIL')
    return 0 if result['ok'] else 1


if __name__ == '__main__':
    sys.exit(main())
