# Open-sourcing Seyd: the repositories, the license, the notices

Ordered work, decided 2026-10-06. Nothing here has a date; each step is done
when its check passes.

## Decisions

| Decision | Choice | Why |
|---|---|---|
| License for everything public | **Apache-2.0** | Permissive with an explicit patent grant; what customers' legal teams already approve for SDKs they link into products. The cloud stays private, so open-core needs no copyleft. |
| Copyright holder | **Anton Gravestam** (personal, until a company exists) | Reassigning to the company later is one commit over the headers plus a NOTICE line. |
| Repositories | **`github.com/seyd-io/seyd`** (public, primary), **`github.com/seyd-io/seyd-cloud`** (private) and **`github.com/seyd-io/seyd-business`** (private, documents only) | The cloud consumes the open repo as a git submodule, so outside contributors see an ordinary repository and the cloud pins a known commit. `Cargo.toml` already names the public URL. |
| History | **Filtered, not squashed** | `git filter-repo` keeps every commit that touched a public path: the ADRs, the measurements and the reasoning in commit messages are the project's record. The deleted Python prototype stays in history, as PROTOTYPE.md already says. |
| Contributions | **DCO** (`Signed-off-by`), not a CLA | Enough for Apache-2.0 inbound = outbound; no paperwork for a first contributor. |
| Branch | `main` | The local `master` is renamed at push. |

## What goes where

**Public, `seyd-io/seyd`** — everything a robot, a pilot page or a
contributor needs, and nothing that only the hosted cloud needs:

| Path | Note |
|---|---|
| `packages/` except `seyd-prober` | The Rust core; `Cargo.toml` loses the prober member |
| `sdks/` | C, Python, `@seyd/core`, `@seyd/web` |
| `web/theme`, `web/demo`, `web/docs` | The design system, the landing and pilot pages, the developer docs |
| `examples/`, `sim/`, `tools/`, `skills/` | Demo robots, the simulation, the harnesses and generators, the integration skill |
| `docs/` | ADRs, protocol contracts, encoder setup, latency docs, field test, Starlink, this file; `docs/eu-hosting.md` moves private |
| `PLAN.md`, `SPEC.md`, `PROTOTYPE.md`, `DEMO.md`, `DEMO-ROVER.md`, `DEMO-TELLO.md` | The engineering plan and product spec; their business sections move to `seyd-business` (see "Where plans and decisions live") |
| `CLAUDE.md` | The engineering half only (see "Splitting CLAUDE.md") |
| `demo-start.sh`, `demo-seyd.sh`, `demo-tello.sh`, `sim-robot.sh`, `py-robot.sh` | They talk to the deployed cloud by URL, which is public |
| `Cargo.toml`, `Cargo.lock`, `pnpm-workspace.yaml`, `package.json`, `pnpm-lock.yaml`, `.gitignore` | `pnpm-workspace.yaml` loses `cloud/*` |
| New: `LICENSE`, `NOTICE`, `README.md`, `CONTRIBUTING.md`, `SECURITY.md`, `.github/` | Below |

**Private, `seyd-io/seyd-cloud`** — the hosted service and how it is run:

| Path | Note |
|---|---|
| `cloud/` | `api` (signal server, console API, relay), `db`, `logto`, `docker-compose.yml`, `README.md` |
| `web/console` | The fleet console: it encodes the cloud's product surface and depends only on `@seyd/theme`, which it takes from the submodule |
| `packages/seyd-prober` → `prober/` | Its own small Cargo workspace; it has no `seyd-*` dependencies |
| `deploy/` | Cloud Build, Terraform, the Firebase redirect |
| `.gcloudignore`, `.dockerignore`, `.env.local` (never committed) | |
| `docs/eu-hosting.md`, the hosting/deploy/enrolment half of `CLAUDE.md` | |
| `seyd/` | The public repo as a submodule, pinned |

The private workspace: `pnpm-workspace.yaml` with `seyd/sdks/js/*`,
`seyd/web/theme`, `seyd/web/demo`, `seyd/web/docs`, `cloud/*`, `web/console`;
`cloud/api/deploy.sh` copies `../../seyd/web/demo/dist`,
`../../web/console/dist` and `../../seyd/web/docs/dist` into the image. The
docs site is built in the public repo and served by the private one, so a
docs change is a submodule bump.

