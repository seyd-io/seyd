"""
PTZ camera control adapter (Hikvision ISAPI).

**Boundary note.** This is robot-side customer code, not part of Seyd. The
daemon (`seydd`) forwards generic `ptz` command messages to a UDP port and
publishes generic publisher-control messages on another; `bridge.py` consumes
both and this module speaks ISAPI. Nothing in `packages/` knows the word
"Hikvision".

**Why `momentary` and not `continuous`.** `continuous` is a velocity command with
no expiry: the camera moves until it is explicitly told to stop. Over the public
internet, with an operator whose laptop can sleep, whose tab can be closed, and
whose WebSocket can drop mid-gesture, "stop" is a message that sometimes does not
arrive — and the failure mode is a camera that pans until it hits its stop.
`momentary` carries a duration and expires on its own, so a lost stop costs at
most one duration of extra travel. Measured on a DS-2DE2A404IWG1-E: pan=40 for
500 ms moved 1.89° and halted with no further command.

The pilot repeats while a key or mouse button is held, and each repeat renews the
window. So `_DURATION_MS` must exceed the pilot's repeat interval or motion
stutters, and must stay small enough that an abandoned gesture stops promptly.

**Latest-value-wins.** A held arrow key generates commands faster than an HTTP
round trip completes. Queueing them would build exactly the lag this project
exists to avoid, so this mirrors the video path's single-slot design: one request
in flight, one pending target, and a superseded target is discarded unsent. The
operator's most recent intent is the only one worth executing.

Pure stdlib HTTP (urllib): no dependencies to install on the demo host.
"""

import asyncio
import concurrent.futures
import logging
import re
import urllib.error
import urllib.request

log = logging.getLogger(__name__)

# Must exceed the pilot's repeat interval (PTZ_REPEAT_MS in pilot.js, 200 ms) or
# held-key motion stutters between renewals. Kept short so an abandoned gesture
# — closed tab, dropped link, sleeping laptop — stops within a fraction of a
# second rather than running to the mechanical stop.
_DURATION_MS = 600

# PTZ is a control input on a moving machine: a stale one is worse than none, so
# a request that cannot be completed quickly is abandoned rather than retried.
_TIMEOUT_S = 1.5

_PTZ_XML = ('<?xml version="1.0" encoding="UTF-8"?>'
            '<PTZData><pan>{pan}</pan><tilt>{tilt}</tilt><zoom>{zoom}</zoom>'
            '<Momentary><duration>{duration}</duration></Momentary></PTZData>')

_STOP_XML = ('<?xml version="1.0" encoding="UTF-8"?>'
             '<PTZData><pan>0</pan><tilt>0</tilt><zoom>0</zoom></PTZData>')

_ABSOLUTE_XML = ('<?xml version="1.0" encoding="UTF-8"?>'
                 '<PTZData><AbsoluteHigh><elevation>{elevation}</elevation>'
                 '<azimuth>{azimuth}</azimuth>'
                 '<absoluteZoom>{zoom}</absoluteZoom></AbsoluteHigh></PTZData>')


def _clamp(value, lo=-100, hi=100) -> int:
    try:
        return max(lo, min(hi, int(value)))
    except (TypeError, ValueError):
        return 0


