// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
//! Seyd agent core — the whole robot side of Seyd, as a Rust API.
//!
//! Bottom to top: channel bookkeeping (`channels`), turning encoded frames
//! into wire chunks with FEC (`packer`), the control-stream vocabulary
//! (`control`), the session engine that binds those to `seyd-transport`
//! (`engine`), and the lifecycle that turns a configuration into a running,
//! announced, self-maintaining robot (`agent`).
//!
//! [`agent::Agent`] is the entry point every host uses — `seydd`, `seyd-ffi`
//! and the SDKs above it (ADR 0004). A host supplies media and consumes
//! [`agent::AgentEvent`]; it never reimplements discovery, signaling or
//! certificate maintenance, which is what keeps the form factors from
//! drifting.

pub mod agent;
pub mod channels;
pub mod control;
pub mod engine;
pub mod packer;

pub use agent::{Agent, AgentConfig, AgentEvent};
pub use channels::{ChannelKind, ChannelSpec};
pub use engine::{Role, VideoFrame};
