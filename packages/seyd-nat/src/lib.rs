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
pub struct NetworkWatch {
    /// One message per detected change; the payload is a human-readable reason.
    pub events: tokio::sync::mpsc::Receiver<String>,
    /// Inject a synthetic change (e.g. after repeated port-mapping renewal
    /// failures) without waiting for the poller.
    pub force: tokio::sync::mpsc::Sender<String>,
}

/// Watch for interface-address changes by polling [`ifaces::interfaces`]
/// every 10 s. Polling rather than netlink/`SCDynamicStore` keeps it portable;
/// a 10 s detection delay is negligible against QUIC's idle timeout. Callers
/// treat a message as "re-run [`gather()`] on the same sockets".
pub fn watch_network_changes() -> NetworkWatch {
    watch_network_changes_with(ifaces::interfaces, std::time::Duration::from_secs(10))
}

/// Test seam: same watcher with an injected address provider and period.
pub fn watch_network_changes_with<F>(mut provider: F, period: std::time::Duration) -> NetworkWatch
where
    F: FnMut() -> Vec<std::net::IpAddr> + Send + 'static,
{
    let (ev_tx, events) = tokio::sync::mpsc::channel(4);
    let (force, mut force_rx) = tokio::sync::mpsc::channel::<String>(4);
    // A clone lives inside the task so `force_rx.recv()` can never yield
    // `None` and busy-loop the select.
    let keep = force.clone();
    tokio::spawn(async move {
        let _keep = keep;
        let mut prev: std::collections::BTreeSet<std::net::IpAddr> =
            provider().into_iter().collect();
        let mut tick = tokio::time::interval(period);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tick.tick().await; // the immediate first tick is the baseline, not a change
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    let now: std::collections::BTreeSet<std::net::IpAddr> =
                        provider().into_iter().collect();
                    if now != prev {
                        let added = now.difference(&prev).count();
                        let removed = prev.difference(&now).count();
                        prev = now;
                        // try_send: a slow consumer coalesces changes instead
                        // of queueing a burst of stale ones.
                        let _ = ev_tx.try_send(format!(
                            "interface addresses changed (+{added} -{removed})"
                        ));
                    }
                }
                reason = force_rx.recv() => {
                    if let Some(reason) = reason {
                        let _ = ev_tx.try_send(reason);
                    }
                }
            }
        }
    });
    NetworkWatch { events, force }
}

#[cfg(test)]
mod watch_tests {
    use std::net::IpAddr;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[tokio::test]
    async fn fires_on_address_change_and_force_only() {
        let addrs: Arc<Mutex<Vec<IpAddr>>> =
            Arc::new(Mutex::new(vec!["192.168.1.2".parse().unwrap()]));
        let p = addrs.clone();
        let mut w = super::watch_network_changes_with(
            move || p.lock().unwrap().clone(),
            Duration::from_millis(20),
        );
        // Stable set: no event.
        assert!(
            tokio::time::timeout(Duration::from_millis(120), w.events.recv())
                .await
                .is_err(),
            "no event expected while addresses are stable"
        );
        // Change the set: exactly one event.
        addrs.lock().unwrap().push("10.0.0.7".parse().unwrap());
        let reason = tokio::time::timeout(Duration::from_millis(500), w.events.recv())
            .await
            .expect("event expected after change")
            .unwrap();
        assert!(reason.contains("+1"), "{reason}");
        // Forced event passes through.
        w.force.send("renewal failed".into()).await.unwrap();
        let reason = tokio::time::timeout(Duration::from_millis(500), w.events.recv())
            .await
            .expect("forced event expected")
            .unwrap();
        assert_eq!(reason, "renewal failed");
    }
}
