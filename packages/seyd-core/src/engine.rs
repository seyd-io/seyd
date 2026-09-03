//! Session engine: binds the packer and control vocabulary to
//! `seyd-transport` sessions. Owns nothing network-discovery related and
//! nothing vendor related; the host (seydd, or an SDK wrapper) feeds frames
//! and messages in and consumes `Event`s out.

use crate::channels::{ChannelKind, ChannelSpec};
use crate::control::{encode_line, FromPilot, ToPilot};
use crate::packer::{self, FrameParams};
use bytes::Bytes;
use seyd_qos::abr::{AbrController, Sample as AbrSample};
use seyd_qos::Profile;
use seyd_transport::{Endpoint, SendError, Session};
use seyd_wire::v2::{ChunkHeader, FrameMeta};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Notify};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Driver,
    Observer,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Driver => "driver",
            Role::Observer => "observer",
        }
    }
}

/// What the engine tells its host.
#[derive(Debug, Clone)]
pub enum Event {
    SessionStarted {
        signal_id: String,
        role: Role,
        path_label: String,
    },
    SessionEnded {
        signal_id: String,
        reason: String,
    },
    /// A driver sent a message on a command channel.
    Command {
        channel: u8,
        payload: Bytes,
    },
    /// Seyd's request to the publisher (`video-config`).
    RequestedConfig(serde_json::Value),
    /// A pilot needs a recovery point on this channel (`recovery-request`).
    RecoveryRequest {
        channel: u8,
        kind: &'static str,
        reason: &'static str,
    },
}

/// One encoded picture from the host.
pub struct VideoFrame {
    pub data: Bytes,
    pub keyframe: bool,
    pub capture_ts_us: u64,
}

#[derive(Clone)]
pub struct EngineConfig {
    pub channels: Vec<ChannelSpec>,
    pub profile: &'static Profile,
    pub max_sessions: usize,
    pub chunk_len: u16,
}

#[derive(Default, Debug)]
pub struct Counters {
    pub frames_in: AtomicU64,
    pub frames_sent: AtomicU64,
    pub frames_dropped_backlog: AtomicU64,
    pub frames_skipped_stale: AtomicU64,
    pub keyframes_requested: AtomicU64,
    pub chunks_sent: AtomicU64,
    pub parity_sent: AtomicU64,
    pub bytes_sent: AtomicU64,
}

struct SessionState {
    transport: Arc<Session>,
    signal_id: String,
    role: Role,
    lines: mpsc::UnboundedSender<String>,
    /// Set after a delta frame was dropped for this session; every following
    /// delta is useless to its decoder until a keyframe arrives.
    skip_until_key: std::sync::atomic::AtomicBool,
    /// What this pilot last reported, paired with our own counters, for ABR.
    pilot: Mutex<PilotSample>,
}

#[derive(Default)]
struct PilotSample {
    /// (arrival time, agent `chunks_sent`, pilot `chunks_rx`) as the pilot read
    /// them together; loss is measured across a sliding window of them (≥ 3 s),
    /// so a keyframe burst in flight at one boundary cannot read as 10 % loss.
    pairs: VecDeque<(Instant, u64, u64)>,
    loss_pct: Option<f64>,
    /// The pilot's own `chunks_missing`-based estimate, for cross-checking.
    loss_pilot_pct: Option<f64>,
    last_incomplete: Option<(u64, u64)>,
    /// Genuinely incomplete frames (timed-out ones excluded) since last tick.
    incomplete_delta: u64,
    /// Lifetime QUIC lost packets at the last ABR tick (for the delta).
    prev_lost_packets: u64,
}

struct AbrView {
    bitrate_kbps: u32,
    fec: (u32, u32),
    reason: &'static str,
    loss_pct: Option<f64>,
    loss_pilot_pct: Option<f64>,
}

/// ABR state shared between the 1 Hz loop, the sender and the stats task.
struct AbrState {
    controllers: HashMap<u8, AbrController>,
    /// FEC rates in force (delta %, key %) — the profile's until ABR moves them.
    fec: (u32, u32),
    bitrate_kbps: u32,
    reason: &'static str,
    prev_backlog: u64,
    loss_pct: Option<f64>,
    loss_pilot_pct: Option<f64>,
    last_keyframe_sent: Option<Instant>,
}

struct Pending {
    role: Role,
    since: Instant,
}

