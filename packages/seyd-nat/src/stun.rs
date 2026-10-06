// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
//! STUN client (RFC 5389) on a socket we own, plus NAT classification.
//!
//! Two servers on different operators are queried so the answers can be
//! compared: the same `ip:port` from both means a cone NAT and a usable
//! reflexive candidate; the same IP with different ports means the mapping is
//! chosen per destination — symmetric — and the port STUN saw is not the port
//! the pilot would arrive on. Anycast within one provider could hide that,
//! which is why two operators rather than one.

use rand::RngCore;
use serde::Serialize;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

pub const STUN_SERVERS: [(&str, u16); 2] =
    [("stun.l.google.com", 19302), ("stun.cloudflare.com", 3478)];

const MAGIC: u32 = 0x2112_A442;
const BINDING_REQUEST: u16 = 0x0001;
const BINDING_RESPONSE: u16 = 0x0101;
const XOR_MAPPED_ADDRESS: u16 = 0x0020;

/// Compressed RFC 5389 retransmission ladder. The full schedule runs to
/// 39.5 s, far longer than any pilot will wait for us.
const RETRANSMIT_DELAYS: [Duration; 4] = [
    Duration::ZERO,
    Duration::from_millis(250),
    Duration::from_millis(500),
    Duration::from_millis(1000),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum NatType {
    Cone,
    Symmetric,
    Unknown,
}

/// What STUN could work out about our position behind the NAT.
#[derive(Debug, Clone, Default)]
pub struct NatDiscovery {
    /// External `ip:port`, only when both servers agreed.
    pub reflexive: Option<SocketAddr>,
    pub observed: Vec<SocketAddr>,
    pub symmetric: bool,
}

impl NatDiscovery {
    pub fn nat_type(&self) -> NatType {
        if self.symmetric {
            NatType::Symmetric
        } else if self.reflexive.is_some() {
            NatType::Cone
        } else {
            NatType::Unknown
        }
    }
}

/// Encode a Binding Request with the given 12-byte transaction id.
pub fn encode_binding_request(txid: &[u8; 12]) -> [u8; 20] {
    let mut out = [0u8; 20];
    out[0..2].copy_from_slice(&BINDING_REQUEST.to_be_bytes());
    out[4..8].copy_from_slice(&MAGIC.to_be_bytes());
    out[8..20].copy_from_slice(txid);
    out
}

/// Extract XOR-MAPPED-ADDRESS from a Binding Success Response for `txid`.
pub fn parse_binding_response(data: &[u8], txid: &[u8; 12]) -> Option<SocketAddr> {
    if data.len() < 20 {
        return None;
    }
    let msg_type = u16::from_be_bytes([data[0], data[1]]);
    let magic = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    if magic != MAGIC || msg_type != BINDING_RESPONSE || &data[8..20] != txid {
        return None;
    }
    let mut off = 20;
    while off + 4 <= data.len() {
        let attr_type = u16::from_be_bytes([data[off], data[off + 1]]);
        let attr_len = u16::from_be_bytes([data[off + 2], data[off + 3]]) as usize;
        off += 4;
        let attr = data.get(off..off + attr_len)?;
        off += (attr_len + 3) & !3; // attributes are 4-byte aligned
        if attr_type != XOR_MAPPED_ADDRESS || attr.len() < 8 {
            continue;
        }
        let family = attr[1];
        let port = u16::from_be_bytes([attr[2], attr[3]]) ^ (MAGIC >> 16) as u16;
        match family {
            0x01 => {
                let ip = u32::from_be_bytes([attr[4], attr[5], attr[6], attr[7]]) ^ MAGIC;
                return Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::from(ip)), port));
            }
            0x02 if attr.len() >= 20 => {
                // IPv6 is XORed with the magic cookie followed by the transaction id.
                let mut mask = [0u8; 16];
                mask[..4].copy_from_slice(&MAGIC.to_be_bytes());
                mask[4..].copy_from_slice(txid);
                let mut raw = [0u8; 16];
                for i in 0..16 {
                    raw[i] = attr[4 + i] ^ mask[i];
                }
                return Some(SocketAddr::new(IpAddr::V6(Ipv6Addr::from(raw)), port));
            }
            _ => {}
        }
    }
    None
}

