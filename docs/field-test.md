# Field test: the P2P path off the LAN

Everything measured so far ran with the pilot on the robot's LAN or
hairpinning through the same router. This is the procedure for the two runs
that test what Seyd is actually for, and what to capture from each.

## What we are testing

| run | pilot | robot | expected path | what it proves |
|---|---|---|---|---|
| A | laptop on an iPhone hotspot (or any other network) | Mac on the office LAN, as today | `srflx` after the NAT probe (cone NAT here) — or `portmap` if the router exposes a public address | the hole punch and the real internet path: RTT, loss, jitter |
| B | laptop on office broadband | Mac tethered to the iPhone hotspot | `host6` if the carrier gives the Mac a global IPv6 **and** the pilot network has IPv6; otherwise **no direct path** (CGNAT) and the failure card | the IPv6 story, and the honesty of the P2P-only failure UX |

Robot IPv6 support exists (`[::]:4433` listener, v6 SAN in the cert, `host6`
candidate) but has never run — this LAN has none.

## Before you start

* `./demo-seyd.sh` on the robot Mac (camera on the LAN). Watch its first
  lines: the `candidate` and `NAT type` lines and the `nat_report` are the
  robot's own reading of its reachability.
* Landing: `https://seyd-signal-flj7s44j4a-ew.a.run.app/` — the list shows
  `p2p likely | lan-only | none` per robot from the same report.
* On the pilot laptop, `tools/.venv` (see `tools/setup-machine.sh`) if you
  want the recorder; the browser alone is enough for a manual run.

## Run A — pilot off-LAN

1. Put the pilot laptop on the hotspot (turn Wi-Fi off, USB/hotspot on).
2. Open `https://seyd-signal-flj7s44j4a-ew.a.run.app/pilot/?robot=seyd-demo`.
   Note the time to first frame and the path in the HUD (`S`). Expected
   `srflx`; `host` cannot work from outside.
3. Drive the camera for 2–3 minutes, including long pans (motion = bigger
   frames = the interesting case). Note any smear, freeze or red border, and
   how long a keyframe takes to arrive after one.
4. Record the numbers (10 min is ideal):
   ```bash
   tools/.venv/bin/python3 tools/seyd-smoke.py --robot seyd-demo \
       --page https://seyd-signal-flj7s44j4a-ew.a.run.app/pilot/ \
       --signal wss://seyd-signal-flj7s44j4a-ew.a.run.app/ws \
       --no-sensor --record 600 --record-file run-a.jsonl
   ```
   (this drives its own headless Chrome; it prints a line every 10 s and
   appends `lastStats` once per second — path, fps, kbps, true loss, g2g
   p50/p95, rtt, incomplete/recovered frames, keyframe requests).
5. Keep the robot's `demo-seyd.sh` output: `frames_dropped_backlog`,
   `keyframes_requested`, session start/end reasons.

## Run B — robot on the hotspot

1. On the robot Mac: Wi-Fi off, connect to the iPhone hotspot (USB is more
   stable than Wi-Fi tethering). The camera must stay reachable: keep the
   Ethernet to the office LAN up (the Mac then has two interfaces — that is
   fine; `seydd` gathers candidates on all of them and the camera is on the
   wired one).
2. `./demo-seyd.sh`. Read the startup lines:
   * `candidate [host6 …]` present → the carrier gave a global IPv6.
   * `nat_report … cgnat: true` and no `host6` → expect **no direct path**.
   * `NAT type: Symmetric` → same.
3. From the office laptop open the pilot page. Either it connects over `host6`
   (check the HUD; g2g will be tens of ms — carrier RTT), or you get the
   `<seyd-connect-error>` card. Check that the card's diagnosis matches what
   the robot printed (that is the point of the card) and that the landing page
   showed `p2p none/lan-only` *before* you clicked.
4. If it connects, repeat the drive + `--record` capture as `run-b.jsonl`.

## What to bring back

* `run-a.jsonl`, `run-b.jsonl`, the two `demo-seyd.sh` logs, and the robot's
  candidate/NAT lines for each run.
* Subjective notes: first-frame delay, smear after motion, anything that felt
  like a replay/jump.
* For run B, whether the phone's plan has IPv6 (Settings → Cellular usually
  shows an IPv6 address under the APN when it does).

## What we will do with it

* True loss and its burstiness on a real cellular path decide the FEC
  defaults per profile and the first ABR trace fixtures (PLAN.md §1.3, §1.7).
* RTT/g2g on `srflx` vs `host` sets the numbers on the landing page.
* Run B decides how loudly the product must say "you need IPv6 or a port
  mapping" — and whether the native pilot agent with direction-agnostic QUIC
  (PLAN.md §2.2) moves up the list.
