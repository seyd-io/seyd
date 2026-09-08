#!/usr/bin/env python3
"""
Per-frame latency A/B for the pilot pipeline. Drives the demo pilot page with
`?trace=1` in headless Chrome (tools/cdp.py), records the engine's per-frame
trace — `IN` when a frame is reassembled, `SHOW` when it is painted — plus
`lastStats`, and prints a summary: frame sizes, g2g (first chunk sent → frame
reassembled; NOT glass-to-glass, see docs/latency-sources.md §0), arrival
cadence and paint cadence judder, and gaps over two frame intervals.

    tools/.venv/bin/python3 tools/latency-ab.py --robot seyd-sim --pd 100 --seconds 40 \
        --label idr-pd100 --out idr-pd100.json

`--pd` is the presentation delay passed to the page (`?pd=`); `--out` keeps the
raw trace for later analysis. Run against a link shaped by tools/link-shaper.py
to see keyframe bursts — on loopback they cost nothing. The 2026-09-08
intra-refresh measurements in docs/latency-sources.md came from this script.
"""
import argparse, asyncio, json, os, re, statistics, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import cdp as smoke  # noqa: E402
import websockets  # noqa: E402

IN_RE = re.compile(r'^(?P<t>[\d.]+) IN id=(?P<id>\d+) key=(?P<key>[01]) bytes=(?P<bytes>\d+) rec=(?P<rec>[01]) cap=(?P<cap>-?\d+) spread=(?P<spread>[\d.]+) g2g=(?P<g2g>[-\d.]+) q=(?P<q>\S+) (?P<order>\S+)')
SHOW_RE = re.compile(r'^(?P<t>[\d.]+) SHOW ts=(?P<ts>\d+) q=(?P<q>\d+)')


def pct(a, p):
    if not a: return None
    s = sorted(a); return s[min(len(s) - 1, int(p * (len(s) - 1)))]


