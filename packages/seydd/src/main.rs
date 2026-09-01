//! seydd — the Seyd robot daemon. See docs/protocol/seydd.md.

mod config;
mod input;
mod publisher_control;

use clap::Parser;
use config::{ChannelKind, Config};
use seyd_core::channels::{ChannelKind as CoreKind, ChannelSpec};
use seyd_core::engine::{Engine, EngineConfig, Event, Role, VideoFrame};
use seyd_signal_client::messages::{Announce, Candidate, ChannelInfo};
use seyd_transport::{Cert, Endpoint, TransportConfig};
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Path to seydd.toml
    #[arg(long, default_value = "/etc/seyd/seydd.toml")]
    config: PathBuf,
    /// Open the video inputs, print frame statistics for N seconds, exit.
    #[arg(long, value_name = "SECONDS")]
    probe_input: Option<u64>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse()?),
        )
        .init();
    let cli = Cli::parse();
    let cfg = Config::load(&cli.config)?;
    tracing::info!(robot_id = %cfg.agent.robot_id, channels = cfg.channels.len(), version = env!("CARGO_PKG_VERSION"), "seydd starting");

    if let Some(secs) = cli.probe_input {
        return probe_input(&cfg, secs).await;
    }
    run(cfg).await
}

async fn run(cfg: Config) -> anyhow::Result<()> {
    // ── channels ─────────────────────────────────────────────────────────
    let specs: Vec<ChannelSpec> = cfg
        .channels
        .iter()
        .enumerate()
        .map(|(i, c)| ChannelSpec {
            id: (i + 1) as u8,
            kind: match c.kind {
                ChannelKind::Video => CoreKind::Video,
                ChannelKind::Sensor => CoreKind::Sensor,
                ChannelKind::Command => CoreKind::Command,
            },
            name: c.name.clone(),
            codec: c.codec.clone(),
            fps: c.fps,
        })
        .collect();
    let profile = seyd_qos::get(&cfg.agent.qos_profile).unwrap_or(seyd_qos::DEFAULT);

    // ── sockets, discovery, certificate, transport ───────────────────────
    let socks = seyd_nat::bind_sockets(cfg.agent.quic_port, cfg.agent.ipv6)?;
    // Clones share the fds, so a later re-gather runs STUN on the very
    // sockets QUIC is serving (never rebound) — same rule as at startup.
    let gather_socks: Vec<std::net::UdpSocket> = socks
        .iter()
        .map(|s| s.try_clone())
        .collect::<Result<_, _>>()?;
    let mut gathered = seyd_nat::gather(&socks, &mk_gather_opts(&cfg)).await;
    for c in &gathered.candidates {
        tracing::info!(label = %c.label, priority = c.priority, probe = c.needs_probe, url = %c.url, "candidate");
    }
    tracing::info!(hint = ?gathered.p2p_hint, "p2p hint");
    let mut cert = Cert::generate(&gathered.san_ips)?;
    let mut old_fp: Option<(String, Instant)> = None;
    tracing::info!(fingerprint = %cert.fingerprint_hex(), days = cert.days_left() as u32, "certificate");
    let endpoint = Arc::new(Endpoint::bind(socks, &cert, TransportConfig::default())?);

    // ── engine ───────────────────────────────────────────────────────────
    let (engine, mut events) = Engine::new(EngineConfig {
        channels: specs.clone(),
        profile,
        max_sessions: cfg.agent.max_sessions as usize,
        chunk_len: seyd_wire::v2::DEFAULT_CHUNK_LEN,
    });
    tokio::spawn(engine.clone().accept_loop(endpoint.clone()));

    // ── inputs ───────────────────────────────────────────────────────────
    for spec in &specs {
        let c = &cfg.channels[(spec.id - 1) as usize];
        match spec.kind {
            CoreKind::Video => {
                let (tx, mut rx) = tokio::sync::mpsc::channel(4);
                input::spawn_video(
                    input::rtsp::resolve_credentials(c.input.as_deref().unwrap()),
                    tx,
                )?;
                let eng = engine.clone();
                let id = spec.id;
                tokio::spawn(async move {
                    while let Some(au) = rx.recv().await {
                        eng.push_video(
                            id,
                            VideoFrame {
                                data: au.data,
                                keyframe: au.keyframe,
                                capture_ts_us: au.capture_ts_us,
                            },
                        );
                    }
                });
            }
            CoreKind::Sensor => {
                let (tx, mut rx) = tokio::sync::mpsc::channel(64);
                tokio::spawn(input::udp::run(
                    input::udp::addr_from_url(c.input.as_deref().unwrap())?,
                    tx,
                ));
                let eng = engine.clone();
                let id = spec.id;
                tokio::spawn(async move {
                    while let Some(m) = rx.recv().await {
                        eng.push_message(id, &m);
                    }
                });
            }
            CoreKind::Command => {}
        }
    }

    // ── outputs: command UDP sinks and publisher control ─────────────────
    let out_sock = std::net::UdpSocket::bind("0.0.0.0:0")?;
    out_sock.set_nonblocking(true)?;
    let mut command_targets: HashMap<u8, String> = HashMap::new();
    for spec in specs.iter().filter(|s| s.kind == CoreKind::Command) {
        let c = &cfg.channels[(spec.id - 1) as usize];
        command_targets.insert(
            spec.id,
            input::udp::addr_from_url(c.output.as_deref().unwrap())?,
        );
    }
    let publisher = publisher_control::PublisherControl::new(
        cfg.publisher_control.as_ref().map(|p| p.udp.as_str()),
    );
    if publisher.enabled() {
        for spec in specs.iter().filter(|s| s.kind == CoreKind::Video) {
            publisher.send(&profile.publisher_config(spec.id, "profile"));
        }
    }

    // ── signaling ────────────────────────────────────────────────────────
    let identity = seyd_signal_client::Identity::load_or_create(&cfg.agent.credential_path)?;
    tracing::info!(public_key = %identity.public_key_b64(), "robot identity");
    let (signal, mut signal_events) = seyd_signal_client::start(
        seyd_signal_client::Config {
            signal_url: cfg.agent.signal_url.clone(),
            robot_id: cfg.agent.robot_id.clone(),
            agent_version: format!("seydd/{}", env!("CARGO_PKG_VERSION")),
            heartbeat: Duration::from_secs(5),
        },
        identity,
    );
    signal
        .announce(build_announce(
            &gathered,
            current_fingerprints(&cert, &mut old_fp),
            &specs,
            cfg.agent.max_sessions,
        )?)
        .await;

    // ── maintenance: lease renewal, cert rotation, network watch ─────────
    let (maint_tx, mut maint_rx) = tokio::sync::mpsc::channel::<Maint>(8);
    let (mapping_tx, mapping_rx) = tokio::sync::watch::channel(gathered.mapping.clone());
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
        let tx = maint_tx.clone();
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

    // ── event loops ──────────────────────────────────────────────────────
    let probe = |ip: Option<String>| {
        let Some(ip) = ip.and_then(|s| s.parse::<std::net::IpAddr>().ok()) else {
            return;
        };
        if let Some(idx) = endpoint.socket_index_for(ip) {
            if let Some(p) = endpoint.prober(idx) {
                // Chrome's QUIC source port is unknowable; the probe only helps
                // address-restricted NATs, where the port is irrelevant.
                p.start_default(std::net::SocketAddr::new(ip, 443));
            }
        }
    };

    loop {
        tokio::select! {
            ev = signal_events.recv() => {
                let Some(ev) = ev else { break };
                match ev {
                    seyd_signal_client::Event::PilotConnecting { session_id, pilot_ip, role, .. } => {
                        let role = match role { seyd_signal_client::messages::Role::Driver => Role::Driver, _ => Role::Observer };
                        engine.expect_pilot(&session_id, role);
                        probe(pilot_ip);
                    }
                    seyd_signal_client::Event::Punch { pilot_ip, .. } => probe(pilot_ip),
                    seyd_signal_client::Event::SessionRevoked { session_id, reason } => engine.revoke(&session_id, &reason),
                    seyd_signal_client::Event::Denied { reason } => tracing::error!(%reason, "signal denied"),
                    seyd_signal_client::Event::Connected | seyd_signal_client::Event::Disconnected => {}
                }
            }
            ev = events.recv() => {
                let Some(ev) = ev else { break };
                match ev {
                    Event::SessionStarted { signal_id, role, path_label } => {
                        signal.session_accepted(&signal_id, &path_label);
                        signal.set_sessions(engine.signal_session_ids()).await;
                        publisher.send(&serde_json::json!({"type":"session","state":"started","session_id":signal_id,"role":role.as_str(),"sessions":engine.session_count()}));
                    }
                    Event::SessionEnded { signal_id, reason } => {
                        signal.session_ended(&signal_id, &reason);
                        signal.set_sessions(engine.signal_session_ids()).await;
                        publisher.send(&serde_json::json!({"type":"session","state":"ended","session_id":signal_id,"sessions":engine.session_count()}));
                    }
                    Event::Command { channel, payload } => {
                        if let Some(t) = command_targets.get(&channel) {
                            let _ = out_sock.send_to(&payload, t);
                        }
                    }
                    Event::RequestedConfig(v) => { publisher.send(&v); }
                    Event::RecoveryRequest { channel, kind, reason } => {
                        publisher.send(&serde_json::json!({"type":"recovery-request","channel":channel,"kind":kind,"reason":reason}));
                    }
                }
            }
            ev = maint_rx.recv() => {
                let Some(ev) = ev else { break };
                let rotate_due = match &ev {
                    Maint::RotateNow => true,
                    Maint::RotateCheck => cert.days_left() < 3.0,
                    Maint::NetworkChanged(_) => false,
                };
                if let Maint::NetworkChanged(reason) = &ev {
                    if last_regather.elapsed() < Duration::from_secs(30) {
                        tracing::debug!(%reason, "network change ignored (rate-limited)");
                    } else {
                        last_regather = Instant::now();
                        tracing::info!(%reason, "network change — re-gathering candidates");
                        let new_g = seyd_nat::gather(&gather_socks, &mk_gather_opts(&cfg)).await;
                        let san_changed = {
                            let a: std::collections::BTreeSet<&IpAddr> = gathered.san_ips.iter().collect();
                            let b: std::collections::BTreeSet<&IpAddr> = new_g.san_ips.iter().collect();
                            a != b
                        };
                        for c in &new_g.candidates {
                            tracing::info!(label = %c.label, priority = c.priority, url = %c.url, "candidate");
                        }
                        gathered = new_g;
                        let _ = mapping_tx.send(gathered.mapping.clone());
                        if san_changed {
                            // New addresses must be in the certificate's SAN
                            // or Chrome rejects the handshake.
                            if let Err(e) = do_rotate(&mut cert, &mut old_fp, &endpoint, &gathered.san_ips) {
                                tracing::error!(error = %e, "certificate rotation after network change failed");
                            }
                        }
                        signal.announce(build_announce(&gathered, current_fingerprints(&cert, &mut old_fp), &specs, cfg.agent.max_sessions)?).await;
                    }
                }
                if rotate_due {
                    match do_rotate(&mut cert, &mut old_fp, &endpoint, &gathered.san_ips) {
                        Ok(()) => {
                            signal.announce(build_announce(&gathered, current_fingerprints(&cert, &mut old_fp), &specs, cfg.agent.max_sessions)?).await;
                        }
                        Err(e) => tracing::error!(error = %e, "certificate rotation failed"),
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => { tracing::info!("shutting down"); break; }
        }
    }
    endpoint.close();
    Ok(())
}

async fn probe_input(cfg: &Config, secs: u64) -> anyhow::Result<()> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    for c in cfg.channels.iter().filter(|c| c.kind == ChannelKind::Video) {
        let url = input::rtsp::resolve_credentials(c.input.as_deref().unwrap());
        input::spawn_video(url, tx.clone())?;
    }
    drop(tx);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    let (mut frames, mut keyframes, mut bytes, mut loss) = (0u64, 0u64, 0u64, 0u64);
    let mut last_report = tokio::time::Instant::now();
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            au = rx.recv() => {
                let Some(au) = au else { break };
                frames += 1;
                bytes += au.data.len() as u64;
                loss += au.input_loss as u64;
                if au.keyframe { keyframes += 1; }
                if last_report.elapsed().as_secs() >= 1 {
                    tracing::info!(frames, keyframes, kbps = bytes * 8 / 1000 / last_report.elapsed().as_secs().max(1), input_loss = loss, "probe");
                    last_report = tokio::time::Instant::now();
                    bytes = 0;
                }
            }
        }
    }
    println!(
        "probe: {frames} frames, {keyframes} keyframes, {loss} input packets lost over {secs}s"
    );
    if frames == 0 {
        anyhow::bail!("no frames received");
    }
    Ok(())
}

