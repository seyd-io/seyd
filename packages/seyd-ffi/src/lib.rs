// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
//! The Seyd C ABI (ADR 0004).
//!
//! This crate is the *only* place Seyd crosses into C. It contains no protocol
//! logic: it owns a tokio runtime, drives [`seyd_core::Agent`], and translates
//! between C types and Rust ones. Everything a C++, Python or ROS 2 wrapper
//! needs is here, so those wrappers never reimplement a header parser, an FEC
//! table or a NAT heuristic (ADR 0004 rule 1).
//!
//! # Lifecycle
//!
//! `seyd_agent_create` → `seyd_channel_add` (once per channel, in the order
//! the channels should be numbered) → `seyd_agent_start` → push media →
//! `seyd_agent_stop` → `seyd_agent_destroy`.
//!
//! # Threading
//!
//! Callbacks are delivered on one dedicated Seyd thread, in order, and must
//! not block (ADR 0004 rule 2) — a slow callback stalls every later event, not
//! the media path. `seyd_push_*` may be called from any thread and never
//! blocks.

// C names, deliberately: these identifiers are the public ABI and must read
// the same in `seyd.h` as they do here.
#![allow(non_camel_case_types)]

use bytes::Bytes;
use seyd_core::channels::{ChannelKind as CoreKind, ChannelSpec};
use seyd_core::{Agent, AgentConfig, AgentEvent, Role, VideoFrame};
use std::ffi::{c_char, c_void, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::Mutex;

/// Bumped only when an existing symbol or struct layout changes; additions are
/// appended (ADR 0004 rule 3).
pub const SEYD_ABI_VERSION: u32 = 1;

// ── status codes ─────────────────────────────────────────────────────────

/// Result of every fallible call. `SEYD_OK` is zero; everything else is an
/// error, and [`seyd_last_error`] carries a human-readable reason.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum seyd_status {
    SEYD_OK = 0,
    /// A null pointer, a non-UTF-8 string, or an out-of-range value.
    SEYD_ERR_INVALID_ARG = 1,
    /// Called in the wrong lifecycle state (e.g. `seyd_channel_add` after start).
    SEYD_ERR_STATE = 2,
    /// The configuration is not usable (e.g. an unknown QoS profile).
    SEYD_ERR_CONFIG = 3,
    /// Sockets, discovery or the certificate failed; the agent did not start.
    SEYD_ERR_NETWORK = 4,
    /// No channel with that id.
    SEYD_ERR_NOT_FOUND = 5,
    /// Reserved: the send path is saturated. Not currently returned — the
    /// engine drops under pressure by design (whole frame or nothing) and
    /// reports it through `frames_dropped_backlog` in [`seyd_counters`].
    SEYD_ERR_BACKPRESSURE = 6,
    SEYD_ERR_INTERNAL = 7,
}

use seyd_status::*;

