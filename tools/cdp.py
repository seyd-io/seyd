"""
Minimal Chrome DevTools Protocol helper shared by the Seyd test tools.

Drives a real headless Chrome over its raw DevTools WebSocket — no puppeteer,
no playwright — because everything unique to the pilot path (WebTransport,
WebCodecs, the candidate race) only exists in a browser.

    python3 -m venv tools/.venv && tools/.venv/bin/pip install websockets
"""
import asyncio
import json
import shutil
import subprocess
import tempfile
import urllib.request

CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'

_KEYS = {
    'ArrowLeft': (37, 'ArrowLeft'), 'ArrowUp': (38, 'ArrowUp'),
    'ArrowRight': (39, 'ArrowRight'), 'ArrowDown': (40, 'ArrowDown'),
    'Space': (32, ' '), 'KeyH': (72, 'h'), 'KeyS': (83, 's'),
}


class CDP:
    """Minimal DevTools Protocol client over an open websocket."""

    def __init__(self, ws):
        self.ws = ws
        self._id = 0
        self.events = []

    async def call(self, method, **params):
        self._id += 1
        msg_id = self._id
        await self.ws.send(json.dumps({'id': msg_id, 'method': method, 'params': params}))
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
        r = await self.call('Runtime.evaluate', expression=expr, returnByValue=True, awaitPromise=True)
        return r.get('result', {}).get('value')

    async def key(self, code, down=True):
        vk, key = _KEYS[code]
        await self.call('Input.dispatchKeyEvent', type='keyDown' if down else 'keyUp',
                        key=key, code=code, windowsVirtualKeyCode=vk, nativeVirtualKeyCode=vk)


def launch_chrome(extra_flags=()):
    """Start headless Chrome; returns (process, profile_dir, page_websocket_url)."""
    profile = tempfile.mkdtemp(prefix='seyd-cdp-')
    chrome = subprocess.Popen(
        [CHROME, '--headless=new', '--remote-debugging-port=0', f'--user-data-dir={profile}',
         '--no-first-run', '--no-default-browser-check', '--disable-gpu', '--window-size=1280,800',
         *extra_flags, 'about:blank'],
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    ws_url = None
    for _ in range(80):
        line = chrome.stdout.readline()
        if 'ws://' in line:
            ws_url = 'ws://' + line.strip().split('ws://', 1)[1]
            break
    if not ws_url:
        chrome.kill()
        raise RuntimeError('Chrome did not print its DevTools URL')
    base = ws_url.replace('ws://', 'http://').rsplit('/devtools', 1)[0]
    info = json.loads(urllib.request.urlopen(base + '/json/list').read())
    page_ws = next(t['webSocketDebuggerUrl'] for t in info if t['type'] == 'page')
    return chrome, profile, page_ws


def stop_chrome(chrome, profile):
    chrome.kill()
    shutil.rmtree(profile, ignore_errors=True)


def camera_azimuth(ip, user, password):
    """Read a Hikvision camera's azimuth over ISAPI (test oracle for PTZ)."""
    mgr = urllib.request.HTTPPasswordMgrWithDefaultRealm()
    mgr.add_password(None, f'http://{ip}/', user, password)
    opener = urllib.request.build_opener(urllib.request.HTTPDigestAuthHandler(mgr))
    try:
        with opener.open(f'http://{ip}/ISAPI/PTZCtrl/channels/1/status', timeout=5) as r:
            text = r.read().decode()
        i = text.find('<azimuth>')
        return int(text[i + 9:text.find('</azimuth>')]) if i >= 0 else None
    except Exception as e:
        print(f'  (could not read camera azimuth: {e})')
        return None
