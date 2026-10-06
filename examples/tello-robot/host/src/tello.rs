//! The Tello's binary protocol — the same subset `../tello.py` speaks, so the
//! two hosts differ only in what sits between the drone and the agent. See
//! that file's module comment and DEMO-TELLO.md for the protocol itself.
//!
//! Pure functions over bytes; the sockets and loops are in `main.rs`.

use std::time::{SystemTime, UNIX_EPOCH};

pub const START_OF_PACKET: u8 = 0xcc;
pub const WIFI_MSG: u16 = 0x001a;
pub const VIDEO_ENCODER_RATE_CMD: u16 = 0x0020;
pub const VIDEO_START_CMD: u16 = 0x0025;
pub const VIDEO_MODE_CMD: u16 = 0x0031;
pub const EXPOSURE_CMD: u16 = 0x0034;
pub const TIME_CMD: u16 = 0x0046;
pub const STICK_CMD: u16 = 0x0050;
pub const TAKEOFF_CMD: u16 = 0x0054;
pub const LAND_CMD: u16 = 0x0055;
pub const FLIGHT_MSG: u16 = 0x0056;
pub const SET_ALT_LIMIT_CMD: u16 = 0x0058;
pub const LOG_HEADER_MSG: u16 = 0x1050;
pub const LOG_DATA_MSG: u16 = 0x1051;

pub const PT_GET: u8 = 0x48;
pub const PT_DATA1: u8 = 0x50;
pub const PT_DATA2: u8 = 0x60;
pub const PT_SET: u8 = 0x68;

const CRC8_TABLE: [u8; 256] = [
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
    0x74, 0x2a, 0xc8, 0x96, 0x15, 0x4b, 0xa9, 0xf7, 0xb6, 0xe8, 0x0a, 0x54, 0xd7, 0x89, 0x6b, 0x35,
];

const CRC16_TABLE: [u16; 256] = [
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
    0xf78f, 0xe606, 0xd49d, 0xc514, 0xb1ab, 0xa022, 0x92b9, 0x8330, 0x7bc7, 0x6a4e, 0x58d5, 0x495c, 0x3de3, 0x2c6a, 0x1ef1, 0x0f78,
];

pub fn crc8(buf: &[u8]) -> u8 {
    buf.iter().fold(0x77u8, |c, &v| CRC8_TABLE[(c ^ v) as usize])
}

pub fn crc16(buf: &[u8]) -> u16 {
    buf.iter().fold(0x3692u16, |c, &v| CRC16_TABLE[((c ^ v as u16) & 0xff) as usize] ^ (c >> 8))
}

/// Frame one command: `0xcc`, 13-bit length `<< 3`, CRC-8, type, command, sequence, payload, CRC-16.
pub fn build_packet(cmd: u16, pkt_type: u8, payload: &[u8], seq: u16) -> Vec<u8> {
    let total = 9 + payload.len() + 2;
    let mut b = Vec::with_capacity(total);
    b.push(START_OF_PACKET);
    b.push(((total << 3) & 0xff) as u8);
    b.push(((total >> 5) & 0xff) as u8);
    let c8 = crc8(&b);
    b.push(c8);
    b.push(pkt_type);
    b.extend_from_slice(&cmd.to_le_bytes());
    b.extend_from_slice(&seq.to_le_bytes());
    b.extend_from_slice(payload);
    let c16 = crc16(&b);
    b.extend_from_slice(&c16.to_le_bytes());
    b
}

/// Wall-clock fields the stick and time packets carry: h, m, s, ms low, ms high as five LE u16.
pub fn time_payload(h: u16, m: u16, s: u16, ms: u16) -> [u8; 10] {
    let mut out = [0u8; 10];
    for (i, v) in [h, m, s, ms & 0xff, (ms >> 8) & 0xff].iter().enumerate() {
        out[i * 2..i * 2 + 2].copy_from_slice(&v.to_le_bytes());
    }
    out
}

pub fn time_payload_now() -> [u8; 10] {
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = since.as_secs();
    let ms = since.subsec_millis() as u16;
    time_payload(((secs / 3600) % 24) as u16, ((secs / 60) % 60) as u16, (secs % 60) as u16, ms)
}

