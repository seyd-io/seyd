//! HTTP/3 + WebTransport session establishment on top of `h3-webtransport`.
//!
//! This is the only file that knows HTTP/3. If the `h3-webtransport` crate
//! proves unusable it is replaced here alone: the rest of the crate sees a
//! [`Session`] with datagrams and a control stream.
//!
//! Datagrams bypass h3-webtransport's handlers: the WebTransport framing is a
//! single QUIC varint (the CONNECT stream id / 4) in front of the payload, and
//! writing it ourselves lets `Session::send_datagram` gate on quinn's buffer
//! space instead of letting quinn evict older datagrams.

use crate::session::{split_prefix, varint, ControlReader, ControlWriter, Session};
use bytes::{Buf, Bytes};
use h3::ext::Protocol;
use h3_webtransport::server::{AcceptedBi, WebTransportSession};
use http::{Method, StatusCode};
use tokio::sync::{mpsc, oneshot};

pub const PATH: &str = "/seyd";

pub(crate) async fn serve(
    conn: quinn::Connection,
    id: u64,
    sessions: mpsc::Sender<Session>,
) -> anyhow::Result<()> {
    let h3_conn = h3_quinn::Connection::new(conn.clone());
    let mut h3: h3::server::Connection<h3_quinn::Connection, Bytes> = h3::server::builder()
        .enable_webtransport(true)
        .enable_extended_connect(true)
        .enable_datagram(true)
        .max_webtransport_sessions(1)
        .send_grease(false)
        .build(h3_conn)
        .await?;

    loop {
        let resolver = match h3.accept().await {
            Ok(Some(r)) => r,
            Ok(None) => {
                tracing::debug!(session = id, "h3 accept returned None (peer done)");
                return Ok(());
            }
            Err(e) => {
                tracing::debug!(session = id, "h3 accept error: {e}");
                return Err(e.into());
            }
        };
        let (req, mut stream) = resolver.resolve_request().await?;
        tracing::debug!(session = id, method = %req.method(), path = %req.uri().path(), protocol = ?req.extensions().get::<Protocol>(), "h3 request");
        let is_wt_connect = req.method() == Method::CONNECT
            && req.extensions().get::<Protocol>() == Some(&Protocol::WEB_TRANSPORT);
        if !(is_wt_connect && req.uri().path() == PATH) {
            tracing::debug!(session = id, method = %req.method(), path = %req.uri().path(), "rejecting non-seyd request");
            let resp = http::Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(())
                .unwrap();
            let _ = stream.send_response(resp).await;
            let _ = stream.finish().await;
            continue;
        }

        let wt = WebTransportSession::accept(req, stream, h3).await?;
        // The datagram prefix is the QUIC varint of (CONNECT stream id / 4).
        // h3 keeps the raw id behind a feature gate, so let h3-datagram's own
        // encoder produce the prefix: an empty-payload datagram *is* the prefix.
        let session_stream_id: h3::quic::StreamId = wt.session_id().into();
        let mut encoded =
            h3_datagram::datagram::Datagram::new(session_stream_id, Bytes::new()).encode();
        let prefix = encoded.copy_to_bytes(encoded.remaining());
        let quarter = split_prefix(prefix.clone())
            .map(|(q, _)| q)
            .ok_or_else(|| anyhow::anyhow!("bad datagram prefix"))?;
        debug_assert_eq!(varint(quarter), prefix);

        let (dg_tx, dg_rx) = mpsc::channel::<Bytes>(1024);
        let (ctl_tx, ctl_rx) = oneshot::channel::<(ControlReader, ControlWriter)>();
        let session = Session::new(id, conn.clone(), prefix, dg_rx, ctl_rx);

        // Inbound datagrams: strip the prefix, drop anything not ours.
        let dg_conn = conn.clone();
        tokio::spawn(async move {
            let mut dropped = 0u64;
            while let Ok(buf) = dg_conn.read_datagram().await {
                match split_prefix(buf) {
                    Some((q, payload)) if q == quarter => {
                        if dg_tx.try_send(payload).is_err() {
                            dropped += 1;
                        }
                    }
                    _ => {}
                }
            }
            if dropped > 0 {
                tracing::debug!(
                    session = id,
                    dropped,
                    "inbound datagrams dropped (receiver slow)"
                );
            }
        });

        // Streams: the first bidi stream is the control stream; the session
        // object stays alive here for as long as the connection does.
        tokio::spawn(async move {
            let mut ctl_tx = Some(ctl_tx);
            loop {
                match wt.accept_bi().await {
                    Ok(Some(AcceptedBi::BidiStream(_sid, bidi))) => {
                        if let Some(tx) = ctl_tx.take() {
                            let (r, w) = tokio::io::split(bidi);
                            let reader: ControlReader = tokio::io::BufReader::new(Box::new(r));
                            let writer: ControlWriter = Box::new(w);
                            let _ = tx.send((reader, writer));
                        } else {
                            tracing::debug!(session = id, "extra bidi stream ignored");
                        }
                    }
                    Ok(Some(AcceptedBi::Request(_req, mut s))) => {
                        let resp = http::Response::builder()
                            .status(StatusCode::NOT_FOUND)
                            .body(())
                            .unwrap();
                        let _ = s.send_response(resp).await;
                    }
                    Ok(None) => break,
                    Err(e) => {
                        tracing::debug!(session = id, "session stream loop ended: {e}");
                        break;
                    }
                }
            }
        });

        if sessions.send(session).await.is_err() {
            conn.close(0u32.into(), b"endpoint closed");
        }
        return Ok(());
    }
}
