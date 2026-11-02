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
- **Status:** Accepted (lead), refined by D-031 and D-036 · 2026-11-01
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
- **Status:** Accepted (lead), refined by D-033 · 2026-11-01
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
- **Status:** Accepted (owner); the out-of-process part is refined by D-034 · 2026-11-01
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
- **Status:** Accepted (lead), refined by D-041 · 2026-11-01
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
- **Status:** Accepted (lead); the `TERMOS` value is superseded by D-029 · 2026-11-01
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

## D-029: `termos` attaches to or creates "main"; tmux-style session commands
- **Status:** Accepted (owner: default command; lead: the rest) · 2026-11-01
- **Decision:**
  - **`termos`** with no subcommand attaches to the session `main`, creating it (and the server) if needed, like `tmux new -A -s main`.
  - **Subcommands:**
    - `new [-s NAME] [-d]`: the session starts in the client's directory;
    - `attach [-t NAME]`, default: the most recently used session;
    - `detach [-s NAME]`, `ls`, `kill-session [-t NAME]`, `kill-server`;
    - `msg METHOD [JSON]`, `restore [NAMES]`;
    - the internal `server`.
    - tmux aliases: `a`, `attach-session`, `new-session`, `list-sessions`, `detach-client`.
  - **Server selection:** `-L NAME` picks a server by name and `-S PATH` by socket path.
  - **Session names:** 1 to 64 characters, with no `/`, `:` or control characters. Names starting with `$` are reserved for ids (`$1`). Unnamed sessions get the first free number.
  - **Lifetime:** the server stops when its last session ends, as tmux does.
  - **Pane environment:** panes get `TERMOS=<socket>,<server pid>,$<session id>`; this **supersedes the value in D-027**. Commands run inside a pane use it to find their server and session: `termos detach`, `termos kill-session` and `termos msg` (which adds `"session"` to its parameters) act on the pane's own session. The nesting guard applies only to commands that attach a terminal.
  - **Keys:** `prefix d` detaches, `prefix (` / `prefix )` go to the previous/next session, `prefix $` renames the session. `session.switch NAME`, `session.new` and `session.restore` are palette commands.
- **Alternatives:** keeping a single-process `termos` and adding the daemon behind a flag. Rejected: detach is the point of M3.

## D-030: The server daemon's lifecycle
- **Status:** Accepted (lead), refined by D-037 for Windows · 2026-11-01
- **Decision:**
  - **Startup:** a client that finds no server spawns `termos server -S <socket>` with null stdio and waits up to 10 s for the socket. If two clients race, the loser's server sees a live socket and exits; the client then uses the winner.
  - **Detaching from the terminal:** the server calls `rustix::process::setsid()` itself (safe Rust, no `pre_exec`), so it has no controlling terminal and survives the terminal that started it.
  - **Signals:** SIGTERM, SIGINT and SIGHUP are handled with `tokio::signal`. The server writes every session's snapshot, tells clients why it stopped, removes the socket and exits. Interactive shells ignore SIGTERM, so on a reboot the server saves before its panes go away.
  - **Logs:** `server.log.<date>` and `termos.log.<date>` (clients) in the state log directory.
  - **Plugins** start with the first session (D-034).

## D-031: Socket, handshake and framing (refines D-010)
- **Status:** Accepted (lead), refined by D-036 for Windows and D-043 (protocol versions) · 2026-11-01
- **Decision:**
  - **Socket:** `$XDG_RUNTIME_DIR/termos/<name>.sock`, falling back to `$TMPDIR/termos-<uid>/<name>.sock`.
    - The directory is created with mode 0700; one owned by another user is refused, and wider permissions are tightened.
    - The socket itself is chmod 0600.
    - A stale socket (nothing listening) is replaced; a live one makes a second server refuse to start.
  - **Peer check:** `UnixStream::peer_cred()` (SO_PEERCRED / getpeereid). Any uid but the server's is answered with JSON-RPC error `-32003` and closed.
  - **Handshake:** every connection's first line is a JSON-RPC request `hello {version, kind: frontend|control|plugin, client, session?, create?, token?, cwd?}`.
    - Errors: a different `version` gets `-32001` with a message telling the user to restart the server; a missing session gets `-32004`.
    - The reply is `{version, server, pid, session}`.
  - **Frontends:** after the reply they switch to frames: a little-endian u32 length plus a postcard body, at most 16 MiB. The first frame is the client's `Hello {caps, cols, rows}`. The server can send `Detached {session}` as well as `Exit {reason}`.
  - **Control and plugin connections:** stay on newline-delimited JSON-RPC 2.0, with lines capped at 4 MiB.
  - **Errors:** `-32700/-32600/-32601/-32602/-32603` standard; `-32001` version mismatch, `-32002` hello required, `-32003` forbidden, `-32004` not found.
  - **Control API (v1):**
    - server: `server.info`, `server.kill`, `config.reload`;
    - sessions: `session.list/new/kill/rename/restore/saved`;
    - clients: `client.list/detach`;
    - panes: `pane.list/send-text/send-keys/kill`;
    - other: `notify.send`, `command.run`, `plugin.list`.
    - Every named desktop command, and every plugin command, also works directly as a method (`termos msg pane.split-right`).
- **Alternatives:** a binary handshake (unreadable to scripts); a separate control socket (more to secure).

## D-032: One server, many sessions, many clients
- **Status:** Accepted (lead) · 2026-11-01
- **Decision:**
  - **Sessions:** the server holds many sessions, one `Desktop` each. Pane ids are allocated server-wide, so PTY events route by pane id.
  - **What is per-client, what is per-session:**
    - per-client (`ClientUi`): terminal size and capabilities, prefix/key-table state, which-key, palette, prompts, mouse drags and status-line hit regions;
    - per-session: workspaces, panes, copy mode, the paste buffer and notifications.
  - **Session size:** `[session] window-size = latest | smallest | largest` (default latest). A client whose terminal is smaller than the session sees the top-left part; frames are cropped server-side, and a wide character cut in half becomes a blank.
  - **Backpressure:** a client gets its next frame only after its connection task has written the previous one. A slow link (SSH) gets fewer, coalesced frames instead of a growing queue; the next frame is a diff against what it last received.
  - **Ending a session:** its clients move to the most recently used other session, or get `Exit` if there is none.
