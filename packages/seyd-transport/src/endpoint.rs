use crate::cert::Cert;
use crate::prober::Prober;
use crate::session::Session;
use crate::webtransport;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Congestion {
    /// Model-based; keeps the modem queue shallow. The default for media.
    Bbr,
    /// Loss-based; selectable for A/B on netem.
    Cubic,
}

#[derive(Debug, Clone)]
pub struct TransportConfig {
    pub congestion: Congestion,
    /// Bytes quinn may hold in its outgoing datagram buffer. Seyd keeps its own
    /// single-slot frame buffer in front, so this only needs to absorb about
    /// two frames — small enough that a backlog cannot hide inside the stack.
    pub datagram_send_buffer: usize,
    pub datagram_receive_buffer: usize,
    pub max_idle: Duration,
    pub keep_alive: Duration,
    /// Starting MTU assumption; DPLPMTUD probes upward from here.
    pub initial_mtu: u16,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            congestion: Congestion::Bbr,
            datagram_send_buffer: 750 * 1024,
            datagram_receive_buffer: 2 * 1024 * 1024,
            max_idle: Duration::from_secs(10),
            keep_alive: Duration::from_secs(2),
            initial_mtu: 1200,
        }
    }
}

/// One quinn endpoint per pre-bound socket, all feeding one session queue.
pub struct Endpoint {
    endpoints: Vec<quinn::Endpoint>,
    probers: Vec<Prober>,
    sessions: tokio::sync::Mutex<mpsc::Receiver<Session>>,
    local_addrs: Vec<std::net::SocketAddr>,
}

fn install_crypto_provider() {
    // Idempotent; a second install returns Err, which is fine.
    let _ = rustls::crypto::ring::default_provider().install_default();
}

impl Endpoint {
    /// Serve on already-bound UDP sockets (the caller binds them, runs STUN on
    /// them, and hands them over; they are never rebound).
    pub fn bind(
        socks: Vec<std::net::UdpSocket>,
        cert: &Cert,
        cfg: TransportConfig,
    ) -> anyhow::Result<Endpoint> {
        install_crypto_provider();
        anyhow::ensure!(!socks.is_empty(), "at least one socket is required");

        let cert_chain = vec![rustls::pki_types::CertificateDer::from(
            cert.cert_der.clone(),
        )];
        let key = rustls::pki_types::PrivateKeyDer::Pkcs8(
            rustls::pki_types::PrivatePkcs8KeyDer::from(cert.key_der.clone()),
        );
        let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(cert_chain, key)?;
        tls.alpn_protocols = vec![b"h3".to_vec()];
        tls.max_early_data_size = 0;

        let mut transport = quinn::TransportConfig::default();
        transport
            .max_idle_timeout(Some(cfg.max_idle.try_into()?))
            .keep_alive_interval(Some(cfg.keep_alive))
            .datagram_send_buffer_size(cfg.datagram_send_buffer)
            .datagram_receive_buffer_size(Some(cfg.datagram_receive_buffer))
            .initial_mtu(cfg.initial_mtu)
            .mtu_discovery_config(Some(quinn::MtuDiscoveryConfig::default()))
            // Streams are only the control channel and, later, reliable
            // channels; datagrams carry media.
            .max_concurrent_bidi_streams(16u32.into())
            .max_concurrent_uni_streams(16u32.into());
        match cfg.congestion {
            Congestion::Bbr => {
                transport.congestion_controller_factory(Arc::new(
                    quinn::congestion::BbrConfig::default(),
                ));
            }
            Congestion::Cubic => {
                transport.congestion_controller_factory(Arc::new(
                    quinn::congestion::CubicConfig::default(),
                ));
            }
        }

        let quic_server = quinn::crypto::rustls::QuicServerConfig::try_from(tls)?;
        let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(quic_server));
        server_config.transport_config(Arc::new(transport));

        let (tx, rx) = mpsc::channel(16);
        let next_id = Arc::new(AtomicU64::new(1));
        let mut endpoints = Vec::new();
        let mut probers = Vec::new();
        let mut local_addrs = Vec::new();
        for sock in socks {
            sock.set_nonblocking(true)?;
            let probe_sock = sock.try_clone()?;
            local_addrs.push(sock.local_addr()?);
            let ep = quinn::Endpoint::new(
                quinn::EndpointConfig::default(),
                Some(server_config.clone()),
                sock,
                Arc::new(quinn::TokioRuntime),
            )?;
            probers.push(Prober::new(probe_sock));
            let accept_ep = ep.clone();
            let tx = tx.clone();
            let next_id = next_id.clone();
            tokio::spawn(async move {
                while let Some(incoming) = accept_ep.accept().await {
                    let tx = tx.clone();
                    let id = next_id.fetch_add(1, Ordering::Relaxed);
                    tokio::spawn(async move {
                        let conn = match incoming.await {
                            Ok(c) => c,
                            Err(e) => {
                                tracing::debug!("handshake failed: {e}");
                                return;
                            }
                        };
                        tracing::info!(session = id, peer = %conn.remote_address(), "quic connection");
                        if let Err(e) = webtransport::serve(conn, id, tx).await {
                            tracing::debug!(session = id, "h3 connection ended: {e:#}");
                        }
                    });
                }
            });
            endpoints.push(ep);
        }
        Ok(Endpoint {
            endpoints,
            probers,
            sessions: tokio::sync::Mutex::new(rx),
            local_addrs,
        })
    }

    /// Next accepted WebTransport session, or `None` once the endpoint is closed.
    pub async fn accept(&self) -> Option<Session> {
        self.sessions.lock().await.recv().await
    }

    pub fn local_addrs(&self) -> &[std::net::SocketAddr] {
        &self.local_addrs
    }

    /// Hole-punch prober bound to socket `index` (same order as `bind`).
    pub fn prober(&self, index: usize) -> Option<&Prober> {
        self.probers.get(index)
    }

    /// Index of the socket whose address family matches `ip`.
    pub fn socket_index_for(&self, ip: std::net::IpAddr) -> Option<usize> {
        self.local_addrs
            .iter()
            .position(|a| a.is_ipv6() == ip.is_ipv6())
    }

    pub fn close(&self) {
        for ep in &self.endpoints {
            ep.close(0u32.into(), b"shutdown");
        }
    }
}
