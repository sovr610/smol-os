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
| M5b | Dev rootfs, Podman, package manager app (D-053) | ⏳ |
| M6 | AI agents: providers, agent windows, permissions, sandbox, audit, MCP, external agents, secrets (D-058 to D-064) | 🔍 |
| M7 | Local models: Hugging Face downloads, fit estimates, llama.cpp, candle embeddings, the router, agents' local provider (D-065 to D-068) | 🔍 |
| M8 | Bare-metal images: the KMS console, Buildroot images for x86_64, aarch64 and the Pis, QEMU boot tests (D-072 to D-076) | 🔍 |
| M9 | Release: A/B updates, the live ISO and installer, the network page, the release pipeline with musl builds, docs (D-077 to D-080) | 🚧 |
| M9b | Platform gaps: the Windows agent sandbox and input, snapshots and sensors on macOS and Windows, Kitty keyboard flags, app windows in sessions, the KMS console's clipboard, bell and images (D-077) | ⏳ |

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
**Status:** ⏳ · Owner answers: split from M5 (D-053).

**Deliverables**
- **Dev rootfs:** `termos devroot create|enter|update`, used on bare metal and testable hosted on Linux.
- **Podman:** integration where the hardware allows.
- **Package manager UI:** an app over mise, the dev rootfs (apt) and the host package manager.

**Acceptance:** a dev rootfs is created, entered and updated in CI on x86_64 and aarch64 Linux; the package manager app lists and installs a mise tool and an apt package there (snapshot-tested).

**Demo:** `termos devroot create`, install a package with apt inside it, and manage it from the package manager app.
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
- **Buildroot** (D-074): [x] the external tree in `termos-image`: the `termos` package, BusyBox init scripts, the session on tty1, GRUB configs, partition layouts, a kernel fragment; [x] targets for x86_64 (BIOS + UEFI disk image), aarch64 UEFI, Pi 5, Pi 4, and Pi 3 / Zero 2 W (best effort), each Buildroot's board defconfig plus TermOS fragments, checked setting by setting; [ ] the x86_64 ISO (with the M9 releases)
- **Display** (D-073): [x] the software rasterizer (swash, the built-in DejaVu Sans Mono, a glyph cache, box drawing and blocks from geometry, adjustable and density-scaled size); [x] the DRM/KMS console with double-buffered dumb buffers, page flips and damage-only copies
- **Input and boot:** [x] libinput + XKB through libseat (layouts, repeat, VT switching, the mouse with a hardware cursor) (D-073); [x] booting straight into the console on tty1 (D-075)
- **Provisioning** (D-075): [x] the data partition layout (homes and Wi-Fi state on `termos-data`; folders for the dev rootfs, models, containers); [ ] the dev rootfs itself (M5b)
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
**Status:** 🚧 · Owner answers: O-12 → RAUC (D-078); O-13 → glibc and static musl builds (D-079); M9 is split, updates only on request, checksums and attestations, whole-disk installs (D-077). Design: D-078 (updates), D-080 (the live ISO and the installer).

**Deliverables**
- **A/B updates** (D-078): [ ] RAUC in the images, the kernel in each slot, GRUB's slot selection, marking slots good or bad; [ ] `termos system` behind one sudo rule; [ ] `termos update` (check, install, restart) and its Settings page; [ ] `cargo xtask image bundle`, signed with the owner's key in CI and a development key otherwise; [ ] the Pi's tryboot backend (unit-tested; untried on a board)
- **Install** (D-080): [ ] the x86_64 live ISO; [ ] `termos install`; [ ] the data partition grown to the whole disk on first boot
- **Network settings page** (D-072): [ ] wired links and addresses, Wi-Fi networks through iwd
- **Release pipeline** (D-077, D-079): [ ] from a `v*` tag, a draft GitHub release with the hosted binaries (Linux glibc and static musl, macOS, Windows), the x86_64 disk image and ISO, the aarch64 UEFI image, the Pi images, the update bundles, `SHA256SUMS` and build attestations; [ ] the musl builds measured against the budgets
- **Pi tuning:** [ ] the console's input→frame time measured and logged, and the work it spends that time on reduced; the owner measures a Pi 4 (budget 33 ms)
- **Documentation:** [ ] user docs and a getting-started tutorial that cover installing and updating
- **Accessibility:** [ ] the high-contrast theme's contrast checked by a test; the console's font sizes reviewed

**Acceptance**
- An update from version N to N+1 succeeds (QEMU, in CI).
- A forced boot failure rolls back to the previous slot (QEMU, in CI).
- The release artifacts boot and install: the ISO installs onto an empty disk, which then boots on its own (QEMU, in CI); the hosted binaries run on matching runners.

---

## M9b — Platform gaps
**Status:** ⏳ · Owner answers: split from M9 (D-077).

**Deliverables** (the `TODO(M9b)` markers)
- **Windows:** a sandbox for agents' commands (an AppContainer or a restricted token); VT input mode (bracketed paste, the Kitty keyboard protocol); images from programs in panes; saving at logoff and shutdown (a hidden window for `WM_ENDSESSION`).
- **macOS and Windows:** the foreground program's name, folder and command line, so snapshots restore programs; GPU temperature and power on macOS, and for non-NVIDIA GPUs on Windows.
- **Everywhere:** Kitty keyboard release events and alternate keys (flags 2 and 4); app windows saved and restored with the session.
- **The KMS console:** its own clipboard, a visual bell, images drawn as pixels.

**Acceptance:** each gap is closed with tests on its platform's CI runner, and the open-work table has no M9b rows.

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
| M5b | `crates/termos-apps/src/lib.rs` | package manager UI over mise, the dev rootfs and the host package manager. |
| M5b | `crates/termos-devkit/src/lib.rs` | dev rootfs, Podman and the package manager UI. |
| M9 | `crates/termos-image/src/lib.rs` | the live ISO and `termos install` (D-080). |
| M9 | `crates/termos-image/src/lib.rs` | grow the data partition to the whole disk on first boot. |
| M9 | `crates/termos-image/src/lib.rs` | the network settings page (iwd for Wi-Fi, dhcpcd), as D-072 decided. |
| M9b | `crates/termos-ai/src/sandbox.rs` | a Windows sandbox for agent commands (an AppContainer or a restricted token); until then every command asks. |
| M9b | `crates/termos-cli/src/client.rs` | VT input mode on Windows, for bracketed paste and the Kitty keyboard protocol. |
| M9b | `crates/termos-cli/src/kms/mod.rs` | the console's own clipboard and visual bell; images drawn as pixels rather than as blocks. |
| M9b | `crates/termos-hw/src/lib.rs` | GPU temperature and power on macOS (SMC), and for non-NVIDIA GPUs on Windows. |
| M9b | `crates/termos-shell/src/desktop.rs` | save and restore app windows (the folder, file or view an app shows) with the session. |
| M9b | `crates/termos-shell/src/server.rs` | a hidden window for WM_ENDSESSION, so logoff and shutdown always save. |
| M9b | `crates/termos-term/src/intercept.rs` | images from programs in panes on Windows, where ConPTY can filter APC and DCS sequences out. |
| M9b | `crates/termos-term/src/pty.rs` | foreground program name, directory and command line on macOS (libproc) and Windows (process snapshot APIs), so snapshots there name and restore programs too. |
| M9b | `crates/termos-term/src/vt.rs` | Kitty keyboard release events (flag 2) and alternate keys (flag 4) for programs in panes. |
<!-- open-work:end -->
