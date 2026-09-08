use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChannelKind {
    Video,
    Sensor,
    Command,
}

/// A declared channel; serialises to the `{id, kind, name, codec, fps}` shape
/// used by `welcome` and `announce`, plus `layers` where the publisher offers
/// more than one encoding of the same picture (ADR 0008).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelSpec {
    pub id: u8,
    pub kind: ChannelKind,
    pub name: String,
    /// WebCodecs identifier, carried to the pilot's decoder untouched. With
    /// simulcast it must admit the *highest* layer: the pilot configures once,
    /// and `avc1.42001f` is Baseline level 3.1, whose 3600-macroblock ceiling is
    /// exactly 1280x720.
    pub codec: String,
    #[serde(default)]
    pub fps: u32,
    /// Simulcast layers, lowest first. Empty or one-element means a single
    /// stream and no selection ever happens — the pre-simulcast behaviour.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<seyd_qos::simulcast::VideoLayer>,
}
