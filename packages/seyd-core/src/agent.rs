//! Agent lifecycle: everything between a configuration and a running robot —
//! sockets, candidate discovery, certificate, QUIC endpoint, the session
//! [`Engine`], signaling, and the maintenance that keeps all of it alive
//! (port-mapping lease renewal, certificate rotation, network-change
//! re-gather).
//!
//! `seydd` and `seyd-ffi` are both *hosts* of this type. A host supplies media
//! (`push_video`, `push_message`), consumes [`AgentEvent`], and owns nothing of
//! the lifecycle itself — which is what keeps the daemon and the SDK from
//! drifting apart. Anything a host does differently (where frames come from,
//! where commands go, how the publisher is told to reconfigure) stays in the
//! host; anything that must be identical everywhere lives here.

use crate::channels::ChannelSpec;
use crate::engine::{Engine, EngineConfig, Event, Role, VideoFrame};
use seyd_signal_client::messages::{Announce, Candidate, ChannelInfo};
use seyd_transport::{Cert, Endpoint, TransportConfig};
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Notify};

/// How a robot is configured, independent of where that configuration came
/// from — `seydd.toml`, a C `seyd_config`, or a test.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// Ask for the cheapest useful recovery point (LTR → intra-refresh → IDR)
    /// instead of always demanding a keyframe. Turn off for a publisher that
    /// mishandles any `kind` but `idr`.
    pub recovery_ladder: bool,
    pub robot_id: String,
    pub signal_url: String,
    pub credential_path: PathBuf,
    pub quic_port: u16,
    pub ipv6: bool,
    pub port_mapping: bool,
    /// Skip discovery and advertise this host only (LAN testing).
    pub host_override: Option<String>,
    pub qos_profile: String,
    pub max_sessions: u32,
    pub channels: Vec<ChannelSpec>,
    /// Reported to the signal server, e.g. `seydd/0.1.0`.
    pub agent_version: String,
    /// Accept a session carried by the cloud relay when the pilot could not
    /// connect directly (ADR 0010). The relay is a last resort the pilot
    /// chooses after its candidate race fails; this only says whether the
    /// robot will serve it. Off means such pilots get a diagnosis instead.
    pub relay: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        AgentConfig {
            robot_id: String::new(),
            signal_url: String::new(),
            recovery_ladder: true,
            credential_path: PathBuf::from("/var/lib/seyd/robot.key"),
            quic_port: 4433,
            ipv6: true,
            port_mapping: true,
            host_override: None,
            qos_profile: "balanced".into(),
            max_sessions: 4,
            channels: Vec::new(),
            agent_version: concat!("seyd-core/", env!("CARGO_PKG_VERSION")).into(),
            relay: true,
        }
    }
}

/// What a running agent tells its host. The engine's own events pass through
/// unchanged; the rest describe the lifecycle the host no longer runs itself.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// A pilot session was admitted. Signaling has already been told.
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
        payload: bytes::Bytes,
    },
    /// Seyd's request to the publisher (`video-config`).
    RequestedConfig(serde_json::Value),
    /// A pilot needs a recovery point on this channel.
    RecoveryRequest {
        channel: u8,
        kind: &'static str,
        reason: &'static str,
    },
    /// The signaling connection came up or went down. Robots are unreachable
    /// while it is down, so a host with a UI should show it.
    SignalConnected,
    SignalDisconnected,
    /// Signaling refused authentication — almost always a misconfiguration.
    SignalDenied {
        reason: String,
    },
    /// The relayed simulcast layer changed (ADR 0008). A host that can force a
    /// keyframe on the incoming layer should do so — the engine switches on that
    /// layer's next keyframe, and asking makes it the next one.
    LayerChanged {
        channel: u8,
        layer: u8,
        name: String,
        reason: &'static str,
    },
    /// Candidates were (re-)gathered and announced. Carries the `NatReport`
    /// as JSON; the host may surface it or ignore it.
    NatReport(serde_json::Value),
}

/// A running robot agent. Cheap to clone; every clone refers to the same agent.
#[derive(Clone)]
pub struct Agent {
    engine: Engine,
    signal: seyd_signal_client::Handle,
    shutdown: Arc<Notify>,
    channels: Arc<Vec<ChannelSpec>>,
}

