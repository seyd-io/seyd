//! `seydd.toml` — see docs/protocol/seydd.md.

use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub agent: Agent,
    #[serde(default, rename = "channel")]
    pub channels: Vec<Channel>,
    #[serde(default)]
    pub publisher_control: Option<PublisherControl>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Agent {
    pub robot_id: String,
    pub signal_url: String,
    #[serde(default = "default_credential_path")]
    pub credential_path: PathBuf,
    #[serde(default = "default_quic_port")]
    pub quic_port: u16,
    #[serde(default = "yes")]
    pub ipv6: bool,
    #[serde(default = "yes")]
    pub port_mapping: bool,
    #[serde(default = "default_profile")]
    pub qos_profile: String,
    #[serde(default = "default_max_sessions")]
    pub max_sessions: u32,
    /// Skip discovery and advertise this host only (LAN testing).
    #[serde(default)]
    pub host_override: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChannelKind {
    Video,
    Sensor,
    Command,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Channel {
    pub kind: ChannelKind,
    pub name: String,
    #[serde(default)]
    pub input: Option<String>,
    #[serde(default)]
    pub output: Option<String>,
    #[serde(default = "default_codec")]
    pub codec: String,
    #[serde(default)]
    pub fps: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PublisherControl {
    pub udp: String,
}

fn default_credential_path() -> PathBuf {
    PathBuf::from("/var/lib/seyd/robot.key")
}
fn default_quic_port() -> u16 {
    4433
}
fn yes() -> bool {
    true
}
fn default_profile() -> String {
    "balanced".into()
}
fn default_max_sessions() -> u32 {
    4
}
fn default_codec() -> String {
    "json".into()
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
        let cfg: Config = toml::from_str(&text)?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> anyhow::Result<()> {
        if self.channels.len() > 255 {
            anyhow::bail!("at most 255 channels");
        }
        if seyd_qos::get(&self.agent.qos_profile).is_none() {
            anyhow::bail!("unknown qos_profile {:?}", self.agent.qos_profile);
        }
        for c in &self.channels {
            match c.kind {
                ChannelKind::Video | ChannelKind::Sensor if c.input.is_none() => {
                    anyhow::bail!("channel {:?}: {:?} needs `input`", c.name, c.kind)
                }
                ChannelKind::Command if c.output.is_none() => {
                    anyhow::bail!("channel {:?}: command needs `output`", c.name)
                }
                _ => {}
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_reference_config() {
        let cfg: Config = toml::from_str(
            r#"
[agent]
robot_id = "seyd-demo"
signal_url = "ws://localhost:8080/ws"

[[channel]]
kind = "video"
name = "main"
input = "rtsp://cam/x"
codec = "avc1.42001f"
fps = 25

[[channel]]
kind = "command"
name = "ptz"
output = "udp://127.0.0.1:5004"

[publisher_control]
udp = "127.0.0.1:5003"
"#,
        )
        .unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.agent.quic_port, 4433);
        assert_eq!(cfg.channels[0].kind, ChannelKind::Video);
        assert_eq!(cfg.channels[1].codec, "json");
    }
}
