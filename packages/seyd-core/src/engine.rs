//! Session engine: binds the packer and control vocabulary to
//! `seyd-transport` sessions. Owns nothing network-discovery related and
//! nothing vendor related; the host (seydd, or an SDK wrapper) feeds frames
//! and messages in and consumes `Event`s out.

use crate::channels::{ChannelKind, ChannelSpec};
use crate::control::{encode_line, FromPilot, ToPilot};
use crate::packer::{self, FrameParams};
use bytes::Bytes;
use seyd_qos::abr::{AbrController, Sample as AbrSample};
use seyd_qos::simulcast::LayerSelector;
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
    /// The relayed simulcast layer changed (ADR 0008). The host may use it to
    /// force an IDR on the incoming layer so the switch lands sooner; the engine
    /// switches on that layer's next keyframe either way.
    LayerChanged {
        channel: u8,
        layer: u8,
        name: String,
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
    /// Ask for the cheapest useful recovery point (LTR, then intra-refresh,
    /// then IDR) rather than always demanding a keyframe. Set false for a
    /// publisher that mishandles anything but `idr`.
    pub recovery_ladder: bool,
    pub chunk_len: u16,
}

#[derive(Default, Debug)]
pub struct Counters {
    pub frames_in: AtomicU64,
    pub frames_sent: AtomicU64,
    pub frames_dropped_backlog: AtomicU64,
    pub frames_skipped_stale: AtomicU64,
    /// Frames arriving on a simulcast layer we are not relaying. Expected and
    /// large whenever more than one layer is declared — it is the cost of
    /// having the alternative already encoded and ready to switch to.
    pub frames_dropped_inactive_layer: AtomicU64,
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
    /// Epoch-relative millisecond deadline after which delta frames resume
    /// even without a keyframe. 0 while not waiting. See `FrameQueue::resume_due`.
    skip_deadline_ms: std::sync::atomic::AtomicU64,
    /// What this pilot last reported, paired with our own counters, for ABR.
    pilot: Mutex<PilotSample>,
    /// When the last keyframe was queued to this session and its size on the
    /// wire, so admission control can tell that keyframe draining from a
    /// congested link (`keyframe_allowance`).
    last_keyframe: Mutex<Option<(Instant, usize)>>,
    /// Since when this session's backlog has been continuously over the
    /// threshold; `None` while it is under. See `backlog_sustained`.
    backlog_since: Mutex<Option<Instant>>,
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

/// Which simulcast layer a video channel is relaying (ADR 0008).
struct ChannelLayers {
    selector: LayerSelector,
    /// The layer whose frames reach the queue.
    active: u8,
    /// Set when the selector moved; the switch completes on this layer's next
    /// keyframe, so the pilot's decoder never meets a mid-GOP change of SPS.
    pending: Option<u8>,
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
/// makes the queued deltas that lead nowhere irrelevant — see
/// `keyframe_flushes_queue`) and a recovery request goes out.
struct FrameQueue {
    frames: VecDeque<(u8, VideoFrame)>,
    skip_until_key: bool,
    /// When to resume sending deltas even though no keyframe arrived. Set
    /// when recovery was asked for in a form that never produces one.
    resume_at: Option<Instant>,
}

const MAX_QUEUED_FRAMES: usize = 6;

/// Queue depth from which an arriving keyframe discards what is queued. Half
/// capacity: shallow enough that the common case keeps its frames, deep enough
/// that a keyframe still leaves the sender room to recover.
const KEYFRAME_FLUSH_FRAMES: usize = MAX_QUEUED_FRAMES / 2;

/// Whether an arriving keyframe should discard the frames already queued.
///
/// An IDR makes queued deltas unnecessary for *decoding*, which is not the same
/// as worthless. When the queue is contiguous with the keyframe — the ordinary
/// case, the sender simply being a few frames behind — those deltas are the
/// motion between the last frame sent and this recovery point. Dropping them
/// hands the pilot a jump of exactly the kind ADR 0005 exists to remove, and
/// buys almost nothing: the queue drains at link speed, not capture rate, and
/// they are small deltas (~2.3 KB on the wire against a keyframe's 14.6 KB).
///
/// They are genuinely stale in two cases. With `skip_until_key` set the chain
/// is already broken, so a gap sits between them and the keyframe either way
/// and sending them only delays the recovery point the pilot is waiting for.
/// And from `KEYFRAME_FLUSH_FRAMES` up the sender is far enough behind that
/// draining the queue matters more than the motion in it.
impl FrameQueue {
    /// Whether the wait for a recovery point has run out.
    ///
    /// A publisher answering with intra-refresh or an LTR reference never
    /// sends a keyframe, so a latch that only a keyframe can clear would hold
    /// the stream silent for the rest of the session. This is the release
    /// valve, and the grace period is the QoS profile's (`recovery_grace_ms`).
    fn resume_due(&self) -> bool {
        self.resume_at.is_some_and(|t| Instant::now() >= t)
    }
}

fn keyframe_flushes_queue(queued: usize, skip_until_key: bool) -> bool {
    skip_until_key || queued >= KEYFRAME_FLUSH_FRAMES
}

/// Whether the transport backlog for one session has exceeded the profile's
/// byte budget, so the next delta frame should be dropped rather than queued.
///
/// `queued` is what sits in quinn's datagram buffer, not yet packetised
/// (`Session::send_buffer_queued`). The threshold is
/// `Profile::drop_threshold_bytes`: a few frames' worth at the ceiling rate.
///
/// The check used to be `space < threshold` against the buffer's *free* space,
/// which with a 750 KB buffer meant nothing was dropped until ~725 KB — about
/// two seconds of video — had queued up. That hid the latency inside the
/// stack and silenced the ABR's primary congestion signal, which is this drop.
fn backlog_exceeds(queued: usize, threshold: usize, keyframe_allowance: usize) -> bool {
    queued.saturating_sub(keyframe_allowance) > threshold
}

/// How long the backlog must stay over the threshold before a delta is
/// dropped for it.
///
/// This exists because of BBR's ProbeRTT. Every 10 s quinn's controller
/// shrinks the congestion window to 0.75 × its bandwidth-delay estimate for
/// 200 ms (`quinn-proto` `congestion/bbr`, `PROBE_RTT_BASED_ON_BDP`), so the
/// link is throttled to three quarters of the video rate for a fifth of a
/// second and the queue crosses a two-frame threshold every time — on a
/// loopback RTT it does not matter, at 46 ms RTT it did (measured 2026-09-08
/// through a 4.5 Mbps shaped link: 21–28 delta frames dropped once every 10 s,
/// each followed by a 700–900 ms freeze while the sender waited for a
/// keyframe). A backlog that persists longer than the probe is congestion; one
/// that does not is the controller measuring the path, and dropping for it
/// costs the operator a freeze to save 200 ms of queue that drains by itself.
///
/// Not a QoS-profile knob: it tracks the congestion controller's behaviour,
/// not a latency/quality preference, and is the same for every profile.
const BACKLOG_SUSTAIN: Duration = Duration::from_millis(300);

/// Whether a backlog reading should drop the frame: `over` must have held
/// continuously for `sustain`. `since` is the session's record of when the
/// backlog was first seen over the threshold, cleared whenever it is under.
fn backlog_sustained(over: bool, since: &mut Option<Instant>, now: Instant, sustain: Duration) -> bool {
    if !over {
        *since = None;
        return false;
    }
    let start = *since.get_or_insert(now);
    now.duration_since(start) >= sustain
}

/// How much of the queue a recently sent keyframe accounts for.
///
/// A keyframe is several times a delta and is queued in one go, so for the
/// time it takes to drain at the requested bitrate the queue is legitimately
/// deeper than the threshold. Counting that as backlog would drop the delta
/// after every keyframe and ask the publisher for *another* keyframe — an IDR
/// storm on a link that is merely full, not congested. While the keyframe is
/// still plausibly draining (twice its serialisation time at the current
/// bitrate request, to allow for pacing), its size is discounted; after that a
/// deep queue is real congestion and the drop is right.
fn keyframe_allowance(last_keyframe: Option<(Duration, usize)>, bitrate_kbps: u32) -> usize {
    let Some((since, wire_bytes)) = last_keyframe else {
        return 0;
    };
    let drain_ms = wire_bytes as u64 * 8 / u64::from(bitrate_kbps.max(1));
    if since <= Duration::from_millis(drain_ms * 2) {
        wire_bytes
    } else {
        0
    }
}

/// What the publisher is asked to produce to make the stream decodable again,
/// cheapest first.
///
/// Seyd states intent and the robot-side publisher decides how to meet it: a
/// publisher that cannot honour `Ltr` is free to send an IDR instead, and one
/// that ignores `kind` entirely behaves exactly as before. Asking for the
/// cheapest rung matters because an IDR is a bandwidth spike — measured at
/// 30-60 KB against a ~2 KB delta — and on a ~2 Mbps uplink that spike is
/// 120-240 ms of serialisation, which is what field run A recorded as p95.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Recovery {
    /// Re-encode one frame against a long-term reference the pilot still has.
    /// Cheapest: a single ordinary-sized frame restores decodability.
    Ltr,
    /// Refresh the picture progressively across many frames. No single frame
    /// is large, so the bitrate stays flat, but decodability returns gradually.
    IntraRefresh,
    /// A full keyframe. The only rung that lets a decoder start from nothing,
    /// and the only one a pilot's `request-keyframe` can be answered with.
    Idr,
}

impl Recovery {
    fn as_str(self) -> &'static str {
        match self {
            Recovery::Ltr => "ltr",
            Recovery::IntraRefresh => "intra_refresh",
            Recovery::Idr => "idr",
        }
    }

