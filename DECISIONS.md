# TermOS decision log

Every significant technical decision lands here, including ones the project
owner made. Entries are append-only: to change a decision, add a new entry
that **supersedes** the old one and update the old entry's status line. Never
rewrite history.

**Statuses:**
- **Accepted:** in force.
- **Proposed:** our current direction, to be confirmed with the owner when its milestone starts.
- **Open:** not decided yet. The owner is asked before the milestone that needs it.
- **Superseded by D-xxx:** replaced by a later entry.

**Sources:**
- **spec:** the original project brief.
- **owner:** answered by the project owner.
- **lead:** chosen by the lead engineer and approved in the milestone plan.

---

## D-001: Linux kernel + Buildroot userland, no custom kernel, no display server
- **Status:** Accepted (spec) · 2026-11-01
- **Context:** The brief asks for an OS whose entire UI lives in the terminal and forbids writing a kernel.
- **Decision:** Bare-metal TermOS is the Linux kernel plus a minimal userland built with Buildroot. It boots directly into TermOS. There is no X11 or Wayland: graphics go to a terminal emulator (hosted mode) or straight to DRM/KMS (bare-metal mode).
- **Consequences:** We own the font rasterizer and input stack (libinput/xkbcommon) in bare-metal mode. Buildroot has no runtime package manager; D-004 deals with that.

## D-002: Rust Cargo workspace with the suggested crate layout
- **Status:** Accepted (spec + lead) · 2026-11-01
- **Context:** The brief suggests eleven crates.
- **Decision:** One workspace with `crates/termos-{core,render,wm,term,shell,apps,ai,models,devkit,image,cli}` and a std-only `xtask` (D-020).
  - `termos-image` is a real crate. It will hold the Buildroot external tree (`br2-external/`) next to the Rust code that drives image builds and QEMU boot tests, so board support and its tooling version together.
  - Two more crates are planned: `termos-plugin` (the plugin SDK and WIT definitions, M3) and `termos-hw` (hardware probing shared by the system monitor in M4 and local models in M7). Each gets its own entry here when it's created.
  - Dependency direction is fixed in ARCHITECTURE.md §4. `termos-render` is the leaf; `termos-cli` is the only binary.
- **Alternatives:** Creating crates lazily, one per milestone. Rejected because having the layout from M0 makes the architecture concrete and lets CI exercise it from day one.

## D-003: License GPL-3.0-or-later
- **Status:** Accepted (owner) · 2026-11-01
- **Decision:**
  - `LICENSE` contains the GPLv3 text.
  - Every crate declares `license = "GPL-3.0-or-later"`, and every `.rs` file starts with `// SPDX-License-Identifier: GPL-3.0-or-later` (enforced by `cargo xtask check-todos`).
  - `deny.toml` allows only GPLv3-compatible dependency licenses. Excluded: GPL-2.0-only, AGPL, OpenSSL/SSLeay and proprietary licenses.
- **Alternatives:** MIT OR Apache-2.0, Apache-2.0, proprietary.
- **Consequences:**
  - TLS goes through rustls; `openssl`/`openssl-sys` are banned.
  - Proprietary GPU libraries (NVML, CUDA) are loaded dynamically at runtime as system libraries and never linked or redistributed by us. Their redistribution inside images needs a legal check in M8.
  - Buildroot packages carry their own licenses, which Buildroot's `legal-info` collects (M8).

## D-004: Bare-metal packages, an immutable Buildroot base plus a Debian dev rootfs, with mise
- **Status:** Accepted (owner) · 2026-11-01
- **Context:**
  - Buildroot images have no runtime package manager.
  - Buildroot doesn't support building a native compiler for the target.
  - The brief wants a developer-first OS with a package manager UI.
- **Decision:**
  - **Base:** the bare-metal base is small, read-only and updated atomically (A/B, M9).
  - **Dev rootfs:** compilers, system libraries and arbitrary packages live in a Debian-based *dev rootfs* on the data partition. Dev terminals enter it via bubblewrap, or Podman where the hardware allows.
  - **Languages:** mise manages language versions, both in hosted mode and inside the dev rootfs.
  - **Package manager UI:** manages mise tools, the dev rootfs (apt) and, in hosted mode, the host's package manager.
