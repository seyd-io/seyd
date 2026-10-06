# The interview and the plan template

Ask only what the codebase and the user's message did not already answer.
Group the questions into one or two batches. Every question below says why
it matters and which part of the plan it decides, so you can drop the ones
whose answer you already have and explain the ones the user finds odd.

## A. The video

1. **What produces the video, and in what form does it leave the producer?**
   An IP camera publishing RTSP (vendor and model); a pipeline publishing RTP
   on a UDP port; an encoder inside the user's process handing out access
   units; raw frames with no encoder yet; a ROS 2 image topic (raw: needs an
   encoder).
   *Decides the form factor.* Socket → `seydd`. In-process → an SDK. Raw →
   an encoder must be added first, and the shortest path is usually a
   GStreamer or FFmpeg pipeline that publishes RTP to the daemon.
2. **Codec, resolution and frame rate.** H.264 (profile and level), H.265,
   MJPEG; 720p/1080p; 25/30/60 fps.
   *Decides the codec string* (`avc1.42001f` admits up to 1280x720;
   `avc1.420028` for 1080p; `hev1.…` only decodes where the pilot's machine
   has a hardware HEVC decoder; `mjpeg` costs about ten times the bandwidth
   and is RTSP-only in the daemon).
3. **What can the encoder change while running?** Bitrate cap; GOP length;
   periodic intra refresh; a keyframe on request (an API call, a signal, a
   GStreamer `force-key-unit`); frame-rate cap.
   *Decides how the publisher contract is met.* An encoder that can do none
   of these still works, but Seyd then cannot act on a congested link and a
   joining pilot waits up to `maxGopMs` for a picture; the plan must say so.
4. **Does the camera or encoder already produce a second, lower-resolution
   stream of the same picture?** Most IP cameras do (main and sub stream).
   *Decides simulcast.* Two layers let Seyd adapt by switching streams instead
   of reconfiguring one.
5. **Is there a bitrate the encoder cannot exceed?** A drone's radio, a
   fixed-setting camera.
   *Decides `max_bitrate_kbps`* on the video channel, so the controller never
   asks for more than the encoder can produce.
6. **B-frames, lookahead, frame threading: can they be switched off?** Each
   holds a frame back before Seyd sees it.
   *A contract requirement.* If the encoder cannot, note the added latency.

## B. The robot program and platform

7. **Operating system, architecture and board.** Linux x86-64, ARM64 (Jetson,
   Raspberry Pi 5, an industrial PC), macOS for development.
   *Decides the build* (every Seyd crate is pure Rust; cross-builds are plain).
8. **Language of the software that will hold the Seyd agent**, if an SDK is
   needed: Python, C, C++, Go, Rust, ROS 2 (C++ or Python node).
   *Decides the SDK.* C++ and Go go through `seyd.h`; ROS 2 nodes wrap the C
   ABI or Python SDK themselves (no ROS 2 package is built yet).
9. **Can the robot run a daemon under systemd with a config file in
   `/etc/seyd/` and state in `/var/lib/seyd/`?** Or must everything be one
   process, or a container?
   *Decides whether `seydd` is acceptable* and how the credential file is kept.
10. **How is the robot provisioned?** Image, script, by hand.
    *Decides how the enrolment token is delivered* (environment variable in
    a provisioning script, or an interactive `seydd enrol`).

## C. Sensors (robot → pilot)

11. **What telemetry should the operator see, at what rate, and which process
    has it?** Battery, pose, speed, state machine, alarms. Small and frequent
    is the sensor channel's job; bulk data is not.
    *Decides the sensor channels* (name, codec `json` or `octet-stream`,
    source: a UDP datagram per message to the daemon, or `push_message`).
12. **Is there an existing message shape?** A JSON schema, protobuf, a ROS
    message.
    *Keep it.* The pilot receives the bytes; JSON is parsed for you,
    anything else arrives raw.

## D. Commands (pilot → robot) and safety

13. **What does the operator control, and what does a command look like?**
    Velocities (preferred: a lost stop costs one expiry window), setpoints,
    discrete actions. One channel per control scheme (`drive`, `ptz`,
    `flight`, `arm`).
    *Decides the command channels* and the pilot's controls.
14. **What must happen when the driver vanishes?** Stop, park, hover, land,
    return home, hold the last command for N ms then zero.
    *Decides the session-end handler and the command hold.* Seyd fires
    `session ended` at once when the pilot closes its page and within about
    ten seconds (QUIC idle timeout) when it simply disappears; anything
    faster than that is the robot's own hold on the command stream (zero
    the actuators when the pilot's repeats stop).
15. **How old may a command be before it is ignored?** A command that sat in
    a queue is not the operator's current intent.
    *Decides the stale-command rule.* Act on the newest datagram and drop
    the ones behind it; the demo additionally carries a `ts` of the pilot's
    clock, which only works with NTP-synced clocks.
16. **How many pilots at once, and should observers exist?** One driver, up to
    `max_sessions - 1` observers who see everything and send nothing.
    *Decides `max_sessions`.*
17. **Is there an existing control path** (UDP, ROS topic, MQTT, serial)?
    *Reuse it.* The daemon writes each command as one UDP datagram with the
    raw payload; a bridge of a few lines converts to anything else.

## E. The pilot

18. **Who are the pilots?** The user's own staff; the user's customers inside
    the user's product; anyone (a public demo).
    *Decides access:* console users with roles and grants; the user's backend
    with an API key minting session tokens; a public grant.
19. **Which browser and device?** Chrome or Edge on desktop or Android work.
    Safari does not implement WebTransport, so no browser on iPhone or iPad
    can run the pilot.
    *A hard constraint to state in the plan.*
