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
| M3 | Session server, detach/attach, persistence, plugin API | 🔍 |
| M3.5 | Native Windows and CI test builds (D-035 to D-041) | 🔍 |
| M4a | Graphics and themes: capability probe, images, themes, animations, Kitty keyboard (D-042 to D-049) | 🔍 |
| M4b | Apps, `termos-hw`, WASM widgets (D-042, D-050 to D-052) | 🔍 |
| M5a | Languages and editors: `termos lang` (mise), managed Neovim/Helix with LSP and DAP, templates, task panels, Git UI (D-053 to D-057) | 🔍 |
| M5b | Dev rootfs, Podman, package manager app (D-053, D-109, D-110) | 🔍 |
| M6 | AI agents: providers, agent windows, permissions, sandbox, audit, MCP, external agents, secrets (D-058 to D-064) | 🔍 |
| M7 | Local models: Hugging Face downloads, fit estimates, llama.cpp, candle embeddings, the router, agents' local provider (D-065 to D-068) | 🔍 |
| M8 | Bare-metal images: the KMS console, Buildroot images for x86_64, aarch64 and the Pis, QEMU boot tests (D-072 to D-076) | 🔍 |
| M9 | Release: A/B updates, the live ISO and installer, the network page, the release pipeline with musl builds, docs (D-077 to D-083) | 🔍 |
| M10a | Mouse-first desktop: menus, context menus, hover and double-click; the `tos` command in every pane (D-084, D-085) | 🔍 |
| M10b | Graphics: richer chrome, icons, wallpaper, pixel charts where the terminal can show them (D-084, D-087 to D-090) | 🔍 |
| M10c | The command center: live views and history of the AI and the OS, with actions (D-084, D-092, D-093) | 🔍 |
| M10d | Claude Code and Codex CLI: installed by TermOS, with a TermOS MCP server (D-084, D-094, D-095) | 🔍 |
| M11a | AI accounts: several subscriptions per provider, switching with the conversation kept, the usage app (D-096, D-097) | 🔍 |
| M11b | Skills that teach Claude Code, Codex and other CLIs to use TermOS, and `termos agent ask` (D-096, D-098) | 🔍 |
| M11c | The harness: one coding tool over Claude Code, Codex and API models, routing each task to the model that suits it (D-096, D-099, D-100) | 🔍 |
| M11d | AI accounts: sign-in from TermOS through each vendor's own flow, API keys per account, Gemini CLI (D-111, D-112) | 🔍 |
| M12a | Parallel agents in git worktrees, a review window that answers the agent, best-of-N (D-111, D-113, D-114) | 🔍 |
| M12b | Agent attention and resume: "needs you" badges, jump to the waiting agent, agent panes resumed after restore, command blocks (D-111, D-115, D-116) | 🔍 |
| M12c | Remote access: a web client with share links, remote attach with local rendering, approvals on the phone (D-111) | ⏳ |
| M12d | Hardening: pinned Actions, seccomp, a macOS sandbox, a network allowlist for agent CLIs (D-111) | ⏳ |
| M12e | Harness v2, from T3 Code's design: long-lived two-way sessions with the CLIs, capability-based fallbacks, per-task checkpoints (proposed) | ⏳ |
| M13a | Links, hints, search and finished commands: programs' links and paths clickable, hint mode, search across panes, alerts when a long command finishes (D-117, D-118) | 🚧 |
| M13b | Layouts and workspace tools: layout files, a drop-down terminal, typing into several panes, saved commands, SSH hosts, pane recording (D-117, D-119, D-120) | 🚧 |
| M13c | Processes and files: a process manager; bookmarks, two panes, search, archives, git status, bulk rename and disk usage in Files (D-117) | ⏳ |
| M13d | The image as a computer: lock screen, power menu, battery, brightness and lid, clock and time zones, USB drives, GPT disks, AMD graphics (D-117) | ⏳ |
| M13e | The image's hardware: several displays and hotplug, Bluetooth, sound (D-117) | ⏳ |
| M13f | Accessibility and translations: a screen-reader mode, speech on the console, the interface in other languages (D-117) | ⏳ |
| M13g | Packages, a gallery, settings that move: Homebrew, winget, Scoop, .deb, .rpm, AUR, Nix; plugins, widgets and themes to install; settings export and import (D-117) | ⏳ |
| M9b | Platform gaps: the Windows agent sandbox and input, snapshots and sensors on macOS and Windows, Kitty keyboard flags, app windows in sessions, the KMS console's clipboard, bell and images (D-077, D-101–D-108) | 🔍 |

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
**Status:** 🔍 done, awaiting review · Owner answers: plain `termos` attaches to or creates `main` (D-029); restore relaunches shells plus a safe-program allowlist, policy `ask` by default (D-033).

**Deliverables**
- **Server and socket:**
  - [x] `termos server` daemon: started on demand, `setsid`, signal handling, stops with its last session (D-030)
  - [x] Unix socket in an owner-checked 0700 directory; stale-socket replacement; peer-uid check (D-031)
  - [x] Versioned JSON-RPC `hello` handshake for frontends, control clients and plugins; clean version-mismatch and not-found errors
  - [x] Frontends on length-prefixed postcard frames (16 MiB cap); JSON-RPC lines (4 MiB cap)
  - [x] `termos` (attach/create `main`), `new [-s] [-d]`, `attach [-t]`, `detach`, `ls`, `kill-session`, `kill-server`, `restore`, `-L`/`-S`; tmux aliases
  - [x] `C-b d` detach, `C-b (`/`)` session switching, `C-b $` rename; session commands in the palette
  - [x] `$TERMOS=<socket>,<pid>,$<session>` in panes, so CLI commands inside a pane target their own server and session
- **Clients:**
  - [x] Several sessions per server and several clients per session, with per-client palette, prompts and which-key (D-032)
  - [x] Session size policy `latest | smallest | largest`, with frames cropped for smaller terminals
  - [x] Frame backpressure: one frame in flight per client, so slow links get coalesced diffs
- **Persistence:**
  - [x] Snapshots: layout, directories and foreground programs, plus optional scrollback text. Mode 0600, atomic writes, debounced, written at shutdown and deleted on an intentional end (D-033)
  - [x] Restore: shells in their saved directories; allowlisted programs re-run; others named; damaged files repaired
  - [x] Policy `always | ask | never`: the status-line question, in-place restore, `termos restore` and `session.restore` on demand
- **SSH:**
  - [x] [docs/sessions.md](docs/sessions.md): the SSH workflow, `-L` servers, OSC 52 over SSH, slow links
- **Control API:**
  - [x] JSON-RPC 2.0 methods for the server, sessions, clients, panes, notifications, commands and plugins; every named command is also a method
  - [x] `termos msg METHOD [JSON]`
- **Plugins:**
  - [x] Plugin API v1: manifest, consent through `[plugins] enabled`, per-launch tokens, permissions, windows, palette commands, status widgets, notifications, input events (D-034)
  - [x] `termos-plugin` SDK crate (protocol types, blocking client) and `examples/clock.rs`
  - [x] [docs/plugins.md](docs/plugins.md), with Rust and Python examples
- **Fuzzing:**
  - [x] `proto_decode` target (frames in both directions, JSON-RPC and hello parsing) in the CI fuzz job

**Acceptance (tested)**
- `termos-shell/tests/server.rs` runs a real socket server in a temp directory and covers:
  - **Detach and reattach:** the earlier output and the running shell are still there.
  - **Two clients:** both see the session; keys from either reach the pane; the smaller terminal gets cropped frames.
  - **Handshake:** a foreign uid is refused (`-32003`), and so are control connections from it. A version mismatch gets `-32001`; a missing session gets `-32004`.
  - **Control API:** splits by command and by method, `pane.list`, `send-text`, `send-keys`, notifications, `pane.kill`, session create/rename/kill, and the error codes.
  - **Sessions:** switching with `C-b (`/`)`; when a client's session ends, it moves to another one.
  - **Server lifecycle:** it stops with its last session, `server.kill` works, a second server is refused on a live socket, and the socket file is removed.
  - **Restart and restore:** a restarted server restores the layout, the directories, allowlisted `tail -f` (re-run) and a named `sleep` (not re-run). Command lines of other programs are not stored. Covered under `always`, `ask` → y (in place) and `ask` → n then on-demand restore (renamed `main-2`). Snapshots are deleted on intentional ends.
  - **Plugins:** a raw-JSON-RPC Python plugin shows a window and a widget; its command runs from the palette and from the API; keys reach it; close notices arrive. A permission refusal, a missing plugin, the Rust SDK against the real server, one connection per token, and disabling by config reload are covered too.
  - **Damage tracking:** one echoed key is a one-cell frame; an idle server sends nothing.
- `termos-cli/tests/e2e.rs` runs the real binary and daemon on PTYs:
  - split, `C-b d` (`[detached (from session main)]`), `termos ls`, `termos msg pane.list`, reattach from a second terminal with the screen intact;
  - `msg` send-text and notify; `msg` and `detach` from inside a pane through `$TERMOS`;
  - a missing session, `kill-server`, `ls` with no server, then `termos restore`.
- Unit tests:
  - the frame codec and size caps; handshake checks;
  - snapshot round-trip, file permissions and damaged files;
  - relaunch rules; layout remap and repair;
  - manifests, tokens, styled text and command names; the SDK against a fake server.
- **Budgets** (release build on the dev container, `cargo xtask perf 10`, client and daemon together):

  | Metric | Measured | Budget |
  | --- | --- | --- |
  | Keystroke-to-screen latency, p50 | 0.7 ms | — |
  | Keystroke-to-screen latency, p99 | 1.8 ms | < 16 ms |
  | Idle CPU over 10 s | 0.1% (one 10 ms tick: the debounced snapshot after `cat` came to the front) | < 1% |
  | RSS | 5.3 MB client + 6.8 MB server = 12.2 MB | < 150 MB |
  | 300k-line flood | rendered in 0.17 s | — |

**Demo** (live, in an isolated `tmux -L termos-demo` with its own TermOS home):
1. `termos`, `C-b %`, then `C-b d` prints `[detached (from session main)]`.
2. `termos ls` shows `main: 1 workspaces, 2 panes`. `termos attach` brings back both panes with their output.
3. A second terminal attaches at the same time (`termos msg client.list` shows two clients). `termos msg notify.send` shows a toast in both.
4. Enabling the example clock plugin and reloading the config opens its window and puts `⏱ 18:09` in the status line.
5. `kill -9` the server. `termos ls` then says no server is running, and `termos` asks "Restore session main saved at 18:09? (y/n)". `y` restores the split; the plugin widget is still there.

The demo found one bug, fixed with a regression test: a session restored in place lost the plugin widgets and the palette's session list.

**CI** (all 8 jobs green, including macOS) found three more things, all fixed:
- macOS refuses socket timeouts once the peer has hung up, which made the SDK report an I/O error instead of "closed";
- two tests read output before it had arrived;
- several tests checked whole screen rows, which the pane beside them could fool.

The shell and CLI suites then passed 6 out of 6 runs with every CPU busy.

```bash
termos            # C-b d to detach
termos ls
termos attach
ssh -t host termos
```

**Not in this milestone:** sandboxing launched plugins (M6, with the agent sandbox); WASM widgets (M4); attaching a local client to a remote server through a forwarded socket (later); `subscribe` event streams on the control API (with M6 agents).

---

## M3.5 — Native Windows and CI test builds
**Status:** 🔍 done, awaiting review · Owner answers: native Windows (Windows Terminal or PowerShell, panes through ConPTY), as its own reviewed step before M4a; ready-to-run test builds for every push on all four platforms (D-035, D-041). Leftover Windows work is tracked as `TODO(M9)`.

**Deliverables**
- **Platform seams:**
  - [x] `termos_core::local`: one local-IPC API (listener, stream, peer identity, probe) over Unix sockets and Windows named pipes; transport, server and CLI use only it (D-036)
  - [x] Paths per platform (`%APPDATA%`, `%LOCALAPPDATA%`), with `XDG_*` overrides everywhere (D-038)
  - [x] Default shell: `pwsh` → `powershell` → `%COMSPEC%` → `cmd.exe`; `$SHELL` ignored on Windows
  - [x] Snapshots: Unix modes only on Unix, retried renames, Windows device names encoded
  - [x] Plugin tokens from `getrandom` (D-040)
  - [x] `.gitattributes` (LF everywhere, binary images); the Windows C runtime linked statically
- **Windows:**
  - [x] Named pipes with an owner-only DACL, remote clients rejected, first instance reserved, SID checks both ways, anonymous impersonation (D-036)
  - [x] The daemon: no console window, own process group, outside the starting job; console close, logoff and shutdown save like SIGTERM (D-037)
  - [x] ConPTY panes with a waiter thread for end of output (D-039)
  - [x] Client: VT support required, AltGr typing, truecolor by default
  - [x] Plugin SDK over the pipe with overlapped I/O (D-040)
- **Tests:**
  - [x] Shell-agnostic helpers (`cmd.exe` on Windows, `/bin/sh` elsewhere), so the desktop, server and CLI suites run on Windows; only POSIX-specific tests are gated
  - [x] Pipe names, Windows paths and shell order are unit-tested on every platform
- **CI** (D-041):
  - [x] Windows x86_64 test job; Windows-target clippy in the lint job and in `cargo xtask ci`
  - [x] Daemon smoke test on every OS (`.github/scripts/daemon-smoke.sh`)
  - [x] Test builds for every push: Linux x86_64/aarch64 and macOS arm64 `.tar.gz`, Windows x86_64 `.zip`, with `BUILDINFO.txt`, kept 14 days; the Linux builds are smoke-run on matching runners
- **Docs:**
  - [x] [getting-started.md](docs/getting-started.md): test builds, macOS quarantine and SmartScreen, Windows notes
  - [x] Windows in [sessions.md](docs/sessions.md) and [plugins.md](docs/plugins.md); ARCHITECTURE.md §1–3, §7, §8, §14, §17, §18

**Acceptance**
- `cargo xtask ci` green on Linux, including clippy for the Windows target.
- All CI jobs green, including the Windows tests, the daemon smoke on all four platforms and the downloaded Linux builds.
- The owner tries the Windows zip in Windows Terminal: attach, split, detach, `termos ls`, attach again.

**What runs on Windows in CI**
- Unit tests of every crate, including the secured pipes (connect, peer SID, second server refused), pipe names, Windows paths and shell order, and ConPTY panes (`cmd.exe`, exit codes, input).
- `termos-shell/tests/desktop.rs` and `tests/server.rs` with real `cmd.exe` panes: detach and reattach, two clients, the control API, sessions, the server lifecycle, and plugins (the Python plugin over the pipe, the Rust SDK). The POSIX-only cases (`stty`, `tail -f` restore, Unix modes, `/proc`) stay `cfg(unix)`.
- `termos-cli/tests/e2e.rs`: the real binary and daemon on ConPTY terminals, read through `vt100`.
- The daemon smoke script against the debug build and against the packaged release build.

**Demo** (live, Linux, in an isolated `tmux -L termos-demo` with its own TermOS home, as a regression check of M3 after the refactor):
1. `termos`, `C-b %`, `echo right-pane-alive`, `C-b d`.
2. `termos ls` shows `main: 1 workspaces, 2 panes`; `termos msg notify.send` and `termos msg pane.list` answer.
3. `termos attach` brings back both panes with their output.
4. `kill -9` the server; `termos ls` says no server is running; `termos` asks "Restore session main saved at 20:17? (y/n)", and `y` restores the split.

