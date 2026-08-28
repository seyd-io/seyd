import asyncio
import json
import logging
import os
import re
import tempfile

import av

import fec
import qos

log = logging.getLogger(__name__)

# Default source framerate, used only to convert the profile's backlog threshold
# from frame-times into bytes. Being wrong here just scales the drop threshold.
# Overridable per source because an IP camera is often not 30 fps — the demo
# camera's sensor is PAL and caps at 25.
_ASSUMED_FPS = 30

# SDP descriptor for the local RTP video source.
_VIDEO_SDP_TEMPLATE = """\
v=0
o=- 0 0 IN IP4 127.0.0.1
s=DARC Video Source
c=IN IP4 127.0.0.1
t=0 0
m=video {port} RTP/AVP 96
a=rtpmap:96 H264/90000
a=fmtp:96 packetization-mode=1
a=framerate:30
"""

# rtsp://user:pass@host/path — the credentials sit in the URL, so anything that
# might carry one to a log has to go through here first. PyAV/libavformat puts
# the full URL into its exception messages, which is the non-obvious leak: the
# password ends up in the error path even when the happy path is careful.
_CREDS_IN_URL = re.compile(r'(?<=://)[^/@\s]+:[^/@\s]+(?=@)')


def _redact(text) -> str:
    return _CREDS_IN_URL.sub('***:***', str(text))


class _SensorProtocol(asyncio.DatagramProtocol):
    def __init__(self, on_data):
        self._on_data = on_data

    def datagram_received(self, data, addr):
        self._on_data(data)

    def error_received(self, exc):
        log.warning('sensor UDP error: %s', exc)