/// Four axes in -1..1 → 11 bits each around 1024 ± 660, fast-mode bit, six LE bytes.
pub fn stick_payload(roll: f32, pitch: f32, throttle: f32, yaw: f32, fast: bool) -> [u8; 6] {
    let axis = |v: f32| ((1024.0 + 660.0 * v.clamp(-1.0, 1.0)) as i64 as u64) & 0x7ff;
    let packed = axis(roll) | (axis(pitch) << 11) | (axis(throttle) << 22) | (axis(yaw) << 33) | ((fast as u64) << 44);
    let b = packed.to_le_bytes();
    [b[0], b[1], b[2], b[3], b[4], b[5]]
}

/// `conn_req:` + the video port as a little-endian u16 (6038 = bytes `96 17`).
pub fn conn_req(video_port: u16) -> Vec<u8> {
    let mut v = b"conn_req:".to_vec();
    v.extend_from_slice(&video_port.to_le_bytes());
    v
}

pub struct Parsed<'a> {
    pub cmd: u16,
    pub seq: u16,
    pub payload: &'a [u8],
}

/// A framed packet, or `None` for anything else (bad start byte, short, bad CRC).
pub fn parse_packet(data: &[u8]) -> Option<Parsed<'_>> {
    if data.len() < 11 || data[0] != START_OF_PACKET {
        return None;
    }
    let n = data.len();
    let want = u16::from_le_bytes([data[n - 2], data[n - 1]]);
    if want != crc16(&data[..n - 2]) {
        return None;
    }
    Some(Parsed {
        cmd: u16::from_le_bytes([data[5], data[6]]),
        seq: u16::from_le_bytes([data[7], data[8]]),
        payload: &data[9..n - 2],
    })
}

/// Command 0x56 as the community decodes it: height in dm, speeds in dm/s, fly_time in 0.1 s.
#[derive(Debug, Clone, Copy, Default)]
pub struct FlightData {
    pub height: i16,
    pub north_speed: i16,
    pub east_speed: i16,
    pub ground_speed: i16,
    pub fly_time: i16,
    pub imu_state: bool,
    pub wind_state: bool,
    pub battery_percentage: u8,
    pub em_sky: bool,
    pub battery_low: bool,
    pub battery_lower: bool,
    pub fly_mode: u8,
    pub temperature_height: bool,
}

impl FlightData {
    pub fn parse(d: &[u8]) -> Self {
        if d.len() < 24 {
            return Self::default();
        }
        let i16_at = |i: usize| i16::from_le_bytes([d[i], d[i + 1]]);
        Self {
            height: i16_at(0),
            north_speed: i16_at(2),
            east_speed: i16_at(4),
            ground_speed: i16_at(6),
            fly_time: i16_at(8),
            imu_state: d[10] & 1 != 0,
            wind_state: d[10] & 0x80 != 0,
            battery_percentage: d[12],
            em_sky: d[17] & 1 != 0,
            battery_low: d[17] & 0x20 != 0,
            battery_lower: d[17] & 0x40 != 0,
            fly_mode: d[18],
            temperature_height: d[23] & 1 != 0,
        }
    }
    pub fn flying(&self) -> bool {
        self.em_sky
    }
}

/// Visual odometry (record 29) and IMU attitude (record 2048) from the 0x1051 log stream.
#[derive(Debug, Clone, Copy, Default)]
pub struct LogState {
    pub pos: [f32; 3],
    pub vel: [f32; 3],
    pub quat: [f32; 4],
}

impl LogState {
    pub fn update(&mut self, data: &[u8]) {
        let mut pos = 0usize;
        while pos + 10 <= data.len() {
            if data[pos] != 0x55 {
                return;
            }
            let len = u16::from_le_bytes([data[pos + 1], data[pos + 2]]) as usize;
            if len < 12 || pos + len > data.len() {
                return;
            }
            let id = u16::from_le_bytes([data[pos + 4], data[pos + 5]]);
            let key = data[pos + 6];
            let payload: Vec<u8> = data[pos + 10..pos + len - 2].iter().map(|x| x ^ key).collect();
            let f32_at = |i: usize| f32::from_le_bytes([payload[i], payload[i + 1], payload[i + 2], payload[i + 3]]);
            if id == 29 && payload.len() >= 20 {
                let v = |i: usize| i16::from_le_bytes([payload[i], payload[i + 1]]) as f32 / 100.0;
                self.vel = [v(2), v(4), v(6)];
                self.pos = [f32_at(8), f32_at(12), f32_at(16)];
            } else if id == 2048 && payload.len() >= 64 {
                self.quat = [f32_at(48), f32_at(52), f32_at(56), f32_at(60)];
            }
            pos += len;
        }
    }