    /// The next rung up, for when the previous request did not restore the
    /// stream. Saturates at `Idr`.
    fn escalate(self) -> Self {
        match self {
            Recovery::Ltr => Recovery::IntraRefresh,
            Recovery::IntraRefresh | Recovery::Idr => Recovery::Idr,
        }
    }

    /// Whether answering this request must produce a keyframe. Only `Idr`
    /// does; the others recover without one, which is why a sender waiting
    /// for a keyframe has to time out rather than wait forever.
    fn yields_keyframe(self) -> bool {
        matches!(self, Recovery::Idr)
    }
}

/// Quiet for this long and the ladder resets to its cheapest rung: an
/// isolated loss should not permanently escalate a link.
const RECOVERY_ESCALATE_WINDOW: Duration = Duration::from_millis(2_000);

#[derive(Clone, Copy)]
struct RecoveryState {
    /// When recovery was last asked for, for the rate limit and the reset.
    last: Instant,
    rung: Recovery,
    /// When the ladder arrived at this rung. A rung is given the profile's
    /// `recovery_grace_ms` to take effect before the ladder climbs — losses
    /// arrive faster than any publisher can answer, and escalating on arrival
    /// rate rather than on elapsed time reaches IDR within two requests and
    /// makes the cheaper rungs unreachable.
    rung_since: Instant,
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
    /// Per channel: the state of the recovery ladder.
    last_recovery: Mutex<HashMap<u8, RecoveryState>>,
    queue: Mutex<FrameQueue>,
    slot_notify: Notify,
    epoch: Instant,
    abr: Mutex<AbrState>,
    /// Video channels that declared more than one layer. Absent for every
    /// single-stream channel, which therefore pays nothing for this.
    simulcast: Mutex<HashMap<u8, ChannelLayers>>,
}

