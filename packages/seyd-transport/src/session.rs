// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
use crate::relay::Relay;
use bytes::{Buf, Bytes, BytesMut};
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

pub type ControlReader = BufReader<Box<dyn tokio::io::AsyncRead + Send + Unpin>>;
pub type ControlWriter = Box<dyn tokio::io::AsyncWrite + Send + Unpin>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SendError {
    /// Not enough room in the datagram send buffer. Nothing was queued, so the
    /// caller can skip the whole frame cleanly.
    #[error("datagram send would block")]
    Blocked,
    #[error("datagram exceeds the current path MTU")]
    TooLarge,
    #[error("session closed")]
    Closed,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PathStats {
    pub rtt_ms: f64,
    /// Lowest RTT observed by this session's sampling of `rtt`.
    pub min_rtt_ms: f64,
    pub cwnd: u64,
    /// UDP bytes transmitted per second over the last `stats()` interval.
    pub delivery_kbps: u64,
    pub lost_packets: u64,
    pub current_mtu: u16,
}

/// How the pilot is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionKind {
    /// A direct WebTransport session over QUIC — the product.
    Direct,
    /// Bytes carried through the cloud relay (ADR 0010): the last resort when
    /// no candidate connected, and always shown to the pilot as such.
    Relay,
}

struct RateSample {
    at: Instant,
    bytes: u64,
    min_rtt_ms: f64,
}

struct Quic {
    conn: quinn::Connection,
    /// Quarter-stream-id prefix identifying this session in datagrams.
    prefix: Bytes,
    rate: Mutex<RateSample>,
    /// `TransportConfig::datagram_send_buffer` for this connection, so the
    /// queued backlog can be recovered from quinn's free-space figure.
    send_buffer_capacity: usize,
}

enum Backend {
    Quic(Quic),
    Relay(Relay),
}

/// One session with one pilot: a WebTransport session over QUIC, or, when no
/// direct path connected, the same datagrams and control stream carried through
/// the cloud relay. The engine sees one type either way; only [`Session::kind`]
/// tells them apart, and it is reported to the pilot rather than hidden.
pub struct Session {
    id: u64,
    backend: Backend,
    datagrams: Mutex<Option<mpsc::Receiver<Bytes>>>,
    control: Mutex<Option<oneshot::Receiver<(ControlReader, ControlWriter)>>>,
}

impl Session {
    pub(crate) fn new(
        id: u64,
        conn: quinn::Connection,
        prefix: Bytes,
        datagrams: mpsc::Receiver<Bytes>,
        control: oneshot::Receiver<(ControlReader, ControlWriter)>,
        send_buffer_capacity: usize,
    ) -> Self {
        let bytes = conn.stats().udp_tx.bytes;
        Self {
            id,
            backend: Backend::Quic(Quic {
                conn,
                prefix,
                rate: Mutex::new(RateSample {
                    at: Instant::now(),
                    bytes,
                    min_rtt_ms: f64::MAX,
                }),
                send_buffer_capacity,
            }),
            datagrams: Mutex::new(Some(datagrams)),
            control: Mutex::new(Some(control)),
        }
    }

    pub(crate) fn from_relay(
        id: u64,
        relay: Relay,
        datagrams: mpsc::Receiver<Bytes>,
        control: oneshot::Receiver<(ControlReader, ControlWriter)>,
    ) -> Self {
        Self {
            id,
            backend: Backend::Relay(relay),
            datagrams: Mutex::new(Some(datagrams)),
            control: Mutex::new(Some(control)),
        }
    }

    /// Open a relayed session through the cloud (ADR 0010). `url`, `session_id`
    /// and `token` come from the signal server's `relay-open`. Resolves once
    /// the relay has paired this socket with the pilot's, or fails.
    pub async fn relay(
        id: u64,
        url: &str,
        session_id: &str,
        token: &str,
        send_buffer_capacity: usize,
    ) -> anyhow::Result<Session> {
        crate::relay::connect(id, url, session_id, token, send_buffer_capacity).await
    }

    pub fn session_id(&self) -> u64 {
        self.id
    }

    pub fn kind(&self) -> SessionKind {
        match &self.backend {
            Backend::Quic(_) => SessionKind::Direct,
            Backend::Relay(_) => SessionKind::Relay,
        }
    }

    /// The pilot's address for a direct session; `None` through the relay,
    /// where the only peer address is the cloud's.
    pub fn remote_addr(&self) -> Option<SocketAddr> {
        match &self.backend {
            Backend::Quic(q) => Some(q.conn.remote_address()),
            Backend::Relay(_) => None,
        }
    }

    /// Largest payload `send_datagram` currently accepts (path MTU minus QUIC
    /// and WebTransport framing). Changes as DPLPMTUD converges.
    pub fn max_datagram_size(&self) -> usize {
        match &self.backend {
            Backend::Quic(q) => q
                .conn
                .max_datagram_size()
                .unwrap_or(0)
                .saturating_sub(q.prefix.len()),
            Backend::Relay(r) => r.max_datagram_size(),
        }
    }

    /// Bytes the send buffer can take right now without dropping anything.
    /// Compare a whole frame's size against this *before* sending its first chunk.
    pub fn send_buffer_space(&self) -> usize {
        match &self.backend {
            Backend::Quic(q) => q
                .conn
                .datagram_send_buffer_space()
                .saturating_sub(q.prefix.len()),
            Backend::Relay(r) => r.send_buffer_space(),
        }
    }

