#!/usr/bin/env python3
"""
End-to-end smoke test for the Seyd stack: drives the real demo page in
headless Chrome against a running signal server + seydd, and asserts that

  * the pilot reaches state 'connected' over WebTransport (P2P, no relay exists)
  * frames decode and keep decoding
  * sensor messages arrive on the page
  * a PTZ key press reaches the robot's generic UDP command output (:5004)

    tools/seyd-smoke.py --robot sim --page http://localhost:8080/ \
        --signal ws://localhost:8080/ws --command-port 5004

Uses tools/cdp.py (raw DevTools Protocol, no puppeteer). Needs `websockets`:
    python3 -m venv tools/.venv && tools/.venv/bin/pip install websockets
"""
import argparse, asyncio, importlib.util, json, os, shutil, socket, subprocess, sys, tempfile, threading, time, urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import cdp as smoke  # noqa: E402
import websockets  # noqa: E402


def udp_collector(port, out):
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.bind(('127.0.0.1', port)); s.settimeout(0.5)
    while not out.get('stop'):
        try:
            data, _ = s.recvfrom(65536)
            out.setdefault('msgs', []).append(data.decode('utf-8', 'replace'))
        except socket.timeout:
            pass


async def run(args):
    collected = {}
    if not args.camera_ip:
        threading.Thread(target=udp_collector, args=(args.command_port, collected), daemon=True).start()
    chrome, profile, page_ws = smoke.launch_chrome()
    ok = True
    def check(cond, label):
        nonlocal ok
        print(('  ok   ' if cond else '  FAIL ') + label)
        ok = ok and bool(cond)
    async with websockets.connect(page_ws, max_size=None) as ws:
        cdp = smoke.CDP(ws)
        for m in ('Runtime.enable', 'Log.enable', 'Page.enable', 'Console.enable'): await cdp.call(m)
        url = f'{args.page.rstrip("/")}/?robot={args.robot}&signal={args.signal}' + (('&' + args.query) if args.query else '')
        print('page', url)
        await cdp.call('Page.navigate', url=url)
        state = None
        for _ in range(int(args.timeout * 4)):
            await cdp.pump(0.25)
            state = await cdp.eval("document.getElementById('video').session?.state")
            if state in ('connected', 'p2p-failed'): break
        check(state == 'connected', f'session state = {state!r}')
        if state != 'connected':
            fail = await cdp.eval("JSON.stringify(document.getElementById('video').session?.lastFailure ?? null)")
            print('  failure:', fail)
        await cdp.pump(3)
        s1 = await cdp.eval("JSON.stringify(document.getElementById('video').session?.lastStats ?? null)")
        await cdp.pump(2)
        s2 = await cdp.eval("JSON.stringify(document.getElementById('video').session?.lastStats ?? null)")
        st1, st2 = json.loads(s1 or 'null') or {}, json.loads(s2 or 'null') or {}
        fd1, fd2 = st1.get('framesDecoded', 0), st2.get('framesDecoded', 0)
        check(fd2 > fd1 > 0, f'frames decoding: {fd1} -> {fd2} (path={st2.get("path")}, fps={st2.get("fps")}, kbps={st2.get("kbps")}, lossTrue={st2.get("lossTrue")}, g2g p50={st2.get("g2gP50")})')
        size = await cdp.eval("document.getElementById('video').canvas?.width + 'x' + document.getElementById('video').canvas?.height")
        print('  canvas', size)
        if not args.no_sensor:
            sensor = await cdp.eval("document.getElementById('sensor')?.textContent")
            check(sensor and 'telemetry' in sensor, f'sensor text = {sensor!r}')
        role = await cdp.eval("document.getElementById('video').session?.role")
        badge = await cdp.eval("(function(){const b=document.getElementById('role'); return b && !b.hidden ? b.textContent : null})()")
        print('  badge', repr(badge))
        has_ptz = await cdp.eval("document.getElementById('video').session?.hasCommandChannel('ptz')")
        check(role == 'driver' and has_ptz, f'role={role!r} ptz channel={has_ptz}')
        before = len(collected.get('msgs', []))
        az0 = smoke.camera_azimuth(args.camera_ip, os.getenv('CAMERA_USER', 'admin'), os.getenv('CAMERA_PASSWORD', '')) if args.camera_ip else None
        await cdp.call('Input.dispatchKeyEvent', type='keyDown', key='ArrowRight', code='ArrowRight', windowsVirtualKeyCode=39)
        await cdp.pump(1.0)
        await cdp.call('Input.dispatchKeyEvent', type='keyUp', key='ArrowRight', code='ArrowRight', windowsVirtualKeyCode=39)
        await cdp.pump(1.5)
        if args.camera_ip:
            az1 = smoke.camera_azimuth(args.camera_ip, os.getenv('CAMERA_USER', 'admin'), os.getenv('CAMERA_PASSWORD', ''))
            moved = az0 is not None and az1 is not None and az0 != az1
            check(moved, f'camera azimuth moved on ArrowRight: {az0} -> {az1}')
        else:
            msgs = collected.get('msgs', [])[before:]
            pans = [json.loads(m).get('pan') for m in msgs if m.startswith('{')]
            check(any(p and p > 0 for p in pans) and pans and pans[-1] == 0, f'ptz on udp:{args.command_port}: {len(msgs)} msgs, pans={pans[:6]}..{pans[-1:] if pans else []}')
        if args.record > 0:
            print(f'  recording lastStats to {args.record_file} for {args.record:.0f}s (Ctrl-C to stop early)')
            t0 = time.time()
            with open(args.record_file, 'a') as f:
                try:
                    while time.time() - t0 < args.record:
                        await cdp.pump(1.0)
                        raw = await cdp.eval("JSON.stringify(document.getElementById('video').session?.lastStats ?? null)")
                        st = json.loads(raw or 'null') or {}
                        st['t'] = round(time.time() - t0, 1); st['state'] = await cdp.eval("document.getElementById('video').session?.state")
                        f.write(json.dumps(st) + '\n'); f.flush()
                        if int(st['t']) % 10 == 0:
                            print(f"  t={st['t']:>5} path={st.get('path')} fps={st.get('fps')} kbps={st.get('kbps')} loss={st.get('lossTrue')} g2g={st.get('g2gP50')}/{st.get('g2gP95')} rtt={st.get('rttMs')} lost={st.get('framesIncomplete')} rec={st.get('framesRecovered')}")
                except KeyboardInterrupt:
                    pass
        exceptions = [e for e in cdp.events if e['method'] == 'Runtime.exceptionThrown']
        check(not exceptions, f'no uncaught exceptions ({len(exceptions)})')
        for e in exceptions[:3]: print('   ', e['params']['exceptionDetails'].get('exception', {}).get('description', '')[:300])
    collected['stop'] = True
    smoke.stop_chrome(chrome, profile)
    print('RESULT', 'PASS' if ok else 'FAIL')
    return 0 if ok else 1


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--robot', default='seyd-demo')
    ap.add_argument('--page', default='http://localhost:8080/pilot/')
    ap.add_argument('--signal', default='ws://localhost:8080/ws')
    ap.add_argument('--command-port', type=int, default=5004)
    ap.add_argument('--timeout', type=float, default=15)
    ap.add_argument('--query', default='', help='extra page query, e.g. loss=0.05&burst=3')
    ap.add_argument('--no-sensor', action='store_true', help='robot has no sensor channel')
    ap.add_argument('--record', type=float, default=0, metavar='SECONDS', help='after the checks, keep the session open and append lastStats once per second to --record-file (field tests)')
    ap.add_argument('--record-file', default='seyd-record.jsonl')
    ap.add_argument('--camera-ip', default=None, help='verify PTZ by reading this Hikvision camera\'s azimuth (CAMERA_USER/PASSWORD env)')
    sys.exit(asyncio.run(run(ap.parse_args())))


if __name__ == '__main__':
    main()
