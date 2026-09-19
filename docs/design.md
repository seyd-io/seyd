# Seyd design system

One visual language for everything a person sees from Seyd: the public demo
(landing and pilot pages), the console, the SDK's overlays inside a customer's
page, and the presentations and walkthrough documents we publish. The system
grew out of the presentation artifacts (the pitch deck, *Who Buys Seyd*, the
*Inside the Seyd Cloud / Engine* walkthroughs); this document is the rulebook
distilled from them, and `web/theme` (`@seyd/theme`) is the same rulebook as
code. If a surface disagrees with this page, the surface is wrong.

## 1. Principles

- **Quiet chrome, loud picture.** Seyd's product is a live video feed and the
  numbers around it. Everything we draw exists to frame that; nothing competes
  with it. Flat surfaces, one-pixel rules, no shadows, no gradients.
- **Colour means something.** Four hues, each with one meaning (section 3).
  A green dot always means the same thing in the console, on the landing page
  and in the HUD. Decorative colour is not used.
- **Honest by design.** The system exists to make state legible: online or
  offline, direct or relayed, driver or observer. A relayed session is amber
  everywhere it appears (ADR 0010); a degraded picture is red-bordered. We
  never dress a worse state up as a better one.
- **Mono for machines, grotesk for people.** Identifiers, measurements,
  labels and eyebrows are monospaced; headings are a compact grotesk; running
  text is a humanist sans. The reader always knows which kind of thing they
  are reading.
- **Both modes, always.** Every surface renders correctly in light and dark,
  follows the system preference by default, and offers an explicit override.
  No colour is ever defined only for one mode.

## 2. Tokens

Tokens are CSS custom properties prefixed `--seyd-` and defined once in
`web/theme/src/tokens.css`. Product surfaces import the package; presentations
copy the two palettes below. The prefix matters: SDK overlays live in a
customer's page and read these names from the host document, so they must not
collide with the customer's own variables.

### Colour

| Token | Light | Dark | Use |
|---|---|---|---|
| `--seyd-bg` | `#f2f5f4` | `#0e1618` | page ground |
| `--seyd-surface` | `#ffffff` | `#162124` | cards, panels, table rows, nav |
| `--seyd-surface-2` | `#e6ecea` | `#1d2a2e` | sunken: inputs, code, hovered rows |
| `--seyd-ink` | `#10201f` | `#e7eeec` | headings, primary text |
| `--seyd-ink-2` | `#4a5c5a` | `#b4c2bf` | body text |
| `--seyd-muted` | `#7f9290` | `#7f9290` | captions, labels, placeholders |
| `--seyd-line` | `#cfd9d6` | `#2a3a3e` | 1px rules and borders |
| `--seyd-accent` | `#0f8a68` | `#12a37a` | Seyd green: direct path, links, primary action, online |
| `--seyd-accent-ink` | `#ffffff` | `#06130f` | text on an accent fill |
| `--seyd-accent-bg` | `#dcefe7` | `#143229` | accent tint: active nav, selected row, focus ring |
| `--seyd-amber` | `#b8801c` | `#d9a441` | attention: relayed, in use, degraded, LAN-only |
| `--seyd-amber-bg` | `#fbf1dc` | `#2a2413` | amber tint for notes and one-time secrets |
| `--seyd-danger` | `#b3382f` | `#e0776c` | stop: errors, unreachable, destructive actions |
| `--seyd-danger-bg` | `#f9e4e1` | `#3a1f1c` | danger tint |
| `--seyd-scrim` | `rgba(8,14,15,.72)` | same | backing for anything drawn over video |
| `--seyd-on-scrim` | `#e7eeec` | same | text on the scrim |
| `--seyd-on-scrim-2` | `#b4c2bf` | same | secondary text on the scrim |

The neutrals are a cool grey-green, never a pure grey, so the accent sits
inside the palette rather than on top of it. The muted grey is the same in
both modes on purpose: it is the one colour that reads as "de-emphasised"
against both grounds.