- **Consequences:** Several terminals can show the same session; each has its own overlays and prompts.

## D-033: Snapshots and restore (refines D-011)
- **Status:** Accepted (owner: restore scope and default policy; lead: format and timing) · 2026-11-01
- **Decision:**
  - **File:** JSON at `$XDG_STATE_HOME/termos/sessions/<server>/<session>.json`.
    - Mode 0600 in a 0700 directory, written atomically (temporary file, fsync, rename).
    - Session names are percent-encoded in file names.
    - Versioned (`version: 1`); damaged or future files are skipped with a warning.
  - **Contents:** the wm `Session` (workspaces, layout trees, floats, focus, names). Per pane: working directory, the foreground program's name and title, and the text of the scrollback if `[session] save-scrollback = true` (default false).
    - The foreground program's **command line is stored only if the program is on the allowlist**, so a password typed on a command line never reaches the disk.
  - **When it's written:** after structural changes (splits, closes, renames, a new program in front, `cd` reported by OSC 7), debounced by `snapshot-delay-ms` (2000), and at shutdown. No timer runs while nothing changes.
  - **When it's deleted:** when the session ends on purpose (its last pane exits, `kill-session`, `session.quit`). `kill-server` and crashes keep it.
  - **Restore (owner):** each pane gets a shell in its saved directory, or the default one if the directory is gone. Programs on `[session] restore-programs` (default `vim nvim less man htop top`) are typed back into that shell, so quitting them leaves the user at a prompt. Other programs are only named in a notification. Built-in app windows are not restored. Layouts are repaired if damaged, and pane ids are renumbered.
  - **Policy (owner):** `[session] restore = "ask"` (default), `"always"` or `"never"`.
    - **ask:** the first client of a fresh server is asked in the status line, "Restore session main saved at 14:02? (y/n)". "y" replaces the fresh session with its saved state and restores the others beside it. "n" keeps the snapshots for `termos restore` / `session.restore`.
    - Until the question is answered, the asking session's own snapshot is not overwritten.
    - Restoring on demand next to a running session of the same name gives it a new name (`main-2`).
- **Alternatives:**
  - CRIU-style checkpointing: out of scope (D-011).
  - Periodic snapshots: they cost wakeups (D-028).

## D-034: Plugins v1 (refines the out-of-process part of D-013)
- **Status:** Accepted (lead), refined by D-040 (token source, SDK transport) · 2026-11-01
- **Decision:**
  - **Install and consent:** a plugin is a directory `~/.config/termos/plugins/<name>/` with a `plugin.toml`:
    - `name` (must equal the directory name: lowercase, digits, `-`, `_`), `version`, `description`;
    - `exec = [...]`: a program containing `/` is relative to the plugin directory; otherwise it's found in `PATH`;
    - `[permissions] windows, status, commands, notify`, all false by default.
    - It runs **only** while listed in `[plugins] enabled`; listing it is the user's consent.
  - **Launch:** the server starts plugins with its first session (so they have somewhere to open windows), and on config reload.
    - Each gets `TERMOS_SOCKET`, `TERMOS_PLUGIN_NAME` and a fresh `TERMOS_PLUGIN_TOKEN`: 128 bits from `/dev/urandom`, compared in constant time, never logged or shown in Debug output.
    - stdout goes to `/dev/null`; stderr goes to the server log.
    - A token admits one connection at a time.
  - **Protocol (JSON-RPC over the session socket):**
    - plugin → server: `window.open/draw/close`, `status.set`, `command.register`, `notify.send`;
    - server → plugin notifications: `window.key/mouse/resize/closed`, `command.invoked`.
    - Text is a list of lines; a line is a string or styled spans `{text, fg, bg, bold, dim, italic, underline, reverse}`, with colours written as in the config.
    - Each request is checked against the manifest. A refusal (`-32003`) names the permission to add.
  - **UI:**
    - Plugin windows are floating in-process `App`s whose content the plugin draws remotely.
    - Status widgets sit before the clock (at most 40 columns each).
    - Commands are named `<plugin>.<name>`, cannot shadow built-ins, and appear in the palette and as control methods.
    - Limits per plugin: 16 windows, 1000 lines of 1000 columns, 64 commands.
  - **Failure:** a plugin that disconnects or exits loses its windows, widget and commands, and the user is notified. There's no automatic restart in v1; a config reload starts it again.
  - **SDK:** a new leaf crate, `termos-plugin` (std, serde and serde_json only), holds the protocol types and a blocking SDK, with `examples/clock.rs`. Other languages speak JSON-RPC directly; the test suite includes a plain Python plugin.
  - **Deferred:** running launched plugins under the agent sandbox comes with M6; the WASM widgets of D-013 are M4.
- **Alternatives:** an async SDK (it would pull in tokio); auto-restart with backoff (to be revisited with real plugins).

## D-035: Native Windows is a Tier 2 hosted platform
- **Status:** Accepted (owner) · 2026-11-01
- **Decision:**
  - **Owner:** TermOS runs natively on Windows (Windows Terminal, or any console with VT support, on Windows 10 1809 or newer), so it can be tested on Linux and Windows alike. This replaces the "not supported, use WSL2" line in ARCHITECTURE.md §1 and §18. WSL2 stays supported as Linux.
  - **Owner:** it ships as its own milestone, M3.5, before M4a, with CI test builds on every push (D-041).
  - **Tier 2** means: built, linted and tested on every push; the daemon smoke-tested; known gaps tracked as `TODO(M9)` markers instead of blocking. Bare metal stays Linux-only.
  - **Scope:** everything in the hosted desktop: the server daemon, sessions, detach and attach, the control API, snapshots and restore, plugins, PTY panes.
  - **Known gaps** (Windows only, `TODO(M9)`):
    - no bracketed paste and no Kitty keyboard input from the outer terminal (the client reads console input events, not VT input);
    - logoff and shutdown save on a best-effort basis (`WM_ENDSESSION` needs a hidden window);
    - the foreground program of a pane is not known, so snapshots name no programs and restore re-runs none;
    - ConPTY consumes image escape sequences, so images inside panes (M4a) won't work there.