20. **Is there an existing operator web app, and which framework?** React,
    Vue, Svelte, plain HTML, none.
    *Decides `<seyd-video>` in that page versus `SeydSession` under the user's
    own UI.* The elements are framework-free custom elements; a page with its
    own render loop or several pictures uses `SeydSession` directly.
21. **How does the user's backend authenticate its users today?** OIDC,
    sessions, API tokens.
    *Decides the token flow:* the user's backend holds a Seyd API key and
    calls `POST /api/v1/session-tokens` with `for: <their user id>` so Seyd
    stores no human identity; or pilots sign in to the Seyd console.

## F. The network

22. **How does the robot reach the internet?** Home or office router; a
    corporate network; a 4G/5G SIM (carrier-grade NAT, almost always no
    inbound); Starlink (CGNAT on the standard plan; IPv6 available); a static
    public address; a VPN.
    *Decides which candidates will exist* and what must change for a direct
    path (`networking.md`).
23. **Can a UDP port (4433 by default) be forwarded or a firewall pinhole
    opened to the robot?** Who controls the router.
    *Decides the `portmap` or manual-forward candidate.*
24. **Is IPv6 available to the robot?** Often the way out of CGNAT.
25. **Is a metered relay through the Seyd cloud acceptable as the last
    resort when no direct path exists?** It adds latency and is priced
    separately; it is never the first choice and is always shown.
    *Decides `relay = true/false`* on the robot and `relay` on the pilot.
26. **Where will pilots be?** Same LAN as the robot, same site, anywhere.
    Same-LAN pilots need Chrome's local-network-access permission when the
    page is served from a public origin.

## G. The cloud

27. **Hosted Seyd cloud or self-hosted?** The hosted cloud is a plain
    container in the EU; the same container runs under `docker compose`
    with Postgres and Logto.
    *Decides `signal_url`* and who creates the organisation and tokens.
28. **Does the user have an organisation and can they mint enrolment
    tokens?** From the console's Fleet page, or `node dist/bootstrap.js`
    with database access on a self-hosted cloud.
    *Decides the first step of enrolment.*
29. **Data residency or audit requirements?** Every access change and every
    session token is in the organisation's audit log.

## H. The bar

30. **Latency target and uplink bandwidth.** The profile is a ceiling:
    `latency` (1.5 Mbps, 100 ms budget), `balanced` (3 Mbps, 100 ms),
    `quality` (6 Mbps, 200 ms). Numbers in `qos-profiles.md`.
    *Decides `qos_profile`*, the profile the robot starts in. A driver may
    switch the live session to any of the three (`setQos`, the `qos`
    attribute), including a higher one, so the robot's setting is a
    default, not a cap; cap the encoder with `max_bitrate_kbps` where the
    uplink or the encoder cannot follow. Pick the resolution per profile
    yourself (resolution degrades first): 720p is the natural partner of
    `latency`, 1080p of `quality`.
31. **What counts as done?** A LAN session; a session from another network;
    a session through loss; a week unattended.
    *Decides which rungs of `verification.md` the plan commits to.*

## The plan template

Fill every section. Write "none" or "not needed" where that is the answer.

```markdown
# Seyd integration plan: <robot>

## 1. Summary
<the robot, the operator, what the session carries, in three sentences>

## 2. Form factor
<seydd | Python SDK | C ABI | Rust crate>, because <where the frames are>.
Known gaps of this form factor that affect us: <from form-factor.md>.

## 3. Channels
| # | Kind | Name | Codec | Source / sink | Rate | Notes |
|---|---|---|---|---|---|---|
| 1 | video | main | avc1.42001f | rtsp://… (layers: low, high) | 25 fps | max_bitrate_kbps … |
| 2 | sensor | telemetry | json | udp://127.0.0.1:5002 | 10 Hz | |
| 3 | command | drive | json | udp://127.0.0.1:5004 | on input | velocities, zero on release |

## 4. Video publisher
Encoder: <what>. Settings against the contract:
- B-frames / lookahead / threading: <off | cannot: adds ~N ms>
- Parameter sets inline on every keyframe: <how>
- Bitrate cap follows maxBitrateKbps: <how, coalesced how often>
- VBV about 100 ms: <setting>
- Recovery points: <intra refresh | long GOP + IDR on request>; maxGopMs honoured by <setting>
- recovery-request answered by: <API call / signal / force-key-unit>; expected reaction time <N frames>
- layer answered by: <force IDR on named stream | ignored>
- suggestedFps: <applied how | ignored because …>
Simulcast: <layers and activate_above_kbps | none because …>

## 5. Robot-side program
<what is written, in which language, in which process; the UDP ports if the
daemon; the handlers if an SDK>
Session end: <stop | park | hover | land …> within <N ms>.
Stale commands: dropped after <N ms>.
Observers: <max_sessions = N>.

## 6. Enrolment and access
Cloud: <hosted | self-hosted at …>. Org: <name>.
Enrolment: <SEYD_ENROLMENT_TOKEN in the provisioning env | seydd enrol by hand>.
Credential: <path>, backed up by <how>.
Pilot access: <own backend + API key → session tokens (scope drive/observe) |
console users with grants | public grant (demo only)>.

## 7. Pilot page
<`<seyd-video>` in <existing app/framework> | SeydSession under own UI>.
Controls: <scheme per command channel>. Token fetch: <from where>.
HUD and `<seyd-connect-error>`: <included>. Browser: Chrome/Edge; no iOS.

## 8. Network
Robot network: <type>. Expected candidates: <host, portmap, srflx, host6>.
Change to make: <forward UDP 4433 | enable PCP/UPnP | IPv6 | none>.
Relay: <allowed as last resort | refused>.

## 9. Verification
<the rungs from verification.md, each with what "good" is>

## 10. Open questions
## 11. Out of scope
```
