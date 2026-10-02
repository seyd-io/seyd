#!/usr/bin/env python3
"""
A pretend Tello on localhost, so the whole chain — bridge → seydd → cloud →
pilot page — runs on one laptop before the drone is unpacked.

It speaks exactly the subset of the binary protocol tello.py uses: answers
`conn_req:` with `conn_ack:`, pushes flight data at 10 Hz, Wi-Fi strength at
1 Hz and a log header once (then odometry/IMU records once acked), takes off
and lands on command, integrates the stick input into a crude height and
position model, and streams H.264 to the video port named in the handshake.
The picture comes from FFmpeg (`testsrc2`, 960x720, 30 fps, baseline) chopped
into the drone's 2-byte-headed datagrams; "start video" makes it send the
cached SPS/PPS as a picture of their own, as the real drone is reported to,
so the bridge's parameter-set caching is exercised. `--loss` drops datagrams
at random to exercise the torn-frame path.

    python3 examples/tello-robot/fake_tello.py            # 127.0.0.1:8889
    python3 examples/tello-robot/bridge.py --drone-ip 127.0.0.1

Not part of Seyd; a test double for the example.
"""
import argparse
import asyncio
import logging
import os
import random
import socket
import struct
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import tello as T  # noqa: E402
from h264rtp import split_annexb, NAL_SPS, NAL_PPS, NAL_AUD  # noqa: E402

log = logging.getLogger('fake-tello')

DGRAM = 1460


def flight_payload(height_dm: int, speed_dms: int, battery: int, flying: bool, fly_time_ds: int) -> bytes:
    d = bytearray(24)
    struct.pack_into('<hhhhh', d, 0, height_dm, 0, 0, speed_dms, fly_time_ds)
    d[10] = 0b0000_1101       # imu ok, down visual ok, power ok
    d[11] = 0
    d[12] = battery
    struct.pack_into('<hh', d, 13, 0, 900)
    d[17] = (1 if flying else 0) | (0 if flying else 2) | (0x20 if battery < 20 else 0)
    d[18] = 6 if flying else 1
    d[20] = 0
    d[21] = 0
    return bytes(d)


def log_record(rec_id: int, payload: bytes, key: int = 0x5a) -> bytes:
    body = bytes(x ^ key for x in payload)
    length = 10 + len(body) + 2
    head = bytearray([0x55]) + struct.pack('<H', length) + b'\x00' + struct.pack('<H', rec_id) + bytes([key]) + b'\x00\x00\x00'
    return bytes(head) + body + b'\x00\x00'


