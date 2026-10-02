//! seydd — the Seyd robot daemon. See docs/protocol/seydd.md.
//!
//! The daemon is a *host* of [`seyd_core::Agent`] (ADR 0004): the agent owns
//! discovery, transport, sessions, signaling and certificate maintenance, and
//! seydd supplies the parts that are specific to running as a daemon — TOML
//! configuration, RTSP/RTP and UDP inputs, UDP command sinks, and the UDP
//! publisher-control channel. `seyd-ffi` hosts the same agent differently.

mod config;
mod enrol;
mod input;
mod publisher_control;

use clap::Parser;
use config::{ChannelKind, Config};
use seyd_core::channels::{ChannelKind as CoreKind, ChannelSpec};
use seyd_core::{Agent, AgentConfig, AgentEvent, VideoFrame};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Path to seydd.toml
    #[arg(long, default_value = "/etc/seyd/seydd.toml")]
    config: PathBuf,
    /// Open the video inputs, print frame statistics for N seconds, exit.
    #[arg(long, value_name = "SECONDS")]
    probe_input: Option<u64>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Redeem an enrolment token from the console, registering this robot.
    Enrol {
        /// The token shown once when the token was created.
        #[arg(long)]
        token: String,
        /// Override the signal server from the config file.
        #[arg(long)]
        signal_url: Option<String>,
    },
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

    if let Some(Command::Enrol { token, signal_url }) = &cli.command {
        let url = signal_url.as_deref().unwrap_or(&cfg.agent.signal_url);
        return enrol::run(&cfg.agent.robot_id, url, &cfg.agent.credential_path, token).await;
    }

    tracing::info!(robot_id = %cfg.agent.robot_id, channels = cfg.channels.len(), version = env!("CARGO_PKG_VERSION"), "seydd starting");

    // A robot with no channels connects, announces nothing and looks healthy
    // in the console — there is no media or telemetry for a pilot to receive.
    // The usual cause is the section being spelled `[[channels]]`: the key is
    // `channel`, and serde's `default` turns the typo into an empty list
    // rather than an error.
    if cfg.channels.is_empty() {
        tracing::warn!(
            config = %cli.config.display(),
            "no channels configured — this robot will announce nothing and a pilot will \
             receive no media. Channel sections are spelled [[channel]], not [[channels]]."
        );
    }

    if let Some(secs) = cli.probe_input {
        return probe_input(&cfg, secs).await;
    }
    run(cfg).await
}

/// Redeem an enrolment token on start, so provisioning a robot is one step
/// rather than two.
///
/// This deliberately also runs when a credential already exists. Enrolment
/// registers the *public* half of whatever key the robot holds, so a robot that
/// predates enrolment — one that joined a dev server by trust-on-first-use —
/// keeps its identity and simply becomes known. Requiring a missing credential
/// stranded exactly those robots: they connected fine and the server answered
/// `unknown-robot` with nothing to do about it.
///
/// With a credential already present the attempt is best-effort: a single-use
/// token left in the config or environment will fail on the second start, and
/// that must not stop a robot that is already enrolled from running.
async fn enrol_if_needed(cfg: &Config) -> anyhow::Result<()> {
    let Some(token) = cfg.agent.enrolment_token() else {
        return Ok(());
    };
    let first_run = !cfg.agent.credential_path.exists();
    let result = enrol::run(
        &cfg.agent.robot_id,
        &cfg.agent.signal_url,
        &cfg.agent.credential_path,
        &token,
    )
    .await;

    match result {
        Ok(()) => Ok(()),
        // No credential yet means this robot cannot connect at all without
        // enrolling, so the failure is fatal and worth stopping for.
        Err(e) if first_run => Err(e),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "enrolment did not succeed, continuing with the existing credential — \
                 expected if this robot is already enrolled and the token was spent"
            );
            Ok(())
        }
    }
}

