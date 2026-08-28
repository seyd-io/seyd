//! Local addresses worth advertising.

use crate::addr::is_global;
use std::net::IpAddr;

/// Non-loopback local addresses: IPv4 (private *and* public — the private
/// one is the same-LAN candidate, the fastest path there is when the pilot
/// shares the network) and globally routable IPv6 only. Link-local and
/// unique-local IPv6 are filtered: a pilot elsewhere on the internet cannot
/// reach them, so advertising them only wastes a connection attempt.
pub fn interfaces() -> Vec<IpAddr> {
    let mut out: Vec<IpAddr> = Vec::new();
    let Ok(addrs) = if_addrs::get_if_addrs() else {
        return out;
    };
    for a in addrs {
        let ip = a.ip();
        if a.is_loopback() || ip.is_multicast() {
            continue;
        }
        let keep = match ip {
            IpAddr::V4(v4) => !v4.is_link_local() && !v4.is_unspecified(),
            IpAddr::V6(v6) => is_global(IpAddr::V6(v6)),
        };
        if keep && !out.contains(&ip) {
            out.push(ip);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn no_loopback_or_link_local() {
        for ip in super::interfaces() {
            assert!(!ip.is_loopback());
            if let std::net::IpAddr::V6(v6) = ip {
                assert_ne!(v6.segments()[0] & 0xffc0, 0xfe80);
            }
        }
    }
}
