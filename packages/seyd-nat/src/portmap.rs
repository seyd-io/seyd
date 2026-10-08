// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
//! Router port mapping — PCP (RFC 6887), NAT-PMP (RFC 6886), UPnP-IGD.
//!
//! An installed mapping beats an inferred one: it also covers port-restricted
//! and many symmetric NATs, and it does not expire the way a hole-punched
//! mapping does. Carrier-grade NAT refuses all three quickly and definitively,
//! which is itself useful — it tells the pilot to stop hoping.
//!
//! All three are implemented directly (no miniupnpc, no reqwest): the UPnP
//! side needs only SSDP, one HTTP GET and two SOAP POSTs on the LAN.

use crate::addr::is_global;
use rand::RngCore;
use serde::Serialize;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddrV4};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};

const PCP_PORT: u16 = 5351;
const SSDP_ADDR: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(239, 255, 255, 250), 1900);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MapProtocol {
    Pcp,
    NatPmp,
    Upnp,
}

/// An external `ip:port` the router agreed to forward to us.
#[derive(Debug, Clone)]
pub struct PortMapping {
    pub protocol: MapProtocol,
    pub external_ip: IpAddr,
    pub external_port: u16,
    pub internal_port: u16,
    /// Seconds; 0 means the router calls it permanent.
    pub lifetime_s: u32,
    /// Whether `external_ip` is reachable from the public internet. A router
    /// under double NAT or CGNAT installs the mapping and reports an address
    /// nobody can route to; the mapping is still useful (it removes the inner
    /// NAT from the path) but must not be advertised.
    pub routable: bool,
    gateway: Option<Ipv4Addr>,
    upnp: Option<UpnpService>,
}

#[derive(Debug, Clone)]
struct UpnpService {
    control_url: String,
    service_type: String,
    internal_ip: Ipv4Addr,
}

impl PortMapping {
    fn new(
        protocol: MapProtocol,
        external_ip: IpAddr,
        external_port: u16,
        internal_port: u16,
        lifetime_s: u32,
    ) -> Self {
        PortMapping {
            protocol,
            external_ip,
            external_port,
            internal_port,
            lifetime_s,
            routable: is_global(external_ip),
            gateway: None,
            upnp: None,
        }
    }
}

// ── gateway discovery ──────────────────────────────────────────────────────

/// Best-effort IPv4 default gateway, for the UDP-to-gateway protocols.
/// `None` means PCP and NAT-PMP are skipped; UPnP still runs (SSDP finds the
/// router by multicast, not by address).
pub fn default_gateway() -> Option<Ipv4Addr> {
    #[cfg(windows)]
    {
        windows_default_gateway()
    }
    #[cfg(not(windows))]
    {
        unix_default_gateway()
    }
}

/// Windows: ask the IP helper API for the route it would take to a public
/// address. The next hop of that route is the gateway. No `route print`
/// parsing — its table headers are localised, its columns are not stable.
#[cfg(windows)]
fn windows_default_gateway() -> Option<Ipv4Addr> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{GetBestRoute, MIB_IPFORWARDROW};
    // Any globally routed address resolves to the default route; this one is
    // never contacted, only looked up.
    let probe = u32::from_ne_bytes(Ipv4Addr::new(1, 1, 1, 1).octets());
    // SAFETY: `row` is a plain-data struct the call fills in; zeroed is a
    // valid initial state and the pointer outlives the call.
    let row = unsafe {
        let mut row: MIB_IPFORWARDROW = std::mem::zeroed();
        if GetBestRoute(probe, 0, &mut row) != 0 {
            return None;
        }
        row
    };
    let next_hop = Ipv4Addr::from(row.dwForwardNextHop.to_ne_bytes());
    // An on-link "route" to the probe itself means there is no gateway.
    (!next_hop.is_unspecified() && next_hop != Ipv4Addr::new(1, 1, 1, 1)).then_some(next_hop)
}