- **Alternatives:** Nix on top of Buildroot (heavy disk and RAM on the Pi 3 and Zero 2 W). Replacing Buildroot with Alpine or Debian (contradicts D-001).
- **Consequences:** The Buildroot toolchain must be glibc (D-014). The dev rootfs adds disk usage, roughly hundreds of MB, but costs no RAM while idle.

## D-005: Rust edition 2024 with a pinned toolchain
- **Status:** Accepted (lead) · 2026-11-01
- **Decision:**
  - `rust-toolchain.toml` pins `1.97.0` with rustfmt and clippy, and `rust-version = "1.97"`.
  - We use edition 2024 and resolver 3, which is MSRV-aware, so dependency updates respect the pin.
  - Toolchain bumps are deliberate PRs that also fix any new clippy findings.
- **Alternatives:** Tracking floating `stable`. That lets new clippy lints break the `-D warnings` gate without notice.

## D-006: Async runtime is tokio
- **Status:** Accepted (lead) · 2026-11-01
- **Decision:** The server event loop, transports, HTTP clients and subprocess management run on tokio.
- **Rationale:**
  - The crates we'll depend on are tokio-based: rmcp (MCP), reqwest (providers), hf-hub.
  - tokio is the most mature runtime.
  - An idle tokio runtime with no armed timers makes no wakeups, which serves the <1% idle CPU budget.
- **Alternatives:**
  - smol: lighter, but bridging tokio-only crates costs more than it saves.
  - A hand-rolled mio loop: more code, and the ecosystem would still pull in tokio.
- **Consequences:** Blocking work (PTY reads where the API is blocking, file scans) goes to `spawn_blocking` or dedicated threads, never onto the runtime threads.

## D-007: Client/server split from M1, with a Transport abstraction
- **Status:** Accepted (lead) · 2026-11-01
- **Context:** Detach/attach (M3) needs a tmux-like server. Retrofitting that onto a monolithic renderer is a rewrite.
- **Decision:**
  - The **server** owns sessions, the window manager, PTYs, VT state, apps and composition.
  - **Frontends** only decode input, encode frames for their output device, and report their capabilities. There are three: the terminal client, the headless test client and, later, the KMS client.
  - They talk through a `Transport` trait. In M1 the transport is an in-memory channel inside one process; M3 adds a Unix-socket transport.
- **Consequences:**
  - The server composes a cell grid per attached client. Each client encodes it for its own terminal, so a client over SSH without Kitty graphics gets a fallback while a local client gets images.
  - The KMS frontend in M8 is just another client.

## D-008: VT emulation uses alacritty_terminal behind a `VtBackend` trait
- **Status:** Accepted (lead) · 2026-11-01
- **Evaluation** (versions as of 2026-11):

  | Crate | License | Strengths | Weaknesses |
  | --- | --- | --- | --- |
  | `alacritty_terminal` 0.26 | Apache-2.0 | Most complete xterm-style emulation (vte parser), scrollback, selection, regex search, a damage-tracking API, event callbacks (title, bell, OSC 52, PTY write-back). Battle-tested by Alacritty and Zed. | The API changes between minor versions. No image protocols. |
  | `vt100` 0.18 | MIT | Simple screen model. `contents_diff` and `state_formatted` are handy for reattach. | Gaps in less common sequences. Smaller user base. |
  | `termwiz` 0.23 | MIT | A good parser, surface/change model and capability/input handling. | WezTerm's full emulator (`wezterm-term`) isn't on crates.io. We'd need a git dependency, and `deny.toml` rejects those. |

- **Decision:**
  - Use `alacritty_terminal`, pinned to an exact version and wrapped in our own `VtBackend` trait, so we can switch to `vt100` without touching other crates.
  - Fuzz the input path from M1.
- **Consequences:** Images *inside* PTY windows (Kitty or Sixel output from child programs) need us to intercept APC/DCS sequences before the emulator sees them. That is a TODO for M4.

## D-009: Rendering stack is our own compositor Grid, ratatui widgets and crossterm I/O
- **Status:** Accepted (lead) · 2026-11-01
- **Decision:**
  - `termos-render` owns a `Grid` of `Cell`s holding the grapheme, width/continuation, fg/bg/underline colour, attributes and hyperlink ID. The compositor draws z-ordered windows, borders, shadows and overlays into it.
  - Output is a per-client diff against the last frame that client was sent.
  - **ratatui** is the widget toolkit for built-in apps and shell UI. Its buffers are blitted into window regions.
  - **crossterm** handles terminal I/O: raw mode, input events, Kitty keyboard flags, synchronized output.