// ── enums ────────────────────────────────────────────────────────────────

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum seyd_channel_kind {
    /// Encoded pictures, FEC-protected, whole frame or nothing.
    SEYD_CHANNEL_VIDEO = 0,
    /// Robot → pilot messages.
    SEYD_CHANNEL_SENSOR = 1,
    /// Pilot → robot messages; delivered to `on_command`.
    SEYD_CHANNEL_COMMAND = 2,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum seyd_role {
    SEYD_ROLE_DRIVER = 0,
    SEYD_ROLE_OBSERVER = 1,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum seyd_signal_state {
    SEYD_SIGNAL_CONNECTED = 0,
    SEYD_SIGNAL_DISCONNECTED = 1,
    /// Authentication was refused; `detail` says why. The client keeps
    /// retrying, but this is almost always a configuration problem.
    SEYD_SIGNAL_DENIED = 2,
}

// ── configuration ────────────────────────────────────────────────────────

/// How the robot is configured. Strings are borrowed for the duration of the
/// `seyd_agent_create` call only; Seyd copies what it keeps.
#[repr(C)]
pub struct seyd_config {
    /// Robot id as enrolled with the cloud. Required.
    pub robot_id: *const c_char,
    /// Signal server WebSocket URL. Required.
    pub signal_url: *const c_char,
    /// Ed25519 credential file; created on first run. NULL → `/var/lib/seyd/robot.key`.
    pub credential_path: *const c_char,
    /// UDP port for QUIC. 0 → 4433.
    pub quic_port: u16,
    pub ipv6: bool,
    /// Attempt PCP/NAT-PMP/UPnP port mapping during discovery.
    pub port_mapping: bool,
    /// Skip discovery and advertise this address only (LAN testing). NULL to discover.
    pub host_override: *const c_char,
    /// QoS ceiling by name. NULL → `balanced`.
    pub qos_profile: *const c_char,
    /// Concurrent pilot sessions. 0 → 4.
    pub max_sessions: u32,
}

/// One channel, added before the agent starts. Channels are numbered from 1 in
/// the order they are added, and that numbering is what the pilot sees.
#[repr(C)]
pub struct seyd_channel_config {
    pub kind: seyd_channel_kind,
    /// Channel name shown to the pilot. Required.
    pub name: *const c_char,
    /// Codec string for video (e.g. `avc1.42001f`). NULL → `json`.
    pub codec: *const c_char,
    /// Nominal frames per second for video; 0 otherwise.
    pub fps: u32,
}

/// Engine counters, sampled atomically-ish (each field is atomic; the set is
/// not a consistent snapshot).
#[repr(C)]
#[derive(Default)]
pub struct seyd_counters_t {
    pub frames_in: u64,
    pub frames_sent: u64,
    pub frames_dropped_backlog: u64,
    pub frames_skipped_stale: u64,
    pub keyframes_requested: u64,
    pub chunks_sent: u64,
    pub parity_sent: u64,
    pub bytes_sent: u64,
}

/// Callbacks, all optional (leave a field NULL to ignore that event). They are
/// invoked on one dedicated Seyd thread and must not block. Pointers passed to
/// a callback are valid only for the duration of the call.
#[repr(C)]
pub struct seyd_callbacks {
    /// Passed back verbatim as the first argument of every callback.
    pub user: *mut c_void,
    pub on_session_started: Option<
        unsafe extern "C" fn(
            user: *mut c_void,
            session_id: *const c_char,
            role: seyd_role,
            path_label: *const c_char,
        ),
    >,
    /// Park actuators here: this fires whether the pilot said goodbye or vanished.
    pub on_session_ended: Option<
        unsafe extern "C" fn(user: *mut c_void, session_id: *const c_char, reason: *const c_char),
    >,
    pub on_command:
        Option<unsafe extern "C" fn(user: *mut c_void, channel: u8, data: *const u8, len: usize)>,
    /// Seyd's transport-observable targets for the publisher, as the
    /// `video-config` JSON of docs/protocol/seydd.md (`maxBitrateKbps`,
    /// `latencyBudgetMs`, `maxGopMs`, `preferIntraRefresh`, `suggestedFps`,
    /// `reason`). The only place Seyd talks down to the encoder. Keyframes are
    /// on demand (ADR 0009): honour `on_recovery_request`, and treat `maxGopMs`
    /// as a long ceiling rather than a cadence.
    pub on_requested_config: Option<unsafe extern "C" fn(user: *mut c_void, json: *const c_char)>,
    /// A pilot needs a recovery point: `kind` is `ltr`, `intra_refresh` or `idr`.
    pub on_recovery_request: Option<
        unsafe extern "C" fn(
            user: *mut c_void,
            channel: u8,
            kind: *const c_char,
            reason: *const c_char,
        ),
    >,
    /// The relayed simulcast layer changed (ADR 0008). Seyd switches on that
    /// layer's next keyframe; a publisher that can force one should, which turns
    /// "next keyframe" from up to a GOP into immediately. `reason` is `up` or
    /// `down`.
    pub on_layer_changed: Option<
        unsafe extern "C" fn(
            user: *mut c_void,
            channel: u8,
            layer: u8,
            name: *const c_char,
            reason: *const c_char,
        ),
    >,
    /// The robot is unreachable while signaling is down. `detail` is NULL
    /// except for `SEYD_SIGNAL_DENIED`.
    pub on_signal_state: Option<
        unsafe extern "C" fn(user: *mut c_void, state: seyd_signal_state, detail: *const c_char),
    >,
    /// Candidates were gathered and announced; the `NatReport` as JSON.
    pub on_nat_report: Option<unsafe extern "C" fn(user: *mut c_void, json: *const c_char)>,
}

/// Callbacks travel to the dispatch thread. C has no opinion about `Send`, so
/// the contract is the caller's: the `user` pointer must remain valid and
/// usable from another thread until `seyd_agent_destroy` returns.
struct Callbacks(seyd_callbacks);
unsafe impl Send for Callbacks {}

// ── the agent handle ─────────────────────────────────────────────────────

/// Opaque agent handle.
pub struct seyd_agent {
    runtime: tokio::runtime::Runtime,
    state: Mutex<State>,
    callbacks: Option<Callbacks>,
}

enum State {
    /// Before `seyd_agent_start`: configuration is still being assembled.
    Configuring {
        cfg: AgentConfig,
    },
    Running {
        agent: Agent,
        pump: Option<std::thread::JoinHandle<()>>,
    },
    Stopped,
}

// ── error reporting ──────────────────────────────────────────────────────

thread_local! {
    static LAST_ERROR: std::cell::RefCell<CString> = std::cell::RefCell::new(CString::default());
}

fn set_error(msg: impl Into<Vec<u8>>) {
    let c = CString::new(msg).unwrap_or_default();
    LAST_ERROR.with(|e| *e.borrow_mut() = c);
}

fn fail(status: seyd_status, msg: impl Into<Vec<u8>>) -> seyd_status {
    set_error(msg);
    status
}

/// Run a public entry point, turning a panic into `SEYD_ERR_INTERNAL` rather
/// than unwinding into C (which would be undefined behaviour).
fn guard(f: impl FnOnce() -> seyd_status) -> seyd_status {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(s) => s,
        Err(_) => fail(SEYD_ERR_INTERNAL, "panic in seyd"),
    }
}

/// Borrow a C string. `None` for NULL; `Err` for non-UTF-8.
unsafe fn opt_str<'a>(p: *const c_char) -> Result<Option<&'a str>, ()> {
    if p.is_null() {
        return Ok(None);
    }
    CStr::from_ptr(p).to_str().map(Some).map_err(|_| ())
}

// ── entry points ─────────────────────────────────────────────────────────

/// ABI version of this library. Compare with `SEYD_ABI_VERSION` from the
/// header a wrapper was built against.
#[no_mangle]
pub extern "C" fn seyd_abi_version() -> u32 {
    SEYD_ABI_VERSION
}

/// Library version string, e.g. `0.1.0`. Static; never free it.
#[no_mangle]
pub extern "C" fn seyd_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr() as *const c_char
}

