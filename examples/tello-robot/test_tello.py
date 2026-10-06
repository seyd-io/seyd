# Copyright 2026 Anton Gravestam
# SPDX-License-Identifier: Apache-2.0
"""
Unit tests for the Tello example: packet framing against byte vectors produced
by TelloPy (the reference implementation), frame reassembly, Annex B splitting
and the RTP re-framing. Run with:

    python3 -m unittest discover -s examples/tello-robot -p 'test_*.py'
"""
import datetime
import struct
import unittest

import h264rtp
import tello as T

# Produced by TelloPy's Packet/fixup with these exact inputs (2026-09-22).
T_FIXED = datetime.datetime(2026, 1, 1, 12, 34, 56, 789000)
VECTORS = {
    'takeoff': 'cc58007c6854000000b289',
    'land': 'cc6000276855000000007eee',
    'video_start': 'cc58007c60250000006c95',
    'encoder_rate_4': 'cc600027682000000004fd9b',
    'exposure_0': 'cc600027483400000000e9c1',
    'alt_limit_30': 'cc68005168580000001e00a85c',
    'time': 'ccb0007f5046000000000c0022003800150003008eb2',
    'log_ack_1234': 'cc7000cb50501000000034123510',
    'video_mode_0': 'cc600027683100000000dd62',
    'stick_centre': 'ccd8005360500000000004200001080c0022003800150003004bfd',
    'stick_mixed': 'ccd8005360500000004a654b2923040c002200380015000300e16d',
    'conn_req': '636f6e6e5f7265713a9617',
}


class Framing(unittest.TestCase):
    def test_crc_seeds(self):
        self.assertEqual(T.crc8(b'\xcc\x58\x00'), 124)
        self.assertEqual(T.crc16(bytes.fromhex('cc580065680052000000')), 55026)

    def test_packets_match_tellopy(self):
        self.assertEqual(T.build_packet(T.TAKEOFF_CMD, T.PT_SET).hex(), VECTORS['takeoff'])
        self.assertEqual(T.build_packet(T.LAND_CMD, T.PT_SET, b'\x00').hex(), VECTORS['land'])
        self.assertEqual(T.build_packet(T.VIDEO_START_CMD, T.PT_DATA2).hex(), VECTORS['video_start'])
        self.assertEqual(T.build_packet(T.VIDEO_ENCODER_RATE_CMD, T.PT_SET, b'\x04').hex(), VECTORS['encoder_rate_4'])
        self.assertEqual(T.build_packet(T.EXPOSURE_CMD, T.PT_GET, b'\x00').hex(), VECTORS['exposure_0'])
        self.assertEqual(T.build_packet(T.SET_ALT_LIMIT_CMD, T.PT_SET, b'\x1e\x00').hex(), VECTORS['alt_limit_30'])
        self.assertEqual(T.build_packet(T.TIME_CMD, T.PT_DATA1, b'\x00' + T.time_payload(T_FIXED)).hex(), VECTORS['time'])
        self.assertEqual(T.build_packet(T.LOG_HEADER_MSG, T.PT_DATA1, b'\x00' + struct.pack('<H', 0x1234)).hex(), VECTORS['log_ack_1234'])
        self.assertEqual(T.build_packet(T.VIDEO_MODE_CMD, T.PT_SET, b'\x00').hex(), VECTORS['video_mode_0'])

    def test_stick_packing_matches_tellopy(self):
        centre = T.build_packet(T.STICK_CMD, T.PT_DATA2, T.stick_payload(0, 0, 0, 0) + T.time_payload(T_FIXED))
        self.assertEqual(centre.hex(), VECTORS['stick_centre'])
        mixed = T.build_packet(T.STICK_CMD, T.PT_DATA2, T.stick_payload(0.5, -1.0, 0.25, -0.75) + T.time_payload(T_FIXED))
        self.assertEqual(mixed.hex(), VECTORS['stick_mixed'])

    def test_conn_req_encodes_the_video_port(self):
        self.assertEqual(T.conn_req(6038).hex(), VECTORS['conn_req'])
        self.assertEqual(T.conn_req(6038)[9:], bytes([0x96, 0x17]))

    def test_length_field_spans_two_bytes_beyond_31_bytes(self):
        pkt = T.build_packet(0x1051, T.PT_DATA1, bytes(60))
        total = len(pkt)
        self.assertEqual(pkt[1] | (pkt[2] << 8), total << 3)
        self.assertEqual(pkt[3], T.crc8(pkt[:3]))

    def test_parse_round_trip_and_crc_rejection(self):
        pkt = T.build_packet(T.FLIGHT_MSG, T.PT_DATA1, bytes(range(24)), seq=7)
        p = T.parse_packet(pkt)
        self.assertEqual((p.cmd, p.seq, p.pkt_type, p.payload), (T.FLIGHT_MSG, 7, T.PT_DATA1, bytes(range(24))))
        bad = bytearray(pkt)
        bad[12] ^= 0xff
        self.assertIsNone(T.parse_packet(bytes(bad)))
        self.assertIsNone(T.parse_packet(b'conn_ack:\x96\x17'))