**Private, `seyd-io/seyd-business`** — documents only, no code: the
business plan, pricing and tiers, the customer pipeline and demo programs,
the competitor landscape, fundraising. Versioned in git because these are
worked on with Claude Code like everything else; separate from `seyd-cloud`
because a cloud engineer or contractor will be given that repo and must not
be given this one.

The signal protocol (`docs/protocol/signal-v2.md`), the cloud's HTTP API as
documented on *Enrolment and access*, and the relay contract are public: they
are what the open SDKs speak. The implementation is not.

## Where plans and decisions live

One question routes a document: *who needs to read it?*

| Question | Repository | Examples |
|---|---|---|
| Does it explain the code or the protocol to someone outside? | `seyd` (public) | `docs/adr/` (all eleven: wire format, quinn, MoQ, the C ABI, pacing, loss measurement, identity planes, simulcast, keyframes, the relay, bitrate ceilings), `PLAN.md` Parts 1–3 as the engineering roadmap, `SPEC.md`'s product spec, `docs/` |
| Does it explain how *our* hosted cloud is built and run? | `seyd-cloud` (private) | `docs/adr/C-0001-…` (own numbering, so the two series never collide): hosting region, scaling, metering mechanics, provider choices; `docs/eu-hosting.md`; the operational `CLAUDE.md` |
| Is it about money, customers or competitors? | `seyd-business` (private) | `PLAN.md` (the business plan), `pricing.md`, `customers/`, `competitors.md`, the demo customer programs |

The test for an ADR is the one `CLAUDE.md` already applies to docs: if the
decision changes what an integrator sees, it is public. A decision made in
`seyd-business` (a pricing model, a tier) is copied into the engineering
plan as a fact once made, never debated there.

What moves out of the three root documents when the split happens:

- `PLAN.md`: "Owner decisions still open" items that are business
  questions (the pricing model, the tiers), and the demo customer program
  notes in §2.7, go to `seyd-business`; the engineering items stay.
- `SPEC.md`: "Customer Archetypes", "Where these archetypes live (target
  verticals)" and the competitor sections ("C2 systems, pub/sub middleware
  and DDS") go to `seyd-business/competitors.md` and `customers/`, with a
  one-line pointer left behind. The vision, problem statement, use cases,
  architecture, security model, latency reality and technology choices stay.
