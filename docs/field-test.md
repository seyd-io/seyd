# Field test: the P2P path off the LAN

Everything measured so far ran with the pilot on the robot's LAN or
hairpinning through the same router. This is the procedure for the two runs
that test what Seyd is actually for, and what to capture from each.

## What we are testing

| run | pilot | robot | expected path | what it proves |
|---|---|---|---|---|
| A | laptop on an iPhone hotspot | Mac on the office LAN, real camera | `srflx` after the NAT probe (cone NAT) | the hole punch and a real internet path — **done twice, see PLAN.md** |
| B | laptop on an iPhone hotspot | Mac **and the camera behind a 4G router** | `srflx` if the 4G router gets a public IPv4 or exposes IPv6; `portmap` if it speaks PCP/UPnP with a public address; otherwise **no direct path** (CGNAT) | what a robot on a cellular router looks like from the internet — the product's typical deployment |
| C | laptop on office broadband (or a second hotspot) | this Mac **tethered to the iPhone hotspot**, webcam through FFmpeg (`./sim-robot.sh`) | `host6` if the phone's plan gives a global IPv6 **and** the pilot side has IPv6; otherwise no direct path | the IPv6 story on a phone plan, and the honesty of the P2P-only failure UX |

Robot IPv6 support exists (`[::]:4433` listener, v6 SAN in the cert, `host6`
candidate) but has never run — the office LAN has none. Both B and C are
mostly about what the robot's startup lines say (`candidate …`, `NAT type`,
`nat_report`) versus what the pilot then experiences.

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

## Run B — robot behind the 4G router

1. Put the robot Mac and the camera on the 4G router's LAN (the camera must
   be reachable from the Mac: `CAMERA_IP=… ./demo-seyd.sh` does the ISAPI
   preflight and will say if it is not).
2. Read `demo-seyd.sh`'s startup lines:
   * `candidate [portmap …]` → the router accepted a PCP/NAT-PMP/UPnP mapping
     *with a public address* — best case.
   * `candidate [srflx …]` and `NAT type: Cone` → hole punch should work.
   * `candidate [host6 …]` → the router hands out global IPv6.
   * `not globally reachable — double NAT or CGNAT` and no `srflx`/`host6`,
     or `NAT type: Symmetric` → expect **no direct path**.
3. From the hotspot laptop open the pilot page. Note path, first-frame time,
   and whether the landing page's `p2p …` hint matched the outcome. If it
   fails, check that the `<seyd-connect-error>` diagnosis matches the robot's
   own lines.
4. If it connects, drive for a few minutes and record (`--record 600`, as in
   run A) — this is the run whose loss/RTT traces set the cellular defaults.

## Run C — this Mac on the hotspot, webcam robot

1. Wi-Fi off, connect the Mac to the iPhone hotspot (USB is more stable).
2. `./sim-robot.sh` — webcam through FFmpeg → RTP → `seydd`, robot id
   `seyd-sim`, against the deployed cloud (`VIDEO_DEVICE=lavfi` for the
   synthetic source). Read the startup lines as in run B; the interesting
   one is whether `host6` appears (phone plan IPv6).
3. From a pilot elsewhere (office broadband, or a second phone), open
   `https://seyd-signal-flj7s44j4a-ew.a.run.app/pilot/?robot=seyd-sim`.
   Either `host6` connects (g2g will be tens of ms), or the failure card —
   check its diagnosis against the robot's lines.
4. If it connects, record as `run-c.jsonl`.

## Findings so far

* **Run B, first attempt (2026-08-31, pilot on office broadband by mistake):**
  the Tele2 4G router network gives the robot **global IPv6** (three `host6`
  candidates) and a public, non-CGNAT IPv4 (`37.2.207.116`, STUN says cone) —
  but no inbound IPv4 ever arrived, so the carrier NAT is in practice
  **port-restricted** (unreachable by hole punch from a browser, exactly as the
  NAT table predicts; STUN cannot distinguish this from address-restricted, so
  the hint stays "likely"). The office pilot has no IPv6, so `host6` could not
  be tried. Conclusion: on this carrier, **the pilot side must have IPv6** for
  a direct path. Retry with the pilot on the iPhone hotspot.
* Bug found and fixed: a `report outcome=failed` did not release the driver
  slot, so the same pilot's retries were offered `observer`.
* It also moves the cloud prober up the list: it would have measured "IPv4
  inbound: blocked, IPv6: works" at announce time instead of at the demo.

## What to bring back

* `run-a.jsonl`, `run-b.jsonl`, the two `demo-seyd.sh` logs, and the robot's
  candidate/NAT lines for each run.
* Subjective notes: first-frame delay, smear after motion, anything that felt
  like a replay/jump.
* For B: the 4G router model and whether it offers a public IPv4 / UPnP / IPv6.
* For C, whether the phone's plan has IPv6 (Settings → Cellular usually
  shows an IPv6 address under the APN when it does).

## What we will do with it

* True loss and its burstiness on a real cellular path decide the FEC
  defaults per profile and the first ABR trace fixtures (PLAN.md §1.3, §1.7).
* RTT/g2g on `srflx` vs `host` sets the numbers on the landing page.
* Runs B and C decide how loudly the product must say "you need IPv6 or a port
  mapping" — and whether the native pilot agent with direction-agnostic QUIC
  (PLAN.md §2.2) moves up the list.
