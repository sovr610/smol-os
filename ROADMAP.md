# TermOS roadmap

TermOS is built milestone by milestone. Every milestone ends with tests
passing, a short demo, a commit and a summary, and then **stops for owner
review**. Before a milestone starts, the owner is asked about its open
questions (DECISIONS.md, "Open questions").

**Status legend:**
- ✅ done
- 🔍 done, awaiting review
- 🚧 in progress
- ⏳ not started

| Milestone | Theme | Status |
| --- | --- | --- |
| M0 | Plan docs, repo scaffold, CI, CLAUDE.md | 🔍 |
| M1 | Hosted-mode renderer: one window running a shell | ⏳ |
| M2 | Window manager, palette, config | ⏳ |
| M3 | Session server, detach/attach, persistence, plugin API | ⏳ |
| M4 | Graphics, themes, first built-in apps, WASM widgets | ⏳ |
| M5 | Developer environment | ⏳ |
| M6 | AI agents | ⏳ |
| M7 | Local models | ⏳ |
| M8 | Bare-metal images | ⏳ |
| M9 | Polish and release | ⏳ |

Unfinished work in code is always a `TODO(M<n>)` marker (D-018). The
generated table at the end of this file lists every open marker.

---

## M0 — Plan docs, repo scaffold, CI, CLAUDE.md
**Status:** 🔍 awaiting review

**Deliverables**
- [x] ARCHITECTURE.md, ROADMAP.md, DECISIONS.md (D-001 to D-021, open questions O-1 to O-14)
- [x] CLAUDE.md with project conventions; README.md
- [x] Cargo workspace: 11 crates from the spec plus a std-only `xtask`. Library crates document their planned modules and carry `TODO(Mn)` markers.
- [x] `termos --version` / `--help` (clap), with integration tests
- [x] `cargo xtask ci | check-todos | todos [--write]`: checks work-marker format, milestone existence, ROADMAP sync and SPDX headers, with unit tests
- [x] Pinned toolchain (1.97.0), workspace lints (no `todo!`/`unimplemented!`/`dbg!`; unsafe denied by default)
- [x] GPL-3.0-or-later license and a GPLv3-compatible `deny.toml`
- [x] GitHub Actions: lint (fmt, repo checks, cargo-deny); clippy + tests on x86_64 Linux, aarch64 Linux and macOS; zigbuild cross-builds for aarch64 and x86_64 Linux
- [x] Dependabot for cargo and Actions

**Acceptance:** `cargo xtask ci` passes locally, `cargo deny check` passes, the aarch64 cross-build produces an aarch64 ELF, and CI is green.

**Demo**
```bash
cargo xtask ci
cargo run -p termos-cli -- --version
cargo xtask todos
```

---

## M1 — Hosted-mode renderer: one window, one shell, damage tracking
**Status:** ⏳ · **Ask first:** nothing; O-2 (PTY layer) is the lead's call and gets logged.

**Deliverables**
- **`termos-render`:**
  - `Grid` / `Cell` / `Color`, wide-char handling (`unicode-width`, grapheme segmentation);
  - frame diff into row spans;
  - the terminal backend: crossterm, synchronized output, colour downsampling, env-based capability detection;
  - the headless backend.
- **`termos-term`:**
  - PTY spawning of `$SHELL` with resize (`TIOCSWINSZ`);
  - `VtBackend` over `alacritty_terminal` (pinned) and blitting into the grid;
  - title and bell events.
- **`termos-core`:**
  - the tokio event loop and the event-driven frame scheduler (coalescing, no idle timers);
  - the in-memory `Transport`;
  - the first protocol message types;
  - XDG paths;
  - file logging (`tracing`).
- **`termos-cli`:** `termos` opens the alternate screen with one bordered, titled window running the user's shell. A quit binding exits. Terminal resizes resize the window and PTY.
- **Fuzzing:** a `fuzz/` target that feeds arbitrary bytes into `VtBackend`, and a CI fuzz smoke job (timed run on nightly).
- **Performance harness:** the idle-CPU test, input→flush latency instrumentation, and criterion benchmarks for diff/encode.

**Acceptance**
- Interactive shells, `vim`/`nvim`, `htop` and `less` work, including resize.
- Damage tracking is proven: echoing one character emits bytes for exactly one cell (headless test).
- Snapshot tests of window chrome and shell output.
- Idle CPU < 1% and p99 input→flush < 16 ms on the CI x86_64 runner.
- Hosted RSS < 150 MB.
- The fuzz smoke run finds no crash.