#[cfg(not(windows))]
fn unix_default_gateway() -> Option<Ipv4Addr> {
    // Linux: the kernel routing table.
    if let Ok(table) = std::fs::read_to_string("/proc/net/route") {
        for line in table.lines().skip(1) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() > 2 && parts[1] == "00000000" {
                if let Ok(raw) = u32::from_str_radix(parts[2], 16) {
                    return Some(Ipv4Addr::from(raw.swap_bytes()));
                }
            }
        }
    }
    // macOS / BSD: `route -n get default` prints "gateway: 192.168.1.1".
    if let Ok(out) = std::process::Command::new("route")
        .args(["-n", "get", "default"])
        .output()
    {
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            if let Some(rest) = line.trim().strip_prefix("gateway:") {
                if let Ok(ip) = rest.trim().parse() {
                    return Some(ip);
                }
            }
        }
    }
    None
}

/// Which of our addresses the kernel would use to reach `host`.
fn local_ip_towards(host: Ipv4Addr) -> Option<Ipv4Addr> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect((host, 9)).ok()?;
    match s.local_addr().ok()?.ip() {
        IpAddr::V4(v4) => Some(v4),
        _ => None,
    }
}

async fn udp_request(gateway: Ipv4Addr, payload: &[u8], expect_len: usize) -> Option<Vec<u8>> {
    let sock = UdpSocket::bind("0.0.0.0:0").await.ok()?;
    let mut buf = [0u8; 1024];
    for _ in 0..3 {
        sock.send_to(payload, (gateway, PCP_PORT)).await.ok()?;
        match tokio::time::timeout(Duration::from_secs(1), sock.recv_from(&mut buf)).await {
            Ok(Ok((n, _))) if n >= expect_len => return Some(buf[..n].to_vec()),
            Ok(Ok(_)) => continue,
            Ok(Err(_)) => return None,
            Err(_) => continue,
        }
    }
    None
}

// ── PCP (RFC 6887) ─────────────────────────────────────────────────────────

/// PCP MAP request: version 2, opcode MAP, lifetime, client address as
/// IPv4-mapped IPv6, 12-byte nonce, protocol UDP, internal + suggested port,
/// any external address.
pub fn encode_pcp_map(
    client_ip: Ipv4Addr,
    nonce: &[u8; 12],
    internal_port: u16,
    lifetime_s: u32,
) -> [u8; 60] {
    let mut r = [0u8; 60];
    r[0] = 2;
    r[1] = 1;
    r[4..8].copy_from_slice(&lifetime_s.to_be_bytes());
    r[8..24].copy_from_slice(&client_ip.to_ipv6_mapped().octets());
    r[24..36].copy_from_slice(nonce);
    r[36] = 17;
    r[40..42].copy_from_slice(&internal_port.to_be_bytes());
    r[42..44].copy_from_slice(&internal_port.to_be_bytes());
    r
}

/// Parse a PCP MAP response: `(lifetime, external_port, external_ip)`.
pub fn parse_pcp_map(reply: &[u8]) -> Option<(u32, u16, IpAddr)> {
    if reply.len() < 60 || reply[0] != 2 || reply[1] != 0x81 {
        return None;
    }
    if reply[3] != 0 {
        tracing::debug!("PCP refused: result={}", reply[3]);
        return None;
    }
    let lifetime = u32::from_be_bytes([reply[4], reply[5], reply[6], reply[7]]);
    let ext_port = u16::from_be_bytes([reply[42], reply[43]]);
    let mut raw = [0u8; 16];
    raw.copy_from_slice(&reply[44..60]);
    let v6 = Ipv6Addr::from(raw);
    let ip = match v6.to_ipv4_mapped() {
        Some(v4) => IpAddr::V4(v4),
        None => IpAddr::V6(v6),
    };
    Some((lifetime, ext_port, ip))
}

async fn try_pcp(gateway: Ipv4Addr, internal_port: u16, lifetime_s: u32) -> Option<PortMapping> {
    let client_ip = local_ip_towards(gateway)?;
    let mut nonce = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce);
    let req = encode_pcp_map(client_ip, &nonce, internal_port, lifetime_s);
    let reply = udp_request(gateway, &req, 60).await?;
    let (granted, ext_port, ext_ip) = parse_pcp_map(&reply)?;
    let mut m = PortMapping::new(MapProtocol::Pcp, ext_ip, ext_port, internal_port, granted);
    m.gateway = Some(gateway);
    Some(m)
}

// ── NAT-PMP (RFC 6886) ─────────────────────────────────────────────────────

