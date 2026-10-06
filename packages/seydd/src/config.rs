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
    /// The robot's id as enrolled with the cloud; what pilots and the console see.
    pub robot_id: String,
    /// The signal server's WebSocket URL, `wss://host/ws`. Enrolment derives
    /// its REST address from it.
    pub signal_url: String,
    /// Ed25519 seed file, created on first run. The private half never leaves
    /// the robot; enrolment registers the public half (ADR 0007).
    #[serde(default = "default_credential_path")]
    pub credential_path: PathBuf,
    /// UDP port the QUIC endpoint binds; the port a router forward or pinhole
    /// must open.
    #[serde(default = "default_quic_port")]
    pub quic_port: u16,
    /// Bind and advertise IPv6 as well as IPv4.
    #[serde(default = "yes")]
    pub ipv6: bool,
    /// Ask the router for a port mapping (PCP, NAT-PMP, UPnP) during discovery,
    /// so a robot behind a consumer router is reachable without a manual forward.
    #[serde(default = "yes")]
    pub port_mapping: bool,
    /// The QoS ceiling: `latency`, `balanced` or `quality`.
    #[serde(default = "default_profile")]
    pub qos_profile: String,
    /// Concurrent pilot sessions (one driver, the rest observers).
    #[serde(default = "default_max_sessions")]
    pub max_sessions: u32,
    /// Skip discovery and advertise this host only (LAN testing).
    #[serde(default)]
    pub host_override: Option<String>,
    /// Ask for the cheapest useful recovery point (LTR → intra-refresh → IDR)
    /// rather than always demanding a keyframe. Set false for a publisher that
    /// mishandles any `kind` but `idr`.
    #[serde(default = "yes")]
    pub recovery_ladder: bool,
    /// Serve a session through the cloud relay when the pilot could not
    /// connect directly (ADR 0010). The pilot only asks after its direct
    /// attempts fail, and its HUD says so; set false to refuse relayed
    /// sessions altogether.
    #[serde(default = "yes")]
    pub relay: bool,
    /// One-time enrolment token, redeemed on first start when no credential
    /// exists yet. `SEYD_ENROLMENT_TOKEN` overrides it — prefer the
    /// environment, so a provisioning secret need not be written to disk.
    #[serde(default)]
    pub enrolment_token: Option<String>,
}

