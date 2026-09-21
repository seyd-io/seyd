---
title: Rust crates (rustdoc)
description: The workspace crates, documented by rustdoc.
---

The engine is Rust, and every crate is documented with rustdoc from its own source comments; `cargo doc --workspace --no-deps` is run by the docs build and the output is served here:

- **[seyd_core](/docs/rust/seyd_core/)** — the agent lifecycle, channels, sessions and events. `seyd_core::Agent` is what every host drives (ADR 0004); [A robot in Rust](/docs/robot/rust/) is the guide.
- **[seyd_ffi](/docs/rust/seyd_ffi/)** — the C ABI, whose doc comments become `seyd.h`.
- **[seyd_qos](/docs/rust/seyd_qos/)** — the profiles, the closed-loop controller and simulcast selection.
- **[seyd_wire](/docs/rust/seyd_wire/)**, **[seyd_fec](/docs/rust/seyd_fec/)**, **[seyd_transport](/docs/rust/seyd_transport/)**, **[seyd_nat](/docs/rust/seyd_nat/)**, **[seyd_signal_client](/docs/rust/seyd_signal_client/)** — the layers under the agent, one job each.
- **[seydd](/docs/rust/seydd/)** — the daemon.

Doctests in these crates are compiled by `cargo test --workspace`, so a code example in a Rust doc comment is verified with every test run.

:::note
When the site is built locally with `SEYD_DOCS_SKIP_RUSTDOC=1`, the links above have nothing behind them; the deployed site always carries the full output.
:::
