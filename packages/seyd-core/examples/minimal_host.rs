// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
//! minimal_host — the smallest host of [`seyd_core::Agent`].
//!
//! A *host* supplies media and consumes [`AgentEvent`]s; the agent owns
//! everything else (sockets, discovery, certificate, signaling, lease renewal,
//! cert rotation). This program is the whole contract in one file: build an
//! [`AgentConfig`], declare a sensor channel and a command channel, start,
//! push a counter once a second, print every event, stop on Ctrl-C.
//!
//!     cargo run -p seyd-core --example minimal_host -- <robot_id> <signal_url> [credential_path]
//!
//! `seydd` is the reference host (packages/seydd/src/main.rs): the same calls,
//! plus TOML, RTSP/RTP inputs and UDP sinks. Add video with
//! [`Agent::push_video`], handing it the access units your encoder already
//! produces; Seyd never decodes or re-encodes.

use seyd_core::{Agent, AgentConfig, AgentEvent, ChannelKind, ChannelSpec};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: minimal_host <robot_id> <signal_url> [credential_path]");
        std::process::exit(2);
    }

    // Channels are numbered from 1 in declaration order; that numbering is
    // what the pilot sees and what `push_message` and `Command` refer to.
    let telemetry = ChannelSpec {
        id: 1,
        kind: ChannelKind::Sensor,
        name: "telemetry".into(),
        codec: "json".into(),
        fps: 0,
        layers: Vec::new(),
        max_bitrate_kbps: 0,
    };
    let drive = ChannelSpec {
        id: 2,
        kind: ChannelKind::Command,
        name: "drive".into(),
        codec: "json".into(),
        fps: 0,
        layers: Vec::new(),
        max_bitrate_kbps: 0,
    };

    // Everything not set here keeps the same default as `seydd.toml`.
    let cfg = AgentConfig {
        robot_id: args[1].clone(),
        signal_url: args[2].clone(),
        credential_path: PathBuf::from(args.get(3).map(String::as_str).unwrap_or("./robot.key")),
        qos_profile: "latency".into(),
        channels: vec![telemetry.clone(), drive],
        agent_version: "minimal_host/0.1".into(),
        ..AgentConfig::default()
    };

    // Binds, discovers candidates, announces to the cloud; returns once pilots
    // may connect. The receiver is the host's side of the agent.
    let (agent, mut events) = Agent::start(cfg).await?;
    println!("robot {} online; pilots may connect", args[1]);

    // A 1 Hz counter on the sensor channel. `push_message` never blocks, so
    // this can run on any task or thread.
    let sensor = agent.clone();
    let ticker = tokio::spawn(async move {
        let mut seq: u64 = 0;
        loop {
            let t_us = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_micros() as u64)
                .unwrap_or(0);
            let msg = serde_json::json!({ "seq": seq, "t_us": t_us });
            sensor.push_message(telemetry.id, msg.to_string().as_bytes());
            seq += 1;
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });

    // The host's event loop. Nothing here is protocol: it is the robot's own
    // policy about what a command means and what the publisher is told.
    loop {
        tokio::select! {
            ev = events.recv() => {
                let Some(ev) = ev else { break };
                match ev {
                    AgentEvent::Command { channel, payload } => {
                        println!("command on channel {channel}: {}", String::from_utf8_lossy(&payload));
                    }
                    AgentEvent::SessionEnded { signal_id, reason } => {
                        // Park actuators here: this fires whether the pilot said
                        // goodbye or simply vanished.
                        println!("session {signal_id} ended ({reason}); {} left", agent.session_count());
                    }
                    other => println!("{other:?}"),
                }
            }
            _ = tokio::signal::ctrl_c() => {
                println!("stopping");
                break;
            }
        }
    }

    ticker.abort();
    agent.stop();
    Ok(())
}
