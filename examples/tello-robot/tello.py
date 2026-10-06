# Copyright 2026 Anton Gravestam
# SPDX-License-Identifier: Apache-2.0
"""
Ryze/DJI Tello driver — the vendor-specific half of the Tello demo robot.

**Boundary note.** This is robot-side customer code, not part of Seyd. Nothing
in `packages/` knows the word "Tello"; `bridge.py` consumes seydd's generic UDP
interfaces (docs/protocol/seydd.md) and uses this module to talk to the drone.

The protocol is the Tello's *binary* protocol, the one the official app speaks,
reverse-engineered by the TelloPilots community and implemented in TelloPy
(https://github.com/hanyazou/TelloPy) and Gobot. It is preferred over the
text "SDK mode" because it carries continuous stick input (needed for a pilot
holding a key), pushes flight telemetry at ~10 Hz without polling, and streams
H.264 that the drone keyframes on request. What is known, all learned from
TelloPy's source and the TelloPilots wiki:

* **Addresses.** The drone is `192.168.10.1`; it hands the laptop
  `192.168.10.2` over its own Wi-Fi. Control is UDP to port 8889 from a fixed
  local port (9000). Video arrives on a local UDP port the handshake names.
* **Handshake.** `conn_req:` followed by the video port as a little-endian
  16-bit value (6038 = 0x1796, bytes `96 17` — TelloPy spells the same two
  bytes as the digit string "9617"). The drone answers `conn_ack:`; every
  other datagram is a framed packet.
* **Packet framing.** `0xcc`, a 13-bit length stored `<< 3` little-endian
  over two bytes, CRC-8 of those three bytes, a packet type byte, a 16-bit
  command id, a 16-bit sequence, the payload, and CRC-16 over everything
  before it. Both CRCs are table-driven with non-standard seeds (0x77 and
  0x3692); the tables below are the protocol's, copied from TelloPy.
* **Sticks.** Command 0x50, sent continuously: four 11-bit axes
  (roll, pitch, throttle, yaw as 1024 ± 660) and a "fast mode" bit packed into
  48 bits, then the wall-clock time. A drone that stops receiving stick
  packets treats the controller as gone.
* **Telemetry.** The drone pushes command 0x56 (flight data: height, speeds,
  battery, state bits) unprompted, 0x1a (Wi-Fi strength), and, once the
  0x1050 log header is acknowledged, 0x1051 log records carrying visual
  odometry position/velocity and the IMU quaternion.
* **Video.** Raw H.264 Annex B (960×720 4:3, or 1280×720 with `zoom`), not
  RTP: each UDP datagram is a 2-byte header — frame number, then packet index
  within the frame with bit 7 set on the frame's last packet — followed by
  the next slice of the elementary stream. Command 0x25 ("start video") makes
  the drone send its parameter sets and a keyframe; TelloPy resends it every
  two seconds, we send it on demand (see bridge.py). Command 0x20 sets the
  encoder bitrate level (0 auto, 1–5 = 1, 1.5, 2, 3, 4 Mbps, measured), 0x34 the
  exposure (0–2), 0x31 the aspect (0 = 4:3 wide, 1 = 16:9 zoomed).

Pure stdlib, asyncio. Everything measured on the bench is marked as such in
DEMO-TELLO.md; everything else here is what the reference implementations do.
"""
from __future__ import annotations

import asyncio
import datetime
import logging
import math
import struct
import time
from dataclasses import dataclass, field
from typing import Callable, Optional

log = logging.getLogger('tello')

TELLO_ADDR = ('192.168.10.1', 8889)
CONTROL_PORT = 9000      # local port the drone answers to
VIDEO_PORT = 6038        # local port the drone streams to (named in conn_req)

# ── message ids (TelloPilots wiki / TelloPy protocol.py) ─────────────────────
START_OF_PACKET = 0xcc
WIFI_MSG = 0x001a
VIDEO_ENCODER_RATE_CMD = 0x0020
VIDEO_START_CMD = 0x0025
VIDEO_MODE_CMD = 0x0031
EXPOSURE_CMD = 0x0034
LIGHT_MSG = 0x0035
TIME_CMD = 0x0046
STICK_CMD = 0x0050
TAKEOFF_CMD = 0x0054
LAND_CMD = 0x0055
FLIGHT_MSG = 0x0056
SET_ALT_LIMIT_CMD = 0x0058
FLIP_CMD = 0x005c
PALM_LAND_CMD = 0x005e
LOG_HEADER_MSG = 0x1050
LOG_DATA_MSG = 0x1051
LOG_CONFIG_MSG = 0x1052
ALT_LIMIT_MSG = 0x1056
LOW_BAT_THRESHOLD_MSG = 0x1057
ATT_LIMIT_MSG = 0x1059

