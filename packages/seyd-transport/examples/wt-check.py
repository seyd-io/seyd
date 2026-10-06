#!/usr/bin/env python3
# Copyright 2026 Anton Gravestam
# SPDX-License-Identifier: Apache-2.0
"""
WebTransport spike check: real Chrome ↔ seyd-transport echo server.

    cargo build -p seyd-transport --example wt-server
    python3 packages/seyd-transport/examples/wt-check.py

Starts examples/wt-server, drives headless Chrome over the DevTools Protocol
(plain WebSocket, no puppeteer — same approach as tools/cdp.py), opens
a WebTransport session pinned by serverCertificateHashes, sends 20 datagrams
and 3 control lines, and asserts every echo returns. Prints datagram RTT.
"""
import asyncio, functools, http.server, json, os, shutil, socketserver, subprocess
import sys, tempfile, threading, urllib.request

import websockets

CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'
ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))))
PORT = 4433

PAGE = r"""<!doctype html><title>wt-check</title><script>
window.result = null;
(async () => {
  const q = new URLSearchParams(location.search);
  const fp = q.get('fp'), port = q.get('port');
  const hash = Uint8Array.from(fp.match(/../g).map(h => parseInt(h, 16)));
  const log = [];
  try {
    const t0 = performance.now();
    const wt = new WebTransport(`https://127.0.0.1:${port}/seyd`,
        { serverCertificateHashes: [{ algorithm: 'sha-256', value: hash }] });
    await wt.ready;
    const tReady = performance.now() - t0;
    // datagrams
    const writer = wt.datagrams.writable.getWriter();
    const reader = wt.datagrams.readable.getReader();
    const sent = new Map();
    const rtts = [];
    let got = 0;
    const recv = (async () => {
      while (got < 20) {
        const { value, done } = await reader.read();
        if (done) break;
        const txt = new TextDecoder().decode(value);
        const m = txt.match(/^dg-(\d+):/);
        if (m && sent.has(m[1])) { rtts.push(performance.now() - sent.get(m[1])); got++; }
      }
    })();
    for (let i = 0; i < 20; i++) {
      const payload = `dg-${i}:` + 'x'.repeat(900);
      sent.set(String(i), performance.now());
      await writer.write(new TextEncoder().encode(payload));
    }
    await Promise.race([recv, new Promise(r => setTimeout(r, 3000))]);
    // control stream
    const bidi = await wt.createBidirectionalStream();
    const w = bidi.writable.getWriter();
    const r = bidi.readable.getReader();
    const lines = ['{"type":"hello","proto":2}', '{"type":"ping","t1":1}', '{"type":"bye"}'];
    for (const l of lines) await w.write(new TextEncoder().encode(l + '\n'));
    let buf = '', echoes = [];
    const dec = new TextDecoder();
    const readLines = (async () => {
      while (echoes.length < 3) {
        const { value, done } = await r.read();
        if (done) break;
        buf += dec.decode(value, { stream: true });
        const parts = buf.split('\n'); buf = parts.pop();
        for (const p of parts) if (p.trim()) echoes.push(JSON.parse(p));
      }
    })();
    await Promise.race([readLines, new Promise(r => setTimeout(r, 3000))]);
    rtts.sort((a, b) => a - b);
    window.result = { ok: got === 20 && echoes.length === 3, readyMs: tReady, datagramsEchoed: got,
      rttMs: { min: rtts[0], median: rtts[Math.floor(rtts.length / 2)], max: rtts[rtts.length - 1] },
      echoes, maxDatagramSize: wt.datagrams.maxDatagramSize };
    wt.close();
  } catch (e) {
    window.result = { ok: false, error: String(e), log };
  }
})();
</script>"""


class CDP:
    def __init__(self, ws):
        self.ws, self._id = ws, 0

    async def call(self, method, **params):
        self._id += 1
        await self.ws.send(json.dumps({'id': self._id, 'method': method, 'params': params}))
        while True:
            raw = json.loads(await self.ws.recv())
            if raw.get('id') == self._id:
                if 'error' in raw:
                    raise RuntimeError(f"{method}: {raw['error']}")
                return raw.get('result', {})

    async def eval(self, expr):
        r = await self.call('Runtime.evaluate', expression=expr, returnByValue=True, awaitPromise=True)
        return r.get('result', {}).get('value')