class FakeTello:
    def __init__(self, port: int, loss: float, no_video: bool, fps: int, gop: int):
        self.port = port
        self.loss = loss
        self.no_video = no_video
        self.fps = fps
        self.gop = gop
        self.client = None            # (ip, port) of the controller
        self.video_port = 6038
        self.flying = False
        self.height_m = 0.0
        self.pos = [0.0, 0.0, 0.0]
        self.vel = [0.0, 0.0, 0.0]
        self.yaw = 0.0
        self.battery = 87
        self.fly_time = 0.0
        self.sticks = (0.0, 0.0, 0.0, 0.0)
        self.last_stick = 0.0
        self.log_acked = False
        self.video_requested = asyncio.Event()
        self.sps = self.pps = None
        self.frame_no = 0
        self.sent_pictures = 0
        self.dropped = 0
        self.stick_packets = 0
        self.encoder_rate = 4
        self.transport = None
        self.vsock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)

    def _send(self, cmd: int, pkt_type: int, payload: bytes = b'') -> None:
        if self.client and self.transport:
            self.transport.sendto(T.build_packet(cmd, pkt_type, payload, 0), self.client)

    def datagram_received(self, data: bytes, addr) -> None:
        if data.startswith(b'conn_req:') and len(data) >= 11:
            self.client = addr
            self.video_port = struct.unpack_from('<H', data, 9)[0]
            self.transport.sendto(b'conn_ack:' + data[9:11], addr)
            log.info('controller %s connected; video → %s:%d', addr, addr[0], self.video_port)
            return
        p = T.parse_packet(data)
        if p is None:
            log.warning('bad packet from %s: %s', addr, data[:12].hex())
            return
        if p.cmd == T.STICK_CMD:
            self.stick_packets += 1
            if len(p.payload) >= 6:
                packed = int.from_bytes(p.payload[:6] + b'\x00\x00', 'little')
                axes = [((packed >> (11 * i)) & 0x7ff) for i in range(4)]
                self.sticks = tuple((a - 1024) / 660.0 for a in axes)
                self.last_stick = time.monotonic()
        elif p.cmd == T.TAKEOFF_CMD:
            log.info('TAKEOFF')
            self.flying = True
            self.height_m = 0.8
            self._send(T.TAKEOFF_CMD, T.PT_DATA1, b'\x00')
        elif p.cmd == T.LAND_CMD:
            log.info('LAND')
            self.flying = False
            self.height_m = 0.0
            self._send(T.LAND_CMD, T.PT_DATA1, b'\x00')
        elif p.cmd == T.VIDEO_START_CMD:
            self.video_requested.set()
        elif p.cmd == T.VIDEO_ENCODER_RATE_CMD:
            self.encoder_rate = p.payload[0] if p.payload else 0
            log.info('encoder rate level %d', self.encoder_rate)
        elif p.cmd == T.LOG_HEADER_MSG:
            self.log_acked = True
        elif p.cmd == T.SET_ALT_LIMIT_CMD:
            log.info('alt limit %d m', p.payload[0] if p.payload else -1)
        elif p.cmd == T.TIME_CMD:
            pass
        elif p.cmd in (T.EXPOSURE_CMD, T.VIDEO_MODE_CMD, T.PALM_LAND_CMD, T.FLIP_CMD):
            self._send(p.cmd, T.PT_DATA1, b'\x00')
        else:
            log.info('unhandled cmd 0x%04x', p.cmd)

    async def telemetry_loop(self) -> None:
        tick = 0
        sent_header = False
        while True:
            await asyncio.sleep(0.1)
            if not self.client:
                continue
            dt = 0.1
            if self.flying:
                roll, pitch, thr, yaw = self.sticks if time.monotonic() - self.last_stick < 0.3 else (0, 0, 0, 0)
                self.height_m = max(0.3, min(5.0, self.height_m + thr * 1.0 * dt))
                self.yaw = (self.yaw + yaw * 90 * dt) % 360
                self.vel = [pitch * 1.5, roll * 1.5, thr * 1.0]
                self.pos[0] += self.vel[0] * dt
                self.pos[1] += self.vel[1] * dt
                self.pos[2] = self.height_m
                self.fly_time += dt
                if tick % 300 == 0:
                    self.battery = max(5, self.battery - 1)
            else:
                self.vel = [0.0, 0.0, 0.0]
            speed = (self.vel[0] ** 2 + self.vel[1] ** 2) ** 0.5
            self._send(T.FLIGHT_MSG, T.PT_DATA1, flight_payload(int(self.height_m * 10), int(speed * 10), self.battery,
                                                                 self.flying, int(self.fly_time * 10)))
            if tick % 10 == 0:
                self._send(T.WIFI_MSG, T.PT_DATA1, bytes([random.randint(80, 95), 0]))
            if not sent_header and tick >= 5:
                sent_header = True
                self._send(T.LOG_HEADER_MSG, T.PT_DATA1, b'\x00' + struct.pack('<H', 0x1234) + b'\x00' * 40)
            if self.log_acked and tick % 2 == 0:
                mvo = struct.pack('<hhhh', 0, int(self.vel[0] * 100), int(self.vel[1] * 100), int(self.vel[2] * 100)) \
                    + struct.pack('<fff', *self.pos) + b'\x00' * 8
                import math
                h = math.radians(self.yaw) / 2
                imu = b'\x00' * 48 + struct.pack('<ffff', math.cos(h), 0.0, 0.0, math.sin(h)) + b'\x00' * 16
                self._send(T.LOG_DATA_MSG, T.PT_DATA1, b'\x00' + log_record(T.LogState.ID_MVO, mvo) + log_record(T.LogState.ID_IMU_ATTI, imu))
            tick += 1

    def send_picture(self, data: bytes) -> None:
        if not self.client:
            return
        n = max(1, (len(data) + DGRAM - 1) // DGRAM)
        for i in range(n):
            chunk = data[i * DGRAM:(i + 1) * DGRAM]
            hdr = bytes([self.frame_no & 0xff, (i & 0x7f) | (0x80 if i == n - 1 else 0)])
            if self.loss and random.random() < self.loss:
                self.dropped += 1
                continue
            self.vsock.sendto(hdr + chunk, (self.client[0], self.video_port))
        self.frame_no = (self.frame_no + 1) & 0xff
        self.sent_pictures += 1

    async def video_loop(self) -> None:
        if self.no_video:
            return
        await self.video_requested.wait()
        cmd = ['ffmpeg', '-hide_banner', '-loglevel', 'error', '-re', '-f', 'lavfi',
               '-i', f'testsrc2=size=960x720:rate={self.fps}', '-c:v', 'libx264', '-preset', 'ultrafast',
               '-tune', 'zerolatency', '-profile:v', 'baseline', '-g', str(self.gop), '-keyint_min', str(self.gop),
               '-sc_threshold', '0', '-b:v', '1500k', '-bsf:v', 'h264_metadata=aud=insert', '-f', 'h264', 'pipe:1']
        proc = await asyncio.create_subprocess_exec(*cmd, stdout=asyncio.subprocess.PIPE)
        log.info('ffmpeg streaming testsrc2 960x720@%d, gop %d', self.fps, self.gop)
        buf = b''
        picture = []
        try:
            while True:
                chunk = await proc.stdout.read(65536)
                if not chunk:
                    break
                buf += chunk
                # split on access unit delimiters; keep the tail (an AU in progress)
                last_aud = buf.rfind(b'\x00\x00\x00\x01\x09')
                if last_aud <= 0:
                    continue
                complete, buf = buf[:last_aud], buf[last_aud:]
                for nal in split_annexb(complete):
                    t = nal[0] & 0x1f
                    if t == NAL_AUD:
                        if picture:
                            self._emit(picture)
                            picture = []
                        continue
                    if t == NAL_SPS:
                        self.sps = nal
                    elif t == NAL_PPS:
                        self.pps = nal
                    picture.append(nal)
        finally:
            proc.kill()

    def _emit(self, nals) -> None:
        if self.video_requested.is_set() and self.sps and self.pps:
            # as the real drone: parameter sets first, as their own picture, then the stream
            self.video_requested.clear()
            self.send_picture(b''.join(b'\x00\x00\x00\x01' + n for n in (self.sps, self.pps)))
        self.send_picture(b''.join(b'\x00\x00\x00\x01' + n for n in nals))

    async def stats_loop(self) -> None:
        while True:
            await asyncio.sleep(10)
            log.info('pictures=%d dropped_dgrams=%d stick_packets=%d flying=%s h=%.1f sticks=%s',
                     self.sent_pictures, self.dropped, self.stick_packets, self.flying, self.height_m,
                     tuple(round(s, 2) for s in self.sticks))


class _Proto(asyncio.DatagramProtocol):
    def __init__(self, d: FakeTello):
        self.d = d

    def connection_made(self, transport):
        self.d.transport = transport

    def datagram_received(self, data, addr):
        self.d.datagram_received(data, addr)


async def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--port', type=int, default=8889)
    ap.add_argument('--bind', default='127.0.0.1')
    ap.add_argument('--loss', type=float, default=0.0, help='probability of dropping each video datagram')
    ap.add_argument('--fps', type=int, default=30)
    ap.add_argument('--gop', type=int, default=300, help='keyframe interval in frames (the real drone keyframes on request)')
    ap.add_argument('--no-video', action='store_true')
    args = ap.parse_args()
    logging.basicConfig(level='INFO', format='%(asctime)s %(levelname)s %(name)s: %(message)s')
    d = FakeTello(args.port, args.loss, args.no_video, args.fps, args.gop)
    loop = asyncio.get_running_loop()
    await loop.create_datagram_endpoint(lambda: _Proto(d), local_addr=(args.bind, args.port))
    log.info('fake tello on %s:%d (loss %.1f%%)', args.bind, args.port, args.loss * 100)
    await asyncio.gather(d.telemetry_loop(), d.video_loop(), d.stats_loop())


if __name__ == '__main__':
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        pass
