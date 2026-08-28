//! Session engine: binds the packer and control vocabulary to
//! `seyd-transport` sessions. Owns nothing network-discovery related and
//! nothing vendor related; the host (seydd, or an SDK wrapper) feeds frames
//! and messages in and consumes `Event`s out.

use crate::channels::{ChannelKind, ChannelSpec};
use crate::control::{encode_line, FromPilot, ToPilot};
use crate::packer::{self, FrameParams};
use bytes::Bytes;
use seyd_qos::Profile;
use seyd_transport::{Endpoint, SendError, Session};
use seyd_wire::v2::{ChunkHeader, FrameMeta};
use std::collections::HashMap;
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
}

struct Pending {
    role: Role,
    since: Instant,
}

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
    slot: Mutex<Option<(u8, VideoFrame)>>,
    slot_notify: Notify,
    epoch: Instant,
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
            slot: Mutex::new(None),
            slot_notify: Notify::new(),
            epoch: Instant::now(),
        });
        let engine = Engine { inner };
        tokio::spawn(engine.clone().frame_sender());
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

    /// Offer a new frame. Single slot: if the sender has not picked up the
    /// previous frame yet it is overwritten whole — the pilot always gets the
    /// most recent complete picture and latency cannot accumulate.
    pub fn push_video(&self, channel: u8, frame: VideoFrame) {
        self.inner
            .counters
            .frames_in
            .fetch_add(1, Ordering::Relaxed);
        let mut slot = self.inner.slot.lock().unwrap();
        if slot.is_some() {
            self.inner
                .counters
                .frames_skipped_stale
                .fetch_add(1, Ordering::Relaxed);
        }
        *slot = Some((channel, frame));
        drop(slot);
        self.inner.slot_notify.notify_one();
    }

    /// Send one sensor message to every session.
    pub fn push_message(&self, channel: u8, payload: &[u8]) {
        let seq = match self.inner.seqs.get(&channel) {
            Some(s) => s.fetch_add(1, Ordering::Relaxed),
            None => return,
        };
        let dg = packer::pack_message(channel, seq, self.now_us() as u32, payload);
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
            self.inner.slot_notify.notified().await;
            let Some((channel, frame)) = self.inner.slot.lock().unwrap().take() else {
                continue;
            };
            self.send_frame(channel, frame).await;
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
        let fec_pct = if frame.keyframe {
            profile.fec_key_pct
        } else {
            profile.fec_delta_pct
        };
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

        for s in &sessions {
            // Admission control before the first chunk leaves: whole frame or
            // nothing. A delta frame that would not fit the remaining send
            // buffer — or arrives while the backlog exceeds the profile's
            // byte budget — is skipped cleanly. Keyframes always go.
            let space = s.transport.send_buffer_space();
            if !frame.keyframe && (wire_bytes > space || space < threshold) {
                self.inner
                    .counters
                    .frames_dropped_backlog
                    .fetch_add(1, Ordering::Relaxed);
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
                        Ok(FromPilot::Loss { ch, key, .. }) => {
                            if key { self.request_recovery(ch, "pilot-loss"); }
                        }
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
                        Ok(FromPilot::PilotStats(v)) => tracing::trace!(stats = %v, "pilot-stats"),
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

    fn request_recovery(&self, channel: u8, reason: &'static str) {
        let mut last = self.inner.last_recovery.lock().unwrap();
        if let Some(t) = last.get(&channel) {
            if t.elapsed() < Duration::from_millis(250) {
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