/// The reason for the calling thread's most recent failure. Valid until the
/// next failing call on this thread; never free it.
#[no_mangle]
pub extern "C" fn seyd_last_error() -> *const c_char {
    LAST_ERROR.with(|e| e.borrow().as_ptr())
}

/// Install a tracing subscriber writing to stderr, filtered by `directive`
/// (e.g. `info`, `seyd_core=debug`). NULL → `info`. Optional; call at most
/// once, before creating an agent.
///
/// # Safety
/// `directive` must be a valid C string or NULL.
#[no_mangle]
pub unsafe extern "C" fn seyd_init_logging(directive: *const c_char) -> seyd_status {
    guard(|| {
        let d = match unsafe { opt_str(directive) } {
            Ok(v) => v.unwrap_or("info"),
            Err(()) => return fail(SEYD_ERR_INVALID_ARG, "directive is not UTF-8"),
        };
        match tracing_subscriber::EnvFilter::try_new(d) {
            Ok(filter) => {
                let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
                SEYD_OK
            }
            Err(e) => fail(SEYD_ERR_INVALID_ARG, format!("bad log directive: {e}")),
        }
    })
}

/// Create an agent. Does no network work — add channels, then start.
///
/// # Safety
/// `cfg` and `callbacks` must be valid pointers; `out` receives an owned handle
/// that must be released with `seyd_agent_destroy`.
#[no_mangle]
pub unsafe extern "C" fn seyd_agent_create(
    cfg: *const seyd_config,
    callbacks: *const seyd_callbacks,
    out: *mut *mut seyd_agent,
) -> seyd_status {
    guard(|| {
        if cfg.is_null() || out.is_null() {
            return fail(SEYD_ERR_INVALID_ARG, "cfg and out must not be NULL");
        }
        let cfg = &*cfg;
        let (robot_id, signal_url) = match (opt_str(cfg.robot_id), opt_str(cfg.signal_url)) {
            (Ok(Some(r)), Ok(Some(s))) => (r.to_string(), s.to_string()),
            (Err(()), _) | (_, Err(())) => {
                return fail(SEYD_ERR_INVALID_ARG, "robot_id/signal_url is not UTF-8")
            }
            _ => return fail(SEYD_ERR_INVALID_ARG, "robot_id and signal_url are required"),
        };
        let (credential_path, host_override, qos_profile) = match (
            opt_str(cfg.credential_path),
            opt_str(cfg.host_override),
            opt_str(cfg.qos_profile),
        ) {
            (Ok(c), Ok(h), Ok(q)) => (c, h, q),
            _ => return fail(SEYD_ERR_INVALID_ARG, "a config string is not UTF-8"),
        };
        let qos_profile = qos_profile.unwrap_or("balanced").to_string();
        if seyd_qos::get(&qos_profile).is_none() {
            return fail(
                SEYD_ERR_CONFIG,
                format!("unknown qos_profile {qos_profile:?}"),
            );
        }

        let defaults = AgentConfig::default();
        let agent_cfg = AgentConfig {
            // Not in the C config struct: adding a field there would change
            // the ABI. Hosts get the ladder, and a publisher that ignores
            // `kind` behaves exactly as it did before.
            recovery_ladder: true,
            robot_id,
            signal_url,
            credential_path: credential_path
                .map(PathBuf::from)
                .unwrap_or(defaults.credential_path),
            quic_port: if cfg.quic_port == 0 {
                defaults.quic_port
            } else {
                cfg.quic_port
            },
            ipv6: cfg.ipv6,
            port_mapping: cfg.port_mapping,
            host_override: host_override.map(str::to_string),
            qos_profile,
            max_sessions: if cfg.max_sessions == 0 {
                defaults.max_sessions
            } else {
                cfg.max_sessions
            },
            channels: Vec::new(),
            agent_version: format!("seyd-ffi/{}", env!("CARGO_PKG_VERSION")),
            // Not in the C config struct either (ABI). The relay is a last
            // resort the pilot chooses and shows; serving it costs nothing.
            relay: defaults.relay,
        };

        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(r) => r,
            Err(e) => return fail(SEYD_ERR_INTERNAL, format!("runtime: {e}")),
        };
        let handle = Box::new(seyd_agent {
            runtime,
            state: Mutex::new(State::Configuring { cfg: agent_cfg }),
            callbacks: callbacks.as_ref().map(|c| {
                Callbacks(seyd_callbacks {
                    user: c.user,
                    on_session_started: c.on_session_started,
                    on_session_ended: c.on_session_ended,
                    on_command: c.on_command,
                    on_requested_config: c.on_requested_config,
                    on_recovery_request: c.on_recovery_request,
                    on_layer_changed: c.on_layer_changed,
                    on_signal_state: c.on_signal_state,
                    on_nat_report: c.on_nat_report,
                })
            }),
        });
        *out = Box::into_raw(handle);
        SEYD_OK
    })
}

