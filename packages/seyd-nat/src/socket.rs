// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
//! Binding the UDP sockets the QUIC server will own.

use socket2::{Domain, Protocol, Socket, Type};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};

/// Bind the UDP sockets QUIC will use: IPv4 on `0.0.0.0:port` and, when
/// `want_ipv6`, IPv6 on `[::]:port` with `IPV6_V6ONLY` so the two do not
/// collide. Both are non-blocking and `SO_REUSEADDR` — except on Windows,
/// where that option means something else (see [`set_port_policy`]).
///
/// These are bound here, up front, because STUN has to run on the very socket
/// that later receives QUIC. Binding a temporary socket and letting the QUIC
/// stack rebind the port makes the advertised reflexive address a guess — on
/// port-preserving home routers it usually held, on strict ones it silently
/// did not. Same socket, never rebound, no guess.
///
/// IPv6 failure is not fatal: the agent continues IPv4-only.
pub fn bind_sockets(port: u16, want_ipv6: bool) -> std::io::Result<Vec<UdpSocket>> {
    let mut socks = Vec::with_capacity(2);

    let s4 = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    set_port_policy(&s4)?;
    s4.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)).into())?;
    s4.set_nonblocking(true)?;
    socks.push(UdpSocket::from(s4));

    if want_ipv6 {
        match bind_v6(port) {
            Ok(s6) => socks.push(s6),
            Err(e) => tracing::info!("IPv6 listener unavailable ({e}) — continuing IPv4-only"),
        }
    }
    Ok(socks)
}

fn bind_v6(port: u16) -> std::io::Result<UdpSocket> {
    let s6 = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
    set_port_policy(&s6)?;
    s6.set_only_v6(true)?;
    s6.bind(&SocketAddr::from((Ipv6Addr::UNSPECIFIED, port)).into())?;
    s6.set_nonblocking(true)?;
    Ok(UdpSocket::from(s6))
}

/// Who else may bind our port.
///
/// On Unix `SO_REUSEADDR` lets a restarted agent take the port straight back
/// (the scripts `pkill` and relaunch `seydd` in one breath). On Windows the
/// same option lets *any other process* bind the port while we hold it and
/// receive our QUIC packets instead of us — so there it is the opposite,
/// `SO_EXCLUSIVEADDRUSE`, and a restart simply waits for the old process to
/// exit, which Windows frees immediately for UDP.
fn set_port_policy(s: &Socket) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawSocket;
        use windows_sys::Win32::Networking::WinSock::{
            setsockopt, SOL_SOCKET, SO_EXCLUSIVEADDRUSE,
        };
        let on: i32 = 1;
        // SAFETY: a live socket handle, a valid 4-byte option value, and the
        // length that matches it; socket2 has no wrapper for this option.
        let rc = unsafe {
            setsockopt(
                s.as_raw_socket() as usize,
                SOL_SOCKET,
                SO_EXCLUSIVEADDRUSE,
                (&on as *const i32).cast(),
                std::mem::size_of::<i32>() as i32,
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
    #[cfg(not(windows))]
    {
        s.set_reuse_address(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binds_both_families_on_an_ephemeral_port() {
        let socks = bind_sockets(0, true).unwrap();
        assert!(!socks.is_empty());
        assert!(socks[0].local_addr().unwrap().is_ipv4());
        if socks.len() == 2 {
            assert!(socks[1].local_addr().unwrap().is_ipv6());
        }
    }
}
