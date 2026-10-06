//! tello-host — the Tello drone as a Seyd robot, hosting `seyd_core::Agent`
//! directly. The counterpart of `../bridge.py` + `seydd`: same protocol to the
//! drone, same `flight` and `telemetry` channels, same safety rules, same
//! loss handling — but the pictures go from the drone's datagrams straight
//! into the agent as access units, with no RTP re-framing, no loopback hop
//! and no daemon depacketizer between them. Built to measure what that
//! difference is worth (DEMO-TELLO.md, "Native host versus daemon").
//!
//!     cargo run -p tello-host --release -- [--drone-ip 192.168.10.1] [--signal-url …]
//!
//! It shares the robot id and key file with the daemon version, so the two
//! run alternately against the same enrolment, never together (the drone
//! accepts one controller, and both bind the same local ports).
//!
//! Not part of Seyd: a customer-style program under examples/.

mod tello;
mod video;

use bytes::Bytes;
use clap::Parser;
use seyd_core::{Agent, AgentConfig, AgentEvent, ChannelKind, ChannelSpec, VideoFrame};
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::net::UdpSocket;

/// Encoder level → bitrate, measured on the drone (2026-10-02).
const ENCODER_LEVELS_KBPS: [(u8, u32); 5] = [(1, 1000), (2, 1500), (3, 2000), (4, 3000), (5, 4000)];
const STICK_HZ: u64 = 50;
const CONTROL_TIMEOUT: Duration = Duration::from_secs(3);
const VIDEO_TIMEOUT: Duration = Duration::from_secs(1);
const KF_MIN_INTERVAL: Duration = Duration::from_millis(250);
const LINK_UP_HOLD_MIN: Duration = Duration::from_secs(15);
const LINK_UP_HOLD_MAX: Duration = Duration::from_secs(120);

#[derive(Parser, Debug, Clone)]
#[command(about = "Tello drone as a Seyd robot, hosting seyd-core directly")]
struct Args {
    #[arg(long, default_value = "192.168.10.1", env = "TELLO_IP")]
    drone_ip: String,
    #[arg(
        long,
        default_value = "wss://seyd-signal-flj7s44j4a-ew.a.run.app/ws",
        env = "SIGNAL_URL"
    )]
    signal_url: String,
    #[arg(long, default_value = "seyd-tello", env = "ROBOT_ID")]
    robot_id: String,
    #[arg(long, default_value = "examples/tello-robot/.robot.key")]
    credential: PathBuf,
    #[arg(long, default_value = "latency", env = "DARC_QOS_PROFILE")]
    qos_profile: String,
    #[arg(long, default_value_t = 5, env = "TELLO_ALT_LIMIT_M")]
    alt_limit_m: u8,
    #[arg(long, default_value_t = 4)]
    encoder_rate: u8,
    #[arg(long, default_value_t = 400)]
    hold_ms: u64,
    #[arg(long, default_value_t = 1500)]
    max_command_age_ms: u64,
    #[arg(long, default_value_t = 5.0)]
    orphan_land_s: f64,
    #[arg(long, default_value_t = 1.0)]
    stick_scale: f32,
    /// Refuse take-off (bench work with the propellers off).
    #[arg(long)]
    no_takeoff: bool,
    /// Torn pictures in 5 s that step the encoder level down (0 disables link adaptation).
    #[arg(long, default_value_t = 10)]
    link_down_tears: usize,
    /// Torn pictures in 15 s at or below which the level steps back up.
    #[arg(long, default_value_t = 3)]
    link_up_tears: usize,
}

#[derive(Default)]
struct Stats {
    commands: u64,
    stale: u64,
    takeoffs: u64,
    landings: u64,
    keyframe_requests: u64,
    rate_changes: u64,
    orphan_landings: u64,
    safety_net_keyframes: u64,
    link_steps_down: u64,
    link_steps_up: u64,
    tx: u64,
    rx: u64,
    bad_crc: u64,
    video_bytes: u64,
    reconnects: u64,
}

