---
name: setup-dev-machine
description: Use when the user wants to prepare a freshly checked-out DARC repo for local development on a new Mac — installing Homebrew, Node.js, Python 3.12, FFmpeg, the darc-agent virtualenv, and npm dependencies. Triggers on phrasing like "install all tools needed and set up this machine for development", "bootstrap this machine", "set up my dev environment", "get DARC running on a new Mac".
---

# Setting up a DARC development machine

DARC's prototype runs on macOS only (Mac-to-Mac teleoperation demo). This skill
bootstraps a fresh checkout so `./dev.sh` works immediately afterward.

## What "set up" means here

1. Homebrew (installed if missing, for both Apple Silicon and Intel prefixes)
2. Node.js (for `darc-signal` and `darc-pilot`)
3. Python 3.12 + a venv at `packages/agent/.venv` with `requirements.txt` installed (for `darc-agent`)
4. FFmpeg (for `sim/video-source.sh`, and for the agent's PyAV dependency at the system level)
5. `npm install` in `packages/signal` and `packages/pilot`
6. Executable bits on `dev.sh`, `robot.sh`, `sim/video-source.sh`
7. A check (not install) for the `gcloud` CLI — only needed to deploy `darc-signal`, not for local dev

## How to do it

Run the setup script from the repo root:

```bash
./tools/setup-machine.sh
```

The script is idempotent — safe to re-run on a machine that already has some
of these tools. It auto-detects `arm64` (Homebrew at `/opt/homebrew`) vs
`x86_64` (Homebrew at `/usr/local`) and uses the correct prefix either way.

After it completes, verify the environment works:

```bash
node --version
packages/agent/.venv/bin/python --version   # should be 3.12.x
ffmpeg -version | head -1
```

Then offer to run `./dev.sh` to bring up the full local prototype (signal
server + simulated robot + agent + pilot page), or point to `robot.sh` if the
user wants to connect this machine to the already-deployed cloud signal
server instead (see `packages/agent/SETUP.md` and `PROTOTYPE.md` for the
`--signal-url`).

## Things to flag to the user, not silently work around

- **Camera permission prompt.** The first time `ffmpeg` captures the webcam
  via `sim/video-source.sh`, macOS prompts for camera access for the Terminal
  app. If video never appears, this is the first thing to check.
- **Homebrew install requires sudo/admin** on a completely fresh Mac. If the
  script fails during Homebrew installation, tell the user to run it
  interactively rather than trying to sudo around it yourself.
- **Existing Python/Node installs.** The script installs `python@3.12`
  specifically (matching what `dev.sh`/`robot.sh` expect) even if another
  Python is already the system default — it does not touch `python3` on
  `PATH`, only creates the project's own venv.
- If `./tools/setup-machine.sh` doesn't exist or has diverged from this
  description, read it before running — it's the source of truth, this file
  is just the summary.