impl Agent {
    /// Bind, discover, announce, and start serving. Returns once the agent is
    /// announced and accepting sessions; the maintenance loop runs in the
    /// background until [`Agent::stop`] or the event receiver is dropped.
    pub async fn start(cfg: AgentConfig) -> anyhow::Result<(Agent, mpsc::Receiver<AgentEvent>)> {
        let profile = seyd_qos::get(&cfg.qos_profile)
            .ok_or_else(|| anyhow::anyhow!("unknown qos_profile {:?}", cfg.qos_profile))?;
        let specs = cfg.channels.clone();

        // ── sockets, discovery, certificate, transport ───────────────────
        let socks = seyd_nat::bind_sockets(cfg.quic_port, cfg.ipv6)?;
        // Clones share the fds, so a later re-gather runs STUN on the very
        // sockets QUIC is serving (never rebound) — same rule as at startup.
        let gather_socks: Vec<std::net::UdpSocket> = socks
            .iter()
            .map(|s| s.try_clone())
            .collect::<Result<_, _>>()?;
        let gathered = seyd_nat::gather(&socks, &gather_opts(&cfg)).await;
        for c in &gathered.candidates {
            tracing::info!(label = %c.label, priority = c.priority, probe = c.needs_probe, url = %c.url, "candidate");
        }
        tracing::info!(hint = ?gathered.p2p_hint, "p2p hint");
        let cert = Cert::generate(&gathered.san_ips)?;
        tracing::info!(fingerprint = %cert.fingerprint_hex(), days = cert.days_left() as u32, "certificate");
        let endpoint = Arc::new(Endpoint::bind(socks, &cert, TransportConfig::default())?);

        // ── engine ───────────────────────────────────────────────────────
        let (engine, events) = Engine::new(EngineConfig {
            recovery_ladder: cfg.recovery_ladder,
            channels: specs.clone(),
            profile,
            max_sessions: cfg.max_sessions as usize,
            chunk_len: seyd_wire::v2::DEFAULT_CHUNK_LEN,
        });
        tokio::spawn(engine.clone().accept_loop(endpoint.clone()));

        // ── signaling ────────────────────────────────────────────────────
        let identity = seyd_signal_client::Identity::load_or_create(&cfg.credential_path)?;
        tracing::info!(public_key = %identity.public_key_b64(), "robot identity");
        let (signal, signal_events) = seyd_signal_client::start(
            seyd_signal_client::Config {
                signal_url: cfg.signal_url.clone(),
                robot_id: cfg.robot_id.clone(),
                agent_version: cfg.agent_version.clone(),
                heartbeat: Duration::from_secs(5),
            },
            identity,
        );
        let mut fps = Fingerprints::new();
        signal
            .announce(build_announce(
                &gathered,
                fps.advertise(&cert),
                &specs,
                cfg.max_sessions,
                cfg.relay,
            )?)
            .await;

        let shutdown = Arc::new(Notify::new());
        let agent = Agent {
            engine: engine.clone(),
            signal: signal.clone(),
            shutdown: shutdown.clone(),
            channels: Arc::new(specs.clone()),
        };

        let (host_tx, host_rx) = mpsc::channel(256);
        tokio::spawn(maintain(
            Lifecycle {
                cfg,
                specs,
                gather_socks,
                gathered,
                cert,
                fps,
                engine,
                endpoint,
                signal,
                shutdown,
            },
            events,
            signal_events,
            host_tx,
        ));
        Ok((agent, host_rx))
    }

    /// Hand an encoded picture to the engine. Never blocks: a frame that finds
    /// the slot full is dropped by the engine's admission control.
    pub fn push_video(&self, channel: u8, frame: VideoFrame) {
        self.engine.push_video(channel, frame);
    }

    /// As `push_video`, for a channel publishing several simulcast layers
    /// (ADR 0008). Offer every layer's frames; the agent relays one and drops
    /// the rest, so switching costs a keyframe and never a reconnect. On a
    /// channel that declared no ladder this is `push_video` and `layer` is
    /// ignored.
    pub fn push_video_layer(&self, channel: u8, layer: u8, frame: VideoFrame) {
        self.engine.push_video_layer(channel, layer, frame);
    }

    /// Hand a message to a sensor channel.
    pub fn push_message(&self, channel: u8, payload: &[u8]) {
        self.engine.push_message(channel, payload);
    }

    pub fn channels(&self) -> &[ChannelSpec] {
        &self.channels
    }

    pub fn channel_by_name(&self, name: &str) -> Option<&ChannelSpec> {
        self.channels.iter().find(|c| c.name == name)
    }