# Packet type byte per command, as the reference implementations send them.
PT_GET = 0x48
PT_DATA1 = 0x50
PT_DATA2 = 0x60
PT_SET = 0x68
PT_FLIP = 0x70

_CRC8 = bytes([
    0x00, 0x5e, 0xbc, 0xe2, 0x61, 0x3f, 0xdd, 0x83, 0xc2, 0x9c, 0x7e, 0x20, 0xa3, 0xfd, 0x1f, 0x41,
    0x9d, 0xc3, 0x21, 0x7f, 0xfc, 0xa2, 0x40, 0x1e, 0x5f, 0x01, 0xe3, 0xbd, 0x3e, 0x60, 0x82, 0xdc,
    0x23, 0x7d, 0x9f, 0xc1, 0x42, 0x1c, 0xfe, 0xa0, 0xe1, 0xbf, 0x5d, 0x03, 0x80, 0xde, 0x3c, 0x62,
    0xbe, 0xe0, 0x02, 0x5c, 0xdf, 0x81, 0x63, 0x3d, 0x7c, 0x22, 0xc0, 0x9e, 0x1d, 0x43, 0xa1, 0xff,
    0x46, 0x18, 0xfa, 0xa4, 0x27, 0x79, 0x9b, 0xc5, 0x84, 0xda, 0x38, 0x66, 0xe5, 0xbb, 0x59, 0x07,
    0xdb, 0x85, 0x67, 0x39, 0xba, 0xe4, 0x06, 0x58, 0x19, 0x47, 0xa5, 0xfb, 0x78, 0x26, 0xc4, 0x9a,
    0x65, 0x3b, 0xd9, 0x87, 0x04, 0x5a, 0xb8, 0xe6, 0xa7, 0xf9, 0x1b, 0x45, 0xc6, 0x98, 0x7a, 0x24,
    0xf8, 0xa6, 0x44, 0x1a, 0x99, 0xc7, 0x25, 0x7b, 0x3a, 0x64, 0x86, 0xd8, 0x5b, 0x05, 0xe7, 0xb9,
    0x8c, 0xd2, 0x30, 0x6e, 0xed, 0xb3, 0x51, 0x0f, 0x4e, 0x10, 0xf2, 0xac, 0x2f, 0x71, 0x93, 0xcd,
    0x11, 0x4f, 0xad, 0xf3, 0x70, 0x2e, 0xcc, 0x92, 0xd3, 0x8d, 0x6f, 0x31, 0xb2, 0xec, 0x0e, 0x50,
    0xaf, 0xf1, 0x13, 0x4d, 0xce, 0x90, 0x72, 0x2c, 0x6d, 0x33, 0xd1, 0x8f, 0x0c, 0x52, 0xb0, 0xee,
    0x32, 0x6c, 0x8e, 0xd0, 0x53, 0x0d, 0xef, 0xb1, 0xf0, 0xae, 0x4c, 0x12, 0x91, 0xcf, 0x2d, 0x73,
    0xca, 0x94, 0x76, 0x28, 0xab, 0xf5, 0x17, 0x49, 0x08, 0x56, 0xb4, 0xea, 0x69, 0x37, 0xd5, 0x8b,
    0x57, 0x09, 0xeb, 0xb5, 0x36, 0x68, 0x8a, 0xd4, 0x95, 0xcb, 0x29, 0x77, 0xf4, 0xaa, 0x48, 0x16,
    0xe9, 0xb7, 0x55, 0x0b, 0x88, 0xd6, 0x34, 0x6a, 0x2b, 0x75, 0x97, 0xc9, 0x4a, 0x14, 0xf6, 0xa8,
    0x74, 0x2a, 0xc8, 0x96, 0x15, 0x4b, 0xa9, 0xf7, 0xb6, 0xe8, 0x0a, 0x54, 0xd7, 0x89, 0x6b, 0x35])

