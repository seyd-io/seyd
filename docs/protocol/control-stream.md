# Control stream

One bidirectional stream, opened by the **pilot** immediately after the
WebTransport session is established. Newline-delimited JSON, UTF-8, one object
per line. **The pilot speaks first**: the agent learns the stream from the
first inbound line, and nothing it writes before then is delivered.

| direction | type | fields |
|---|---|---|
| P→A | `hello` | `proto: 2`, `session_id` (from the signal `offer`), `client: {kind: "browser"\|"native", name, version}`, `token?` |
| A→P | `welcome` | `session_id`, `role: "driver"\|"observer"`, `channels: [{id, kind, name, codec, fps}]`, `qos: {profile, deadline_delta_ms, deadline_key_ms, on_loss}`, `t_agent_us` |
| A→P | `denied` | `reason` — then the agent closes the session |
| P→A | `ping` | `t1` (pilot µs) |
| A→P | `pong` | `t1`, `t2` (agent µs at receipt) |
| P→A | `loss` | `ch`, `frame_id`, `key: bool` — sent once per unrecoverable frame, immediately |
| P→A | `request-keyframe` | `ch` — sent on `hello` and whenever the decoder needs a keyframe |
| P→A | `set-qos` | `profile` |
| A→P | `qos-ack` | `profile`, `qos: {…}` as in `welcome`, `publisher: "requested"\|"unavailable"` |
| A→P | `agent-stats` | 1 Hz; `frames_in, frames_sent, frames_dropped_backlog, frames_skipped_stale, keyframes_requested, chunks_sent, parity_sent, bytes_sent, rtt_ms, min_rtt_ms, cwnd, delivery_kbps` |
| P→A | `pilot-stats` | 1 Hz; `chunks_rx, chunks_missing, chunks_late, frames_clean, frames_recovered, frames_incomplete, frames_timed_out, frames_timed_out_late, keyframes_lost, keyframes_timed_out, kbps, fps, g2g_ms, decode_q` — the agent pairs `chunks_rx` against what it had sent ≥ RTT earlier to measure true loss |
| P→A | `bye` | — |

Anything unknown is ignored and counted. Commands and sensors do **not** travel
here — they are datagrams (`chunks.md`).

Clock offset: `offset = t2 - (t1 + rtt/2)` using the pilot's local send/receive
times; glass-to-glass = `t_decoded_pilot - (send_ts_agent + offset)` where
`send_ts` comes from the chunk header (low 32 bits of agent µs, wrap-aware).
