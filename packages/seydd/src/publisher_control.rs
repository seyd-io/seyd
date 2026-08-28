//! Best-effort JSON to the robot's publisher (docs/protocol/seydd.md).
//! Fire-and-forget and never fatal: a publisher that ignores this keeps
//! streaming on whatever it was configured with.

use std::net::UdpSocket;

pub struct PublisherControl {
    sock: Option<UdpSocket>,
    target: String,
}

impl PublisherControl {
    pub fn new(target: Option<&str>) -> Self {
        let sock = target.and_then(|_| {
            UdpSocket::bind("0.0.0.0:0")
                .and_then(|s| s.set_nonblocking(true).map(|_| s))
                .ok()
        });
        Self {
            sock,
            target: target.unwrap_or_default().to_string(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.sock.is_some()
    }

    pub fn send(&self, msg: &serde_json::Value) -> bool {
        let Some(sock) = &self.sock else { return false };
        match sock.send_to(msg.to_string().as_bytes(), &self.target) {
            Ok(_) => true,
            Err(e) => {
                tracing::debug!(error = %e, "publisher control send failed");
                false
            }
        }
    }
}