// ── maintenance plumbing ─────────────────────────────────────────────────

enum Maint {
    NetworkChanged(String),
    RotateCheck,
    RotateNow,
}

fn mk_gather_opts(cfg: &Config) -> seyd_nat::GatherOpts {
    seyd_nat::GatherOpts {
        port: cfg.agent.quic_port,
        host_override: cfg
            .agent
            .host_override
            .as_deref()
            .and_then(|h| h.parse().ok()),
        port_mapping: cfg.agent.port_mapping,
        ipv6: cfg.agent.ipv6,
    }
}

/// The fingerprints to advertise: the current cert, plus the previous one
/// while it is still valid so pilots holding an older offer can connect.
fn current_fingerprints(cert: &Cert, old: &mut Option<(String, Instant)>) -> Vec<String> {
    if let Some((_, expires)) = old {
        if Instant::now() >= *expires {
            *old = None;
        }
    }
    let mut v = vec![cert.fingerprint_hex()];
    if let Some((fp, _)) = old {
        v.push(fp.clone());
    }
    v
}

fn build_announce(
    gathered: &seyd_nat::Gathered,
    cert_fingerprints: Vec<String>,
    specs: &[ChannelSpec],
    max_sessions: u32,
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
    })
}

/// Generate and install a fresh certificate; the outgoing one stays
/// advertised until it expires (see [`current_fingerprints`]).
fn do_rotate(
    cert: &mut Cert,
    old: &mut Option<(String, Instant)>,
    endpoint: &Endpoint,
    san_ips: &[IpAddr],
) -> anyhow::Result<()> {
    let new_cert = Cert::generate(san_ips)?;
    endpoint.rotate(&new_cert)?;
    let remaining = cert.days_left() * 86_400.0;
    if remaining > 0.0 {
        *old = Some((
            cert.fingerprint_hex(),
            Instant::now() + Duration::from_secs_f64(remaining),
        ));
    }
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
    maint: tokio::sync::mpsc::Sender<Maint>,
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
mod maint_tests {
    use super::*;

    #[test]
    fn fingerprints_carry_old_until_expiry() {
        let cert = Cert::generate(&["127.0.0.1".parse().unwrap()]).unwrap();
        let newer = Cert::generate(&["127.0.0.1".parse().unwrap()]).unwrap();
        // Old still valid → both advertised, new first.
        let mut old = Some((
            cert.fingerprint_hex(),
            Instant::now() + Duration::from_secs(60),
        ));
        let fps = current_fingerprints(&newer, &mut old);
        assert_eq!(fps, vec![newer.fingerprint_hex(), cert.fingerprint_hex()]);
        // Old expired → pruned.
        let mut old = Some((
            cert.fingerprint_hex(),
            Instant::now() - Duration::from_secs(1),
        ));
        let fps = current_fingerprints(&newer, &mut old);
        assert_eq!(fps, vec![newer.fingerprint_hex()]);
        assert!(old.is_none());
    }
}