    pub fn counters(&self) -> &crate::engine::Counters {
        self.engine.counters()
    }

    pub fn session_count(&self) -> usize {
        self.engine.session_count()
    }

    pub fn profile(&self) -> &'static seyd_qos::Profile {
        self.engine.profile()
    }

    /// The `video-config` a publisher should be sent for `channel` under the
    /// profile in force, with the bitrate clamped to what that channel's
    /// publisher declared it can produce (`ChannelSpec::max_bitrate_kbps`).
    pub fn publisher_config(&self, channel: u8, reason: &str) -> serde_json::Value {
        self.engine.publisher_config(channel, reason)
    }

    /// Switch the QoS ceiling. `false` if no profile by that name exists.
    pub fn set_qos_profile(&self, name: &str, reason: &str) -> bool {
        match seyd_qos::get(name) {
            Some(p) => {
                self.engine.set_profile(p, reason);
                true
            }
            None => false,
        }
    }

    /// Free-form robot status, forwarded to the console through signaling.
    pub async fn set_status(&self, status: Option<serde_json::Value>) {
        self.signal.set_status(status).await;
    }

    pub fn signal_connected(&self) -> bool {
        self.signal.is_connected()
    }

    /// Stop serving. The maintenance loop exits and the endpoint closes;
    /// in-flight sessions are torn down.
    pub fn stop(&self) {
        // `notify_one`, not `notify_waiters`: the maintenance loop may be busy
        // in another `select!` branch, and a stop that arrived then must be
        // remembered rather than dropped.
        self.shutdown.notify_one();
    }
}

// ── maintenance ──────────────────────────────────────────────────────────

/// The mutable half of a running agent, owned solely by [`maintain`].
struct Lifecycle {
    cfg: AgentConfig,
    specs: Vec<ChannelSpec>,
    gather_socks: Vec<std::net::UdpSocket>,
    gathered: seyd_nat::Gathered,
    cert: Cert,
    fps: Fingerprints,
    engine: Engine,
    endpoint: Arc<Endpoint>,
    signal: seyd_signal_client::Handle,
    shutdown: Arc<Notify>,
}

enum Maint {
    NetworkChanged(String),
    RotateCheck,
    RotateNow,
}