/// Build a Binding Success Response carrying `addr` (test helper and useful
/// for a future in-process STUN server).
pub fn encode_binding_response(txid: &[u8; 12], addr: SocketAddr) -> Vec<u8> {
    let mut attr = Vec::new();
    let xport = addr.port() ^ (MAGIC >> 16) as u16;
    match addr.ip() {
        IpAddr::V4(v4) => {
            attr.extend_from_slice(&[0, 0x01]);
            attr.extend_from_slice(&xport.to_be_bytes());
            attr.extend_from_slice(&(u32::from(v4) ^ MAGIC).to_be_bytes());
        }
        IpAddr::V6(v6) => {
            attr.extend_from_slice(&[0, 0x02]);
            attr.extend_from_slice(&xport.to_be_bytes());
            let mut mask = [0u8; 16];
            mask[..4].copy_from_slice(&MAGIC.to_be_bytes());
            mask[4..].copy_from_slice(txid);
            attr.extend(v6.octets().iter().zip(mask).map(|(a, b)| a ^ b));
        }
    }
    let mut out = Vec::with_capacity(20 + 4 + attr.len());
    out.extend_from_slice(&BINDING_RESPONSE.to_be_bytes());
    out.extend_from_slice(&((4 + attr.len()) as u16).to_be_bytes());
    out.extend_from_slice(&MAGIC.to_be_bytes());
    out.extend_from_slice(txid);
    out.extend_from_slice(&XOR_MAPPED_ADDRESS.to_be_bytes());
    out.extend_from_slice(&(attr.len() as u16).to_be_bytes());
    out.extend_from_slice(&attr);
    out
}

async fn resolve(host: &str, port: u16, want_v6: bool) -> Option<SocketAddr> {
    let addrs = tokio::net::lookup_host((host, port)).await.ok()?;
    addrs.into_iter().find(|a| a.is_ipv6() == want_v6)
}

/// One Binding Request with retransmission on `sock`; returns the reflexive address.
async fn binding_transaction(
    sock: &tokio::net::UdpSocket,
    server: (&str, u16),
    timeout: Duration,
) -> Option<SocketAddr> {
    let want_v6 = sock.local_addr().ok()?.is_ipv6();
    let server_addr = match resolve(server.0, server.1, want_v6).await {
        Some(a) => a,
        None => {
            tracing::debug!("STUN DNS lookup for {} failed", server.0);
            return None;
        }
    };
    let mut txid = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut txid);
    let request = encode_binding_request(&txid);
    let deadline = Instant::now() + timeout;
    let mut buf = [0u8; 1024];

    for (i, delay) in RETRANSMIT_DELAYS.iter().enumerate() {
        if let Err(e) = sock.send_to(&request, server_addr).await {
            tracing::debug!("STUN send to {} failed: {e}", server.0);
            return None;
        }
        // Wait for a matching reply until the next retransmit (or the deadline).
        let wait_until = if i + 1 < RETRANSMIT_DELAYS.len() {
            (Instant::now() + RETRANSMIT_DELAYS[i + 1] - *delay).min(deadline)
        } else {
            deadline
        };
        loop {
            let now = Instant::now();
            if now >= wait_until {
                break;
            }
            match tokio::time::timeout(wait_until - now, sock.recv_from(&mut buf)).await {
                Ok(Ok((n, _))) => {
                    if let Some(addr) = parse_binding_response(&buf[..n], &txid) {
                        return Some(addr);
                    }
                    // Unrelated datagram (QUIC probe, other STUN txid): keep waiting.
                }
                Ok(Err(_)) | Err(_) => break,
            }
        }
        if Instant::now() >= deadline {
            break;
        }
    }
    tracing::debug!("STUN timed out against {}", server.0);
    None
}

/// Query every STUN server from `sock` and classify what we see.
///
/// `sock` must be bound and non-blocking, and must be the same socket the
/// QUIC server will go on to use — that is the whole point. The socket is
/// duplicated for the duration of the call; the original is never touched.
pub async fn discover(sock: &UdpSocket) -> NatDiscovery {
    discover_with_timeout(sock, Duration::from_millis(2500)).await
}