class Telemetry(unittest.TestCase):
    def test_flight_data_fields(self):
        d = bytearray(24)
        struct.pack_into('<hhhhh', d, 0, 12, -3, 4, 5, 250)
        d[10] = 0b0000_0101
        d[12] = 77
        d[17] = 0b0010_0001   # em_sky, battery_low
        d[18] = 6
        f = T.FlightData.parse(bytes(d))
        self.assertEqual((f.height, f.north_speed, f.east_speed, f.ground_speed, f.fly_time), (12, -3, 4, 5, 250))
        self.assertEqual((f.imu_state, f.pressure_state, f.down_visual_state), (1, 0, 1))
        self.assertEqual(f.battery_percentage, 77)
        self.assertTrue(f.flying)
        self.assertEqual(f.battery_low, 1)
        self.assertEqual(f.fly_mode, 6)
        self.assertFalse(T.FlightData.parse(b'\x00' * 5).flying)

    def test_log_records_are_unxored_and_walked(self):
        key = 0x5a

        def rec(rec_id, payload):
            body = bytes(x ^ key for x in payload)
            length = 10 + len(body) + 2
            return bytes([0x55]) + struct.pack('<H', length) + b'\x00' + struct.pack('<H', rec_id) + bytes([key]) + b'\x00\x00\x00' + body + b'\x00\x00'
        mvo = struct.pack('<hhhh', 0, 150, -25, 10) + struct.pack('<fff', 1.5, -2.0, 0.75) + b'\x00' * 8
        imu = b'\x00' * 48 + struct.pack('<ffff', 0.7071068, 0.0, 0.0, 0.7071068) + b'\x00' * 16
        s = T.LogState()
        s.update(rec(T.LogState.ID_MVO, mvo) + rec(999, b'\x01\x02') + rec(T.LogState.ID_IMU_ATTI, imu))
        self.assertEqual(s.vel, (1.5, -0.25, 0.1))
        self.assertAlmostEqual(s.pos[1], -2.0, places=5)
        yaw, pitch, roll = s.euler_deg()
        self.assertAlmostEqual(yaw, 90.0, places=3)
        self.assertAlmostEqual(pitch, 0.0, places=3)
        self.assertIn(999, s.unknown_ids)
        s.update(b'\x55\x05')   # truncated: must not raise


class Assembler(unittest.TestCase):
    def setUp(self):
        self.frames, self.losses = [], []
        self.a = T.FrameAssembler(self.frames.append, self.losses.append)

    def dg(self, frame, idx, last=False, body=b'x'):
        return bytes([frame, idx | (0x80 if last else 0)]) + body

    def test_end_flag_closes_a_frame(self):
        self.a.push(self.dg(1, 0, body=b'ab'))
        self.a.push(self.dg(1, 1, body=b'cd'))
        self.a.push(self.dg(1, 2, last=True, body=b'ef'))
        self.assertEqual(self.frames, [b'abcdef'])
        self.assertEqual(self.losses, [])

    def test_frame_change_without_flag_closes_the_previous_frame_when_the_flag_is_unknown(self):
        self.a.push(self.dg(1, 0, body=b'ab'))
        self.a.push(self.dg(2, 0, body=b'cd'))
        self.assertEqual(self.frames, [b'ab'])            # no flag ever seen: the frame number is the boundary
        self.a.push(self.dg(2, 1, last=True, body=b'ef'))
        self.assertEqual(self.frames, [b'ab', b'cdef'])

    def test_a_lost_last_datagram_is_a_loss_once_the_flag_is_known(self):
        self.a.push(self.dg(1, 0, last=True, body=b'ab'))
        self.a.push(self.dg(2, 0, body=b'cd'))             # frame 2's last datagram (with the flag) never arrives
        self.a.push(self.dg(3, 0, last=True, body=b'ef'))
        self.assertEqual(self.frames, [b'ab', b'ef'])
        self.assertEqual(self.losses, [1])

    def test_a_gap_drops_the_whole_frame_and_reports_one_loss(self):
        self.a.push(self.dg(1, 0, body=b'ab'))
        self.a.push(self.dg(1, 2, last=True, body=b'ef'))   # idx 1 missing
        self.assertEqual(self.frames, [])
        self.assertEqual(self.losses, [1])
        self.a.push(self.dg(2, 1, body=b'zz'))               # first datagram of a frame missing
        self.a.push(self.dg(2, 2, last=True, body=b'zz'))
        self.assertEqual(self.losses, [1, 1])
        self.assertEqual(self.a.lost_frames, 2)


