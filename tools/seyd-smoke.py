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

Uses the raw DevTools Protocol helper from tools/pilot-smoke.py (no puppeteer).
"""
import argparse, asyncio, importlib.util, json, os, shutil, socket, subprocess, sys, tempfile, threading, time, urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location('smoke', os.path.join(HERE, 'pilot-smoke.py'))
smoke = importlib.util.module_from_spec(spec); spec.loader.exec_module(smoke)
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
    threading.Thread(target=udp_collector, args=(args.command_port, collected), daemon=True).start()
    profile = tempfile.mkdtemp(prefix='seyd-smoke-')
    chrome = subprocess.Popen([smoke.CHROME, '--headless=new', '--remote-debugging-port=0', f'--user-data-dir={profile}',
        '--no-first-run', '--no-default-browser-check', '--disable-gpu', '--window-size=1280,800', 'about:blank'],
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    ws_url = None
    for _ in range(80):
        line = chrome.stdout.readline()
        if 'ws://' in line: ws_url = 'ws://' + line.strip().split('ws://', 1)[1]; break
    base = ws_url.replace('ws://', 'http://').rsplit('/devtools', 1)[0]
    info = json.loads(urllib.request.urlopen(base + '/json/list').read())
    page_ws = next(t['webSocketDebuggerUrl'] for t in info if t['type'] == 'page')
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
        sensor = await cdp.eval("document.getElementById('sensor')?.textContent")
        check(sensor and 'telemetry' in sensor, f'sensor text = {sensor!r}')
        role = await cdp.eval("document.getElementById('video').session?.role")
        has_ptz = await cdp.eval("document.getElementById('video').session?.hasCommandChannel('ptz')")
        check(role == 'driver' and has_ptz, f'role={role!r} ptz channel={has_ptz}')
        before = len(collected.get('msgs', []))
        await cdp.call('Input.dispatchKeyEvent', type='keyDown', key='ArrowRight', code='ArrowRight', windowsVirtualKeyCode=39)
        await cdp.pump(0.6)
        await cdp.call('Input.dispatchKeyEvent', type='keyUp', key='ArrowRight', code='ArrowRight', windowsVirtualKeyCode=39)
        await cdp.pump(0.5)
        msgs = collected.get('msgs', [])[before:]
        pans = [json.loads(m).get('pan') for m in msgs if m.startswith('{')]
        check(any(p and p > 0 for p in pans) and pans and pans[-1] == 0, f'ptz on udp:{args.command_port}: {len(msgs)} msgs, pans={pans[:6]}..{pans[-1:] if pans else []}')
        exceptions = [e for e in cdp.events if e['method'] == 'Runtime.exceptionThrown']
        check(not exceptions, f'no uncaught exceptions ({len(exceptions)})')
        for e in exceptions[:3]: print('   ', e['params']['exceptionDetails'].get('exception', {}).get('description', '')[:300])
    collected['stop'] = True
    chrome.kill(); shutil.rmtree(profile, ignore_errors=True)
    print('RESULT', 'PASS' if ok else 'FAIL')
    return 0 if ok else 1


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--robot', default='seyd-demo')
    ap.add_argument('--page', default='http://localhost:8080/')
    ap.add_argument('--signal', default='ws://localhost:8080/ws')
    ap.add_argument('--command-port', type=int, default=5004)
    ap.add_argument('--timeout', type=float, default=15)
    ap.add_argument('--query', default='', help='extra page query, e.g. loss=0.05&burst=3')
    sys.exit(asyncio.run(run(ap.parse_args())))


if __name__ == '__main__':
    main()