/// NAT-PMP map-UDP request (opcode 1): version, opcode, reserved, internal,
/// suggested external, lifetime.
pub fn encode_natpmp_map(internal_port: u16, lifetime_s: u32) -> [u8; 12] {
    let mut r = [0u8; 12];
    r[1] = 1;
    r[4..6].copy_from_slice(&internal_port.to_be_bytes());
    r[6..8].copy_from_slice(&internal_port.to_be_bytes());
    r[8..12].copy_from_slice(&lifetime_s.to_be_bytes());
    r
}

/// Parse a NAT-PMP map response: `(external_port, lifetime)`.
pub fn parse_natpmp_map(reply: &[u8]) -> Option<(u16, u32)> {
    if reply.len() < 16 || reply[0] != 0 || reply[1] != 129 {
        return None;
    }
    let result = u16::from_be_bytes([reply[2], reply[3]]);
    if result != 0 {
        tracing::debug!("NAT-PMP refused: result={result}");
        return None;
    }
    let ext_port = u16::from_be_bytes([reply[10], reply[11]]);
    let lifetime = u32::from_be_bytes([reply[12], reply[13], reply[14], reply[15]]);
    Some((ext_port, lifetime))
}

/// Parse a NAT-PMP external-address response (opcode 128).
pub fn parse_natpmp_address(reply: &[u8]) -> Option<Ipv4Addr> {
    if reply.len() < 12 || reply[0] != 0 || reply[1] != 128 || reply[2] != 0 || reply[3] != 0 {
        return None;
    }
    Some(Ipv4Addr::new(reply[8], reply[9], reply[10], reply[11]))
}

async fn try_natpmp(gateway: Ipv4Addr, internal_port: u16, lifetime_s: u32) -> Option<PortMapping> {
    let reply = udp_request(gateway, &encode_natpmp_map(internal_port, lifetime_s), 16).await?;
    let (ext_port, granted) = parse_natpmp_map(&reply)?;
    let addr_reply = udp_request(gateway, &[0, 0], 12).await?;
    let ext_ip = parse_natpmp_address(&addr_reply)?;
    let mut m = PortMapping::new(
        MapProtocol::NatPmp,
        IpAddr::V4(ext_ip),
        ext_port,
        internal_port,
        granted,
    );
    m.gateway = Some(gateway);
    Some(m)
}

// ── UPnP-IGD (SSDP discovery + SOAP control) ───────────────────────────────

const SSDP_SEARCH: &str = "M-SEARCH * HTTP/1.1\r\n\
HOST: 239.255.255.250:1900\r\n\
MAN: \"ssdp:discover\"\r\n\
MX: 2\r\n\
ST: urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\n\
\r\n";

const WAN_SERVICES: [&str; 2] = [
    "urn:schemas-upnp-org:service:WANIPConnection:1",
    "urn:schemas-upnp-org:service:WANPPPConnection:1",
];

async fn ssdp_discover(timeout: Duration) -> Option<String> {
    let sock = UdpSocket::bind("0.0.0.0:0").await.ok()?;
    sock.send_to(SSDP_SEARCH.as_bytes(), SSDP_ADDR).await.ok()?;
    let mut buf = [0u8; 2048];
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return None;
        }
        match tokio::time::timeout(deadline - now, sock.recv_from(&mut buf)).await {
            Ok(Ok((n, _))) => {
                let text = String::from_utf8_lossy(&buf[..n]);
                for line in text.lines() {
                    if let Some((k, v)) = line.split_once(':') {
                        if k.trim().eq_ignore_ascii_case("location") {
                            return Some(v.trim().to_string());
                        }
                    }
                }
            }
            _ => return None,
        }
    }
}

/// Split `http://host[:port]/path` into `(host, port, path)`.
fn parse_http_url(url: &str) -> Option<(String, u16, String)> {
    let rest = url.strip_prefix("http://")?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !h.contains(']') || h.ends_with(']') => (h, p.parse().ok()?),
        _ => (authority, 80),
    };
    Some((
        host.trim_matches(|c| c == '[' || c == ']').to_string(),
        port,
        path.to_string(),
    ))
}