/// Chunk loss across one window of pilot-paired counters, or `None` when the
/// window is too short or too small to divide meaningfully. Both deltas must
/// come from counters the pilot read at the same instant (see
/// `note_pilot_stats`); differencing counters sampled at different moments
/// measures rate variation, not loss.
///
/// `d_rx` can exceed `d_sent` by the chunks in flight when the pilot sampled,
/// which is why the result is clamped rather than allowed to go negative.
fn windowed_loss_pct(span: Duration, d_sent: u64, d_rx: u64) -> Option<f64> {
    if span < Duration::from_secs(3) || d_sent < 50 {
        return None;
    }
    Some(((1.0 - d_rx as f64 / d_sent as f64) * 100.0).clamp(0.0, 100.0))
}

/// Bounded, in-order frame queue between the input and the sender.
///
/// The prototype used a single slot ("latest frame wins"). That is wrong for
/// H.264: a skipped delta frame breaks the reference chain and every later
/// frame decodes against the wrong picture until the next IDR — measured as
/// 10 % of frames skipped on the demo camera (bursty RTSP delivery), i.e. a
/// visible warp/"replay" every few hundred milliseconds. Frames are therefore
/// sent in order; when the queue is full the *new* delta is dropped and every
/// following delta too, until a keyframe (which clears the queue — an IDR
/// makes queued deltas irrelevant) and a recovery request goes out.
struct FrameQueue {
    frames: VecDeque<(u8, VideoFrame)>,
    skip_until_key: bool,
}

const MAX_QUEUED_FRAMES: usize = 6;

struct Inner {
    cfg: EngineConfig,
    profile: RwLock<&'static Profile>,
    sessions: Mutex<HashMap<u64, Arc<SessionState>>>,
    pending: Mutex<HashMap<String, Pending>>,
    frame_ids: HashMap<u8, AtomicU16>,
    seqs: HashMap<u8, AtomicU16>,
    counters: Counters,
    events: mpsc::Sender<Event>,
    last_recovery: Mutex<HashMap<u8, Instant>>,
    queue: Mutex<FrameQueue>,
    slot_notify: Notify,
    epoch: Instant,
    abr: Mutex<AbrState>,
}

#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

impl Engine {
    pub fn new(cfg: EngineConfig) -> (Engine, mpsc::Receiver<Event>) {
        let (events, rx) = mpsc::channel(256);
        let frame_ids = cfg
            .channels
            .iter()
            .map(|c| (c.id, AtomicU16::new(0)))
            .collect();
        let seqs = cfg
            .channels
            .iter()
            .map(|c| (c.id, AtomicU16::new(0)))
            .collect();
        let profile = cfg.profile;
        let inner = Arc::new(Inner {
            profile: RwLock::new(cfg.profile),
            cfg,
            sessions: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            frame_ids,
            seqs,
            counters: Counters::default(),
            events,
            last_recovery: Mutex::new(HashMap::new()),
            queue: Mutex::new(FrameQueue {
                frames: VecDeque::new(),
                skip_until_key: false,
            }),
            slot_notify: Notify::new(),
            epoch: Instant::now(),
            abr: Mutex::new(AbrState {
                controllers: HashMap::new(),
                fec: (profile.fec_delta_pct, profile.fec_key_pct),
                bitrate_kbps: profile.max_bitrate_kbps,
                reason: "steady",
                prev_backlog: 0,
                loss_pct: None,
                loss_pilot_pct: None,
                last_keyframe_sent: None,
            }),
        });
        let engine = Engine { inner };
        tokio::spawn(engine.clone().frame_sender());
        tokio::spawn(engine.clone().abr_loop());
        (engine, rx)
    }