- **Rationale:**
  - Overlapping windows, shadows, per-client diff state, hyperlinks, image occlusion, and a KMS rasterizer reading the same grid all need a surface we control.
  - ratatui brings a large, mature widget ecosystem: tables, charts, sparklines, and ratatui-image to evaluate in M4.
- **Alternatives:**
  - Using ratatui's `Buffer` as the compositor surface: no z-order or occlusion semantics, and no room for extra cell data.
  - termwiz for I/O: heavier, and centred on WezTerm.
  - termion: Unix-only and less active.

## D-010: Wire protocol is postcard frames plus JSON-RPC control, on one Unix socket per server
- **Status:** Accepted (lead) · 2026-11-01
- **Decision:**
  - **Socket:** `$XDG_RUNTIME_DIR/termos/<server>.sock`, in a 0700 directory.
  - **Peer check:** the server checks the connecting uid with `SO_PEERCRED` (Linux) or `getpeereid` (macOS) and rejects other users.
  - **Handshake:** each connection opens with a versioned handshake that declares its kind: frontend, control or plugin.
  - **Frontends:** use length-prefixed **postcard** messages (serde, compact, with a documented and stable wire format).
  - **Control clients and plugins:** speak newline-delimited **JSON-RPC 2.0**, so the `termos` CLI, shell scripts and plugins in any language can drive the desktop.
- **Alternatives:**
  - bincode, MessagePack.
  - Protobuf/Cap'n Proto: better schema evolution, but a heavier toolchain.
  - A separate socket for control: more paths to secure for no gain.

## D-011: What "restore sessions after a reboot" means
- **Status:** Accepted (lead) · 2026-11-01
- **Decision:**
  - **Restored:** sessions, workspaces, layout trees, window geometry and state. For each window, a restore recipe: kind, argv, working directory (from `/proc/<pid>/cwd` or OSC 7), an allowlisted environment and the title.
  - **Scrollback:** optional and off by default, because it can contain secrets. Stored with mode 0600.
  - **Process state:** programs are **relaunched, not resumed**. There's no CRIU-style checkpointing.
  - **Built-in apps:** restore their own state through the `App` API.
  - **Agents:** restore their conversation history, but not in-flight tool calls.
- **Consequences:** Like tmux-resurrect. Long-running jobs have to be restarted by the user, and the docs will say so plainly.

## D-012: Neovim is the default editor, Helix is selectable
- **Status:** Accepted (owner) · 2026-11-01
- **Decision:**
  - TermOS ships a managed Neovim configuration that uses the built-in LSP client (`vim.lsp.config` / `vim.lsp.enable`), nvim-dap and nvim-dap-ui, with plugins pinned by a lockfile.
  - It runs under `NVIM_APPNAME=termos-nvim`, so the user's own `~/.config/nvim` is never touched.
  - Helix gets a generated `languages.toml` and is one setting away.
- **Alternatives:** Helix as the default (its DAP support is experimental). Both first-class (twice the M5 work).

## D-013: Plugins come in two forms, out-of-process apps over JSON-RPC and WASM status-bar widgets
- **Status:** Accepted (owner) · 2026-11-01
- **Decision:**
  - **Out-of-process (M3):** a plugin is any executable with a `plugin.toml` manifest declaring the permissions it requests. It connects to the session socket as a `plugin` client. It can create windows and draw cells, receive input, register palette commands and status widgets, and post notifications. Plugins launched by TermOS can run under the same sandbox as agents (M6). A Rust SDK, `termos-plugin`, wraps the protocol.
  - **WASM widgets (M4):** small status-bar widgets compiled to WASM components against a WIT world. The runtime sits behind a cargo feature and is instantiated only when a WASM widget is configured, so it costs nothing on a Pi that doesn't use it. Fuel/epoch and memory limits apply, and there's no filesystem or network access unless granted.
  - The engine (wasmtime or wasmi) is **Open (O-8)**.
- **Consequences:** Ordinary TUI programs need no plugin at all; they run in PTY windows.