/// The agent's one event loop: engine events and signaling events in, host
/// events out, with lease renewal, certificate rotation and re-gather on the
/// side. Exits on [`Agent::stop`] or when the host drops its receiver.
async fn maintain(
    mut lc: Lifecycle,
    mut events: mpsc::Receiver<Event>,
    mut signal_events: mpsc::Receiver<seyd_signal_client::Event>,
    host: mpsc::Sender<AgentEvent>,
) {
    let (maint_tx, mut maint_rx) = mpsc::channel::<Maint>(8);
    let (mapping_tx, mapping_rx) = tokio::sync::watch::channel(lc.gathered.mapping.clone());
    tokio::spawn(renew_loop(mapping_rx, maint_tx.clone()));
    {
        // Interface changes → re-gather. The watcher's own force sender is
        // unused here; renewal failures go through the maint channel directly.
        let tx = maint_tx.clone();
        let mut watch = seyd_nat::watch_network_changes();
        tokio::spawn(async move {
            while let Some(reason) = watch.events.recv().await {
                if tx.send(Maint::NetworkChanged(reason)).await.is_err() {
                    return;
                }
            }
        });
    }
    {
        let tx = maint_tx;
        tokio::spawn(async move {
            // Test hook: force a rotation shortly after startup.
            if let Some(secs) = std::env::var("SEYD_CERT_ROTATE_AFTER_SECS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
            {
                tokio::time::sleep(Duration::from_secs(secs)).await;
                let _ = tx.send(Maint::RotateNow).await;
            }
            loop {
                tokio::time::sleep(Duration::from_secs(3600)).await;
                if tx.send(Maint::RotateCheck).await.is_err() {
                    return;
                }
            }
        });
    }
    let mut last_regather = Instant::now();

    loop {
        tokio::select! {
            ev = signal_events.recv() => {
                let Some(ev) = ev else { break };
                match ev {
                    seyd_signal_client::Event::PilotConnecting { session_id, pilot_ip, role, .. } => {
                        let role = match role { seyd_signal_client::messages::Role::Driver => Role::Driver, _ => Role::Observer };
                        lc.engine.expect_pilot(&session_id, role);
                        probe(&lc.endpoint, pilot_ip);
                    }
                    seyd_signal_client::Event::Punch { pilot_ip, .. } => probe(&lc.endpoint, pilot_ip),
                    seyd_signal_client::Event::SessionRevoked { session_id, reason } => lc.engine.revoke(&session_id, &reason),
                    seyd_signal_client::Event::RelayOpen { session_id, url, token, role, pilot_ip } => {
                        if lc.cfg.relay {
                            let role = match role { seyd_signal_client::messages::Role::Driver => Role::Driver, _ => Role::Observer };
                            lc.engine.expect_pilot(&session_id, role);
                            tokio::spawn(open_relay(lc.engine.clone(), session_id, url, token, pilot_ip));
                        } else {
                            tracing::warn!(session = %session_id, "relay-open ignored: relay disabled in config");
                        }
                    }
                    seyd_signal_client::Event::Denied { reason } => {
                        tracing::error!(%reason, "signal denied");
                        if host.send(AgentEvent::SignalDenied { reason }).await.is_err() { break }
                    }
                    seyd_signal_client::Event::Connected => {
                        if host.send(AgentEvent::SignalConnected).await.is_err() { break }
                    }
                    seyd_signal_client::Event::Disconnected => {
                        if host.send(AgentEvent::SignalDisconnected).await.is_err() { break }
                    }
                }
            }
            ev = events.recv() => {
                let Some(ev) = ev else { break };
                // Signaling is the agent's business; the host only learns what happened.
                let out = match ev {
                    Event::SessionStarted { signal_id, role, path_label } => {
                        lc.signal.session_accepted(&signal_id, &path_label);
                        lc.signal.set_sessions(lc.engine.signal_session_ids()).await;
                        AgentEvent::SessionStarted { signal_id, role, path_label }
                    }
                    Event::SessionEnded { signal_id, reason } => {
                        lc.signal.session_ended(&signal_id, &reason);
                        lc.signal.set_sessions(lc.engine.signal_session_ids()).await;
                        AgentEvent::SessionEnded { signal_id, reason }
                    }
                    Event::Command { channel, payload } => AgentEvent::Command { channel, payload },
                    Event::RequestedConfig(v) => AgentEvent::RequestedConfig(v),
                    Event::RecoveryRequest { channel, kind, reason } => AgentEvent::RecoveryRequest { channel, kind, reason },
                    Event::LayerChanged { channel, layer, name, reason } => AgentEvent::LayerChanged { channel, layer, name, reason },
                };
                if host.send(out).await.is_err() { break }
            }
            ev = maint_rx.recv() => {
                let Some(ev) = ev else { break };
                if !handle_maint(&mut lc, ev, &mut last_regather, &mapping_tx, &host).await { break }
            }
            _ = lc.shutdown.notified() => { tracing::info!("agent stopping"); break; }
        }
    }
    lc.endpoint.close();
}

/// One maintenance event. Returns `false` when the host is gone.
async fn handle_maint(
    lc: &mut Lifecycle,
    ev: Maint,
    last_regather: &mut Instant,
    mapping_tx: &tokio::sync::watch::Sender<Option<seyd_nat::PortMapping>>,
    host: &mpsc::Sender<AgentEvent>,
) -> bool {
    let rotate_due = match &ev {
        Maint::RotateNow => true,
        Maint::RotateCheck => lc.cert.days_left() < 3.0,
        Maint::NetworkChanged(_) => false,
    };
    if let Maint::NetworkChanged(reason) = &ev {
        if last_regather.elapsed() < Duration::from_secs(30) {
            tracing::debug!(%reason, "network change ignored (rate-limited)");
        } else {
            *last_regather = Instant::now();
            tracing::info!(%reason, "network change — re-gathering candidates");
            let new_g = seyd_nat::gather(&lc.gather_socks, &gather_opts(&lc.cfg)).await;
            let san_changed = {
                let a: std::collections::BTreeSet<&IpAddr> = lc.gathered.san_ips.iter().collect();
                let b: std::collections::BTreeSet<&IpAddr> = new_g.san_ips.iter().collect();
                a != b
            };
            for c in &new_g.candidates {
                tracing::info!(label = %c.label, priority = c.priority, url = %c.url, "candidate");
            }
            lc.gathered = new_g;
            let _ = mapping_tx.send(lc.gathered.mapping.clone());
            if san_changed {
                // New addresses must be in the certificate's SAN or Chrome
                // rejects the handshake.
                if let Err(e) = do_rotate(
                    &mut lc.cert,
                    &mut lc.fps,
                    &lc.endpoint,
                    &lc.gathered.san_ips,
                ) {
                    tracing::error!(error = %e, "certificate rotation after network change failed");
                }
            }
            if !announce(lc, host).await {
                return false;
            }
        }
    }
    if rotate_due {
        match do_rotate(
            &mut lc.cert,
            &mut lc.fps,
            &lc.endpoint,
            &lc.gathered.san_ips,
        ) {
            Ok(()) => return announce(lc, host).await,
            Err(e) => tracing::error!(error = %e, "certificate rotation failed"),
        }
    }
    true
}

/// Publish the current candidates and fingerprints, and tell the host.
/// Returns `false` when the host is gone.
async fn announce(lc: &mut Lifecycle, host: &mpsc::Sender<AgentEvent>) -> bool {
    let fingerprints = lc.fps.advertise(&lc.cert);
    match build_announce(
        &lc.gathered,
        fingerprints,
        &lc.specs,
        lc.cfg.max_sessions,
        lc.cfg.relay,
    ) {
        Ok(a) => {
            let report = a.nat_report.clone();
            lc.signal.announce(a).await;
            host.send(AgentEvent::NatReport(report)).await.is_ok()
        }
        Err(e) => {
            tracing::error!(error = %e, "building announce failed");
            true
        }
    }
}

/// Nudge an address-restricted NAT open toward a pilot we were told about.
fn probe(endpoint: &Endpoint, ip: Option<String>) {
    let Some(ip) = ip.and_then(|s| s.parse::<IpAddr>().ok()) else {
        return;
    };
    if let Some(idx) = endpoint.socket_index_for(ip) {
        if let Some(p) = endpoint.prober(idx) {
            // Chrome's QUIC source port is unknowable; the probe only helps
            // address-restricted NATs, where the port is irrelevant.
            p.start_default(std::net::SocketAddr::new(ip, 443));
        }
    }
}

fn gather_opts(cfg: &AgentConfig) -> seyd_nat::GatherOpts {
    seyd_nat::GatherOpts {
        port: cfg.quic_port,
        host_override: cfg.host_override.as_deref().and_then(|h| h.parse().ok()),
        port_mapping: cfg.port_mapping,
        ipv6: cfg.ipv6,
    }
}

/// Which certificate fingerprints to advertise. After a rotation the outgoing
/// certificate stays advertised until it expires, so a pilot holding an older
/// offer can still connect.
#[derive(Default)]
struct Fingerprints {
    old: Option<(String, Instant)>,
}

impl Fingerprints {
    fn new() -> Self {
        Fingerprints::default()
    }

    fn advertise(&mut self, cert: &Cert) -> Vec<String> {
        if let Some((_, expires)) = &self.old {
            if Instant::now() >= *expires {
                self.old = None;
            }
        }
        let mut v = vec![cert.fingerprint_hex()];
        if let Some((fp, _)) = &self.old {
            v.push(fp.clone());
        }
        v
    }

    fn retire(&mut self, cert: &Cert) {
        let remaining = cert.days_left() * 86_400.0;
        if remaining > 0.0 {
            self.old = Some((
                cert.fingerprint_hex(),
                Instant::now() + Duration::from_secs_f64(remaining),
            ));
        }
    }
}

/// Dial the cloud relay for one session and hand it to the engine (ADR 0010).
/// Runs on its own task so a slow relay handshake never stalls signaling.
async fn open_relay(
    engine: Engine,
    session_id: String,
    url: String,
    token: String,
    pilot_ip: Option<String>,
) {
    let id = RELAY_IDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    tracing::info!(session = %session_id, %url, pilot_ip = ?pilot_ip, "pilot on the relay; dialling");
    match seyd_transport::Session::relay(
        id,
        &url,
        &session_id,
        &token,
        TransportConfig::default().datagram_send_buffer,
    )
    .await
    {
        Ok(session) => engine.serve(session),
        Err(e) => tracing::warn!(session = %session_id, error = %e, "relay attach failed"),
    }
}

/// Session ids for relayed sessions: the endpoint numbers its own from 1, so
/// these start high enough never to collide.
static RELAY_IDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1 << 40);

