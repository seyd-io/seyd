# seyd-transport

QUIC on [quinn](https://crates.io/crates/quinn) serving WebTransport (ALPN
`h3`) to browser pilots. ADR 0002.

## Shipped surface

```rust
Cert::generate(&[IpAddr]) -> Cert { cert_der, key_der, fingerprint_sha256, not_after }
cert.fingerprint_hex(); cert.days_left()

Endpoint::bind(Vec<std::net::UdpSocket>, &Cert, TransportConfig) -> Endpoint
endpoint.accept().await -> Option<Session>
endpoint.prober(sock_index) -> Option<&Prober>;  endpoint.socket_index_for(ip)
prober.start(target, interval, duration) / start_default(target) / stop()

session.session_id(); session.remote_addr()
session.max_datagram_size(); session.send_buffer_space()
session.send_datagram(Bytes) -> Result<(), SendError{Blocked|TooLarge|Closed}>
session.take_datagrams() -> Option<mpsc::Receiver<Bytes>>
session.control().await -> Option<(ControlReader, ControlWriter)>   // pilot's first bidi stream
seyd_transport::control::{read_line, write_line}                    // NDJSON helpers
session.stats() -> PathStats { rtt_ms, min_rtt_ms, cwnd, delivery_kbps, lost_packets, current_mtu }
session.close(code, reason); session.closed().await
```

`TransportConfig`: `congestion` (Bbr default | Cubic), `datagram_send_buffer`
(750 KiB), `datagram_receive_buffer`, `max_idle` 10 s, `keep_alive` 2 s,
`initial_mtu` 1200 with DPLPMTUD on.

## Decisions and gotchas

* **h3-webtransport 0.1.2 is used** (with h3 0.0.8, h3-quinn 0.0.10 `datagram`
  feature, h3-datagram 0.0.2) for extended CONNECT and session streams only.
  Everything HTTP/3 lives in `src/webtransport.rs`; replace that one file if
  the crate ever blocks us.
* **Datagrams bypass h3-webtransport.** The WebTransport datagram framing is a
  single QUIC varint (CONNECT stream id / 4) before the payload. We write it
  ourselves on the quinn connection so `send_datagram` can refuse with
  `Blocked` when `datagram_send_buffer_space()` is short — quinn would
  otherwise *evict older queued datagrams*, i.e. tear a frame. Callers compare
  a whole frame against `send_buffer_space()` before sending its first chunk.
* h3's `StreamId` raw value is feature-gated; the prefix is obtained by
  encoding an empty datagram through `h3_datagram::datagram::Datagram`.
* Chrome cert rules (all enforced here): ECDSA P-256, validity ≤ 14 days (we
  use 13), IP addresses in `subjectAltName`. Chrome first reports
  `maxDatagramSize` 1024 for a fresh session; larger sizes need PMTUD to run.
* Sockets are handed in pre-bound and never rebound; a `try_clone` of each is
  kept for hole-punch probes (`Prober`), which quinn's ownership would
  otherwise make impossible.
* `Session::stats().delivery_kbps` is the UDP *transmit* rate over the last
  `stats()` interval, not acked bytes (quinn does not expose those).

## Verification

```
cargo test -p seyd-transport
cargo build -p seyd-transport --example wt-server
tools/.venv/bin/python3 packages/seyd-transport/examples/wt-check.py
```

`wt-check.py` starts the echo server, drives real headless Chrome over the
DevTools protocol (no puppeteer), opens a WebTransport session pinned by
`serverCertificateHashes`, and asserts 20 datagram echoes and 3 control-line
echoes. Measured locally: session ready in ~12 ms, datagram RTT 0.2–0.9 ms
(median 0.5 ms). `SEYD_WT_ATTACH=<fingerprint>` attaches to a running server;
`SEYD_WT_NETLOG=<file>` captures Chrome's net-log.