**CI found** (each fixed, with the platform's tests now covering it):
- ConPTY asks for the cursor position when it starts and sends nothing until answered: panes were fine because the emulator answers, but the raw PTY tests and the e2e harness needed to answer too.
- With one waiting pipe instance, plugins starting together could find the pipe busy; Python's `open()` doesn't retry. The server now keeps four instances waiting.
- Git Bash has no `/dev/stderr`; macOS can't name the peer of a socket that already hung up; a CLI test hard-coded a Unix socket path.
- A daemon started under a caller that captures output (`$out = termos restore`) inherited the caller's stdout pipe, so the caller waited for end of file as long as the server ran. Windows children inherit every inheritable handle; the client now clears the flag on its standard handles first.
- A ConPTY reports NUL (the e2e harness's Ctrl+Space) as Ctrl+Shift+2; it now means Ctrl+Space on Windows, as Ctrl+@ does everywhere.
- A ConPTY repaints full-width rows by letting them wrap, which `vt100` joined into one line; the e2e harness reads rows one by one.

All 13 CI jobs, including the Windows tests and both Windows/macOS test builds, then passed (run 41351452891).

**Not in this milestone:**
- bracketed paste and Kitty keyboard input from the outer terminal on Windows (`TODO(M9)`);
- a hidden window for `WM_ENDSESSION`, so logoff and shutdown always save (`TODO(M9)`);
- foreground program names, directories and command lines on macOS and Windows, so snapshots there restore programs (`TODO(M9)`; Linux reads `/proc`);
- shipping Windows Terminal's `conpty.dll` (only if the system ConPTY gives trouble);
- signed builds (M9 releases).

---

## M4a — Graphics and themes
**Status:** 🔍 done, awaiting review · Owner answers: M4 is split, graphics and themes first (D-042); animations are on by default but off over SSH and with reduced motion (D-042). Design: D-043 to D-049, ARCHITECTURE.md §5.7, §5.8, §8 and §9; user guide [graphics.md](docs/graphics.md).

**Deliverables**
- **Capability probe** (ARCHITECTURE.md §5.7, D-043):
  - [x] One query batch at attach (XTVERSION, Kitty graphics, DECRQM 2026, `CSI ? u`, cell and window pixel sizes, OSC 11), terminated by DA1; 200 ms timeout (800 ms over SSH); `TERMOS_PROBE=0` skips it; Windows consoles are detected from the environment
  - [x] A pure, fuzzed reply parser; fixtures for kitty, WezTerm, foot, iTerm2, xterm, Windows Terminal and GNOME Terminal (rebuilt from documented formats) and tmux 3.4 (recorded)
  - [x] Caps v2 in the handshake; `termos debug caps` (`--raw` prints the replies). The frontend protocol is now version 3; the JSON-RPC used by `msg` and plugins has its own version, so plugins don't break when frames change
- **Themes** (D-044):
  - [x] Dark, light and high-contrast presets; `[theme] base = "auto" | "dark" | "light" | "high-contrast"` plus per-colour overrides; existing configs stay valid
  - [x] `auto` follows the terminal background (OSC 11, then `COLORFGBG`); themes are per client; `theme.select`
  - [x] Programs in panes get real answers to OSC 10/11 colour queries (from the last active client's terminal)
- **Animations** (D-045):
  - [x] Float open/close and toasts (120 ms, ease-out), per client; off over SSH, with `[ui] reduced-motion` or `TERMOS_REDUCED_MOTION=1`, or `[ui] animations = false`
  - [x] No timer unless an animation is running for some client (D-028)
- [x] **Kitty keyboard protocol for programs in panes** (flags 1, 8, 16), with a guard against the emulator's unbounded mode stack (D-046)
- **Images** (§5.8, D-047 to D-049):
  - [x] The fallback ladder per client: Kitty → Sixel → iTerm2 → quadrant blocks, with occlusion-correct placement. Braille is not on the ladder (one colour per cell); its raster is kept for M4b's charts
  - [x] Images as compositor objects; Kitty placements cropped per visible piece from one upload; Sixel/iTerm2 only when fully visible, off the last row and still, blocks otherwise
  - [x] Image viewer app (`image.open PATH`): fit or actual size, next/previous image in the folder, decoded off the server loop
  - [x] Images inside panes: Kitty graphics and Sixel from programs, anchored to their text so they scroll and clear with it; Kitty's file and shared-memory transmissions refused; per-pane limits

**Acceptance**
- Reply-parser tests replaying fixtures from the terminals above; fuzz targets `caps_reply`, `sixel_decode`, `apc_dcs_intercept`.
- Snapshots for each theme, block and braille images, an image under a float, and a frame mid-animation.
- Proptests for rectangle subtraction and the block raster; golden bytes for each encoder and a Sixel round trip.
- Server tests: an idle server with an image sends nothing; an animation produces a handful of frames and then no deadline; an SSH client gets no animation frames; an image is uploaded once per variant.
- Idle CPU and RSS within budget with an image open (`cargo xtask perf`).

**What the tests cover**
- **Probe and caps:** replies from the eight terminals above; tmux never gets Kitty or iTerm2 even if they answer; SSH, reduced-motion and graphics overrides. The three new fuzz targets run in the CI fuzz job.
- **Snapshots:** the dark, light and high-contrast themes; half and quadrant block images; a float halfway through opening. The braille raster and the image under a float are checked cell by cell instead (`images_fall_back_to_blocks_that_floats_cover`).
- **Proptests:** rectangle subtraction (at most four pieces, inside the first rectangle, outside the second, disjoint, exact area) and the block raster (a uniform image gives uniform cells).
- **Encoders:** golden bytes for Kitty, iTerm2 and Sixel; a Sixel round trip; colour reduction to 256; decoding bounded by the pixels drawn; chunked Kitty uploads.
- **Animation:** the local client gets 5 to 10 changed frames, an SSH client sees only the final frame, and no deadline remains afterwards (`floats_animate_only_where_they_should`).
- **Server:** a Kitty client gets one upload; under the palette the image becomes crops of that same upload with matching source offsets; closing the palette brings the whole image back with no new upload; an idle server with an image sends nothing. An image printed by a program in a pane scrolls with its text and is cleared with it (Unix).
- **End to end:** the real binary, given a canned Kitty probe reply, writes Kitty upload and placement bytes to the terminal.
- **Emulator:** Kitty images anchored to the text, chunked transmissions, refused file transmissions and `a=q` queries; Sixel images, `CSI 16 t` and XTSMGRAPHICS answers; the Kitty keyboard spec table; the keyboard mode stack stays bounded.
- **Budgets** (release build on the dev container, `cargo xtask perf 10`, client and daemon together):

  | Metric | Measured | Budget |
  | --- | --- | --- |
  | Keystroke-to-screen latency, p50 | 0.7 ms | — |
  | Keystroke-to-screen latency, p99 | 2.0 ms | < 16 ms |
  | Idle CPU over 10 s | 0.1% | < 1% |
  | RSS | 6.1 MB client + 8.1 MB server = 14.1 MB | < 150 MB |
  | 300k-line flood | rendered in 0.18 s | — |
  | A 3840×2160 PNG opened in the viewer | shown in 0.36 s | — |
  | Idle CPU with that image open | 0.000% | < 1% |
  | RSS with that image open | 7.0 MB client + 60.1 MB server = 67.1 MB | < 150 MB |

  Most of the server's share is the decoded 4K picture (33 MB of RGBA) and its scaled variants.

**Demo** (live, Linux, in an isolated `tmux -L termos-m4a` with its own TermOS home):
1. `termos debug caps` inside tmux 3.4 reports Sixel as the image protocol (tmux passes Sixel on, never Kitty or iTerm2).
2. `TERMOS_GRAPHICS=blocks termos`, then `termos msg image.open '{"arg": "/tmp/m4a/pics/sunset.png"}'`: the viewer float shows the sunset in quadrant blocks, with the info line `sunset.png  160×80  fit  2/2   n/p next/prev · z zoom · q close`.
3. `Ctrl+Space` opens the palette over the image and covers its rows; `termos msg theme.select '{"arg": "high-contrast"}'` turns the status line to yellow on black.
4. `q` closes the viewer. `cat kitty.img` (a Kitty APC image, 40×40 px) draws the image as blocks in the pane; `seq 1 40` scrolls it out of view; printed again, then `clear` removes it.
5. `printf '\e[>1u'; cat -v`, then Esc, shows `^[[27u`.

The animated float and an SSH-flagged client staying still are covered by the test above rather than shown live: a tmux capture can't show motion. Pixel protocols are checked through the raw output in tests; the owner can confirm them in kitty, WezTerm or Windows Terminal.

All 13 CI jobs passed for the last code commit, including the Windows tests and the three new fuzz targets (run 41357183710).

**Not in this milestone:**
- Kitty keyboard release events and alternate keys (flags 2 and 4) for programs in panes (`TODO(M9)`);
- images printed inside panes on Windows, where ConPTY can filter the sequences out (`TODO(M9)`);
- Kitty's Unicode placeholders, and animated GIFs (the first frame is shown);
- chart widgets, drop shadows and rounded borders (M4b).
---

## M4b — Apps, termos-hw, WASM widgets
**Status:** 🔍 done, awaiting review · Owner answers: O-8 → wasmtime (D-042); sysinfo plus our own GPU probes, delete to the trash, clipboard history in memory only, WASM widgets in every build (D-050). Design: D-050 to D-052, ARCHITECTURE.md §4, §8, §9, §14, §16 and §17; user guides [apps.md](docs/apps.md) and [widgets.md](docs/widgets.md).

**Deliverables**
- **Widgets for apps** (D-052):
  - [x] A braille line chart (several series), sparklines and meters in eighth blocks, and an image slot (`termos_core::widgets`); a braille `Canvas` in termos-render; an `error` theme colour
  - [x] App timers that run only while the app is on screen; `Copy` and `PasteAndClose` effects; pictures that scroll partly out of an app
- **Polish:**
  - [x] Rounded borders and drop shadows (floats had them since M1) now also on the palette and which-key, following `[floats] border` and `shadow`
- **`termos-hw`** (D-050):
  - [x] An inventory (OS, CPU, memory, GPUs) for M7 and samples of CPU per core, memory, load, uptime, temperatures, GPUs, disks, network and processes; `termos debug hw`
  - [x] sysinfo for everything, including the GPU list on Linux, macOS and Windows; our probes add GPU temperature and power (Linux sysfs, NVIDIA through NVML loaded at run time) and SoC GPUs such as the Raspberry Pi's
- **Apps** (D-052, [apps.md](docs/apps.md)):
  - [x] File manager (`files.open`): folders first, previews of pictures, markdown, text and folders, filter, hidden files, sort, rename, new folder, copy/cut/paste, delete to the trash; `file.open` opens by kind and `file.edit` runs your editor in a new pane
  - [x] System monitor (`monitor.open`): CPU chart and cores, memory, GPUs, network, temperatures, disks, processes; reads once a second only while visible
  - [x] Markdown viewer (`markdown.open`): headings, lists and task lists, quotes, code, tables, links, local pictures inline
  - [x] Settings (`settings.open`): the common options, saved into config.toml with its comments kept, checked first, applied at once
  - [x] Clipboard manager (`clipboard.open`, prefix `=`): the history of copies (copy mode, mouse, OSC 52, apps), in memory only; paste, copy again, pin, delete, clear, filter
- **WASM widgets** (D-051, [widgets.md](docs/widgets.md)):
  - [x] The `termos:widget` WIT world: init, render, tick and click, each returning when the widget next wants to run
  - [x] wasmtime behind the `wasm-widgets` feature, on by default; one thread, started only when a widget is configured; WASI without network or environment, files only where granted, read-only; fuel, memory, time and output bounds per call
  - [x] Widgets in the status line, after plugin widgets; clicks go to them
  - [x] A sample widget (`examples/widgets/pomodoro`) kept built as a test fixture; CI rebuilds it from source and tests that build

**Acceptance**
- A snapshot test for each app.
- Idle CPU stays under 1% with the system monitor closed.
- With no WASM widget configured, RAM is unchanged.

**What the tests cover**
- **Snapshots:** each app's window (the file manager on a temporary folder, the monitor wide and narrow from a fixture machine, the markdown layout, the clipboard manager, settings after changes); the chart, a spike and a partly filled meter.
- **termos-hw:** fake sysfs trees (AMD, Intel, a Raspberry Pi with its display card, connectors, a network card that is not a GPU); merging sysfs and NVML readings by PCI slot; the real machine on every CI platform.
- **Desktop:** the monitor's timer runs only while it is visible (none when minimized or closed); copy mode and programs' OSC 52 land in the clipboard history, and prefix `=` pastes an entry back into the shell; the file manager lists folders first, previews text and markdown, asks before trashing and opens markdown in its viewer; settings write config.toml and the config reloads; markdown with a picture drawn in blocks.
- **Apps:** paste, copy, pin, delete and clear in the clipboard manager; settings keep comments, write valid files, stay in range and leave a broken file alone; copying and moving folder trees, free names for copies, previews by kind.
- **WASM widgets:** the sample draws, does not tick while ready, ticks once a second while running and hears clicks; a widget that loops forever is stopped (out of fuel); bad settings and a file that is not a component are reported; end to end, the widget appears in a client's status line and a mouse click on it starts the countdown.
- **Fuzzing:** `markdown_layout` joins the CI fuzz pass.
- **Budgets** (release build on the dev container, `cargo xtask perf 10`, client and daemon together):

  | Metric | Measured | Budget |
  | --- | --- | --- |
  | Keystroke-to-screen latency, p50 | 0.6 ms | — |
  | Keystroke-to-screen latency, p99 | 1.7 ms | < 16 ms |
  | Idle CPU over 10 s | 0.000% | < 1% |
  | RSS, no widget configured | 8.2 MB client + 10.6 MB server = 18.8 MB | < 150 MB |
  | 300k-line flood | rendered in 0.18 s | — |
  | A 3840×2160 PNG opened in the viewer | shown in 0.38 s | — |
  | Idle CPU / RSS with that image open | 0.000% / 72.4 MB | < 1% / < 150 MB |
  | CPU with the system monitor open | 0.6% (one reading a second) | — |
  | Idle CPU with the monitor closed | 0.000% | < 1% |
  | Idle CPU / RSS with a WASM widget configured | 0.000% / 8.1 + 23.7 = 31.9 MB | < 1% / < 150 MB |
  | The `termos` binary | 28.0 MB (9.98 MB without `wasm-widgets`; 8.1 MB at M4a) | — |

  **"With no WASM widget configured, RAM is unchanged": partly met.** Without a widget no engine, thread or heap exists (the server's heap and anonymous memory are the same as a build without the feature, but for the next point). The larger binary still costs memory: each process's private, non-reclaimable memory grows by 0.3 MB (relocated read-only data: 1.40 vs 1.09 MB in the server), and its RSS by about 1.7 MB of clean pages of the binary, shared and reclaimable (18.8 MB against 15.4 MB for the same commit built without the feature). The rest of the growth since M4a (14.1 → 15.4 MB) is the new apps and crates in the binary. A build without widgets is one flag away (`--no-default-features`).

**Demo** (live, Linux, in an isolated `tmux -L termos-m4b` with its own TermOS home):

1. With `[widgets.pomodoro]` in the config and `TERMOS_GRAPHICS=blocks`, `termos` shows `🍅 25:00` at the right of the status line.
2. `termos msg files.open '{"arg": "."}'`: Files lists `photos/` first, then `README.md` (124B) and `todo.txt` (19B), and previews the folder; Enter goes into `photos/`, and selecting `sunset.png` draws it in blocks on the right.
3. `termos msg monitor.open`, then `pane.snap-max`: the header (host, OS, CPU, uptime, load), the CPU chart and four cores at about 100% (a release build was compiling), memory, network sparklines, the three disks and the busiest processes, two columns wide.
4. A mouse click on the widget (an SGR mouse sequence sent to the client) starts the countdown: `🍅 24:58` in bold, a second less every second.
5. `printf '\033]52;c;…'` from the shell copies `ssh build@ci.example`; prefix `=` lists it in the clipboard history, with its source and age.

The demo found two things, both fixed with tests: Linux lists every thread as a process, so threads filled the process list (they are now left out); and a program's copy was labelled with the pane's long window title, which pushed the age off the row (it is now the program's name, and the age always shows).

**CI found** (each fixed):
- The GPU readings' helpers exist only for Linux and Windows; on macOS they were unused and failed `-D warnings`. `cargo xtask ci` now also runs clippy for macOS when that target is installed, so this shows up before a push.
- cmd.exe keeps `''` inside words, so a clipboard test that relied on sh quoting failed on Windows; it now uses the shared shell-aware helper.
- wasmtime compiles C for Windows, which the Linux lint job cannot do; cross-target clippy now runs without default features, and the native jobs check the widgets.
- On the aarch64 Linux runner, the widget test waited 60 s for exactly `🍅 0:59` and never saw it, although the countdown ran (it ended at `done!`). The cause is not understood yet. The test now accepts any value in the first ten seconds and, if it fails again, lists every drawing it saw with timestamps.

For the WASM widgets commit, 13 of the 14 CI jobs passed, including the Windows and macOS tests and the new widget-sample job (run 41375317379); the aarch64 widget test above failed and was changed in the docs commit, which then passed all 14 jobs (run 41377022077).

**Not in this milestone:**
- GPU temperature and power on macOS, and for non-NVIDIA GPUs on Windows (`TODO(M9)`);
- the WASM engine's memory on a Raspberry Pi 4 (D-042 asks for it; measured on x86_64 here, Pi measurements come with the Pi images, M8);
- saving and restoring app windows with sessions (`TODO(M9)`; app windows are not in snapshots yet);
- syntax highlighting in the markdown viewer and code previews;
- the package manager UI (M5).

---

## M5a — Languages and editors
**Status:** 🔍 done, awaiting review · Owner answers: M5 is split (D-053); O-3 → lazygit, embedded (D-053); TermOS uses the mise on `PATH` or fetches a pinned, checksummed one (D-053); the full language matrix runs nightly and on developer-environment changes (D-053). Design: D-053 to D-057, ARCHITECTURE.md §10, §14, §16 and §17; user guide [languages.md](docs/languages.md).

**Deliverables**
- **mise** (D-053):
  - [x] The `mise` on `PATH` when it is 2025.10.0 or newer (or `TERMOS_MISE`), else mise 2026.10.14 fetched from GitHub with the system's `curl` and checked against SHA-256 sums compiled into TermOS (Linux, macOS and Windows, x64 and arm64)
  - [x] TermOS panes get mise's shims at the front of `PATH`
- **Toolchains** (D-054):
  - [x] `termos lang install|use|list|remove <lang>[@version]` for Rust, C, C++, Zig, Go, Python, Java, Kotlin, C#/.NET, Node, Bun, Deno, HTML/CSS, Lua and shell; servers and debuggers installed with each language; per-project versions in `mise.toml` (or `.tool-versions`)
  - [x] Platform limits that name the tool: `termos lang install` leaves it out and says why (clangd on aarch64 Linux)
- **Editor** (D-012, D-054):
  - [x] TermOS's Neovim (0.12.6, `NVIM_APPNAME=termos-nvim`): every installed server through `vim.lsp`, started by `mise exec` with exact versions and the runtime line it was installed with; nvim-dap with nvim-dap-ui (pinned through `vim.pack`) for delve, codelldb, debugpy, js-debug and netcoredbg; `lua/user.lua` is the user's
  - [x] Helix's `languages.toml` (or `languages.termos.toml` beside the user's own)
  - [x] `termos edit` and `[dev] editor = auto | termos-nvim | helix | env`, also in Settings and the file manager
- **Panels** (D-055):
  - [x] `termos create <template> [dir]` (named so because `termos new` makes sessions): one template per language, with a `mise.toml` pinning the installed version
  - [x] Build, run and test from project detection and `[tasks]` in a project `termos.toml`: `termos task`, and `task.build|run|test` / `task NAME` panels at the bottom of the screen
  - [x] The Git UI: `git.open` (prefix `g`) opens lazygit, installed through mise, in a float
- **Acceptance test and CI** (D-056):
  - [x] `termos lang check <lang>`: install, create the template, build, run (output checked) and test it, TermOS's Neovim until every server for the main file completes LSP `initialize`, and the debugger's DAP `initialize`
  - [x] `.github/workflows/languages.yml`: every language on ubuntu-24.04 and ubuntu-24.04-arm, nightly, on developer-environment changes and on demand; ci.yml runs Rust, Python and Go on every push
- **Docs:**
  - [x] [languages.md](docs/languages.md), with which tools are unavailable on which architecture

**Acceptance:** for each language on x86_64 and aarch64 (where supported), CI installs it with mise, builds and runs a hello-world, and completes a headless `nvim` LSP initialize handshake (nightly and on developer-environment changes; a quick subset on every push).

**Acceptance results** (`termos lang check` in the language CI, `languages.yml`; each job fetches mise and checks it, installs the language, creates its template, builds, runs and tests it, then completes the LSP and DAP handshakes):

| Language | Language servers (LSP `initialize`) | Debugger (DAP `initialize`) | Linux x86_64 | Linux aarch64 |
| --- | --- | --- | --- | --- |
| Rust | rust-analyzer | codelldb | ✓ | ✓ |
| C, C++ | clangd | codelldb | ✓ | ✓, without clangd (no aarch64 build; reported as skipped) |
| Zig | zls | codelldb | ✓ | ✓ |
| Go | gopls | delve | ✓ | ✓ |
| Python | basedpyright, ruff | debugpy | ✓ | ✓ |
| Java | jdtls 1.63.0 | — | ✓ | ✓ |
| Kotlin | kotlin-language-server (on Java 21) | — | ✓ | ✓ |
| C# / .NET | csharp-ls | netcoredbg | ✓ | ✓ |
| Node, Bun | vtsls | js-debug | ✓ | ✓ |
| Deno | `deno lsp` | — | ✓ | ✓ |
| HTML, CSS | vscode-html-language-server (for the template's `index.html`) | — | ✓ | ✓ (no build or run step: a page) |
| Lua | lua-language-server | — | ✓ | ✓ |
| Shell | bash-language-server | — | ✓ | ✓ |

Rust, Python and Go also pass in ci.yml's quick subset on every push.

**What CI found on the way** (each fixed before this review):
- js-debug's releases have no per-platform asset: its spec now picks the DAP server tarball.
- mise's `http:` backend cannot resolve `latest`: jdtls is pinned to a milestone.
- jdtls 1.63.0 needs Java 25 although its launcher checks for 21: Java is now Temurin 25, and servers start with the runtime line they were installed with (D-057).
- kotlin-language-server 1.3.13 cannot run on Java 25: Kotlin keeps Temurin 21, which no longer replaces a Java default the user already has (D-057).
- A server that died before initializing kept the check waiting until its timeout: server exits now end the wait.
- A WASM widget test assumed it would see exactly `🍅 0:59` (aarch64 runners can land the first tick late).

**What the tests cover**
- **Unit:** finding mise (version floor, the fetched copy, checksums); `termos lang` against a recording mise (global runtime and helpers, servers installed, failures reported, platform limits); `mise ls --json`; the generated Neovim and Helix configs (snapshot), exact versions, filetypes merged for shared servers, and the runtime line a server was installed with; templates (pins, `mise trust`, generators, not over a project); task detection for every project kind and `termos.toml` overrides; the check's build, run and test steps, skips and failures; DAP framing (events first, refusals, closed streams, oversized frames) and a DAP handshake over a socket.
- **CLI, against a stand-in mise:** `termos lang install|list`, `termos edit` starting TermOS's Neovim, `termos create` and `termos task`, and `termos lang check` from start to finish, including a failure that keeps the project.
- **Desktop:** task panels and the Git UI run the `termos` binary in panes.
- **Not covered here:** interactive editing (completion, diagnostics, stepping through code) is not tested; the handshakes show the servers and debuggers start with TermOS's config. macOS and Windows run the unit and CLI tests but not the language matrix.

**Demo:** `termos lang install go`, `termos create go-cli demo`, then edit in Neovim with diagnostics and a breakpoint.

What was shown, and where:

1. **Live, Linux, in an isolated `tmux -L termos-m5a` with its own TermOS home** (no mise on `PATH`, nothing downloaded):
   - `termos create --list` shows the 15 templates; `termos create shell-script hello-sh` writes `main.sh`; in it, `termos task` lists `run` (`sh main.sh`) and `test` (`shellcheck main.sh`), and `termos task run` prints `Hello from hello-sh!` and `✓ run finished`.
   - On the desktop, in a Cargo project: `termos msg task.build` opens "task: build" at the bottom of the screen with real `cargo build` output and `✓ build finished`, and waits for Enter; `task.test` and `task.run` (`Hello, world`) open their own panels; `task.run` again replaces its panel (the session still has five panes).
2. **The Go part, by CI.** This sandbox refuses to run downloaded programs (mise, Neovim, lazygit), so `termos lang install go` and the editor were not run here. The `go` jobs of the language CI show the rest on both architectures: mise fetched and checked, Go, gopls and delve installed, `go-cli` created, built, run and tested, gopls initialized in TermOS's Neovim, delve answering DAP `initialize`.
3. **Not shown:** diagnostics and a breakpoint in an interactive Neovim. The handshakes above are what the acceptance asks for; for the owner to try: `termos lang install go`, `termos create go-cli demo`, `termos edit demo/main.go`, break a line, then `F9` on another and `F5`.

---

## M5b — Dev rootfs and packages
**Status:** 🔍 done, awaiting review · Owner answers: split from M5 (D-053); Debian 13 from the official image, pinned by digest; rootless with subordinate IDs; the host's Podman, hosted only (D-109). Design: D-110, ARCHITECTURE.md §10, §13, §14 and §17; user guide [packages.md](docs/packages.md).

**Deliverables**
- **Dev rootfs** (`termos devroot create|enter|update|status|stop|remove`, D-110):
  - [x] Debian 13's official layer, pinned per architecture, downloaded with `curl` and checked by size and SHA-256; `--from FILE` (and `--sha256` for another file)
  - [x] unpacked and entered in a user namespace with the user's subordinate IDs (root inside, never the host's), bubblewrap with the home and the project at their paths; apt and dpkg work; `update` is apt's upgrade; `remove` deletes it from inside
  - [x] `status` says what is missing (programs, subordinate IDs, AppArmor's or the kernel's restriction) and how to fix it
  - [x] the images: bubblewrap, util-linux's `unshare`, shadow's ID helpers, a range for `termos`; the boot test checks the dev rootfs can run
- **Podman** (hosted only, D-109): [x] `termos devroot create --podman` (the same image by digest, in a container; the way on macOS and Windows); [x] its images and containers in the package manager
- **Package manager** (D-110):
  - [x] the app (`packages.open`, **Apps → Packages**, `tos packages`): a tab per source (mise, the dev rootfs's apt, the host's apt, dnf, pacman, zypper, apk, Homebrew, winget or Scoop, Podman); lists, searches, installs, removes and upgrades, confirmed and run in a window; follows the package database
  - [x] `termos pkg list|search|install|remove|upgrade|setup|sources`
  - [x] the desktop: `devroot.enter` (**Project → Dev rootfs shell**), `devroot.create`, `pkg.run`

**Acceptance:** a dev rootfs is created, entered and updated in CI on x86_64 and aarch64 Linux; the package manager app lists and installs a mise tool and an apt package there (snapshot-tested).

**What the tests cover**
- **The dev rootfs, for real here and on CI's Linux runners** (`TERMOS_REQUIRE_DEVROOT`): the real `termos` makes one from a stand-in rootfs (the host's own `sh` and tools, a recording `apt-get`): a wrong checksum unpacks nothing; root inside, the home's files the user's outside, exit codes passed through; update, `termos pkg` listing from dpkg's database, search and install through apt inside; removal. Podman through a recording stand-in.
- **The real Debian, on CI** (`devroot` job, x86_64 and aarch64): created from the pinned layer, Debian 13 inside, a project folder outside the home bound, `hello` searched, installed, run and removed with apt and `jq` with the real mise through `termos pkg`, upgraded, deleted.
- **Unit:** subordinate-ID files, the namespace and bubblewrap arguments, the clean environment, unpacking (modes, setuid, links, hard links, device nodes left out); the parsers of dpkg, apt, mise, Podman and the eight host managers (recorded output); the pinned layers.
- **The app:** snapshots of each tab (installed, search results with what is installed marked, a question, a failure, a missing source); install, remove and upgrade send the right `termos pkg`; tabs by mouse; the list followed when its database changes. On a real desktop: the window lists mise's tools, a search and `y` run `termos pkg install mise jq` in a window.
- **The images** (`images` workflow): both build with curl 8.19.0 on mbedTLS 3.6.5 (`TERMOS-CURL`); the boot test waits for `termos devroot status` as the desktop's user and requires "bubblewrap: ready".
- **Found by CI:** apt checks signatures as its `_apt` user in `/tmp`, which bubblewrap's tmpfs made 0755, so every repository was refused as unsigned: `/tmp` is 1777 now and the stand-in test checks it. The boot test stopped reading the serial console at `TERMOS-DATA`, before the dev rootfs's line arrived on the emulated aarch64 machine: it now waits for that line.

**Demo**
1. **Here, as a user with subordinate IDs** (`unshare` 2.39, bubblewrap): the real Debian layer (49 407 895 bytes) downloaded, its SHA-256 matching the pinned one, unpacked in 2 s with owners mapped (`/etc/shadow`'s group is the user's subordinate ID for gid 42) and setuid bits kept; the stand-in rootfs created, entered (`uid=0`, `HOME=/root`, the project at its path, a file made inside owned by the user outside), updated and removed. Running Debian's own programs is CI's part: this sandbox refuses downloaded binaries.
2. **The rest, by CI** (`devroot` on both architectures): apt and mise for real, as listed above.
---

## M6 — AI agents
**Status:** 🔍 done, awaiting review · Owner answers: O-4 → our own thin clients with shared SSE parsing (D-058); O-5 → an age file with a passphrase where no keyring exists, TPM later (D-058). Design: D-058 to D-064, ARCHITECTURE.md §11, §14, §16 and §17; user guide [agents.md](docs/agents.md).

**Deliverables**
- **Providers** (D-058, D-059):
  - [x] The `Provider` trait with Anthropic (Messages API 2026-10-21), OpenAI, Moonshot Kimi, generic OpenAI-compatible and local implementations; our own thin clients over one SSE parser (fuzzed: `sse_parse`)
  - [x] Model lists from the provider APIs (context windows, output limits and listed prices); a model can be typed where a provider cannot list; model names are never hardcoded
- **Agent windows** (D-059):
  - [x] Streaming chat windows (`agent.open`, prefix `a`) with tool calling, approvals with diff previews, a live terminal for commands, cancellation (Esc), retries
  - [x] Persisted history (`agent.resume`, prefix `A`; `termos agent history`) and per-session token and cost tracking
- **Tools:** [x] files (`read_file`, `write_file`, `edit_file`, `list_dir`), [x] PTY-backed `run_command`, [x] code search (ripgrep's crates), [x] `fetch_url`
- **Safety:**
  - [x] The permission engine, profiles and the confirmation UI with diff previews (D-060)
  - [x] The bubblewrap + Landlock sandbox (D-061), also for local MCP servers and, completing D-034, launched plugins (D-064)
  - [x] The hash-chained audit log, `termos audit list|verify|show` and its viewer (`audit.open`) (D-062)
- **MCP:** [x] an `rmcp` client over stdio (sandboxed) and streamable HTTP, with a per-profile allowlist (D-063)
- **External agents:** [x] launcher presets for Claude Code, Codex CLI, Gemini CLI, aider and opencode (`agent.external NAME`), optionally sandboxed (D-063)
- **Secrets:** [x] the OS keyring or the age-encrypted secrets file, as `secrecy` types; `termos secret set|rm|list` (D-058)

**Acceptance**
- Contract tests against recorded fixtures and mock servers; no live keys in CI.
- Sandbox tests: writing outside the workspace and reaching the network are both denied.
- Audit-chain verification.
- A canary-key test: no secret appears in logs, audit records or snapshots.

**Acceptance results**
- **Contract tests:** the real Anthropic and OpenAI-compatible clients against a mock HTTP server (`crates/termos-ai/tests/providers.rs`): every page of the model list, headers (`x-api-key`, `anthropic-version`, bearer keys), request bodies (snapshots), streamed text, thinking blocks with signatures and tool calls fed in 23- and 37-byte pieces, 401 and 529 errors with the API's message and never the key, a local server without a key. The fixtures follow the providers' documented stream formats with fixture model names; they were not recorded from live APIs (no keys in this environment or in CI).
- **A whole session** against a scripted provider (`tests/agent.rs`): the secrets file's passphrase (a wrong one first), the model picked from the fetched list, `read_file` allowed by the profile, `edit_file` asked with its diff and approved, `run_command` on a PTY approved, the answer; then the edited file, the audit chain checked against the head the conversation saved, and the conversation itself.
- **Sandbox** (`crates/termos-cli/tests/agents.rs`, the real binary; required on CI's Linux runners): a write in the workspace succeeds; writes to the home folder and elsewhere outside do not reach the disk; `termos debug connect` to a listener on this machine fails without `--net` and succeeds with it. A sandboxed Python plugin still connects to the session socket while a probe plugin cannot write outside its own folder. Checked here with bubblewrap 0.9 + Landlock ABI 7, and Landlock alone.
- **Audit:** editing, removing and reordering lines, and cutting the end off, are all found (unit tests); `termos audit verify` exits non-zero on a tampered log.
- **Canary key:** a real desktop on a PTY, the key in the secrets file *and* in the server's environment; the agent runs `env` (`set` on Windows); afterwards the key is in no log, audit record, conversation, session snapshot, config or runtime file, and in no request the provider received.

**What the tests cover**
- **Unit:** the SSE parser (every split of a stream gives the same events); both providers' stream states, refusals, cut streams, bad tool input; request bodies (snapshots); costs; the secrets file (round trip, wrong passphrase, mode 0600, no plaintext); the permission engine (scopes, symbolic links out of the workspace, sensitive paths, always-ask actions, session approvals, domains, MCP); the command classifier; the sandbox's command lines and environment filter; redaction; the audit chain; the tools (reads, search, edits with stale-file checks, PTY commands with exit codes and timeouts, HTML to text); MCP against an in-memory server; external presets; history.
- **Not covered here:** live provider APIs; the OS keyring (CI has no Secret Service session; the secrets file is tested instead); MCP over streamable HTTP against a real server (the stdio path and the protocol are tested); the agent window's drawing beyond the end-to-end tests.

**Demo:** open an agent window, pick a provider and model from the fetched list, and ask it to fix a failing test, approving its edits.

What was shown, and where (live, Linux, in an isolated `tmux -L termos-m6` with its own home; the provider was a scripted OpenAI-compatible mock, since no live keys exist here):
1. `termos secret set lab` asked for the key without echo, then created the secrets file with a new passphrase.
2. `C-b a` opened the agent window, which asked for the passphrase, then listed the provider's two models (with context sizes and the listed prices); Enter picked one.
3. "The tests fail. Please fix the bug." The agent asked to run `cargo test` (approved: two failures, the output streaming into the window), read `src/lib.rs` without asking (the profile allows reads), asked to edit it with the diff `-    a - b` / `+    a + b` (approved), asked to run the tests again (approved: they passed, in bubblewrap + Landlock), and answered. The header showed tokens (with cached input) and cost.
4. `termos msg audit.open` showed the session's log, verified; `termos audit verify` printed `ok`, and the key was nowhere under the state, config or runtime folders.

---

## M7 — Local models
**Status:** 🔍 done, awaiting review · Owner answers: O-6 → `llama-server` subprocesses (D-065); O-7 → candle first, for embedding models (D-065, D-067). D-021 confirmed by the owner (D-069). Design: D-065 to D-068, ARCHITECTURE.md §12, §14, §16 and §17; user guide [models.md](docs/models.md).

**Deliverables**
- **CLI:** [x] `termos model search|info|pull|list|rm|gc|recommend`; [x] `termos models serve|status|stop|setup|sidecar`
- **Downloads** (D-066): [x] resumable (Range requests into `.incomplete` blobs), checksum-verified (SHA-256 or git blob id), commit-pinned downloads into the `huggingface_hub` cache, with GC, a size limit and the HF token (`keyring:` or `HF_TOKEN`)
- **Hardware** (D-066):
  - [x] Detection in `termos_hw::accel`: CPU features, RAM, NVML / `nvidia-smi`, AMD (sysfs, ROCm), the Vulkan loader, Metal, the Pi model, Coral, Hailo, Cloud TPU chips
  - [x] The fit estimator (weights + KV cache from the GGUF header or `config.json` + overhead → GPU, split, CPU or too big), shown before any download
- **Backends:**
  - [x] llama.cpp as `llama-server` subprocesses (D-065): the official release for this machine through mise, `auto` choosing Metal, Vulkan or CPU, `[models] llama-cpp` / `llama-server` overrides (CUDA and ROCm by hand)
  - [x] candle for embedding models, in-process (D-067); ONNX Runtime and candle chat models are not included (D-065)
  - [x] The Python sidecar: `[models.servers.<name>]` and `termos models sidecar vllm|transformers` (D-068)
  - [x] TPU, Coral and Hailo: detected, with their limits stated by `recommend` and in docs/models.md (Coral and Hailo-8 run no LLMs; TPUs go through the vLLM sidecar)
- **Router** (D-068): [x] the OpenAI-compatible `termos models serve` (token, on-demand backends, LRU eviction within a memory budget, streaming, embeddings), [x] wired into termos-ai: a `local` provider without `base-url` finds the router through its endpoint file and starts it when none runs
- **Docs:** [x] [docs/models.md](docs/models.md), with each accelerator's limits

**Acceptance**
- Resume test against a mock Hub with an interrupted transfer.
- A checksum mismatch is rejected.
- Estimator tests against known model metadata.
- End to end on CI: pull a tiny GGUF and chat with it through the router on CPU.

**Acceptance results**
- **Resume** (`crates/termos-models/tests/hub.rs`, a mock Hub over HTTP): the first transfer of a 300 KB file is cut after 100 KB; the error says to run the command again, which sends `Range: bytes=…`, completes, verifies and links the file; the cache then has `huggingface_hub`'s layout (blobs, snapshot, `refs/main`).
- **Checksum:** a Hub serving different bytes than its listed SHA-256 gets the download rejected and deleted; nothing is linked.
- **Estimator** (`fit.rs`, `gguf.rs`): Llama 3 8B's shape (32 layers, 8 KV heads of 128) at 8192 tokens gives a 1 GiB KV cache as published (half at 4096, capped at the trained context); its Q4_K_M (about 6 GiB in all) runs fully on a 24 GB GPU, split on a 4 GB one, on the CPU of a 16 GB machine without a GPU, fully on the GPU of a 16 GB Mac, and not at all with 4 GB; with 8 GB of VRAM the largest file that fits on the GPU is ranked first, split ones after; quantization labels from file names; absurd headers saturate instead of overflowing (fuzzed: `gguf_parse`).
- **Router** (`tests/router.rs`, with a stand-in llama-server): the token is required; models start on their first request with the expected arguments (and their key in the environment, not on the command line); a second model over the budget stops the first, whose process has exited before the next starts; a tiny random BERT answers `/v1/embeddings` through candle; a configured server serves its repository; `/admin/stop` stops every backend.
- **Agents:** the `local` provider reads the endpoint file, and starts a stand-in `termos models serve --daemon` when the file is stale (`provider/local.rs`).
- **End to end on CI (passed):** the `models` job pulls `ggml-org/models:tinyllamas/stories260K.gguf` and `sentence-transformers/all-MiniLM-L6-v2` from the Hub (the header read remotely by `model info`, then checksummed downloads; a second pull finds everything verified), installs llama.cpp's pinned build `b11960` through mise (D-071), and starts the router through `termos agent models local`. A request without the token gets 401; llama-server answers a chat on the CPU, plain and streamed, with its key off its command line; candle's embeddings put "A cat sits on the mat." nearer "A kitten is sitting on a rug." (cosine 0.698) than "Stock prices fell sharply today." (0.063); then stop, `rm` and `gc`. Its first run found llama.cpp's new release scheme and a broken-pipe panic, both fixed.

**What the tests cover**
- **Unit:** GGUF parsing (every value type, prefixes ask for more bytes, garbage refused); the estimator and placement; build choice and release asset names; finding `llama-server` in any layout; model references and ids; byte sizes; the endpoint file (round trip, mode 0600, only our own removed); hardware probes against fake `/sys`, `/proc` and `/dev` trees (Pi model, Coral, Hailo, TPU, Vulkan loader, `nvidia-smi` output); candle embeddings (unit length; the same text gives the same vector whatever the batch).
- **Not covered here:** real Hub downloads, llama.cpp's releases and real chat (only the CI `models` job); GPU builds of llama.cpp (no GPU runners); the vLLM and transformers sidecars beyond the configured-server path (multi-GB installs); Coral, Hailo and TPU hardware (only detection against fake trees).

**Demo:** `termos model recommend`, `termos model pull <repo>`, then chat with it in an agent window.

What was shown, and where (Linux, in an isolated `tmux -L termos-m7` with its own home; the Hub and GitHub are out of reach here, so the model was a tiny GGUF placed in the cache and `TERMOS_LLAMA_SERVER` pointed at the stand-in llama-server from the tests):
1. `termos model recommend` printed this machine (4 threads, AVX-512, 15.7 GiB) and "up to about 11.9 GiB of weights" on the CPU; `termos model list` showed `demo/tiny-chat-GGUF`.
2. `C-b a` opened an agent window on the `local` provider (`[agents.providers.local] kind = "local"`, no address). No router ran, so it started `termos models serve --daemon`, read its endpoint file and listed `demo/tiny-chat-GGUF`.
3. Picking it and sending a message started the stand-in llama-server, whose answer streamed into the window: `served demo/tiny-chat-GGUF … --ctx-size 512 --n-gpu-layers 0 --alias demo/tiny-chat-GGUF --jinja` (no key on its command line). The router's token appeared nowhere under the state, config, data or home folders.
4. `termos models stop` stopped the router and the backend; nothing was left running.

---

## M8 — Bare-metal images
**Status:** 🔍 done, awaiting review · Owner answers: O-9 → swash; O-10 → BusyBox init; O-11 → iwd + dhcpcd; O-14 → GRUB2 (D-072). Design: D-073 to D-076, ARCHITECTURE.md §5.9 and §13; user guide [bare-metal.md](docs/bare-metal.md).

**Deliverables**
- **Buildroot** (D-074): [x] the external tree in `termos-image`: the `termos` package, BusyBox init scripts, the session on tty1, GRUB configs, partition layouts, a kernel fragment; [x] targets for x86_64 (BIOS + UEFI disk image), aarch64 UEFI, Pi 5, Pi 4, and Pi 3 / Zero 2 W (best effort), each Buildroot's board defconfig plus TermOS fragments, checked setting by setting; [x] the x86_64 ISO (built and tested in M9)
- **Display** (D-073): [x] the software rasterizer (swash, the built-in DejaVu Sans Mono, a glyph cache, box drawing and blocks from geometry, adjustable and density-scaled size); [x] the DRM/KMS console with double-buffered dumb buffers, page flips and damage-only copies
- **Input and boot:** [x] libinput + XKB through libseat (layouts, repeat, VT switching, the mouse with a hardware cursor) (D-073); [x] booting straight into the console on tty1 (D-075)
- **Provisioning** (D-075): [x] the data partition layout (homes and Wi-Fi state on `termos-data`; folders for the dev rootfs, models, containers); [x] the dev rootfs itself (M5b, D-110)
- **QEMU boot tests** (D-076): [x] `cargo xtask image boot` (x86_64 OVMF and SeaBIOS, aarch64 virt) with the serial marker, a screendump check and RAM measurement; [x] the `images` CI workflow
- **Legal:** [x] `cargo xtask image build --legal-info`, kept by CI

**Acceptance**
- CI builds the x86_64 and aarch64 virt images and boots both to `TERMOS-READY`.
- Idle RAM < 150 MB, measured in QEMU.
- Pi 4 latency measured on hardware and recorded.

**Acceptance results**
- **The images, built and booted by CI (passed):** the `images` workflow's sixth run built both images from scratch (x86_64 in 38 minutes, aarch64-virt in 54, on four-core runners) and booted them in QEMU to the desktop. The time is from QEMU's start to `TERMOS-READY` on the serial console; the memory is `MemTotal − MemAvailable` 30 s (KVM) or 60 s (emulated) after boot:

  | Image | Firmware | Ready after | Screen | In use |
  | --- | --- | --- | --- | --- |
  | x86_64 (KVM) | UEFI (OVMF) | 3.5 s | 812 colours, status bar rows 780–799 | 60 MiB of 979 MiB |
  | x86_64 (KVM) | BIOS (SeaBIOS) | 3.0 s | 824 colours, status bar rows 780–799 | 59 MiB of 985 MiB |
  | aarch64-virt (emulated) | UEFI (AAVMF) | 15.0 s | 816 colours, status bar rows 780–799 | 110 MiB of 959 MiB |
- **Idle RAM < 150 MB:** 59–60 MiB on x86_64 and 110 MiB on aarch64-virt, the kernel and every service included.
- **What the first runs found, all fixed:** Bootlin's toolchains name their tools `<arch>-linux-gcc` (the build now asks Buildroot for `TARGET_CROSS`); the `libseat` crate's default C logger shim adds `-I /usr/local/include`, which Buildroot's compiler refuses (the feature is off); the PC base configuration brings ConnMan (left out; iwd and dhcpcd do its work); GRUB's BIOS first stage is not among the images (copied from its build); Buildroot links `/var/lib/dbus` to `/tmp/dbus` on read-only roots (the data partition is bound there); genimage fills file systems that name no files from its root folder (it gets an empty one, so the data partition starts empty).
- **The console in QEMU before the images:** the KMS client ran here on Ubuntu's kernel with a small initramfs: seatd gave it the seat, the screenshot showed the desktop at 1280×800, keys typed through the virtio keyboard reached a shell, and Ctrl+= made the text bigger while the desktop was resized to fit.
- **Pi 4 latency:** to be measured by the owner on hardware.

**What the tests cover**
- **Unit:** the rasterizer (metrics, glyphs, bold, italic, missing glyphs, box drawing that joins across cells, attributes, the cursor, clipping, wide characters); the console's screen (damage, the cursor, resizes), keys through real XKB keymaps (US, German), the mouse, font loading, density scaling; the boot harness (PPM, screenshot checks, PNG, serial markers, QMP against a fake QEMU), fragment checks, targets, QEMU command lines, the cargo environment for cross builds.
- **Benchmarks:** `raster/full-1080p` (about 7 ms on one x86 core) and `raster/one-row`.

**Demo:** `cargo xtask image build x86_64 && cargo xtask image boot x86_64`

---

## M9 — Release
**Status:** 🔍 done, awaiting review · Owner answers: O-12 → RAUC (D-078); O-13 → glibc and static musl builds (D-079); M9 is split, updates only on request, checksums and attestations, whole-disk installs (D-077). Design: D-078 (updates), D-080 (the live ISO and the installer), D-081 (the network page), D-082 and D-083 (accessibility, the console's latency, the Updates window, release rehearsals); user guides [bare-metal.md](docs/bare-metal.md), [releasing.md](docs/releasing.md).

**Deliverables**
- **A/B updates** (D-078): [x] RAUC in the images, the kernel in each slot, GRUB's slot selection, marking slots good or bad; [x] `termos system` behind one sudo rule; [x] `termos update` (status, check, install, restart), its Updates window and Settings row (D-083); [x] `cargo xtask image bundle`, signed with the owner's key in CI and a development key otherwise; [x] the Pi's tryboot backend (unit-tested, and the Pi 4 image builds in CI; untried on a board)
- **Install** (D-080): [x] the x86_64 live ISO; [x] `termos install`; [x] the data partition grown to the whole disk on first boot
- **Network settings page** (D-081): [x] wired links and addresses, Wi-Fi networks through iwd
- **Release pipeline** (D-077, D-079): [x] from a `v*` tag, a draft GitHub release with the hosted binaries (Linux glibc and static musl, macOS, Windows), the x86_64 disk image and ISO, the aarch64 UEFI image, the Pi images, the update bundles, `SHA256SUMS` and build attestations, with a rehearsal that runs without a tag; [x] the musl builds measured against the budgets
- **Pi tuning:** [x] the console's input→frame time measured and logged (D-019's measure, and the time until the screen shows it, D-083), and the copy into the display reduced to the rows that changed; the owner measures a Pi 4 (budget 33 ms)
- **Documentation:** [x] user docs and a getting-started tutorial that cover installing and updating
- **Accessibility:** [x] every theme's contrast checked by a test (high contrast to WCAG AAA, the others AA); the console's font sizes reviewed (6 to 96 pixels, checked in the config)

**Acceptance**
- An update from version N to N+1 succeeds (QEMU, in CI).
- A forced boot failure rolls back to the previous slot (QEMU, in CI).
- The release artifacts boot and install: the ISO installs onto an empty disk, which then boots on its own (QEMU, in CI); the hosted binaries run on matching runners.

**Acceptance results**
- **Update N → N+1 (passed, CI, images run 14, x86_64 with KVM):** the image (version 0.0.0) fetched a signed bundle of version `0.0.0-update-test` over HTTP with `termos update install --yes --restart`; slot A installed it into slot B, the machine restarted into slot B, and slot B booted the new version and was marked good.
- **Rollback after a forced boot failure (passed, same run):** slot B then installed a bundle whose root file system has no init into slot A; slot A was tried once, the kernel found no init and panicked (`panic=10`), and the next boot fell back to slot B, still marked good, on the new version.
- **Install (passed, same run):** the live ISO booted to the live desktop with SeaBIOS; with UEFI it ran `termos install --erase vda` onto an empty 8 GiB disk and turned off; the disk alone then booted with UEFI and with SeaBIOS, slot A good, its data partition grown to 6468 MiB.
- **The images' own boots (passed, same run):**

  | Image | Firmware | Ready after | In use | Data partition | Slot |
  | --- | --- | --- | --- | --- | --- |
  | x86_64 (KVM) | UEFI (OVMF) | 4.0 s | 66 MiB of 978 MiB | 1494 MiB | A, good |
  | x86_64 (KVM) | BIOS (SeaBIOS) | 3.0 s | 67 MiB of 984 MiB | 1494 MiB | A, good |
  | aarch64-virt (emulated, images run 13) | UEFI (AAVMF) | 15.0 s | 118 MiB of 959 MiB | 1494 MiB | A, good |

  The data partition starts at 512 MiB; the test disks are 1 GiB bigger, and it took the space.
- **The Pi 4 image** builds in CI with the tryboot layout (images run 12); it is not booted there.
- **The release pipeline (passed, rehearsal run 2):** the four Linux builds (glibc 2.31 and static musl, x86_64 and aarch64), macOS arm64 and x86_64 and Windows built and packaged, `SHA256SUMS` was made and the files kept as the run's artifact. What only a `v*` tag does (attestations, the draft release, images signed with the owner's keys from secrets) runs for the first release; the images job is the same build as the `images` workflow's.
- **musl against the budgets (passed, CI run 72, release builds on the same runner type):**

  | | glibc | static musl |
  | --- | --- | --- |
  | Keystroke→screen p50 / p99 | 0.29 / 0.65 ms | 0.28 / 0.53 ms |
  | Idle CPU, 30 s | 0.000% | 0.000% |
  | RSS, server + client | 24.7 MB | 20.1 MB |
  | RSS with a 4K picture open | 78.5 MB | 73.8 MB |
  | 300 000-line flood | 162 ms | 410 ms |
  | System monitor open, 30 s | 0.63% | 1.03% |

  musl keeps every budget, so it keeps its own allocator (D-079); its allocator makes the flood about 2.5× slower, well inside the test's limit.
- **What the CI runs found, all fixed:** cargo-deny refused a yanked `async-executor`; the release rehearsal's Intel macOS build stopped on a warning in the patched `gemm-common` (CI now checks that target); the fuzz smoke found a Sixel hue that overflowed and a PPM with no pixels but 340 million rows; Buildroot's PC kernel has no FAT character sets, so the x86_64 images could not mount their boot partition and marked no slot; udev drops the data partition's label link for a moment after it grows; BusyBox `df` wraps long device names; the update test raced DHCP.
- **Pi 4 latency:** to be measured by the owner on a board (the console logs it every 100 keys).

**What the tests cover**
- **Unit:** signing keys (generated and from the environment), bundle manifests and names, versions; serial markers (slot, data size, update, install); QEMU command lines with a CD and fw_cfg; the tryboot state file and its transitions; `termos update` (versions, release offers, bundle paths); `termos install` (disks, names, the target's checks); Wi-Fi names, passphrases and iwd's files; network links from sysfs; the Network page (snapshot); the console's damage bands (proptest), row copies and latency reports; every theme's contrast; font sizes in the config; PPM limits; Sixel hues; work markers with a milestone letter.
- **QEMU, in CI** (`cargo xtask image boot x86_64 --firmware all --update --install`): both firmwares, the update and the rollback, the ISO and the installed disk; aarch64-virt boots with UEFI.
- **Release:** the rehearsal builds every hosted binary on its runner; the test builds' smoke runs (`--version`, the static check for musl) on matching runners.

**Demo:** `cargo xtask image build x86_64 && cargo xtask image boot x86_64 --firmware all --update --install` (what the `images` workflow runs); on a TermOS machine, `termos update check`, then the palette's "Updates" or `termos update install`.

---

## M9b — Platform gaps
**Status:** 🔍 · Owner answers: split from M9 (D-077); an AppContainer, the ConPTY in the Windows zip, the OS's own GPU sources, one review (D-101); AppContainer grants kept until `termos sandbox reset` (D-108). Design: D-102 to D-108.

**Deliverables** (the `TODO(M9b)` markers)
- **Windows:** a sandbox for agents' commands (an AppContainer or a restricted token); VT input mode (bracketed paste, the Kitty keyboard protocol); images from programs in panes; saving at logoff and shutdown (a hidden window for `WM_ENDSESSION`).
- **macOS and Windows:** the foreground program's name, folder and command line, so snapshots restore programs; GPU temperature and power on macOS, and for non-NVIDIA GPUs on Windows.
- **Everywhere:** Kitty keyboard release events and alternate keys (flags 2 and 4); app windows saved and restored with the session.
- **The KMS console:** its own clipboard, a visual bell, images drawn as pixels.

**Progress**
- [x] Terminal input through one decoder on every platform, and VT input mode on Windows: bracketed paste, the Kitty keyboard protocol and the capability probe there; a fuzz target and an end-to-end paste and Kitty-keys test (D-102)
- [x] Kitty keyboard release events and alternate keys (flags 2 and 4): the terminal is asked for them only while the focused program wants them; desktop and end-to-end tests (D-103)
- [x] App windows saved and restored with the session: each app gives the command that reopens it; restore puts it back in the saved layout (D-104)
- [x] The KMS console's clipboard, visual bell and pixel images: the console keeps the last copy (Ctrl+Shift+V, Shift+Insert), flashes for the bell, and draws Kitty-style placements as pixels (D-105)
- [x] Foreground programs on macOS and Windows; GPU readings from the OS: sysinfo for one process on macOS, the newest program under the shell on Windows, restored commands quoted per shell; macOS GPU temperature from the system's sensors (D-106)
- [x] Windows: job objects, saving at logoff, the bundled ConPTY and images in panes: harness CLIs in a job (started suspended); a hidden window holds logoff and shutdown until the snapshots are written; Microsoft's ConPTY 1.24 pinned in the zip, with a pass-through test (D-107)
- [x] Windows: the AppContainer sandbox: workspace writable, toolchains readable, profile out of reach, no network unless asked; grants kept until `termos sandbox reset` (owner, D-108)

**Acceptance:** each gap is closed with tests on its platform's CI runner, and the open-work table has no M9b rows. ✅ No `TODO(M9b)` markers remain. CI (four platforms) covers:
- **Windows:**
  - Ctrl+Space, Escape and pastes through a ConPTY in the end-to-end tests;
  - the newest program under `cmd.exe` named, its folder found;
  - a job ending a CLI's background child;
  - the logoff window waiting for the save;
  - Sixel and Kitty pictures through the bundled ConPTY;
  - the AppContainer, required on the runner: writes, reads, the network with and without `--net`, reset.
- **macOS:** the foreground program and its folder.
- **Everywhere:**
  - the input decoder (property tests and the `input_decode` fuzz target);
  - Kitty repeats, releases and alternate keys from the terminal to `cat -v`;
  - app windows back in their places after a restore;
  - the console's pictures, bell and paste keys (unit tests on the screen and keyboard).

**Results**
- The client reads its terminal on a thread that waits until input comes or it is told to stop, so an idle terminal still costs nothing; the performance budgets passed on every push. `termos debug keys` now prints the bytes and the keys decoded from them.
- A Windows CI run found one thing the unit tests could not: a ConPTY leaves Ctrl+Space untranslated in VT input mode. That record is read as the NUL it stands for.
- `cargo install` builds on Windows still use the system's ConPTY (no pictures from programs); the zip's `conpty.dll` and `OpenConsole.exe` beside `termos.exe` turn them on, and `termos debug caps` says which is in use.
- Plugins still run without a sandbox on Windows: they need the session's named pipe (D-108).

---

## M10a — Mouse-first desktop and the `tos` command
**Status:** 🔍 done, awaiting review · Owner answers: M10 in four parts, next (D-084); one `tos` command (D-084). Design: D-085, ARCHITECTURE.md §6 and §7; user guides [getting-started.md](docs/getting-started.md#mouse) and [tos.md](docs/tos.md).

**Deliverables**
- **Menus** (D-085):
  - [x] A `◆ Menu` start button at the left of the status line (also prefix `Enter`, the `menu` command, `tos menu`) opening the TermOS menu: new terminals, the apps, AI agents (TermOS's own, Claude Code, Codex CLI, Gemini CLI, aider, opencode), project tasks and Git, layouts, themes, workspaces and sessions, help, detach and quit; submenus open on hover; each item shows its key
  - [x] Context menus on a right-click in a terminal or app (Shift+right-click in programs that use the mouse), on a floating window's title and on a workspace tab
- **Mouse polish:**
  - [x] Hover highlights for menu rows, window buttons, tabs, `◆ Menu` and `+`, redrawn only when what is under the pointer changes
  - [x] Double-click a floating title to maximize it and again to restore its size (`pane.maximize`); double-click a tab to rename it
  - [x] Clicks and the wheel in the palette; clicks in the key list (`C-b ?`); a `+` tab for a new workspace; a click on a message opens the notification center
  - [x] Panes can be named (`pane.rename`, Rename in their menus, `tos title`)
- **`tos`** (D-085, [tos.md](docs/tos.md)):
  - [x] The `termos` binary answers to `tos`; the server keeps `<data>/bin/tos` (a symbolic link on Unix, a hard link or copy on Windows, remade when the executable changes) first on every pane's `PATH`
  - [x] Verbs on the pane they run in: `split`, `run`, `float`, `zoom`, `close`, `title`, `open` (files by kind, web addresses in the browser), `files`, the apps, `ai [claude|codex|…]`, `copy` (arguments or stdin), `paste`, `notify`, `theme`, `layout`, `workspace`, `menu`, `palette`, `info`
  - [x] Every `termos` subcommand through `tos` (`tos lang install go`, `tos task test`)
  - [x] New control methods for it: `pane.command`, `pane.open`, `pane.rename`, `clipboard.copy`, `clipboard.get`
- **Docs:**
  - [x] The mouse in [getting-started.md](docs/getting-started.md#mouse), the `tos` guide, ARCHITECTURE.md §6 and §7, D-085

**Acceptance:** everything in the menus and context menus works with the mouse alone; desktop tests drive menus, hover and double-clicks through mouse events; `tos` is tested end to end against a real server on Linux, macOS and Windows CI; idle CPU stays at zero with the pointer still and moving over the same item.

**What the tests cover**
- **Menus** (unit): placement inside the screen and above the start button; hover selects and opens submenus beside (to the left at the right edge); disabled rows and separators are skipped; a press runs, a press outside closes; the keyboard (arrows, Enter, Esc, Left/Right, letters); a drawing snapshot.
- **Desktop, through mouse events only:** `◆ Menu` → Apps → System monitor opens the monitor; the monitor's title-bar menu closes it; a right-click in the shell splits it; Esc and a press elsewhere close menus; prefix `Enter` and the arrows run an item; `+` adds a workspace and a right-click on a tab selects it and offers Rename; moving over a shell or along one button does not redraw, moving onto the button does; a double-click on a floating title maximizes it and another restores its exact size; a click in the palette and in the key list runs the command.
- **Status line:** the start button, `+` and message regions, distinct hover targets, the hover style.
- **`tos`, end to end:** typed into a real pane's shell (sh, or cmd on Windows), found through the pane's `PATH`: `tos title` names the pane, `echo … | tos copy` and `tos paste > file` round-trip, `tos ls` runs the `termos` subcommand against this server, `tos split right` and `tos run --float` add panes; outside a pane it explains what it needs. The link is made and remade when the executable changes.

**Demo** (live, Linux, in an isolated `tmux -L termos-m10a` with its own TermOS home, clicks sent to the client as SGR mouse events):

1. A click on `◆ Menu` opens the TermOS menu above the status line, with key hints (`New floating terminal  C-b F`, `Detach  C-b d`).
2. Pointing at "AI agents" opens its submenu beside it: New conversation, Resume, Audit logs, then Claude Code, Codex CLI, Gemini CLI, aider and opencode.
3. Apps → System monitor (pointer, then a click) opens the monitor; a double-click on its title maximizes it to the whole screen; a right-click on the title offers Maximize or restore, the halves, Center, Minimize, Tile, Rename and Close, and Close closes it.
4. In the shell: `tos title builder` names the pane; `echo hello-from-tos | tos copy && tos paste` prints `hello-from-tos`; `tos notify built in 3s` lands in the notification center (next to "copied 15 characters"); `tos info` shows the session, server, pane `%0`, its name and folder; `tos split right -- sh -c 'echo I was started by tos; exec sh'` opens a pane on the right running it.
5. A right-click in the shell opens its menu (Paste, Select and copy text, Split right and down, Zoom, Float, Rename, Browse this folder, Close). The demo found that a menu opened near the bottom covered the status line; menus now stay inside the layout area and open upward there (with a test).

Not shown: Windows and macOS by hand; CI runs the `tos` end-to-end test on both (cmd.exe on Windows).

---

## M10b — Graphics
**Status:** 🔍 done, awaiting review · Owner answers: graphics "where it seems fitting" (D-084); Unicode icons by default, Nerd Font and plain-text sets selectable; a wallpaper by default (D-087). Design: D-088 to D-090, ARCHITECTURE.md §5.3, §5.8, §8 and §9; user guide [graphics.md](docs/graphics.md#icons-and-gradients).

**Deliverables**
- **Richer chrome** (D-088):
  - [x] The focused floating window's title bar filled with the theme's accent, with a gradient in truecolor and flat in 256 colours; unfocused windows keep a plain border
  - [x] Gradients on `◆ Menu` and the current workspace tab; every gradient moves away from its text's lightness, so contrast never drops (tested for every preset)
  - [x] `[ui] gradients` (on by default), per client: only clients whose terminal shows truecolor get them
- **Icons** (D-087, D-088):
  - [x] `[ui] icons = "unicode" | "nerd" | "ascii" | "none"`, Unicode by default; every glyph one cell wide and never an emoji (tested)
  - [x] Icons for every app (before its window title), every menu item and submenu (in a gutter, so labels line up) and files by kind in the file manager
- **Wallpaper, splash, About** (D-089):
  - [x] A wallpaper where a workspace has no tiled panes: clickable shortcuts (lit under the pointer), the logo and a hint, a dot pattern with an accent glow in truecolor; a right-click on it opens the TermOS menu
  - [x] `[ui] wallpaper = "pattern" | "none" | <picture>`: a picture decoded on its own thread and shown through the image layer, under the floats
  - [x] A splash on attach (900 ms, only where animations run, ended by any input; `[ui] splash`)
  - [x] About TermOS (Help menu, `about.open`, `tos about`)
- **Charts, progress, spinners** (D-090):
  - [x] The monitor's CPU chart as a picture where the terminal shows pixels (braille beneath, cleared there), braille elsewhere; made again only when a reading arrives
  - [x] Meters that shade from the accent through warning to error in truecolor
  - [x] A spinner in place of the icon while a window is busy (the monitor's first reading, a folder or picture loading, an agent waiting on the model), turning only while some client animates
  - [x] The command center's charts: with the command center (M10c)
- **Settings:** [x] icons, gradients, wallpaper and splash under Appearance

**Acceptance:** snapshot tests for every theme in truecolor, 256 colours and with each icon set; the budgets of ARCHITECTURE.md §15 still hold.

**What the tests cover**
- **Styled snapshots** (text plus every cell's colours): the window layer for dark, light and high contrast, each flat (256 colours) and with gradients (truecolor); the status line with each icon set, with and without gradients; menus with the Unicode, Nerd Font and ASCII sets and without icons (no gutter); the wallpaper, the splash card and the file manager with icons.
- **Icons:** every glyph of every set is one cell wide and none is an emoji-prone code point; spinners turn in one cell in every set; files get icons by kind.
- **Contrast:** every gradient's far end reads at least as well as its start, for each preset and each place gradients are drawn.
- **Desktop, end to end:** a 256-colour and a truecolor terminal on the same session get a flat and a shaded title bar; `[ui] icons = "ascii"` reaches the titles and the start button; the wallpaper shows only with nothing tiled, its shortcuts light up and open apps, and a right-click on it opens the menu; a picture wallpaper (relative to the config) is decoded off the loop and drawn, `none` draws nothing; the splash shows on attach, ends by itself with its timer, and a key ends another client's splash at once; About opens with the version; the CPU chart goes to a Kitty client in pixels with the braille beneath cleared, while a block-only client keeps the braille; a busy window spins and the spinner keeps a timer only where something animates.
- **Charts:** the rasterized chart (line, fill, guides, clamping, empty data); shaded meters change colour along the bar.

**Budgets** (`cargo xtask perf 60`, release build, Linux x86_64, this container while other builds ran):

| | M10b | Budget |
| --- | --- | --- |
| Keystroke→screen p50 / p99 | 0.39 / 1.90 ms | p99 < 16 ms |
| Idle CPU, 60 s (also with a 4K picture open, the monitor closed, a WASM widget) | 0.000% | < 1% |
| CPU with the system monitor open, 60 s | 0.50% | < 1% |
| RSS, server + client | 24.8 MB | < 150 MB |
| RSS with a 4K picture open | 79.0 MB | < 150 MB |
| 300 000-line flood | 162 ms | no regression (162 ms in M9) |

**Demo** (live, Linux, release build, in an isolated `tmux -L termos-m10b` with its own TermOS home, truecolor, clicks sent as SGR mouse events):

1. On attach, the splash: the logo on a card, gone after a moment.
2. `tos float` floats the only shell: the wallpaper appears behind it, with the shortcuts at the left and the logo and hint at the bottom right; `◆ Menu` and the tab show their gradients (`#e5a54b` to `#eec58a` on the button).
3. The pointer over Files lights it with a gradient; a click opens the file manager with icons by kind (`■` folders, `≡` text, `□` other files). A click on Monitor opens the monitor: braille CPU chart (tmux shows no pixels), and the disk bar at 95% shades from the accent (108,182,255) through orange to red (244,112,103).
4. `◆ Menu` opens the menu with an icon per item and submenu; Help → About TermOS shows the logo, version 0.0.0, linux x86_64, the config, log and data folders, the licence and the source.
5. A right-click on the wallpaper opens the TermOS menu at the pointer.
6. A second terminal without truecolor attaches to the same session: the same chrome in flat 256-colour palette colours (one colour per bar).
7. Changing `icons = "ascii"` in the config switches every icon live (`>` terminal, `/` files, `%` monitor, `*` menu).

The demo found that About cut long paths at the right, hiding the file name; long values now keep their end (`…/termos/config.toml`), with a test.

Not shown here: pixel graphics (the CPU chart as a picture, a picture wallpaper) need a terminal that shows images; the desktop tests drive them for a Kitty client. To try them by hand: kitty, WezTerm or Ghostty, then Apps → System monitor.

**Owner review (2026-11-03, Windows):**
- The owner's TermOS looked like plain PowerShell: every workspace began as one tiled terminal, so the wallpaper never showed. Sessions and workspaces now start on the desktop, with the terminal in a floating window (D-091, `[ui] start`), with a desktop test.
- Tabs read `C:\Windows\Syst…`: Windows consoles title themselves with the program's path, which is now cut to its name (`powershell`), with a test.
- Typing in PowerShell dropped and repeated letters. Under investigation: `portable-pty` creates every pseudo-console with `PSEUDOCONSOLE_WIN32_INPUT_MODE`, while TermOS sends plain characters; a Windows CI test now types into PowerShell key by key to reproduce it.

---

## M10c — The command center
**Status:** 🔍 done, awaiting review · Owner answers: observe and act, with history (D-084); the machine sampled every minute, also while closed (D-092). Design: D-093, ARCHITECTURE.md §4, §9 and §11; user guide [center.md](docs/center.md).

**Deliverables**
- **The window** (`center.open`, prefix `C`, the menu, `tos center`), mouse first, every action a button and a key:
  - [x] **Overview:** agents running and asking (with Approve and Deny), tokens, cost and tool calls over the range, the audit chains, local models, CPU, memory, network, the busiest processes, sessions and panes, charts of CPU and tokens
  - [x] **AI:** live sessions (profile, provider and model, state, tokens, cost, the pending call) with Approve, Deny, Stop and Audit; usage by model; tool calls by decision and the most used tools; every audit chain verified; local models and the router; MCP servers; CLI agents with how many windows run each
  - [x] **System:** processes (End, after asking), every session's panes (Go, Close after asking), disks, network, version and updates, the system monitor, the log's last lines (Open)
  - [x] **History:** 1 h, 24 h or 7 days of CPU and memory (a picture where the terminal shows pixels), tokens, cost, usage by model, decisions and what was done from the command center
- **History** (D-092, D-093):
  - [x] `termos_core::journal`: daily private JSON-lines files, numbers and names only (never prompts, tool arguments or secrets), kept `[center] keep-days` (7)
  - [x] Agent sessions record every turn's tokens and cost, every tool decision, their start and end
  - [x] The machine every minute, also while closed (`[center] sample = "1m" | "5m" | "off"`)
- **Actions:** [x] stop an agent, approve or deny its pending call (the live registry, through the session's own input), end a process (SIGTERM; never TermOS itself), go to or close a pane (control API, new `pane.focus`), open the related window or log
- **Settings:** [x] history kept and sampling under Command center

**Acceptance:** each view is snapshot-tested against fixtures; the history survives a restart and expires; actions are tested end to end; idle CPU stays under budget with the window closed.

**What the tests cover**
- **Journal:** records survive a new `Journal` (a restart), ranges read back, a torn last line is skipped, files are private, days past keeping are deleted when a new day starts.
- **Sampler:** one record per tick, from a fixture machine.
- **History:** samples average into buckets, gaps stay empty, tokens and cost sum by bucket and by model (unpriced models have no cost), decisions and tools are counted, older records stay out of a short range.
- **The window, against fixtures:** a snapshot of every tab; Approve, Deny and Stop through clicks and keys reach the registry; Audit and CLI agent buttons run their commands; End and Close ask, then act, and a failure says so; No cancels; Go goes; a new range reads the history again, the timer does not; the CPU history is a pixels-only picture.
- **Agents:** a whole session against a mock provider is listed in the registry, its edit approved from it; the journal holds its start, every decision, its usage (adding up to the session's cost) and its end, and no secret.
- **End to end, real binary:** the data source against a real server lists the session and panes, reads the machine and the log, brings a pane forward, closes another, ends a real process, and the history shows both actions; on Unix, the window in a real client shows the server's panes, and a click on Close and `y` close one.

**Budgets** (`cargo xtask perf 60`, release build, Linux x86_64, with the one-minute sampler running):

| | M10c | Budget |
| --- | --- | --- |
| Keystroke→screen p50 / p99 | 0.34 / 1.31 ms | p99 < 16 ms |
| Idle CPU, 60 s (also with the command center and monitor closed) | 0.000% | < 1% |
| Idle CPU with a 4K picture open, with a WASM widget | 0.017% | < 1% |
| CPU with the system monitor open, 60 s | 0.48% | < 1% |
| RSS, server + client | 26.6 MB | < 150 MB |
| RSS with a 4K picture open | 80.9 MB | < 150 MB |
| 300 000-line flood | 172 ms | no regression (162 ms in M10b) |

**Demo** (live, Linux, release build, default settings, in an isolated `tmux -L termos-m10c` with its own TermOS home):

1. TermOS starts on the desktop. In its shell, `yes > /dev/null &` starts a busy process and `tos run -- sh` opens a second pane. Three minutes later the journal holds three system samples (`{"kind":"system","cpu":25.15,…}`).
2. `C-b C` opens the command center: no agents, no tokens, audit and models empty; CPU 25%, the busiest process `yes 100%`, one session with three panes.
3. `3` (System) lists `yes` first; `e` asks "End process 10810 (yes)?", `y` ends it (the process is gone) and the footer says "ended process 10810 (yes)".
4. A click on pane `%1`'s **[Close]** asks "Close pane %1 (sh)?", `y` closes it; with nothing tiled left, the wallpaper shows behind the window again.
5. `4` (History), `[` for 1 h: the CPU and memory chart holds the minutes sampled so far, and "Done from here" lists both actions.

Not shown live: agent sessions (no provider keys here; the registry, its Approve, Deny and Stop, and the journal records are tested against a mock provider), and pictures (the terminal in the demo shows no images).

---

## M10d — Claude Code and Codex CLI
**Status:** 🔍 done, awaiting review · Owner answers: installed by TermOS, with a TermOS MCP server, not shipped in images (D-084); verified native binaries, the CLI asks for each tool call (TermOS rules and audit on top), their usage read as numbers only (D-094). Design: D-095, ARCHITECTURE.md §11, §14 and §17; user guide [agents.md](docs/agents.md#claude-code-and-codex-cli).

**Deliverables**
- **Install** (`termos agent install|update|remove|status claude|codex`, D-094, D-095):
  - [x] Claude Code from Anthropic's release site (the `stable` version, its `manifest.json`, the platform's SHA-256); Codex CLI from OpenAI's GitHub releases (the asset's SHA-256 digest; none: refused); a mismatch installs nothing
  - [x] Linux x86_64 and aarch64 (glibc and musl), macOS, Windows; into `<data>/agents`, linked in `<data>/bin` (first on every pane's `PATH`); `NAME@VERSION` pins
  - [x] From the menu: **AI agents → Claude Code / Codex CLI** offers to install when missing; **Update Claude Code and Codex**; palette `agent.install NAME`, `agent.update`
  - [x] Sign-in is theirs, in their own window on first start; Claude Code's own updater off for TermOS's copy
- **TermOS's MCP server** (`termos mcp serve`):
  - [x] `list_panes`, `read_pane` (keys masked), `send_to_pane` (never its own pane), `open_pane`, `open_file`, `run_command`, `notify`, `copy_to_clipboard`, acting as the agent's own pane; the control API gains `pane.read`
  - [x] Their CLI asks for each call; `[agents.termos-mcp]` denies tools and commands on top (quitting, detaching and closing by default); every call audited (`mcp-…` logs) and in the history
  - [x] Registered on install (`claude mcp add --scope user`; `[mcp_servers.termos]` in Codex's `config.toml`); `termos mcp register|unregister` for CLIs installed otherwise
- **The command center:** [x] their token counts, models and times from their session logs, never their text (`[center] cli-usage`, in Settings), next to TermOS's own agents; their MCP calls among the tool calls
- **Bare metal:** [x] the images ship curl with mbedTLS (D-109), which the installer runs as on every other platform; the CLIs' musl builds are the ones picked there. The QEMU boot tests check that the image's curl is the mbedTLS build (`TERMOS-CURL`: `curl 8.19.0 … libcurl/8.19.0 mbedTLS/3.6.5` on both images).

**Acceptance:** CI installs both on every platform and completes an MCP handshake with each (no live API keys: their own `mcp` commands and recorded fixtures).

**What the tests cover**
- **Installer, against recorded release data:** each vendor's platform names; Claude Code installed from its manifest and linked, then removed; a bad checksum, a release that is not JSON and a version that is not a plain name install nothing; a pinned version skips the channel; Codex from its GitHub release with its digest, refused without one, unpacked from a real archive that also holds a helper program; registration commands and Codex's `config.toml` edited and restored with the rest kept.
- **MCP server, on a recorded TermOS:** the handshake and version negotiation, notifications, unknown methods, bad JSON, batches; every tool (the agent's own pane marked and refused, pane text masked, commands denied by default never reach TermOS); denied tools hidden and refused; the audit log verifies and the history holds names and decisions, not what was typed. Fuzzed: `mcp_message`.
- **End to end, real binary:** rmcp's MCP client starts `termos mcp serve` against a real server, lists the tools, types a command into another pane and reads its output back, is refused its own pane and `session.quit`, posts a notification; `termos audit verify` passes and the history has the calls. `termos mcp register` with stand-in `claude` and `codex` programs, `termos agent status`, `unregister`.
- **Their usage:** Claude Code and Codex logs (rebuilt from their formats, with marker text that must never come out): numbers only, a message written in several lines counted once, Codex's running totals turned into turns, appended lines read once and half lines left for later; the command center's data source shows a Claude Code log's tokens next to a real server.
- **CI, for real** (`agents.yml`): both installed by `termos agent install` on Linux x86_64 and aarch64, macOS and Windows; both run; Claude Code's `mcp list` connects; Codex connects through `codex exec` against a stand-in model server that refuses at once; both connections found in TermOS's audit log (`claude-code`, `codex-mcp-client`); update, remove and unregister.

**Budgets** (`cargo xtask perf 60`, release build, Linux x86_64, with the one-minute sampler running):

| | M10d | Budget |
| --- | --- | --- |
| Keystroke→screen p50 / p99 | 0.57 / 2.15 ms | p99 < 16 ms |
| Idle CPU, 60 s | 0.017% | < 1% |
| Idle CPU with a 4K picture open, with a WASM widget | 0.017% | < 1% |
| CPU with the system monitor open, 60 s | 0.53% | < 1% |
| RSS, server + client | 26.7 MB | < 150 MB |
| RSS with a 4K picture open | 80.9 MB | < 150 MB |
| 300 000-line flood | 179 ms | no regression (172 ms in M10c; this container varies by about as much) |

None of M10d runs while idle: the MCP server is a separate process the agent's CLI starts, and their logs are read only while the command center is open.

**Found and fixed on the way**
- Reading `config.toml` (from `termos mcp serve`, `termos agent external`, `termos config check`) made the server reload it every 150 ms for as long as it ran: inotify reports opens, and the server's own reload is one. Only a real change counts now (regression test).
- On Windows, Codex's archive holds helper programs beside it; the installer now takes the program by its exact name (caught by CI).

---

## M11a — AI accounts and the usage app
**Status:** 🔍 done, awaiting review · Owner answers: usage from each account's own logs plus the limit data the CLIs record, budgets for Claude Code; accounts as profiles, switched by the user, the conversation kept (D-096). Design: D-097, ARCHITECTURE.md §4 and §11; user guide [accounts.md](docs/accounts.md).

**Deliverables**
- **Accounts:** [x] several of each CLI (Claude Code, Codex), each a profile in its own folder (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`), signed in once; `termos account list|add|use|sign-in|label|budget|statusline|remove`; the existing login is `default`; settings, instructions and skills shared from it (linked, copied where links are not allowed); TermOS's MCP server registered in each.
- **Switching:** [x] `termos account use`, ◆ Menu → AI agents → Accounts and `account.use`; `claude` and `codex` in every TermOS window follow the account in use (TermOS's shims) or one pinned for the window (`TERMOS_ACCOUNT_CLAUDE`); "Continue as …" on an agent window (`agent.swap`) restarts it under another account, its folder's newest conversation copied there and resumed. Always the user's choice.
- **The usage app** (`usage.open`, the menu, `tos usage`): [x] every account of each CLI with its plan's limits as the CLI reported them (Codex's logs; Claude Code's status line once `termos account statusline claude on`), its 5-hour window and week against a budget, its tokens today and over 7 days, its main model and a sparkline, who it is signed in as; TermOS's own agents by provider; Use, Open, Sign in, Budget, Add account and Show limits as buttons and keys. The command center counts every account too.

**Acceptance:** accounts, switching and resuming are tested end to end with stand-in CLIs and recorded session logs; CI creates a second account for each real CLI and checks that it is separate and registered; the usage app is snapshot-tested.

**What the tests cover**
- **Accounts:** add, use (kept for other processes), budgets and labels, remove (a confirmation; `default` never), a broken index is an error rather than a reset; settings and skills shared but never the login; Codex's config copied; with the real binary and stand-in CLIs: the folder, the per-account MCP registration (`claude mcp add` run with the account's folder), the sign-in in the account's folder.
- **Shims:** the account from the pin, then the one in use; the user's own folder wins; an unknown account and a missing CLI say so; with the real binary started as `claude`: the server makes the links, and each case runs the stand-in with the right folder.
- **Carrying a conversation:** Claude Code's transcript with its sub-agents and file snapshots, Codex's session found by its folder (not just the newest), copied not moved, resume arguments; with the real binary: `--account work --resume-from default` copies and resumes, and a folder with no conversation starts a new one; on the desktop: an agent window as the account in use, "Continue as" in its place, `account.use`, the Accounts menu, account tasks in windows, `agent.external claude@work`.
- **Limits:** Codex's `rate_limits` in every shape it has used (the newest reading kept), Claude Code's status line input, the status line chained after the user's own and put back; with the real binary: kept per account, the user's line still shown.
- **The usage app:** a snapshot of every account with limits and budgets; buttons and keys run the desktop commands; stale limits are not shown; 5-hour windows open on the hour and close; tokens by account, model, day and provider add up; the live source reads every account's logs.
- **CI, for real** (`agents.yml`, every platform): a second account of each CLI has its own settings with TermOS's server, the shims follow `account use` (`mcp list` connects), the status line keeps Claude Code's limits; then removed.

**Budgets** (`cargo xtask perf 60`, release build, Linux x86_64):

| | M11a | Budget |
| --- | --- | --- |
| Keystroke→screen p50 / p99 | 0.67 / 1.58 ms | p99 < 16 ms |
| Idle CPU, 60 s | 0.000% | < 1% |
| Idle CPU with a 4K picture open, with a WASM widget | 0.033% / 0.000% | < 1% |
| CPU with the system monitor open, 60 s | 0.60% | < 1% |
| RSS, server + client | 26.8 MB | < 150 MB |
| RSS with a 4K picture open | 81.1 MB | < 150 MB |
| 300 000-line flood | 125 ms | no regression (179 ms in M10d) |

Nothing M11a adds runs while idle: the shims run only when `claude` or `codex` starts, and the usage app reads the logs every 30 seconds only while it is on screen.

**Found and fixed on the way**
- Codex starts MCP servers with an emptied environment, so `termos mcp serve` under Codex could not find TermOS or its pane (M10d): its entry now forwards `TERMOS`, `TERMOS_PANE` and the XDG folders.
- Registering TermOS's server waited for the CLI without limit; a CLI that stalls now gives up after a minute.

---

## M11b — Skills for the agent CLIs
**Status:** 🔍 · Owner answers: D-096, D-098.

**Deliverables**
- [x] Skills that teach Claude Code, Codex and other CLIs what TermOS offers and how to use it: its MCP tools, `tos`, panes for servers and watchers, task panels, TermOS's own agents and models (`termos`, `termos-dev`, `termos-agents`; `crates/termos-devkit/skills/`).
- [x] Installed into every Claude Code account and the shared `~/.agents/skills` (Codex, Gemini CLI, opencode); kept up to date by TermOS (at the server's start, `termos agent install`, `termos account add`), never over a skill of the user's or a copy they changed; `termos skills install|remove|status|show`; `[agents.skills] install`.
- [x] `termos agent ask`: TermOS's agent answers once on standard output (or as JSON), read-only, audited, saved and counted, so the CLIs can hand sub-tasks to an API model or a local one; `termos agent profiles`.
- [x] `tos panes`, `tos read` and `tos run -p` for CLIs without TermOS's MCP server.

**Acceptance:** each CLI lists the skills (CI, real CLIs, no keys). ✅ `agents.yml` on Linux x86_64 and aarch64, macOS and Windows: Claude Code (in the default account and a second one) and Codex each run once against a stand-in model server, and the request each sends the model lists the three skills with their descriptions. Gemini CLI and opencode read the same folder; they are not installed in CI.

**Results**
- Unit and end-to-end tests: install, update, kept and removed copies, a skill of the user's never touched, accounts that share a folder; the server installing them at start, `termos skills remove` staying removed across a restart; `termos agent ask` against a mock provider (the answer, JSON, standard input, refusals, the questions it cannot ask); `tos run -p`, `tos panes`, `tos read` in a real pane.
- Nothing M11b adds runs while idle: the skills are checked once when the server starts (a few small file reads), and `termos agent ask` runs only when called.

**Found and fixed on the way**
- `pane.read` (behind `tos read` and the MCP server's `read_pane`) returned screen rows, so a key the pane wrapped was masked on its first row only. It now returns the lines as the program wrote them.

---

## M11c — The harness
**Status:** 🔍 · Owner answers: CLIs and API models as backends, research into tiers that the user fills with models (D-096); the model plans and the user approves, edits in turn and reads in parallel, edits plus allowed commands for headless CLIs, subscriptions first (D-099). Design: D-100.

**Deliverables**
- [x] One TermOS coding window that plans, splits the work into tasks and hands each to Claude Code or Codex (headless, under the user's subscriptions) or to an API model (TermOS's own agent), by rules in settings; every hand-off visible and audited (`termos-harness`; `harness.open`, `tos harness`, `termos harness plan|run|status`).
- [x] Settings: which models and accounts each backend uses, which task types go to which tier, budgets and fallbacks (`[harness]`, `[harness.backends.*]`, `[harness.routes]`; cross-provider review).
- [x] A researched guide to each provider's models' strengths and weaknesses, with sources, and tier defaults drawn from it ([docs/model-guide.md](docs/model-guide.md), 63 sources).

**Acceptance:** routing and hand-offs are tested against stand-in CLIs and a mock provider; CI runs a plan through real CLIs as far as is possible without keys. ✅ Unit tests (routes, plans, the CLIs' JSON lines, permissions), integration tests with stand-in Claude Code and Codex (a plan limit and the fallback, edits in turn and reads alongside, stopping, cross-review, the audit chain) and TermOS's agent against a mock provider (planning and an unattended edit, a refused command), the real binary end to end, and `agents.yml`: the real Claude Code and Codex on four platforms, against stand-in model servers (Claude Code plans and writes; Codex is refused for the review, which falls back to Claude Code).

**Results**
- Nothing runs while the harness is idle: its worker waits on a channel; the window ticks four times a second only while a plan is planned or run, for its spinner and clocks.
- The research replaced the benchmarks the brief named: SWE-bench Verified is no longer reported by the vendors and Aider's board stopped in 2025, so the guide uses Terminal-Bench 4.0 (with an independent cross-vendor run), DeepSWE, FrontierCode and CursorBench. OpenAI and Google pages were read through search summaries only; the guide marks those sources.

---

## M11d — AI accounts: sign-in, API keys, Gemini CLI
**Status:** 🔍 done, awaiting review · Owner answers: sign-in through each vendor's own flow, run by TermOS in the account's folder, no subscription credential stored by TermOS; API keys per account; Gemini CLI with accounts (D-111). Design: D-112, ARCHITECTURE.md §11; user guides [accounts.md](docs/accounts.md) and [agents.md](docs/agents.md).

**Deliverables**
- **Sign-in from TermOS:** [x] each account signs in through its vendor's own flow in its folder: Claude Code's `claude auth login` (a pasted code where there is no browser; `--console`, `--sso`), Codex's `codex login` (its device code where there is no browser, or `--device`), Gemini CLI's Google login; from `termos account sign-in`, a new account, and the usage window's **[Sign in]**.
- **Who is signed in:** [x] `termos account status`, from `claude auth status` and `codex login status` (Gemini CLI: its folder), never from login files.
- **API keys per account:** [x] `termos account key` and **[API key]**: Claude Code's and Gemini CLI's kept in TermOS's secret store and given to the CLI when it starts; Codex's handed to `codex login --with-api-key`.
- **Isolation:** [x] the shims, agent windows and the harness drop inherited sign-in variables for TermOS's accounts and keyed ones.
- **Gemini CLI:** [x] `termos agent install|update|remove gemini` (Google's bundle, digest-checked, run by Node.js from mise); accounts (`GEMINI_CLI_HOME`), the `gemini` shim, TermOS's MCP server in each account's settings, its usage in the usage app and the command center, "Continue as" carrying its conversation.

**Acceptance:** sign-in runs the vendor's command in the account's folder and keys reach only their account (stand-in CLIs that record their environment); CI asks the real CLIs who each account is (no keys) and installs the real Gemini CLI with TermOS's MCP server registered.

**What the tests cover**
- **Unit:** the CLIs' status output (Claude Code's JSON for a subscription, a key, signed out; Codex's lines, a configured key never echoed; Gemini CLI's folder); the variables each CLI signs in with; `SecretEnv` never showing a value; the shim dropping inherited credentials for an account and handing its key over, and saying whose key cannot be read; Gemini CLI's release (its digest required), its bundle unpacked and run through the launcher by a stand-in Node.js, its settings (TermOS's server, updates off, the sign-in method, comments refused); its usage (counted once when its tokens arrive, text never kept, other files under `tmp` skipped); carrying a Gemini session to an account that names the folder differently, or not at all.
- **End to end, the real binary with stand-in CLIs:** `account sign-in` runs `claude auth login` (`--console`) and `codex login --device-auth` in the account's folder with inherited `ANTHROPIC_API_KEY` and `CLAUDE_CODE_OAUTH_TOKEN` dropped; Codex's key piped to `codex login --with-api-key` and only named afterwards; Claude Code's key typed into a real terminal, unlocked into the secrets file, named in `accounts.toml` but never written there, and a sign-in refused while it is set; Gemini CLI accounts with their own homes (settings copied with TermOS's server, instructions shared, the login not), its sign-in chosen and run without a browser, its status from its folder.
- **CI, for real** (`agents.yml`, four platforms): with keys in the environment, the real CLIs report each second account as not signed in, and `default` as using the key; Claude Code through the shim in the second account is not signed in either; Gemini CLI installed by TermOS runs, has TermOS's server registered and its own updates off in a second account.

**What CI found**
- Isolation also kept the workflow's fixture key from the second account, so its skills check now runs Claude Code pointed at the account's folder, and the workflow checks the isolation instead.
- `termos account status` and `sign-in` dropped the environment's sign-in for `default` too, unlike its windows; one rule now serves both (`default` keeps the environment), and signing `default` in names the variables that come before it.
- Gemini CLI's Windows launcher now doubles a `%` in its paths (cmd expands it even inside quotes); macOS's `wc` pads counts, so the workflow counts with `grep -c`.

---

## M12a — Parallel agents in worktrees
**Status:** 🔍 done, awaiting review · Owner answers (D-113): the worktree only, no container; a block of ports per workspace; in a git repository the harness's editing tasks run in parallel worktrees and its result lands on a branch for review; pull requests through `gh` or `glab`. Design: D-114, ARCHITECTURE.md §10 and §11; user guides [worktrees.md](docs/worktrees.md) and [harness.md](docs/harness.md).

**Deliverables**
- **Workspaces:** [x] a workspace per task: a branch and a git worktree, a setup script, ignored files copied in (`.env`), a block of ports, panes for the agent, a shell and servers; archived after the merge. `termos worktree new|list|status|diff|env|setup|commit|merge|pr|archive|discard|restore|forget|open`; `worktree.new`, `worktree.open`, `worktree.run`.
- **The harness in parallel:** [x] its editing tasks each in their own worktree, merged into the plan's branch as they finish; a conflict reruns a task on top once, a second stops the plan; the result reviewed in the plan's worktree.
- **Review:** [x] a review window per workspace (`worktree.review`): the diff, comments sent to the agent as its next message (or the first of a new one), then commit, pull request, merge or discard.
- **Best-of-N:** [x] one prompt to several backends (`claude:2,codex:1`, `harness.best`, `termos harness best`), their diffs and test results side by side, one kept.

**Acceptance:** a worktree is made with its ports and setup, opens as a workspace whose panes have its ports, is reviewed with comments reaching its agent, and is merged, opened as a pull request or discarded; a plan's edits run at once in worktrees and land on its branch; best-of-N attempts are tested and one kept. Real git throughout; stand-in CLIs, `gh` and models.

**What the tests cover**
- **Unit (devkit):** worktree lists parsed; names, titles and port blocks (taken, busy, the range's end); forges and their pages; diffs with line numbers and `--name-status`/`--numstat` together; a project's `[worktree]` read alone; a worktree made, copied into (`.env`, not `node_modules`), reviewed, committed, merged into a checked-out base, archived, restored, discarded (untracked files refused for archive), restored from its ref and forgotten; merging into a base not checked out (fast-forward and `merge-tree`), and a conflict that leaves the checkout as it was. The review window's snapshot, comments on a line and a file sent as one message, its actions and buttons, and its refresh deadline. `[worktrees]` settings checked.
- **Harness:** two edits overlapping in time in their own worktrees, merged into the plan's branch, a review that sees both in the plan's worktree, the project untouched, a note for changes not committed, nothing left in the registry; two edits of one line, the second rerun on top with what conflicted; best-of-N with Claude Code twice and Codex once, each attempt's backend, diff and test result, one kept and the rest gone; specs parsed.
- **End to end, the real binary:** `termos worktree` made with its setup (the port it wrote), env, list, status, diff, commit, merge with archive, restore, discard, forget (on every platform); `pr` pushing to a bare remote and running a stand-in `gh` with the title and commits, then the forge's page with only git on `PATH` (Unix); in the desktop, `worktree.open` making a named workspace whose server and shell print the worktree's port, opened once, and its review window listing the change; review comments typed into a running stand-in agent, then starting a new one with them as its first message; `termos harness best` listing two attempts with their tests and worktrees.
- **CI, for real** (`agents.yml`, four platforms): `termos harness run` with the real Claude Code and Codex in a git repository: the plan's branch, the edit's own worktree in what Claude Code was sent, the plan's worktree listed, shown and discarded.

**What CI found**
- On macOS and Windows a path inside a worktree that did not exist yet was not found in it (resolved paths compared with unresolved ones); paths now resolve through their nearest existing folder.
- The harness passed the record from before discarding to `forget`, which refused it, so finished tasks stayed in the registry as discarded (found by the best-of-N test; the tests now check nothing is left).
- trash 5.3.0 moved its Linux backend behind a `freedesktop` feature, which the fuzz build (resolving afresh) lacked.
- A stand-in script run while another test thread's child still held it open for writing failed with "Text file busy"; tests write their scripts through a short-lived `sh`.

---

## M12b — Agent attention and resume
**Status:** 🔍 ready for review (D-111, D-115, D-116) · Asked: which agent events become notifications by default (D-115: needs-you loud, done quiet; shell integration injected; system notifications; agents resume their conversation).

**Deliverables**
- [x] "Needs you": the CLIs' hooks (registered per account like TermOS's MCP server) and programs' OSC 9/99/777 notifications mark a waiting agent: a ring on its pane, a badge on its tab, the notification center, one key to jump to the agent waiting longest.
  - Hooks: `termos agent event` in Claude Code's and Gemini CLI's `settings.json` and Codex's `hooks.json`, per account; added on install, new accounts, agent windows and `termos agent hooks`.
  - Needs you: a ring and a `● needs you` badge on the pane, `●` on its tab, a toast, a center entry, and a system notification through the outer terminal (kitty, iTerm2, WezTerm, Ghostty, Warp, foot, rxvt; `[notifications] system`). A finished turn: `✓` on its tab and a center entry only.
  - Prefix `j` (`agent.waiting`) goes to the one waiting longest; `pane.list` reports `waiting`.
- [x] Agent panes come back after a session restore: Claude Code, Codex and Gemini CLI resumed in the same account and folder.
  - The conversation the hooks reported when it is still there, else the folder's latest; an agent run in a shell is typed into the restored shell (`[session] restore-agents`).
- [x] Command blocks (OSC 133): jump between prompts, copy one command's output, failed commands marked; `read_pane` and `tos read` give agents the last command, its output and exit status.
  - bash, zsh, fish and PowerShell start with an integration script after the user's own configuration (`[shell] integration`); blocks are anchored in the text with hidden links.
  - Copy mode `[` `]` `o`, `pane.prompt-prev|next`, `pane.copy-output` (prefix `y`), `✗ status` beside failed commands, `tos read --command`, `read_pane`'s `last_command`.

**What the tests cover**
- The interceptor and parsers: OSC 9/99/777/133/633 split across reads, ConEmu's numbered OSC 9 left alone, kitty's notifications in parts and base64, VS Code's escapes, size limits; fuzz target `osc_signals`, and `vt_advance` now reads blocks.
- Blocks in the emulator: output between prompts (wrapped lines joined, output without a final newline), exit statuses only after `C`, an empty line, a running command, clearing the screen, the `✗` mark only over blank cells.
- Real shells on a real desktop (`termos-shell/tests/attention.rs`): bash 5 after a `.bashrc` whose own `PROMPT_COMMAND` reads `$?`, zsh after a `.zshrc`, fish 3 (CI installs zsh and fish and requires them on Linux); PowerShell with a native program's exit code on Windows.
- The desktop: needs-you toast, tab badge, pane badge and system notification once per wait; the jump across workspaces; typing clears it; a finished turn's tick without a toast, cleared when seen; a program's OSC 9; a hand-printed block copied with `pane.copy-output`; an agent saved in a snapshot and typed into the restored shell.
- The binary: `termos agent hooks` adds and removes the hooks in all three CLIs' files, keeping other settings, and writes through an account's linked `settings.json`; `termos agent event` outside TermOS does nothing; a hook's JSON piped to it in a pane marks the pane until answered; `tos read --command` in bash; `termos agent external --resume --conversation` continues the reported conversation, else the folder's latest.
- The real CLIs (Agent CLIs workflow, Linux x86_64 and arm64, macOS, Windows): Claude Code reports started, working, done and ended through TermOS's hooks; Codex, with its hooks trusted for the run, started, working and ended (the stand-in model refuses, so no turn finishes); `TERMOS_HOOK_LOG` records what reached TermOS.

**What CI found**
- Ubuntu's global `/etc/zsh/zshrc` runs `compinit`, which stopped at a question about insecure directories on the runners: the zsh test's `.zshenv` sets `skip_global_compinit` (and so checks that the user's `.zshenv` still loads).
- On Windows the CLIs' folders come from `USERPROFILE`: the hooks test now sets it, after writing to the runner's real profile once (and removing it again).
- macOS names a workspace after the test program, which cut the toast short: the test checks its start, and the system notification's whole text.
- Codex allows its `SessionEnd` hooks 3 s and warns about more: TermOS registers 3 s for Codex.
- An account's `settings.json` can be a link to the main one: the hooks are written through the link (found by the accounts tests locally).
- GitHub's release downloads answered 504 for a while (Codex, Gemini CLI, neovim, mise): those jobs passed when run again.
- An older accounts test read the screen between two lines of `termos account key`'s output on macOS: it waits for both.

---
---

## M12c — Remote access
**Status:** ⏳ planned (D-111) · Ask first: how a browser authenticates; whether a relay for phones is TermOS's own or none.

**Deliverables**
- A web client: sessions over HTTPS, read-only share links, a phone-sized layout.
- Remote attach with local rendering: a local client to a remote server over SSH, surviving sleep and network changes, TermOS's terminfo and `tos` set up on the remote host.
- Approvals and "done" or "needs you" alerts on the phone for agents' tool calls.

---

## M12d — Hardening
**Status:** ⏳ planned (D-111) · Ask first: the macOS sandbox's mechanism.

**Deliverables**
- GitHub Actions pinned by commit, starting with the signing workflow, kept current by Dependabot.
- seccomp under the bubblewrap sandbox; a sandbox for agents' commands on macOS; plugins sandboxed on Windows.
- A network allowlist for Claude Code, Codex and Gemini CLI panes (a local proxy, "allow once").

---

## M12e — Harness v2 (proposed)
**Status:** ⏳ proposed after reading T3 Code's harness (pingdotgg/t3code, MIT) · Ask first: whether to move from one-shot runs to long-lived sessions; Codex's `app-server` protocol and Claude Code's stream-json input are each vendor's, not a shared standard.

**Deliverables**
- Long-lived, two-way sessions instead of one run per task: Codex through `codex app-server` (threads, turns, `turn/steer`, `turn/interrupt`, approvals as requests to TermOS), Claude Code through its stream-json input and output with a permission callback; mid-task approvals and steering in the harness window.
- A capability record per backend (steering, approvals, plan mode, rollback) that routing and fallbacks read, instead of the backend's name; a mode a backend lacks runs stricter, never imitated.
- Failures classified before falling back (a plan limit with its reset time, transport errors, a broken thread): wait, retry, another account, or another backend.
- A checkpoint per task as a hidden git ref (a temporary index, `commit-tree`, `update-ref`): per-task diffs and undo without touching the branch (pairs with M12a's review window).
- A bounded hand-off when a task moves between backends: a token-capped selection of the conversation plus what changed since that backend last took part.
- Durable queued work (kept across restarts), interrupts with a time limit, and a `delegate_task` tool in TermOS's MCP server so the CLIs can hand sub-tasks to each other.
- Tests that replay recorded CLI sessions through the real adapters.

---

## M13a — Links, hints, search and finished commands
**Status:** 🚧 in progress: written, not yet built or tested (the session could not download crates) · Owner answers (D-118): the plan's: links open on the client's machine; a finished long command is loud for failures only; `u` for hints and `/` for search. Design: D-118, ARCHITECTURE.md §5.1, §6, §7 and §8; user guide [links.md](docs/links.md).

**Deliverables**
- [ ] Programs' links (OSC 8) kept in the grid, sent to the outer terminal and opened with Ctrl+click; TermOS's own anchors and unsafe schemes left out.
- [ ] Web and email addresses, paths with `:line`, git hashes and IP addresses found on screen; paths open in the editor at their line.
- [ ] Hint mode: labels on every match in view, to open, copy or paste one.
- [ ] Search across every pane's text, landing in copy mode at the match.
- [ ] A long command that finishes in a pane nobody is looking at marks its tab and says so; `pane.notify-when-done` for the next one.

---

## M13b — Layouts and workspace tools
**Status:** 🚧 in progress: written, not yet built or tested (the session could not download crates) · Owner answers (D-119): TermOS's TOML; prefix `` ` `` for the drop-down; recordings of output only. Design: D-120, ARCHITECTURE.md §4, §6 and §8; user guide [layouts.md](docs/layouts.md).

**Deliverables**
- [ ] Layout files (user and project): open a workspace with its splits, folders and commands; save the current one as a layout.
- [ ] A drop-down terminal one key away, kept running.
- [ ] Typing into every pane of a workspace, or a chosen set, at once.
- [ ] Saved commands with parameters, from the palette and `termos snippet`.
- [ ] SSH hosts from `~/.ssh/config` in the menu and palette.
- [ ] Recording a pane as asciinema, and `termos play`.

---

## M13c — Processes and files
**Status:** ⏳ proposed (D-117; plan: [docs/plans/m13-os.md](docs/plans/m13-os.md#m13c--processes-and-files)) · Ask first: the process manager in the monitor or its own window; bulk rename in the editor or a window; which archive formats.

**Deliverables**
- [ ] A process manager: filter, tree, a pane's own processes, listening ports (Linux); end, kill, pause and priority, each confirmed.
- [ ] Files: marking several entries, bookmarks and a sidebar, two panes, search by name and inside files.
- [ ] Archives browsed like folders, extracted and made.
- [ ] A git status column, bulk rename through the editor with undo, a disk-usage view, open with.

---

## M13d — The image as a computer
**Status:** ⏳ proposed (D-117; plan: [docs/plans/m13-os.md](docs/plans/m13-os.md#m13d--on-the-image-lock-power-laptops-clock-drives-disks)) · Ask first: the lock's password (the system user's or TermOS's own); a lock in hosted mode too; AMD firmware in every x86_64 image; the default idle times.

**Deliverables**
- [ ] A password set at first boot; a lock screen on a key, after idle, on suspend and lid close, optionally at start; other virtual terminals closed to it.
- [ ] A power menu: lock, suspend, restart, shut down.
- [ ] Battery in the status line with low-battery steps; brightness keys; lid and idle suspend.
- [ ] Time sync, time zones and a Clock settings page.
- [ ] USB drives shown, mounted and ejected from Files.
- [ ] GPT disks for x86_64 and aarch64-virt, partitions found by label, new ids per installed disk.
- [ ] AMD graphics firmware.

---

## M13e — The image's hardware
**Status:** ⏳ proposed (D-117; plan: [docs/plans/m13-os.md](docs/plans/m13-os.md#m13e--on-the-image-displays-bluetooth-sound)) · Ask first: several displays extended or mirrored; ALSA or PipeWire; Bluetooth audio in this milestone or input devices only.

**Deliverables**
- [ ] Every connected display driven, with hotplug, per-display mode and scale, a Displays page; extend or mirror.
- [ ] Bluetooth: power, scan, pair, connect and forget, from the Network app.
- [ ] Sound: volume and mute in the status line, media keys, a Sound page, an audible bell.

---

## M13f — Accessibility and translations
**Status:** ⏳ proposed (D-117; plan: [docs/plans/m13-os.md](docs/plans/m13-os.md#m13f--accessibility-and-translations)) · Ask first: who tests with a screen reader, on which platforms; which languages first and who reviews them; speech on the console now or later.

**Deliverables**
- [ ] A research spike with NVDA, VoiceOver and Orca, then a screen-reader mode: plain looks, the status line in words, announcements on a line readers follow.
- [ ] Speech on the console (`espeak-ng`): new output, focus, menus and notifications, review keys.
- [ ] The interface in other languages (Fluent), per client, with a pseudo-locale in tests and a key check in CI.

---

## M13g — Packages, a gallery, settings that move
**Status:** ⏳ proposed (D-117; plan: [docs/plans/m13-os.md](docs/plans/m13-os.md#m13g--packages-a-gallery-settings-that-move)) · Ask first: which channels, and who owns their accounts; where the gallery is hosted and who reviews entries; which kinds at first.

**Deliverables**
- [ ] Packages for Homebrew, winget, Scoop, Debian and Ubuntu, Fedora, Arch (AUR) and Nix, each installed and run in CI; shell completions and a man page.
- [ ] A signed gallery index of plugins, widgets, themes, layouts and saved commands; `termos gallery` and a Gallery window.
- [ ] Settings export and import, never with secrets.

---

## After 1.0 (unscheduled)
- Multi-user seats on bare metal. (The lock screen is now planned in M13d, and a screen-reader mode in M13f, D-117.)
- Typing Chinese, Japanese or Korean on the console (an input method), printing, and a GPU-drawn console ([docs/plans/m13-os.md](docs/plans/m13-os.md#not-in-this-plan)).

---

## Open work markers

Generated from the `TODO(M<n>)` markers in the code. Don't edit by hand; run `cargo xtask todos --write`.

<!-- open-work:begin (generated by `cargo xtask todos --write`) -->
_No open work markers._
<!-- open-work:end -->