_CRC16 = [
    0x0000, 0x1189, 0x2312, 0x329b, 0x4624, 0x57ad, 0x6536, 0x74bf, 0x8c48, 0x9dc1, 0xaf5a, 0xbed3, 0xca6c, 0xdbe5, 0xe97e, 0xf8f7,
    0x1081, 0x0108, 0x3393, 0x221a, 0x56a5, 0x472c, 0x75b7, 0x643e, 0x9cc9, 0x8d40, 0xbfdb, 0xae52, 0xdaed, 0xcb64, 0xf9ff, 0xe876,
    0x2102, 0x308b, 0x0210, 0x1399, 0x6726, 0x76af, 0x4434, 0x55bd, 0xad4a, 0xbcc3, 0x8e58, 0x9fd1, 0xeb6e, 0xfae7, 0xc87c, 0xd9f5,
    0x3183, 0x200a, 0x1291, 0x0318, 0x77a7, 0x662e, 0x54b5, 0x453c, 0xbdcb, 0xac42, 0x9ed9, 0x8f50, 0xfbef, 0xea66, 0xd8fd, 0xc974,
    0x4204, 0x538d, 0x6116, 0x709f, 0x0420, 0x15a9, 0x2732, 0x36bb, 0xce4c, 0xdfc5, 0xed5e, 0xfcd7, 0x8868, 0x99e1, 0xab7a, 0xbaf3,
    0x5285, 0x430c, 0x7197, 0x601e, 0x14a1, 0x0528, 0x37b3, 0x263a, 0xdecd, 0xcf44, 0xfddf, 0xec56, 0x98e9, 0x8960, 0xbbfb, 0xaa72,
    0x6306, 0x728f, 0x4014, 0x519d, 0x2522, 0x34ab, 0x0630, 0x17b9, 0xef4e, 0xfec7, 0xcc5c, 0xddd5, 0xa96a, 0xb8e3, 0x8a78, 0x9bf1,
    0x7387, 0x620e, 0x5095, 0x411c, 0x35a3, 0x242a, 0x16b1, 0x0738, 0xffcf, 0xee46, 0xdcdd, 0xcd54, 0xb9eb, 0xa862, 0x9af9, 0x8b70,
    0x8408, 0x9581, 0xa71a, 0xb693, 0xc22c, 0xd3a5, 0xe13e, 0xf0b7, 0x0840, 0x19c9, 0x2b52, 0x3adb, 0x4e64, 0x5fed, 0x6d76, 0x7cff,
    0x9489, 0x8500, 0xb79b, 0xa612, 0xd2ad, 0xc324, 0xf1bf, 0xe036, 0x18c1, 0x0948, 0x3bd3, 0x2a5a, 0x5ee5, 0x4f6c, 0x7df7, 0x6c7e,
    0xa50a, 0xb483, 0x8618, 0x9791, 0xe32e, 0xf2a7, 0xc03c, 0xd1b5, 0x2942, 0x38cb, 0x0a50, 0x1bd9, 0x6f66, 0x7eef, 0x4c74, 0x5dfd,
    0xb58b, 0xa402, 0x9699, 0x8710, 0xf3af, 0xe226, 0xd0bd, 0xc134, 0x39c3, 0x284a, 0x1ad1, 0x0b58, 0x7fe7, 0x6e6e, 0x5cf5, 0x4d7c,
    0xc60c, 0xd785, 0xe51e, 0xf497, 0x8028, 0x91a1, 0xa33a, 0xb2b3, 0x4a44, 0x5bcd, 0x6956, 0x78df, 0x0c60, 0x1de9, 0x2f72, 0x3efb,
    0xd68d, 0xc704, 0xf59f, 0xe416, 0x90a9, 0x8120, 0xb3bb, 0xa232, 0x5ac5, 0x4b4c, 0x79d7, 0x685e, 0x1ce1, 0x0d68, 0x3ff3, 0x2e7a,
    0xe70e, 0xf687, 0xc41c, 0xd595, 0xa12a, 0xb0a3, 0x8238, 0x93b1, 0x6b46, 0x7acf, 0x4854, 0x59dd, 0x2d62, 0x3ceb, 0x0e70, 0x1ff9,
    0xf78f, 0xe606, 0xd49d, 0xc514, 0xb1ab, 0xa022, 0x92b9, 0x8330, 0x7bc7, 0x6a4e, 0x58d5, 0x495c, 0x3de3, 0x2c6a, 0x1ef1, 0x0f78]


