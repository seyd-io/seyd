//! Robot-side client for the Seyd signal protocol v2.
//!
//! Contract: `docs/protocol/signal-v2.md`. This crate owns the WebSocket,
//! the challenge/response authentication with the robot's Ed25519 identity,
//! reconnection with backoff, and the re-announce after a reconnect. It knows
//! nothing about QUIC, media or NAT — the caller supplies the `Announce` and
//! consumes `Event`s.

pub mod identity;
pub mod messages;

use futures_util::{SinkExt, StreamExt};
use messages::*;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch, Mutex};
use tokio_tungstenite::tungstenite::Message;

pub use identity::Identity;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("websocket: {0}")]
    Ws(Box<tokio_tungstenite::tungstenite::Error>),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("denied: {0}")]
    Denied(String),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

impl From<tokio_tungstenite::tungstenite::Error> for Error {
    fn from(e: tokio_tungstenite::tungstenite::Error) -> Self {
        Error::Ws(Box::new(e))
    }
}

/// Inbound events the agent must react to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Authenticated and ready; the client re-sends the current announce itself.
    Connected,
    PilotConnecting {
        session_id: String,
        pilot_ip: Option<String>,
        role: Role,
        direction: Direction,
    },
    Punch {
        session_id: String,
        pilot_ip: Option<String>,
    },
    SessionRevoked {
        session_id: String,
        reason: String,
    },
    /// The socket dropped; the client is reconnecting. Sessions already
    /// established over QUIC are unaffected.
    Disconnected,
    /// Authentication was refused. The client keeps retrying slowly, but this
    /// is almost always a configuration problem the operator must see.
    Denied {
        reason: String,
    },
}

#[derive(Clone)]
pub struct Config {
    pub signal_url: String,
    pub robot_id: String,
    pub agent_version: String,
    pub heartbeat: Duration,
}

/// Outbound side. Cheap to clone; safe to call from any task.
#[derive(Clone)]
pub struct Handle {
    tx: mpsc::UnboundedSender<Outbound>,
    announce: Arc<Mutex<Option<Announce>>>,
    sessions: Arc<Mutex<Vec<String>>>,
    status: Arc<Mutex<Option<serde_json::Value>>>,
    connected: watch::Receiver<bool>,
}

impl Handle {
    /// Set the current announce; sent now if connected and again after every reconnect.
    pub async fn announce(&self, a: Announce) {
        *self.announce.lock().await = Some(a.clone());
        // If not connected, the post-auth re-announce delivers it; sending
        // here too would announce twice.
        if *self.connected.borrow() {
            let _ = self.tx.send(Outbound::Announce(a));
        }
    }
    pub async fn set_sessions(&self, ids: Vec<String>) {
        *self.sessions.lock().await = ids;
    }
    pub async fn set_status(&self, status: Option<serde_json::Value>) {
        *self.status.lock().await = status;
    }
    pub fn session_accepted(&self, session_id: &str, path_label: &str) {
        let _ = self.tx.send(Outbound::SessionAccepted {
            session_id: session_id.into(),
            path_label: path_label.into(),
        });
    }
    pub fn session_ended(&self, session_id: &str, reason: &str) {
        let _ = self.tx.send(Outbound::SessionEnded {
            session_id: session_id.into(),
            reason: reason.into(),
        });
    }
    pub fn is_connected(&self) -> bool {
        *self.connected.borrow()
    }
}

/// Start the client. Returns the outbound handle and the inbound event stream.
/// The connection task runs until the handle and receiver are dropped.
pub fn start(cfg: Config, identity: Identity) -> (Handle, mpsc::Receiver<Event>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let (ev_tx, ev_rx) = mpsc::channel(64);
    let (conn_tx, conn_rx) = watch::channel(false);
    let handle = Handle {
        tx,
        announce: Arc::new(Mutex::new(None)),
        sessions: Arc::new(Mutex::new(Vec::new())),
        status: Arc::new(Mutex::new(None)),
        connected: conn_rx,
    };
    tokio::spawn(run(cfg, identity, handle.clone(), rx, ev_tx, conn_tx));
    (handle, ev_rx)
}

const BASE_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// A connection that stayed up this long was healthy, not flapping, so the
/// failure that ends it starts the backoff over. Without this a robot up for
/// hours pays the full `MAX_BACKOFF` for a one-second blip.
const STABLE_SESSION: Duration = Duration::from_secs(30);

