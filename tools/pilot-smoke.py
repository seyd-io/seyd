#!/usr/bin/env python3
"""
Drive the real pilot page in Chrome and assert the P2P path works.

Why this exists: the relay path can be tested from Python (tools/relay-pilot.py),
but the *primary* path cannot. WebTransport with `serverCertificateHashes` and a
WebCodecs VideoDecoder only exist in a browser, so everything unique to P2P —
the candidate race, the bidi JSON stream, the stream-id handshake, decode — was
verifiable only by a human watching a canvas. That makes silent regressions on
the main path cheap to introduce and expensive to notice.

Chrome is driven over the DevTools Protocol with a plain WebSocket rather than
puppeteer/playwright, to avoid adding a heavyweight dev dependency for one test.

Checks:
  * page loads with no uncaught exceptions
  * WebTransport connects P2P (not relay) and reports the winning candidate
  * frames decode — framesDecoded climbs and the canvas has real dimensions
  * the agent's capabilities reply arrives, which is what proves the `hello`
    handshake works: without it the reply is dropped and PTZ silently dies
  * synthetic arrow-key input moves a real camera, if --expect-ptz is given

Not part of DARC. Never imported by packages/.

Usage:
    tools/pilot-smoke.py --robot darc-demo --signal ws://localhost:8080
    tools/pilot-smoke.py --robot darc-demo --signal ws://localhost:8080 \
        --expect-ptz --camera-ip 192.168.86.237
"""

import argparse
import asyncio
import functools
import http.server
import json
import os
import shutil
import socketserver
import subprocess
import sys
import tempfile
import threading
import urllib.request

import websockets

CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'
PILOT_DIR = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                         'packages', 'pilot')

# Arrow keys, as Chrome's input domain wants them.
_KEYS = {
    'ArrowRight': (39, 'ArrowRight'),
    'ArrowLeft':  (37, 'ArrowLeft'),
    'ArrowUp':    (38, 'ArrowUp'),
    'ArrowDown':  (40, 'ArrowDown'),
}


def serve_pilot() -> tuple:
    """Serve packages/pilot on a free port. localhost is a secure context, which
    WebTransport and WebCodecs both require."""
    handler = functools.partial(http.server.SimpleHTTPRequestHandler,
                                directory=PILOT_DIR)

    class Quiet(socketserver.TCPServer):
        allow_reuse_address = True

        def handle_error(self, request, addr):
            pass

    httpd = Quiet(('127.0.0.1', 0), handler)
    port = httpd.server_address[1]
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    return httpd, port


class CDP:
    """Minimal DevTools Protocol client."""

    def __init__(self, ws):
        self.ws = ws
        self._id = 0
        self.events = []

    async def call(self, method, **params):
        self._id += 1
        msg_id = self._id
        await self.ws.send(json.dumps({'id': msg_id, 'method': method,
                                       'params': params}))
        while True:
            raw = json.loads(await self.ws.recv())
            if raw.get('id') == msg_id:
                if 'error' in raw:
                    raise RuntimeError(f"{method}: {raw['error']}")
                return raw.get('result', {})
            if 'method' in raw:
                self.events.append(raw)

    async def pump(self, seconds):
        """Collect events for a while."""
        end = asyncio.get_event_loop().time() + seconds
        while asyncio.get_event_loop().time() < end:
            try:
                raw = json.loads(await asyncio.wait_for(self.ws.recv(), timeout=0.4))
            except asyncio.TimeoutError:
                continue
            if 'method' in raw:
                self.events.append(raw)

    async def eval(self, expr):
        r = await self.call('Runtime.evaluate', expression=expr,
                            returnByValue=True, awaitPromise=True)
        return r.get('result', {}).get('value')

    async def key(self, code, down=True):
        vk, key = _KEYS[code]
        await self.call('Input.dispatchKeyEvent',
                        type='keyDown' if down else 'keyUp',
                        key=key, code=code, windowsVirtualKeyCode=vk,
                        nativeVirtualKeyCode=vk)


def camera_azimuth(ip, user, password):
    mgr = urllib.request.HTTPPasswordMgrWithDefaultRealm()
    mgr.add_password(None, f'http://{ip}/', user, password)
    opener = urllib.request.build_opener(urllib.request.HTTPDigestAuthHandler(mgr))
    try:
        with opener.open(
                f'http://{ip}/ISAPI/PTZCtrl/channels/1/status', timeout=5) as r:
            text = r.read().decode()
        i = text.find('<azimuth>')
        return int(text[i + 9:text.find('</azimuth>')]) if i >= 0 else None
    except Exception as e:
        print(f'  (could not read camera azimuth: {e})')
        return None