- `DEMO.md`, `DEMO-ROVER.md`, `DEMO-TELLO.md`: the engineering of each demo
  stays public (they are the customer programs' worked examples); who the
  demo is for and what it is meant to win goes to `seyd-business`.

## What stops being true, and the wording that replaces it

- **"Run the cloud yourself" (`web/docs/…/cloud/self-hosting.mdx`)** and
  **"Identity providers"** describe `docker compose up` in `cloud/`. With
  the cloud private that is an offer to customers, not to the public. Reword
  both: the cloud is portable and self-hostable *for customers under a
  commercial license*; the hosted cloud is the default; what a self-hosted
  deployment delegates to an identity provider stays as written. Keep the
  pages, because the portability is a product fact.
- **"The whole cloud locally" in `CLAUDE.md` and `tools/setup-machine.sh`**
  move to the private repo; the public `setup-machine.sh` stops building
  `cloud/api` and stops installing Colima.
- **`<the seyd repository>`** placeholders in the quickstart, `form-factor.md`
  and `SKILL.md` become `https://github.com/seyd-io/seyd`.

## Review before anything is public

Each item is a decision by the owner, recorded here when made:

1. **Secrets scan of the full history**: `gitleaks git .` (install with
   `brew install gitleaks`) on the unfiltered repo and again on each filtered
   one. A first pass by pattern (`seyd_live_`, `seyd_enr_`, database URLs,
   `SEYD_PROBER_TOKEN`, `CAMERA_PASSWORD`, any `.env`/`.key`/`.pem` ever
   added) found nothing but the compose placeholders; the tool is the proof.
2. **`SPEC.md`** and **`PLAN.md`** carry business sections; they move to
   `seyd-business` as listed under "Where plans and decisions live".
3. **`CLAUDE.md`** names the Neon database, the bootstrap commands, the
   enrolled robots and the owner's pending invitation: private half.
4. **`examples/demo-robot/seydd.toml`** carries the demo camera's LAN
   address (`192.168.86.237`). Harmless, but it is rendered on the docs site;
   keep it (it is the real config, which is the point) or move the address
   to `CAMERA_IP` as `demo-seyd.sh` already does for the preflight.
5. **`DEMO.md`, `docs/field-test.md`** mention the office network and the
   camera model; fine to publish, read once for anything personal.
6. **Memory**: Claude's memory directory is outside the repo and keyed to
   `~/code/darc`; keeping that path as the public checkout preserves it.

## Licensing mechanics

- **`LICENSE`**: the Apache-2.0 text, verbatim. **`NOTICE`**:
  ```
  Seyd
  Copyright 2026 Anton Gravestam
  ```
  Third-party notices appended as they are found (below).
- **Headers** on every source file the project wrote (`.rs`, `.ts`, `.mjs`,
  `.py`, `.c`, `.h`, `.sh`, `.astro`, `.css`, `.toml` where comments are
  allowed), two lines in the file's comment syntax:
  ```
  // Copyright 2026 Anton Gravestam
  // SPDX-License-Identifier: Apache-2.0
  ```
  The year is the year of first publication and is not updated annually.
  `tools/add-headers.py` adds them (idempotent, respects a shebang,
  skips generated files, vendored code and `node_modules`/`target`/`dist`);
  `tools/check-headers.py` fails CI on a source file without one.
  `sdks/c/include/seyd.h` is cbindgen output: the header goes in
  `cbindgen.toml` (`header = "…"`) so regeneration keeps it. The generated
  docs references and skill references carry no header (they are build
  products of headed sources).
- **Package metadata**: `license = "Apache-2.0"` in the Cargo workspace
  (`license.workspace = true` already propagates), `"license": "Apache-2.0"`
  in every public `package.json`, `license = "Apache-2.0"` in
  `sdks/python/pyproject.toml`, `repository` fields pointing at the public
  repo. The private repo's packages stay `UNLICENSED` with
  `Copyright 2026 Anton Gravestam. All rights reserved.` headers.
- **Third-party licenses**: `cargo deny check licenses` with a `deny.toml`
  allowing Apache-2.0, MIT, BSD-2/3, ISC, Unicode, Zlib, MPL-2.0 (and
  denying GPL/AGPL/SSPL) for the Rust tree; `pnpm licenses list` for the
  JavaScript tree; `cargo about` to generate the bundled notices for
  `libseyd` and `seydd` binaries (static linking carries the obligations).
  The fonts (`@fontsource/*`) are OFL-1.1, which permits bundling; their
  license files ship with the packages. Document the result in `NOTICE`.
- **Docs prose** is Apache-2.0 like the code, so one license governs the
  repo; CC BY 4.0 for prose is the alternative if a documentation-only
  reuse case appears.
- **DCO**: `CONTRIBUTING.md` explains `git commit -s`; the DCO GitHub app
  enforces it on pull requests.

## The steps, in order

1. **Prepare in the current repository** (still private, one branch
   `open-source-prep`). **Done 2026-10-06**, in five commits on that branch;
   what was staged for the later steps: `CLAUDE-cloud.md` (the private
   repo's `CLAUDE.md`), `business/` (the seed of `seyd-business`),
   `cloud/github-workflows/ci.yml` (the private repo's `.github/workflows/
   ci.yml`), `cloud/setup-cloud.sh` (the container runtime and API build the
   public `tools/setup-machine.sh` no longer does), and `cloud/prober` as its
   own workspace. The header tools take `--root .` so the private repo runs
   them through the submodule. The existing `rust.yml` and `js.yml` were
   extended rather than replaced (a `licenses` job, a macOS leg for the SDKs,
   Rust and Python for the docs build, the `cloud-api` job moved to the
   staged private workflow). The tree had never been through
   `cargo fmt --check`; it is now, in its own commit.
   - Split `CLAUDE.md` into the engineering half (stays) and `CLAUDE-cloud.md`
     (to move). Move the business sections of `PLAN.md`, `SPEC.md` and the
     `DEMO*.md` files into a `business/` directory (the seed of
     `seyd-business`), leaving pointers. Move `docs/eu-hosting.md` under `cloud/docs/`. Move
     `packages/seyd-prober` to `cloud/prober` with its own `Cargo.toml`
     workspace and `Dockerfile` context; update `deploy/cloudbuild-prober.yaml`.
   - Add `LICENSE`, `NOTICE`, `README.md` (what Seyd is, the quickstart in
     ten lines, the form factors, where the docs are, the license),
     `CONTRIBUTING.md` (verification set, DCO, ADR rule, docs rule),
     `SECURITY.md` (private disclosure address, 90-day coordinated
     disclosure, what is in scope: the agent, the SDKs, the pilot, the
     hosted cloud).
   - Write `tools/add-headers.py` and `tools/check-headers.py`; run the
     first; add the second to the verification set in `CLAUDE.md`.
   - Set the package metadata; add `deny.toml`; run `cargo deny` and fix
     what it finds.
   - Reword the two cloud docs pages and the quickstart; replace the
     repository placeholders.
   - `.github/workflows/ci.yml` on the public tree: the verification set
     from `CLAUDE.md` on `ubuntu-latest` and `macos-latest` (`cargo test`,
     `cargo clippy -D warnings`, the FEC vectors, `make -C sdks/c check`,
     `pytest sdks/python/tests`, `pnpm -r build && pnpm -r test` with
     rustdoc, `check-headers.py`); caches for cargo and pnpm; a concurrency
     group per branch. Dependabot for cargo, npm and actions, weekly.
   - Commit. Everything from here on works on clones of this commit.
2. **Create the GitHub organisation `seyd-io`** and the three repositories,
   *all private at first* (`seyd-business` stays private and starts from the
   sections moved out in step 1). The organisation bills separately from
   the personal account: enter a payment method for it. The Free plan
   covers everything except branch protection on private repositories,
   which needs Team; take Team so `seyd-cloud` has a PR-required `main`.
   Turn on Dependabot alerts everywhere (done 2026-10-06; the three
   repositories exist, private, with squash/rebase-only merges and `main`
   as default); branch protection on `main` (PR required, CI required,
   linear history, no force-push) on `seyd` and `seyd-cloud` once the branch
   exists; install the DCO app on the organisation. Secret scanning with push protection is free on public
   repositories only and switches on when `seyd` goes public; the two
   private repositories get a `gitleaks protect --staged` pre-commit hook
   instead. Claim the npm
   organisation `@seyd` and check the PyPI name `seyd` and the crates.io
   names `seyd-core`, `seyd-wire`, `seyd-fec`, `seyd-qos`, `seyd-nat`,
   `seyd-transport`, `seyd-signal-client`, `seyd-ffi`, `seydd`; publishing
   is later work (PLAN.md), but a name taken by someone else is a rename now.
3. **Split the history** from fresh clones with `git filter-repo`
   (`brew install git-filter-repo`). **Done 2026-10-06**: public `main` at
   83 commits, cloud at 28, business at 3, each scanned clean by `gitleaks`
   and the public clone built and tested on its own (every check in the
   verification set) before the push. One narrowing of the history decision:
   the prototype's own signal server (`packages/signal`, deleted
   2026-08-28) is cloud code and went to the private history, not the
   public one, so the prototype's agent and pilot are public history and
   its cloud is not. The public clone then needed one commit of its own
   (`pnpm-workspace.yaml` without `cloud/*`, the lockfile regenerated, the
   skill's self-hosting wording), and the cloud clone one (`.gitignore`).
   The commands, for the record:
   - Public: `git filter-repo --path cloud/ --path deploy/ --path web/console/
     --path packages/seyd-prober/ --path packages/signal/ --path docs/eu-hosting.md
     --path .gcloudignore --path .dockerignore --path CLAUDE-cloud.md
     --path business/ --path docs/business.md --invert-paths`, then
     `git branch -m master main`.
   - Private cloud: the cloud paths of that list without `--invert-paths`,
     with `--path-rename CLAUDE-cloud.md:CLAUDE.md` and
     `--path-rename cloud/github-workflows/ci.yml:.github/workflows/ci.yml`.
   - Private business: `--path business/ --path docs/business.md` with
     `--path-rename business/:` and `--path-rename docs/business.md:business.md`.
   - `gitleaks git .` on both; `cargo build --workspace` and `pnpm -r build`
     on the public clone with the private one absent.
   - Push: public clone to `seyd-io/seyd`, private clone to
     `seyd-io/seyd-cloud`.
4. **Wire the private repository**: `git submodule add
   https://github.com/seyd-io/seyd seyd`; the private `pnpm-workspace.yaml`,
   `package.json` and `cloud/api/deploy.sh` paths as above; `cloud/README.md`
   gains "clone with `--recurse-submodules`"; the private `CLAUDE.md` opens
   with "the engineering rules are `seyd/CLAUDE.md`; this file is the hosted
   cloud". Deploy from it once and check `/`, `/pilot/`, `/console/`,
   `/docs/` and `/docs/skill/` on the result.
5. **Local checkouts**: point `~/code/darc`'s remote at `seyd-io/seyd` and
   reset it to the filtered `main` (the memory directory keyed to that path
   survives); clone `seyd-io/seyd-cloud` to `~/code/seyd-cloud` with the
   submodule. Day-to-day: core work in `~/code/darc`, pushed to the public
   repo; a cloud change or a deploy in `~/code/seyd-cloud`, bumping the
   submodule when it needs newer open code.
6. **Make `seyd-io/seyd` public.** **Done 2026-10-06**: public, with private
   vulnerability reporting, secret scanning and push protection, Discussions,
   topics, and `main` protected (the six CI jobs and the DCO check required,
   strict, linear history, no force-push or deletion; PRs required for
   everyone but admins, so the owner can still push a fix directly). Branch
   protection on the private repositories needs a paid plan and was not
   taken; `seyd-cloud` relies on there being one committer. The DCO app is
   installed on the organisation; commits are made with `git commit -s`. Enable private vulnerability reporting
   (`SECURITY.md` and the issue-template contact link point at it) and
   push protection. Repository description, topics
   (`teleoperation`, `robotics`, `webtransport`, `quic`, `rust`, `webcodecs`),
   the docs URL in the About box, issue templates (bug, integration
   question, network diagnosis with the guidance box's Diagnostics pasted
   in), Discussions on. Update the landing page's and the docs' links to the
   source.
7. **Afterwards, in PLAN.md**: publish the packages (npm `@seyd/core`,
   `@seyd/web`; PyPI wheels bundling `libseyd` per platform; crates.io);
   tag `v0.1.0`; GitHub Releases with the `seydd` binaries for x86-64 and
   ARM64 Linux built by CI.

## Verification

- A fresh clone of the public repository, on a machine without the private
  one, passes the full verification set in `CLAUDE.md` and builds the docs
  with rustdoc; `grep -rn "cloud/" --exclude-dir=node_modules` in it finds
  only prose that says the cloud is hosted.
- The private repository builds and deploys the image from the submodule;
  the deployed `/docs/skill/files.txt` matches the public repo's
  `skills/seyd`.
- `gitleaks git .` is clean on all three repositories; `check-headers.py`
  and `cargo deny check licenses` are green in CI. CI runs on the public
  repository only: its minutes are free there and macOS runners cost ten
  times the minutes on a private one.
- Every public `Cargo.toml`, `package.json` and `pyproject.toml` declares
  `Apache-2.0`; every private one `UNLICENSED`.
- `git log --follow docs/adr/0001-wire-protocol-v2.md` in the public repo
  shows the original commits: the history survived the filter.

## Open questions

- The company name, when it exists: one commit over the headers and a
  NOTICE line (`Copyright 2026 Anton Gravestam; assigned to <company> <date>`).
- Whether the demo camera's LAN address stays in the published config
  (review item 4).