**Demo:** `cargo run --release -p termos-cli`

---

## M2 — Window manager
**Status:** ⏳ · **Ask first:** O-1 (default keybinding scheme).

**Deliverables**
- **Windows and layouts:**
  - the window tree: workspaces, a floating layer, and a tiling tree with splits, tabs and stacks;
  - master-stack, grid and BSP layouts;
  - minimize/maximize/fullscreen, z-order and snapping.
- **Mouse:** focus, drag-move via the title bar, edge/corner resize, wheel scrollback, and forwarding to mouse-aware children.
- **Workspaces:** virtual desktops with switching.
- **Shell (`termos-shell`):**
  - the command palette (Ctrl+Space) over named commands, fuzzy-matched with `nucleo`;
  - the which-key overlay;
  - a status bar showing workspaces, a task strip and a clock;
  - notification toasts and a notification center;
  - the first-run tutorial.
- **Clipboard:** mouse selection and copy, internal clipboard ring, OSC 52 passthrough.
- **App trait:** the in-process `App` trait in `termos-core` (ARCHITECTURE.md §9).
- **Config:** `config.toml` with keybindings and layout defaults, span-accurate errors, hot reload, `termos config check`, and a config-parser fuzz target.

**Acceptance**
- Snapshot tests for every layout.
- proptest invariants for the layouts.
- Scripted mouse drag/resize tests through the headless client.
- A hot-reload test.
- The latency and idle budgets still hold with 8 windows open.

**Demo:** `termos`, then open several shells and drag, tile, switch workspaces and use the palette.

---

## M3 — Session server, persistence, control and plugin API
**Status:** ⏳ · **Ask first:** nothing new.

**Deliverables**
- **Server and socket:**
  - `termos server` daemon and the Unix-socket `Transport`, with SO_PEERCRED/getpeereid checks;
  - the versioned handshake;
  - `termos attach [-t session]`, `detach`, `ls`, `kill-session`, `-L <server>`.
- **Clients:** several clients at once, and the session size policy.
- **Persistence:** session snapshots and restore after a reboot (D-011); restore policy `always | ask | never`.
- **SSH:** documentation for the SSH workflow.
- **Control API:** JSON-RPC 2.0 and `termos msg <method> [params]`.
- **Plugins:**
  - out-of-process plugin API v1 (manifest, permissions, windows, palette commands, status widgets, notifications);
  - the `termos-plugin` SDK crate (logged in DECISIONS.md) and an example plugin.
- **Fuzzing:** a protocol-decoder fuzz target.

**Acceptance**
- Integration tests:
  - detach and reattach preserves the screen and running processes;
  - two clients attach at the same time;
  - killing and restarting the server restores layout and working directories;
  - foreign-uid connections are rejected;
  - a protocol version mismatch is handled cleanly;
  - the example plugin opens a window.

**Demo:** `termos`, detach, `termos ls`, `termos attach`; then `ssh -t host termos attach`.

---

## M4 — Graphics, themes, first apps, WASM widgets
**Status:** ⏳ · **Ask first:** O-8 (WASM engine). M4 may be split into M4a (graphics and themes) and M4b (apps and widgets) if it grows too large; the owner decides.

**Deliverables**
- **Capability detection:** full detection with active queries (ARCHITECTURE.md §5.7).
- **Images:** the fallback ladder (Kitty / Sixel / iTerm2 / blocks / braille), with occlusion-correct placement.
- **Widgets:** chart and image widgets for apps.
- **Images in PTY windows:** intercepting Kitty and Sixel sequences.
- **Themes and polish:**
  - dark, light and high-contrast themes, with the default following OSC 11;
  - drop shadows and rounded borders;
  - smooth animations (window open/close, toasts) that respect reduced motion.
- **`termos-hw`:** a new crate (logged in DECISIONS.md) for CPU/GPU/RAM/temperature probing.
- **Apps:** file manager, system monitor (CPU/GPU/RAM/temperatures), image viewer, markdown viewer, settings, clipboard manager.
- **WASM widgets:** the runtime behind the `wasm-widgets` feature, the WIT world, and a sample widget.

**Acceptance**
- Capability-detection unit tests replaying recorded responses from kitty, WezTerm, foot, iTerm2, xterm, Windows Terminal, GNOME Terminal and tmux.
- A snapshot test for each app.
- Idle CPU stays under 1% with the system monitor closed.
- With no WASM widget configured, RAM is unchanged.