/// Add a channel. Only valid before `seyd_agent_start`. `out_channel_id`
/// receives the id to pass to `seyd_push_*`; may be NULL.
///
/// # Safety
/// `agent` must come from `seyd_agent_create`; `cfg` must be valid.
#[no_mangle]
pub unsafe extern "C" fn seyd_channel_add(
    agent: *mut seyd_agent,
    cfg: *const seyd_channel_config,
    out_channel_id: *mut u8,
) -> seyd_status {
    guard(|| {
        let Some(agent) = agent.as_ref() else {
            return fail(SEYD_ERR_INVALID_ARG, "agent must not be NULL");
        };
        if cfg.is_null() {
            return fail(SEYD_ERR_INVALID_ARG, "cfg must not be NULL");
        }
        let cfg = &*cfg;
        let name = match opt_str(cfg.name) {
            Ok(Some(n)) => n.to_string(),
            _ => return fail(SEYD_ERR_INVALID_ARG, "channel name is required"),
        };
        let codec = match opt_str(cfg.codec) {
            Ok(c) => c.unwrap_or("json").to_string(),
            Err(()) => return fail(SEYD_ERR_INVALID_ARG, "codec is not UTF-8"),
        };
        let mut state = agent.state.lock().unwrap();
        let State::Configuring { cfg: acfg } = &mut *state else {
            return fail(SEYD_ERR_STATE, "channels must be added before start");
        };
        if acfg.channels.len() >= 255 {
            return fail(SEYD_ERR_INVALID_ARG, "at most 255 channels");
        }
        if acfg.channels.iter().any(|c| c.name == name) {
            return fail(SEYD_ERR_INVALID_ARG, format!("duplicate channel {name:?}"));
        }
        let id = (acfg.channels.len() + 1) as u8;
        acfg.channels.push(ChannelSpec {
            id,
            kind: match cfg.kind {
                seyd_channel_kind::SEYD_CHANNEL_VIDEO => CoreKind::Video,
                seyd_channel_kind::SEYD_CHANNEL_SENSOR => CoreKind::Sensor,
                seyd_channel_kind::SEYD_CHANNEL_COMMAND => CoreKind::Command,
            },
            name,
            codec,
            fps: cfg.fps,
            // Layers are declared afterwards with `seyd_channel_add_layer`, so
            // adding one channel stays a single struct with no array in it.
            layers: Vec::new(),
            // Not in the C ABI yet: a host with a rate-limited encoder caps
            // its own requests in `on_requested_config` until it is.
            max_bitrate_kbps: 0,
        });
        if let Some(o) = out_channel_id.as_mut() {
            *o = id;
        }
        SEYD_OK
    })
}

