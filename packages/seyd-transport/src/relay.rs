// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
//! The cloud relay backend of [`Session`] (ADR 0010).
//!
//! When no candidate connected, the pilot may ask the signal server to carry
//! the session: each party opens one WebSocket to the relay, the cloud pairs
//! them by session id and forwards binary frames verbatim. The frames are the
//! very bytes that would have been QUIC datagrams and control-stream bytes:
//!
//! | byte 0 | rest |
//! |---|---|
//! | `1` | one datagram (a chunk, exactly as `send_datagram` would have sent it) |
//! | `2` | a segment of the control stream (NDJSON bytes, any split) |
//!
//! Nothing above the transport knows the difference: the engine's admission
//! control still measures a queued backlog against the profile's threshold,
//! and the pilot's reassembler still assembles chunks. The relay is TCP, so
//! loss is retransmitted rather than repaired by FEC, and a congested leg
//! shows up as backlog here (`send_buffer_queued`) instead of as QUIC loss.

use crate::session::{ControlReader, ControlWriter, PathStats, SendError, Session};
use bytes::{Bytes, BytesMut};
use futures_util::{SinkExt, StreamExt};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::Message;

pub const KIND_DATAGRAM: u8 = 1;
pub const KIND_CONTROL: u8 = 2;

/// How long the cloud gets to pair us with the pilot.
const ATTACH_TIMEOUT: Duration = Duration::from_secs(10);
/// A relayed datagram may be as large as a WebSocket frame; the engine's
/// chunk length is what actually bounds it.
const MAX_DATAGRAM: usize = 60_000;

enum Out {
    /// A complete frame (kind byte included) and, for datagrams, the payload
    /// bytes to release from the queued count once it has been written.
    Frame(Bytes, usize),
    Close(String),
}

#[derive(Default)]
struct RttSample {
    rtt_ms: f64,
    min_rtt_ms: f64,
}

struct Rate {
    at: Instant,
    bytes: u64,
}

pub struct Relay {
    out: mpsc::UnboundedSender<Out>,
    /// Datagram payload bytes handed to the writer task and not yet written.
    queued: Arc<AtomicUsize>,
    capacity: usize,
    bytes_tx: Arc<AtomicU64>,
    rtt: Arc<Mutex<RttSample>>,
    rate: Mutex<Rate>,
    closed: watch::Receiver<bool>,
}

impl Relay {
    pub fn max_datagram_size(&self) -> usize {
        MAX_DATAGRAM
    }

    pub fn send_buffer_space(&self) -> usize {
        self.capacity
            .saturating_sub(self.queued.load(Ordering::Relaxed))
    }

    pub fn send_buffer_queued(&self) -> usize {
        self.queued.load(Ordering::Relaxed)
    }

    pub fn send_datagram(&self, payload: Bytes) -> Result<(), SendError> {
        if *self.closed.borrow() {
            return Err(SendError::Closed);
        }
        if payload.len() > MAX_DATAGRAM {
            return Err(SendError::TooLarge);
        }
        // Reserve before queueing so two frames in flight cannot both pass the
        // check against the same free space.
        let len = payload.len();
        let queued = self.queued.fetch_add(len, Ordering::Relaxed);
        if queued + len > self.capacity {
            self.queued.fetch_sub(len, Ordering::Relaxed);
            return Err(SendError::Blocked);
        }
        let mut frame = BytesMut::with_capacity(len + 1);
        frame.extend_from_slice(&[KIND_DATAGRAM]);
        frame.extend_from_slice(&payload);
        if self.out.send(Out::Frame(frame.freeze(), len)).is_err() {
            self.queued.fetch_sub(len, Ordering::Relaxed);
            return Err(SendError::Closed);
        }
        Ok(())
    }

    pub fn stats(&self) -> PathStats {
        let rtt = self.rtt.lock().unwrap();
        let now = Instant::now();
        let mut rate = self.rate.lock().unwrap();
        let bytes = self.bytes_tx.load(Ordering::Relaxed);
        let dt = now.duration_since(rate.at).as_secs_f64();
        let delivery_kbps = if dt > 0.05 {
            ((bytes.saturating_sub(rate.bytes)) as f64 * 8.0 / dt / 1000.0) as u64
        } else {
            0
        };
        if dt > 0.05 {
            rate.at = now;
            rate.bytes = bytes;
        }
        PathStats {
            rtt_ms: rtt.rtt_ms,
            min_rtt_ms: rtt.min_rtt_ms,
            cwnd: 0,
            delivery_kbps,
            lost_packets: 0,
            current_mtu: 0,
        }
    }

    pub fn close(&self, reason: &str) {
        let _ = self.out.send(Out::Close(reason.to_string()));
    }

    pub async fn closed(&self) {
        let mut rx = self.closed.clone();
        while !*rx.borrow() {
            if rx.changed().await.is_err() {
                return;
            }
        }
    }
}