## D-014: Buildroot uses a glibc toolchain
- **Status:** Accepted (lead, follows from D-004) · 2026-11-01
- **Decision:** Bare-metal images use glibc, not musl or uClibc.
- **Rationale:** mise's prebuilt runtimes (Node, Python standalone, Go, JDKs, .NET, Bun, Deno) and most LSP/DAP binaries are built against glibc.
- **Consequences:** Hosted Linux binaries also target glibc. The floor is set to 2.31 through cargo-zigbuild (D-015); a static musl variant is Open (O-13).

## D-015: Cross-compiling uses cargo-zigbuild
- **Status:** Accepted (lead) · 2026-11-01
- **Decision:** aarch64 and x86_64 Linux builds use `cargo zigbuild --target <triple>.2.31`.
- **Rationale:** No Docker, works on Linux and macOS hosts, lets us pick the glibc version, and was verified locally in M0.
- **Alternatives:** `cross`: needs Docker and is slower in CI, but is useful for running tests under qemu-user if we ever need that.

## D-016: CI runs on GitHub Actions with native runners
- **Status:** Accepted (lead) · 2026-11-01
- **Decision:**
  - **lint:** fmt, `xtask check-todos`, cargo-deny.
  - **test:** clippy `-D warnings` plus tests on `ubuntu-24.04` (x86_64), `ubuntu-24.04-arm` (aarch64) and `macos-latest`.
  - **cross:** zigbuild for both Linux targets.
  - Native arm64 runners are free because the repository is public.
  - Later milestones add a fuzz smoke job (M1), QEMU boot tests (M8) and a release pipeline (M9).
- **Consequences:** If the repository goes private, the arm64 job may need to move to a paid runner, or to cross-builds plus qemu-user.

## D-017: Error, logging and secret-handling conventions
- **Status:** Accepted (lead) · 2026-11-01
- **Decision:**
  - **Errors:** `thiserror` error types in libraries; `anyhow` in binaries. `xtask` uses `Box<dyn Error>` to stay std-only.
  - **Logging:** `tracing`, written to a rotating file under `$XDG_STATE_HOME/termos/`. Never to stdout or stderr, because those are the user's terminal.
  - **Secrets:** wrapped in `secrecy::SecretString`, so Debug output is redacted. Config refers to secrets by name (`keyring:<name>`), never by value.
  - A test (M6) runs agents with a canary key and fails if the key appears in logs, audit files or snapshots.

## D-018: No silent stubs, enforced by tooling
- **Status:** Accepted (lead) · 2026-11-01
- **Decision:**
  - Clippy denies `todo!`, `unimplemented!` and `dbg!`.
  - Unfinished work must be a comment of the form `TODO(M<n>): <description>`, naming a milestone that has a section in ROADMAP.md. The `FIXME` tag is rejected.
  - `cargo xtask todos --write` regenerates the "Open work markers" table in ROADMAP.md, and `cargo xtask check-todos` fails CI if the table is stale or a marker is malformed.
  - Markdown is not scanned, so docs can discuss the convention freely.

## D-019: How the performance budgets are defined
- **Status:** Accepted (lead) · 2026-11-01
- **Decision:**
  - **Idle CPU < 1%:** server and client CPU time (utime + stime), summed over 60 s with one idle shell window, is under 1% of one core. Automated from M1.
  - **Input-to-frame latency:** < 16 ms on x86_64 and < 33 ms on a Pi 4, measured at p99 from reading the input bytes to flushing the frame that reflects them. An end-to-end PTY harness measures it too. x86 is automated from M1; the Pi is measured on hardware from M8.
  - **Base desktop RAM < 150 MB:**
    - Bare metal: `MemTotal − MemAvailable` after booting to the default desktop, measured in QEMU from M8.
    - Hosted: the combined RSS of server and client with the default desktop.

## D-020: Repository automation uses a std-only `xtask` crate
- **Status:** Accepted (lead) · 2026-11-01
- **Decision:** `cargo xtask ci | check-todos | todos [--write]`. It has no dependencies, so it builds in seconds and runs everywhere Cargo runs. It will grow to drive fuzzing (M1), image builds and QEMU tests (M8).
- **Alternatives:** `just` or `make`: an extra tool to install, and not portable to every contributor machine.

