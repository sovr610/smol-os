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
| M0 | Plan docs, repo scaffold, CI, CLAUDE.md | ✅ |
| M1 | tmux-style hosted desktop: renderer, PTY panes, window manager, palette, config (M1+M2 combined, D-022) | 🔍 |
| M2 | Merged into M1 | 🔍 |
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
**Status:** ✅ done

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

## M1 — tmux-style hosted desktop (M1 and M2 combined)
**Status:** 🔍 done, awaiting review · Combined at the owner's request (D-022); tmux-style interaction (D-023), tiled-first with floats (D-024).

**Deliverables**
- **Renderer (`termos-render`):**
  - [x] `Grid` / `Cell` / `Color` with wide characters and graphemes; clipped drawing helpers (text, boxes, shadows)
  - [x] Frame diffing into minimal spans; terminal encoder with cursor/SGR state, DEC 2026 synchronized output and 256/16-colour downsampling
  - [x] Environment-based capability detection; themes derived from configurable base colours
- **Terminal panes (`termos-term`):**
  - [x] PTY panes on portable-pty with reader and writer threads (D-025)
  - [x] `VtBackend` over `alacritty_terminal`: rendering, events, modes, scrollback, OSC 7, sync updates
  - [x] xterm encoding of keys, mouse (SGR/X10), paste and focus for child programs
- **Core (`termos-core`):**
  - [x] Protocol-neutral input and tmux key notation
  - [x] `ClientMsg` / `ServerMsg` / `FrameUpdate`; in-process transport (D-026)
  - [x] `config.toml` with errors located by line and column, hot reload, XDG paths, file logging
  - [x] The in-process `App` trait over ratatui
- **Window manager (`termos-wm`):**
  - [x] Layout tree with splits, BSP auto-split, resize, swap, directional focus
  - [x] Presets: even-horizontal, even-vertical, main-vertical (master-stack), main-horizontal, tiled (grid); "no space" refusal
  - [x] Workspaces; floating windows (raise, cascade, minimize, snap, minimum size); zoom
  - [x] Hit testing; composition with junction-aware separators, an accent ring on the focused pane, and float borders, titles, buttons and shadows
- **Desktop (`termos-shell`):**
  - [x] Prefix key `C-b` with tmux default bindings, repeatable resize, root bindings, config overrides and unbinding
  - [x] Command registry shared by keys, palette and (later) JSON-RPC
  - [x] tmux-style status line: session, tabs with `*`, `-`, `!` and `Z` flags, minimized chips, PREFIX/COPY badges, toasts, optional clock
  - [x] Command palette (`C-Space`, `prefix :`) with fuzzy matching and key hints
  - [x] Which-key after a pause following the prefix; `prefix ?` shows every binding
  - [x] Status-line prompts: rename; y/n confirmations
  - [x] Copy mode: vi motions, selection, search, yank to the paste buffer and OSC 52, paste
  - [x] Mouse:
    - [x] focus;
    - [x] separator drag;
    - [x] float move, resize and edge-snap;
    - [x] title buttons;
    - [x] wheel scrollback;
    - [x] drag-select to copy;
    - [x] forwarding to mouse-aware programs (Shift bypasses);
    - [x] alternate scroll for less/man;
    - [x] status-line clicks
  - [x] Notifications and the notification-center app; the first-run tutorial app
  - [x] Event-driven server loop: immediate frames for small updates, coalescing only for floods, no idle wakeups apart from the optional clock (D-028)
- **The `termos` command (`termos-cli`):**
  - [x] `termos` runs server and client in-process, with panic-safe terminal restore and a nesting guard (D-027)
  - [x] `termos config check | default | path`
- **Quality:**
  - [x] Fuzz targets (VT, config, key notation) and a CI fuzz job
  - [x] Release-mode performance-budget job (`cargo xtask perf`)
  - [x] [docs/getting-started.md](docs/getting-started.md)

**Acceptance (measured)**
- **Interactive use:** shells, splits, floats, workspaces, palette, copy mode and quit all work, driven in a real terminal (tmux) and by the PTY end-to-end test.
- **Damage tracking:** one echoed keystroke produces a one-cell frame, and an idle desktop sends nothing (server test).
- **Snapshots:** splits, every preset, floats with shadows and zoom.
- **Property tests:** encoded frames reproduce the grid in an independent emulator; any sequence of layout operations tiles the screen exactly.
- **Budgets, release build on the dev container:**

  | Metric | Measured | Budget |
  | --- | --- | --- |
  | Keystroke-to-screen latency, p50 | 0.5 ms | — |
  | Keystroke-to-screen latency, p99 | 2.6 ms | < 16 ms |
  | Idle CPU over 10 s | 0.000% | < 1% |
  | RSS | 5.8 MB | < 150 MB |
  | 300k-line flood | rendered in 0.23 s | — |

- **Fuzzing:** CI runs 60 s per target. The VT target found an out-of-memory: alacritty_terminal reflowing a wide character into a one-column pane. It is fixed by never sizing the emulator below 2 columns (`MIN_COLS`), and a regression test keeps it fixed. 3 minutes of local fuzzing afterwards found nothing more.

**Demo**
```bash
cargo run --release -p termos-cli
```
Then try: `C-b %`, `C-b "`, `C-b f`, `C-b c`, `Ctrl+Space`, `C-b [`, and the mouse.

**Not in this milestone:** detach/attach and the socket transport (M3); images and active capability queries (M4); the Kitty keyboard protocol *for child programs* (M4).

---

## M2 — merged into M1
**Status:** 🔍 · Everything planned for M2 was delivered in M1 (D-022). This heading stays so milestone numbers and work markers stay stable.

---

## M3 — Session server, persistence, control and plugin API
**Status:** ⏳ · **Ask first:** nothing new. Session restore (D-011) should store the tmux-style layout tree and float geometry from M1.

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
| M3 | `crates/termos-cli/src/main.rs` | run the server as a daemon on its socket and attach to it. |
| M3 | `crates/termos-shell/src/lib.rs` | out-of-process plugin host over the JSON-RPC control API. |
| M3 | `crates/termos-shell/src/server.rs` | plugin connections (token check, plugin API). |
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
