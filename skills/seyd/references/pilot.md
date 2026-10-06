# The pilot page

The operator opens a web page in Chrome or Edge (desktop or Android; Safari
has no WebTransport, so nothing on iPhone or iPad). `@seyd/web` is three
framework-free custom elements over `@seyd/core`'s `SeydSession`; use the
elements inside any page or framework, or the session directly under a UI of
your own. There is no React package; the elements are the integration.

## Getting the packages

Not on npm yet. From a checkout: `pnpm install && pnpm -r build`, then
depend on the built package: `"@seyd/web": "file:../seyd/sdks/js/web"`
(outside the workspace) or `"workspace:*"` (inside). `@seyd/web` depends on
`@seyd/core` from the same checkout; build both. Serve the page from a
bundler that resolves the import (Vite, esbuild, webpack). The engine's Web
Worker is created with `new Worker(new URL('./worker.js', import.meta.url),
{ type: 'module' })`, which Vite and webpack bundle on their own; a bundler
that cannot builds the `@seyd/core/worker` entry and passes it as the
`worker` option.

## `<seyd-video>`

The smallest page is `sdks/js/web/examples/minimal.html`:

```html
<seyd-video id="video" robot-id="my-robot" signal-url="wss://seyd-signal-flj7s44j4a-ew.a.run.app/ws" token="…"></seyd-video>
<seyd-hud id="hud"></seyd-hud>
<seyd-connect-error id="err"></seyd-connect-error>
<script type="module">
  import '@seyd/web';
  const video = document.getElementById('video');
  video.addEventListener('seyd-session', (e) => {        // fired once per session; re-fired on restart
    const session = e.detail;
    document.getElementById('hud').session = session;
    document.getElementById('err').session = session;
    session.on('sensor', ({ channel, data }) => console.log(channel.name, data));
    session.on('welcome', ({ role }) => { /* driver: enable controls; observer: disable */ });
    session.on('state', ({ state }) => { /* disable controls on anything but 'connected' */ });
    window.addEventListener('keydown', (k) => { if (k.key === 'ArrowUp') session.send('drive', { throttle: 1 }); });
  });
</script>
```

Attributes: `robot-id`, `signal-url` (both required; nothing starts until
both are set), `qos` (`latency` | `balanced` | `quality`; the only attribute
that applies live), `token` (a session token; omit only for a public-grant
robot), `loss` and `burst` (inject chunk loss, tests), `host` (`auto` |
`worker` | `inline`), `trace`, `paths` (race only these candidate labels;
`none` forces the race to fail), `presentation-delay` (ms behind the capture
timeline; `0` = decode on arrival), `relay` (`0`/`false`/`off` refuses the
relay). Changing any attribute other than `qos` restarts the session;
changes in the same task coalesce. Set `robot-id` last, after the token is
fetched. Properties: `.session`, `.canvas`. Children are positioned
absolutely over the picture (your controls go there).

The element draws a status line, a red border while the picture is degraded
by loss, and an amber RELAY badge for the life of a relayed session. The
badge cannot be switched off.

**Inside React, Vue or Svelte**: import `@seyd/web` once for its side effect
(it defines the elements), render `<seyd-video>` as a plain tag, and set the
attributes with `setAttribute` or the framework's attribute binding (not
properties: the element observes attributes). Set `token` and `signal-url`
before `robot-id`, in the same tick, so one session starts; in React that is
one effect after the token fetch resolves. Listen for `seyd-session` with
`addEventListener` on a ref, not an `onSeydSession` prop, and `close()` the
session in the cleanup. `<seyd-hud>` and `<seyd-connect-error>` take
`.session` as a property on their refs.

`<seyd-hud>`: set `.session`; hidden until `S` or `.toggle()` (a button for
touch); `.visible`. `<seyd-connect-error>`: set `.session`; red after
`p2p-failed` (no session; full guidance and a "How to fix this" link to the
networking page for that failure class), amber after `relay` (one line,
expandable). Both can be dismissed; a pointer event on the box never reaches
the video.

Theming: the overlays read `--seyd-scrim`, `--seyd-on-scrim`,
`--seyd-accent`, `--seyd-amber`, `--seyd-danger`, `--seyd-font-*`,
`--seyd-radius*` from the host page with dark-theme fallbacks. Keep the
meanings: green is direct, amber is relayed, red is stop.

## `SeydSession` (your own UI)