fn build_announce(
    gathered: &seyd_nat::Gathered,
    cert_fingerprints: Vec<String>,
    specs: &[ChannelSpec],
    max_sessions: u32,
    relay: bool,
) -> anyhow::Result<Announce> {
    Ok(Announce {
        candidates: gathered
            .candidates
            .iter()
            .map(|c| Candidate {
                url: c.url.clone(),
                label: c.label.clone(),
                priority: c.priority,
                needs_probe: c.needs_probe,
                family: c.family,
            })
            .collect(),
        cert_fingerprints,
        alpns: vec!["h3".into()],
        nat_report: serde_json::to_value(&gathered.nat_report)?,
        channels: specs
            .iter()
            .map(|s| ChannelInfo {
                id: s.id,
                kind: serde_json::to_value(s.kind)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_string(),
                name: s.name.clone(),
                codec: s.codec.clone(),
                fps: s.fps,
            })
            .collect(),
        p2p_hint: format!("{:?}", gathered.p2p_hint)
            .to_lowercase()
            .replace("lanonly", "lan-only"),
        max_sessions,
        relay,
    })
}

/// Generate and install a fresh certificate; the outgoing one stays advertised
/// until it expires (see [`Fingerprints`]).
fn do_rotate(
    cert: &mut Cert,
    fps: &mut Fingerprints,
    endpoint: &Endpoint,
    san_ips: &[IpAddr],
) -> anyhow::Result<()> {
    let new_cert = Cert::generate(san_ips)?;
    endpoint.rotate(&new_cert)?;
    fps.retire(cert);
    tracing::info!(new = %new_cert.fingerprint_hex(), old = %cert.fingerprint_hex(),
                   days = new_cert.days_left() as u32, "certificate rotated");
    *cert = new_cert;
    Ok(())
}