class CameraControl:
    """
    Drives a Hikvision PTZ camera over ISAPI.

    Every method is best-effort and never raises into the caller: a camera that
    is unplugged, rebooting, or wrong-passworded must not take down the video
    relay, which is the part the operator actually depends on.
    """

    def __init__(self, host: str, user: str, password: str,
                 channel: int = 1, home: tuple | None = None):
        self.host    = host
        self.channel = channel
        self.home    = home          # (elevation, azimuth, zoom) in ISAPI units

        self._base = f'http://{host}/ISAPI/PTZCtrl/channels/{channel}'

        self._user     = user
        self._password = password
        self._opener   = self._build_opener()

        # Bitrate and frame rate live in the same channel document, and ISAPI
        # only offers read-modify-write of the whole thing. Without this lock an
        # interleaved GET/GET/PUT/PUT silently discards one of the two changes.
        self._doc_lock = asyncio.Lock()

        # Single worker thread: serialises requests onto the camera (it does not
        # benefit from concurrency) and bounds thread use to one regardless of
        # how fast the operator moves.
        self._pool = concurrent.futures.ThreadPoolExecutor(
            max_workers=1, thread_name_prefix='ptz')

        self._desired: tuple | None = None      # (pan, tilt, zoom) pending
        self._wake:  asyncio.Event | None = None
        self._task:  asyncio.Task  | None = None
        self._last_sent: tuple | None = None

        self.stats = {'ptz_sent': 0, 'ptz_coalesced': 0, 'ptz_errors': 0}

    # ── lifecycle ────────────────────────────────────────────────────────────

    async def start(self):
        self._wake = asyncio.Event()
        self._task = asyncio.ensure_future(self._worker())
        log.info('PTZ control → %s (channel %d)', self.host, self.channel)

    async def stop(self):
        if self._task:
            self._task.cancel()
            try:
                await self._task
            except asyncio.CancelledError:
                pass
            self._task = None
        # Leaving a camera in motion on shutdown is the one outcome worth a
        # blocking call on the way out.
        await self._request('/continuous', _STOP_XML)
        self._pool.shutdown(wait=False)

    # ── public API (vendor-neutral) ──────────────────────────────────────────

    def move(self, pan, tilt, zoom=0):
        """
        Request a PTZ velocity. Non-blocking and lossy by design.

        Overwrites any target that has not been sent yet — see the module
        docstring on latest-value-wins.
        """
        target = (_clamp(pan), _clamp(tilt), _clamp(zoom))
        if self._desired is not None:
            self.stats['ptz_coalesced'] += 1
        self._desired = target
        if self._wake:
            self._wake.set()

    async def go_home(self) -> bool:
        """Return to the configured home position, if one was given."""
        if not self.home:
            return False
        elevation, azimuth, zoom = self.home
        self._desired = None          # abandon any pending velocity
        ok = await self._request('/absolute', _ABSOLUTE_XML.format(
            elevation=elevation, azimuth=azimuth, zoom=zoom))
        log.info('PTZ home → elev %s az %s zoom %s (%s)',
                 elevation, azimuth, zoom, 'ok' if ok else 'failed')
        return ok

    async def request_keyframe(self, stream_channel: int = 101) -> bool:
        """Ask the encoder for an immediate IDR (`recovery-request` from seydd)."""
        url = f'http://{self.host}/ISAPI/Streaming/channels/{stream_channel}/requestKeyFrame'
        return await asyncio.get_event_loop().run_in_executor(
            self._pool, self._blocking_url, url, '')

    async def _read_channel_field(self, tag: str, stream_channel: int) -> int | None:
        """Read one integer field out of the streaming channel document."""
        url = f'http://{self.host}/ISAPI/Streaming/channels/{stream_channel}'
        # The channel document is slow on this firmware; give it 4 s.
        xml = await asyncio.get_event_loop().run_in_executor(
            self._pool, self._blocking_url, url, None, 4.0)
        if not xml:
            return None
        m = re.search(f'<{tag}>(\\d+)</{tag}>', xml.decode('utf-8', 'replace'))
        return int(m.group(1)) if m else None

    async def _patch_channel_field(self, tag: str, value: int, stream_channel: int) -> bool:
        """
        GET the streaming channel document, patch one field, PUT it back.

        Read-modify-write of the whole document is the only option ISAPI offers
        here, so every other encoder setting — resolution, GOP, codec — is
        carried across untouched by construction.
        """
        url = f'http://{self.host}/ISAPI/Streaming/channels/{stream_channel}'
        loop = asyncio.get_event_loop()
        async with self._doc_lock:
            xml = await loop.run_in_executor(self._pool, self._blocking_url, url, None, 4.0)
            if not xml:
                return False
            text = xml.decode('utf-8', 'replace')
            if f'<{tag}>' not in text:
                log.warning('%s: no <%s> in channel %s XML', tag, tag, stream_channel)
                return False
            patched = re.sub(f'<{tag}>\\d+</{tag}>', f'<{tag}>{int(value)}</{tag}>', text, count=1)
            return bool(await loop.run_in_executor(self._pool, self._blocking_url, url, patched, 4.0))

    async def set_bitrate_cap(self, kbps: int, stream_channel: int = 101) -> bool:
        """
        Set the stream's VBR upper cap (`video-config.maxBitrateKbps` from
        seydd's ABR).
        """
        ok = await self._patch_channel_field('vbrUpperCap', kbps, stream_channel)
        if ok:
            log.info('bitrate cap → %d kbps (channel %d)', kbps, stream_channel)
        return ok

    async def get_bitrate_cap(self, stream_channel: int = 101) -> int | None:
        return await self._read_channel_field('vbrUpperCap', stream_channel)

    async def set_max_frame_rate(self, fps: int, stream_channel: int = 101) -> bool:
        """
        Set the stream's frame-rate cap (`video-config.suggestedFps` from seydd's
        ABR), the last rung of the degradation ladder.

        ISAPI states the cap in hundredths of a frame per second, so 25 fps is
        2500, and only accepts values the channel's `capabilities` document
        lists. Unlike a resolution change, this takes effect on the *running*
        stream: no reconnect, no gap.
        """
        ok = await self._patch_channel_field('maxFrameRate', int(fps) * 100, stream_channel)
        if ok:
            log.info('frame rate cap → %d fps (channel %d)', fps, stream_channel)
        return ok

    async def get_max_frame_rate(self, stream_channel: int = 101) -> int | None:
        raw = await self._read_channel_field('maxFrameRate', stream_channel)
        return None if raw is None else raw // 100

    async def status(self) -> dict | None:
        """Current position, or None if the camera did not answer."""
        body = await self._request('/status', None)
        if not isinstance(body, bytes):
            return None
        text = body.decode('utf-8', 'replace')

        def field(name):
            start = text.find(f'<{name}>')
            if start < 0:
                return None
            start += len(name) + 2
            end = text.find(f'</{name}>', start)
            return text[start:end] if end > 0 else None

        try:
            return {
                'elevation': int(field('elevation')),
                'azimuth':   int(field('azimuth')),
                'zoom':      int(field('absoluteZoom')),
            }
        except (TypeError, ValueError):
            return None

    # ── private ──────────────────────────────────────────────────────────────

    async def _worker(self):
        """
        Send the most recent requested velocity, one request at a time.

        Reads the pending slot only between requests, so a burst of inputs
        collapses to the latest rather than queueing behind each other.
        """
        while True:
            await self._wake.wait()
            self._wake.clear()

            target = self._desired
            self._desired = None            # claim the slot
            if target is None:
                continue

            pan, tilt, zoom = target
            if pan == 0 and tilt == 0 and zoom == 0:
                # An explicit halt goes to `continuous`, which stops immediately.
                # Sending momentary(0) would instead mean "no motion for 600 ms",
                # leaving the current gesture to run out its own window.
                if self._last_sent == (0, 0, 0):
                    continue            # already stopped; don't spam the camera
                ok = await self._request('/continuous', _STOP_XML)
            else:
                ok = await self._request('/momentary', _PTZ_XML.format(
                    pan=pan, tilt=tilt, zoom=zoom, duration=_DURATION_MS))

            if ok:
                self._last_sent = target
                self.stats['ptz_sent'] += 1
            else:
                self.stats['ptz_errors'] += 1

    async def _request(self, path: str, body: str | None):
        """
        Blocking ISAPI call, off the event loop.

        Returns response bytes for a GET, True/False for a PUT. Never raises —
        the video relay must survive a dead camera.
        """
        loop = asyncio.get_event_loop()
        try:
            return await loop.run_in_executor(self._pool, self._blocking, path, body)
        except Exception as e:              # pool shutting down, etc.
            log.debug('PTZ request dropped: %s', e)
            return False

    def _build_opener(self):
        # Digest, not basic. Hikvision rejects basic auth outright, which is the
        # trap in DEMO.md's original example.
        mgr = urllib.request.HTTPPasswordMgrWithDefaultRealm()
        mgr.add_password(None, f'http://{self.host}/', self._user, self._password)
        return urllib.request.build_opener(
            urllib.request.HTTPDigestAuthHandler(mgr))

    def _blocking(self, path: str, body: str | None):
        return self._blocking_url(self._base + path, body)

    def _blocking_url(self, url: str, body: str | None, timeout: float = _TIMEOUT_S,
                      _retry: bool = True):
        path = url.rsplit('/ISAPI', 1)[-1]
        try:
            if body is None:
                req = urllib.request.Request(url, method='GET')
            else:
                req = urllib.request.Request(
                    url, data=body.encode(), method='PUT',
                    headers={'Content-Type': 'application/xml'})
            with self._opener.open(req, timeout=timeout) as resp:
                data = resp.read()
            return data if body is None else True
        except urllib.error.HTTPError as e:
            # A 401 wedges urllib's digest handler: it keeps a retry counter and,
            # once tripped, raises "digest auth failed" for every later request
            # even after the camera is happy again. Hikvision trips it routinely
            # by locking a host out for ~30 min after a few failed logins (see
            # DEMO.md), so an unattended demo would lose PTZ until restarted.
            # Throw the handler away and try once more with a clean one.
            if e.code == 401 and _retry:
                self._opener = self._build_opener()
                return self._blocking_url(url, body, timeout, _retry=False)
            log.warning('PTZ %s → HTTP %s', path, e.code)
            return False
        except Exception as e:
            log.warning('PTZ %s failed: %s', path, e)
            return False
