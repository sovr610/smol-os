# CLAUDE.md: TermOS project conventions

TermOS is a terminal-first desktop OS. It runs hosted inside any terminal
(Linux, macOS, Windows), or on bare metal as Linux + Buildroot rendering
through DRM/KMS. It's written in Rust as a Cargo workspace.

**Before starting any milestone, read [ARCHITECTURE.md](ARCHITECTURE.md),
[ROADMAP.md](ROADMAP.md) and [DECISIONS.md](DECISIONS.md).**

## Repo map
```
crates/termos-render   grid, damage, colour/themes, caps, backends (terminal/headless/KMS)
crates/termos-core     event loop, bus, Transport, protocol, App trait, config, paths
crates/termos-term     PTY + VT emulation (alacritty_terminal behind VtBackend)
crates/termos-wm       window tree, layouts, workspaces, input routing, chrome
crates/termos-shell    Desktop state, palette, which-key, bar, notifications, plugin host
crates/termos-apps     built-in apps (App trait + ratatui): viewers, files, monitor, clipboard, settings
crates/termos-hw       CPU/GPU/memory/temperature/disk/network readings (sysinfo + sysfs/NVML)
crates/termos-plugin   plugin protocol and Rust SDK; wit/ holds the WASM widget world
crates/termos-devkit   mise toolchains, Neovim/Helix + LSP/DAP, dev rootfs, panels
crates/termos-ai       providers, agent loop, tools, permissions, sandbox, audit, MCP
crates/termos-models   HF adapter, hardware fit, inference backends, OpenAI-compatible router
crates/termos-image    Buildroot external tree, image builds, QEMU boot tests
crates/termos-cli      the `termos` binary (only binary in the product)
xtask/                 repo automation (std-only)
examples/widgets/      sample WASM status-line widget (its own workspace, wasm32-wasip2)
```
Dependency direction is fixed in ARCHITECTURE.md §4. Never add an edge that points upward.

## Commands
- `cargo xtask ci`: fmt check, clippy `-D warnings`, tests, clippy for the Windows and macOS targets, repo checks and cargo-deny. **Must pass before every commit.** Install the targets once with `rustup target add x86_64-pc-windows-msvc aarch64-apple-darwin`; without one, its check is skipped (CI still runs it).
- `cargo xtask todos --write`: regenerate the ROADMAP open-work table after adding or removing work markers.
- `cargo xtask check-todos`: work-marker format, ROADMAP sync and SPDX headers.
- `cargo run -p termos-cli -- <args>`: run `termos`. With no arguments it starts the desktop, which needs a real terminal.
- `cargo xtask perf [SECS] [--target T]`: performance budgets on a release build (Linux), with SECS seconds of idle measurement (default 60); `--target x86_64-unknown-linux-musl` measures the static musl build (D-079; needs `musl-tools`).
- `termos lang check <lang>`: one language from install to the DAP handshake (D-056). It downloads mise and the language's tools, so CI runs it (`.github/workflows/languages.yml`, run on demand with workflow_dispatch); local tests use a stand-in mise through `TERMOS_MISE`.
- `cargo xtask image list|build <target>|boot <target>`: bare-metal images with Buildroot and their QEMU boot test (Linux; docs/bare-metal.md). Builds take about an hour, so the `images` workflow runs them in CI; `build` needs Buildroot's host tools and the Rust target for the board.
- `cargo xtask widgets`: rebuild the sample WASM widget and refresh `crates/termos-shell/tests/fixtures/pomodoro.wasm` (needs `rustup target add wasm32-wasip2`). Commit the fixture with the source change; CI rebuilds it and tests the fresh build.
- `cd fuzz && cargo +nightly fuzz run <target> -- -max_total_time=60`: fuzz a parser. The targets are `vt_advance`, `config_parse`, `keyspec_parse`, `input_decode`, `proto_decode`, `caps_reply`, `sixel_decode`, `apc_dcs_intercept`, `osc_signals`, `markdown_layout`, `sse_parse`, `gguf_parse`, `ppm_parse`, `mcp_message`, `plan_parse`, `cli_events`, `link_detect`, `layout_parse`, `ssh_config`, `uevent_parse` and `gallery_index`.
- `INSTA_UPDATE=always cargo test -p <crate>`: write new snapshots; review the `.snap` files before committing.
- `cargo zigbuild --release -p termos-cli --target aarch64-unknown-linux-gnu.2.31`: cross-build. Needs zig and cargo-zigbuild.
- `cargo deny check`: licenses, advisories, sources.