    pub fn channels(&self) -> &[ChannelSpec] {
        &self.inner.cfg.channels
    }
    pub fn channel_by_name(&self, name: &str) -> Option<&ChannelSpec> {
        self.inner.cfg.channels.iter().find(|c| c.name == name)
    }
    pub fn counters(&self) -> &Counters {
        &self.inner.counters
    }
    pub fn profile(&self) -> &'static Profile {
        *self.inner.profile.read().unwrap()
    }
    pub fn now_us(&self) -> u64 {
        self.inner.epoch.elapsed().as_micros() as u64
    }
    pub fn session_count(&self) -> usize {
        self.inner.sessions.lock().unwrap().len()
    }
    pub fn signal_session_ids(&self) -> Vec<String> {
        self.inner
            .sessions
            .lock()
            .unwrap()
            .values()
            .map(|s| s.signal_id.clone())
            .collect()
    }

    /// The cloud told us a pilot is coming (`pilot-connecting`). Remembered
    /// for 30 s so its `hello` can be matched to a role.
    pub fn expect_pilot(&self, signal_id: &str, role: Role) {
        let mut p = self.inner.pending.lock().unwrap();
        p.retain(|_, v| v.since.elapsed() < Duration::from_secs(30));
        p.insert(
            signal_id.to_string(),
            Pending {
                role,
                since: Instant::now(),
            },
        );
    }

    /// The cloud revoked a session (pilot left / abort / revoke).
    pub fn revoke(&self, signal_id: &str, reason: &str) {
        let victim = self
            .inner
            .sessions
            .lock()
            .unwrap()
            .values()
            .find(|s| s.signal_id == signal_id)
            .cloned();
        if let Some(s) = victim {
            s.transport.close(0, reason);
        }
    }

    /// Change the QoS ceiling live (also reachable by pilots via `set-qos`).
    pub fn set_profile(&self, profile: &'static Profile, reason: &str) {
        *self.inner.profile.write().unwrap() = profile;
        {
            // A new ceiling resets the controller; the profile's own rates
            // apply until the loop measures otherwise.
            let mut abr = self.inner.abr.lock().unwrap();
            abr.controllers.clear();
            abr.fec = (profile.fec_delta_pct, profile.fec_key_pct);
            abr.bitrate_kbps = profile.max_bitrate_kbps;
            abr.reason = "steady";
        }
        for c in self
            .inner
            .cfg
            .channels
            .iter()
            .filter(|c| c.kind == ChannelKind::Video)
        {
            let _ = self.inner.events.try_send(Event::RequestedConfig(
                profile.publisher_config(c.id, reason),
            ));
        }
    }

    /// Offer a new frame. In-order bounded queue (see `FrameQueue`): a
    /// keyframe flushes stale deltas; an overflowing delta is dropped together
    /// with every delta after it until the next keyframe, and a recovery
    /// request is raised so that keyframe arrives in ~1 RTT.
    pub fn push_video(&self, channel: u8, frame: VideoFrame) {
        self.inner
            .counters
            .frames_in
            .fetch_add(1, Ordering::Relaxed);
        let mut q = self.inner.queue.lock().unwrap();
        if frame.keyframe {
            let flushed = q.frames.len();
            q.frames.clear();
            q.skip_until_key = false;
            if flushed > 0 {
                self.inner
                    .counters
                    .frames_skipped_stale
                    .fetch_add(flushed as u64, Ordering::Relaxed);
            }
        } else if q.skip_until_key || q.frames.len() >= MAX_QUEUED_FRAMES {
            let first = !q.skip_until_key;
            q.skip_until_key = true;
            self.inner
                .counters
                .frames_skipped_stale
                .fetch_add(1, Ordering::Relaxed);
            drop(q);
            if first {
                self.request_recovery(channel, "sender-backlog");
            }
            return;
        }
        q.frames.push_back((channel, frame));
        drop(q);
        self.inner.slot_notify.notify_one();
    }

    /// Send one sensor message to every session.
    pub fn push_message(&self, channel: u8, payload: &[u8]) {
        let seq = match self.inner.seqs.get(&channel) {
            Some(s) => s.fetch_add(1, Ordering::Relaxed),
            None => return,
        };
        let dg = packer::pack_message(channel, seq, self.now_us() as u32, payload);
        // Counted once (not per session): the pilot pairs its own datagram
        // count with `chunks_sent` to measure true loss, and it receives
        // sensor datagrams on the same socket as video chunks.
        self.inner
            .counters
            .chunks_sent
            .fetch_add(1, Ordering::Relaxed);
        self.inner
            .counters
            .bytes_sent
            .fetch_add(dg.len() as u64, Ordering::Relaxed);
        for s in self.inner.sessions.lock().unwrap().values() {
            let _ = s.transport.send_datagram(dg.clone());
        }
    }

    /// Accept sessions from the endpoint forever.
    pub async fn accept_loop(self, endpoint: Arc<Endpoint>) {
        while let Some(session) = endpoint.accept().await {
            tokio::spawn(self.clone().run_session(Arc::new(session)));
        }
    }

    async fn frame_sender(self) {
        loop {
            let next = self.inner.queue.lock().unwrap().frames.pop_front();
            match next {
                Some((channel, frame)) => self.send_frame(channel, frame).await,
                None => self.inner.slot_notify.notified().await,
            }
        }
    }

    async fn send_frame(&self, channel: u8, frame: VideoFrame) {
        let sessions: Vec<Arc<SessionState>> = self
            .inner
            .sessions
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        let frame_id = match self.inner.frame_ids.get(&channel) {
            Some(f) => f.fetch_add(1, Ordering::Relaxed),
            None => return,
        };
        if sessions.is_empty() {
            return;
        }
        let profile = self.profile();
        // FEC rates in force: the profile's, or what ABR moved them to.
        let (fec_delta, fec_key) = self.inner.abr.lock().unwrap().fec;
        let fec_pct = if frame.keyframe { fec_key } else { fec_delta };
        let fps = self
            .inner
            .cfg
            .channels
            .iter()
            .find(|c| c.id == channel)
            .map(|c| c.fps)
            .filter(|f| *f > 0)
            .unwrap_or(30);
        let pack = packer::pack_video(
            FrameParams {
                channel_id: channel,
                frame_id,
                keyframe: frame.keyframe,
                fec_pct,
                chunk_len: self.inner.cfg.chunk_len,
                send_ts: self.now_us() as u32,
                meta: Some(FrameMeta {
                    capture_ts_us: frame.capture_ts_us,
                    seq_in_gop: 0,
                }),
            },
            &frame.data,
        );
        let wire_bytes: usize = pack.chunks.iter().map(|c| c.len()).sum();
        let threshold = profile.drop_threshold_bytes(fps);
        let mut sent_any = false;
        tracing::trace!(
            frame_id,
            keyframe = frame.keyframe,
            bytes = frame.data.len(),
            wire_bytes,
            chunks = pack.chunks.len(),
            "frame"
        );

        for s in &sessions {
            // Admission control before the first chunk leaves: whole frame or
            // nothing. A delta frame that would not fit the remaining send
            // buffer — or arrives while the backlog exceeds the profile's
            // byte budget — is skipped cleanly. Keyframes always go.
            let space = s.transport.send_buffer_space();
            if frame.keyframe {
                s.skip_until_key.store(false, Ordering::Relaxed);
            } else if s.skip_until_key.load(Ordering::Relaxed) {
                self.inner
                    .counters
                    .frames_dropped_backlog
                    .fetch_add(1, Ordering::Relaxed);
                continue;
            } else if wire_bytes > space || space < threshold {
                self.inner
                    .counters
                    .frames_dropped_backlog
                    .fetch_add(1, Ordering::Relaxed);
                s.skip_until_key.store(true, Ordering::Relaxed);
                tracing::debug!(
                    frame_id,
                    wire_bytes,
                    space,
                    threshold,
                    "delta frame dropped: backlog; skipping until keyframe"
                );
                self.request_recovery(channel, "backlog");
                continue;
            }
            let mut ok = true;
            for c in &pack.chunks {
                let mut tries = 0;
                loop {
                    match s.transport.send_datagram(c.clone()) {
                        Ok(()) => break,
                        Err(SendError::Blocked) if frame.keyframe && tries < 50 => {
                            tries += 1;
                            tokio::time::sleep(Duration::from_millis(1)).await;
                        }
                        Err(_) => {
                            ok = false;
                            break;
                        }
                    }
                }
                if !ok {
                    break;
                }
            }
            if ok {
                sent_any = true;
            }
        }
        if sent_any {
            let c = &self.inner.counters;
            c.frames_sent.fetch_add(1, Ordering::Relaxed);
            c.chunks_sent
                .fetch_add(pack.chunks.len() as u64, Ordering::Relaxed);
            c.parity_sent
                .fetch_add(pack.parity_chunks as u64, Ordering::Relaxed);
            c.bytes_sent.fetch_add(wire_bytes as u64, Ordering::Relaxed);
            if frame.keyframe {
                self.inner.abr.lock().unwrap().last_keyframe_sent = Some(Instant::now());
            }
        }
    }

    async fn run_session(self, transport: Arc<Session>) {
        let tid = transport.session_id();
        let remote = transport.remote_addr();
        let Ok(Some((mut reader, mut writer))) =
            tokio::time::timeout(Duration::from_secs(5), transport.control()).await
        else {
            tracing::warn!(%remote, "session opened no control stream within 5 s");
            transport.close(1, "no control stream");
            return;
        };
        // The pilot speaks first.
        let Some(first) = seyd_transport::control::read_line(&mut reader).await else {
            transport.close(1, "control stream closed");
            return;
        };
        let (signal_id, _client) = match serde_json::from_str::<FromPilot>(&first) {
            Ok(FromPilot::Hello {
                proto: 2,
                session_id,
                client,
                ..
            }) => (session_id, client),
            Ok(FromPilot::Hello { proto, .. }) => {
                let _ = seyd_transport::control::write_line(
                    &mut writer,
                    &encode_line(&ToPilot::Denied {
                        reason: "unsupported proto",
                    }),
                )
                .await;
                transport.close(1, &format!("proto {proto}"));
                return;
            }
            _ => {
                transport.close(1, "expected hello");
                return;
            }
        };
        let role = self
            .inner
            .pending
            .lock()
            .unwrap()
            .remove(&signal_id)
            .map(|p| p.role)
            .unwrap_or(Role::Observer);
        if self.session_count() >= self.inner.cfg.max_sessions {
            let _ = seyd_transport::control::write_line(
                &mut writer,
                &encode_line(&ToPilot::Denied {
                    reason: "robot-busy",
                }),
            )
            .await;
            transport.close(1, "robot-busy");
            return;
        }

        let (lines_tx, mut lines_rx) = mpsc::unbounded_channel::<String>();
        let state = Arc::new(SessionState {
            transport: transport.clone(),
            signal_id: signal_id.clone(),
            role,
            lines: lines_tx,
            skip_until_key: std::sync::atomic::AtomicBool::new(false),
            pilot: Mutex::new(PilotSample::default()),
        });
        self.inner
            .sessions
            .lock()
            .unwrap()
            .insert(tid, state.clone());
        let path_label = if remote.ip().is_ipv6() {
            "host6"
        } else {
            "host"
        }
        .to_string();
        let _ = state.lines.send(encode_line(&ToPilot::Welcome {
            session_id: &signal_id,
            role: role.as_str(),
            channels: &self.inner.cfg.channels,
            qos: self.profile().pilot_config(),
            t_agent_us: self.now_us(),
        }));
        let _ = self
            .inner
            .events
            .send(Event::SessionStarted {
                signal_id: signal_id.clone(),
                role,
                path_label: path_label.clone(),
            })
            .await;
        tracing::info!(session = %signal_id, %remote, role = role.as_str(), "session started");

        // Writer task: serialises every line to the control stream.
        let writer_task = tokio::spawn(async move {
            while let Some(line) = lines_rx.recv().await {
                if seyd_transport::control::write_line(&mut writer, &line)
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        // Datagram task: command channels from the driver.
        let engine = self.clone();
        let st = state.clone();
        let dg_task = tokio::spawn(async move {
            let Some(mut rx) = st.transport.take_datagrams() else {
                return;
            };
            while let Some(dg) = rx.recv().await {
                let Some((h, payload)) = ChunkHeader::parse(&dg) else {
                    continue;
                };
                let Some(ch) = engine
                    .inner
                    .cfg
                    .channels
                    .iter()
                    .find(|c| c.id == h.channel_id)
                else {
                    continue;
                };
                if ch.kind != ChannelKind::Command || st.role != Role::Driver {
                    continue;
                }
                let _ = engine
                    .inner
                    .events
                    .send(Event::Command {
                        channel: h.channel_id,
                        payload: Bytes::copy_from_slice(payload),
                    })
                    .await;
            }
        });

        // Stats task: 1 Hz agent-stats.
        let engine = self.clone();
        let st = state.clone();
        let stats_task = tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            loop {
                tick.tick().await;
                let c = &engine.inner.counters;
                let p = st.transport.stats();
                let abr = {
                    let a = engine.inner.abr.lock().unwrap();
                    AbrView {
                        bitrate_kbps: a.bitrate_kbps,
                        fec: a.fec,
                        reason: a.reason,
                        loss_pct: a.loss_pct,
                        loss_pilot_pct: a.loss_pilot_pct,
                    }
                };
                let v = serde_json::json!({
                    "frames_in": c.frames_in.load(Ordering::Relaxed),
                    "frames_sent": c.frames_sent.load(Ordering::Relaxed),
                    "frames_dropped_backlog": c.frames_dropped_backlog.load(Ordering::Relaxed),
                    "frames_skipped_stale": c.frames_skipped_stale.load(Ordering::Relaxed),
                    "keyframes_requested": c.keyframes_requested.load(Ordering::Relaxed),
                    "chunks_sent": c.chunks_sent.load(Ordering::Relaxed),
                    "parity_sent": c.parity_sent.load(Ordering::Relaxed),
                    "bytes_sent": c.bytes_sent.load(Ordering::Relaxed),
                    "rtt_ms": p.rtt_ms, "min_rtt_ms": p.min_rtt_ms, "cwnd": p.cwnd,
                    "delivery_kbps": p.delivery_kbps, "lost_packets": p.lost_packets, "mtu": p.current_mtu,
                    "abr_bitrate_kbps": abr.bitrate_kbps, "abr_ceiling_kbps": engine.profile().max_bitrate_kbps,
                    "abr_fec_delta": abr.fec.0, "abr_fec_key": abr.fec.1, "abr_reason": abr.reason,
                    "abr_loss_pct": abr.loss_pct.map(|x| (x * 10.0).round() / 10.0),
                    "abr_loss_pilot_pct": abr.loss_pilot_pct.map(|x| (x * 10.0).round() / 10.0),
                });
                if st.lines.send(encode_line(&ToPilot::AgentStats(v))).is_err() {
                    break;
                }
            }
        });

        // Control reader (this task).
        let mut reason = "pilot-closed".to_string();
        loop {
            tokio::select! {
                line = seyd_transport::control::read_line(&mut reader) => {
                    let Some(line) = line else { break };
                    match serde_json::from_str::<FromPilot>(&line) {
                        Ok(FromPilot::Ping { t1 }) => {
                            let _ = state.lines.send(encode_line(&ToPilot::Pong { t1, t2: self.now_us() }));
                        }
                        Ok(FromPilot::Loss { ch, .. }) => self.request_recovery(ch, "pilot-loss"),
                        Ok(FromPilot::RequestKeyframe { ch }) => self.request_recovery(ch, "pilot-request"),
                        Ok(FromPilot::SetQos { profile }) => {
                            match seyd_qos::get(&profile) {
                                Some(p) => {
                                    self.set_profile(p, "pilot-request");
                                    let _ = state.lines.send(encode_line(&ToPilot::QosAck { profile: p.name, qos: p.pilot_config(), publisher: "requested" }));
                                }
                                None => tracing::debug!(%profile, "unknown qos profile"),
                            }
                        }
                        Ok(FromPilot::PilotStats(v)) => {
                            tracing::trace!(stats = %v, "pilot-stats");
                            self.note_pilot_stats(&state, &v);
                        }
                        Ok(FromPilot::Bye) => { reason = "bye".into(); break; }
                        Ok(FromPilot::Hello { .. }) => {}
                        Ok(FromPilot::Unknown) => {}
                        Err(e) => tracing::debug!(error = %e, "bad control line"),
                    }
                }
                _ = transport.closed() => { reason = "transport-closed".into(); break; }
            }
        }
        self.inner.sessions.lock().unwrap().remove(&tid);
        transport.close(0, &reason);
        writer_task.abort();
        dg_task.abort();
        stats_task.abort();
        tracing::info!(session = %signal_id, %reason, "session ended");
        let _ = self
            .inner
            .events
            .send(Event::SessionEnded { signal_id, reason })
            .await;
    }

    /// Pair the pilot's counters with ours: `chunks_sent` vs `chunks_rx`
    /// between two reports is the true chunk loss over that window (a frame
    /// whose chunks were *all* lost is invisible to the pilot alone).
    ///
    /// Both halves of the pair are read by the *pilot*, at the instant our
    /// `agent-stats` reached it (`chunks_sent_seen` / `chunks_rx_seen`).
    /// Aligning them on one clock is the whole point. This used to difference
    /// our own send log as of (now − rtt − 100 ms) against a `chunks_rx` taken
    /// at a different moment, so the two ends of the ratio covered windows
    /// offset by ~100 ms. Whenever the chunk rate varied across that offset —
    /// a keyframe, a pan changing frame sizes — the mismatch surfaced as loss,
    /// and because the result was clamped at zero the negative excursions were
    /// discarded while the positive ones survived. That rectified ordinary
    /// rate variation into a steady 0.25–2.7 % phantom loss, comfortably above
    /// the controller's 0.2 % FEC threshold, and the bitrate flapped
    /// 3000↔2760↔2500 kbps against a link that was losing nothing at all.
    /// Chunks in flight at the sampling instant still bias both endpoints, but
    /// equally, so they cancel in the difference.
    fn note_pilot_stats(&self, st: &SessionState, v: &serde_json::Value) {
        let get = |k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
        let opt = |k: &str| v.get(k).and_then(|x| x.as_u64());
        let rx = get("chunks_rx");
        let missing = get("chunks_missing");
        let incomplete = get("frames_incomplete");
        // Timed-out frames whose chunks then arrived late are jitter, not loss.
        let timed_out = get("frames_timed_out_late");
        let now = Instant::now();
        // A pilot too old to send the pair leaves `loss_pct` at None: the ABR
        // then runs on residual loss and latency alone, which is strictly
        // better than steering on a number we cannot compute correctly.
        let pair = opt("chunks_sent_seen").zip(opt("chunks_rx_seen"));
        let mut ps = st.pilot.lock().unwrap();
        if let Some((sent_seen, rx_seen)) = pair {
            ps.pairs.push_back((now, sent_seen, rx_seen));
            while ps
                .pairs
                .front()
                .is_some_and(|(t, _, _)| now.duration_since(*t) > Duration::from_secs(5))
            {
                ps.pairs.pop_front();
            }
            if let (Some(first), Some(last)) = (ps.pairs.front(), ps.pairs.back()) {
                if let Some(pct) = windowed_loss_pct(
                    last.0.duration_since(first.0),
                    last.1.saturating_sub(first.1),
                    last.2.saturating_sub(first.2),
                ) {
                    ps.loss_pct = Some(pct);
                }
            }
        }
        if rx + missing > 0 {
            ps.loss_pilot_pct = Some(missing as f64 / (rx + missing) as f64 * 100.0);
        }
        if let Some((i0, t0)) = ps.last_incomplete {
            let genuine = incomplete
                .saturating_sub(i0)
                .saturating_sub(timed_out.saturating_sub(t0));
            ps.incomplete_delta += genuine;
        }
        ps.last_incomplete = Some((incomplete, timed_out));
    }

    /// 1 Hz: feed the controller the worst session's view and apply.
    async fn abr_loop(self) {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let sessions: Vec<Arc<SessionState>> = self
                .inner
                .sessions
                .lock()
                .unwrap()
                .values()
                .cloned()
                .collect();
            if sessions.is_empty() {
                continue;
            }
            let profile = self.profile();
            let backlog_total = self
                .inner
                .counters
                .frames_dropped_backlog
                .load(Ordering::Relaxed);
            let mut sample = AbrSample::default();
            let mut worst_inflation = f64::MIN;
            let mut loss_pilot = 0.0f64;
            for st in &sessions {
                let p = st.transport.stats();
                let mut ps = st.pilot.lock().unwrap();
                let lost_delta = p.lost_packets.saturating_sub(ps.prev_lost_packets);
                ps.prev_lost_packets = p.lost_packets;
                let inflation = p.rtt_ms - p.min_rtt_ms;
                if inflation > worst_inflation {
                    worst_inflation = inflation;
                    sample.rtt_ms = p.rtt_ms;
                    sample.min_rtt_ms = p.min_rtt_ms;
                }
                sample.delivery_kbps = sample.delivery_kbps.max(p.delivery_kbps);
                sample.send_kbps = sample.send_kbps.max(p.delivery_kbps);
                sample.lost_packets_delta += lost_delta;
                sample.pilot_frames_incomplete_delta += std::mem::take(&mut ps.incomplete_delta);
                if let Some(l) = ps.loss_pct {
                    sample.pilot_chunk_loss_pct =
                        Some(sample.pilot_chunk_loss_pct.map_or(l, |x: f64| x.max(l)));
                }
                loss_pilot = loss_pilot.max(ps.loss_pilot_pct.unwrap_or(0.0));
            }
            let mut abr = self.inner.abr.lock().unwrap();
            sample.keyframe_just_sent = abr
                .last_keyframe_sent
                .is_some_and(|t| t.elapsed() < Duration::from_millis(1200));
            abr.loss_pct = sample.pilot_chunk_loss_pct;
            abr.loss_pilot_pct = Some(loss_pilot);
            sample.frames_dropped_backlog_delta = backlog_total.saturating_sub(abr.prev_backlog);
            abr.prev_backlog = backlog_total;
            let video_channels: Vec<u8> = self
                .inner
                .cfg
                .channels
                .iter()
                .filter(|c| c.kind == ChannelKind::Video)
                .map(|c| c.id)
                .collect();
            for ch in video_channels {
                let ctl = abr
                    .controllers
                    .entry(ch)
                    .or_insert_with(|| AbrController::new(profile));
                let Some(d) = ctl.step(&sample) else { continue };
                abr.fec = (d.fec_delta_pct, d.fec_key_pct);
                abr.reason = d.reason;
                if d.bitrate_changed {
                    let up = d.max_bitrate_kbps > abr.bitrate_kbps;
                    abr.bitrate_kbps = d.max_bitrate_kbps;
                    let mut cfg =
                        profile.publisher_config(ch, if up { "abr-up" } else { "abr-down" });
                    cfg["maxBitrateKbps"] = serde_json::json!(d.max_bitrate_kbps);
                    cfg["suggestedFps"] = serde_json::json!(d.suggested_fps);
                    let _ = self.inner.events.try_send(Event::RequestedConfig(cfg));
                }
                tracing::info!(
                    channel = ch,
                    bitrate = d.max_bitrate_kbps,
                    fec_delta = d.fec_delta_pct,
                    fec_key = d.fec_key_pct,
                    reason = d.reason,
                    loss = ?sample.pilot_chunk_loss_pct,
                    loss_pilot_est = loss_pilot,
                    residual = sample.pilot_frames_incomplete_delta,
                    kf_recent = sample.keyframe_just_sent,
                    rtt = sample.rtt_ms,
                    min_rtt = sample.min_rtt_ms,
                    "abr"
                );
            }
        }
    }

    fn request_recovery(&self, channel: u8, reason: &'static str) {
        // One request per 250 ms: a lost delta frame smears the picture until
        // a recovery point arrives, so waiting longer costs the operator more
        // than the keyframe costs the link. Spurious requests are avoided at
        // the source (adaptive close-out in the pilot), not by waiting here.
        let min_gap = Duration::from_millis(250);
        let mut last = self.inner.last_recovery.lock().unwrap();
        if let Some(t) = last.get(&channel) {
            if t.elapsed() < min_gap {
                return;
            }
        }
        last.insert(channel, Instant::now());
        self.inner
            .counters
            .keyframes_requested
            .fetch_add(1, Ordering::Relaxed);
        let _ = self.inner.events.try_send(Event::RecoveryRequest {
            channel,
            kind: "idr",
            reason,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: Duration = Duration::from_secs(5);

    #[test]
    fn clean_link_reads_zero() {
        assert_eq!(windowed_loss_pct(W, 1000, 1000), Some(0.0));
    }

    fn near(got: Option<f64>, want: f64) {
        let g = got.expect("expected a measurement");
        assert!((g - want).abs() < 1e-9, "got {g}, want {want}");
    }

    #[test]
    fn loss_is_the_shortfall_against_what_was_sent() {
        near(windowed_loss_pct(W, 1000, 950), 5.0);
        near(windowed_loss_pct(W, 200, 100), 50.0);
    }

    #[test]
    fn in_flight_skew_reads_zero_not_negative() {
        // The pilot can have received a few chunks the agent had not yet
        // counted when it composed agent-stats; that is skew, not gain.
        assert_eq!(windowed_loss_pct(W, 1000, 1008), Some(0.0));
    }

    #[test]
    fn a_window_too_short_or_too_small_yields_nothing() {
        assert_eq!(windowed_loss_pct(Duration::from_secs(2), 1000, 900), None);
        assert_eq!(windowed_loss_pct(W, 49, 0), None);
    }

    /// The regression this pairing exists to prevent. The link is clean — the
    /// pilot received every chunk — but the send rate ramps hard across the
    /// window (a pan enlarging frames, a keyframe at one boundary). Paired on
    /// one clock the deltas match exactly and loss reads 0. The old code
    /// differenced a send count shifted ~100 ms against an unshifted rx count,
    /// so a rate ramp of this shape leaked in as a percent or two of loss —
    /// over the controller's 0.2 % FEC threshold, flapping the bitrate.
    #[test]
    fn a_changing_send_rate_is_not_loss() {
        let mut sent = 0u64;
        let mut rx = 0u64;
        // 5 s of 1 Hz samples, chunk rate climbing 60 → 140 per second.
        let mut pairs = Vec::new();
        for i in 0..6u64 {
            pairs.push((sent, rx));
            let rate = 60 + i * 16;
            sent += rate;
            rx += rate; // nothing is actually lost
        }
        let (s0, r0) = pairs[0];
        let (s1, r1) = *pairs.last().unwrap();
        assert_eq!(windowed_loss_pct(W, s1 - s0, r1 - r0), Some(0.0));
    }
}