async fn run(cfg: Config) -> anyhow::Result<()> {
    enrol_if_needed(&cfg).await?;

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
            layers: c.layer_specs(),
            max_bitrate_kbps: c.max_bitrate_kbps,
        })
        .collect();

    let (agent, mut events) = Agent::start(AgentConfig {
        recovery_ladder: cfg.agent.recovery_ladder,
        robot_id: cfg.agent.robot_id.clone(),
        signal_url: cfg.agent.signal_url.clone(),
        credential_path: cfg.agent.credential_path.clone(),
        quic_port: cfg.agent.quic_port,
        ipv6: cfg.agent.ipv6,
        port_mapping: cfg.agent.port_mapping,
        host_override: cfg.agent.host_override.clone(),
        qos_profile: cfg.agent.qos_profile.clone(),
        max_sessions: cfg.agent.max_sessions,
        channels: specs.clone(),
        agent_version: format!("seydd/{}", env!("CARGO_PKG_VERSION")),
        relay: cfg.agent.relay,
    })
    .await?;

    // ── inputs ───────────────────────────────────────────────────────────
    for spec in &specs {
        let c = &cfg.channels[(spec.id - 1) as usize];
        match spec.kind {
            CoreKind::Video => {
                // One input per simulcast layer (ADR 0008); a single-`input`
                // channel yields exactly one at layer 0. Every layer's frames
                // are offered and the engine relays one, so the alternative is
                // always encoded and one keyframe away.
                let codec = input::Codec::from_codec_string(&c.codec)?;
                for (layer, url) in c.inputs() {
                    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
                    input::spawn_video(input::rtsp::resolve_credentials(&url), codec, tx)?;
                    let agent = agent.clone();
                    let id = spec.id;
                    tokio::spawn(async move {
                        while let Some(au) = rx.recv().await {
                            agent.push_video_layer(
                                id,
                                layer,
                                VideoFrame {
                                    data: au.data,
                                    keyframe: au.keyframe,
                                    capture_ts_us: au.capture_ts_us,
                                },
                            );
                        }
                    });
                }
            }
            CoreKind::Sensor => {
                let (tx, mut rx) = tokio::sync::mpsc::channel(64);
                tokio::spawn(input::udp::run(
                    input::udp::addr_from_url(c.input.as_deref().unwrap())?,
                    tx,
                ));
                let agent = agent.clone();
                let id = spec.id;
                tokio::spawn(async move {
                    while let Some(m) = rx.recv().await {
                        agent.push_message(id, &m);
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
            publisher.send(&agent.publisher_config(spec.id, "profile"));
        }
    }

    // ── event loop ───────────────────────────────────────────────────────
    // Everything here is daemon policy: where a command goes, what the
    // publisher is told. The agent has already done the protocol work.
    loop {
        tokio::select! {
            ev = events.recv() => {
                let Some(ev) = ev else { break };
                match ev {
                    AgentEvent::SessionStarted { signal_id, role, .. } => {
                        publisher.send(&serde_json::json!({"type":"session","state":"started","session_id":signal_id,"role":role.as_str(),"sessions":agent.session_count()}));
                    }
                    AgentEvent::SessionEnded { signal_id, .. } => {
                        publisher.send(&serde_json::json!({"type":"session","state":"ended","session_id":signal_id,"sessions":agent.session_count()}));
                    }
                    AgentEvent::Command { channel, payload } => {
                        if let Some(t) = command_targets.get(&channel) {
                            let _ = out_sock.send_to(&payload, t);
                        }
                    }
                    AgentEvent::RequestedConfig(v) => { publisher.send(&v); }
                    AgentEvent::RecoveryRequest { channel, kind, reason } => {
                        publisher.send(&serde_json::json!({"type":"recovery-request","channel":channel,"kind":kind,"reason":reason}));
                    }
                    // The engine switches on the new layer's next keyframe. A
                    // publisher that can force an IDR on that stream turns "next
                    // keyframe" from up to a GOP into right now.
                    AgentEvent::LayerChanged { channel, layer, name, reason } => {
                        tracing::info!(channel, layer, %name, reason, "layer");
                        publisher.send(&serde_json::json!({"type":"layer","channel":channel,"layer":layer,"name":name,"reason":reason}));
                    }
                    AgentEvent::SignalDenied { .. }
                    | AgentEvent::SignalConnected
                    | AgentEvent::SignalDisconnected
                    | AgentEvent::NatReport(_) => {}
                }
            }
            _ = tokio::signal::ctrl_c() => { tracing::info!("shutting down"); break; }
        }
    }
    agent.stop();
    Ok(())
}

async fn probe_input(cfg: &Config, secs: u64) -> anyhow::Result<()> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    for c in cfg.channels.iter().filter(|c| c.kind == ChannelKind::Video) {
        let url = input::rtsp::resolve_credentials(c.input.as_deref().unwrap());
        input::spawn_video(url, input::Codec::from_codec_string(&c.codec)?, tx.clone())?;
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
