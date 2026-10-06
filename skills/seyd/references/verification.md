# Verification: the ladder

Each rung is checkable on its own and the later ones depend on the earlier
ones. Walk them in order; report what was measured, with its setup. "It
connected once" is rung 3 of 10.

| # | Rung | How | Good looks like |
|---|---|---|---|
| 1 | **The video input alone** | Daemon: `seydd --config … --probe-input 20`. SDK: count access units and keyframes reaching `push_frame` for 20 s. `ffprobe -show_streams` on the source. | Steady frame rate at the nominal fps; `has_b_frames=0`; keyframes at the GOP you configured, with SPS/PPS inline; the first access unit after start is a keyframe within a GOP |
| 2 | **The agent is announced** | Start the robot; read the log (`RUST_LOG=info`): `candidate …` per address, `certificate`, `robot identity`, inputs `playing`/`listening`, no `signal denied`. Check presence: `GET /api/v1/robots` (public robots) or the console's Fleet page. | Online, with the candidates you expect for this network (`networking.md`) and the prober marking at least one reachable when the robot is meant to be reachable from outside |
| 3 | **A pilot connects on the LAN** | Open `/pilot/?robot=<id>` on the signal server (with `?token=` or a public grant), or your own page, from a machine on the robot's LAN. Press `S`. | `session started` in the robot log with role `driver` and a path; HUD `path` shows `p2p (host)` or similar; video within a GOP; first picture time noted |
| 4 | **The publisher contract is answered** | Daemon: watch the publisher-control port (`nc -ul 5003` or your bridge's log) for `video-config` at start; join a second pilot and expect `recovery-request`; measure the time to the next keyframe. SDK: log the handlers. `tools/keyframe-probe.py` against the source with `--request-cmd` for a camera. | `video-config` applied (bitrate cap and GOP changed on the encoder); a `recovery-request` answered within a few frames (demo camera: 96 to 155 ms); a joining pilot's first picture in well under a second, not up to `maxGopMs` |
| 5 | **Commands and sensors** | Send a command from the page; see it on the command output (daemon: `nc -ul 5004`) or in `on_command`; send a sensor datagram and read it in the page's `sensor` event or the demo footer. From a second (observer) pilot, send a command. | Driver's command arrives with the raw payload; observer's command never arrives; sensor messages at the expected rate |
| 6 | **Session-end safety** | Close the pilot tab; kill the pilot's network (airplane mode) with a command held. | The robot parks, stops, hovers or lands as planned, on `session ended` (daemon `session` with `sessions: 0`, SDK `on_session_ended`), within the planned time even when the pilot vanished without goodbye |
| 7 | **Access is tight** | Remove any public grant. Open the page without a token; with an `observe` token try to drive; with a token for another robot. Start a second robot with the same key file. | Rejected: `token-rejected` / `denied`; observe cannot command; wrong `aud` refused; `key-mismatch` on the duplicate; the audit log shows each token minted |
| 8 | **Loss** | `?loss=0.05` (and `&burst=3`) on the page; watch HUD `loss`, `frames`, `key`. | True loss near 5 %; `framesRecovered` rising, `framesIncomplete` low, `keyframesLost` 0 or recovered by a request within a round trip; the picture clears after each red border; ABR raises FEC and lowers the bitrate request |
| 9 | **Off the LAN** | A pilot on another network (a phone hotspot is the standard test). `?relay=0` first, to see the direct path on its own. | HUD `path` shows `srflx`, `portmap` or `host6`, not RELAY; if the race fails the guidance box names the class and `networking.md` has the change; only then allow the relay and confirm it is shown in amber |
| 10 | **An assertion and a record** | `tools/.venv/bin/python3 tools/seyd-smoke.py --robot <id> --page <pilot url> --signal <ws url> [--no-sensor] [--command flight] [--query loss=0.05] [--record 60]` from a checkout, with the harness's virtualenv created by `tools/setup-machine.sh` (or `python3 -m venv tools/.venv && tools/.venv/bin/pip install websockets`) and a Chrome on the machine. The harness drives the demo page's control schemes only: `--command ptz` (default) or `flight`; for a robot with another command channel pass `--no-drive` (no command is sent, the rest is asserted) and cover the command path at rung 5. | Passes; the recorded `seyd-record.jsonl` carries path, fps, kbps, true loss, g2g p50/p95, rtt, recovered frames and keyframe requests, which is what a field report quotes |

## Reading the HUD

`path` (the winning candidate, or `RELAY via cloud` in amber with the
direct-path reason), `qos` (the profile in force; `(transport only)` means
the publisher could not take the targets), `video` (kbps including parity,
fps, FEC share, g2g p50/p95), `loss` (true loss against the agent's send
count; green below 1 %, amber from 1 %, red from 3 %), `frames` (clean,
recovered, incomplete, timed out), `key` (keyframes clean, lost, requested;
amber at one lost), `agent` (frames sent, dropped for backlog, skipped as
stale), `link` (the robot's QUIC view: cwnd, rtt, delivered rate), `abr`
(the controller's request and ceiling, FEC rates, last reason).

**"g2g" is not glass to glass.** It spans first chunk handed to QUIC on the
robot to last chunk reassembled in the pilot's worker; it excludes capture,
encoding, delivery to the agent, the decoder, the presentation delay (50,
100 or 150 ms by profile, deliberately) and the display. Quote it with its
span. A camera-and-stopwatch glass-to-glass number is the only honest
end-to-end figure, and the repository's estimate for it is roughly 200 to
250 ms median on `balanced` and about 150 ms on `latency` on a clean link.

## Measured reference points (same span: HUD g2g)

- Office LAN robot, pilot on a phone hotspot, `srflx`: RTT 22 ms, 25 fps at
  about 1.7 Mbps, g2g p50 17 ms / p95 101 ms (the p95 was 30 to 60 KB IDRs
  over a 2 Mbps uplink; intra refresh removes it), 0 % loss.
- Robot behind a 4G router with UPnP, pilot on a hotspot, `portmap`: RTT
  51 ms, 24 fps, g2g p50 31 / p95 59 ms, 0 % true loss.
- Through a 4.5 Mbps shaped link at 100 ms presentation delay: IDR GOPs
  p50/p95 45/148 ms with 41 arrival gaps in 40 s; intra refresh 44/48 ms
  with 8 gaps. This is why the contract asks for intra refresh.
- Relay latency off loopback: not measured; do not quote one.

## When something is wrong

- No candidates or all dark: `networking.md`.
- `signal denied`: `access.md` (enrol, or fix the key).
- Picture arrives only after many seconds: rung 4; nobody answers
  `recovery-request`, or the GOP is long and the request path is broken.
- Picture smears or freezes under loss: rung 4 and 8; the publisher ignores
  `kind` or produces no recovery point; try `recovery_ladder = false`.
- Hitch once a second and a fat p95: a periodic IDR cadence; move to intra
  refresh or a long GOP with requests.
- Latency grows over minutes: B-frames, lookahead or frame threading in the
  encoder, or a VBV far above 100 ms, or the publisher's bitrate ignoring
  `maxBitrateKbps` so the uplink queues.
- Frames dropped for backlog on the robot (`frames_dropped_backlog`): the
  uplink cannot carry the configured bitrate; lower the profile or set
  `max_bitrate_kbps`.
- Observer sees video but controls do nothing: correct; only the driver
  commands. Two sessions from one page make the second an observer; use
  `allowMultiple` knowingly or close the first.
- H.265 decodes on one machine and not another: hardware decoder
  availability; the pilot says so on screen. Use H.264 for the general case.