/// Minimal HTTP/1.1 client for the LAN gateway: one request, whole body back.
async fn http(url: &str, body: Option<&[u8]>, extra_headers: &[(&str, String)]) -> Option<Vec<u8>> {
    let (host, port, path) = parse_http_url(url)?;
    let mut stream = tokio::time::timeout(
        Duration::from_secs(3),
        TcpStream::connect((host.as_str(), port)),
    )
    .await
    .ok()?
    .ok()?;
    let mut req = format!(
        "{} {} HTTP/1.1\r\nHost: {}:{}\r\nConnection: close\r\nUser-Agent: seyd\r\n",
        if body.is_some() { "POST" } else { "GET" },
        path,
        host,
        port
    );
    for (k, v) in extra_headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    if let Some(b) = body {
        req.push_str(&format!("Content-Length: {}\r\n", b.len()));
    }
    req.push_str("\r\n");
    let mut bytes = req.into_bytes();
    if let Some(b) = body {
        bytes.extend_from_slice(b);
    }
    stream.write_all(&bytes).await.ok()?;
    let mut resp = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut resp))
        .await
        .ok()?
        .ok()?;
    let split = find(&resp, b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&resp[..split]).to_ascii_lowercase();
    let status: u16 = head.split_whitespace().nth(1)?.parse().ok()?;
    let mut body = resp[split + 4..].to_vec();
    if head.contains("transfer-encoding: chunked") {
        body = dechunk(&body);
    }
    if status != 200 {
        tracing::debug!(
            "UPnP HTTP {url} → {status}: {}",
            String::from_utf8_lossy(&body)
                .chars()
                .take(200)
                .collect::<String>()
        );
        return None;
    }
    Some(body)
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn dechunk(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(nl) = find(&body[pos..], b"\r\n") {
        let size_str = String::from_utf8_lossy(&body[pos..pos + nl]);
        let size = usize::from_str_radix(size_str.trim().split(';').next().unwrap_or("0"), 16)
            .unwrap_or(0);
        pos += nl + 2;
        if size == 0 || pos + size > body.len() {
            break;
        }
        out.extend_from_slice(&body[pos..pos + size]);
        pos += size + 2;
    }
    out
}

fn text_between<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = s.find(open)? + open.len();
    let end = s[start..].find(close)? + start;
    Some(&s[start..end])
}

/// Locate a WAN connection service in the device description: `(controlURL, serviceType)`.
pub fn find_control_url(desc: &str, base_url: &str) -> Option<(String, String)> {
    let mut rest = desc;
    while let Some(i) = rest.find("<service>") {
        let block_start = i + "<service>".len();
        let block_end = rest[block_start..].find("</service>")? + block_start;
        let block = &rest[block_start..block_end];
        let stype = text_between(block, "<serviceType>", "</serviceType>")
            .map(str::trim)
            .unwrap_or("");
        let ctl = text_between(block, "<controlURL>", "</controlURL>")
            .map(str::trim)
            .unwrap_or("");
        if WAN_SERVICES.contains(&stype) && !ctl.is_empty() {
            return Some((join_url(base_url, ctl), stype.to_string()));
        }
        rest = &rest[block_end..];
    }
    None
}

fn join_url(base: &str, rel: &str) -> String {
    if rel.starts_with("http://") {
        return rel.to_string();
    }
    let Some((host, port, _)) = parse_http_url(base) else {
        return rel.to_string();
    };
    format!(
        "http://{host}:{port}{}{rel}",
        if rel.starts_with('/') { "" } else { "/" }
    )
}

async fn soap(svc: &UpnpService, action: &str, body: &str) -> Option<Vec<u8>> {
    let envelope = format!(
        "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body><u:{action} xmlns:u=\"{}\">{body}</u:{action}></s:Body></s:Envelope>",
        svc.service_type
    );
    http(
        &svc.control_url,
        Some(envelope.as_bytes()),
        &[
            ("Content-Type", "text/xml; charset=\"utf-8\"".to_string()),
            ("SOAPAction", format!("\"{}#{action}\"", svc.service_type)),
        ],
    )
    .await
}