/// Renew the router port mapping at half its lease. A moved external address
/// or three consecutive failures is treated as a network change; the task then
/// waits for the re-gather to publish a fresh mapping.
async fn renew_loop(
    mut rx: tokio::sync::watch::Receiver<Option<seyd_nat::PortMapping>>,
    maint: mpsc::Sender<Maint>,
) {
    let mut failures = 0u32;
    loop {
        let current = rx.borrow_and_update().clone();
        let Some(mut m) = current else {
            if rx.changed().await.is_err() {
                return;
            }
            failures = 0;
            continue;
        };
        if m.lifetime_s == 0 {
            // The router calls it permanent; nothing to renew.
            if rx.changed().await.is_err() {
                return;
            }
            failures = 0;
            continue;
        }
        loop {
            let wait = Duration::from_secs(u64::from((m.lifetime_s / 2).max(60)));
            tokio::select! {
                r = rx.changed() => {
                    if r.is_err() { return; }
                    failures = 0;
                    break;
                }
                _ = tokio::time::sleep(wait) => {
                    match seyd_nat::renew(&m).await {
                        Some(newm) => {
                            failures = 0;
                            if newm.external_ip != m.external_ip || newm.external_port != m.external_port {
                                tracing::warn!(
                                    old = %format!("{}:{}", m.external_ip, m.external_port),
                                    new = %format!("{}:{}", newm.external_ip, newm.external_port),
                                    "port mapping moved — treating as network change");
                                let _ = maint.send(Maint::NetworkChanged("port mapping external address changed".into())).await;
                                if rx.changed().await.is_err() { return; }
                                break;
                            }
                            tracing::debug!(lease_s = newm.lifetime_s, "port mapping renewed");
                            m = newm;
                        }
                        None => {
                            failures += 1;
                            tracing::warn!(failures, "port mapping renewal failed");
                            if failures >= 3 {
                                failures = 0;
                                let _ = maint.send(Maint::NetworkChanged("port mapping renewal failed 3 times".into())).await;
                                if rx.changed().await.is_err() { return; }
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_carry_old_until_expiry() {
        let cert = Cert::generate(&["127.0.0.1".parse().unwrap()]).unwrap();
        let newer = Cert::generate(&["127.0.0.1".parse().unwrap()]).unwrap();
        // Old still valid → both advertised, new first.
        let mut fps = Fingerprints::new();
        fps.retire(&cert);
        assert_eq!(
            fps.advertise(&newer),
            vec![newer.fingerprint_hex(), cert.fingerprint_hex()]
        );
        // Old expired → pruned.
        let mut fps = Fingerprints {
            old: Some((
                cert.fingerprint_hex(),
                Instant::now() - Duration::from_secs(1),
            )),
        };
        assert_eq!(fps.advertise(&newer), vec![newer.fingerprint_hex()]);
        assert!(fps.old.is_none());
    }
}
