// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
//! Address classification.
//!
//! `Ipv4Addr::is_global` is unstable in std, and the property we need is
//! precise: *can a host on the public internet route to this address?* The
//! trap is carrier-grade NAT space, `100.64.0.0/10` — it is not RFC 1918, so
//! "is it private?" says no, yet nobody on the internet can reach it. A router
//! under CGNAT will happily install a port mapping and report such an address;
//! advertising it only burns a slot in the pilot's candidate race.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Whether `ip` is reachable from the public internet.
pub fn is_global(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_global_v4(v4),
        IpAddr::V6(v6) => is_global_v6(v6),
    }
}

fn is_global_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || ip.is_unspecified()
        || o[0] == 0
        || (o[0] == 100 && (o[1] & 0xc0) == 64)          // 100.64.0.0/10 CGNAT
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)        // 192.0.0.0/24 IETF protocol assignments
        || (o[0] == 198 && (o[1] & 0xfe) == 18)           // 198.18.0.0/15 benchmarking
        || o[0] >= 240) // 240.0.0.0/4 reserved
}

fn is_global_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_global_v4(v4);
    }
    let s = ip.segments();
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || (s[0] & 0xffc0) == 0xfe80                       // fe80::/10 link-local
        || (s[0] & 0xfe00) == 0xfc00                       // fc00::/7 unique local
        || (s[0] == 0x2001 && s[1] == 0x0db8)              // 2001:db8::/32 documentation
        || (s[0] == 0x0064 && s[1] == 0xff9b)              // 64:ff9b::/96 NAT64 (not a host address)
        || (s[0] == 0x2001 && s[1] == 0x0002 && s[2] == 0)) // 2001:2::/48 benchmarking
}

/// Wrap IPv6 literals in brackets so they can go in a URL authority.
pub fn format_host(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => format!("[{v6}]"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(s: &str) -> bool {
        is_global(s.parse().unwrap())
    }

    #[test]
    fn ipv4_classification() {
        assert!(!g("10.1.2.3"));
        assert!(!g("172.16.0.1"));
        assert!(!g("192.168.86.237"));
        assert!(!g("100.87.8.146"), "CGNAT space must not be global");
        assert!(!g("100.127.255.255"));
        assert!(g("100.128.0.1"), "just past the /10");
        assert!(!g("169.254.1.1"));
        assert!(!g("127.0.0.1"));
        assert!(!g("0.0.0.0"));
        assert!(!g("203.0.113.5"), "documentation space");
        assert!(!g("198.18.0.1"));
        assert!(!g("240.0.0.1"));
        assert!(g("8.8.8.8"));
        assert!(g("95.216.1.1"));
    }

    #[test]
    fn ipv6_classification() {
        assert!(!g("fc00::1"));
        assert!(!g("fd12:3456::1"));
        assert!(!g("fe80::1"));
        assert!(!g("::1"));
        assert!(!g("2001:db8::1"));
        assert!(!g("::ffff:192.168.1.1"), "v4-mapped private");
        assert!(g("::ffff:8.8.8.8"), "v4-mapped public");
        assert!(g("2606:4700::1111"));
        assert!(g("2a00:1450:400f:80d::200e"));
    }

    #[test]
    fn host_formatting() {
        assert_eq!(format_host("1.2.3.4".parse().unwrap()), "1.2.3.4");
        assert_eq!(
            format_host("2606:4700::1111".parse().unwrap()),
            "[2606:4700::1111]"
        );
    }
}
