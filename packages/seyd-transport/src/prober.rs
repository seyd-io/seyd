//! NAT hole punching: send small UDP packets from the QUIC socket toward the
//! pilot's address so an address-restricted NAT opens a mapping before the
//! pilot's QUIC Initial arrives. Repeated because a lone packet can be lost and
//! Chrome retries its handshake with backoff. One destination port: for the
//! NAT types this helps, filtering is by source address and the port is
//! irrelevant; where the port would matter it is unknowable anyway.

use std::net::{SocketAddr, UdpSocket};
use std::sync::Mutex;
use std::time::Duration;

pub struct Prober {
    sock: UdpSocket,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Prober {
    pub(crate) fn new(sock: UdpSocket) -> Self {
        Self {
            sock,
            task: Mutex::new(None),
        }
    }

    /// Start probing `target`; a running probe is replaced.
    pub fn start(&self, target: SocketAddr, interval: Duration, duration: Duration) {
        self.stop();
        let sock = match self.sock.try_clone() {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("prober: cannot clone socket: {e}");
                return;
            }
        };
        let handle = tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + duration;
            let mut ticker = tokio::time::interval(interval);
            let mut sent = 0u32;
            while tokio::time::Instant::now() < deadline {
                ticker.tick().await;
                // Non-blocking socket: a would-block here just skips a tick.
                if sock.send_to(&[0u8], target).is_ok() {
                    sent += 1;
                }
            }
            tracing::debug!(%target, sent, "prober finished");
        });
        *self.task.lock().unwrap() = Some(handle);
    }

    pub fn start_default(&self, target: SocketAddr) {
        self.start(target, Duration::from_millis(250), Duration::from_secs(12));
    }

    pub fn stop(&self) {
        if let Some(t) = self.task.lock().unwrap().take() {
            t.abort();
        }
    }
}

impl Drop for Prober {
    fn drop(&mut self) {
        self.stop();
    }
}
