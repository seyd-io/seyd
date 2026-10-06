# Security

## Reporting a vulnerability

Report privately through GitHub's private vulnerability reporting on this
repository (*Security* → *Report a vulnerability*), which reaches the
maintainer without a public issue. Do not open a public issue for a
vulnerability.

You will get an acknowledgement within five working days. We ask for 90 days
of coordinated disclosure from the report, or until a fix ships, whichever
is sooner; we will credit you in the release notes unless you prefer not.

## Scope

- The robot agent: the Rust crates under `packages/`, `seydd`, `libseyd`
  and the C, Python and Rust SDKs.
- The web pilot: `@seyd/core` and `@seyd/web`.
- The protocols documented under `docs/protocol/`.
- The hosted Seyd cloud at `seyd-signal-flj7s44j4a-ew.a.run.app`, whose
  source is a private repository; reports about it are welcome here too.

Out of scope: the robots, cameras and operator applications that customers
build on Seyd, and denial-of-service findings that require more bandwidth
than the target's uplink.

## What the design already assumes

Seyd's threat model is in `docs/adr/0007-identity.md` and the *Enrolment and
access* page of the developer documentation: robots prove an Ed25519 key by
challenge-response, pilots present a short-lived ES256 session token the
robot verifies against the cloud's published key, every session is QUIC with
TLS 1.3 and a per-robot certificate pinned by fingerprint, and the cloud never
sees video on a direct session. A report that shows any of those statements
to be false is exactly what this file is for.