### Type

| Token | Stack | Role |
|---|---|---|
| `--seyd-font-display` | Familjen Grotesk 500/600/700, IBM Plex Sans, system-ui | headings, the wordmark, robot names, big numbers |
| `--seyd-font-body` | IBM Plex Sans 400/500/600, system-ui | running text, controls, table cells |
| `--seyd-font-mono` | IBM Plex Mono 400/500, ui-monospace | eyebrows, table headers, tags, identifiers, measurements, the HUD |

Fonts are self-hosted (bundled from `@fontsource/*`, Latin subset only, about
180 kB of woff2 in total). No surface may load a font from a CDN: Seyd runs
self-hosted and offline, and a visitor's address must not leak to a third
party for a typeface.

Sizes for product UI: body 14px / 1.5; small text 12.5px; eyebrow 11px
uppercase with 0.12em tracking; `h1` 24px, `h2` 18px, `h3` 15px, all 600
weight with slight negative tracking. Presentations scale up (slides use
16px body and `clamp()` display sizes) but keep the same faces and ratios.

### Shape and rhythm

- Radius: `--seyd-radius` 6px for everything except tags and pills (999px)
  and inline code (3px).
- Spacing: a 4px scale, `--seyd-space-1` … `--seyd-space-7`
  (4, 8, 12, 16, 24, 32, 48). Page gutters are 32px on desktop, 16–24px on
  narrow screens.
- Measure: `--seyd-measure` 62ch caps any run of prose.
- Rules, not boxes: sections are separated by a 1px `--seyd-line`, and cards
  have a 1px border and no shadow. Depth is expressed by the three greys
  (ground, surface, sunken), never by elevation.

## 3. Meaning of colour

| State | Colour | Where it shows |
|---|---|---|
| Online, available, direct path, connected, primary action, link | accent | dots, tags, the "Connect" button, the video status line, the HUD's `ok` |
| In use by someone else, relayed session, LAN-only reachability, degraded but working, a one-time secret to copy now | amber | dots, tags, the RELAY badge, the relayed connect-error box, the console's secret note, the HUD's `warn` |
| Offline, no session, unreachable, error, destructive action | danger (dots use `--seyd-line` for plain offline) | the degraded-picture border, error text, the connect-error box, "Delete"/"Revoke" |
| Neutral information | ink / muted | everything else |

A driver is shown in the accent (they hold the direct path); an observer in
amber (someone else does). Offline is a grey dot, not a red one: red is for
something wrong, and a robot that is switched off is not wrong.

## 4. Light and dark

The mechanism is the one the presentations use, and it must be reproduced
exactly on every surface:

1. The complete light palette is defined on bare `:root`.
2. Dark is applied under `@media (prefers-color-scheme: dark)` guarded as
   `:root:not([data-theme="light"])`, so a viewer who explicitly chose light
   keeps it.
3. Dark is applied again under `:root[data-theme="dark"]`, so an explicit
   choice of dark wins over a light system preference.
4. `body` is painted with `--seyd-bg` explicitly; a transparent body would
   borrow whatever the host paints.
5. Every colour is defined in all three places or in none of them. Never give
   a colour its only definition inside a dark block.

`@seyd/theme` applies the saved choice (`localStorage` key `seyd.theme`:
`system`, `light` or `dark`) before first paint, and `mountThemeSwitch()`
renders the three-way control every surface carries: the console's nav
footer, the landing page's footer, the pilot page's footer. A `?theme=light`
or `?theme=dark` query parameter pins a mode for one load without touching
the saved choice, which is how screenshots and the smoke tool get a
deterministic mode.