/// Declare one simulcast layer of a video channel (ADR 0008): the same picture,
/// encoded again at a different operating point. Only valid before
/// `seyd_agent_start`, on a channel added by `seyd_channel_add`.
///
/// Call once per layer, in any order. `activate_above_kbps` is the ABR target at
/// or above which the layer is the right choice; the lowest layer is the base
/// and must be 0. A channel with fewer than two layers behaves exactly as one
/// with none, and `seyd_push_frame` keeps working unchanged.
///
/// Feed every declared layer with `seyd_push_frame_layer`. Seyd relays one and
/// drops the rest, switching on the target layer's next keyframe, so the
/// alternative encoding is always ready and a switch costs no reconnect.
///
/// # Safety
/// `agent` must come from `seyd_agent_create`; `name` must be a valid C string.
#[no_mangle]
pub unsafe extern "C" fn seyd_channel_add_layer(
    agent: *mut seyd_agent,
    channel: u8,
    name: *const c_char,
    activate_above_kbps: u32,
) -> seyd_status {
    guard(|| {
        let Some(agent) = agent.as_ref() else {
            return fail(SEYD_ERR_INVALID_ARG, "agent must not be NULL");
        };
        let name = match opt_str(name) {
            Ok(Some(n)) => n.to_string(),
            _ => return fail(SEYD_ERR_INVALID_ARG, "layer name is required"),
        };
        let mut state = agent.state.lock().unwrap();
        let State::Configuring { cfg: acfg } = &mut *state else {
            return fail(SEYD_ERR_STATE, "layers must be added before start");
        };
        let Some(c) = acfg.channels.iter_mut().find(|c| c.id == channel) else {
            return fail(SEYD_ERR_NOT_FOUND, "no such channel");
        };
        if c.kind != CoreKind::Video {
            return fail(
                SEYD_ERR_INVALID_ARG,
                "simulcast layers are for video channels only",
            );
        }
        if c.layers.iter().any(|l| l.name == name) {
            return fail(SEYD_ERR_INVALID_ARG, format!("duplicate layer {name:?}"));
        }
        if c.layers
            .iter()
            .any(|l| l.activate_above_kbps == activate_above_kbps)
        {
            return fail(
                SEYD_ERR_INVALID_ARG,
                "another layer already activates at that bitrate; one of them \
                 could never be selected",
            );
        }
        if c.layers.len() >= 255 {
            return fail(SEYD_ERR_INVALID_ARG, "at most 255 layers");
        }
        c.layers.push(seyd_qos::simulcast::VideoLayer {
            id: 0, // renumbered below, lowest first
            name,
            activate_above_kbps,
        });
        c.layers.sort_by_key(|l| l.activate_above_kbps);
        for (i, l) in c.layers.iter_mut().enumerate() {
            l.id = i as u8;
        }
        SEYD_OK
    })
}

/// Bind, discover, announce and start serving. Blocks until the agent is
/// announced (discovery takes up to a second or two). Callbacks begin after
/// this returns `SEYD_OK`.
///
/// # Safety
/// `agent` must come from `seyd_agent_create`.
#[no_mangle]
pub unsafe extern "C" fn seyd_agent_start(agent: *mut seyd_agent) -> seyd_status {
    guard(|| {
        let Some(agent) = agent.as_ref() else {
            return fail(SEYD_ERR_INVALID_ARG, "agent must not be NULL");
        };
        let mut state = agent.state.lock().unwrap();
        let State::Configuring { cfg } = &*state else {
            return fail(SEYD_ERR_STATE, "agent already started");
        };
        if cfg.channels.is_empty() {
            return fail(SEYD_ERR_CONFIG, "add at least one channel before starting");
        }
        let cfg = cfg.clone();
        let started = agent.runtime.block_on(Agent::start(cfg));
        let (core, events) = match started {
            Ok(v) => v,
            Err(e) => return fail(SEYD_ERR_NETWORK, format!("{e:#}")),
        };
        // One dedicated thread delivers callbacks in order, off the runtime's
        // workers, so a slow callback cannot stall the media path.
        let pump = agent.callbacks.as_ref().map(|cbs| {
            let cbs = Callbacks(copy_callbacks(&cbs.0));
            std::thread::Builder::new()
                .name("seyd-callbacks".into())
                .spawn(move || pump_events(cbs, events))
                .expect("spawn callback thread")
        });
        *state = State::Running { agent: core, pump };
        SEYD_OK
    })
}

