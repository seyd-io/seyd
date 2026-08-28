//! Seyd transport: QUIC on quinn, serving WebTransport (ALPN `h3`) to browser
//! pilots. See `docs/adr/0002-quinn.md`.
//!
//! What this crate owns: sockets it is *handed* (never binds its own — STUN and
//! hole-punch probes go out of the same socket QUIC uses), certificates pinned
//! by `serverCertificateHashes`, the accept loop, and a [`Session`] per
//! WebTransport session with datagrams, the pilot-opened control stream, and
//! path statistics. It knows nothing about chunks, FEC or channels.
//!
//! Datagram admission is the caller's job through [`Session::send_buffer_space`]
//! and [`Session::send_datagram`]. quinn discards *older* queued datagrams when
//! its buffer is full, which is exactly the torn-frame outcome Seyd forbids, so
//! `send_datagram` refuses with [`SendError::Blocked`] instead of letting that
//! happen.

pub mod cert;
mod endpoint;
mod prober;
pub mod session;
mod webtransport;

pub use cert::Cert;
pub use endpoint::{Congestion, Endpoint, TransportConfig};
pub use prober::Prober;
pub use session as control;
pub use session::{ControlReader, ControlWriter, PathStats, SendError, Session};