class AnnexB(unittest.TestCase):
    def test_split_handles_both_start_codes(self):
        buf = b'\x00\x00\x00\x01\x67\x42\x00\x1f' + b'\x00\x00\x01\x68\xce' + b'\x00\x00\x00\x01\x65\x88\x00\x00\x03\x01'
        self.assertEqual(h264rtp.split_annexb(buf), [b'\x67\x42\x00\x1f', b'\x68\xce', b'\x65\x88\x00\x00\x03\x01'])
        self.assertEqual(h264rtp.split_annexb(b''), [])

    def test_codec_string_from_sps(self):
        self.assertEqual(h264rtp.sps_codec_string(b'\x67\x42\x00\x1f'), 'avc1.42001f')
        self.assertEqual(h264rtp.sps_codec_string(b'\x67\x4d\x40\x1e'), 'avc1.4d401e')


def rtp_fields(pkt):
    v_p_x_cc, m_pt, seq, ts, ssrc = struct.unpack('!BBHII', pkt[:12])
    return {'marker': bool(m_pt & 0x80), 'pt': m_pt & 0x7f, 'seq': seq, 'ts': ts, 'payload': pkt[12:]}


class Rtp(unittest.TestCase):
    def test_small_nals_single_packets_marker_on_last(self):
        pk = h264rtp.Packetizer(ssrc=1)
        pkts = pk.packets([b'\x67\x01', b'\x68\x02', b'\x65\x03'], ts=9000)
        f = [rtp_fields(p) for p in pkts]
        self.assertEqual([x['marker'] for x in f], [False, False, True])
        self.assertEqual([x['payload'] for x in f], [b'\x67\x01', b'\x68\x02', b'\x65\x03'])
        self.assertEqual([x['ts'] for x in f], [9000] * 3)
        self.assertEqual([x['seq'] for x in f], [f[0]['seq'], (f[0]['seq'] + 1) & 0xffff, (f[0]['seq'] + 2) & 0xffff])

    def test_large_nal_becomes_fu_a_and_reassembles(self):
        nal = b'\x65' + bytes(range(256)) * 20   # 5121 bytes, IDR with NRI 3
        pkts = h264rtp.Packetizer(ssrc=1).packets([nal], ts=1)
        self.assertGreater(len(pkts), 3)
        rebuilt = b''
        for i, p in enumerate(pkts):
            f = rtp_fields(p)
            ind, fu = f['payload'][0], f['payload'][1]
            self.assertEqual(ind, (0x65 & 0xe0) | 28)
            self.assertEqual(fu & 0x1f, 5)
            self.assertEqual(bool(fu & 0x80), i == 0)
            self.assertEqual(bool(fu & 0x40), i == len(pkts) - 1)
            self.assertEqual(f['marker'], i == len(pkts) - 1)
            self.assertLessEqual(len(p) - 12, h264rtp.MTU_PAYLOAD)
            if i == 0:
                rebuilt += bytes([(ind & 0xe0) | (fu & 0x1f)])
            rebuilt += f['payload'][2:]
        self.assertEqual(rebuilt, nal)


