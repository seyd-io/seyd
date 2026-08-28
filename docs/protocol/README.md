# Seyd protocol contracts

These documents are the shared contract between the Rust core, the TypeScript
pilot SDK and the cloud. Implementations are written against them, not against
each other. Change a document first, then the code, and add an ADR for anything
that changes wire bytes.

- `chunks.md` — datagram framing: wire v2 header (ADR 0001), FEC blocks, frames, channels
- `control-stream.md` — the pilot↔agent control channel (NDJSON on one bidi stream)
- `signal-v2.md` — the robot/pilot/console ↔ cloud WebSocket protocol
- `seydd.md` — the daemon's configuration and its robot-side UDP interfaces