## Workflow
- **Milestones:** work milestone by milestone, as in ROADMAP.md. Each one ends with green `cargo xtask ci`, a short demo, a commit, a summary, and then a **stop for owner review**. Update the milestone's status and checkboxes in ROADMAP.md.
- **Ask first:** before choosing between major alternatives not already covered by DECISIONS.md, ask the owner. Each milestone lists its open questions ("Ask first").
- **Decisions:** log every decision in DECISIONS.md as a new `D-xxx` entry. The log is append-only: supersede old entries, never rewrite them. Keep ARCHITECTURE.md in sync when the design changes.
- **Commits:** Conventional Commits with a crate scope, e.g. `feat(render): …`, `fix(term): …`, `docs: …`, `ci: …`.
- **Git:** commit only on the assigned development branch, and never force-push shared branches.

## Code rules
- **SPDX header:** the first line of every `.rs` file is `// SPDX-License-Identifier: GPL-3.0-or-later`. The license is GPL-3.0-or-later (D-003).
- **No silent stubs** (D-018):
  - `todo!`, `unimplemented!` and `dbg!` are denied by clippy.
  - Unfinished work is a comment `TODO(M<n>): <what>`, naming a milestone with a `## M<n>` section in ROADMAP.md (a split milestone adds its letter: `TODO(M9b)`).
  - The `FIXME` tag is rejected.
  - Never ship code that pretends to work.
- **Unsafe code:** denied workspace-wide. Where it's unavoidable (FFI, ioctls), add `#[allow(unsafe_code)]` on the narrowest item and a `// SAFETY:` comment on every block. Prefer safe wrappers such as `rustix`.
- **Errors:** `thiserror` in libraries, `anyhow` in binaries. No `unwrap`/`expect` outside tests unless an invariant guarantees it; then use `expect("why this holds")`.
- **Output:** use `tracing`, which goes to the log file. The server never writes to stdout or stderr, because those are the user's terminal.
- **Secrets** (D-017):
  - Hold them in `secrecy` types.
  - Never put them in logs, Debug output, config files, snapshots or audit records.
  - Config refers to them as `keyring:<name>`.
- **Model names:** never hardcode them. List them from the provider APIs; tests use obvious fixture names.
- **Platforms:** hosted mode must build and pass tests on Linux (x86_64, aarch64), macOS and Windows x86_64 (Tier 2, D-035).
  - Gate Linux-only code with `cfg(target_os = "linux")` and Unix-only code with `cfg(unix)`, and give it a clear runtime error elsewhere.
  - Local IPC goes through `termos_core::local` (D-036); the plugin SDK, a leaf crate, has its own `Conn` seam (D-040). Don't use Unix sockets or named pipes directly anywhere else.
  - A Windows gap that isn't fixed yet is a `TODO(M9b)` marker, not a silent no-op (D-077).
  - Tests that drive a shell use the helpers in `tests/common/` (`cmd.exe` on Windows, `/bin/sh` elsewhere).
- **Dependencies:** prefer mature crates. Add versions to `[workspace.dependencies]` and inherit them with `dep.workspace = true`. The license must pass `deny.toml`: no OpenSSL (use rustls), and no git dependencies.
- **Idle cost:** no polling loops, and no timers while idle. Rendering is event-driven (ARCHITECTURE.md §5.5).
- **Budgets:** stay within the performance budgets (ARCHITECTURE.md §15). Hot paths (compose, diff, encode, VT parse) get criterion benchmarks.

## Tests
- Unit tests go next to the code in `#[cfg(test)] mod tests`; integration tests go in `crates/<crate>/tests/`.
- Use `insta` snapshots of rendered grids for anything visual, driven through the headless backend. Review them with `cargo insta review`, and commit the `.snap` files.
- Use `proptest` for layout and diff invariants.
- Fuzz targets live in `fuzz/` (cargo-fuzz, nightly toolchain). Add one for every parser: VT, config, protocol, capability replies.
- For the desktop, test end to end:
  - `termos-shell/tests/desktop.rs` drives a `Desktop` with real `/bin/sh` panes;
  - `termos-cli/tests/e2e.rs` runs the real binary on a PTY and reads the screen through `vt100`.
- To try the UI by hand in a sandbox, use an isolated tmux socket (`tmux -L <name>`), never the user's default server.
- CI never uses live API keys. Use recorded fixtures and mock servers.