/// Dial the relay, attach to `session_id`, and wait until the cloud has
/// paired us with the pilot.
pub(crate) async fn connect(
    id: u64,
    url: &str,
    session_id: &str,
    token: &str,
    capacity: usize,
) -> anyhow::Result<Session> {
    let (ws, _) = tokio_tungstenite::connect_async(url).await?;
    let (mut sink, mut stream) = ws.split();
    let attach = serde_json::json!({
        "type": "relay-attach", "session_id": session_id, "token": token, "party": "robot",
    });
    sink.send(Message::Text(attach.to_string().into())).await?;
    let reply = tokio::time::timeout(ATTACH_TIMEOUT, async {
        loop {
            match stream.next().await {
                Some(Ok(Message::Text(t))) => {
                    let v: serde_json::Value = serde_json::from_str(&t)?;
                    return Ok::<_, anyhow::Error>(v);
                }
                Some(Ok(Message::Close(_))) | None => {
                    anyhow::bail!("relay closed before attaching")
                }
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Err(e.into()),
            }
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("relay attach timed out"))??;
    match reply.get("type").and_then(|t| t.as_str()) {
        Some("relay-attached") => {}
        Some("denied") => anyhow::bail!(
            "relay denied: {}",
            reply
                .get("reason")
                .and_then(|r| r.as_str())
                .unwrap_or("unknown")
        ),
        other => anyhow::bail!("unexpected relay reply {other:?}"),
    }

    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Out>();
    let (dg_tx, dg_rx) = mpsc::channel::<Bytes>(1024);
    let (ctl_tx, ctl_rx) = oneshot::channel::<(ControlReader, ControlWriter)>();
    let (closed_tx, closed_rx) = watch::channel(false);
    let queued = Arc::new(AtomicUsize::new(0));
    let bytes_tx = Arc::new(AtomicU64::new(0));
    let rtt = Arc::new(Mutex::new(RttSample::default()));

    // The control stream is two byte pipes: what the engine writes is framed
    // out as `KIND_CONTROL`; what arrives as `KIND_CONTROL` is fed to what
    // the engine reads. The pilot speaks first, exactly as over QUIC.
    let (ctl_in_engine, mut ctl_in_task) = tokio::io::duplex(64 * 1024);
    let (ctl_out_engine, mut ctl_out_task) = tokio::io::duplex(64 * 1024);
    let reader: ControlReader = tokio::io::BufReader::new(Box::new(ctl_in_engine));
    let writer: ControlWriter = Box::new(ctl_out_engine);
    let _ = ctl_tx.send((reader, writer));

    let relay = Relay {
        out: out_tx,
        queued: queued.clone(),
        capacity,
        bytes_tx: bytes_tx.clone(),
        rtt: rtt.clone(),
        rate: Mutex::new(Rate {
            at: Instant::now(),
            bytes: 0,
        }),
        closed: closed_rx,
    };

    tokio::spawn(async move {
        let mut ping = tokio::time::interval(Duration::from_secs(1));
        ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let epoch = Instant::now();
        let mut ctl_buf = vec![0u8; 16 * 1024];
        let mut dropped = 0u64;
        let reason: String = loop {
            tokio::select! {
                out = out_rx.recv() => {
                    match out {
                        Some(Out::Frame(frame, release)) => {
                            let len = frame.len() as u64;
                            let res = sink.send(Message::Binary(frame)).await;
                            if release > 0 {
                                queued.fetch_sub(release, Ordering::Relaxed);
                            }
                            if res.is_err() {
                                break "write failed".to_string();
                            }
                            bytes_tx.fetch_add(len, Ordering::Relaxed);
                        }
                        Some(Out::Close(reason)) => {
                            let _ = sink.send(Message::Close(None)).await;
                            break reason;
                        }
                        None => break "session dropped".to_string(),
                    }
                }
                n = ctl_out_task.read(&mut ctl_buf) => {
                    match n {
                        Ok(0) | Err(_) => break "control writer closed".to_string(),
                        Ok(n) => {
                            let mut frame = BytesMut::with_capacity(n + 1);
                            frame.extend_from_slice(&[KIND_CONTROL]);
                            frame.extend_from_slice(&ctl_buf[..n]);
                            if sink.send(Message::Binary(frame.freeze())).await.is_err() {
                                break "write failed".to_string();
                            }
                        }
                    }
                }
                msg = stream.next() => {
                    match msg {
                        Some(Ok(Message::Binary(b))) => {
                            match b.first().copied() {
                                Some(KIND_DATAGRAM) => {
                                    if dg_tx.try_send(b.slice(1..)).is_err() {
                                        dropped += 1;
                                    }
                                }
                                Some(KIND_CONTROL) => {
                                    let ok = ctl_in_task.write_all(&b[1..]).await.is_ok();
                                    if !ok {
                                        break "control reader closed".to_string();
                                    }
                                }
                                _ => {}
                            }
                        }
                        Some(Ok(Message::Pong(p))) => {
                            if p.len() == 8 {
                                let sent = u64::from_be_bytes(p[..8].try_into().unwrap());
                                let now = epoch.elapsed().as_micros() as u64;
                                let ms = now.saturating_sub(sent) as f64 / 1000.0;
                                let mut r = rtt.lock().unwrap();
                                r.rtt_ms = ms;
                                if r.min_rtt_ms == 0.0 || ms < r.min_rtt_ms {
                                    r.min_rtt_ms = ms;
                                }
                            }
                        }
                        Some(Ok(Message::Text(t))) => {
                            // `relay-closed` and friends: the cloud ending the session.
                            tracing::debug!(session = id, msg = %t, "relay text");
                            if t.contains("relay-closed") || t.contains("denied") {
                                break "relay closed".to_string();
                            }
                        }
                        Some(Ok(Message::Close(_))) | None => break "relay socket closed".to_string(),
                        Some(Ok(_)) => {}
                        Some(Err(e)) => break format!("relay error: {e}"),
                    }
                }
                _ = ping.tick() => {
                    let ts = (epoch.elapsed().as_micros() as u64).to_be_bytes();
                    if sink.send(Message::Ping(Bytes::copy_from_slice(&ts))).await.is_err() {
                        break "ping failed".to_string();
                    }
                }
            }
        };
        if dropped > 0 {
            tracing::debug!(
                session = id,
                dropped,
                "relayed datagrams dropped (receiver slow)"
            );
        }
        tracing::info!(session = id, %reason, "relay session ended");
        let _ = closed_tx.send(true);
    });

    Ok(Session::from_relay(id, relay, dg_rx, ctl_rx))
}
