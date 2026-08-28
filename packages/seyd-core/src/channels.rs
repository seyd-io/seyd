use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChannelKind {
    Video,
    Sensor,
    Command,
}

/// A declared channel; serialises to the `{id, kind, name, codec, fps}` shape
/// used by `welcome` and `announce`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelSpec {
    pub id: u8,
    pub kind: ChannelKind,
    pub name: String,
    pub codec: String,
    #[serde(default)]
    pub fps: u32,
}