async fn upnp_add(svc: &UpnpService, internal_port: u16, lease: u32) -> bool {
    let body = format!(
        "<NewRemoteHost></NewRemoteHost><NewExternalPort>{internal_port}</NewExternalPort><NewProtocol>UDP</NewProtocol>\
<NewInternalPort>{internal_port}</NewInternalPort><NewInternalClient>{}</NewInternalClient><NewEnabled>1</NewEnabled>\
<NewPortMappingDescription>Seyd agent</NewPortMappingDescription><NewLeaseDuration>{lease}</NewLeaseDuration>",
        svc.internal_ip
    );
    soap(svc, "AddPortMapping", &body).await.is_some()
}

async fn upnp_external_ip(svc: &UpnpService) -> Option<Ipv4Addr> {
    let ext = soap(svc, "GetExternalIPAddress", "").await?;
    let text = String::from_utf8_lossy(&ext);
    text_between(&text, "<NewExternalIPAddress>", "</NewExternalIPAddress>")?
        .trim()
        .parse()
        .ok()
}

async fn try_upnp(internal_port: u16, lifetime_s: u32) -> Option<PortMapping> {
    let location = ssdp_discover(Duration::from_secs(3)).await?;
    let desc = http(&location, None, &[]).await?;
    let (control_url, service_type) = find_control_url(&String::from_utf8_lossy(&desc), &location)?;
    let (host, _, _) = parse_http_url(&location)?;
    let internal_ip = local_ip_towards(host.parse().ok()?)?;
    let svc = UpnpService {
        control_url,
        service_type,
        internal_ip,
    };

    let mut granted = lifetime_s;
    if !upnp_add(&svc, internal_port, lifetime_s).await {
        // Error 725 OnlyPermanentLeasesSupported is common; retry with 0.
        if !upnp_add(&svc, internal_port, 0).await {
            return None;
        }
        granted = 0;
    }
    let ext_ip = upnp_external_ip(&svc).await?;
    let mut m = PortMapping::new(
        MapProtocol::Upnp,
        IpAddr::V4(ext_ip),
        internal_port,
        internal_port,
        granted,
    );
    m.upnp = Some(svc);
    Some(m)
}

// ── public entry points ────────────────────────────────────────────────────

/// Ask the router to forward `internal_port/UDP` to us, trying PCP, NAT-PMP
/// and UPnP in that order. `None` when nothing succeeds (no gateway, CGNAT,
/// UPnP disabled, double NAT). Never panics.
pub async fn map_port(internal_port: u16, lifetime_s: u32) -> Option<PortMapping> {
    let gateway = default_gateway();
    if gateway.is_none() {
        tracing::debug!("no default gateway found — skipping PCP/NAT-PMP");
    }
    if let Some(gw) = gateway {
        if let Some(m) = try_pcp(gw, internal_port, lifetime_s).await {
            return Some(report(m));
        }
        if let Some(m) = try_natpmp(gw, internal_port, lifetime_s).await {
            return Some(report(m));
        }
    }
    if let Some(m) = try_upnp(internal_port, lifetime_s).await {
        return Some(report(m));
    }
    tracing::info!("no router port mapping available (PCP, NAT-PMP and UPnP all declined)");
    None
}

/// Re-request the same mapping before its lease expires. Returns the refreshed
/// mapping, or `None` if the router declined this time (the caller should then
/// re-run discovery and re-announce).
pub async fn renew(mapping: &PortMapping) -> Option<PortMapping> {
    let lifetime = if mapping.lifetime_s == 0 {
        3600
    } else {
        mapping.lifetime_s
    };
    match mapping.protocol {
        MapProtocol::Pcp => try_pcp(mapping.gateway?, mapping.internal_port, lifetime).await,
        MapProtocol::NatPmp => try_natpmp(mapping.gateway?, mapping.internal_port, lifetime).await,
        MapProtocol::Upnp => {
            let svc = mapping.upnp.as_ref()?;
            let granted = if upnp_add(svc, mapping.internal_port, lifetime).await {
                lifetime
            } else if upnp_add(svc, mapping.internal_port, 0).await {
                0
            } else {
                return None;
            };
            let ext_ip = upnp_external_ip(svc).await?;
            let mut m = PortMapping::new(
                MapProtocol::Upnp,
                IpAddr::V4(ext_ip),
                mapping.internal_port,
                mapping.internal_port,
                granted,
            );
            m.upnp = Some(svc.clone());
            Some(m)
        }
    }
}