    /// (yaw, pitch, roll) in degrees, quaternion read as (w, x, y, z).
    pub fn euler_deg(&self) -> (f32, f32, f32) {
        let [w, x, y, z] = self.quat;
        let roll = (2.0 * (w * x + y * z)).atan2(1.0 - 2.0 * (x * x + y * y)).to_degrees();
        let pitch = (2.0 * (w * y - z * x)).clamp(-1.0, 1.0).asin().to_degrees();
        let yaw = (2.0 * (w * z + x * y)).atan2(1.0 - 2.0 * (y * y + z * z)).to_degrees();
        (yaw, pitch, roll)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The same vectors as ../test_tello.py, produced by TelloPy.
    #[test]
    fn packets_match_tellopy() {
        assert_eq!(hex::encode(build_packet(TAKEOFF_CMD, PT_SET, &[], 0)), "cc58007c6854000000b289");
        assert_eq!(hex::encode(build_packet(LAND_CMD, PT_SET, &[0], 0)), "cc6000276855000000007eee");
        assert_eq!(hex::encode(build_packet(VIDEO_START_CMD, PT_DATA2, &[], 0)), "cc58007c60250000006c95");
        assert_eq!(hex::encode(build_packet(VIDEO_ENCODER_RATE_CMD, PT_SET, &[4], 0)), "cc600027682000000004fd9b");
        assert_eq!(hex::encode(build_packet(SET_ALT_LIMIT_CMD, PT_SET, &[0x1e, 0], 0)), "cc68005168580000001e00a85c");
        let mut t = vec![0u8];
        t.extend_from_slice(&time_payload(12, 34, 56, 789));
        assert_eq!(hex::encode(build_packet(TIME_CMD, PT_DATA1, &t, 0)), "ccb0007f5046000000000c0022003800150003008eb2");
        assert_eq!(hex::encode(build_packet(LOG_HEADER_MSG, PT_DATA1, &[0, 0x34, 0x12], 0)), "cc7000cb50501000000034123510");
    }

    #[test]
    fn stick_packing_matches_tellopy() {
        let mut p = stick_payload(0.0, 0.0, 0.0, 0.0, false).to_vec();
        p.extend_from_slice(&time_payload(12, 34, 56, 789));
        assert_eq!(hex::encode(build_packet(STICK_CMD, PT_DATA2, &p, 0)), "ccd8005360500000000004200001080c0022003800150003004bfd");
        let mut p = stick_payload(0.5, -1.0, 0.25, -0.75, false).to_vec();
        p.extend_from_slice(&time_payload(12, 34, 56, 789));
        assert_eq!(hex::encode(build_packet(STICK_CMD, PT_DATA2, &p, 0)), "ccd8005360500000004a654b2923040c002200380015000300e16d");
    }

    #[test]
    fn conn_req_and_parse() {
        assert_eq!(hex::encode(conn_req(6038)), "636f6e6e5f7265713a9617");
        let pkt = build_packet(FLIGHT_MSG, PT_DATA1, &(0..24).collect::<Vec<u8>>(), 7);
        let p = parse_packet(&pkt).unwrap();
        assert_eq!((p.cmd, p.seq, p.payload.len()), (FLIGHT_MSG, 7, 24));
        let mut bad = pkt.clone();
        bad[12] ^= 0xff;
        assert!(parse_packet(&bad).is_none());
        assert_eq!(crc8(&[0xcc, 0x58, 0x00]), 124);
        assert_eq!(crc16(&hex::decode("cc580065680052000000").unwrap()), 55026);
    }

    #[test]
    fn flight_data_fields() {
        let mut d = [0u8; 24];
        d[0..2].copy_from_slice(&12i16.to_le_bytes());
        d[12] = 77;
        d[17] = 0b0010_0001;
        d[18] = 6;
        let f = FlightData::parse(&d);
        assert_eq!((f.height, f.battery_percentage, f.fly_mode), (12, 77, 6));
        assert!(f.flying() && f.battery_low);
    }
}
