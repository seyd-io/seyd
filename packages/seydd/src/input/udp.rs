// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
//! UDP datagram source for sensor channels: one datagram = one message.

use bytes::Bytes;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

pub async fn run(addr: String, tx: mpsc::Sender<Bytes>) {
    let sock = match UdpSocket::bind(&addr).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(%addr, error = %e, "sensor input: bind failed");
            return;
        }
    };
    tracing::info!(%addr, "sensor input listening");
    let mut buf = vec![0u8; 65536];
    while let Ok(n) = sock.recv(&mut buf).await {
        if tx.send(Bytes::copy_from_slice(&buf[..n])).await.is_err() {
            return;
        }
    }
}

/// Parse `udp://host:port` into a bindable/sendable address string.
pub fn addr_from_url(url: &str) -> anyhow::Result<String> {
    let u = url::Url::parse(url)?;
    if u.scheme() != "udp" {
        anyhow::bail!("expected udp:// url, got {url:?}");
    }
    Ok(format!(
        "{}:{}",
        u.host_str().unwrap_or("127.0.0.1"),
        u.port()
            .ok_or_else(|| anyhow::anyhow!("{url:?}: port required"))?
    ))
}