fn report(m: PortMapping) -> PortMapping {
    if m.routable {
        tracing::info!(
            "{:?} mapped {}:{} → us (lifetime {}s)",
            m.protocol,
            m.external_ip,
            m.external_port,
            m.lifetime_s
        );
    } else {
        tracing::info!(
            "{:?} mapped port {}, but the router's external address ({}) is not globally reachable — double NAT or CGNAT above it",
            m.protocol,
            m.external_port,
            m.external_ip
        );
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcp_request_layout() {
        let nonce = [9u8; 12];
        let r = encode_pcp_map(Ipv4Addr::new(192, 168, 1, 20), &nonce, 4433, 3600);
        assert_eq!(r[0], 2, "version");
        assert_eq!(r[1], 1, "MAP opcode");
        assert_eq!(&r[4..8], &3600u32.to_be_bytes());
        assert_eq!(&r[8..20], &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff]);
        assert_eq!(&r[20..24], &[192, 168, 1, 20]);
        assert_eq!(&r[24..36], &nonce);
        assert_eq!(r[36], 17, "UDP");
        assert_eq!(&r[40..42], &4433u16.to_be_bytes());
        assert_eq!(&r[42..44], &4433u16.to_be_bytes());
        assert_eq!(&r[44..60], &[0u8; 16]);
    }

    #[test]
    fn pcp_response_parse() {
        let mut reply = [0u8; 60];
        reply[0] = 2;
        reply[1] = 0x81;
        reply[4..8].copy_from_slice(&7200u32.to_be_bytes());
        reply[42..44].copy_from_slice(&4433u16.to_be_bytes());
        reply[44..60].copy_from_slice(&Ipv4Addr::new(100, 87, 8, 146).to_ipv6_mapped().octets());
        let (life, port, ip) = parse_pcp_map(&reply).unwrap();
        assert_eq!(
            (life, port, ip),
            (7200, 4433, "100.87.8.146".parse().unwrap())
        );
        assert!(!is_global(ip), "CGNAT external must be non-routable");
        reply[3] = 2;
        assert!(parse_pcp_map(&reply).is_none(), "result code != 0");
    }

    #[test]
    fn natpmp_layouts() {
        let r = encode_natpmp_map(5000, 3600);
        assert_eq!(r, [0, 1, 0, 0, 0x13, 0x88, 0x13, 0x88, 0, 0, 0x0e, 0x10]);
        let mut reply = [0u8; 16];
        reply[1] = 129;
        reply[8..10].copy_from_slice(&5000u16.to_be_bytes());
        reply[10..12].copy_from_slice(&5001u16.to_be_bytes());
        reply[12..16].copy_from_slice(&1800u32.to_be_bytes());
        assert_eq!(parse_natpmp_map(&reply), Some((5001, 1800)));
        let mut addr = [0u8; 12];
        addr[1] = 128;
        addr[8..12].copy_from_slice(&[8, 8, 4, 4]);
        assert_eq!(parse_natpmp_address(&addr), Some(Ipv4Addr::new(8, 8, 4, 4)));
    }

    #[test]
    fn upnp_description_parsing() {
        let desc = r#"<root><device><serviceList>
<service><serviceType>urn:schemas-upnp-org:service:Layer3Forwarding:1</serviceType><controlURL>/l3f</controlURL></service>
<service><serviceType>urn:schemas-upnp-org:service:WANIPConnection:1</serviceType>
<controlURL>/ctl/IPConn</controlURL></service></serviceList></device></root>"#;
        let (url, stype) = find_control_url(desc, "http://192.168.1.1:5000/rootDesc.xml").unwrap();
        assert_eq!(url, "http://192.168.1.1:5000/ctl/IPConn");
        assert_eq!(stype, "urn:schemas-upnp-org:service:WANIPConnection:1");
        assert_eq!(
            parse_http_url("http://10.0.0.1/desc.xml"),
            Some(("10.0.0.1".into(), 80, "/desc.xml".into()))
        );
    }

    #[test]
    fn dechunk_works() {
        assert_eq!(dechunk(b"3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n"), b"abcde");
    }
}