pub async fn discover_with_timeout(sock: &UdpSocket, timeout: Duration) -> NatDiscovery {
    let mut result = NatDiscovery::default();
    let dup = match sock.try_clone().and_then(tokio::net::UdpSocket::from_std) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("STUN: cannot register socket with the runtime: {e}");
            return result;
        }
    };
    for server in STUN_SERVERS {
        if let Some(addr) = binding_transaction(&dup, server, timeout).await {
            tracing::debug!("STUN {} reports {addr}", server.0);
            result.observed.push(addr);
        }
    }
    if result.observed.is_empty() {
        tracing::warn!(
            "STUN failed against all {} servers — no reflexive candidate",
            STUN_SERVERS.len()
        );
        return result;
    }
    let first = result.observed[0];
    if result.observed.iter().all(|a| *a == first) {
        result.reflexive = Some(first);
    } else if result.observed.iter().all(|a| a.ip() == first.ip()) {
        // Same external IP, different ports: textbook symmetric NAT.
        result.symmetric = true;
        tracing::info!(
            "symmetric NAT detected ({:?}) — reflexive candidate is unusable",
            result.observed
        );
    } else {
        // Different external IPs — multiple WAN links or a load-balanced CGNAT.
        result.symmetric = true;
        tracing::info!(
            "inconsistent external addresses from STUN ({:?}) — treating as symmetric",
            result.observed
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_layout() {
        let txid = [7u8; 12];
        let r = encode_binding_request(&txid);
        assert_eq!(&r[..8], &[0x00, 0x01, 0x00, 0x00, 0x21, 0x12, 0xa4, 0x42]);
        assert_eq!(&r[8..], &txid);
    }

    #[test]
    fn v4_response_roundtrip_and_known_bytes() {
        let txid = [0xaa; 12];
        let addr: SocketAddr = "203.0.113.5:4433".parse().unwrap();
        let resp = encode_binding_response(&txid, addr);
        // attribute: type 0x0020, len 8, family 1, xport, xaddr
        assert_eq!(&resp[20..24], &[0x00, 0x20, 0x00, 0x08]);
        assert_eq!(resp[25], 0x01);
        assert_eq!(u16::from_be_bytes([resp[26], resp[27]]), 4433 ^ 0x2112);
        assert_eq!(parse_binding_response(&resp, &txid), Some(addr));
        assert_eq!(
            parse_binding_response(&resp, &[0xbb; 12]),
            None,
            "txid mismatch"
        );
        let mut wrong = resp.clone();
        wrong[1] = 0x11;
        assert_eq!(
            parse_binding_response(&wrong, &txid),
            None,
            "error response"
        );
    }

    #[test]
    fn v6_response_roundtrip() {
        let txid = [0x5c; 12];
        let addr: SocketAddr = "[2606:4700::1111]:60001".parse().unwrap();
        let resp = encode_binding_response(&txid, addr);
        assert_eq!(parse_binding_response(&resp, &txid), Some(addr));
    }

    #[test]
    fn skips_unknown_attributes() {
        let txid = [1u8; 12];
        let addr: SocketAddr = "8.8.8.8:53".parse().unwrap();
        let mut resp = encode_binding_response(&txid, addr);
        // Prepend a SOFTWARE attribute (0x8022) with 3-byte value padded to 4.
        let extra = [0x80, 0x22, 0x00, 0x03, b'x', b'y', b'z', 0x00];
        let mut with_extra = resp[..20].to_vec();
        with_extra.extend_from_slice(&extra);
        with_extra.extend_from_slice(&resp[20..]);
        let len = (with_extra.len() - 20) as u16;
        with_extra[2..4].copy_from_slice(&len.to_be_bytes());
        resp = with_extra;
        assert_eq!(parse_binding_response(&resp, &txid), Some(addr));
    }

    #[test]
    fn classification() {
        let a: SocketAddr = "1.2.3.4:1000".parse().unwrap();
        let b: SocketAddr = "1.2.3.4:1001".parse().unwrap();
        let cone = NatDiscovery {
            reflexive: Some(a),
            observed: vec![a, a],
            symmetric: false,
        };
        assert_eq!(cone.nat_type(), NatType::Cone);
        let sym = NatDiscovery {
            reflexive: None,
            observed: vec![a, b],
            symmetric: true,
        };
        assert_eq!(sym.nat_type(), NatType::Symmetric);
        assert_eq!(NatDiscovery::default().nat_type(), NatType::Unknown);
    }
}