`new SeydSession(options)`, then `await session.connect(robotId)`. Options:
`signalUrl` (required), `token`, `canvas` (control transferred to the worker;
nothing else may draw on it), `emitFrames` (also emit `frame` events to
render yourself; you must `close()` each `VideoFrame`), `host`, `worker`,
`qos`, `loss`, `trace`, `paths`, `clientName`, `presentationDelayMs`,
`retryMs` (default 15000), `relay` (default `true`), `allowMultiple`
(default: a second `connect()` to the same robot from one document closes
the first; set it for multi-view, each session its own worker, decoder and
canvas).

States, in order on success: `idle` → `signaling` → `waiting-robot` →
`connecting` → `connected`; otherwise `p2p-failed` (retry after `retryMs`)
or `closed` (final). A signalling reconnect while connected does not disturb
the QUIC session.

Events (`session.on(type, handler)` returns an unsubscribe):

| Event | Payload | Use |
|---|---|---|
| `state` | `{ state, detail? }` | Status line; disable controls on anything but `connected` |
| `welcome` | `{ sessionId, role, channels, qos, pathLabel, transport }` | The moment you know the role (`driver` / `observer`), the channels and the profile |
| `relay` | `{ failure }` | The race failed and the session is relayed; show the diagnosis, it is a symptom |
| `p2p-failed` | `{ reason, natReport, candidates, detail? }` | No session; `reason` is one of `no-candidates`, `all-candidates-timeout`, `cert-mismatch`, `token-rejected`, `pilot-udp-blocked`, `robot-offline`, `handshake-timeout`, `relay-unavailable` |
| `frame` | `{ channel, frame }` | Only with `emitFrames`; already paced; close the frame |
| `sensor` | `{ channel, seq, data, raw, sendTs }` | `data` is parsed JSON for codec `json` (`null` if unparsable); `raw` always |
| `link` | `{ state, rttMs, offsetUs, lossPct, degradedPicture }` | `good` / `degraded` (loss ≥ 1 %) / `poor` (≥ 5 %, or a lost keyframe) / `lost` |
| `stats` | `PilotStats` | Every 500 ms; `transport` is `'p2p'` or `'relay'` |
| `error` | `{ message, fatal }` | Decoder errors are non-fatal; control stream, denial, token refusal are fatal |
| `video-size` | `{ width, height }` | Canvas resized to a new picture size |

Methods: `send(channel, payload)` (object → JSON, string → UTF-8,
`Uint8Array` as is; returns `false` and sends nothing unless `connected` and
the robot declared that command channel; observers' commands are dropped by
the robot), `hasCommandChannel(name)` (hide controls that would do nothing),
`setQos(profile)`, `requestKeyframe()` (rarely needed; the engine does it on
unrepairable loss), `close()` (releases the driver slot; call it on
`pagehide`). Properties: `state`, `role`, `channels`, `qos`, `sessionId`,
`robotId`, `pathLabel`, `transport`, `candidates`, `lastStats`,
`lastFailure`. The no-UI example is
`sdks/js/core/examples/headless-session.ts`.

## Controls that survive packet loss

Send velocities, not positions, and repeat while held (the demo: every
200 ms for PTZ, 100 ms for flight) with one explicit zero on release; stop
on window blur and tab hide; pair it with a robot-side hold that zeroes when
the repeats stop. One control scheme per command channel the robot declares
(`web/demo/src/ptz.ts`, `flight.ts`, `gamepad.ts` are the demo's). Enable
controls on `welcome` for the driver only, and disable them on any state but
`connected`.

## Session tokens on the page

Every pilot needs a session token unless the robot has a public grant. The
page (or its backend) calls `POST /api/v1/session-tokens` on the signal
server's HTTPS origin with `{ robot_id, scope: 'drive' | 'observe', ttl_sec
≤ 300 }`, gets `{ token, subject, scope, expires_in, issuer }`, and sets
`token` before `robot-id`. The demo page asks for `drive`, then `observe`,
and connects without a token only if both are refused. Who may call it and
the API-key flow are in `access.md`. A token lives at most 300 s and names
one robot; fetch it when the page opens, not at build time.

## Same-LAN pilots

A page served from a public HTTPS origin needs Chrome's local-network-access
permission before it may dial the robot's private address; denied, the pilot
falls through to the slower hairpin candidates. Tell operators to allow it.

## Debug query parameters of the demo page

`/pilot/?robot=<id>` plus `?loss=0.05`, `?paths=none` (force the race to
fail; exercises the relay), `?relay=0`, `?pd=<ms>`, `?trace=1`; `S` toggles
the HUD. Useful against any robot on the hosted cloud while your own page is
not ready.