/// Stop serving and tear down sessions. Blocks until the callback thread has
/// finished, so no callback runs after this returns. Idempotent.
///
/// # Safety
/// `agent` must come from `seyd_agent_create`.
#[no_mangle]
pub unsafe extern "C" fn seyd_agent_stop(agent: *mut seyd_agent) -> seyd_status {
    guard(|| {
        let Some(agent) = agent.as_ref() else {
            return fail(SEYD_ERR_INVALID_ARG, "agent must not be NULL");
        };
        let pump = {
            let mut state = agent.state.lock().unwrap();
            match std::mem::replace(&mut *state, State::Stopped) {
                State::Running { agent, pump } => {
                    agent.stop();
                    pump
                }
                _ => None,
            }
        };
        // Joined outside the lock: the callback thread may re-enter the API.
        if let Some(p) = pump {
            let _ = p.join();
        }
        SEYD_OK
    })
}

/// Stop if running, then release the handle. The handle is invalid afterwards.
///
/// # Safety
/// `agent` must come from `seyd_agent_create` and must not be used again.
#[no_mangle]
pub unsafe extern "C" fn seyd_agent_destroy(agent: *mut seyd_agent) {
    if agent.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        seyd_agent_stop(agent);
        drop(Box::from_raw(agent));
    }));
}

/// Hand one encoded access unit to a video channel. Never blocks: a frame that
/// arrives while the previous one is still going out is dropped by admission
/// control (keyframes are never dropped), counted in
/// `frames_dropped_backlog`. `capture_ts_us` is the source's capture clock and
/// travels to the pilot, which paces presentation on it (ADR 0005).
///
/// # Safety
/// `data` must point to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn seyd_push_frame(
    agent: *mut seyd_agent,
    channel: u8,
    data: *const u8,
    len: usize,
    keyframe: bool,
    capture_ts_us: u64,
) -> seyd_status {
    guard(|| {
        with_running(agent, |core| {
            match core.channels().iter().find(|c| c.id == channel) {
                Some(c) if c.kind == CoreKind::Video => {}
                Some(_) => return fail(SEYD_ERR_INVALID_ARG, "not a video channel"),
                None => return fail(SEYD_ERR_NOT_FOUND, "no such channel"),
            }
            if data.is_null() || len == 0 {
                return fail(SEYD_ERR_INVALID_ARG, "empty frame");
            }
            core.push_video(
                channel,
                VideoFrame {
                    data: Bytes::copy_from_slice(std::slice::from_raw_parts(data, len)),
                    keyframe,
                    capture_ts_us,
                },
            );
            SEYD_OK
        })
    })
}

/// As `seyd_push_frame`, naming which simulcast layer the picture belongs to
/// (ADR 0008). Feed every declared layer; Seyd relays one.
///
/// On a channel that declared no layers this is `seyd_push_frame` and `layer` is
/// ignored, so a wrapper may always call this one.
///
/// # Safety
/// `data` must point to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn seyd_push_frame_layer(
    agent: *mut seyd_agent,
    channel: u8,
    layer: u8,
    data: *const u8,
    len: usize,
    keyframe: bool,
    capture_ts_us: u64,
) -> seyd_status {
    guard(|| {
        with_running(agent, |core| {
            match core.channels().iter().find(|c| c.id == channel) {
                Some(c) if c.kind == CoreKind::Video => {}
                Some(_) => return fail(SEYD_ERR_INVALID_ARG, "not a video channel"),
                None => return fail(SEYD_ERR_NOT_FOUND, "no such channel"),
            }
            if data.is_null() || len == 0 {
                return fail(SEYD_ERR_INVALID_ARG, "empty frame");
            }
            core.push_video_layer(
                channel,
                layer,
                VideoFrame {
                    data: Bytes::copy_from_slice(std::slice::from_raw_parts(data, len)),
                    keyframe,
                    capture_ts_us,
                },
            );
            SEYD_OK
        })
    })
}

/// Send a message on a sensor channel. Never blocks.
///
/// # Safety
/// `data` must point to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn seyd_push_message(
    agent: *mut seyd_agent,
    channel: u8,
    data: *const u8,
    len: usize,
) -> seyd_status {
    guard(|| {
        with_running(agent, |core| {
            match core.channels().iter().find(|c| c.id == channel) {
                Some(c) if c.kind == CoreKind::Sensor => {}
                Some(_) => return fail(SEYD_ERR_INVALID_ARG, "not a sensor channel"),
                None => return fail(SEYD_ERR_NOT_FOUND, "no such channel"),
            }
            let payload: &[u8] = if len == 0 {
                &[]
            } else if data.is_null() {
                return fail(SEYD_ERR_INVALID_ARG, "data must not be NULL");
            } else {
                std::slice::from_raw_parts(data, len)
            };
            core.push_message(channel, payload);
            SEYD_OK
        })
    })
}