impl Inner {
    /// What the publisher of `channel` declared it can encode at most, in
    /// kbps; 0 = no limit. Channel 0 asks for the tightest limit across all
    /// video channels, for the agent-wide figures.
    fn publisher_max_kbps(&self, channel: u8) -> u32 {
        self.cfg
            .channels
            .iter()
            .filter(|c| c.kind == ChannelKind::Video && (channel == 0 || c.id == channel))
            .map(|c| c.max_bitrate_kbps)
            .filter(|&k| k > 0)
            .min()
            .unwrap_or(0)
    }

    fn ceiling_kbps(&self, profile: &Profile, channel: u8) -> u32 {
        seyd_qos::abr::effective_ceiling_kbps(profile, self.publisher_max_kbps(channel))
    }

    /// The profile's `video-config`, never asking for more than the channel's
    /// publisher can produce.
    fn publisher_config(&self, profile: &Profile, channel: u8, reason: &str) -> serde_json::Value {
        let mut cfg = profile.publisher_config(channel, reason);
        cfg["maxBitrateKbps"] = serde_json::json!(self.ceiling_kbps(profile, channel));
        cfg
    }
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
        // Only channels that actually declared a ladder get an entry, so a
        // single-stream robot pays nothing for simulcast existing.
        let simulcast: HashMap<u8, ChannelLayers> = cfg
            .channels
            .iter()
            .filter(|c| c.kind == ChannelKind::Video && c.layers.len() > 1)
            .filter_map(|c| {
                LayerSelector::new(
                    c.layers.clone(),
                    seyd_qos::abr::effective_ceiling_kbps(profile, c.max_bitrate_kbps),
                )
                .map(|selector| {
                    let active = selector.current();
                    (
                        c.id,
                        ChannelLayers {
                            selector,
                            active,
                            pending: None,
                        },
                    )
                })
            })
            .collect();
        let initial_ceiling_kbps = seyd_qos::abr::effective_ceiling_kbps(
            profile,
            cfg.channels
                .iter()
                .filter(|c| c.kind == ChannelKind::Video)
                .map(|c| c.max_bitrate_kbps)
                .filter(|&k| k > 0)
                .min()
                .unwrap_or(0),
        );
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
                resume_at: None,
            }),
            slot_notify: Notify::new(),
            epoch: Instant::now(),
            simulcast: Mutex::new(simulcast),
            abr: Mutex::new(AbrState {
                controllers: HashMap::new(),
                fec: (profile.fec_delta_pct, profile.fec_key_pct),
                bitrate_kbps: initial_ceiling_kbps,
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
    /// The bitrate ceiling in force for a video channel: the profile's,
    /// lowered to the publisher's declared maximum (`ChannelSpec::max_bitrate_kbps`).
    pub fn ceiling_kbps(&self, channel: u8) -> u32 {
        self.inner.ceiling_kbps(self.profile(), channel)
    }
    /// The `video-config` a publisher should receive for `channel` under the
    /// current profile, with the bitrate clamped to the channel's ceiling.
    pub fn publisher_config(&self, channel: u8, reason: &str) -> serde_json::Value {
        self.inner.publisher_config(self.profile(), channel, reason)
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
            abr.bitrate_kbps = self.inner.ceiling_kbps(profile, 0);
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
                self.inner.publisher_config(profile, c.id, reason),
            ));
        }
    }

    /// Offer a new frame. In-order bounded queue (see `FrameQueue`): a keyframe
    /// flushes deltas that lead nowhere (`keyframe_flushes_queue`); an
    /// overflowing delta is dropped together with every delta after it until
    /// the next keyframe, and a recovery request is raised so that keyframe
    /// arrives in ~1 RTT.
    pub fn push_video(&self, channel: u8, frame: VideoFrame) {
        self.push_video_layer(channel, 0, frame)
    }

    /// As `push_video`, for a channel that publishes several simulcast layers
    /// (ADR 0008). Frames of every declared layer are offered; the engine
    /// relays one and drops the rest, so the alternative is always encoded and
    /// one keyframe away. A channel that declared no ladder ignores `layer`.
    pub fn push_video_layer(&self, channel: u8, layer: u8, frame: VideoFrame) {
        {
            let mut sim = self.inner.simulcast.lock().unwrap();
            if let Some(st) = sim.get_mut(&channel) {
                // The switch lands here rather than at selection time: a
                // keyframe is the only point where a decoder can start on the
                // new layer's parameter sets.
                if st.pending == Some(layer) && frame.keyframe {
                    st.active = layer;
                    st.pending = None;
                }
                if st.active != layer {
                    self.inner
                        .counters
                        .frames_dropped_inactive_layer
                        .fetch_add(1, Ordering::Relaxed);
                    return;
                }
            }
        }
        self.inner
            .counters
            .frames_in
            .fetch_add(1, Ordering::Relaxed);
        let mut q = self.inner.queue.lock().unwrap();
        if frame.keyframe {
            if keyframe_flushes_queue(q.frames.len(), q.skip_until_key) {
                let flushed = q.frames.len();
                q.frames.clear();
                if flushed > 0 {
                    self.inner
                        .counters
                        .frames_skipped_stale
                        .fetch_add(flushed as u64, Ordering::Relaxed);
                }
            }
            q.skip_until_key = false;
            q.resume_at = None;
        } else if (q.skip_until_key && !q.resume_due()) || q.frames.len() >= MAX_QUEUED_FRAMES {
            let first = !q.skip_until_key;
            q.skip_until_key = true;
            self.inner
                .counters
                .frames_skipped_stale
                .fetch_add(1, Ordering::Relaxed);
            drop(q);
            if first {
                let asked = self.request_recovery(channel, "sender-backlog");
                self.arm_resume(asked);
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

    /// Serve a session that did not come from the endpoint — a relayed one
    /// (ADR 0010). It is treated exactly like an accepted WebTransport session;
    /// only its path label differs.
    pub fn serve(&self, session: Session) {
        tokio::spawn(self.clone().run_session(Arc::new(session)));
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
        // FEC rates in force: the profile's, or what ABR moved them to; and
        // the bitrate currently asked of the publisher, for the keyframe
        // allowance in admission control.
        let ((fec_delta, fec_key), bitrate_kbps) = {
            let abr = self.inner.abr.lock().unwrap();
            (abr.fec, abr.bitrate_kbps)
        };
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
        let threshold = profile.drop_threshold_bytes_at(self.inner.ceiling_kbps(profile, channel), fps);
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
            // buffer — or arrives while the bytes already queued exceed the
            // profile's backlog budget — is skipped cleanly. Keyframes always go.
            let space = s.transport.send_buffer_space();
            let queued = s.transport.send_buffer_queued();
            if frame.keyframe {
                s.skip_until_key.store(false, Ordering::Relaxed);
                s.skip_deadline_ms.store(0, Ordering::Relaxed);
            } else if s.skip_until_key.load(Ordering::Relaxed)
                && self.now_ms() < s.skip_deadline_ms.load(Ordering::Relaxed)
            {
                self.inner
                    .counters
                    .frames_dropped_backlog
                    .fetch_add(1, Ordering::Relaxed);
                continue;
            } else {
                let allowance = keyframe_allowance(
                    s.last_keyframe
                        .lock()
                        .unwrap()
                        .map(|(at, bytes)| (at.elapsed(), bytes)),
                    bitrate_kbps,
                );
                let over = backlog_exceeds(queued, threshold, allowance);
                let sustained = backlog_sustained(
                    over,
                    &mut s.backlog_since.lock().unwrap(),
                    Instant::now(),
                    BACKLOG_SUSTAIN,
                );
                if wire_bytes > space || sustained {
                    self.inner
                        .counters
                        .frames_dropped_backlog
                        .fetch_add(1, Ordering::Relaxed);
                    s.skip_until_key.store(true, Ordering::Relaxed);
                    s.skip_deadline_ms.store(
                        self.now_ms() + self.profile().recovery_grace_ms as u64,
                        Ordering::Relaxed,
                    );
                    tracing::debug!(
                        frame_id,
                        wire_bytes,
                        queued,
                        allowance,
                        threshold,
                        "delta frame dropped: backlog; skipping until keyframe"
                    );
                    let asked = self.request_recovery(channel, "backlog");
                    self.arm_resume(asked);
                    continue;
                }
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
                if frame.keyframe {
                    *s.last_keyframe.lock().unwrap() = Some((Instant::now(), wire_bytes));
                }
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
        let remote = transport
            .remote_addr()
            .map(|a| a.to_string())
            .unwrap_or_else(|| "relay".to_string());
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
            skip_deadline_ms: std::sync::atomic::AtomicU64::new(0),
            pilot: Mutex::new(PilotSample::default()),
            last_keyframe: Mutex::new(None),
            backlog_since: Mutex::new(None),
        });
        self.inner
            .sessions
            .lock()
            .unwrap()
            .insert(tid, state.clone());
        // What the pilot is told it is on. A relayed session is never
        // dressed up as a direct one: the label reaches the HUD (ADR 0010).
        let path_label = match transport.kind() {
            seyd_transport::SessionKind::Relay => "relay",
            seyd_transport::SessionKind::Direct => match transport.remote_addr() {
                Some(a) if a.ip().is_ipv6() => "host6",
                _ => "host",
            },
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
                    "abr_bitrate_kbps": abr.bitrate_kbps, "abr_ceiling_kbps": engine.ceiling_kbps(0),
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
                        Ok(FromPilot::Loss { ch, .. }) => {
                            let asked = self.request_recovery(ch, "pilot-loss");
                            self.arm_resume(asked);
                        }
                        Ok(FromPilot::RequestKeyframe { ch }) => {
                            // Always answered with an IDR, so no resume timer.
                            let _ = self.request_recovery(ch, "pilot-request");
                        }
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
            for ch in video_channels.iter().copied() {
                let ctl = abr
                    .controllers
                    .entry(ch)
                    .or_insert_with(|| {
                        AbrController::with_ceiling(profile, self.inner.publisher_max_kbps(ch))
                    });
                let Some(d) = ctl.step(&sample) else { continue };
                abr.fec = (d.fec_delta_pct, d.fec_key_pct);
                abr.reason = d.reason;
                if d.bitrate_changed {
                    let up = d.max_bitrate_kbps > abr.bitrate_kbps;
                    abr.bitrate_kbps = d.max_bitrate_kbps;
                    let mut cfg = self
                        .inner
                        .publisher_config(profile, ch, if up { "abr-up" } else { "abr-down" });
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

            // Which encoding those bits buy (ADR 0008). Stepped every tick and
            // not only when the bitrate moved, so a link that recovers while
            // sitting at a steady target still climbs back up the ladder.
            let target = abr.bitrate_kbps;
            let mut sim = self.inner.simulcast.lock().unwrap();
            for ch in video_channels {
                let Some(st) = sim.get_mut(&ch) else { continue };
                let Some(sel) = st.selector.step(target) else { continue };
                // Armed, not applied: `push_video_layer` completes the switch on
                // the target layer's next keyframe.
                st.pending = Some(sel.layer);
                let _ = self.inner.events.try_send(Event::LayerChanged {
                    channel: ch,
                    layer: sel.layer,
                    name: st.selector.current_name().to_string(),
                    reason: sel.reason,
                });
                tracing::info!(
                    channel = ch,
                    layer = sel.layer,
                    name = st.selector.current_name(),
                    reason = sel.reason,
                    target_kbps = target,
                    "simulcast"
                );
            }
        }
    }

    fn now_ms(&self) -> u64 {
        self.inner.epoch.elapsed().as_millis() as u64
    }

    /// After asking for a recovery point that will not produce a keyframe,
    /// set the time at which the sender gives up waiting and resumes deltas.
    ///
    /// Without this a stream recovered by intra-refresh or an LTR reference
    /// latches silent: `skip_until_key` is cleared only by a keyframe, and one
    /// never comes.
    fn arm_resume(&self, asked: Option<Recovery>) {
        let Some(kind) = asked else { return };
        if kind.yields_keyframe() {
            return;
        }
        let grace = Duration::from_millis(self.profile().recovery_grace_ms as u64);
        let mut q = self.inner.queue.lock().unwrap();
        q.resume_at = Some(Instant::now() + grace);
    }

    /// Ask the publisher for a recovery point, climbing the ladder only as far
    /// as the situation needs.
    ///
    /// Returns the rung asked for, so the caller can decide how long to wait:
    /// only `Idr` promises a keyframe.
    fn request_recovery(&self, channel: u8, reason: &'static str) -> Option<Recovery> {
        // One request per 250 ms: a lost delta frame smears the picture until
        // a recovery point arrives, so waiting longer costs the operator more
        // than the keyframe costs the link. Spurious requests are avoided at
        // the source (adaptive close-out in the pilot), not by waiting here.
        let min_gap = Duration::from_millis(250);
        let now = Instant::now();
        let mut last = self.inner.last_recovery.lock().unwrap();
        let previous = last.get(&channel).copied();
        if let Some(p) = previous {
            if now.duration_since(p.last) < min_gap {
                return None;
            }
        }

        let grace = Duration::from_millis(self.profile().recovery_grace_ms as u64);
        let kind = if reason == "pilot-request" {
            // The pilot's decoder cannot start or resync without a key frame;
            // no cheaper rung can answer this one.
            Recovery::Idr
        } else if !self.inner.cfg.recovery_ladder {
            Recovery::Idr
        } else {
            match previous {
                // Quiet for a while: an isolated loss, start cheap again.
                Some(p) if now.duration_since(p.last) >= RECOVERY_ESCALATE_WINDOW => Recovery::Ltr,
                // This rung has had its grace period and loss continues, so it
                // did not work — climb.
                Some(p) if now.duration_since(p.rung_since) >= grace => p.rung.escalate(),
                // Still inside the grace: the publisher has not had time yet.
                Some(p) => p.rung,
                None => Recovery::Ltr,
            }
        };

        // `pilot-request` is an out-of-band demand for a key chunk, not a rung.
        // Recording it as one pins the ladder at IDR for every later loss,
        // because IDR escalates to itself — which is how the cheaper rungs
        // became unreachable the first time this ran against a real pilot.
        let ladder_rung = reason != "pilot-request";
        let (rung, rung_since) = match previous {
            _ if !ladder_rung => previous
                .map(|p| (p.rung, p.rung_since))
                .unwrap_or((Recovery::Ltr, now)),
            Some(p) if p.rung == kind => (kind, p.rung_since),
            _ => (kind, now),
        };
        last.insert(
            channel,
            RecoveryState {
                last: now,
                rung,
                rung_since,
            },
        );
        drop(last);

        self.inner
            .counters
            .keyframes_requested
            .fetch_add(1, Ordering::Relaxed);
        let _ = self.inner.events.try_send(Event::RecoveryRequest {
            channel,
            kind: kind.as_str(),
            reason,
        });
        Some(kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seyd_qos::BALANCED;

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

    #[test]
    fn a_keyframe_keeps_a_shallow_contiguous_queue() {
        // The ordinary case: the sender is a frame or two behind and the queued
        // deltas run straight into this keyframe. Discarding them would skip
        // that motion and show the operator a jump instead.
        assert!(!keyframe_flushes_queue(0, false));
        assert!(!keyframe_flushes_queue(1, false));
        assert!(!keyframe_flushes_queue(KEYFRAME_FLUSH_FRAMES - 1, false));
    }

    #[test]
    fn a_keyframe_flushes_deltas_that_lead_nowhere() {
        // Chain already broken: a gap sits between these and the keyframe
        // whatever we do, so sending them only delays the recovery point.
        assert!(keyframe_flushes_queue(1, true));
        assert!(keyframe_flushes_queue(0, true));
        // Sender far enough behind that draining beats the motion.
        assert!(keyframe_flushes_queue(KEYFRAME_FLUSH_FRAMES, false));
        assert!(keyframe_flushes_queue(MAX_QUEUED_FRAMES, false));
    }

    #[test]
    fn backlog_is_measured_on_queued_bytes_not_free_space() {
        // balanced at 30 fps: 2 frames = 25 000 B. Nothing queued → admit.
        let threshold = BALANCED.drop_threshold_bytes(30);
        assert!(!backlog_exceeds(0, threshold, 0));
        assert!(!backlog_exceeds(threshold, threshold, 0), "at the budget still admits");
        // One byte over the budget drops, whatever the buffer's total size —
        // the old free-space check needed ~725 KB queued to reach this point.
        assert!(backlog_exceeds(threshold + 1, threshold, 0));
    }

    #[test]
    fn a_backlog_shorter_than_a_probe_rtt_does_not_drop() {
        // BBR's ProbeRTT throttles the link for 200 ms every 10 s; the queue
        // it builds must not cost a frame, because a dropped delta costs a
        // freeze until the next keyframe.
        let t0 = Instant::now();
        let mut since = None;
        assert!(!backlog_sustained(true, &mut since, t0, BACKLOG_SUSTAIN));
        assert!(!backlog_sustained(true, &mut since, t0 + Duration::from_millis(200), BACKLOG_SUSTAIN));
        // Back under: the clock resets.
        assert!(!backlog_sustained(false, &mut since, t0 + Duration::from_millis(250), BACKLOG_SUSTAIN));
        assert!(since.is_none());
        // Over again and staying over: drops once the sustain has elapsed.
        let t1 = t0 + Duration::from_secs(1);
        assert!(!backlog_sustained(true, &mut since, t1, BACKLOG_SUSTAIN));
        assert!(backlog_sustained(true, &mut since, t1 + BACKLOG_SUSTAIN, BACKLOG_SUSTAIN));
        assert!(backlog_sustained(true, &mut since, t1 + Duration::from_secs(2), BACKLOG_SUSTAIN));
    }

    #[test]
    fn a_draining_keyframe_is_not_backlog() {
        let threshold = BALANCED.drop_threshold_bytes(30);
        let kf = 30_000; // wire bytes of a keyframe, ~80 ms at 3 Mbps
        // Just after the keyframe the queue is keyframe + a delta: legitimately
        // deep, and the allowance discounts the keyframe.
        let a = keyframe_allowance(Some((Duration::from_millis(30), kf)), 3000);
        assert_eq!(a, kf);
        assert!(!backlog_exceeds(kf + 12_500, threshold, a));
        // Still over the budget *beyond* the keyframe: real backlog, drop.
        assert!(backlog_exceeds(kf + threshold + 1, threshold, a));
        // Long after its drain time the keyframe is not an excuse any more.
        let late = keyframe_allowance(Some((Duration::from_millis(500), kf)), 3000);
        assert_eq!(late, 0);
        assert!(backlog_exceeds(kf + 12_500, threshold, late));
        // No keyframe yet: no allowance.
        assert_eq!(keyframe_allowance(None, 3000), 0);
    }

    #[test]
    fn a_slower_bitrate_request_gives_the_keyframe_longer_to_drain() {
        let kf = 60_000; // 320 ms at 1500 kbps, so the window is 640 ms
        assert_eq!(keyframe_allowance(Some((Duration::from_millis(600), kf)), 1500), kf);
        assert_eq!(keyframe_allowance(Some((Duration::from_millis(700), kf)), 1500), 0);
    }

    #[test]
    fn keeping_a_queue_still_leaves_the_sender_room() {
        // A kept queue plus the keyframe itself must stay within capacity, or
        // the very next delta would overflow and set skip_until_key.
        let kept = KEYFRAME_FLUSH_FRAMES - 1;
        assert!(kept + 1 < MAX_QUEUED_FRAMES);
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

#[cfg(test)]
mod recovery_tests {
    use super::*;

    #[test]
    fn the_ladder_climbs_on_repeat_and_resets_when_quiet() {
        assert_eq!(Recovery::Ltr.escalate(), Recovery::IntraRefresh);
        assert_eq!(Recovery::IntraRefresh.escalate(), Recovery::Idr);
        // Saturates: there is nothing more expensive than a keyframe.
        assert_eq!(Recovery::Idr.escalate(), Recovery::Idr);
    }

    #[test]
    fn only_an_idr_promises_a_keyframe() {
        // This is what decides whether the sender may wait for one, or must
        // arm a timer instead. Getting it wrong latches a stream silent.
        assert!(Recovery::Idr.yields_keyframe());
        assert!(!Recovery::Ltr.yields_keyframe());
        assert!(!Recovery::IntraRefresh.yields_keyframe());
    }

    #[test]
    fn the_wire_names_match_the_documented_publisher_control_vocabulary() {
        // docs/protocol/seydd.md publishes these strings; a robot-side
        // publisher matches on them.
        assert_eq!(Recovery::Ltr.as_str(), "ltr");
        assert_eq!(Recovery::IntraRefresh.as_str(), "intra_refresh");
        assert_eq!(Recovery::Idr.as_str(), "idr");
    }

    #[test]
    fn a_queue_waiting_on_a_keyframe_releases_itself_when_the_grace_expires() {
        // The no-IDR hazard: a publisher recovering by intra-refresh never
        // sends a keyframe, so a latch only a keyframe can clear would hold
        // the stream silent for the rest of the session.
        let mut q = FrameQueue {
            frames: VecDeque::new(),
            skip_until_key: true,
            resume_at: Some(Instant::now() - Duration::from_millis(1)),
        };
        assert!(q.resume_due(), "an expired grace must release the latch");

        q.resume_at = Some(Instant::now() + Duration::from_secs(5));
        assert!(!q.resume_due(), "an unexpired grace must still hold it");

        // No deadline armed means an IDR was asked for: wait for it.
        q.resume_at = None;
        assert!(!q.resume_due());
    }
}
