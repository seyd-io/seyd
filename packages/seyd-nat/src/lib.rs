//! Reachability for the Seyd agent.
//!
//! The pilot is always the initiator, so Seyd's traversal problem is the
//! narrow one: **make the agent reachable as a server**. In descending order
//! of reliability that is an installed router port mapping, global IPv6, a
//! STUN-reflexive address plus hole punching, and a manually forwarded port.
//! Port-restricted and symmetric NAT are unreachable by construction while the
//! pilot is a browser (PROTOTYPE.md, "NAT traversal — why this is not ICE").
//! There is no relay: what this crate cannot make reachable is reported in
//! [`NatReport`] so the pilot can tell the operator how to fix the network.
//!
//! Everything runs on the UDP sockets the QUIC server will later own
//! ([`bind_sockets`]) — STUN in particular has to see the same NAT mapping
//! QUIC will use, so the sockets are bound once and never rebound.

pub mod addr;
pub mod gather;
pub mod ifaces;
pub mod portmap;
pub mod socket;
pub mod stun;

pub use addr::{format_host, is_global};
pub use gather::{gather, Candidate, GatherOpts, Gathered, Hint, NatReport};
pub use portmap::{map_port, renew, MapProtocol, PortMapping};
pub use socket::bind_sockets;
pub use stun::{discover, NatDiscovery, NatType};

/// Network-change notifications.
///
/// **Stub.** Returns a receiver that never fires. The real implementation
/// (netlink on Linux, `SCDynamicStore`/route socket on macOS) lands with the
/// reliability work in PLAN.md §1.8; callers should treat a message as
/// "re-run [`gather`]".
pub fn watch_network_changes() -> tokio::sync::mpsc::Receiver<()> {
    let (_tx, rx) = tokio::sync::mpsc::channel(1);
    // `_tx` is dropped here on purpose: the receiver yields `None` forever
    // rather than pretending changes never happen.
    rx
}