## D-021: Local models share the huggingface_hub cache and are served through a separate router process
- **Status:** Proposed (confirm at M7 start) · 2026-11-01
- **Decision:**
  - `termos model pull` stores files in the same cache layout as `huggingface_hub` (`$HF_HOME/hub/models--org--name/{blobs,snapshots,refs}`), so other tools reuse downloads.
  - `termos models serve` is its own process. It exposes one OpenAI-compatible endpoint (localhost or a Unix socket, token-protected) and starts and stops backend workers on demand.
  - Agents use this endpoint like any other OpenAI-compatible provider.
- **Consequences:** The server stays small and crash-isolated from inference. External tools can use the endpoint too.

## D-022: M1 and M2 are built together
- **Status:** Accepted (owner) · 2026-11-01
- **Decision:** M1 (renderer and a shell in a PTY) and M2 (window manager, palette, config) are one milestone. ROADMAP.md keeps both headings: `## M1` holds the combined scope, and `## M2` points to it.
- **Consequences:** Milestone numbers after M2 don't change.

## D-023: tmux-style interaction model
- **Status:** Accepted (owner; resolves O-1) · 2026-11-01
- **Decision:**
  - **Prefix:** `C-b`, as in tmux, configurable. Pressing the prefix twice sends it to the program.
  - **Key tables:** a prefix table with tmux's default keys wherever tmux has an equivalent (`%`, `"`, `c`, `n`, `p`, `l`, `z`, `x`, `[`, `]`, `,`, `&`, `0`–`9`, `Space`, `{`, `}`, `o`, `;`, Ctrl/Alt+arrows to resize), plus a root table (no prefix) holding `C-Space` for the palette. The prefix `:` also opens the palette.
  - **Additions:** TermOS-only keys use free slots: `f`/`F` float, `m`/`M` minimize/restore, `N` notifications, `F1` tutorial, `r` reload config, `?` all keys.
  - **Repeatable keys:** resize bindings repeat without the prefix for `repeat-time-ms` (tmux `bind -r`).
  - **Terminology:** a TermOS *workspace* is a tmux *window* and appears as a tab in the status line. A TermOS *window* or *pane* is a tmux *pane*.
  - **Status line:** tmux-style (`[session] 0:name* 1:name-`), with prompts and confirmations in the status line (`confirm-before`).
- **Alternatives:** Modifier chords (Alt/Super). These collide with shells and editors, and many terminals don't pass Super through.
- **Consequences:**
  - There are no vim-style `hjkl` focus keys by default, because tmux's `l` is "last window". Users can add them in `[keys.prefix]`.
  - `d` (detach) is reserved for M3.

## D-024: Tiled-first layout with a float toggle
- **Status:** Accepted (owner) · 2026-11-01
- **Decision:**
  - Panes tile as in tmux, with one-cell shared separators, splits and presets. The presets are even-horizontal, even-vertical, main-vertical (master-stack), main-horizontal and tiled (grid). BSP comes as an auto-split along the longer side.
  - `prefix f` turns any pane into a floating window, and back. Floating windows have a rounded border, title, minimize/close buttons and a drop shadow. They can be dragged to move, dragged by an edge to resize, snapped to halves or maximized when dropped on a screen edge, and minimized to the status line.
  - Zoom (`prefix z`) maximizes any pane.
  - Splits and presets refuse when panes would have no room ("no space for new pane").

## D-025: PTY layer is portable-pty, with reader and writer threads per pane
- **Status:** Accepted (lead; resolves O-2) · 2026-11-01
- **Decision:**
  - `portable-pty` (from WezTerm) spawns and resizes children on Linux and macOS.
  - Each pane has a blocking reader thread. It sends output chunks over a bounded channel to the server task, which owns every VT and parses on its own loop.
  - Each pane also has a writer thread, so a child that stops reading input can't block the desktop.
- **Alternatives:**
  - alacritty_terminal's own `EventLoop`: we would lose the read loop that M4 needs to intercept image sequences.
  - `pty-process`.
  - `rustix` openpty with `AsyncFd`: more unsafe code (fork/exec) for little gain.
- **Consequences:** Two sleeping threads per pane, which cost nothing while idle.