**Over video, nothing flips.** The picture is dark whatever the theme, so
everything drawn on it (the status line, the HUD, the relay badge, the
connect-error box, the pilot page's notices) uses the scrim tokens, which are
identical in both modes. Only the chrome around the picture follows the
theme.

## 5. Components

The shared vocabulary lives in `web/theme/src/ui.css`. Pages add layout; they
do not restyle these.

- **Wordmark** `.wordmark` — a filled accent circle with a hollow centre,
  then "Seyd" in the display face at 700. The hollow is cut with the page
  ground; add `.on-surface` when the wordmark sits on a surface. The same
  mark is the favicon. It is the only logo.
- **Eyebrow** `.eyebrow` — mono, 11px, uppercase, tracked. Labels a section
  or a value; `.accent` colours it green.
- **Dot** `.dot` with `.on` / `.warn` / `.stop` / `.off` — 8px liveness
  indicator, meaning per section 3.
- **Tag** `.tag` with `.on` / `.warn` / `.stop` / `.fill` — mono uppercase
  pill with a 1px border; `.warn` and `.stop` add their tint.
- **Card** `.card` — surface, 1px line, 6px radius; `.sunk` uses the sunken
  grey with no border (the presentations' card).
- **Note** `.note` with `.warn` / `.stop` — a 3px left rule for asides. The
  console's one-time secret is an amber note.
- **Buttons** — default is a surface with a line border; `.primary` (and
  `type="submit"`) is an accent fill; `.link` is text only; `.danger` is a
  danger outline that fills on hover; `.small` for toolbars.
  `aria-pressed="true"` shows the accent tint. `a.button` gives a link the
  same look.
- **Inputs** — surface, line border, accent border plus `--seyd-accent-bg`
  ring on focus.
- **Tables** — mono uppercase headers, 1px row rules, no zebra striping;
  `tr.row-link` rows highlight on hover; `.num` right-aligns tabular figures.
- **Facts** `dl.facts` — a two-column key/value list with eyebrow keys.
- **Theme switch** `.theme-switch` — the segmented system / light / dark
  control from `mountThemeSwitch()`.

## 6. Surfaces

| Surface | Where | Notes |
|---|---|---|
| Demo landing | `web/demo/index.html` | Two columns: the pitch on the ground, the live robot list on a surface. |
| Pilot page | `web/demo/pilot/index.html` | Header and footer follow the theme; `main` is black and everything on it uses the scrim. |
| Console | `web/console` | Surface nav with accent-tinted active item; ground content area; mono section headings. |
| SDK overlays | `sdks/js/web` | `<seyd-video>`, `<seyd-hud>`, `<seyd-connect-error>` read `--seyd-*` from the host page with fallbacks equal to the dark values, so they look the same inside a customer page that never heard of the theme. |
| Presentations | published artifacts | Copy the two palettes verbatim (short names such as `--accent` are fine there) and the three faces from Google Fonts; slides use the sunken card, mono eyebrow, and the accent/amber meanings unchanged. |
| Walkthrough documents | published artifacts | The long-form register: Source Serif 4 body on the same neutrals; accents may shift per document (teal/amber, indigo/oxide) but green and amber keep their product meanings where they appear. |

## 7. Rules of thumb

- Use the tokens; never a hex value in a page stylesheet. The one exception
  is the SDK overlays' fallbacks, which must equal the token values.
- One accent per screen. If two things are green, they are both "good/direct";
  if that is not what you mean, one of them is the wrong colour.
- Prefer a rule to a box, a box to a shadow, and never a shadow.
- Identifiers (`seyd-demo`, session ids, public keys) are always mono.
- Text on the scrim is `--seyd-on-scrim`, not `--seyd-ink`; `--seyd-ink` is
  near-black in light mode and would vanish on the picture.
- Check both modes before calling a surface done: append `?theme=dark` and
  `?theme=light` to the URL.

## 8. Changing the system

A new colour, face or component is a change to this document first and to
`web/theme` second; the surfaces follow. Give the change a meaning (section
3) before giving it a value. If a token's meaning changes, re-check every row
in the surfaces table.