**Demo:** open the file manager and preview an image; open the system monitor; switch to the high-contrast theme.

---

## M5 — Developer environment
**Status:** ⏳ · **Ask first:** O-3 (Git UI).

**Deliverables**
- **Toolchains:** `termos lang install|use|list|remove` wrapping mise, with per-project versions.
- **Editor:** a managed Neovim config (D-012) with LSP for every listed language (ARCHITECTURE.md §10) and DAP; a Helix `languages.toml`; LSP/DAP installers.
- **Dev rootfs:** `termos devroot create|enter|update`, used on bare metal and testable hosted on Linux.
- **Panels:** the Git UI, `termos new <template>`, and build/run/test panels.
- **Podman:** integration where the hardware allows.
- **Package manager UI:** an app over mise, the dev rootfs (apt) and the host package manager.
- **Docs:** `docs/languages.md` listing which languages and tools are unavailable on which architecture.

**Acceptance:** for each language on x86_64 and aarch64 (where supported), CI installs it with mise, builds and runs a hello-world, and completes a headless `nvim` LSP initialize handshake.

**Demo:** `termos lang install go`, `termos new go-cli demo`, then edit in Neovim with diagnostics and a breakpoint.

---

## M6 — AI agents
**Status:** ⏳ · **Ask first:** O-4 (SDK crates vs our own clients), O-5 (secret store on bare metal).

**Deliverables**
- **Providers:** the `Provider` trait with Anthropic, OpenAI, Moonshot Kimi, generic OpenAI-compatible and local implementations. Model lists come from the provider APIs; model names are never hardcoded.
- **Agent windows:** streaming chat windows with tool calling, persisted history, and per-session token and cost tracking.
- **Tools:** files, commands (PTY-backed), code search, fetch.
- **Safety:**
  - the permission engine, profiles and confirmation UI with diff previews;
  - the bubblewrap + Landlock sandbox;
  - the hash-chained audit log and its viewer.
- **MCP:** an `rmcp` client over stdio and HTTP, with allowlisting.
- **External agents:** launcher presets for Claude Code, Codex CLI and others.
- **Secrets:** stored in the keyring or the encrypted store, as `secrecy` types.

**Acceptance**
- Contract tests against recorded fixtures and mock servers; no live keys in CI.
- Sandbox tests: writing outside the workspace and reaching the network are both denied.
- Audit-chain verification.
- A canary-key test: no secret appears in logs, audit records or snapshots.

**Demo:** open an agent window, pick a provider and model from the fetched list, and ask it to fix a failing test, approving its edits.

---

## M7 — Local models
**Status:** ⏳ · **Ask first:** O-6 (llama.cpp integration), O-7 (ONNX vs candle), and confirm D-021.

**Deliverables**
- **CLI:** `termos model search|pull|list|rm|info|recommend`.
- **Downloads:** resumable, checksum-verified downloads into the HF-compatible cache, with GC and the HF token.
- **Hardware:** detection in `termos-hw` (NVML, ROCm, Vulkan, CPU features, RAM/VRAM, Pi model, Coral, Hailo, TPU VM), and a fit estimator shown before download.
- **Backends:**
  - llama.cpp as the default, with automatic CPU/CUDA/ROCm/Vulkan selection and a manual override;
  - ONNX Runtime and/or candle;
  - the optional Python sidecar;
  - TPU, Coral and Hailo paths where supported.
- **Router:** the OpenAI-compatible `termos models serve`, wired into `termos-ai` as the local provider.
- **Docs:** explicit limits for each accelerator.

**Acceptance**
- Resume test against a mock Hub with an interrupted transfer.
- A checksum mismatch is rejected.
- Estimator tests against known model metadata.
- End to end on CI: pull a tiny GGUF and chat with it through the router on CPU.

**Demo:** `termos model recommend`, `termos model pull <repo>`, then chat with it in an agent window.

---

## M8 — Bare-metal images
**Status:** ⏳ · **Ask first:** O-9 (font rasterizer), O-10 (init), O-11 (networking), O-14 (aarch64 bootloader).

**Deliverables**
- **Buildroot:** the external tree in `termos-image` with defconfigs for x86_64 (UEFI + BIOS ISO), aarch64 UEFI, Pi 5, Pi 4, and Pi 3 / Zero 2 W (best effort).
- **Display:** the DRM/KMS backend with our font rasterizer, adjustable font size and a glyph atlas.
- **Input and boot:** libinput/xkbcommon input through libseat, and booting straight into TermOS.
- **Provisioning:** the dev rootfs and the data partition layout.
- **QEMU boot tests:** x86_64 (OVMF and SeaBIOS) and aarch64 virt in CI, with the serial marker, a screendump check and RAM measurement.
- **Legal:** a `legal-info` license report.

