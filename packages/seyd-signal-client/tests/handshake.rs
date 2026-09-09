//! Drives the client against an in-process fake signal server.

use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use futures_util::{SinkExt, StreamExt};
use seyd_signal_client::{messages::*, Config, Event, Identity};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn authenticates_announces_and_receives_pilot_connecting() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let nonce = vec![7u8; 32];
    let nonce_b64 = base64::engine::general_purpose::STANDARD.encode(&nonce);

    let server = tokio::spawn({
        let nonce = nonce.clone();
        async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            ws.send(Message::Text(
                format!(r#"{{"type":"challenge","nonce":"{nonce_b64}"}}"#).into(),
            ))
            .await
            .unwrap();
            let auth: serde_json::Value =
                serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(auth["type"], "auth");
            assert_eq!(auth["robot_id"], "test-robot");
            let pk = base64::engine::general_purpose::STANDARD
                .decode(auth["public_key"].as_str().unwrap())
                .unwrap();
            let sig = base64::engine::general_purpose::STANDARD
                .decode(auth["sig"].as_str().unwrap())
                .unwrap();
            let vk = VerifyingKey::from_bytes(pk.as_slice().try_into().unwrap()).unwrap();
            vk.verify(&nonce, &Signature::from_slice(&sig).unwrap())
                .expect("signature over nonce");
            ws.send(Message::Text(
                r#"{"type":"auth-ok","robot_id":"test-robot"}"#.into(),
            ))
            .await
            .unwrap();
            let ann: serde_json::Value =
                serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(ann["type"], "announce");
            assert_eq!(ann["p2p_hint"], "likely");
            ws.send(Message::Text(r#"{"type":"pilot-connecting","session_id":"s1","pilot_ip":"9.9.9.9","role":"driver","direction":"robot-listens"}"#.into())).await.unwrap();
            let acc: serde_json::Value =
                serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(acc["type"], "session-accepted");
            assert_eq!(acc["session_id"], "s1");
        }
    });

    let (handle, mut events) = seyd_signal_client::start(
        Config {
            signal_url: format!("ws://127.0.0.1:{port}/ws"),
            robot_id: "test-robot".into(),
            agent_version: "test".into(),
            heartbeat: Duration::from_secs(5),
        },
        Identity::generate(),
    );
    handle
        .announce(Announce {
            candidates: vec![],
            cert_fingerprints: vec![],
            alpns: vec!["h3".into()],
            nat_report: serde_json::json!({}),
            channels: vec![],
            p2p_hint: "likely".into(),
            max_sessions: 1,
            relay: true,
        })
        .await;

    let ev = tokio::time::timeout(Duration::from_secs(3), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ev, Event::Connected);
    let ev = tokio::time::timeout(Duration::from_secs(3), events.recv())
        .await
        .unwrap()
        .unwrap();
    match ev {
        Event::PilotConnecting {
            session_id, role, ..
        } => {
            assert_eq!(session_id, "s1");
            assert_eq!(role, Role::Driver);
            handle.session_accepted("s1", "host");
        }
        other => panic!("unexpected {other:?}"),
    }
    tokio::time::timeout(Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap();
}