class Relay:
    """
    Subscribes to local UDP video (RTP H.264) and sensor streams and relays
    them to the connected pilot over WebTransport.

    Video is forwarded as chunked QUIC DATAGRAM frames with Reed-Solomon parity
    appended — see fec.py for the wire format and the reasoning.

    **Frame-granular latency control.** Only the latest frame is ever sent, but
    the unit of dropping is a whole frame, decided *before* the first chunk goes
    out. An earlier version abandoned frames mid-send and flushed the datagram
    queue ahead of every frame; both emitted partial frames, which is the worst
    available outcome — the bandwidth was already spent, the pilot must discard
    the frame anyway, and because H.264 delta frames reference their
    predecessors, one torn frame corrupts every later frame until the next
    keyframe. Skipping a frame cleanly costs one frame; tearing one costs a GOP.

    Keyframes are never dropped. They are the only case where discarding the
    pending queue is free, since an IDR makes queued chunks from the previous
    GOP irrelevant by definition.

    Sensor data is forwarded as JSON: {"type": "sensor", "data": "42"}
    """

    def __init__(self, video_port: int, sensor_port: int,
                 profile: qos.QoSProfile | None = None,
                 video_url: str | None = None,
                 fps: int = _ASSUMED_FPS):
        self.video_port  = video_port
        self.sensor_port = sensor_port
        self.profile     = profile or qos.get(None)
        # When set, video is pulled from this URL (RTSP from an IP camera)
        # instead of from a local RTP/UDP socket. Everything downstream of
        # container.demux() is identical — see _blocking_video_relay.
        self.video_url   = video_url
        self.fps         = fps or _ASSUMED_FPS

        # Set by agent.py when a pilot connects / disconnects.
        self.send_batch    = None  # async (chunks: list[bytes]) -> None
        self.send_json     = None  # async (msg: dict)           -> None
        self.pending_bytes = None  # sync  () -> int
        self.drop_pending  = None  # sync  () -> None  (None on the relay path)
        self.link_stats    = None  # sync  () -> dict
        self.on_qos        = None  # async (profile: str) -> dict  (ack)
        self.on_ptz        = None  # sync  (pan, tilt, zoom) -> None
        self.on_ptz_home   = None  # async () -> bool
        self.on_hello      = None  # async () -> None  (pilot announced itself)

        self._sensor_transport = None
        self._video_task       = None
        self._sender_task      = None
        self._stats_task       = None
        self._sdp_tmp          = None

        # Single-slot latest-frame buffer shared between the PyAV thread and
        # the asyncio _frame_sender coroutine.
        self._latest_frame: tuple | None = None  # (fid, is_keyframe, payload)
        self._frame_event:  asyncio.Event | None = None
        self._frame_id = 0

        # Counters. The pilot can only see what arrived; pairing these with its
        # own counts is the only way to get a true loss rate rather than an
        # estimate that silently ignores frames lost in their entirety.
        self.stats = {
            'frames_in': 0,             # produced by the demuxer
            'frames_sent': 0,
            'frames_skipped_stale': 0,  # superseded in the slot before sending
            'frames_dropped_backlog': 0,
            'keyframes_forced': 0,      # queue dropped to make room for an IDR
            'chunks_sent': 0,
            'parity_sent': 0,
            'bytes_sent': 0,
        }
        self.last_pilot_stats: dict | None = None

    def set_profile(self, profile: qos.QoSProfile):
        """
        Swap the active QoS profile.

        Only read at frame boundaries in _frame_sender, so changing it mid-flight
        cannot produce a frame whose parity was computed at a different rate than
        its header advertises.
        """
        self.profile = profile
        log.info('QoS profile → %s (cap %d kbps, FEC %d%%/%d%%, drop at %d frame-times)',
                 profile.name, profile.max_bitrate_kbps,
                 profile.fec_delta_pct, profile.fec_key_pct,
                 profile.backlog_drop_frames)

    async def start(self):
        self._frame_event = asyncio.Event()
        self._sender_task = asyncio.ensure_future(self._frame_sender())
        self._stats_task  = asyncio.ensure_future(self._stats_reporter())
        await self._start_sensor_relay()
        self._video_task = asyncio.ensure_future(self._video_relay_loop())
        log.info('relay started (video %s, sensor :%d, profile %s, %d fps)',
                 _redact(self.video_url) if self.video_url else f':{self.video_port}',
                 self.sensor_port, self.profile.name, self.fps)

    async def handle_message(self, msg: dict):
        t = msg.get('type', 'unknown')
        if t == 'hello':
            # The pilot's first write on the bidi stream, and the reason this
            # message exists at all. On the P2P path the agent learns the JSON
            # stream's id only when data first arrives on it, so anything it
            # tries to send between session-accept and that first write is
            # dropped on the floor by transport.send_json. An unprompted
            # announcement at connect time is exactly that case. Having the
            # pilot speak first turns it into a reply, which is always safe.
            if self.on_hello:
                await self.on_hello()
            return
        if t == 'pilot-stats':
            self.last_pilot_stats = msg
            return
        if t == 'qos' and self.on_qos:
            ack = await self.on_qos(msg.get('profile'))
            if self.send_json and ack:
                await self.send_json(ack)
            return
        if t == 'ptz':
            # Deliberately unacknowledged. These arrive many per second while a
            # key is held, and an ack per command would put a round trip's worth
            # of chatter on the same path that is carrying video, to tell the
            # operator something the moving picture already tells them.
            if self.on_ptz:
                self.on_ptz(msg.get('pan', 0), msg.get('tilt', 0), msg.get('zoom', 0))
            return
        if t == 'ptz-home':
            if self.on_ptz_home:
                ok = await self.on_ptz_home()
                if self.send_json:
                    await self.send_json({'type': 'ack', 'cmd': 'ptz-home',
                                          'ok': bool(ok), 'ts': msg.get('ts', '')})
            return
        ts = msg.get('ts', '')
        log.info('[cmd] type=%s ts=%s', t, ts)
        if self.send_json:
            await self.send_json({'type': 'ack', 'cmd': t, 'ts': ts})

    async def stop(self):
        for task in (self._sender_task, self._video_task, self._stats_task):
            if task:
                task.cancel()
                try:
                    await task
                except asyncio.CancelledError:
                    pass
        self._sender_task = None
        self._video_task  = None
        self._stats_task  = None
        if self._sensor_transport:
            self._sensor_transport.close()
            self._sensor_transport = None
        if self._sdp_tmp and os.path.exists(self._sdp_tmp):
            os.unlink(self._sdp_tmp)
            self._sdp_tmp = None

    # ── private ──────────────────────────────────────────────────────────────

    async def _start_sensor_relay(self):
        loop = asyncio.get_event_loop()
        try:
            _, self._sensor_transport = await loop.create_datagram_endpoint(
                lambda: _SensorProtocol(self._on_sensor_data),
                local_addr=('127.0.0.1', self.sensor_port),
            )
            log.info('sensor relay bound on UDP :%d', self.sensor_port)
        except OSError as e:
            log.error('could not bind sensor port %d: %s', self.sensor_port, e)

    def _on_sensor_data(self, data: bytes):
        try:
            text = data.decode('utf-8')
        except UnicodeDecodeError:
            return  # silently drop binary (e.g. stray RTCP) packets
        if self.send_json:
            asyncio.ensure_future(
                self.send_json({'type': 'sensor', 'data': text})
            )

    async def _video_relay_loop(self):
        loop = asyncio.get_event_loop()
        backoff = 1
        while True:
            try:
                await loop.run_in_executor(None, self._blocking_video_relay, loop)
                log.info('video relay ended — restarting in %ds', backoff)
            except asyncio.CancelledError:
                return
            except Exception as e:
                log.error('video relay error: %s — retrying in %ds', _redact(e), backoff)
            await asyncio.sleep(backoff)
            backoff = min(backoff * 2, 30)

    def _open_video(self):
        """
        Open the video source and return the PyAV container.

        Two sources, one downstream path. A local RTP/UDP socket needs an SDP
        descriptor written to a temp file because there is no in-band signalling
        to describe the stream; an RTSP camera describes itself, so the URL is
        enough. Either way what comes out of demux() is H.264 access units in
        Annex B, which is the only thing the rest of this class knows about.
        """
        if self.video_url:
            container = av.open(
                self.video_url,
                options={
                    # TCP, not UDP. An IP camera's RTP/UDP has no FEC of its own
                    # and its losses would arrive as corrupt access units that
                    # DARC would then faithfully chunk, protect, and relay —
                    # spending parity on already-broken frames. The camera link
                    # is a short, fast LAN hop where TCP's retransmit costs
                    # microseconds; the lossy path worth protecting is the one
                    # after the agent, not before it.
                    'rtsp_transport': 'tcp',
                    'rtsp_flags':     'prefer_tcp',
                    'fflags':         'nobuffer',
                    'flags':          'low_delay',
                    'max_delay':      '0',
                    'reorder_queue_size': '0',
                    # Keep probing short: every millisecond here is added to
                    # startup before the first frame reaches the operator.
                    'analyzeduration': '500000',
                    'probesize':       '500000',
                    'stimeout':        '5000000',   # 5s, in microseconds
                },
            )
            log.info('video relay open on %s', _redact(self.video_url))
            return container

        sdp = _VIDEO_SDP_TEMPLATE.format(port=self.video_port)
        tmp = tempfile.NamedTemporaryFile(suffix='.sdp', mode='w', delete=False)
        tmp.write(sdp)
        tmp.close()
        self._sdp_tmp = tmp.name

        container = av.open(
            self._sdp_tmp,
            format='sdp',
            options={
                'protocol_whitelist': 'file,crypto,data,rtp,udp',
                'fflags': 'nobuffer',
                'flags': 'low_delay',
                'max_delay': '0',
                'reorder_queue_size': '0',
                'analyzeduration': '1000000',
                'probesize': '1000000',
                # A keyframe is 20+ back-to-back RTP packets. The default UDP
                # receive buffer can overflow on that burst even over
                # loopback, which looks identical to network loss. Verify with
                # `netstat -sp udp | grep "full socket buffers"` rather than
                # trusting that this option reaches the udp protocol.
                'buffer_size': '4194304',
            },
        )
        log.info('video relay open on UDP :%d', self.video_port)
        return container

    def _blocking_video_relay(self, loop):
        """
        Blocking: demux H.264 from the video source via PyAV. Each complete
        access unit is written to _latest_frame, overwriting any unprocessed
        frame. The asyncio _frame_sender picks up the newest frame and sends it,
        skipping any frames produced while it was busy — no queue buildup is
        possible.

        PyAV's H.264 depacketiser assembles fragmented NAL units (FU-A) and
        outputs complete access units in Annex B format (start codes included).
        Keyframe packets include SPS + PPS + IDR NAL units in sequence.
        """
        try:
            container = self._open_video()
            # An IP camera may carry audio the operator never asked for. Select
            # the video stream explicitly rather than relaying whatever arrives:
            # an AAC packet chunked as if it were an access unit would be fed to
            # a VideoDecoder as a delta frame.
            video = container.streams.video[0]
            for packet in container.demux(video):
                if packet.size == 0:
                    continue
                self._frame_id += 1
                self.stats['frames_in'] += 1
                # Overwrite any earlier unprocessed frame — the sender always
                # forwards the most recent one. Losing the older frame whole is
                # the point; it is what keeps latency bounded without ever
                # putting a partial frame on the wire.
                if self._latest_frame is not None:
                    self.stats['frames_skipped_stale'] += 1
                self._latest_frame = (
                    self._frame_id & 0xFFFF,
                    bool(packet.is_keyframe),
                    bytes(packet),
                )
                loop.call_soon_threadsafe(self._frame_event.set)
        except Exception as e:
            log.error('video relay read error: %s', _redact(e))
        finally:
            if self._sdp_tmp and os.path.exists(self._sdp_tmp):
                os.unlink(self._sdp_tmp)
                self._sdp_tmp = None

    async def _frame_sender(self):
        """
        Send the latest available video frame, with parity, as one batch.

        Admission control happens before the first chunk leaves: either the whole
        frame goes or none of it does. See the class docstring for why partial
        frames are worse than skipped ones.
        """
        while True:
            await self._frame_event.wait()
            self._frame_event.clear()

            frame = self._latest_frame
            self._latest_frame = None  # claim the slot
            if frame is None or self.send_batch is None:
                continue

            fid, is_keyframe, payload = frame
            profile = self.profile            # read once, at a frame boundary

            # Is the link already backed up? Compare against a byte budget, not
            # against zero: the pending queue also grows simply because aioquic
            # paces packets out, so "queue non-empty" does not mean congested.
            backlog = 0
            if self.pending_bytes:
                backlog = self.pending_bytes()
            threshold = profile.drop_threshold_bytes(self.fps)

            if backlog > threshold:
                if not is_keyframe:
                    # Drop before spending anything. Costs the pilot one frame
                    # and costs the link nothing.
                    self.stats['frames_dropped_backlog'] += 1
                    continue
                # A keyframe always goes. Clearing the queue is free here: the
                # IDR invalidates everything still pending from the last GOP.
                if self.drop_pending:
                    self.drop_pending()
                    self.stats['keyframes_forced'] += 1

            fec_pct = profile.fec_key_pct if is_keyframe else profile.fec_delta_pct
            chunks, n, k = fec.pack_frame(
                payload, frame_id=fid, is_keyframe=is_keyframe, fec_pct=fec_pct)

            try:
                await self.send_batch(chunks)
            except Exception as e:
                log.debug('frame send failed: %s', e)
                continue

            self.stats['frames_sent'] += 1
            self.stats['chunks_sent'] += len(chunks)
            self.stats['parity_sent'] += k
            self.stats['bytes_sent']  += sum(len(c) for c in chunks)

    async def _stats_reporter(self):
        """
        Push counters to the pilot at 1 Hz over the JSON stream.

        The pilot cannot compute a true loss rate on its own — a frame whose
        chunks were all lost is invisible to it. Pairing chunks_sent with the
        pilot's chunksRx is what turns an estimate into a measurement.
        """
        while True:
            await asyncio.sleep(1.0)
            if not self.send_json:
                continue
            msg = {'type': 'agent-stats', **self.stats}
            if self.pending_bytes:
                msg['pending_bytes'] = self.pending_bytes()
            if self.link_stats:
                msg.update(self.link_stats())
            try:
                await self.send_json(msg)
            except Exception:
                pass