/// Everything the loops share. One mutex, held for microseconds at a time.
struct State {
    connected: bool,
    video_enabled: bool,
    last_control_rx: Option<Instant>,
    last_video_rx: Option<Instant>,
    seq: u16,
    sticks: [f32; 4],
    sticks_until: Option<Instant>,
    flight: tello::FlightData,
    logs: tello::LogState,
    wifi_strength: u8,
    wifi_disturb: u8,
    encoder_rate: u8,
    /// What Seyd asked for (video-config) and what the drone's own link
    /// allows; the drone gets min(requested, link). Seyd cannot see the
    /// drone's 2.4 GHz link (loss is measured on the pilot leg, ADR 0006;
    /// PLAN.md item 24 is the proper fix), so the host steps the level down
    /// when pictures tear often — fewer datagrams per picture, keyframes
    /// that survive — and back up when the link is quiet.
    requested_level: u8,
    link_level: u8,
    link_changed_at: Instant,
    /// Quiet time required before the next step up. Doubles each time a
    /// step up is reversed within 20 s (the link only *looked* quiet at the
    /// lower level), up to 2 min; resets once a step up survives a minute.
    /// Without this the first range flight stepped up and down every 20 s.
    link_up_hold: Duration,
    last_step_up_at: Option<Instant>,
    tears: VecDeque<Instant>,
    max_gop_ms: u64,
    last_kf_req: Option<Instant>,
    last_command_at: Option<Instant>,
    sessions: usize,
    notice: Option<(String, Instant)>,
    pending_rate: Option<u8>,
    last_rate_at: Option<Instant>,
    stats: Stats,
    /// Assembly span per picture (first→last datagram) and the hop to the
    /// agent (last datagram→push), rolling, for the daemon comparison.
    assembly_ms: VecDeque<f64>,
    push_ms: VecDeque<f64>,
    fps_window: (u64, u64, Instant),
    fps: f64,
    kbps: u64,
}

struct Drone {
    ctl: Arc<UdpSocket>,
    addr: SocketAddr,
    st: Arc<Mutex<State>>,
}

impl Drone {
    fn send(&self, cmd: u16, pkt_type: u8, payload: &[u8]) {
        let seq = {
            let mut s = self.st.lock().unwrap();
            s.seq = s.seq.wrapping_add(1);
            s.stats.tx += 1;
            s.seq
        };
        let pkt = tello::build_packet(cmd, pkt_type, payload, seq);
        let _ = self.ctl.try_send_to(&pkt, self.addr);
    }
    fn start_video(&self) {
        self.st.lock().unwrap().video_enabled = true;
        self.send(tello::VIDEO_START_CMD, tello::PT_DATA2, &[]);
    }
    fn video_setup(&self, encoder_rate: u8) {
        self.send(tello::VIDEO_MODE_CMD, tello::PT_SET, &[0]);
        self.send(tello::EXPOSURE_CMD, tello::PT_GET, &[0]);
        self.send(
            tello::VIDEO_ENCODER_RATE_CMD,
            tello::PT_SET,
            &[encoder_rate],
        );
        self.start_video();
    }
    fn send_time(&self) {
        let mut p = vec![0u8];
        p.extend_from_slice(&tello::time_payload_now());
        self.send(tello::TIME_CMD, tello::PT_DATA1, &p);
    }
    fn takeoff(&self, alt_limit_m: u8) {
        self.send(
            tello::SET_ALT_LIMIT_CMD,
            tello::PT_SET,
            &[alt_limit_m.clamp(1, 30), 0],
        );
        self.send(tello::TAKEOFF_CMD, tello::PT_SET, &[]);
        tracing::info!(alt_limit_m, "takeoff");
    }
    fn land(&self) {
        {
            let mut s = self.st.lock().unwrap();
            s.sticks = [0.0; 4];
            s.sticks_until = None;
        }
        self.send(tello::LAND_CMD, tello::PT_SET, &[0]);
        tracing::info!("land");
    }
    /// A keyframe request, rate-limited like the daemon's own (one per 250 ms).
    fn request_keyframe(&self, reason: &str) {
        {
            let mut s = self.st.lock().unwrap();
            let now = Instant::now();
            if s.last_kf_req.is_some_and(|t| now - t < KF_MIN_INTERVAL) {
                return;
            }
            s.last_kf_req = Some(now);
            s.stats.keyframe_requests += 1;
        }
        tracing::debug!(reason, "keyframe request");
        self.start_video();
    }
    fn notice(&self, text: String) {
        tracing::warn!("{text}");
        self.st.lock().unwrap().notice = Some((text, Instant::now() + Duration::from_secs(8)));
    }
}

