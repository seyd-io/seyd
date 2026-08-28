//! Seyd agent engine — the part of the agent that knows nothing about the
//! network: channel bookkeeping, turning encoded frames into wire chunks with
//! FEC (`packer`), and the control-stream message vocabulary (`control`).
//! The session engine that binds these to `seyd-transport` lives in
//! `engine`.

pub mod channels;
pub mod control;
pub mod engine;
pub mod packer;