def crc8(buf: bytes) -> int:
    c = 0x77
    for v in buf:
        c = _CRC8[(c ^ v) & 0xff]
    return c


def crc16(buf: bytes) -> int:
    c = 0x3692
    for v in buf:
        c = _CRC16[(c ^ v) & 0xff] ^ (c >> 8)
    return c


def build_packet(cmd: int, pkt_type: int, payload: bytes = b'', seq: int = 0) -> bytes:
    """
    Frame one command. The 13-bit length is the whole packet including both
    CRCs; the reference code stores it shifted left by three bits, which
    TelloPy only gets right below 32 bytes — this does it across both bytes.
    """
    total = 9 + len(payload) + 2
    head = bytearray([START_OF_PACKET, (total << 3) & 0xff, (total >> 5) & 0xff])
    head.append(crc8(bytes(head)))
    head.append(pkt_type & 0xff)
    head += struct.pack('<HH', cmd & 0xffff, seq & 0xffff)
    head += payload
    head += struct.pack('<H', crc16(bytes(head)))
    return bytes(head)


def time_payload(t: Optional[datetime.datetime] = None) -> bytes:
    """Wall-clock fields the stick and time packets carry: h, m, s, ms as five little-endian 16-bit values."""
    t = t or datetime.datetime.now()
    ms = t.microsecond // 1000
    return struct.pack('<HHHHH', t.hour, t.minute, t.second, ms & 0xff, (ms >> 8) & 0xff)


def stick_payload(roll: float, pitch: float, throttle: float, yaw: float, fast: bool = False) -> bytes:
    """
    Four axes in -1..1 → 11 bits each around 1024 ± 660, plus the fast-mode
    bit, packed little-endian into six bytes: roll | pitch<<11 | throttle<<22
    | yaw<<33 | fast<<44. Positive is right, forward, up, clockwise.
    """
    def axis(v: float) -> int:
        v = max(-1.0, min(1.0, v))
        return int(1024 + 660.0 * v) & 0x7ff
    packed = axis(roll) | (axis(pitch) << 11) | (axis(throttle) << 22) | (axis(yaw) << 33) | (int(fast) << 44)
    return struct.pack('<Q', packed)[:6]


def conn_req(video_port: int = VIDEO_PORT) -> bytes:
    """`conn_req:` + the video port as a little-endian 16-bit value (6038 = 0x1796 → bytes 96 17)."""
    return b'conn_req:' + struct.pack('<H', video_port)


@dataclass
class Parsed:
    cmd: int
    seq: int
    pkt_type: int
    payload: bytes


def parse_packet(data: bytes) -> Optional[Parsed]:
    """A framed packet, or None for anything else (bad start byte, short, bad CRC)."""
    if len(data) < 11 or data[0] != START_OF_PACKET:
        return None
    if struct.unpack_from('<H', data, len(data) - 2)[0] != crc16(data[:-2]):
        return None
    cmd, seq = struct.unpack_from('<HH', data, 5)
    return Parsed(cmd, seq, data[4], data[9:-2])


# ── telemetry ────────────────────────────────────────────────────────────────