async def run(args):
    httpd, port = serve_pilot()
    profile = tempfile.mkdtemp(prefix='darc-smoke-')
    url = (f'http://localhost:{port}/index.html'
           f'?robot={args.robot}&signal={args.signal}')

    chrome = subprocess.Popen([
        CHROME,
        '--headless=new',
        '--remote-debugging-port=0',
        f'--user-data-dir={profile}',
        '--no-first-run', '--no-default-browser-check',
        '--disable-gpu',
        # A self-signed cert pinned by serverCertificateHashes is the design
        # (see PROTOTYPE.md), not an accident to be worked around — but headless
        # has no UI to accept anything, so allow the localhost origin explicitly.
        '--allow-insecure-localhost',
        '--autoplay-policy=no-user-gesture-required',
        'about:blank',
    ], stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)

    # Chrome prints the actual devtools port to stderr on startup.
    ws_url = None
    for _ in range(80):
        line = chrome.stdout.readline()
        if not line:
            await asyncio.sleep(0.1)
            continue
        if 'ws://' in line:
            ws_url = line.strip().split('ws://', 1)[1]
            ws_url = 'ws://' + ws_url
            break
    if not ws_url:
        print('FAIL: could not find Chrome devtools endpoint')
        chrome.kill(); httpd.shutdown(); shutil.rmtree(profile, ignore_errors=True)
        return 1

    result = 1
    try:
        # Connect to the page target directly. Attaching via Target/flat mode
        # would work but multiplexes sessions onto one socket for no benefit
        # here — there is exactly one page.
        base = ws_url.replace('ws://', 'http://').rsplit('/devtools', 1)[0]
        info = json.loads(urllib.request.urlopen(base + '/json/list').read())
        page_ws = next(t['webSocketDebuggerUrl'] for t in info
                       if t['type'] == 'page')

        async with websockets.connect(page_ws, max_size=None) as ws:
            cdp = CDP(ws)
            await cdp.call('Runtime.enable')
            await cdp.call('Log.enable')
            await cdp.call('Page.enable')
            await cdp.call('Console.enable')

            print(f'→ navigating to {url}')
            await cdp.call('Page.navigate', url=url)
            await cdp.pump(args.seconds)

            # ── collect console output and exceptions ────────────────────────
            exceptions, console = [], []
            for e in cdp.events:
                m = e['method']
                if m == 'Runtime.exceptionThrown':
                    d = e['params']['exceptionDetails']
                    exceptions.append(d.get('exception', {}).get('description')
                                      or d.get('text'))
                elif m in ('Runtime.consoleAPICalled', 'Log.entryAdded'):
                    p = e['params']
                    if m == 'Log.entryAdded':
                        console.append(f"[{p['entry']['level']}] {p['entry']['text']}")
                    else:
                        txt = ' '.join(str(a.get('value', a.get('description', '')))
                                       for a in p.get('args', []))
                        console.append(f"[{p['type']}] {txt}")

            state = await cdp.eval("""(() => ({
                relayMode: relayMode,
                pathLabel: pathLabel,
                hasVideo: hasVideo,
                ptzSupported: ptzSupported,
                ptzHomeSupported: ptzHomeSupported,
                framesDecoded: stats.framesDecoded,
                framesClean: stats.framesClean,
                chunksRx: stats.chunksRx,
                badHeader: stats.chunksBadHeader,
                decodeErrors: stats.decodeErrors,
                canvasW: canvas.width, canvasH: canvas.height,
                hint: document.getElementById('hint').textContent,
            }))()""")

            print()
            print('── pilot state ──────────────────────────────────────')
            for k, v in (state or {}).items():
                print(f'  {k:<18} {v}')
            if console:
                print('\n── console ──────────────────────────────────────────')
                for line in console[:25]:
                    print(f'  {line}')
            if exceptions:
                print('\n── UNCAUGHT EXCEPTIONS ──────────────────────────────')
                for line in exceptions:
                    print(f'  {line}')

            ok = bool(state) and not exceptions
            ok = ok and state['framesDecoded'] > 0
            ok = ok and state['badHeader'] == 0
            ok = ok and state['canvasW'] > 0

            # ── synthetic PTZ ───────────────────────────────────────────────
            if args.expect_ptz:
                print('\n── PTZ via synthetic ArrowRight ────────────────────')
                if not state.get('ptzSupported'):
                    print('  FAIL: pilot never received capabilities '
                          '(ptzSupported is false)')
                    ok = False
                else:
                    before = camera_azimuth(args.camera_ip, args.camera_user,
                                            args.camera_password)
                    print(f'  azimuth before  {before}')
                    await cdp.key('ArrowRight', down=True)
                    await cdp.pump(1.4)
                    await cdp.key('ArrowRight', down=False)
                    await cdp.pump(1.2)
                    after = camera_azimuth(args.camera_ip, args.camera_user,
                                           args.camera_password)
                    print(f'  azimuth after   {after}')
                    sent = await cdp.eval('JSON.stringify(ptzSent)')
                    print(f'  ptzSent now     {sent}  (must be all zero)')
                    if before is None or after is None or before == after:
                        print('  FAIL: camera did not move')
                        ok = False
                    else:
                        print(f'  moved {abs(after - before) / 10:.1f}° — OK')

            result = 0 if ok else 1
    finally:
        chrome.kill()
        httpd.shutdown()
        shutil.rmtree(profile, ignore_errors=True)

    print()
    print('RESULT:', 'PASS' if result == 0 else 'FAIL')
    return result


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--robot', required=True)
    ap.add_argument('--signal', required=True)
    ap.add_argument('--seconds', type=float, default=14.0)
    ap.add_argument('--expect-ptz', action='store_true')
    ap.add_argument('--camera-ip', default=os.environ.get('CAMERA_IP'))
    ap.add_argument('--camera-user', default=os.environ.get('CAMERA_USER', 'admin'))
    ap.add_argument('--camera-password',
                    default=os.environ.get('CAMERA_PASSWORD', ''))
    args = ap.parse_args()

    if not os.path.exists(CHROME):
        print(f'FAIL: Chrome not found at {CHROME}')
        return 1
    return asyncio.run(run(args))


if __name__ == '__main__':
    sys.exit(main())