**Acceptance**
- CI builds the x86_64 and aarch64 virt images and boots both to `TERMOS-READY`.
- Idle RAM < 150 MB, measured in QEMU.
- Pi 4 latency measured on hardware and recorded.

**Demo:** `cargo xtask image build x86_64 && cargo xtask image boot x86_64`

---

## M9 — Polish and release
**Status:** ⏳ · **Ask first:** O-12 (A/B update tool), O-13 (musl binaries).

**Deliverables**
- **Install and update:** an installer (`termos install` from the live ISO), and atomic A/B updates with signed bundles and rollback.
- **Pi tuning:** performance work until the Pi 4 is under 33 ms input→frame.
- **Release pipeline:** hosted binaries, the x86_64 ISO, the aarch64 UEFI image, Pi `.img` files, checksums and signatures.
- **Documentation:** user docs and a getting-started tutorial.
- **Accessibility:** a review of the high-contrast theme and KMS font sizes.

**Acceptance**
- An update from version N to N+1 succeeds.
- A forced boot failure rolls back to the previous slot.
- The release artifacts boot and install.

---

## After 1.0 (unscheduled)
- A screen-reader-friendly mode (the brief lists it as "later").
- Remote attach with local capabilities: a local client talking to a remote server over a forwarded socket.
- seccomp filters for agent sandboxes, if they aren't done in M6.
- Lock screen and multi-user seats on bare metal.

---

## Open work markers

Generated from the `TODO(M<n>)` markers in the code. Don't edit by hand; run `cargo xtask todos --write`.

<!-- open-work:begin (generated by `cargo xtask todos --write`) -->
| Milestone | Where | What |
| --- | --- | --- |
| M1 | `crates/termos-cli/src/main.rs` | run the hosted desktop (server + terminal client) when invoked without a subcommand. |
| M3 | `crates/termos-core/src/paths.rs` | runtime directory for server sockets ($XDG_RUNTIME_DIR/termos, 0700, with a per-uid fallback). |
| M3 | `crates/termos-core/src/transport.rs` | Unix-socket adapter (postcard frames, SO_PEERCRED/getpeereid checks, handshake versioning). |
| M3 | `crates/termos-shell/src/lib.rs` | out-of-process plugin host over the JSON-RPC control API. |
| M4 | `crates/termos-apps/src/lib.rs` | file manager, system monitor, image viewer, markdown viewer, settings, clipboard manager. |
| M4 | `crates/termos-render/src/caps.rs` | active capability queries (DA1, XTVERSION, Kitty graphics, DECRQM 2026, OSC 11, cell size). |
| M4 | `crates/termos-render/src/lib.rs` | image protocols (Kitty, Sixel, iTerm2, block/braille fallback) with occlusion handling. |
| M4 | `crates/termos-shell/src/lib.rs` | optional WASM runtime for status-bar widgets (behind a cargo feature). |
| M4 | `crates/termos-term/src/input.rs` | Kitty keyboard protocol encoding for child programs that enable it (CSI > u). |
| M4 | `crates/termos-term/src/lib.rs` | intercept Kitty graphics / Sixel sequences so images work inside PTY windows. |
| M5 | `crates/termos-apps/src/lib.rs` | package manager UI over mise, the dev rootfs and the host package manager. |
| M5 | `crates/termos-devkit/src/lib.rs` | mise-backed toolchain manager, Neovim + LSP/DAP setup, dev rootfs, Git/build panels. |
| M6 | `crates/termos-ai/src/lib.rs` | providers, streaming agent windows, sandboxed tools, permission engine, audit log, MCP, secrets. |
| M7 | `crates/termos-models/src/lib.rs` | HF adapter, hardware-aware recommendations, llama.cpp backend, OpenAI-compatible router. |
| M8 | `crates/termos-image/src/lib.rs` | Buildroot external tree, image build driver, QEMU boot tests for x86_64 and aarch64 virt. |
| M8 | `crates/termos-render/src/lib.rs` | DRM/KMS backend with font rasterizer and adjustable font size. |
| M9 | `crates/termos-image/src/lib.rs` | installer and atomic A/B updates. |
<!-- open-work:end -->