/// Look up a channel id by name; useful when a wrapper configures by name.
///
/// # Safety
/// `name` must be a valid C string; `out_channel_id` must be writable.
#[no_mangle]
pub unsafe extern "C" fn seyd_channel_id(
    agent: *mut seyd_agent,
    name: *const c_char,
    out_channel_id: *mut u8,
) -> seyd_status {
    guard(|| {
        let Ok(Some(name)) = opt_str(name) else {
            return fail(SEYD_ERR_INVALID_ARG, "name is required");
        };
        if out_channel_id.is_null() {
            return fail(SEYD_ERR_INVALID_ARG, "out_channel_id must not be NULL");
        }
        with_running(agent, |core| match core.channel_by_name(name) {
            Some(c) => {
                *out_channel_id = c.id;
                SEYD_OK
            }
            None => fail(SEYD_ERR_NOT_FOUND, "no such channel"),
        })
    })
}

/// Switch the QoS ceiling by name (`latency`, `balanced`, `quality`).
///
/// # Safety
/// `name` must be a valid C string.
#[no_mangle]
pub unsafe extern "C" fn seyd_set_qos_profile(
    agent: *mut seyd_agent,
    name: *const c_char,
) -> seyd_status {
    guard(|| {
        let Ok(Some(name)) = opt_str(name) else {
            return fail(SEYD_ERR_INVALID_ARG, "name is required");
        };
        with_running(agent, |core| {
            if core.set_qos_profile(name, "host") {
                SEYD_OK
            } else {
                fail(SEYD_ERR_CONFIG, format!("unknown qos profile {name:?}"))
            }
        })
    })
}

/// Publish free-form robot status to the console, as a JSON object. NULL clears it.
///
/// # Safety
/// `json` must be a valid C string or NULL.
#[no_mangle]
pub unsafe extern "C" fn seyd_set_status_json(
    agent: *mut seyd_agent,
    json: *const c_char,
) -> seyd_status {
    guard(|| {
        let value = match opt_str(json) {
            Ok(None) => None,
            Ok(Some(s)) => match serde_json::from_str(s) {
                Ok(v) => Some(v),
                Err(e) => return fail(SEYD_ERR_INVALID_ARG, format!("status is not JSON: {e}")),
            },
            Err(()) => return fail(SEYD_ERR_INVALID_ARG, "status is not UTF-8"),
        };
        let Some(agent) = agent.as_ref() else {
            return fail(SEYD_ERR_INVALID_ARG, "agent must not be NULL");
        };
        let state = agent.state.lock().unwrap();
        let State::Running { agent: core, .. } = &*state else {
            return fail(SEYD_ERR_STATE, "agent is not running");
        };
        let core = core.clone();
        agent
            .runtime
            .spawn(async move { core.set_status(value).await });
        SEYD_OK
    })
}

/// Number of live pilot sessions.
///
/// # Safety
/// `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn seyd_session_count(agent: *mut seyd_agent, out: *mut u32) -> seyd_status {
    guard(|| {
        if out.is_null() {
            return fail(SEYD_ERR_INVALID_ARG, "out must not be NULL");
        }
        with_running(agent, |core| {
            *out = core.session_count() as u32;
            SEYD_OK
        })
    })
}

/// Snapshot the engine counters.
///
/// # Safety
/// `out` must point to a writable `seyd_counters_t`.
#[no_mangle]
pub unsafe extern "C" fn seyd_counters(
    agent: *mut seyd_agent,
    out: *mut seyd_counters_t,
) -> seyd_status {
    guard(|| {
        if out.is_null() {
            return fail(SEYD_ERR_INVALID_ARG, "out must not be NULL");
        }
        with_running(agent, |core| {
            use std::sync::atomic::Ordering::Relaxed;
            let c = core.counters();
            *out = seyd_counters_t {
                frames_in: c.frames_in.load(Relaxed),
                frames_sent: c.frames_sent.load(Relaxed),
                frames_dropped_backlog: c.frames_dropped_backlog.load(Relaxed),
                frames_skipped_stale: c.frames_skipped_stale.load(Relaxed),
                keyframes_requested: c.keyframes_requested.load(Relaxed),
                chunks_sent: c.chunks_sent.load(Relaxed),
                parity_sent: c.parity_sent.load(Relaxed),
                bytes_sent: c.bytes_sent.load(Relaxed),
            };
            SEYD_OK
        })
    })
}

// ── internals ────────────────────────────────────────────────────────────