## D-026: The server sends diffs, the client encodes them, and only output floods are coalesced
- **Status:** Accepted (lead) · 2026-11-01
- **Decision:**
  - **Server side:** the server composes a full grid per client and keeps the last grid that client received. It sends `FrameUpdate { spans, cursor, clear }` containing only the changed cell runs.
  - **Client side:** the client encodes the spans with our own SGR and cursor encoder (minimal moves, DEC 2026 synchronized output, colour downsampling). crossterm is used only for terminal modes and input.
  - **Scheduling:** a frame is drawn as soon as something changes, which keeps typing latency low. While a pane is flooding output (≥ 4 KB since the last frame), frames are capped at one per 8 ms.
- **Consequences:** The M3 socket transport carries the same messages unchanged. Measured on the M1 build: p50 keystroke-to-screen latency is about 0.5 ms, and a 300k-line flood renders in about 0.25 s.

## D-027: Child environment and nesting
- **Status:** Accepted (lead) · 2026-11-01
- **Decision:**
  - Panes get `TERM=xterm-256color`, `COLORTERM=truecolor`, `TERMOS=<server pid>` and `TERMOS_PANE=%N`.
  - `termos` refuses to start inside a TermOS pane unless `--force` is given, as tmux does.
  - New panes start in the focused pane's working directory: from OSC 7 when the shell reports it, otherwise from `/proc/<pid>/cwd` on Linux.
  - OSC 52 *reads* from programs are refused (an exfiltration risk); OSC 52 copies are passed through to the outer terminal.

## D-028: The only periodic wakeups
- **Status:** Accepted (lead) · 2026-11-01
- **Decision:**
  - An idle desktop has no timers, with one exception: the optional status-line clock wakes once per wall-clock minute.
  - Which-key, toast-expiry, key-repeat and program sync-update timers are armed only while those things are active.
- **Consequences:** Idle CPU measured over 10 s on the M1 build is 0.000%.

---

## Open questions (the owner will be asked before the milestone that needs each one)

| ID | Question | Needed by | Current lean |
| --- | --- | --- | --- |
| O-1 | Default WM keybinding scheme: a tmux-style leader key, or modifier chords (Alt/Super)? | M2 | **Resolved by D-023:** tmux prefix `C-b`. |
| O-2 | PTY layer: `portable-pty`, `pty-process` or direct `rustix` openpty with tokio `AsyncFd`? | M1 | **Resolved by D-025:** `portable-pty`. |
| O-3 | Git UI: embed lazygit/gitui in a window, or build our own panel? | M5 | Embed first, integrate later. |
| O-4 | LLM clients: provider SDK crates (async-openai, others) or our own thin reqwest clients behind `Provider`? | M6 | Own thin clients plus shared SSE parsing, so the model-listing and tool-calling shapes stay uniform. |
| O-5 | Secret store on bare metal, where there's no Secret Service: an age-encrypted file unlocked by passphrase, a TPM-sealed key, or something else? | M6 | age file with a passphrase; TPM later. |
| O-6 | llama.cpp integration: a managed `llama-server` subprocess or FFI (`llama-cpp-2`)? | M7 | Subprocess: crash isolation, a ready-made OpenAI API, easy CUDA/ROCm/Vulkan build swaps. |
| O-7 | Rust-native inference: ONNX Runtime (`ort`), candle, or both? | M7 | candle first (pure Rust, no C++ runtime). |
| O-8 | WASM engine for widgets: wasmtime (component model, JIT, larger) or wasmi (interpreter, small)? | M4 | wasmtime if component-model support decides it; measure RAM on a Pi 4. |
| O-9 | KMS font rasterizer: swash or fontdue? | M8 | swash (shaping and hinting quality). |
| O-10 | Init system: BusyBox init, s6, or systemd? | M8 | BusyBox init or s6, for size. |
| O-11 | Network management: connman, NetworkManager, or iwd + a small DHCP client? | M8 | iwd + dhcpcd, with a TermOS settings UI. |
| O-12 | A/B updates: RAUC or SWUpdate? | M9 | RAUC (Buildroot package, Pi tryboot support). |
| O-13 | Hosted Linux binaries: glibc 2.31 floor only, or also static musl? | M9 | Both if musl doesn't hurt performance. |
| O-14 | Bootloader for generic aarch64 UEFI: GRUB2 or systemd-boot? | M8 | GRUB2, shared with x86_64. |
