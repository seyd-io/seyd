// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
//! Control-stream vocabulary (docs/protocol/control-stream.md). NDJSON.

use crate::channels::ChannelSpec;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum FromPilot {
    Hello {
        #[serde(default)]
        proto: u8,
        #[serde(default)]
        session_id: String,
        #[serde(default)]
        client: serde_json::Value,
        #[serde(default)]
        token: Option<String>,
    },
    Ping {
        t1: u64,
    },
    Loss {
        ch: u8,
        frame_id: u16,
        #[serde(default)]
        key: bool,
    },
    RequestKeyframe {
        ch: u8,
    },
    SetQos {
        profile: String,
    },
    PilotStats(serde_json::Value),
    Bye,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ToPilot<'a> {
    Welcome {
        session_id: &'a str,
        role: &'a str,
        channels: &'a [ChannelSpec],
        qos: serde_json::Value,
        t_agent_us: u64,
    },
    Denied {
        reason: &'a str,
    },
    Pong {
        t1: u64,
        t2: u64,
    },
    QosAck {
        profile: &'a str,
        qos: serde_json::Value,
        publisher: &'a str,
    },
    AgentStats(serde_json::Value),
}

pub fn encode_line<T: Serialize>(msg: &T) -> String {
    let mut s = serde_json::to_string(msg).expect("control messages are serialisable");
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hello_and_unknown() {
        let m: FromPilot = serde_json::from_str(
            r#"{"type":"hello","proto":2,"session_id":"abc","client":{"kind":"browser"}}"#,
        )
        .unwrap();
        assert!(matches!(m, FromPilot::Hello { proto: 2, .. }));
        let m: FromPilot = serde_json::from_str(r#"{"type":"whatever"}"#).unwrap();
        assert!(matches!(m, FromPilot::Unknown));
        let m: FromPilot =
            serde_json::from_str(r#"{"type":"loss","ch":1,"frame_id":9,"key":true}"#).unwrap();
        assert!(matches!(
            m,
            FromPilot::Loss {
                ch: 1,
                frame_id: 9,
                key: true
            }
        ));
    }

    #[test]
    fn welcome_line() {
        let line = encode_line(&ToPilot::Welcome {
            session_id: "s",
            role: "driver",
            channels: &[],
            qos: seyd_qos::BALANCED.pilot_config(),
            t_agent_us: 1,
        });
        assert!(line.starts_with(r#"{"type":"welcome""#) && line.ends_with('\n'));
    }
}