/// Run `f` against a started agent, or fail with the reason it isn't one.
unsafe fn with_running(
    agent: *mut seyd_agent,
    f: impl FnOnce(&Agent) -> seyd_status,
) -> seyd_status {
    let Some(agent) = agent.as_ref() else {
        return fail(SEYD_ERR_INVALID_ARG, "agent must not be NULL");
    };
    let state = agent.state.lock().unwrap();
    match &*state {
        State::Running { agent, .. } => f(agent),
        _ => fail(SEYD_ERR_STATE, "agent is not running"),
    }
}

fn copy_callbacks(c: &seyd_callbacks) -> seyd_callbacks {
    seyd_callbacks {
        user: c.user,
        on_session_started: c.on_session_started,
        on_session_ended: c.on_session_ended,
        on_command: c.on_command,
        on_requested_config: c.on_requested_config,
        on_recovery_request: c.on_recovery_request,
        on_layer_changed: c.on_layer_changed,
        on_signal_state: c.on_signal_state,
        on_nat_report: c.on_nat_report,
    }
}

/// Deliver events to C, in order, on this thread until the agent stops.
fn pump_events(cbs: Callbacks, mut events: tokio::sync::mpsc::Receiver<AgentEvent>) {
    let c = &cbs.0;
    while let Some(ev) = events.blocking_recv() {
        // A panic while marshalling must not unwind into C, and must not kill
        // the pump — the next event may well be fine.
        let _ = catch_unwind(AssertUnwindSafe(|| unsafe { dispatch(c, ev) }));
    }
}

/// Marshal one event into a C callback. Every `CString` is bound to a local so
/// it outlives the call — the pointers are only valid for its duration, which
/// is the documented contract.
unsafe fn dispatch(c: &seyd_callbacks, ev: AgentEvent) {
    fn cs(s: &str) -> CString {
        CString::new(s).unwrap_or_default()
    }
    match ev {
        AgentEvent::SessionStarted {
            signal_id,
            role,
            path_label,
        } => {
            if let Some(f) = c.on_session_started {
                let (id, path) = (cs(&signal_id), cs(&path_label));
                let role = match role {
                    Role::Driver => seyd_role::SEYD_ROLE_DRIVER,
                    Role::Observer => seyd_role::SEYD_ROLE_OBSERVER,
                };
                f(c.user, id.as_ptr(), role, path.as_ptr());
            }
        }
        AgentEvent::SessionEnded { signal_id, reason } => {
            if let Some(f) = c.on_session_ended {
                let (id, reason) = (cs(&signal_id), cs(&reason));
                f(c.user, id.as_ptr(), reason.as_ptr());
            }
        }
        AgentEvent::Command { channel, payload } => {
            if let Some(f) = c.on_command {
                f(c.user, channel, payload.as_ptr(), payload.len());
            }
        }
        AgentEvent::RequestedConfig(v) => {
            if let Some(f) = c.on_requested_config {
                let json = cs(&v.to_string());
                f(c.user, json.as_ptr());
            }
        }
        AgentEvent::RecoveryRequest {
            channel,
            kind,
            reason,
        } => {
            if let Some(f) = c.on_recovery_request {
                let (kind, reason) = (cs(kind), cs(reason));
                f(c.user, channel, kind.as_ptr(), reason.as_ptr());
            }
        }
        AgentEvent::LayerChanged {
            channel,
            layer,
            name,
            reason,
        } => {
            if let Some(f) = c.on_layer_changed {
                let (name, reason) = (cs(&name), cs(reason));
                f(c.user, channel, layer, name.as_ptr(), reason.as_ptr());
            }
        }
        AgentEvent::SignalConnected => {
            if let Some(f) = c.on_signal_state {
                f(
                    c.user,
                    seyd_signal_state::SEYD_SIGNAL_CONNECTED,
                    std::ptr::null(),
                );
            }
        }
        AgentEvent::SignalDisconnected => {
            if let Some(f) = c.on_signal_state {
                f(
                    c.user,
                    seyd_signal_state::SEYD_SIGNAL_DISCONNECTED,
                    std::ptr::null(),
                );
            }
        }
        AgentEvent::SignalDenied { reason } => {
            if let Some(f) = c.on_signal_state {
                let reason = cs(&reason);
                f(
                    c.user,
                    seyd_signal_state::SEYD_SIGNAL_DENIED,
                    reason.as_ptr(),
                );
            }
        }
        AgentEvent::NatReport(v) => {
            if let Some(f) = c.on_nat_report {
                let json = cs(&v.to_string());
                f(c.user, json.as_ptr());
            }
        }
    }
}
