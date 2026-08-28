//! Echo server for the WebTransport spike: prints the certificate fingerprint,
//! echoes datagrams, and echoes control lines as `{"echo": <line>}`.
use seyd_transport::{session, Cert, Endpoint, TransportConfig};
use std::net::UdpSocket;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let port: u16 = std::env::args()
        .nth(1)
        .map(|p| p.parse().unwrap())
        .unwrap_or(4433);
    let sock = UdpSocket::bind(("0.0.0.0", port))?;
    let cert = Cert::generate(&["127.0.0.1".parse()?])?;
    println!("FINGERPRINT {}", cert.fingerprint_hex());
    let ep = Endpoint::bind(vec![sock], &cert, TransportConfig::default())?;
    println!("LISTENING {:?}", ep.local_addrs());
    while let Some(s) = ep.accept().await {
        tokio::spawn(async move {
            let s = std::sync::Arc::new(s);
            println!("SESSION {} from {}", s.session_id(), s.remote_addr());
            let dg = s.clone();
            let mut rx = s.take_datagrams().unwrap();
            tokio::spawn(async move {
                while let Some(d) = rx.recv().await {
                    let _ = dg.send_datagram(d);
                }
            });
            if let Some((mut r, mut w)) = s.control().await {
                while let Some(line) = session::read_line(&mut r).await {
                    let reply = format!("{{\"echo\":{line}}}");
                    if session::write_line(&mut w, &reply).await.is_err() {
                        break;
                    }
                }
            }
            s.closed().await;
            println!("CLOSED {} stats={:?}", s.session_id(), s.stats());
        });
    }
    Ok(())
}