@dataclass
class FlightData:
    """
    Command 0x56, 24+ bytes, as decoded by TelloPy/Gobot. Units are the
    community's reading of the app: height in decimetres, speeds in
    decimetres per second, fly_time in tenths of a second; DEMO-TELLO.md
    records what the bench confirmed.
    """
    height: int = 0
    north_speed: int = 0
    east_speed: int = 0
    ground_speed: int = 0
    fly_time: int = 0
    imu_state: int = 0
    pressure_state: int = 0
    down_visual_state: int = 0
    power_state: int = 0
    battery_state: int = 0
    gravity_state: int = 0
    wind_state: int = 0
    imu_calibration_state: int = 0
    battery_percentage: int = 0
    drone_battery_left: int = 0
    drone_fly_time_left: int = 0
    em_sky: int = 0
    em_ground: int = 0
    em_open: int = 0
    drone_hover: int = 0
    outage_recording: int = 0
    battery_low: int = 0
    battery_lower: int = 0
    factory_mode: int = 0
    fly_mode: int = 0
    throw_fly_timer: int = 0
    camera_state: int = 0
    electrical_machinery_state: int = 0
    front_in: int = 0
    front_out: int = 0
    front_lsc: int = 0
    temperature_height: int = 0

    @classmethod
    def parse(cls, d: bytes) -> 'FlightData':
        f = cls()
        if len(d) < 24:
            return f
        (f.height, f.north_speed, f.east_speed, f.ground_speed, f.fly_time) = struct.unpack_from('<hhhhh', d, 0)
        b = d[10]
        f.imu_state, f.pressure_state, f.down_visual_state, f.power_state = b & 1, (b >> 1) & 1, (b >> 2) & 1, (b >> 3) & 1
        f.battery_state, f.gravity_state, f.wind_state = (b >> 4) & 1, (b >> 5) & 1, (b >> 7) & 1
        f.imu_calibration_state = d[11]
        f.battery_percentage = d[12]
        f.drone_battery_left, f.drone_fly_time_left = struct.unpack_from('<hh', d, 13)
        b = d[17]
        f.em_sky, f.em_ground, f.em_open, f.drone_hover = b & 1, (b >> 1) & 1, (b >> 2) & 1, (b >> 3) & 1
        f.outage_recording, f.battery_low, f.battery_lower, f.factory_mode = (b >> 4) & 1, (b >> 5) & 1, (b >> 6) & 1, (b >> 7) & 1
        f.fly_mode, f.throw_fly_timer, f.camera_state, f.electrical_machinery_state = d[18], d[19], d[20], d[21]
        b = d[22]
        f.front_in, f.front_out, f.front_lsc = b & 1, (b >> 1) & 1, (b >> 2) & 1
        f.temperature_height = d[23] & 1
        return f

    @property
    def flying(self) -> bool:
        return bool(self.em_sky)


@dataclass
class LogState:
    """Visual odometry (record 29) and IMU attitude (record 2048) from the 0x1051 log stream."""
    pos: tuple = (0.0, 0.0, 0.0)      # metres, drone frame at take-off
    vel: tuple = (0.0, 0.0, 0.0)      # m/s
    quat: tuple = (1.0, 0.0, 0.0, 0.0)
    updated: float = 0.0
    unknown_ids: set = field(default_factory=set)

    ID_MVO = 29
    ID_IMU_ATTI = 2048

    def update(self, data: bytes) -> None:
        """
        Records start with 0x55, a 16-bit length, a checksum byte, the record
        id, a tick, and a payload XORed with the byte at offset 6. Truncated or
        corrupt input stops the walk rather than raising: telemetry is
        best-effort.
        """
        pos = 0
        while pos + 10 <= len(data):
            if data[pos] != 0x55:
                return
            length = struct.unpack_from('<H', data, pos + 1)[0]
            if length < 12 or pos + length > len(data):
                return
            rec_id = struct.unpack_from('<H', data, pos + 4)[0]
            key = data[pos + 6]
            payload = bytes(x ^ key for x in data[pos + 10:pos + length - 2])
            if rec_id == self.ID_MVO and len(payload) >= 20:
                vx, vy, vz = struct.unpack_from('<hhh', payload, 2)
                self.vel = (vx / 100.0, vy / 100.0, vz / 100.0)
                self.pos = struct.unpack_from('<fff', payload, 8)
                self.updated = time.monotonic()
            elif rec_id == self.ID_IMU_ATTI and len(payload) >= 64:
                self.quat = struct.unpack_from('<ffff', payload, 48)
                self.updated = time.monotonic()
            else:
                self.unknown_ids.add(rec_id)
            pos += length

    def euler_deg(self) -> tuple:
        """(yaw, pitch, roll) in degrees from the quaternion, read as (w, x, y, z)."""
        w, x, y, z = self.quat
        sinr = 2 * (w * x + y * z)
        cosr = 1 - 2 * (x * x + y * y)
        roll = math.degrees(math.atan2(sinr, cosr))
        sinp = max(-1.0, min(1.0, 2 * (w * y - z * x)))
        pitch = math.degrees(math.asin(sinp))
        siny = 2 * (w * z + x * y)
        cosy = 1 - 2 * (y * y + z * z)
        yaw = math.degrees(math.atan2(siny, cosy))
        return (yaw, pitch, roll)


# ── video frame reassembly ───────────────────────────────────────────────────