async fn run(
    cfg: Config,
    identity: Identity,
    handle: Handle,
    mut rx: mpsc::UnboundedReceiver<Outbound>,
    ev_tx: mpsc::Sender<Event>,
    conn_tx: watch::Sender<bool>,
) {
    let mut backoff = BASE_BACKOFF;
    loop {
        let started = Instant::now();
        let outcome = session(&cfg, &identity, &handle, &mut rx, &ev_tx, &conn_tx).await;
        // Reset before logging, so the delay reported is the one actually slept.
        if started.elapsed() >= STABLE_SESSION {
            backoff = BASE_BACKOFF;
        }
        match outcome {
            Ok(()) => backoff = BASE_BACKOFF,
            Err(Error::Denied(reason)) => {
                tracing::error!(%reason, "signal server denied this robot");
                let _ = ev_tx.send(Event::Denied { reason }).await;
                backoff = MAX_BACKOFF;
            }
            Err(e) => {
                tracing::warn!(error = %e, "signal connection failed; retrying in {backoff:?}")
            }
        }
        let _ = conn_tx.send(false);
        if ev_tx.send(Event::Disconnected).await.is_err() {
            return; // consumer gone
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

async fn session(
    cfg: &Config,
    identity: &Identity,
    handle: &Handle,
    rx: &mut mpsc::UnboundedReceiver<Outbound>,
    ev_tx: &mpsc::Sender<Event>,
    conn_tx: &watch::Sender<bool>,
) -> Result<(), Error> {
    let (ws, _) = tokio_tungstenite::connect_async(&cfg.signal_url).await?;
    let (mut sink, mut stream) = ws.split();
    tracing::info!(url = %cfg.signal_url, "signal connected");

    // ── challenge / auth ─────────────────────────────────────────────────
    let challenge: Inbound = next_json(&mut stream).await?;
    let nonce = match challenge {
        Inbound::Challenge { nonce } => nonce,
        other => {
            return Err(Error::Protocol(format!(
                "expected challenge, got {other:?}"
            )))
        }
    };
    let nonce_bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &nonce)
        .map_err(|e| Error::Protocol(format!("bad nonce: {e}")))?;
    let auth = Outbound::Auth {
        v: 2,
        role: "robot".into(),
        robot_id: cfg.robot_id.clone(),
        public_key: identity.public_key_b64(),
        sig: identity.sign_b64(&nonce_bytes),
        agent_version: cfg.agent_version.clone(),
    };
    sink.send(Message::Text(serde_json::to_string(&auth)?.into()))
        .await?;
    match next_json(&mut stream).await? {
        Inbound::AuthOk { .. } => {}
        Inbound::Denied { reason } => return Err(Error::Denied(reason)),
        other => return Err(Error::Protocol(format!("expected auth-ok, got {other:?}"))),
    }
    let _ = conn_tx.send(true);
    let _ = ev_tx.send(Event::Connected).await;

    // Re-announce whatever we last knew.
    if let Some(a) = handle.announce.lock().await.clone() {
        sink.send(Message::Text(
            serde_json::to_string(&Outbound::Announce(a))?.into(),
        ))
        .await?;
    }

    let mut heartbeat = tokio::time::interval(cfg.heartbeat);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    heartbeat.tick().await; // first tick is immediate; skip it

    loop {
        tokio::select! {
            msg = stream.next() => {
                let msg = match msg {
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(Message::Ping(p))) => { sink.send(Message::Pong(p)).await?; continue; }
                    Some(Ok(Message::Close(_))) | None => return Ok(()),
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => return Err(e.into()),
                };
                match serde_json::from_str::<Inbound>(&msg) {
                    Ok(Inbound::PilotConnecting { session_id, pilot_ip, role, direction }) => {
                        let _ = ev_tx.send(Event::PilotConnecting { session_id, pilot_ip, role, direction }).await;
                    }
                    Ok(Inbound::Punch { session_id, pilot_ip }) => {
                        let _ = ev_tx.send(Event::Punch { session_id, pilot_ip }).await;
                    }
                    Ok(Inbound::SessionRevoked { session_id, reason }) => {
                        let _ = ev_tx.send(Event::SessionRevoked { session_id, reason }).await;
                    }
                    Ok(Inbound::Denied { reason }) => return Err(Error::Denied(reason)),
                    Ok(other) => tracing::debug!(?other, "ignoring signal message"),
                    Err(e) => tracing::debug!(error = %e, raw = %msg, "unparseable signal message"),
                }
            }
            out = rx.recv() => {
                let Some(out) = out else { return Ok(()) };
                sink.send(Message::Text(serde_json::to_string(&out)?.into())).await?;
            }
            _ = heartbeat.tick() => {
                let hb = Outbound::Heartbeat {
                    sessions: handle.sessions.lock().await.clone(),
                    status: handle.status.lock().await.clone(),
                };
                sink.send(Message::Text(serde_json::to_string(&hb)?.into())).await?;
            }
        }
    }
}

async fn next_json<S>(stream: &mut S) -> Result<Inbound, Error>
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        match stream.next().await {
            Some(Ok(Message::Text(t))) => return Ok(serde_json::from_str(&t)?),
            Some(Ok(Message::Close(_))) | None => {
                return Err(Error::Protocol("closed during handshake".into()))
            }
            Some(Ok(_)) => continue,
            Some(Err(e)) => return Err(e.into()),
        }
    }
}