async def run(a):
    chrome, profile, page_ws = smoke.launch_chrome()
    try:
        async with websockets.connect(page_ws, max_size=None) as ws:
            c = smoke.CDP(ws)
            for m in ('Runtime.enable', 'Page.enable'): await c.call(m)
            url = f'{a.page.rstrip("/")}/?robot={a.robot}&signal={a.signal}&trace=1&pd={a.pd}&qos={a.qos}'
            print('page', url)
            await c.call('Page.navigate', url=url)
            state = None
            for _ in range(80):
                await c.pump(0.25)
                state = await c.eval("document.getElementById('video').session?.state")
                if state in ('connected', 'p2p-failed'): break
            if state != 'connected':
                print('NOT CONNECTED:', state, await c.eval("JSON.stringify(document.getElementById('video').session?.lastFailure ?? null)"))
                return 1
            # Wait for first decoded frame, then warm up.
            for _ in range(120):
                await c.pump(0.25)
                fd = await c.eval("document.getElementById('video').session?.lastStats?.framesDecoded ?? 0")
                if fd and fd > 0: break
            print('first frame after', fd, 'decoded; warming up', a.warmup, 's')
            await c.pump(a.warmup)
            await c.eval("window.__seydTrace = []")   # discard warm-up trace
            t0 = time.time()
            stats = []
            while time.time() - t0 < a.seconds:
                await c.pump(1.0)
                raw = await c.eval("JSON.stringify(document.getElementById('video').session?.lastStats ?? null)")
                st = json.loads(raw or 'null') or {}
                stats.append(st)
            trace = await c.eval("JSON.stringify(window.__seydTrace ?? [])")
            trace = json.loads(trace)
    finally:
        smoke.stop_chrome(chrome, profile)

    ins, shows = [], []
    for line in trace:
        m = IN_RE.match(line)
        if m: ins.append({k: (float(v) if k in ('t', 'spread', 'g2g') else v) for k, v in m.groupdict().items()}); continue
        m = SHOW_RE.match(line)
        if m: shows.append((float(m['t']), int(m['ts'])))
    for i in ins:
        i['bytes'] = int(i['bytes']); i['key'] = i['key'] == '1'; i['cap'] = int(i['cap'])
    frame_ms = 1000.0 / a.fps
    sizes = [i['bytes'] for i in ins]
    keys = [i['bytes'] for i in ins if i['key']]
    deltas = [i['bytes'] for i in ins if not i['key']]
    g2g = [i['g2g'] for i in ins if i['g2g'] >= 0]
    spread = [i['spread'] for i in ins]
    # Arrival jitter: reassembly time minus capture time, relative to the min over the run.
    lag = [i['t'] - i['cap'] / 1000.0 for i in ins if i['cap'] >= 0]
    lag_j = [l - min(lag) for l in lag] if lag else []
    # Arrival cadence: gap between consecutive reassembled frames vs the frame interval.
    arr_gap = [ins[k]['t'] - ins[k - 1]['t'] for k in range(1, len(ins))]
    arr_judder = [abs(g - frame_ms) for g in arr_gap]
    # Paint cadence from SHOW lines.
    paint_gap = [shows[k][0] - shows[k - 1][0] for k in range(1, len(shows))]
    paint_judder = [abs(g - frame_ms) for g in paint_gap]
    # End-to-end on the wire timeline: paint time − capture time, relative to min (pipeline latency variation incl. pacing)
    show_lag = [t - ts / 1000.0 for t, ts in shows]
    show_lag_j = [l - min(show_lag) for l in show_lag] if show_lag else []
    last = stats[-1] if stats else {}
    summary = {
        'label': a.label, 'pd': a.pd, 'seconds': a.seconds, 'frames_in': len(ins), 'frames_shown': len(shows),
        'keyframes': len(keys),
        'bytes': {'mean_delta': statistics.mean(deltas) if deltas else None, 'mean_key': statistics.mean(keys) if keys else None,
                  'max': max(sizes) if sizes else None, 'p95': pct(sizes, .95), 'p99': pct(sizes, .99)},
        'g2g_ms': {'p50': pct(g2g, .5), 'p95': pct(g2g, .95), 'p99': pct(g2g, .99), 'max': max(g2g) if g2g else None},
        'spread_ms': {'p50': pct(spread, .5), 'p95': pct(spread, .95), 'p99': pct(spread, .99)},
        'arrival_lag_ms_over_min': {'p50': pct(lag_j, .5), 'p95': pct(lag_j, .95), 'p99': pct(lag_j, .99), 'max': max(lag_j) if lag_j else None},
        'arrival_judder_ms': {'mean': statistics.mean(arr_judder) if arr_judder else None, 'p95': pct(arr_judder, .95), 'gaps_gt_2frames': sum(1 for g in arr_gap if g > 2 * frame_ms)},
        'paint_judder_ms': {'mean': statistics.mean(paint_judder) if paint_judder else None, 'p95': pct(paint_judder, .95), 'gaps_gt_120': sum(1 for g in paint_gap if g > 120), 'gaps_gt_2frames': sum(1 for g in paint_gap if g > 2 * frame_ms)},
        'paint_lag_ms_over_min': {'p50': pct(show_lag_j, .5), 'p95': pct(show_lag_j, .95), 'max': max(show_lag_j) if show_lag_j else None},
        'stats_last': {k: last.get(k) for k in ('fps', 'kbps', 'g2gP50Ms', 'g2gP95Ms', 'spreadP95Ms', 'renderJitterMs', 'framesShown', 'framesSkippedLate', 'framesDecoded', 'framesIncomplete', 'framesTimedOut', 'lossTruePct', 'rttMs', 'presentationDelayMs', 'keyframesRequested', 'decodeQueue')},
        'agent_last': (last.get('agent') or {}),
    }
    print(json.dumps(summary, indent=1))
    if a.out:
        with open(a.out, 'w') as f: json.dump({'summary': summary, 'ins': ins, 'shows': shows, 'stats': stats}, f)
    return 0


if __name__ == '__main__':
    ap = argparse.ArgumentParser()
    ap.add_argument('--robot', required=True); ap.add_argument('--signal', default='ws://localhost:8080/ws')
    ap.add_argument('--page', default='http://localhost:8080/pilot/'); ap.add_argument('--pd', type=int, default=100)
    ap.add_argument('--qos', default='balanced'); ap.add_argument('--fps', type=float, default=30)
    ap.add_argument('--seconds', type=float, default=40); ap.add_argument('--warmup', type=float, default=6)
    ap.add_argument('--label', default=''); ap.add_argument('--out', default='')
    sys.exit(asyncio.run(run(ap.parse_args())))