class FrameAssembler:
    """
    Rebuilds one Annex B picture from the drone's 2-byte-headed datagrams.

    Whole frame or nothing, the same rule Seyd applies downstream: a frame
    with a missing datagram is dropped entirely and reported as lost, never
    delivered torn, because a slice-less picture smears every frame until the
    next keyframe, while a dropped one costs exactly one recovery request.

    Frame end is bit 7 of the index byte. A lost *last* datagram leaves no gap
    in the indices — the only sign is the missing flag — so once the stream
    has shown it uses the flag, a frame closed by a frame-number change
    without it counts as torn. Until then (a drone that never sets it) the
    frame-number change is the only boundary there is and is trusted.
    """

    def __init__(self, on_frame: Callable[[bytes], None], on_loss: Callable[[int], None]):
        self.on_frame = on_frame
        self.on_loss = on_loss
        self.frame_no: Optional[int] = None
        self.expect_idx = 0
        self.parts: list = []
        self.torn = False
        self.frames = 0
        self.lost_frames = 0
        self.packets = 0
        self.uses_end_flag = False
        # Timing of the picture most recently delivered: first and last
        # datagram arrival (monotonic). The bridge reads them to measure the
        # assembly span and its own hop to the daemon.
        self.first_at = 0.0
        self.last_first_at = 0.0
        self.last_close_at = 0.0

    def push(self, dgram: bytes) -> None:
        if len(dgram) < 3:
            return
        self.packets += 1
        frame_no, idx = dgram[0], dgram[1] & 0x7f
        last = bool(dgram[1] & 0x80)
        if frame_no != self.frame_no:
            if self.parts:
                self._close(missing_flag=True)
            self.frame_no = frame_no
            self.expect_idx = 0
            self.torn = idx != 0
            self.parts = []
            self.first_at = time.monotonic()
        elif idx != self.expect_idx:
            self.torn = True
        self.expect_idx = idx + 1
        self.parts.append(dgram[2:])
        if last:
            self.uses_end_flag = True
            self._close()

    def _close(self, missing_flag: bool = False) -> None:
        parts, torn = self.parts, self.torn
        self.parts, self.torn = [], False
        if missing_flag and self.uses_end_flag:
            torn = True   # the tail was lost; the indices alone cannot show it
        if torn:
            self.lost_frames += 1
            self.on_loss(1)
            return
        if missing_flag:
            log.debug('frame %d closed by frame change (no end flag)', self.frame_no)
        self.frames += 1
        self.last_first_at, self.last_close_at = self.first_at, time.monotonic()
        self.on_frame(b''.join(parts))


# ── the drone ────────────────────────────────────────────────────────────────