class RelayRules(unittest.TestCase):
    SPS = b'\x00\x00\x00\x01\x67\x42\x00\x1f\xaa'
    PPS = b'\x00\x00\x00\x01\x68\xce\x38\x80'
    IDR = b'\x00\x00\x00\x01\x65\x88\x84\x00'
    P = b'\x00\x00\x00\x01\x41\x9a\x02\x03'

    def setUp(self):
        self.sent, self.kf = [], []
        self.clock = [0.0]
        self.r = h264rtp.Relay(self.sent.append, lambda: self.clock[0], lambda: self.kf.append(1), after_loss='wait')

    def payloads(self):
        return [rtp_fields(p)['payload'] for p in self.sent]

    def test_parameter_sets_are_cached_and_prepended_to_the_idr(self):
        self.assertEqual(self.r.push_picture(self.SPS + self.PPS), (0, False))   # a picture with no slice goes nowhere
        self.assertEqual(self.sent, [])
        n, key = self.r.push_picture(self.IDR)
        self.assertTrue(key)
        self.assertEqual(self.payloads(), [self.SPS[4:], self.PPS[4:], self.IDR[4:]])
        self.assertTrue(rtp_fields(self.sent[-1])['marker'])
        self.assertEqual(self.r.codec, 'avc1.42001f')

    def test_nothing_before_the_first_keyframe_and_nothing_after_a_loss_until_one(self):
        self.assertEqual(self.r.push_picture(self.P), (0, False))
        self.assertEqual(self.kf, [1])                    # deltas before any keyframe: ask for one
        self.r.push_picture(self.SPS + self.PPS + self.IDR)
        self.r.push_picture(self.P)
        self.assertEqual(len(self.sent), 4)
        self.r.mark_loss()
        self.assertEqual(self.kf, [1, 1])                 # asked the drone once for the loss
        self.r.mark_loss()
        self.assertEqual(self.kf, [1, 1])                 # not again while already waiting
        self.assertEqual(self.r.push_picture(self.P), (0, False))
        self.assertEqual(self.r.stats['dropped_awaiting_idr'], 2)
        self.clock[0] = 1.0
        n, key = self.r.push_picture(self.IDR)
        self.assertTrue(key)
        self.assertEqual(rtp_fields(self.sent[-1])['ts'], 90000)
        self.assertEqual(self.r.idr_intervals, [1.0])
        self.r.push_picture(self.P)
        self.assertEqual(self.r.stats['frames'], 4)

    def test_an_idr_before_its_parameter_sets_is_not_forwarded(self):
        self.assertEqual(self.r.push_picture(self.IDR), (0, False))
        self.assertEqual(self.sent, [])
        self.assertEqual(self.kf, [1])                    # asked for another
        self.r.push_picture(self.SPS)
        self.r.push_picture(self.PPS)                     # the drone sends them as pictures of their own
        n, key = self.r.push_picture(self.IDR)
        self.assertTrue(key)
        self.assertEqual(self.payloads(), [self.SPS[4:], self.PPS[4:], self.IDR[4:]])

    def test_forward_mode_keeps_deltas_flowing_and_asks_until_the_keyframe(self):
        r = h264rtp.Relay(self.sent.append, lambda: self.clock[0], lambda: self.kf.append(1))   # forward is the default
        self.assertEqual(r.push_picture(self.P), (0, False))   # still nothing before the first keyframe
        self.kf.clear()
        r.push_picture(self.SPS + self.PPS + self.IDR)
        before = len(self.sent)
        r.mark_loss()
        r.mark_loss()
        self.assertEqual(self.kf, [1])                    # one request for the burst
        self.assertEqual(r.push_picture(self.P), (1, False))   # the delta after the torn picture is forwarded
        self.assertEqual(len(self.sent), before + 1)
        self.clock[0] = 0.6
        r.push_picture(self.P)
        self.assertEqual(self.kf, [1, 1])                 # still no keyframe: asked again
        self.assertEqual(r.stats['forwarded_unrepaired'], 2)
        r.push_picture(self.IDR)
        self.clock[0] = 2.0
        r.push_picture(self.P)
        self.assertEqual(self.kf, [1, 1])                 # repaired: no more requests
        self.assertEqual(r.stats['dropped_awaiting_idr'], 1)   # only the one before the first keyframe

    def test_rerequests_while_waiting_and_resumes_after_the_bound(self):
        self.r.push_picture(self.SPS + self.PPS + self.IDR)
        self.r.mark_loss()
        self.assertEqual(self.kf, [1])
        self.clock[0] = 0.3
        self.r.push_picture(self.P)
        self.assertEqual(self.kf, [1])                    # too soon to ask again
        self.clock[0] = 0.6
        self.r.push_picture(self.P)
        self.assertEqual(self.kf, [1, 1])                 # asked again after 500 ms
        self.clock[0] = 1.6
        n, key = self.r.push_picture(self.P)              # past resume_after_s: forwarded despite no IDR
        self.assertEqual((n, key), (1, False))
        self.assertFalse(self.r.awaiting_idr)
        self.assertEqual(self.r.stats['resumed_without_idr'], 1)


if __name__ == '__main__':
    unittest.main()