def serve_page(directory):
    handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=directory)

    class Quiet(socketserver.TCPServer):
        allow_reuse_address = True

        def handle_error(self, request, addr):
            pass

    httpd = Quiet(('127.0.0.1', 0), handler)
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    return httpd, httpd.server_address[1]


async def main():
    if not os.path.exists(CHROME):
        print(f'FAIL: Chrome not found at {CHROME}'); return 1
    exe = os.path.join(ROOT, 'target', 'debug', 'examples', 'wt-server')
    if not os.path.exists(exe):
        print('FAIL: build first: cargo build -p seyd-transport --example wt-server'); return 1
    server_log = open(os.path.join(tempfile.gettempdir(), 'seyd-wt-server.log'), 'w')
    fp = os.environ.get('SEYD_WT_ATTACH')      # fingerprint of an already-running server
    server = None
    if not fp:
        server = subprocess.Popen([exe, str(PORT)], stdout=subprocess.PIPE, stderr=server_log, text=True)
        for _ in range(50):
            line = server.stdout.readline()
            if line.startswith('FINGERPRINT '):
                fp = line.split()[1]
            if line.startswith('LISTENING'):
                break
    if not fp:
        print('FAIL: server did not print a fingerprint'); server.kill(); return 1
    print(f'→ server up, fingerprint {fp[:16]}…')

    www = tempfile.mkdtemp(prefix='seyd-wt-')
    with open(os.path.join(www, 'index.html'), 'w') as f:
        f.write(PAGE)
    httpd, port = serve_page(www)
    profile = tempfile.mkdtemp(prefix='seyd-wt-profile-')
    chrome = subprocess.Popen([
        CHROME, '--headless=new', '--remote-debugging-port=0', f'--user-data-dir={profile}',
        '--no-first-run', '--no-default-browser-check', '--disable-gpu',
    ] + ([f"--log-net-log={os.environ['SEYD_WT_NETLOG']}", '--net-log-capture-mode=Everything']
         if os.environ.get('SEYD_WT_NETLOG') else []) + ['about:blank'], stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    ws_url = None
    for _ in range(80):
        line = chrome.stdout.readline()
        if not line:
            await asyncio.sleep(0.1); continue
        if 'ws://' in line:
            ws_url = 'ws://' + line.strip().split('ws://', 1)[1]; break
    result, rc = None, 1
    try:
        if not ws_url:
            print('FAIL: no devtools endpoint'); return 1
        base = ws_url.replace('ws://', 'http://').rsplit('/devtools', 1)[0]
        info = json.loads(urllib.request.urlopen(base + '/json/list').read())
        page_ws = next(t['webSocketDebuggerUrl'] for t in info if t['type'] == 'page')
        async with websockets.connect(page_ws, max_size=None) as ws:
            cdp = CDP(ws)
            await cdp.call('Runtime.enable')
            await cdp.call('Page.enable')
            await cdp.call('Page.navigate', url=f'http://localhost:{port}/index.html?fp={fp}&port={PORT}')
            for _ in range(100):
                await asyncio.sleep(0.2)
                result = await cdp.eval('JSON.stringify(window.result)')
                if result and result != 'null':
                    result = json.loads(result); break
        print(json.dumps(result, indent=1))
        rc = 0 if result and result.get('ok') else 1
        print('PASS' if rc == 0 else 'FAIL')
        if rc:
            server_log.flush()
            print('--- server log tail ---')
            print(''.join(open(server_log.name).readlines()[-40:]))
    finally:
        chrome.kill(); httpd.shutdown()
        if server: server.kill()
        shutil.rmtree(profile, ignore_errors=True); shutil.rmtree(www, ignore_errors=True)
    return rc


if __name__ == '__main__':
    sys.exit(asyncio.run(main()))