class Tello:
    """
    Connection, stick loop, telemetry and the raw video datagrams. Callbacks:
    `on_flight(FlightData)`, `on_video(bytes)` (one reassembled Annex B
    picture), `on_video_loss(n)`, `on_state(str)` for connected/disconnected.

    Sticks carry a hold: `set_sticks(..., hold_s)` centres them again when the
    hold expires without a renewal. Over the internet "stop" is a message that
    sometimes does not arrive; a pilot that stops renewing is a pilot that is
    gone, and a drone with centred sticks hovers.
    """
    STICK_HZ = 50
    CONNECT_RETRY_S = 1.0
    CONTROL_TIMEOUT_S = 3.0
    VIDEO_TIMEOUT_S = 1.0

    def __init__(self, addr=TELLO_ADDR, control_port=CONTROL_PORT, video_port=VIDEO_PORT,
                 alt_limit_m: int = 5, encoder_rate: int = 4, exposure: int = 0, zoom: bool = False):
        self.addr = addr
        self.control_port = control_port
        self.video_port = video_port
        self.alt_limit_m = alt_limit_m
        self.encoder_rate = encoder_rate
        self.exposure = exposure
        self.zoom = zoom
        self.on_flight: Callable[[FlightData], None] = lambda f: None
        self.on_video: Callable[[bytes], None] = lambda b: None
        self.on_video_loss: Callable[[int], None] = lambda n: None
        self.on_state: Callable[[str], None] = lambda s: None
        self.flight = FlightData()
        self.logs = LogState()
        self.wifi_strength = 0
        self.wifi_disturb = 0
        self.light = 0
        self.connected = False
        self.video_enabled = False
        self.last_control_rx = 0.0
        self.last_video_rx = 0.0
        self.last_flight_rx = 0.0
        self.seq = 0
        self._sticks = (0.0, 0.0, 0.0, 0.0)
        self._sticks_until = 0.0
        self._fast = False
        self._ctl: Optional[asyncio.DatagramTransport] = None
        self._vid: Optional[asyncio.DatagramTransport] = None
        self._tasks: list = []
        self.assembler = FrameAssembler(self._video_frame, self._video_loss)
        self.stats = {'tx': 0, 'rx': 0, 'bad_crc': 0, 'unknown': 0, 'video_bytes': 0, 'video_starts': 0, 'reconnects': 0}

    # ── lifecycle ──
    async def start(self) -> None:
        loop = asyncio.get_running_loop()
        self._ctl, _ = await loop.create_datagram_endpoint(
            lambda: _Proto(self._control_rx), local_addr=('0.0.0.0', self.control_port))
        self._vid, _ = await loop.create_datagram_endpoint(
            lambda: _Proto(self._video_rx), local_addr=('0.0.0.0', self.video_port))
        sock = self._vid.get_extra_info('socket')
        if sock is not None:
            import socket as _s
            sock.setsockopt(_s.SOL_SOCKET, _s.SO_RCVBUF, 1 << 20)
        self._tasks = [asyncio.create_task(self._connect_loop()), asyncio.create_task(self._stick_loop())]
        log.info('tello driver up: control :%d → %s:%d, video :%d', self.control_port, *self.addr, self.video_port)

    async def stop(self) -> None:
        for t in self._tasks:
            t.cancel()
        for t in self._tasks:
            try:
                await t
            except (asyncio.CancelledError, Exception):
                pass
        for tr in (self._ctl, self._vid):
            if tr:
                tr.close()

    # ── commands ──
    def _send(self, cmd: int, pkt_type: int, payload: bytes = b'') -> None:
        if not self._ctl:
            return
        self.seq = (self.seq + 1) & 0xffff
        self._ctl.sendto(build_packet(cmd, pkt_type, payload, self.seq), self.addr)
        self.stats['tx'] += 1

    def set_sticks(self, roll: float, pitch: float, throttle: float, yaw: float, hold_s: float = 0.4) -> None:
        self._sticks = (roll, pitch, throttle, yaw)
        self._sticks_until = time.monotonic() + hold_s

    def centre_sticks(self) -> None:
        self._sticks = (0.0, 0.0, 0.0, 0.0)
        self._sticks_until = 0.0

    def takeoff(self) -> None:
        # The altitude limit precedes take-off in every reference implementation.
        self._send(SET_ALT_LIMIT_CMD, PT_SET, bytes([max(1, min(30, self.alt_limit_m)), 0]))
        self._send(TAKEOFF_CMD, PT_SET)
        log.info('takeoff (alt limit %d m)', self.alt_limit_m)

    def land(self) -> None:
        self.centre_sticks()
        self._send(LAND_CMD, PT_SET, b'\x00')
        log.info('land')

    def palm_land(self) -> None:
        self._send(PALM_LAND_CMD, PT_SET, b'\x00')

    def start_video(self) -> None:
        """Ask for parameter sets and a keyframe. Idempotent; the drone answers every time."""
        self.video_enabled = True
        self._send(VIDEO_START_CMD, PT_DATA2)
        self.stats['video_starts'] += 1

    def set_video_encoder_rate(self, level: int) -> None:
        self.encoder_rate = max(0, min(5, int(level)))
        self._send(VIDEO_ENCODER_RATE_CMD, PT_SET, bytes([self.encoder_rate]))

    def set_exposure(self, level: int) -> None:
        self.exposure = max(0, min(2, int(level)))
        self._send(EXPOSURE_CMD, PT_GET, bytes([self.exposure]))

    def set_video_mode(self, zoom: bool) -> None:
        self.zoom = zoom
        self._send(VIDEO_MODE_CMD, PT_SET, bytes([int(zoom)]))

    def _send_time(self) -> None:
        self._send(TIME_CMD, PT_DATA1, b'\x00' + time_payload())

    # ── loops ──
    async def _connect_loop(self) -> None:
        while True:
            now = time.monotonic()
            if self.connected and now - self.last_control_rx > self.CONTROL_TIMEOUT_S:
                log.warning('control link timed out (%.1fs without a packet)', now - self.last_control_rx)
                self._set_connected(False)
            if not self.connected:
                if self._ctl:
                    self._ctl.sendto(conn_req(self.video_port), self.addr)
            elif self.video_enabled and now - self.last_video_rx > self.VIDEO_TIMEOUT_S:
                # No picture for a second: the drone forgets a video request
                # across its own hiccups, so ask again (TelloPy does the same).
                self.start_video()
            await asyncio.sleep(self.CONNECT_RETRY_S)

    async def _stick_loop(self) -> None:
        period = 1.0 / self.STICK_HZ
        while True:
            if self.connected:
                if self._sticks_until and time.monotonic() > self._sticks_until:
                    if any(self._sticks):
                        log.info('stick hold expired — centring')
                    self._sticks = (0.0, 0.0, 0.0, 0.0)
                    self._sticks_until = 0.0
                r, p, t, y = self._sticks
                self._send(STICK_CMD, PT_DATA2, stick_payload(r, p, t, y, self._fast) + time_payload())
            await asyncio.sleep(period)

    def _set_connected(self, v: bool) -> None:
        if v == self.connected:
            return
        self.connected = v
        if v:
            self.stats['reconnects'] += 1
        else:
            self.centre_sticks()
        self.on_state('connected' if v else 'disconnected')

    # ── receive ──
    def _control_rx(self, data: bytes, addr) -> None:
        self.stats['rx'] += 1
        self.last_control_rx = time.monotonic()
        if data.startswith(b'conn_ack:'):
            was = self.connected
            self._set_connected(True)
            if not was:
                log.info('connected to %s (conn_ack)', addr[0])
                self._send_time()
                self._send_video_setup()
            return
        p = parse_packet(data)
        if p is None:
            if len(data) and data[0] == START_OF_PACKET:
                self.stats['bad_crc'] += 1
            return
        if p.cmd == FLIGHT_MSG:
            self.flight = FlightData.parse(p.payload)
            self.last_flight_rx = self.last_control_rx
            self.on_flight(self.flight)
        elif p.cmd == WIFI_MSG:
            if len(p.payload) >= 2:
                self.wifi_strength, self.wifi_disturb = p.payload[0], p.payload[1]
            elif p.payload:
                self.wifi_strength = p.payload[0]
        elif p.cmd == LIGHT_MSG:
            self.light = p.payload[0] if p.payload else 0
        elif p.cmd == LOG_HEADER_MSG:
            # Acknowledging the header is what starts the 0x1051 log stream.
            if len(p.payload) >= 2:
                self._send(LOG_HEADER_MSG, PT_DATA1, b'\x00' + p.payload[0:2])
        elif p.cmd == LOG_DATA_MSG:
            try:
                self.logs.update(p.payload[1:])
            except Exception as e:  # never let telemetry parsing take the loop down
                log.debug('log data: %s', e)
        elif p.cmd == TIME_CMD:
            self._send_time()
        elif p.cmd in (LOG_CONFIG_MSG, ALT_LIMIT_MSG, LOW_BAT_THRESHOLD_MSG, ATT_LIMIT_MSG,
                       SET_ALT_LIMIT_CMD, TAKEOFF_CMD, LAND_CMD, VIDEO_START_CMD, VIDEO_ENCODER_RATE_CMD,
                       PALM_LAND_CMD, EXPOSURE_CMD, VIDEO_MODE_CMD, FLIP_CMD):
            log.debug('ack cmd=0x%04x seq=%d', p.cmd, p.seq)
        else:
            self.stats['unknown'] += 1
            log.debug('unknown cmd=0x%04x len=%d', p.cmd, len(p.payload))

    def _send_video_setup(self) -> None:
        self._send(VIDEO_MODE_CMD, PT_SET, bytes([int(self.zoom)]))
        self._send(EXPOSURE_CMD, PT_GET, bytes([self.exposure]))
        self._send(VIDEO_ENCODER_RATE_CMD, PT_SET, bytes([self.encoder_rate]))
        self.start_video()

    def _video_rx(self, data: bytes, addr) -> None:
        self.last_video_rx = time.monotonic()
        self.stats['video_bytes'] += len(data)
        self.assembler.push(data)

    def _video_frame(self, frame: bytes) -> None:
        self.on_video(frame)

    def _video_loss(self, n: int) -> None:
        self.on_video_loss(n)


class _Proto(asyncio.DatagramProtocol):
    def __init__(self, on_dgram):
        self.on_dgram = on_dgram

    def datagram_received(self, data, addr):
        self.on_dgram(data, addr)

    def error_received(self, exc):
        log.debug('socket error: %s', exc)
