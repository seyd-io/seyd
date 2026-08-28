# ADR 0002 — QUIC stack: quinn

**Status:** accepted (2026-08-28)

## Context

The production agent core is Rust with a C ABI (owner decision). It must serve
WebTransport to browsers, speak plain QUIC to native peers, run on ARM Linux
(Jetson, Raspberry Pi 5, RK3588) and ship inside self-contained Python wheels.
The prototype's aioquic behaviours that must survive: STUN and hole-punch
probes on the *same* UDP socket QUIC uses, self-signed ECDSA P-256 certs
pinned via `serverCertificateHashes`, a bounded datagram send queue that the
application can observe, and per-frame admission control.

Candidates: quinn, quiche (Cloudflare), s2n-quic (AWS). MsQuic (C) was the
earlier plan and is superseded by the Rust decision.

## Decision

**quinn.**

| | quinn | quiche | s2n-quic |
|---|---|---|---|
| Pure Rust, rustls | yes | BoringSSL via FFI | yes |
| HTTP/3 + WebTransport server | `h3` + `h3-quinn` + `h3-webtransport` | own h3, no WT plumbing | none |
| Pluggable UDP socket (pre-bound, shared with STUN/probes) | `AsyncUdpSocket` | sans-IO (possible, more work) | limited |
| Custom cert verifier for fingerprint pinning | rustls `ServerCertVerifier` | BoringSSL config | rustls |
| Congestion control | NewReno, Cubic, BBR; `Controller` trait | Cubic, BBR, BBR2 | Cubic, BBR |
| DPLPMTUD | yes | yes | yes |
| Cross-compile to aarch64 | trivial (`cross`) | needs BoringSSL toolchain | ok |

The decisive factor is WebTransport: quinn is the only stack with an existing
Rust WebTransport server layer, and everything is pure Rust, so ARM builds and
wheel packaging are mechanical.

Transport policy on top of quinn:
* ALPNs `h3` (browser WebTransport, CONNECT path `/seyd`) and `seyd/2`
  (native QUIC: same wire format, no HTTP/3 framing) on one socket.
* BBR with pacing for the media path; Cubic selectable for A/B.
* Datagram send buffer sized to ~2 frames of the profile bitrate; Seyd keeps
  its own single-slot frame buffer *in front of* quinn so a backlog can never
  hide inside the stack.
* `rcgen` ECDSA P-256 certificates, 13-day validity, SAN = every advertised
  IP; rotated at day 10 with a one-hour overlap.

## Risks

`h3-webtransport` is labelled experimental. The surface Seyd needs is small
(extended CONNECT, SETTINGS `H3_DATAGRAM` + `ENABLE_WEBTRANSPORT`, the
quarter-stream-id datagram prefix, session-scoped streams). If the crate
blocks us, an in-house `seyd-transport/src/webtransport.rs` over plain `h3`
is the fallback. The first spike in Milestone A is Chrome connecting with
`serverCertificateHashes`.

quinn's BBR is also marked experimental; validate on netem in Milestone A and
fall back to Cubic plus tighter ABR if it misbehaves.