- **Rationale:** the owner tests on both systems; the architecture was already split at the transport and the PTY layer, so the port is mostly behind two seams (D-036, D-039).
- **Alternatives:** WSL2 only (no native experience); a Windows client attaching to a Linux server (keeps the server Unix-only, but needs a remote transport we don't have yet).

## D-036: Local IPC is a `termos_core::local` seam; Windows uses secured named pipes (refines D-010, D-031)
- **Status:** Accepted (lead) · 2026-11-01
- **Decision:**
  - **Seam:** `termos_core::local` holds `LocalListener::{bind, accept}`, `LocalStream::{connect, peer, into_split}`, `LocalRead`/`LocalWrite`, `PeerId` (a uid on Unix, a SID string on Windows), and `probe`/`exists`/`remove_endpoint`. The transport, the server and the CLI use only these. Unix behaviour is unchanged (D-031).
  - **Windows endpoint:** `\\.\pipe\termos-<16 hex digits>-<server name>`. The hex digits are FNV-1a-64 of the lower-cased runtime directory (by default `%LOCALAPPDATA%\termos\run`). The hash is written out by hand so it never changes between Rust versions. Setting `XDG_RUNTIME_DIR` gives an isolated set of servers, as on Unix (tests rely on it). `-S` must name a pipe (`\\.\pipe\…`).
  - **Security (each pipe instance):**
    - a protected DACL, `D:P(A;;GA;;;<our SID>)`: only our user, nothing inherited;
    - remote clients are rejected;
    - the first instance is created with `first_pipe_instance`, so a second server on the same name gets "already running" instead of sharing it;
    - the server checks the SID of each client's process and answers any other user with `-32003`, as on Unix;
    - the client checks the SID of the server's process **before sending `hello`**, so a process that registered our pipe name first ("pipe squatting") never sees a token or a keystroke;
    - clients connect with `SECURITY_ANONYMOUS`, so the server can never act as the client.
  - **Accepting:** four instances wait for clients at once, each in its own task, and a new one replaces each instance that connects, so clients arriving together (plugins starting with a session) all find one. Our clients also retry `ERROR_PIPE_BUSY` for up to 2 seconds. A waiting instance is a pending overlapped connect, so nothing wakes while idle.
  - **Unsafe code:** the Win32 calls (SDDL conversion, process and token SIDs, pipe creation with security attributes) sit in one `#[allow(unsafe_code)]` module with `// SAFETY:` comments, using `windows-sys` (already in `Cargo.lock`).
- **Alternatives:**
  - TCP on localhost with a token: reachable by every local user and by any program that finds the port; needs a token file on disk.
  - AF_UNIX on Windows: supported by Windows since 2018, but not by tokio or the Rust standard library on Windows, and it has no peer credentials.
  - The `interprocess` crate: another dependency for what tokio already provides, and it still leaves the security attributes to us.

## D-037: The daemon on Windows (refines D-030)
- **Status:** Accepted (lead) · 2026-11-01
- **Decision:**
  - **Starting:** the client spawns `termos server` with `CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB`, retrying without the last flag when the job forbids breaking away. So no console window pops up, Ctrl+C in the starting terminal never reaches the server, and closing that terminal (or an SSH session's job) doesn't take the server with it. The server's hidden console is inherited by its plugins.
  - **Stopping:** `tokio::signal::windows` Ctrl+C, Ctrl+Break, close, logoff and shutdown events do what SIGTERM does on Unix: save every snapshot, tell clients, exit. Logoff and shutdown are best-effort until a hidden window handles `WM_ENDSESSION` (`TODO(M9)`).
  - **Startup failures** (on every platform) are written to the server log, and the client names the log directory, since the daemon's stderr goes nowhere.
- **Alternatives:** a Windows service (needs installation and admin rights, and runs in another session than the user's console).

## D-038: Paths and the default shell on Windows
- **Status:** Accepted (lead) · 2026-11-01
- **Decision:**
  - **Directories:** config `%APPDATA%\termos`, state (logs, snapshots) `%LOCALAPPDATA%\termos`, runtime `%LOCALAPPDATA%\termos\run` (only hashed into pipe names, D-036). `XDG_CONFIG_HOME`, `XDG_STATE_HOME` and `XDG_RUNTIME_DIR` override them on every platform. The directories rely on the profile's ACLs; Unix keeps its 0700/0600 modes.
  - **Snapshots:** a rename over an existing snapshot is retried briefly (antivirus and indexers hold files open). File names that are reserved device names on Windows (`CON`, `NUL`, `COM1`…) are encoded like other unsafe characters. Program names drop `.exe` and accept `\`.
  - **Default shell:** config `shell`, else `pwsh.exe` on `PATH`, else `powershell.exe`, else `%COMSPEC%`, else `cmd.exe`. `$SHELL` is ignored on Windows: Git Bash sets it to an MSYS path that a native program can't run.
  - **Client:** refuses to start without VT processing (old consoles) with a clear error; assumes truecolor when `TERM` is unset; a key reported as Ctrl+Alt with a printable symbol (`@`, `{`, `€`) is AltGr typing, not a chord.
- **Alternatives:** the `dirs` crate (one more dependency for three environment variables).

## D-039: PTY panes on Windows use the system ConPTY
- **Status:** Accepted (lead) · 2026-11-01
- **Decision:**
  - portable-pty's ConPTY backend (D-025) runs Windows panes. It needs Windows 10 1809 or newer.
  - **End of output:** ConPTY keeps the output pipe open after the child exits. A waiter thread per pane waits for the child, sends its exit code, and closes the pseudo-console, which ends the reader thread's stream. Unix still learns the exit at EOF.
  - **Foreground program:** not available (there's no process-group leader); `foreground_pid` falls back to the shell's pid, and the program name stays Linux-only (`TODO(M9)`).
  - Windows Terminal's newer `conpty.dll` is not shipped. If ConPTY bugs on older builds turn up, shipping it is the fix to consider.
- **Alternatives:** WinPTY (unmaintained); our own ConPTY wrapper (portable-pty already is one).

## D-040: Plugin tokens from `getrandom`; the SDK speaks named pipes (refines D-034)
- **Status:** Accepted (lead) · 2026-11-01
- **Decision:**
  - **Tokens:** 128 bits from the `getrandom` crate (the OS generator on every platform: `getrandom(2)`, `SecRandom`, `ProcessPrng`) instead of reading `/dev/urandom`. Comparison, single use and redaction are unchanged.
  - **SDK on Windows:** the pipe is opened for overlapped I/O behind a private `Conn` trait, so a read can wait with a timeout and be cancelled. A synchronous pipe handle serializes its I/O, so a thread blocked in a read would block every write. The client side uses `SECURITY_ANONYMOUS` as in D-036. The SDK stays blocking and tokio-free; on Unix it is unchanged.
  - **Other languages:** a plugin opens the path in `TERMOS_SOCKET` as a file when it starts with `\\.\pipe\` (Python: `open(path, "r+b", buffering=0)`), and as a Unix socket otherwise.
- **Alternatives:** keeping `/dev/urandom` with a Windows branch (two code paths for what one crate does); a reader thread in the SDK (deadlocks on a synchronous pipe).

## D-041: CI tests Windows and publishes test builds on every push (refines D-016)
- **Status:** Accepted (owner: test builds on every push for all four platforms; lead: the layout) · 2026-11-01
- **Decision:**
  - **Test matrix:** Linux x86_64, Linux aarch64, macOS arm64 and **Windows x86_64** (`windows-latest`): clippy, the whole test suite, and a **daemon smoke test** (`.github/scripts/daemon-smoke.sh`: `new -d`, `ls`, `msg pane.list`, `msg notify.send`, `msg server.info`, `kill-server`) in an isolated home. The script runs in bash on every OS (Git Bash on Windows).
  - **Lint job:** also runs clippy for `x86_64-pc-windows-msvc` from Linux. `cargo xtask ci` does the same when that target is installed and prints SKIPPED otherwise. Crates whose tests compile C code for the target (criterion) are checked without their tests there; the Windows runner covers them.
  - **Test builds** (not releases), kept 14 days, each with a `BUILDINFO.txt` (target, commit, branch, date, run link):
    - Linux x86_64 and aarch64: the zigbuild glibc 2.31 builds (D-015), as `termos-<sha7>-linux-<arch>.tar.gz`;
    - macOS arm64: `termos-<sha7>-macos-arm64.tar.gz`;
    - Windows x86_64: `termos-<sha7>-windows-x86_64.zip`, with the C runtime linked statically (no Visual C++ redistributable needed). CI sets `RUSTFLAGS` itself because the toolchain action's `RUSTFLAGS` overrides `.cargo/config.toml`.
  - Test builds don't wait for the test jobs, so a build is available even when a test fails; the run still shows red. The Linux builds are downloaded and smoke-run on matching runners; the macOS and Windows builds are smoke-run where they are built.
  - Builds are unsigned: macOS needs `xattr -d com.apple.quarantine`, and Windows SmartScreen needs "More info → Run anyway" or `Unblock-File` (docs/getting-started.md). Signing comes with releases (M9).
- **Alternatives:** test builds only on `main` or on demand (the owner wanted every push); GitHub releases for test builds (they'd clutter the release list).

## D-042: M4 is split; wasmtime for widgets; animation policy
- **Status:** Accepted (owner) · 2026-11-01
- **Decision:**
  - **Split (owner):** M4 becomes **M4a, graphics and themes** (the capability probe, images with the fallback ladder, images inside panes, the image viewer, themes following OSC 11, animations, the Kitty keyboard protocol for programs in panes) and **M4b, apps and widgets** (`termos-hw`, the other apps, chart widgets, shadows and rounded borders, WASM widgets). M4a goes first. Both stay under milestone number 4 for work markers.
  - **WASM engine (owner, resolves O-8):** wasmtime, used in M4b. Its RAM cost on a Pi 4 is measured there; the runtime stays behind the `wasm-widgets` feature, so builds without widgets don't pay for it.
  - **Animations (owner):** on by default, decided per client: off when the client is remote (SSH), when `[ui] reduced-motion = true`, or with `[ui] animations = false`. They never add a timer while nothing animates (D-028).
- **Alternatives:** one M4 (too large to review in one step); wasmi (smaller, interpreted; the owner chose wasmtime); animations off by default.

## D-043: The capability probe; capabilities v2; frontend and RPC versions apart
- **Status:** Accepted (lead) · 2026-11-01
- **Decision:**
  - **Probe:** at attach, in raw mode, the client writes one query batch (XTVERSION, a Kitty graphics query, DECRQM 2026, `CSI ? u`, `CSI 16 t`, `CSI 14 t`, OSC 11) ending with DA1, which every terminal answers last, and reads stdin directly (`poll` + `read`) until the DA1 reply or 200 ms (800 ms over SSH). It replaces crossterm's keyboard probe, which could wait 2 s. Keys typed meanwhile are passed on. `TERMOS_PROBE=0` skips it; Windows consoles are detected from the environment only.
  - **Parser:** `termos_render::probe::parse_replies` is pure, fuzzed (`caps_reply`) and tested against replies from kitty, WezTerm, foot, iTerm2, xterm, Windows Terminal, GNOME Terminal (rebuilt from documented formats) and tmux 3.4 (recorded with `termos debug caps --raw`).
  - **Caps v2:** graphics (Kitty, Sixel, iTerm2), a `TERMOS_GRAPHICS` override, cell and text-area pixel sizes, the background colour (or `COLORFGBG`), the terminal's name, `in_tmux`, `remote` (SSH) and `reduced_motion`. Inside tmux or screen, never Kitty or iTerm2 (they would not pass it on); Sixel only if tmux itself reports it.
  - **Versions (refines D-031):** the frontend protocol (postcard frames) is now version 3 (capabilities v2, then images). Control clients and plugins keep their own JSON-RPC version (1), so scripts and plugins are not affected by frame changes. `server.info` reports both.
- **Alternatives:** crossterm's probes (one query at a time, long timeouts); querying through the server (the client owns the terminal).

## D-044: Themes are presets, follow the terminal, and are per client
- **Status:** Accepted (lead) · 2026-11-01
- **Decision:**
  - Presets dark, light and high contrast (every text colour 7:1 or more against its background, tested). `[theme] base = "auto" | "dark" | "light" | "high-contrast"`; the colour keys are overrides, so existing configs keep working.
  - `auto` is light on a light terminal background (OSC 11, then `COLORFGBG`), dark otherwise.
  - Each client has its own theme, from its terminal's background or a `theme.select` pick (from the control API: the session's newest client). Drop shadows blend toward the real background.
  - Programs in panes asking for the default colours (OSC 10/11) get the most recently active client's terminal colours.
- **Alternatives:** one theme per session (two terminals with different backgrounds would get one wrong).

## D-045: Animations are drawn per client and cost nothing at rest
- **Status:** Accepted (lead), policy by D-042 · 2026-11-01
- **Decision:** floats grow from their centre when they open, a closing float's frame shrinks away, a toast slides in: 120 ms, cubic ease-out. They change only how a frame is drawn for clients that animate, never the layout, hit-testing or PTY sizes. A 16 ms frame deadline exists only while an animation runs and some client animates; other clients' frames do not change, so they receive nothing. `Desktop::render_at(now)` makes frames testable.
- **Alternatives:** animating the layout (PTY resizes mid-animation); a fixed frame timer (idle cost).

## D-046: The Kitty keyboard protocol for programs in panes; two guards for the emulator
- **Status:** Accepted (lead) · 2026-11-01
- **Decision:**
  - Programs can enable the Kitty keyboard protocol in their pane; TermOS encodes flags 1 (disambiguate), 8 (all keys as escape codes) and 16 (associated text), and the `CSI ? u` reply lists only those. Release events and alternate keys (flags 2 and 4) are `TODO(M9)`.
  - `alacritty_terminal` 0.26 panics when a program pushes more than 4096 keyboard modes (it removes from the wrong stack). A streaming guard in front of the emulator mirrors both screens' stack depths and drops pushes beyond 256; it follows the parser's rules (control bytes inside sequences, aborted sequences, long parameter lists) so it never under-counts.
  - Any panic while parsing program output now resets that pane's emulator instead of taking the server, and every session, down. Both layers were verified against the real panic.
- **Alternatives:** waiting for an upstream fix (a crafted 16 KB of output could stop the server meanwhile); forking alacritty_terminal.

## D-047: Images: one model, a per-client ladder, the server decides, the client encodes
- **Status:** Accepted (lead) · 2026-11-01
- **Decision:**
  - **Decoding:** the `image` crate (png, jpeg, gif first frame, webp, bmp; no rayon), at most 8192 px per side and 256 MiB per decode. Our own area-average resampling (sharp at whole ratios), block and braille rasters, Kitty/Sixel/iTerm2 encoders and a Sixel decoder bounded by the pixels actually drawn. `ratatui-image` was evaluated and not used: it writes straight to a terminal and has no notion of several clients or of occlusion.
  - **Ladder, per client:** Kitty → Sixel → iTerm2 → quadrant blocks. Blocks are drawn into the cell grid (windows simply cover them). Kitty gets one placement per visible piece (the image minus floats above and the palette and which-key boxes), each a crop of one upload. Sixel and iTerm2 are used only for a wholly visible image, off the last row, while nothing moves; otherwise blocks in the visible pieces. `[graphics] protocol` or `TERMOS_GRAPHICS` forces one. Braille is not on the ladder: it has one colour per cell, so quadrant blocks show pictures better; its raster is kept for charts (M4b).
  - **Wire (protocol 3):** `FrameUpdate.images` carries the whole placement list only when it changed, so idle frames stay silent; image data travels as PNG in `ImageData` chunks of at most 1 MiB, once per client and size, and `ImageFree` releases it. The server keeps PNG variants in a shared cache and poisons a client's previous grid where a Sixel or iTerm2 image moved or went away, so the next diff paints over its pixels. The client writes images after the cells in the same synchronized frame and frees Kitty images when it leaves.
- **Alternatives:** encoding on the server (it would need every terminal's protocol); one placement per image with z-index tricks (Kitty would draw over overlays).

## D-048: Images inside panes are anchored to the text
- **Status:** Accepted (lead) · 2026-11-01
- **Decision:**
  - A streaming interceptor takes Kitty graphics APCs and Sixel DCSs (at most 32 MiB each, across reads), `CSI 16 t` and XTSMGRAPHICS out of program output before the emulator; everything else passes unchanged. Fuzz target `apc_dcs_intercept`.
  - Kitty graphics: direct transmission only (files, temp files and shared memory are refused, so program output can never make TermOS read a file), PNG/RGB/RGBA, zlib, chunks, put, crops, cell sizes, delete, and replies as kitty sends them. Sixel is decoded within the pane's budget. At most 16 MiB and 128 images per pane, oldest first out.
  - Each image is anchored by a hidden OSC 8 link on its top-left cell: it scrolls, reflows and clears with the text, and the cursor moves as kitty and xterm move it. Nothing in TermOS reads links, so the anchors never show.
  - Programs get what image tools need: DA1 with Sixel, real `14t`/`16t` pixel sizes, XTSMGRAPHICS, and pixel sizes on the PTY. Pane images go through the same ladder as app images, including images partly scrolled above the view.
  - Windows: not supported yet (`TODO(M9)`). The system ConPTY can filter these sequences out before TermOS reads them, so the pane tests run on Unix only.
- **Alternatives:** our own grid overlay keyed by line numbers (breaks with reflow and clears); Kitty's Unicode placeholders (few programs use them).

## D-049: Apps show images; the image viewer
- **Status:** Accepted (lead) · 2026-11-01
- **Decision:** `App::images()` reports images (a shared `Picture`, a key, a cell rect to fit in); `AppWaker` lets an app ask for a redraw from another thread (it reaches the desktop as a pane event). The image viewer (`image.open PATH`, also `termos msg image.open '{"arg": "…"}'`) decodes on its own thread within a 64 MiB file limit, pages through the folder's images and toggles fit/1:1. It is the first app in `termos-apps`.
- **Alternatives:** decoding on the server loop (a large image would stall every session); a separate viewer process (no occlusion or per-client ladder).


## D-050: M4b owner answers: hardware readings, trash, clipboard history, WASM in every build
- **Status:** Accepted (owner) · 2026-11-02
- **Decision:**
  - **`termos-hw` (a new crate, below `termos-apps` and `termos-models`):** the `sysinfo` crate for CPU, memory, temperatures, disks, network and processes on every platform, without its rayon feature. GPUs: sysinfo 0.40 lists them itself on Linux, Windows and macOS (name, load, video memory); our own probes add what it lacks: temperature and power from Linux sysfs (each DRM card's `hwmon`), GPUs that are not on PCI such as the Raspberry Pi's, and NVIDIA temperature and power through NVML loaded at run time (`nvml-wrapper`; no CUDA needed to build or run). GPU temperature and power on macOS, and for non-NVIDIA GPUs on Windows, are `TODO(M9)`. The same crate tells M7 which accelerators exist; `termos debug hw` prints what it reads. (Refined while building: the owner chose sysinfo plus our own GPU probes when sysinfo was thought to have no GPU support; its own list now covers more platforms, so the probes only fill in.)
  - **File manager deletes to the trash** (`trash` crate: freedesktop trash, the Recycle Bin, the macOS Trash). Where no trash is available it asks again and then deletes permanently.
  - **The clipboard manager keeps what was copied inside TermOS** (copy mode, mouse selection, programs using OSC 52, apps), in memory only: never written to disk, logs or snapshots, and gone when the server stops. TermOS does not read the operating system's clipboard.
  - **WASM widgets are in every build:** the `wasm-widgets` feature is on by default. The engine is created only when a widget is configured, so without widgets RAM and CPU are unchanged; the binary grows (measured in M4b).
- **Alternatives:** our own `/proc` and `/sys` readers (Linux only); `sysinfo` without GPUs; permanent deletion after confirming; reading the desktop clipboard with `arboard` (only works on the same graphical desktop, not over SSH or on bare metal); an opt-in saved history; widgets only in builds made with the feature.

## D-051: WASM widgets: the `termos:widget` world, one thread, bounded calls
- **Status:** Accepted (lead; the engine is the owner's, D-042, and in every build, D-050) · 2026-11-02
- **Decision:**
  - **World:** `termos:widget@0.1.0` in `crates/termos-plugin/wit/widget.wit`. A widget exports `init(settings) -> result<update, string>`, `render(width) -> list<span>`, `tick() -> update` and `on-event(event) -> update` (a click, with the column). Every call returns `update { redraw, next-tick-ms }`: the widget says when it next wants to run, instead of the fixed tick interval first sketched in ARCHITECTURE.md §9, so a widget at rest is never called.
  - **Imports:** WASI 0.2 only, so any toolchain that targets it works (Rust `wasm32-wasip2`, componentize-py, jco, TinyGo). The context denies network, name lookup, environment and arguments; files only in the folders `[widgets.<name>] dirs` grants, read-only.
  - **Host:** wasmtime 50 with the component model and cranelift, without its parallel compilation (rayon), GC, profiling or debugging features. One thread runs every widget on its own current-thread tokio runtime with async bindings, so wasmtime-wasi never starts its hidden multi-threaded runtime. The thread and the engine exist only while `[widgets]` names a widget; a config reload restarts the widgets when their config changed.
  - **Bounds, per call:** fuel (500 M instructions for `init`, 50 M otherwise), 32 MiB of memory, 2 s of wall-clock time, ticks at least 100 ms apart, at most 64 spans and `max-width` columns (default 24), control characters removed. A widget that breaks a bound, traps or fails to load is stopped and a notification says why; the others carry on.
  - **Status line:** drawings come after the plugin widgets; a click on one goes to it.
  - **Sample and tests:** `examples/widgets/pomodoro` (Rust, `wasm32-wasip2`, 70 KB) only ticks while it counts down. Its build is kept as `crates/termos-shell/tests/fixtures/pomodoro.wasm`, so the widget tests run on every OS without the WASI target; `cargo xtask widgets` refreshes it, and a CI job rebuilds it from source and runs the same tests against that build.
  - **Cross-target clippy** (xtask and the CI lint job) runs without default features: wasmtime compiles C for Windows. The native Windows and macOS jobs check and test everything.
- **Alternatives:** no WASI at all (smaller, but only `no_std` Rust guests could be written); a thread per widget; wasmtime-wasi's synchronous bindings (they start a multi-threaded runtime behind the scenes); a fixed tick interval per widget; building the sample in every test run (every machine would need the WASI target).

## D-052: The M4b apps, and what the App API gained
- **Status:** Accepted (lead) · 2026-11-02
- **Decision:**
  - **App API (termos-core):** `App::deadline` and `on_timer`, which the desktop schedules only while the app is on screen for some client (D-028), so a minimized or background app costs nothing; effects `Copy(text)` and `PasteAndClose(text)`; `AppImage::scroll_top` for pictures scrolled partly out of an app.
  - **Widgets for apps:** `termos_core::widgets` holds a line chart drawn in braille (`Canvas` in termos-render), a sparkline and a meter in eighth blocks, and an image slot. The theme gains an `error` colour (7:1 in high contrast).
  - **Clipboard history (D-050):** server-wide, in memory, newest first, without duplicates; `[clipboard] history = 50`, at most 1 MiB per entry and 8 MiB in all; pinned entries survive limits and clearing; `Debug` shows no text. Copy mode, mouse selections, OSC 52 from programs and apps all record into it.
  - **Apps:** the markdown viewer (pulldown-cmark; local pictures only, nothing is fetched); the file manager (a worker thread for listing, previews and copying; deleting to the trash; `file.open` picks the app by kind, `file.edit` runs `$VISUAL`/`$EDITOR`, else `vi` or Notepad, in a new pane); the system monitor (`termos-hw` on its own thread, once a second, only while on screen); the clipboard manager (prefix `=`, like tmux's choose-buffer); settings (each change written into config.toml with toml_edit, keeping its comments, checked with `Config::parse` before an atomic write, then reloaded).
  - **Panels:** the palette and which-key take the floats' border style and drop shadow (`[floats] border`, `shadow`).
- **Alternatives:** apps with timers of their own (they would run while hidden); reading the operating system's clipboard (D-050); a settings app that writes a fresh file (comments lost); syntax highlighting in the markdown viewer (syntect is large; later if wanted).

## D-053: M5 is split; lazygit for Git; mise found or fetched; the language matrix runs nightly
- **Status:** Accepted (owner) · 2026-11-02
- **Decision:**
  - **Split (owner):** M5 becomes **M5a, languages and editors** (`termos lang` over mise, the managed Neovim and Helix configs with language servers and debuggers, project templates, build/run/test panels, the Git UI, the language CI matrix, `docs/languages.md`) and **M5b, dev rootfs and packages** (the Debian dev rootfs through bubblewrap, Podman, the package manager app). Both stay under milestone number 5 for work markers.
  - **Git UI (owner, resolves O-3):** lazygit, embedded in a TermOS window and installed with mise when missing. Deeper integration can follow later.
  - **mise (owner):** TermOS uses the `mise` on `PATH` when it is recent enough; otherwise it downloads one pinned mise release from GitHub on first use, checks it against SHA-256 sums compiled into TermOS, and keeps it in TermOS's own data folder. Nothing else on the system changes.
  - **Language CI (owner):** the full matrix (every listed language on x86_64 and aarch64: install with mise, build and run a hello-world, a headless Neovim language-server handshake) runs nightly and on pushes that change the developer environment; other pushes run a quick subset (Rust, Python, Go).
- **Alternatives:** gitui, or our own Git panel on gitoxide; one M5; requiring mise to be installed, or bundling it in every download; the full matrix on every push, or nightly only.

## D-054: How `termos lang` and the editors use mise; `[dev] editor`
- **Status:** Accepted (lead) · 2026-11-02
- **Decision:**
  - **Languages:** `termos lang install` makes the runtime the global default (`mise use --global`), so TermOS panes, which get mise's shims at the front of `PATH`, run it in any shell. Language servers and debuggers are installed without becoming defaults (`mise install`), and editors start them through `mise exec` with the exact versions installed: editors need nothing on `PATH`, and starting one never triggers an install. A server that fails to install is reported and the command exits non-zero, but the language stays installed.
  - **Downloads:** TermOS fetches only mise itself (D-053), with the system's `curl`, so it links no TLS stack; everything else is mise's job.
  - **Neovim (D-012):** `NVIM_APPNAME=termos-nvim`. TermOS writes `init.lua` and `lua/termos/` (static files embedded in the binary) and `lua/termos/generated.lua` (servers and debuggers with their exact commands); `lua/user.lua` is the user's, created once and never touched. Neovim 0.12.6, through mise; plugins (nvim-dap, nvim-dap-ui, nvim-nio) through Neovim's own `vim.pack`, pinned to exact commits. Servers use Neovim's built-in `vim.lsp.config`/`enable`; vtsls and Deno's server start only inside their own kind of project, so they never both attach.
  - **Helix:** TermOS writes `languages.toml` when there is none (or it is TermOS's); otherwise `languages.termos.toml` beside it, for the user to merge.
  - **`[dev] editor = "auto" | "termos-nvim" | "helix" | "env"`:** auto means `$VISUAL`/`$EDITOR` when set, else TermOS's Neovim. `termos edit [FILE]…` starts it; the file manager's `file.edit` runs `termos edit` unless the choice is the environment's editor.
  - **Debuggers:** delve (Go), codelldb (Rust, C, C++, Zig), debugpy (Python), js-debug (Node, Bun), netcoredbg (.NET). Java, Kotlin, Lua and shell have none yet.
- **Alternatives:** runtimes only per project (panes would find nothing outside projects); servers on `PATH` through shims (needs every server in the global config); a plugin manager such as lazy.nvim (one more moving part now that Neovim has `vim.pack`); always TermOS's Neovim (ignoring a user's `$EDITOR`).

## D-055: Templates are `termos create`; task panels and the Git UI are panes running `termos`
- **Status:** Accepted (lead) · 2026-11-02
- **Decision:**
  - **`termos create <template> [dir]`** instead of the planned `termos new <template>`: `termos new` already creates sessions, as tmux's does. Templates are embedded files, one per language (`rust-cli`, `go-cli`, `python-cli`, `node-cli`, `bun-cli`, `deno-cli`, `c-cli`, `cpp-cli`, `zig-cli`, `java-cli`, `kotlin-cli`, `csharp-cli`, `lua-cli`, `shell-script`, `web-page`), so a project builds the same next year; Zig and .NET use their own generators (`zig init`, `dotnet new console`), whose files change between versions. Each project gets a `mise.toml` pinning the version installed. Only new or empty folders.
  - **Tasks:** build, run and test come from the project's files (Cargo, Go, Deno, npm or Bun, Python, CMake, .NET, Gradle, Maven, Zig, single-file Java, Kotlin, Lua, shell), the nearest such folder up; `[tasks]` in a `termos.toml` there overrides them and adds more. `termos task NAME` runs one in a shell in the project folder.
  - **Panels:** `task.build`, `task.run`, `task.test` and `task NAME` open a pane at the bottom running `termos task NAME --wait` (it waits for Enter, so the output stays); a task's next run replaces its panel. `git.open` (prefix `g`) opens `termos git` (lazygit through mise) in a float. The desktop runs the `termos` binary itself, which keeps termos-devkit out of termos-shell (ARCHITECTURE.md §4).
- **Alternatives:** `termos new` doing both (a session name and a template name would be ambiguous); generating every project with each language's own tool (results drift with versions); task detection inside the shell crate (would put devkit below the shell).

## D-056: `termos lang check` is the language acceptance test; TermOS trusts the `mise.toml` it writes
- **Status:** Accepted (lead) · 2026-11-02
- **Decision:**
  - **`termos lang check <lang>[@version]`** runs, in order: install (as `termos lang install`), create the language's template in a scratch folder, its build, run and test tasks through `mise exec` (as in a pane; the run must print what the template prints), TermOS's Neovim headless on the template's main file until every server for that filetype has completed the LSP `initialize` handshake, and the debugger started as nvim-dap starts it until it answers the DAP `initialize` request. It prints each step, exits non-zero if any failed, and keeps the project only then. The check is in the product, not in CI scripts, so a user can run the same check on their own machine.
  - **CI (D-053):** `.github/workflows/languages.yml` builds `termos` once per architecture and runs one job per language and architecture (ubuntu-24.04 and ubuntu-24.04-arm), nightly, on developer-environment changes and on demand; ci.yml calls it on every push for Rust, Python and Go on x86_64. The check jobs do not check out the repository: the binary carries its templates and editor config.
  - **Platform limits** name the tool (`Limit { platform, tool, why }`): `termos lang install` leaves such a tool out on that platform and says why; the check reports the step it would have tested as skipped, not failed. The first one: clangd has no aarch64 Linux build.
  - **`mise trust`:** `termos create` marks the `mise.toml` it just wrote as trusted. mise can stop to ask before it uses a config file it has not been told to trust, and nobody can answer in CI or behind a task panel; this file was written by TermOS and lists only tools.
  - **Neovim:** each part of TermOS's config (plugins, servers, debuggers) loads on its own, so one failing leaves the others working; root markers that are patterns (`*.csproj`) are found by a `root_dir` function, since `root_markers` takes names literally.
- **Alternatives:** shell scripts in the workflow (users could not run the check, and it would drift from the product); checking servers by starting them outside an editor (it would not test the editor config users get); the full matrix on every push (about 30 jobs of downloads per push).

---

## Open questions (the owner will be asked before the milestone that needs each one)

| ID | Question | Needed by | Current lean |
| --- | --- | --- | --- |
| O-1 | Default WM keybinding scheme: a tmux-style leader key, or modifier chords (Alt/Super)? | M2 | **Resolved by D-023:** tmux prefix `C-b`. |
| O-2 | PTY layer: `portable-pty`, `pty-process` or direct `rustix` openpty with tokio `AsyncFd`? | M1 | **Resolved by D-025:** `portable-pty`. |
| O-3 | Git UI: embed lazygit/gitui in a window, or build our own panel? | M5 | **Resolved by D-053:** embed lazygit. |
| O-4 | LLM clients: provider SDK crates (async-openai, others) or our own thin reqwest clients behind `Provider`? | M6 | Own thin clients plus shared SSE parsing, so the model-listing and tool-calling shapes stay uniform. |
| O-5 | Secret store on bare metal, where there's no Secret Service: an age-encrypted file unlocked by passphrase, a TPM-sealed key, or something else? | M6 | age file with a passphrase; TPM later. |
| O-6 | llama.cpp integration: a managed `llama-server` subprocess or FFI (`llama-cpp-2`)? | M7 | Subprocess: crash isolation, a ready-made OpenAI API, easy CUDA/ROCm/Vulkan build swaps. |
| O-7 | Rust-native inference: ONNX Runtime (`ort`), candle, or both? | M7 | candle first (pure Rust, no C++ runtime). |
| O-8 | WASM engine for widgets: wasmtime (component model, JIT, larger) or wasmi (interpreter, small)? | M4 | **Resolved by D-042:** wasmtime. |
| O-9 | KMS font rasterizer: swash or fontdue? | M8 | swash (shaping and hinting quality). |
| O-10 | Init system: BusyBox init, s6, or systemd? | M8 | BusyBox init or s6, for size. |
| O-11 | Network management: connman, NetworkManager, or iwd + a small DHCP client? | M8 | iwd + dhcpcd, with a TermOS settings UI. |
| O-12 | A/B updates: RAUC or SWUpdate? | M9 | RAUC (Buildroot package, Pi tryboot support). |
| O-13 | Hosted Linux binaries: glibc 2.31 floor only, or also static musl? | M9 | Both if musl doesn't hurt performance. |
| O-14 | Bootloader for generic aarch64 UEFI: GRUB2 or systemd-boot? | M8 | GRUB2, shared with x86_64. |
