// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
//! Candidate gathering and the NatReport.

use crate::addr::{format_host, is_global};
use crate::portmap::{self, PortMapping};
use crate::stun::{self, NatType};
use serde::Serialize;
use std::net::{IpAddr, UdpSocket};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Candidate {
    /// `https://host:port/seyd`
    pub url: String,
    pub label: String,
    pub priority: u32,
    pub needs_probe: bool,
    pub family: u8,
}

/// How hopeful the pilot should be. Drives its P2P deadline so a doomed
/// attempt fails fast while a plausible one gets the full window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Hint {
    Likely,
    LanOnly,
    None,
}

#[derive(Debug, Clone)]
pub struct GatherOpts {
    pub port: u16,
    /// Skip discovery and advertise only this address (dev / manual forwarding).
    pub host_override: Option<IpAddr>,
    pub port_mapping: bool,
    pub ipv6: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct NatReport {
    pub ipv4: Ipv4Report,
    pub ipv6: Ipv6Report,
    pub portmap: PortmapReport,
    pub candidates: Vec<CandidateReport>,
    /// Filled by the cloud prober, never by the agent.
    pub prober: Option<serde_json::Value>,
    pub hint: Hint,
}

#[derive(Debug, Clone, Serialize)]
pub struct Ipv4Report {
    pub local: Vec<IpAddr>,
    pub public: Option<IpAddr>,
    pub nat: NatType,
    pub cgnat: bool,
    pub gateway_external: Option<IpAddr>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Ipv6Report {
    pub present: bool,
    pub global: Vec<IpAddr>,
    pub inbound_ok: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PortmapReport {
    pub protocol: Option<portmap::MapProtocol>,
    pub external: Option<String>,
    pub error: Option<String>,
    pub lease_s: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CandidateReport {
    pub label: String,
    pub ok: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct Gathered {
    pub candidates: Vec<Candidate>,
    /// Every advertised IP — goes into the certificate's SubjectAlternativeName.
    pub san_ips: Vec<IpAddr>,
    pub nat_report: NatReport,
    pub p2p_hint: Hint,
    /// Kept so the caller can renew the lease.
    pub mapping: Option<PortMapping>,
}

fn is_cgnat(ip: IpAddr) -> bool {
    matches!(ip, IpAddr::V4(v4) if v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64)
}

struct Builder {
    port: u16,
    candidates: Vec<Candidate>,
    san_ips: Vec<IpAddr>,
}

impl Builder {
    fn add(&mut self, ip: IpAddr, port: u16, label: &str, priority: u32, needs_probe: bool) {
        let url = format!("https://{}:{}/seyd", format_host(ip), port);
        if self.candidates.iter().any(|c| c.url == url) {
            return;
        }
        tracing::info!("candidate [{label:<8} prio {priority:>3}] {url}");
        self.candidates.push(Candidate {
            url,
            label: label.to_string(),
            priority,
            needs_probe,
            family: if ip.is_ipv6() { 6 } else { 4 },
        });
        if !self.san_ips.contains(&ip) {
            self.san_ips.push(ip);
        }
        let _ = self.port;
    }
}

/// Work out every address a pilot could reach us on.
///
/// Mirrors the prototype's `gather_candidates`: host candidates first (a LAN
/// address wins instantly on a shared network; global IPv6 has no NAT at
/// all), then an explicit router port mapping, then the STUN-reflexive
/// address — suppressed under symmetric NAT, where it can only waste a slot
/// in the pilot's race. `socks[0]` must be the IPv4 socket QUIC will use.
pub async fn gather(socks: &[UdpSocket], opts: &GatherOpts) -> Gathered {
    let mut b = Builder {
        port: opts.port,
        candidates: Vec::new(),
        san_ips: Vec::new(),
    };
    let local = crate::ifaces::interfaces();
    let local_v4: Vec<IpAddr> = local.iter().copied().filter(IpAddr::is_ipv4).collect();
    let global_v6: Vec<IpAddr> = local
        .iter()
        .copied()
        .filter(|ip| ip.is_ipv6() && opts.ipv6)
        .collect();

    if let Some(ip) = opts.host_override {
        b.add(ip, opts.port, "host-override", 250, false);
        let report = NatReport {
            ipv4: Ipv4Report {
                local: local_v4,
                public: None,
                nat: NatType::Unknown,
                cgnat: false,
                gateway_external: None,
            },
            ipv6: Ipv6Report {
                present: !global_v6.is_empty(),
                global: global_v6,
                inbound_ok: None,
            },
            portmap: PortmapReport {
                protocol: None,
                external: None,
                error: Some("skipped: host override".into()),
                lease_s: None,
            },
            candidates: b
                .candidates
                .iter()
                .map(|c| CandidateReport {
                    label: c.label.clone(),
                    ok: None,
                })
                .collect(),
            prober: None,
            hint: Hint::Likely,
        };
        return Gathered {
            candidates: b.candidates,
            san_ips: b.san_ips,
            nat_report: report,
            p2p_hint: Hint::Likely,
            mapping: None,
        };
    }

    // Host candidates.
    for ip in &local {
        match ip {
            IpAddr::V4(_) => b.add(*ip, opts.port, "host", 240, false),
            IpAddr::V6(_) if opts.ipv6 => b.add(*ip, opts.port, "host6", 200, true), // firewall pinhole still helps
            _ => {}
        }
    }

    // Router port mapping — an installed mapping beats an inferred one.
    let mut mapping = None;
    let mut portmap_report = PortmapReport {
        protocol: None,
        external: None,
        error: None,
        lease_s: None,
    };
    let mut gateway_external = None;
    if opts.port_mapping {
        match portmap::map_port(opts.port, 3600).await {
            Some(m) => {
                gateway_external = Some(m.external_ip);
                portmap_report.protocol = Some(m.protocol);
                portmap_report.external = Some(format!(
                    "{}:{}",
                    format_host(m.external_ip),
                    m.external_port
                ));
                portmap_report.lease_s = Some(m.lifetime_s);
                if m.routable {
                    b.add(m.external_ip, m.external_port, "portmap", 220, false);
                    mapping = Some(m);
                } else {
                    portmap_report.error =
                        Some("external address not globally routable (double NAT or CGNAT)".into());
                    // Still keep it for lease renewal: it removes the inner NAT from the path.
                    mapping = Some(m);
                }
            }
            None => portmap_report.error = Some("no-gateway-response".into()),
        }
    } else {
        portmap_report.error = Some("disabled".into());
    }
    let advertised_mapping = mapping.as_ref().filter(|m| m.routable).is_some();

    // Server-reflexive candidate.
    let nat = match socks.first() {
        Some(s) => stun::discover(s).await,
        None => stun::NatDiscovery::default(),
    };
    if let Some(r) = nat.reflexive {
        b.add(r.ip(), r.port(), "srflx", 150, true);
    } else if nat.symmetric {
        tracing::info!("skipping reflexive candidate — symmetric NAT");
    }

    let hint = if advertised_mapping || nat.reflexive.is_some() || !global_v6.is_empty() {
        Hint::Likely
    } else if !b.candidates.is_empty() {
        Hint::LanOnly
    } else {
        Hint::None
    };
    tracing::info!("NAT type: {:?} — P2P outlook: {:?}", nat.nat_type(), hint);

    let public = nat
        .reflexive
        .map(|a| a.ip())
        .or(nat.observed.first().map(|a| a.ip()))
        .or(gateway_external.filter(|ip| is_global(*ip)));
    let cgnat = gateway_external.map(is_cgnat).unwrap_or(false)
        || (nat.symmetric && gateway_external.is_none() && local_v4.iter().any(|ip| is_cgnat(*ip)));

    let report = NatReport {
        ipv4: Ipv4Report {
            local: local_v4,
            public,
            nat: nat.nat_type(),
            cgnat,
            gateway_external,
        },
        ipv6: Ipv6Report {
            present: !global_v6.is_empty(),
            global: global_v6,
            inbound_ok: None,
        },
        portmap: portmap_report,
        candidates: b
            .candidates
            .iter()
            .map(|c| CandidateReport {
                label: c.label.clone(),
                ok: None,
            })
            .collect(),
        prober: None,
        hint,
    };
    let mut candidates = b.candidates;
    candidates.sort_by_key(|c| std::cmp::Reverse(c.priority));
    Gathered {
        candidates,
        san_ips: b.san_ips,
        nat_report: report,
        p2p_hint: hint,
        mapping,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_ordering_and_dedup() {
        let mut b = Builder {
            port: 4433,
            candidates: vec![],
            san_ips: vec![],
        };
        b.add("1.2.3.4".parse().unwrap(), 4433, "srflx", 150, true);
        b.add("192.168.1.5".parse().unwrap(), 4433, "host", 240, false);
        b.add("2606:4700::1111".parse().unwrap(), 4433, "host6", 200, true);
        b.add("192.168.1.5".parse().unwrap(), 4433, "host", 240, false);
        let mut c = b.candidates.clone();
        c.sort_by_key(|c| std::cmp::Reverse(c.priority));
        let labels: Vec<&str> = c.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["host", "host6", "srflx"]);
        assert_eq!(c[1].url, "https://[2606:4700::1111]:4433/seyd");
        assert_eq!(c[1].family, 6);
        assert_eq!(b.san_ips.len(), 3);
    }

    #[test]
    fn cgnat_detection() {
        assert!(is_cgnat("100.87.8.146".parse().unwrap()));
        assert!(!is_cgnat("100.128.0.1".parse().unwrap()));
        assert!(!is_cgnat("10.0.0.1".parse().unwrap()));
    }

    #[test]
    fn report_serialises_to_plan_shape() {
        let r = NatReport {
            ipv4: Ipv4Report {
                local: vec!["192.168.1.20".parse().unwrap()],
                public: None,
                nat: NatType::Symmetric,
                cgnat: true,
                gateway_external: Some("100.87.3.2".parse().unwrap()),
            },
            ipv6: Ipv6Report {
                present: false,
                global: vec![],
                inbound_ok: None,
            },
            portmap: PortmapReport {
                protocol: Some(portmap::MapProtocol::Pcp),
                external: Some("100.87.3.2:4433".into()),
                error: None,
                lease_s: Some(3600),
            },
            candidates: vec![CandidateReport {
                label: "host".into(),
                ok: None,
            }],
            prober: None,
            hint: Hint::LanOnly,
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["ipv4"]["nat"], "symmetric");
        assert_eq!(v["ipv4"]["cgnat"], true);
        assert_eq!(v["portmap"]["protocol"], "pcp");
        assert_eq!(v["hint"], "lan-only");
        assert!(v["prober"].is_null());
        assert!(v["ipv6"]["inbound_ok"].is_null());
    }
}