impl Agent {
    /// The enrolment token to use, environment first.
    pub fn enrolment_token(&self) -> Option<String> {
        std::env::var("SEYD_ENROLMENT_TOKEN")
            .ok()
            .filter(|t| !t.is_empty())
            .or_else(|| self.enrolment_token.clone())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChannelKind {
    Video,
    Sensor,
    Command,
}

/// One simulcast layer of a video channel: the same picture, encoded again at a
/// different operating point (ADR 0008). Declared as `[[channel.layer]]`.
///
/// ```toml
/// [[channel]]
/// kind  = "video"
/// name  = "main"
/// codec = "avc1.42001f"   # must admit the *highest* layer
/// fps   = 25
///
///   [[channel.layer]]
///   name  = "low"
///   input = "rtsp://cam:554/Streaming/Channels/102"
///
///   [[channel.layer]]
///   name  = "high"
///   input = "rtsp://cam:554/Streaming/Channels/101"
///   activate_above_kbps = 1800
/// ```
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    /// The layer's name, reported in `layer` messages and the HUD.
    pub name: String,
    /// The stream carrying this encoding, `rtsp://` or `rtp://`.
    pub input: String,
    /// The ABR target at or above which this layer is the right choice. The
    /// lowest layer is the base and is used whenever nothing better is
    /// affordable, so leaving it at 0 is correct.
    #[serde(default)]
    pub activate_above_kbps: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Channel {
    /// `video`, `sensor` or `command`.
    pub kind: ChannelKind,
    /// The channel's name as shown to the pilot and used in `SeydSession.send`.
    pub name: String,
    #[serde(default)]
    /// Where the bytes come from: `rtsp://` or `rtp://` for video, `udp://` for
    /// a sensor. Required for `video` and `sensor` channels without layers.
    pub input: Option<String>,
    #[serde(default)]
    /// Where a `command` channel delivers each driver message, as one UDP
    /// datagram per message.
    pub output: Option<String>,
    #[serde(default = "default_codec")]
    /// A WebCodecs codec string for video (`avc1.42001f`, `hev1.…`, `mjpeg`);
    /// `json` or `octet-stream` for messages.
    pub codec: String,
    #[serde(default)]
    /// Nominal frame rate of a video channel; 0 for messages.
    pub fps: u32,
    /// Simulcast layers. Empty means the single `input` above — the ordinary
    /// case, and what every pre-simulcast config keeps doing.
    #[serde(default, rename = "layer")]
    pub layers: Vec<Layer>,
    /// The most this video channel's publisher can encode, in kbps; 0 (the
    /// default) means it has no limit of its own. Set it when the encoder
    /// tops out below the QoS profile's ceiling — a drone whose highest
    /// setting is 4 Mbps under the `quality` profile's 6 — so the rate
    /// controller's range, floor and `video-config` requests stay within
    /// what the publisher can deliver.
    #[serde(default)]
    pub max_bitrate_kbps: u32,
}

impl Channel {
    /// The channel's inputs as `(layer_id, url)`, lowest layer first. A
    /// single-`input` channel yields exactly one, at layer 0, so callers need no
    /// special case.
    pub fn inputs(&self) -> Vec<(u8, String)> {
        if self.layers.is_empty() {
            return self.input.iter().map(|u| (0u8, u.clone())).collect();
        }
        let mut ls: Vec<&Layer> = self.layers.iter().collect();
        ls.sort_by_key(|l| l.activate_above_kbps);
        ls.iter()
            .enumerate()
            .map(|(i, l)| (i as u8, l.input.clone()))
            .collect()
    }

    /// The channel's layers as the core declares them, lowest first.
    pub fn layer_specs(&self) -> Vec<seyd_qos::simulcast::VideoLayer> {
        let mut ls: Vec<&Layer> = self.layers.iter().collect();
        ls.sort_by_key(|l| l.activate_above_kbps);
        ls.iter()
            .enumerate()
            .map(|(i, l)| seyd_qos::simulcast::VideoLayer {
                id: i as u8,
                name: l.name.clone(),
                activate_above_kbps: l.activate_above_kbps,
            })
            .collect()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct PublisherControl {
    /// `host:port` the daemon sends its JSON control messages to, one per datagram.
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
            if !c.layers.is_empty() {
                if c.kind != ChannelKind::Video {
                    anyhow::bail!(
                        "channel {:?}: [[channel.layer]] is for video channels only — \
                         simulcast selects between encodings of the same picture",
                        c.name
                    );
                }
                if c.input.is_some() {
                    anyhow::bail!(
                        "channel {:?}: has both `input` and [[channel.layer]]. Give the \
                         single stream as `input`, or every stream as a layer — not both",
                        c.name
                    );
                }
                if c.layers.len() > u8::MAX as usize {
                    anyhow::bail!("channel {:?}: at most 255 layers", c.name);
                }
                let mut seen = std::collections::HashSet::new();
                for l in &c.layers {
                    if !seen.insert(&l.name) {
                        anyhow::bail!(
                            "channel {:?}: two layers named {:?}; names identify them in \
                             logs and the pilot HUD, so they must differ",
                            c.name,
                            l.name
                        );
                    }
                }
                // Two layers that activate at the same target can never both be
                // chosen, which is a config bug that would otherwise present as
                // a rung that is silently never used.
                let mut points: Vec<u32> = c.layers.iter().map(|l| l.activate_above_kbps).collect();
                points.sort_unstable();
                if points.windows(2).any(|w| w[0] == w[1]) {
                    anyhow::bail!(
                        "channel {:?}: two layers share an `activate_above_kbps`; one of \
                         them could never be selected",
                        c.name
                    );
                }
                if points[0] != 0 {
                    anyhow::bail!(
                        "channel {:?}: the lowest layer must have `activate_above_kbps = 0` \
                         (it is the base, used when nothing better is affordable); the \
                         lowest here is {}",
                        c.name,
                        points[0]
                    );
                }
                continue;
            }
            match c.kind {
                ChannelKind::Video if c.input.is_none() => anyhow::bail!(
                    "channel {:?}: video needs `input`, or [[channel.layer]] entries for \
                     simulcast",
                    c.name
                ),
                ChannelKind::Sensor if c.input.is_none() => {
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
        // A single-input channel is layer 0 and nothing else.
        assert_eq!(
            cfg.channels[0].inputs(),
            vec![(0, "rtsp://cam/x".to_string())]
        );
        assert!(cfg.channels[0].layer_specs().is_empty());
    }

    fn simulcast_cfg(layers: &str) -> Config {
        toml::from_str(&format!(
            r#"
[agent]
robot_id = "seyd-demo"
signal_url = "ws://localhost:8080/ws"

[[channel]]
kind = "video"
name = "main"
codec = "avc1.420028"
fps = 25
{layers}
"#
        ))
        .unwrap()
    }

    const LADDER: &str = r#"
  [[channel.layer]]
  name = "high"
  input = "rtsp://cam/101"
  activate_above_kbps = 1800

  [[channel.layer]]
  name = "low"
  input = "rtsp://cam/102"
"#;

    #[test]
    fn layers_are_ordered_lowest_first_whatever_the_file_says() {
        // Declaration order must not decide layer ids: the ladder is defined by
        // the bitrate each rung needs.
        let cfg = simulcast_cfg(LADDER);
        cfg.validate().unwrap();
        let c = &cfg.channels[0];
        assert_eq!(
            c.inputs(),
            vec![
                (0, "rtsp://cam/102".to_string()),
                (1, "rtsp://cam/101".to_string())
            ]
        );
        let specs = c.layer_specs();
        assert_eq!((specs[0].id, specs[0].name.as_str()), (0, "low"));
        assert_eq!((specs[1].id, specs[1].activate_above_kbps), (1, 1800));
    }

    #[test]
    fn input_and_layers_together_is_refused() {
        let cfg = simulcast_cfg(&format!("input = \"rtsp://cam/x\"\n{LADDER}"));
        let e = cfg.validate().unwrap_err().to_string();
        assert!(e.contains("not both"), "{e}");
    }

    #[test]
    fn a_ladder_without_a_base_layer_is_refused() {
        // Every rung gated behind a bitrate means a bad link gets no video at
        // all, which would present as a dead channel rather than a config error.
        let cfg = simulcast_cfg(
            r#"
  [[channel.layer]]
  name = "low"
  input = "rtsp://cam/102"
  activate_above_kbps = 500

  [[channel.layer]]
  name = "high"
  input = "rtsp://cam/101"
  activate_above_kbps = 1800
"#,
        );
        let e = cfg.validate().unwrap_err().to_string();
        assert!(e.contains("activate_above_kbps = 0"), "{e}");
    }

    #[test]
    fn layers_that_can_never_both_be_chosen_are_refused() {
        let cfg = simulcast_cfg(
            r#"
  [[channel.layer]]
  name = "a"
  input = "rtsp://cam/102"

  [[channel.layer]]
  name = "b"
  input = "rtsp://cam/101"
"#,
        );
        let e = cfg.validate().unwrap_err().to_string();
        assert!(e.contains("could never be selected"), "{e}");
    }

    #[test]
    fn layers_on_a_command_channel_are_refused() {
        let cfg: Config = toml::from_str(
            r#"
[agent]
robot_id = "r"
signal_url = "ws://localhost:8080/ws"

[[channel]]
kind = "command"
name = "ptz"
output = "udp://127.0.0.1:5004"

  [[channel.layer]]
  name = "low"
  input = "rtsp://cam/102"
"#,
        )
        .unwrap();
        let e = cfg.validate().unwrap_err().to_string();
        assert!(e.contains("video channels only"), "{e}");
    }

    #[test]
    fn a_video_channel_with_neither_input_nor_layers_says_both_options() {
        let cfg = simulcast_cfg("");
        let e = cfg.validate().unwrap_err().to_string();
        assert!(e.contains("input") && e.contains("channel.layer"), "{e}");
    }
}
