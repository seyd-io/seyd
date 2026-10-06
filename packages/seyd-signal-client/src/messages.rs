// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
//! Signal v2 message types (docs/protocol/signal-v2.md), robot side.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    Driver,
    Observer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Direction {
    RobotListens,
    PilotListens,
    Both,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub url: String,
    pub label: String,
    pub priority: u32,
    pub needs_probe: bool,
    pub family: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelInfo {
    pub id: u8,
    pub kind: String,
    pub name: String,
    pub codec: String,
    #[serde(default)]
    pub fps: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Announce {
    pub candidates: Vec<Candidate>,
    pub cert_fingerprints: Vec<String>,
    pub alpns: Vec<String>,
    pub nat_report: serde_json::Value,
    pub channels: Vec<ChannelInfo>,
    pub p2p_hint: String,
    pub max_sessions: u32,
    /// Whether this robot will accept a relayed session when no direct path
    /// connects (ADR 0010). The cloud offers a relay to pilots only if so.
    #[serde(default)]
    pub relay: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Outbound {
    Auth {
        v: u8,
        role: String,
        robot_id: String,
        public_key: String,
        sig: String,
        agent_version: String,
    },
    Announce(Announce),
    Heartbeat {
        sessions: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        status: Option<serde_json::Value>,
    },
    SessionAccepted {
        session_id: String,
        path_label: String,
    },
    SessionEnded {
        session_id: String,
        reason: String,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Inbound {
    Challenge {
        nonce: String,
    },
    AuthOk {
        #[serde(default)]
        robot_id: String,
    },
    Denied {
        reason: String,
    },
    PilotConnecting {
        session_id: String,
        #[serde(default)]
        pilot_ip: Option<String>,
        #[serde(default = "driver")]
        role: Role,
        #[serde(default = "robot_listens")]
        direction: Direction,
    },
    Punch {
        session_id: String,
        #[serde(default)]
        pilot_ip: Option<String>,
    },
    SessionRevoked {
        session_id: String,
        #[serde(default)]
        reason: String,
    },
    /// The pilot could not connect directly and has attached to the cloud
    /// relay; dial `url`, attach with `token`, and serve the session there.
    RelayOpen {
        session_id: String,
        url: String,
        token: String,
        #[serde(default = "driver")]
        role: Role,
        #[serde(default)]
        pilot_ip: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

fn driver() -> Role {
    Role::Driver
}
fn robot_listens() -> Direction {
    Direction::RobotListens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announce_tags_as_type_announce() {
        let a = Announce {
            candidates: vec![],
            cert_fingerprints: vec!["ab".into()],
            alpns: vec!["h3".into()],
            nat_report: serde_json::json!({}),
            channels: vec![],
            p2p_hint: "likely".into(),
            max_sessions: 4,
            relay: true,
        };
        let v = serde_json::to_value(Outbound::Announce(a)).unwrap();
        assert_eq!(v["type"], "announce");
        assert_eq!(v["alpns"][0], "h3");
    }

    #[test]
    fn inbound_parses_and_tolerates_unknown() {
        let m: Inbound = serde_json::from_str(r#"{"type":"pilot-connecting","session_id":"abc","pilot_ip":"1.2.3.4","role":"observer","direction":"robot-listens"}"#).unwrap();
        match m {
            Inbound::PilotConnecting { role, .. } => assert_eq!(role, Role::Observer),
            _ => panic!(),
        }
        let m: Inbound = serde_json::from_str(r#"{"type":"something-new","x":1}"#).unwrap();
        assert!(matches!(m, Inbound::Unknown));
    }
}
