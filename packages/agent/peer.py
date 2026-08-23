import asyncio
import json
import logging
import math
import os
import struct
import tempfile

import av

log = logging.getLogger(__name__)

# Maximum H.264 payload bytes per QUIC DATAGRAM chunk.
# QUIC packets are capped by path MTU (~1200–1500 bytes). Keeping chunks well
# below that guarantees each fits in one UDP packet with room for
# QUIC / H3 / WebTransport framing overhead.
_MAX_CHUNK_PAYLOAD = 1000

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

    Video is forwarded as chunked QUIC DATAGRAM frames:

      Chunk wire format:
        byte 0:     flags — bit 7 = keyframe
        bytes 1-2:  frame_id  (uint16 BE, rolls at 65535)
        bytes 3-4:  chunk_idx (uint16 BE, 0-based)
        bytes 5-6:  total_chunks (uint16 BE)
        bytes 7+:   H.264 Annex B payload slice (≤ _MAX_CHUNK_PAYLOAD bytes)

    Latency guarantee: only the *latest* decoded frame is ever in flight.
    When a new frame arrives before the current one finishes sending, the
    sender abandons the in-progress send and moves on. Additionally, aioquic's
    internal DATAGRAM send queue is flushed before each frame so congested-path
    stale chunks cannot delay newer ones. The pilot handles incomplete frames
    by simply discarding them.

    Sensor data is forwarded as JSON: {"type": "sensor", "data": "42"}
    """

    def __init__(self, video_port: int, sensor_port: int):
        self.video_port  = video_port
        self.sensor_port = sensor_port

        # Set by agent.py when a pilot connects / disconnects.
        self.send_binary      = None  # async (data: bytes) -> None
        self.send_json        = None  # async (msg:  dict)  -> None
        self.flush_send_queue = None  # sync  ()            -> None

        self._sensor_transport = None
        self._video_task       = None
        self._sender_task      = None
        self._sdp_tmp          = None

        # Single-slot latest-frame buffer shared between the PyAV thread and
        # the asyncio _frame_sender coroutine.
        self._latest_frame: tuple | None = None  # (fid, flags, payload)
        self._frame_event:  asyncio.Event | None = None
        self._frame_id = 0

    async def start(self):
        self._frame_event = asyncio.Event()
        self._sender_task = asyncio.ensure_future(self._frame_sender())
        await self._start_sensor_relay()
        self._video_task = asyncio.ensure_future(self._video_relay_loop())
        log.info('relay started (video :%d, sensor :%d)', self.video_port, self.sensor_port)

    async def handle_message(self, msg: dict):
        t  = msg.get('type', 'unknown')
        ts = msg.get('ts', '')
        log.info('[cmd] type=%s ts=%s', t, ts)
        if self.send_json:
            await self.send_json({'type': 'ack', 'cmd': t, 'ts': ts})

    async def stop(self):
        for task in (self._sender_task, self._video_task):
            if task:
                task.cancel()
                try:
                    await task
                except asyncio.CancelledError:
                    pass
        self._sender_task = None
        self._video_task  = None
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
                log.error('video relay error: %s — retrying in %ds', e, backoff)
            await asyncio.sleep(backoff)
            backoff = min(backoff * 2, 30)

    def _blocking_video_relay(self, loop):
        """
        Blocking: demux H.264 from RTP via PyAV. Each complete access unit is
        written to _latest_frame, overwriting any unprocessed frame. The asyncio
        _frame_sender picks up the newest frame and sends it, skipping any frames
        produced while it was busy — no queue buildup is possible.

        PyAV's H.264 RTP demuxer assembles fragmented NAL units (FU-A) and
        outputs complete access units in Annex B format (start codes included).
        Keyframe packets include SPS + PPS + IDR NAL units in sequence.
        """
        sdp = _VIDEO_SDP_TEMPLATE.format(port=self.video_port)
        tmp = tempfile.NamedTemporaryFile(suffix='.sdp', mode='w', delete=False)
        tmp.write(sdp)
        tmp.close()
        self._sdp_tmp = tmp.name

        try:
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
                },
            )
            log.info('video relay open on UDP :%d', self.video_port)
            for packet in container.demux():
                if packet.size == 0:
                    continue
                self._frame_id += 1
                # Overwrite any earlier unprocessed frame — the sender always
                # forwards the most recent one.
                self._latest_frame = (
                    self._frame_id & 0xFFFF,
                    0x80 if packet.is_keyframe else 0x00,
                    bytes(packet),
                )
                loop.call_soon_threadsafe(self._frame_event.set)
        except Exception as e:
            log.error('video relay read error: %s', e)
        finally:
            if self._sdp_tmp and os.path.exists(self._sdp_tmp):
                os.unlink(self._sdp_tmp)
                self._sdp_tmp = None

    async def _frame_sender(self):
        """
        Send the latest available video frame as chunked QUIC DATAGRAMs.

        Woken by _frame_event whenever a new frame is ready. Before sending,
        flushes aioquic's DATAGRAM send queue so stale chunks from a slow or
        congested path don't delay the new frame. Aborts mid-frame if a newer
        frame arrives, ensuring the pilot always receives the most recent image
        with bounded latency regardless of network conditions.
        """
        while True:
            await self._frame_event.wait()
            self._frame_event.clear()

            frame = self._latest_frame
            self._latest_frame = None  # claim the slot
            if frame is None or self.send_binary is None:
                continue

            fid, flags, payload = frame

            # Flush any datagrams from the previous frame still waiting in
            # aioquic's send queue. On a slow or lossy path these would
            # otherwise arrive at the pilot after the new frame — exactly
            # backwards from what we want.
            if self.flush_send_queue:
                self.flush_send_queue()

            total = max(1, math.ceil(len(payload) / _MAX_CHUNK_PAYLOAD))
            for i in range(total):
                # A newer frame arrived while we were sending — abandon this
                # one. The pilot will discard the incomplete frame silently.
                if self._latest_frame is not None or self.send_binary is None:
                    break
                chunk = payload[i * _MAX_CHUNK_PAYLOAD:(i + 1) * _MAX_CHUNK_PAYLOAD]
                await self.send_binary(struct.pack('>BHHH', flags, fid, i, total) + chunk)