fn level_for_kbps(kbps: f64) -> u8 {
    ENCODER_LEVELS_KBPS
        .iter()
        .filter(|(_, r)| (*r as f64) <= kbps)
        .map(|(l, _)| *l)
        .max()
        .unwrap_or(1)
}

fn percentile(v: &VecDeque<f64>, p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s: Vec<f64> = v.iter().copied().collect();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    s[((s.len() - 1) as f64 * p).round() as usize]
}

fn push_sample(v: &mut VecDeque<f64>, x: f64) {
    v.push_back(x);
    if v.len() > 300 {
        v.pop_front();
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse()?),
        )
        .init();
    let args = Args::parse();

    let channels = vec![
        ChannelSpec {
            id: 1,
            kind: ChannelKind::Video,
            name: "main".into(),
            codec: "avc1.4d4028".into(), // Main profile level 4.0, read from the drone's SPS
            fps: 30,
            layers: Vec::new(),
            max_bitrate_kbps: 4000, // the drone's level 5 (ADR 0011)
        },
        ChannelSpec {
            id: 2,
            kind: ChannelKind::Sensor,
            name: "telemetry".into(),
            codec: "json".into(),
            fps: 0,
            layers: Vec::new(),
            max_bitrate_kbps: 0,
        },
        ChannelSpec {
            id: 3,
            kind: ChannelKind::Command,
            name: "flight".into(),
            codec: "json".into(),
            fps: 0,
            layers: Vec::new(),
            max_bitrate_kbps: 0,
        },
    ];
    let (agent, mut events) = Agent::start(AgentConfig {
        robot_id: args.robot_id.clone(),
        signal_url: args.signal_url.clone(),
        credential_path: args.credential.clone(),
        qos_profile: args.qos_profile.clone(),
        channels,
        agent_version: format!("tello-host/{}", env!("CARGO_PKG_VERSION")),
        ..AgentConfig::default()
    })
    .await?;
    tracing::info!(robot = args.robot_id, signal = args.signal_url, "agent up");

    let ctl = Arc::new(UdpSocket::bind("0.0.0.0:9000").await?);
    let vid = Arc::new(UdpSocket::bind("0.0.0.0:6038").await?);
    let addr: SocketAddr = format!("{}:8889", args.drone_ip).parse()?;
    let st = Arc::new(Mutex::new(State {
        connected: false,
        video_enabled: false,
        last_control_rx: None,
        last_video_rx: None,
        seq: 0,
        sticks: [0.0; 4],
        sticks_until: None,
        flight: tello::FlightData::default(),
        logs: tello::LogState::default(),
        wifi_strength: 0,
        wifi_disturb: 0,
        encoder_rate: args.encoder_rate,
        requested_level: args.encoder_rate.max(1),
        link_level: 5,
        link_changed_at: Instant::now(),
        link_up_hold: LINK_UP_HOLD_MIN,
        last_step_up_at: None,
        tears: VecDeque::new(),
        max_gop_ms: 10_000,
        last_kf_req: None,
        last_command_at: None,
        sessions: 0,
        notice: None,
        pending_rate: None,
        last_rate_at: None,
        stats: Stats::default(),
        assembly_ms: VecDeque::new(),
        push_ms: VecDeque::new(),
        fps_window: (0, 0, Instant::now()),
        fps: 0.0,
        kbps: 0,
    }));
    let drone = Arc::new(Drone {
        ctl: ctl.clone(),
        addr,
        st: st.clone(),
    });
    let relay = Arc::new(Mutex::new(video::Relay::new()));
    let assembler = Arc::new(Mutex::new(video::FrameAssembler::new()));

    // The initial video-config the daemon would have sent: level from the profile's ceiling.
    {
        let cfg = agent.publisher_config(1, "profile");
        let mut s = st.lock().unwrap();
        if let Some(k) = cfg["maxBitrateKbps"].as_f64() {
            s.requested_level = level_for_kbps(k);
            s.encoder_rate = s.requested_level.min(s.link_level);
        }
        if let Some(g) = cfg["maxGopMs"].as_u64() {
            s.max_gop_ms = g;
        }
    }

    // ── control receive ──────────────────────────────────────────────────
    {
        let (ctl, drone, st) = (ctl.clone(), drone.clone(), st.clone());
        tokio::spawn(async move {
            let mut buf = vec![0u8; 2048];
            loop {
                let Ok((n, from)) = ctl.recv_from(&mut buf).await else {
                    continue;
                };
                let data = &buf[..n];
                let now = Instant::now();
                let mut s = st.lock().unwrap();
                s.stats.rx += 1;
                s.last_control_rx = Some(now);
                if data.starts_with(b"conn_ack:") {
                    let was = s.connected;
                    s.connected = true;
                    if !was {
                        s.stats.reconnects += 1;
                        let rate = s.encoder_rate;
                        drop(s);
                        tracing::info!(%from, "connected (conn_ack)");
                        drone.send_time();
                        drone.video_setup(rate);
                    }
                    continue;
                }
                let Some(p) = tello::parse_packet(data) else {
                    if data[0] == tello::START_OF_PACKET {
                        s.stats.bad_crc += 1;
                    }
                    continue;
                };
                match p.cmd {
                    tello::FLIGHT_MSG => s.flight = tello::FlightData::parse(p.payload),
                    tello::WIFI_MSG => {
                        if p.payload.len() >= 2 {
                            s.wifi_strength = p.payload[0];
                            s.wifi_disturb = p.payload[1];
                        }
                    }
                    tello::LOG_HEADER_MSG => {
                        if p.payload.len() >= 2 {
                            let ack = [0, p.payload[0], p.payload[1]];
                            drop(s);
                            drone.send(tello::LOG_HEADER_MSG, tello::PT_DATA1, &ack);
                        }
                    }
                    tello::LOG_DATA_MSG => {
                        if !p.payload.is_empty() {
                            s.logs.update(&p.payload[1..]);
                        }
                    }
                    tello::TIME_CMD => {
                        drop(s);
                        drone.send_time();
                    }
                    cmd => tracing::trace!(cmd, seq = p.seq, "ack or unhandled"),
                }
            }
        });
    }

    // ── video receive → agent ────────────────────────────────────────────
    {
        let (vid, drone, st, agent, relay, assembler) = (
            vid.clone(),
            drone.clone(),
            st.clone(),
            agent.clone(),
            relay.clone(),
            assembler.clone(),
        );
        let t0 = Instant::now();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            loop {
                let Ok(n) = vid.recv(&mut buf).await else {
                    continue;
                };
                let now = Instant::now();
                let outcome = {
                    let mut s = st.lock().unwrap();
                    s.last_video_rx = Some(now);
                    s.stats.video_bytes += n as u64;
                    assembler.lock().unwrap().push(&buf[..n], now)
                };
                match outcome {
                    video::Assembled::Nothing => {}
                    video::Assembled::Lost => {
                        st.lock().unwrap().tears.push_back(now);
                        if relay.lock().unwrap().mark_loss(now) {
                            drone.request_keyframe("torn picture");
                        }
                    }
                    video::Assembled::Picture(pic) => {
                        let (verdict, request) = relay.lock().unwrap().push_picture(&pic.data, now);
                        if request {
                            drone.request_keyframe("relay");
                        }
                        if let video::Verdict::Send(out) = verdict {
                            // Capture clock: arrival of the picture's first datagram,
                            // relative to start — the same choice as the daemon path
                            // (the drone stamps nothing).
                            let capture_ts_us = (pic.first_at - t0).as_micros() as u64;
                            let keyframe = out.keyframe;
                            let bytes = out.data.len() as u64;
                            agent.push_video(
                                1,
                                VideoFrame {
                                    data: Bytes::from(out.data),
                                    keyframe,
                                    capture_ts_us,
                                },
                            );
                            let pushed = Instant::now();
                            let mut s = st.lock().unwrap();
                            push_sample(
                                &mut s.assembly_ms,
                                (pic.last_at - pic.first_at).as_secs_f64() * 1000.0,
                            );
                            push_sample(
                                &mut s.push_ms,
                                (pushed - pic.last_at).as_secs_f64() * 1000.0,
                            );
                            s.fps_window.0 += 1;
                            s.fps_window.1 += bytes;
                        }
                    }
                }
            }
        });
    }

    // ── sticks at 50 Hz ──────────────────────────────────────────────────
    {
        let (drone, st) = (drone.clone(), st.clone());
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(1000 / STICK_HZ));
            loop {
                tick.tick().await;
                let sticks = {
                    let mut s = st.lock().unwrap();
                    if !s.connected {
                        continue;
                    }
                    if s.sticks_until.is_some_and(|u| Instant::now() > u) {
                        if s.sticks.iter().any(|&v| v != 0.0) {
                            tracing::info!("stick hold expired — centring");
                        }
                        s.sticks = [0.0; 4];
                        s.sticks_until = None;
                    }
                    s.sticks
                };
                let mut p = tello::stick_payload(sticks[0], sticks[1], sticks[2], sticks[3], false)
                    .to_vec();
                p.extend_from_slice(&tello::time_payload_now());
                drone.send(tello::STICK_CMD, tello::PT_DATA2, &p);
            }
        });
    }

    // ── connect / watchdog, once a second ────────────────────────────────
    {
        let (ctl, drone, st) = (ctl.clone(), drone.clone(), st.clone());
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            loop {
                tick.tick().await;
                let now = Instant::now();
                let (connected, video_enabled, lost, video_stale) = {
                    let mut s = st.lock().unwrap();
                    let lost = s.connected
                        && s.last_control_rx
                            .map_or(true, |t| now - t > CONTROL_TIMEOUT);
                    if lost {
                        s.connected = false;
                        s.sticks = [0.0; 4];
                        s.sticks_until = None;
                    }
                    let stale = s.video_enabled
                        && s.last_video_rx.map_or(true, |t| now - t > VIDEO_TIMEOUT);
                    (s.connected, s.video_enabled, lost, stale)
                };
                if lost {
                    tracing::warn!("control link timed out");
                }
                if !connected {
                    let _ = ctl.send_to(&tello::conn_req(6038), addr).await;
                } else if video_enabled && video_stale {
                    drone.start_video();
                }
            }
        });
    }

    // ── telemetry at 10 Hz, orphan landing, keyframe safety net, stats ──
    {
        let (drone, st, agent, relay, assembler, args) = (
            drone.clone(),
            st.clone(),
            agent.clone(),
            relay.clone(),
            assembler.clone(),
            args.clone(),
        );
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(100));
            let mut last_log = Instant::now();
            loop {
                tick.tick().await;
                let now = Instant::now();
                let (orphan, safety, msg) = {
                    let mut s = st.lock().unwrap();
                    let f = s.flight;
                    let orphan = f.flying()
                        && s.sessions > 0
                        && s.last_command_at
                            .is_some_and(|t| (now - t).as_secs_f64() > args.orphan_land_s);
                    if orphan {
                        s.last_command_at = None;
                        s.stats.orphan_landings += 1;
                    }
                    let r = relay.lock().unwrap();
                    let safety = s.connected
                        && r.last_idr_at
                            .is_some_and(|t| (now - t).as_millis() as u64 > s.max_gop_ms)
                        && s.last_video_rx
                            .is_some_and(|t| now - t < Duration::from_secs(1));
                    if safety {
                        s.stats.safety_net_keyframes += 1;
                    }
                    // the drone's own link: step the encoder level with the tear rate
                    while s
                        .tears
                        .front()
                        .is_some_and(|t| now - *t > Duration::from_secs(15))
                    {
                        s.tears.pop_front();
                    }
                    if args.link_down_tears > 0 && now - s.link_changed_at >= Duration::from_secs(5)
                    {
                        let recent5 = s
                            .tears
                            .iter()
                            .filter(|t| now - **t <= Duration::from_secs(5))
                            .count();
                        let want = s.requested_level.min(s.link_level).max(1);
                        if recent5 >= args.link_down_tears && want > 1 {
                            // A step up reversed quickly means the link was only quiet
                            // because the level was low: probe less often next time.
                            if s.last_step_up_at
                                .is_some_and(|t| now - t < Duration::from_secs(20))
                            {
                                s.link_up_hold = (s.link_up_hold * 2).min(LINK_UP_HOLD_MAX);
                            }
                            s.last_step_up_at = None; // that step up did not survive; only a surviving one resets the hold
                            s.link_level = want - 1;
                            s.link_changed_at = now;
                            s.stats.link_steps_down += 1;
                            s.pending_rate = Some(s.requested_level.min(s.link_level));
                            tracing::warn!(
                                torn_in_5s = recent5,
                                cap = s.link_level,
                                next_up_hold_s = s.link_up_hold.as_secs(),
                                "drone link: encoder level cap stepped down"
                            );
                        } else if s.tears.len() <= args.link_up_tears
                            && s.link_level < s.requested_level
                            && now - s.link_changed_at >= s.link_up_hold
                        {
                            s.link_level += 1;
                            s.link_changed_at = now;
                            s.last_step_up_at = Some(now);
                            s.stats.link_steps_up += 1;
                            s.pending_rate = Some(s.requested_level.min(s.link_level));
                            tracing::info!(
                                torn_in_15s = s.tears.len(),
                                cap = s.link_level,
                                "drone link quiet: encoder level cap stepped up"
                            );
                        } else if s
                            .last_step_up_at
                            .is_some_and(|t| now - t >= Duration::from_secs(60))
                        {
                            s.last_step_up_at = None;
                            s.link_up_hold = LINK_UP_HOLD_MIN; // the step up held: back to normal probing
                        }
                    }
                    if now - s.fps_window.2 >= Duration::from_secs(1) {
                        let dt = (now - s.fps_window.2).as_secs_f64();
                        s.fps = (s.fps_window.0 as f64 / dt * 10.0).round() / 10.0;
                        s.kbps = (s.fps_window.1 as f64 * 8.0 / dt / 1000.0) as u64;
                        s.fps_window = (0, 0, now);
                    }
                    let (yaw, pitch, roll) = s.logs.euler_deg();
                    let a = assembler.lock().unwrap();
                    let mut msg = serde_json::json!({
                        "host": "rust",
                        "drone": if s.connected { "connected" } else { "disconnected" },
                        "flying": f.flying(),
                        "battery": f.battery_percentage,
                        "battery_low": f.battery_low || f.battery_lower,
                        "height_m": f.height as f64 / 10.0,
                        "speed_mps": f.ground_speed as f64 / 10.0,
                        "speed_ne_mps": [f.north_speed as f64 / 10.0, f.east_speed as f64 / 10.0],
                        "vel_mps": s.logs.vel,
                        "pos_m": s.logs.pos,
                        "yaw_deg": yaw.round(), "pitch_deg": pitch.round(), "roll_deg": roll.round(),
                        "fly_time_s": f.fly_time as f64 / 10.0,
                        "fly_mode": f.fly_mode,
                        "wind": f.wind_state, "imu_ok": f.imu_state, "hot": f.temperature_height,
                        "wifi": s.wifi_strength, "wifi_disturb": s.wifi_disturb,
                        "video": {"fps": s.fps, "kbps": s.kbps, "level": s.encoder_rate,
                                  "level_requested": s.requested_level, "level_link_cap": s.link_level,
                                  "tears_5s": s.tears.iter().filter(|t| now - **t <= Duration::from_secs(5)).count(),
                                  "lost_frames": a.stats.lost_frames, "idr": r.stats.idr, "codec": r.codec,
                                  "assembly_ms_p50": percentile(&s.assembly_ms, 0.5),
                                  "assembly_ms_p95": percentile(&s.assembly_ms, 0.95),
                                  "push_ms_p95": percentile(&s.push_ms, 0.95)},
                    });
                    if let Some((text, until)) = &s.notice {
                        if now < *until {
                            msg["notice"] = serde_json::json!(text);
                        }
                    }
                    (orphan, safety, msg)
                };
                if orphan {
                    drone.notice(format!(
                        "no pilot presence for {:.0}s while airborne — landing",
                        args.orphan_land_s
                    ));
                    drone.land();
                }
                if safety {
                    drone.request_keyframe("safety net");
                }
                agent.push_message(2, msg.to_string().as_bytes());
                if now - last_log >= Duration::from_secs(30) {
                    last_log = now;
                    let s = st.lock().unwrap();
                    let r = relay.lock().unwrap();
                    let a = assembler.lock().unwrap();
                    tracing::info!(
                        "stats commands={} stale={} takeoffs={} landings={} keyframe_requests={} rate_changes={} orphan_landings={} safety_net_keyframes={} link_steps_down={} link_steps_up={} relay={:?} tx={} rx={} bad_crc={} video_bytes={} reconnects={} frames={} lost={} assembly_ms p50={:.1} p95={:.1} push_ms p95={:.2}",
                        s.stats.commands, s.stats.stale, s.stats.takeoffs, s.stats.landings, s.stats.keyframe_requests, s.stats.rate_changes,
                        s.stats.orphan_landings, s.stats.safety_net_keyframes, s.stats.link_steps_down, s.stats.link_steps_up, r.stats, s.stats.tx, s.stats.rx, s.stats.bad_crc, s.stats.video_bytes,
                        s.stats.reconnects, a.stats.frames, a.stats.lost_frames,
                        percentile(&s.assembly_ms, 0.5), percentile(&s.assembly_ms, 0.95), percentile(&s.push_ms, 0.95)
                    );
                }
            }
        });
    }

    // ── encoder level changes, at most one per 2 s, latest wins ─────────
    {
        let (drone, st) = (drone.clone(), st.clone());
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(200));
            loop {
                tick.tick().await;
                let apply = {
                    let mut s = st.lock().unwrap();
                    match s.pending_rate {
                        Some(level)
                            if s.last_rate_at
                                .map_or(true, |t| t.elapsed() >= Duration::from_secs(2)) =>
                        {
                            s.pending_rate = None;
                            s.last_rate_at = Some(Instant::now());
                            if level != s.encoder_rate {
                                s.encoder_rate = level;
                                s.stats.rate_changes += 1;
                                Some(level)
                            } else {
                                None
                            }
                        }
                        _ => None,
                    }
                };
                if let Some(level) = apply {
                    tracing::info!(level, "encoder level");
                    drone.send(tello::VIDEO_ENCODER_RATE_CMD, tello::PT_SET, &[level]);
                }
            }
        });
    }

    // ── the agent's events: the robot's own policy ───────────────────────
    loop {
        tokio::select! {
            ev = events.recv() => {
                let Some(ev) = ev else { break };
                match ev {
                    AgentEvent::Command { channel: 3, payload } => {
                        let Ok(m) = serde_json::from_slice::<serde_json::Value>(&payload) else { continue };
                        let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as f64).unwrap_or(0.0);
                        if let Some(ts) = m["ts"].as_f64() {
                            if (now_ms - ts).abs() > args.max_command_age_ms as f64 {
                                st.lock().unwrap().stats.stale += 1;
                                continue;
                            }
                        }
                        {
                            let mut s = st.lock().unwrap();
                            s.last_command_at = Some(Instant::now());
                            s.stats.commands += 1;
                        }
                        if m["land"].as_bool() == Some(true) {
                            st.lock().unwrap().stats.landings += 1;
                            drone.land();
                        } else if m["takeoff"].as_bool() == Some(true) {
                            let (flying, battery, connected) = { let s = st.lock().unwrap(); (s.flight.flying(), s.flight.battery_percentage, s.connected) };
                            if args.no_takeoff {
                                drone.notice("take-off refused: disabled on this robot (--no-takeoff)".into());
                            } else if !connected {
                                drone.notice("take-off refused: drone not connected".into());
                            } else if battery > 0 && battery < 15 {
                                drone.notice(format!("take-off refused: battery {battery}% (needs 15%)"));
                            } else if !flying {
                                st.lock().unwrap().stats.takeoffs += 1;
                                drone.takeoff(args.alt_limit_m);
                            }
                        } else if ["roll", "pitch", "throttle", "yaw"].iter().any(|k| m.get(k).is_some()) {
                            let scale = args.stick_scale.clamp(0.05, 1.0) / 100.0;
                            let axis = |k: &str| (m[k].as_f64().unwrap_or(0.0) as f32 * scale).clamp(-1.0, 1.0);
                            let mut s = st.lock().unwrap();
                            s.sticks = [axis("roll"), axis("pitch"), axis("throttle"), axis("yaw")];
                            s.sticks_until = Some(Instant::now() + Duration::from_millis(args.hold_ms));
                        }
                    }
                    AgentEvent::Command { .. } => {}
                    AgentEvent::RecoveryRequest { kind, reason, .. } => drone.request_keyframe(&format!("seydd {kind}/{reason}")),
                    AgentEvent::RequestedConfig(cfg) => {
                        tracing::info!(kbps = cfg["maxBitrateKbps"].as_f64(), gop_ms = cfg["maxGopMs"].as_u64(), reason = cfg["reason"].as_str(), "video-config");
                        let mut s = st.lock().unwrap();
                        if let Some(k) = cfg["maxBitrateKbps"].as_f64() {
                            if k > 0.0 {
                                s.requested_level = level_for_kbps(k);
                                s.pending_rate = Some(s.requested_level.min(s.link_level));
                            }
                        }
                        if let Some(g) = cfg["maxGopMs"].as_u64() {
                            if g > 0 { s.max_gop_ms = g; }
                        }
                    }
                    AgentEvent::SessionStarted { role, path_label, .. } => {
                        let mut s = st.lock().unwrap();
                        s.sessions = agent.session_count();
                        // A new driver's silence starts now, not at the previous driver's last command.
                        s.last_command_at = Some(Instant::now());
                        tracing::info!(?role, path_label, sessions = s.sessions, "session started");
                    }
                    AgentEvent::SessionEnded { reason, .. } => {
                        let (flying, n) = {
                            let mut s = st.lock().unwrap();
                            s.sessions = agent.session_count();
                            s.sticks = [0.0; 4];
                            s.sticks_until = None;
                            (s.flight.flying(), s.sessions)
                        };
                        tracing::info!(reason, sessions = n, "session ended");
                        if n == 0 && flying {
                            drone.notice("last session ended while airborne — landing".into());
                            st.lock().unwrap().stats.orphan_landings += 1;
                            drone.land();
                        }
                    }
                    AgentEvent::SignalDenied { reason } => tracing::error!(reason, "signal denied"),
                    other => tracing::debug!(?other, "agent event"),
                }
            }
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    if st.lock().unwrap().flight.flying() {
        drone.land();
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    agent.stop();
    Ok(())
}