    /// Payload bytes queued in the datagram send buffer and not yet packetised:
    /// the backlog that admission control measures against the QoS profile's
    /// threshold. Everything counted here is latency the pilot has not seen yet.
    ///
    /// quinn exposes only the free space (capacity minus queued payload, minus
    /// one bookkeeping constant), so this is the complement; the constant makes
    /// it read a few dozen bytes high, which is nothing against a frame.
    pub fn send_buffer_queued(&self) -> usize {
        match &self.backend {
            Backend::Quic(q) => q
                .send_buffer_capacity
                .saturating_sub(q.conn.datagram_send_buffer_space()),
            Backend::Relay(r) => r.send_buffer_queued(),
        }
    }

    /// Queue one datagram. Never blocks, never evicts queued datagrams.
    pub fn send_datagram(&self, payload: Bytes) -> Result<(), SendError> {
        match &self.backend {
            Backend::Quic(q) => {
                let total = payload.len() + q.prefix.len();
                if q.conn.datagram_send_buffer_space() < total {
                    return Err(SendError::Blocked);
                }
                let mut buf = BytesMut::with_capacity(total);
                buf.extend_from_slice(&q.prefix);
                buf.extend_from_slice(&payload);
                q.conn.send_datagram(buf.freeze()).map_err(|e| match e {
                    quinn::SendDatagramError::TooLarge => SendError::TooLarge,
                    quinn::SendDatagramError::ConnectionLost(_) => SendError::Closed,
                    quinn::SendDatagramError::UnsupportedByPeer
                    | quinn::SendDatagramError::Disabled => SendError::Closed,
                })
            }
            Backend::Relay(r) => r.send_datagram(payload),
        }
    }

    /// Inbound datagrams (WebTransport prefix stripped). Can be taken once.
    pub fn take_datagrams(&self) -> Option<mpsc::Receiver<Bytes>> {
        self.datagrams.lock().unwrap().take()
    }

    /// The first bidirectional stream the pilot opens — the control stream.
    /// Resolves once the pilot has opened it; `None` if the session ends first.
    pub async fn control(&self) -> Option<(ControlReader, ControlWriter)> {
        let rx = self.control.lock().unwrap().take()?;
        rx.await.ok()
    }

    pub fn stats(&self) -> PathStats {
        match &self.backend {
            Backend::Quic(q) => {
                let s = q.conn.stats();
                let now = Instant::now();
                let mut rate = q.rate.lock().unwrap();
                let rtt_ms = s.path.rtt.as_secs_f64() * 1000.0;
                if rtt_ms > 0.0 && rtt_ms < rate.min_rtt_ms {
                    rate.min_rtt_ms = rtt_ms;
                }
                let dt = now.duration_since(rate.at).as_secs_f64();
                let delivery_kbps = if dt > 0.05 {
                    ((s.udp_tx.bytes.saturating_sub(rate.bytes)) as f64 * 8.0 / dt / 1000.0) as u64
                } else {
                    0
                };
                if dt > 0.05 {
                    rate.at = now;
                    rate.bytes = s.udp_tx.bytes;
                }
                PathStats {
                    rtt_ms,
                    min_rtt_ms: if rate.min_rtt_ms == f64::MAX {
                        rtt_ms
                    } else {
                        rate.min_rtt_ms
                    },
                    cwnd: s.path.cwnd,
                    delivery_kbps,
                    lost_packets: s.path.lost_packets,
                    current_mtu: s.path.current_mtu,
                }
            }
            Backend::Relay(r) => r.stats(),
        }
    }

    pub fn close(&self, code: u32, reason: &str) {
        match &self.backend {
            Backend::Quic(q) => q.conn.close(code.into(), reason.as_bytes()),
            Backend::Relay(r) => r.close(reason),
        }
    }

    /// Resolves when the connection is gone.
    pub async fn closed(&self) {
        match &self.backend {
            Backend::Quic(q) => {
                let _ = q.conn.closed().await;
            }
            Backend::Relay(r) => r.closed().await,
        }
    }
}

/// NDJSON helpers on the control stream halves.
pub async fn read_line(reader: &mut ControlReader) -> Option<String> {
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) | Err(_) => return None,
            Ok(_) => {
                let t = line.trim_end_matches(['\n', '\r']);
                if !t.is_empty() {
                    return Some(t.to_string());
                }
            }
        }
    }
}

pub async fn write_line(writer: &mut ControlWriter, line: &str) -> std::io::Result<()> {
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await
}

/// Split a QUIC varint-prefixed WebTransport datagram into (quarter stream id, payload).
pub(crate) fn split_prefix(mut buf: Bytes) -> Option<(u64, Bytes)> {
    if buf.is_empty() {
        return None;
    }
    let first = buf[0];
    let len = 1usize << (first >> 6);
    if buf.len() < len {
        return None;
    }
    let mut v: u64 = (first & 0x3f) as u64;
    for i in 1..len {
        v = (v << 8) | buf[i] as u64;
    }
    buf.advance(len);
    Some((v, buf))
}

/// Encode a QUIC varint.
pub(crate) fn varint(v: u64) -> Bytes {
    let mut out = BytesMut::new();
    if v < 1 << 6 {
        out.extend_from_slice(&[v as u8]);
    } else if v < 1 << 14 {
        out.extend_from_slice(&((v as u16) | 0x4000).to_be_bytes());
    } else if v < 1 << 30 {
        out.extend_from_slice(&((v as u32) | 0x8000_0000).to_be_bytes());
    } else {
        out.extend_from_slice(&(v | 0xc000_0000_0000_0000).to_be_bytes());
    }
    out.freeze()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_roundtrip() {
        for v in [0u64, 1, 63, 64, 16383, 16384, 1 << 29, 1 << 30, 1 << 40] {
            let mut b = BytesMut::from(&varint(v)[..]);
            b.extend_from_slice(b"xyz");
            let (got, rest) = split_prefix(b.freeze()).unwrap();
            assert_eq!(got, v);
            assert_eq!(&rest[..], b"xyz");
        }
    }
}
