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
- **Status:** Accepted (lead), refined by D-040 (token source, SDK transport) and D-064 (the sandbox) · 2026-11-01
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

## D-057: Servers start with the runtime they were installed for; helpers never replace a default
- **Status:** Accepted (lead) · 2026-11-02
- **Context:** the language CI found that jdtls 1.63.0 needs Java 25 (its bundles require JavaSE-25, although its launcher checks for 21), and that a language's helper runtime (Kotlin's Java) replaced whatever default the user had.
- **Decision:**
  - **Java** installs Temurin 25, the current LTS. Kotlin keeps Temurin 21: kotlin-language-server 1.3.13 embeds Kotlin 2.1.0, which cannot parse Java 25's version string and exits.
  - **Servers** start with the newest installed version on the line the registry pins for their runtime and helpers (`temurin-25`, `temurin-21`, `1.63.0`); only floating versions (`latest`, `lts`, `stable`) follow the user's default. A user's or project's older Java cannot break jdtls.
  - **Helpers** become the global default only when the user has none for that tool; otherwise they are only installed. Installing Kotlin no longer changes which `java` a shell runs.
  - **jdtls** is pinned by its milestone URL; TermOS moves the pin.
- **Alternatives:** one Java for every language (jdtls would force Java 25 on kotlin-language-server); a separate tool list per server (more registry for the same result).

## D-058: M6 owner answers: our own thin LLM clients; an age file for secrets where no keyring exists
- **Status:** Accepted (owner) · 2026-11-02
- **Decision:**
  - **LLM clients (owner, resolves O-4):** our own thin clients over `reqwest` behind the `Provider` trait, sharing one SSE parser, instead of provider SDK crates (`async-openai` and others). Model listing, streaming and tool calls keep one shape across providers, and a provider's API changes reach us only through code we own and test against recorded fixtures.
  - **Secrets where there is no keyring (owner, resolves O-5):** an age file (`<data>/secrets.age`) encrypted to a passphrase (scrypt), unlocked for the life of the server when an agent first needs a key. TPM sealing may come later. Where a keyring service exists (Secret Service on Linux, Keychain on macOS, Credential Manager on Windows), keys go there; `[secrets] store` chooses.
  - **Crates:** reqwest 0.13 with rustls and the ring provider (aws-lc-rs, reqwest's default, carries OpenSSL-licensed code, which D-003 rules out); `keyring` 4 (zbus for the Secret Service: no C library to link); `age` 0.11 (0.12 pulls P-256 code with a timing advisory in `crypto-bigint` 0.5, for recipients TermOS never uses; the file format is the same). cargo-deny ignores five "unmaintained" notices, each with its reason in `deny.toml`: three crates zbus folded into itself, and two used only at build time by age's localisation macros.
  - **TLS is a feature of termos-ai (`tls`, on by default):** ring compiles C, which the clippy check for Windows run from Linux cannot build, as with wasmtime (D-051). Without it only plain-HTTP endpoints (local servers) work, and saying so is the error.
- **Alternatives:** SDK crates (one per provider, each with its own types and stream model); a TPM-sealed key first (not every board has a TPM, and the passphrase is needed as a fallback anyway); `rustls-rustcrypto` as the provider (not mature).

## D-059: The agent runtime: a session per window, provider-neutral and append-only
- **Status:** Accepted (lead) · 2026-11-02
- **Decision:**
  - **Windows without a dependency:** `termos_core::app::Launcher` lets the shell open agent windows and the audit viewer (`agent.open [profile]`, `agent.resume [id]`, `audit.open`; prefix `a` and `A`) without depending on termos-ai; the `termos` binary hands the server a launcher (ARCHITECTURE.md §4). Apps also gained `handle_paste`.
  - **A session per window:** each agent window runs its session on a thread of its own with a single-threaded tokio runtime; the window and the session share a `View` (transcript, phase, usage) behind a mutex, and the session wakes the window (`AppWaker`). Closing the window ends the session, its commands and its MCP servers. Nothing runs while a window waits for input.
  - **Providers (D-058):** one `Provider` trait (`list_models`, `chat` streaming deltas and returning the whole reply). Anthropic: Messages API version 2026-10-21, no sampling parameters and no `thinking` setting (each model's default), `eager_input_streaming` on every tool (inputs are parsed and checked here: an input that is not a JSON object, or cut off by the token limit, is answered with an error, never run), top-level prompt caching, `refusal` stops shown, model limits from the Models API. The OpenAI family: Chat Completions for every server (it is what Ollama, vLLM, LM Studio, OpenRouter and Moonshot share), `max_completion_tokens` for OpenAI itself, usage in the stream; Kimi's `reasoning_content` is sent back with its tool calls. No idle HTTP connections are pooled (hyper's pool keeps a timer).
  - **Conversations are append-only:** blocks a provider needs back unchanged (Anthropic's `thinking` blocks with their signatures, anything unknown) are kept verbatim as `Opaque`; the system prompt and tool list are fixed for the session. After a cancelled reply, the next message joins the last user message instead of editing anything the model saw.
  - **The loop:** up to 4 tries for a rate-limited, overloaded or failed request, 2/4/8 s apart, only before any output arrived; at most 60 rounds of tool calls per message, then the user is asked to continue; Esc cancels the reply or the running call, and calls left over are answered "cancelled".
  - **Tokens and cost:** per session, including cache reads and writes; cost only with prices from `[agents.providers.<name>.prices]` or the provider's own list (OpenRouter's).
  - **Tools:** `read_file`, `list_dir`, `search_code` (ripgrep's `grep` and `ignore` crates), `write_file`, `edit_file`, `run_command`, `fetch_url`, MCP tools. `edit_file` is the planned `apply_patch` as exact replacement (the old text must appear once): models apply that far more reliably than unified diffs, and the diff shown is computed from the result. An edit is checked again before it is written (the file may have changed while the user decided). `fetch_url` never follows redirects, so each new host goes through the permissions.
  - **History:** `$XDG_STATE_HOME/termos/agents/<id>.json`, mode 0600, rewritten after every message, with the audit log's last hash; `[agents] history = false` keeps none.
- **Alternatives:** agents as `termos agent` processes in PTY panes (no shared theme, images or confirmation overlays, and every UI piece twice); one runtime for all sessions (one slow session's blocking work would stall the others); the OpenAI Responses API (only OpenAI has it); unified-diff `apply_patch`.

## D-060: The permission engine
- **Status:** Accepted (lead) · 2026-11-02
- **Decision:**
  - **Rules:** a profile gives each class (`read`, `write`, `command`, `network`, `mcp`) `allow`, `ask` or `deny`; defaults: read allow, the rest ask. Filesystem scopes are the workspace (the focused pane's folder) plus `read-paths` and `write-paths`; paths are resolved with symbolic links followed as far as they exist, so a link cannot lead out unnoticed.
  - **Always asks** (a `deny` still refuses): deleting files, discarding changes, rewriting history, pushing and force-pushing, installing or removing packages, running a downloaded script, running as another user, writing disks, stopping processes, changing the system, writing outside the scopes, reading or writing credentials (`~/.ssh`, `~/.aws`, keyrings…), anything on the network (except `fetch_url` to a listed domain with `network = "allow"`), and every command when the sandbox is missing or partial. TermOS's own secrets file is never read or written by tools.
  - **Commands** are classified, not interpreted: split at unquoted `;`, `&&`, `||`, `|`, `&` and newlines; wrappers (`env`, `timeout`, `nice`…) and assignments skipped. `allow-commands` patterns (`*` matches anything) must match every part; a command substitution (`$(…)`, backticks, `<(…)`) or an unclosed quote always falls back to asking.
  - **"Allow for this session"** covers one exact command line, every write inside the scopes, reads of one folder, one MCP tool or one host; it is never offered for what always asks.
- **Alternatives:** parsing commands with a real shell grammar (more code, and still not what the shell would do with aliases and functions; the sandbox is the boundary, the classifier only decides when to ask); approvals per tool class (too coarse for commands).

## D-061: The agent sandbox: bubblewrap, then Landlock through a re-executed helper
- **Status:** Accepted (lead) · 2026-11-02
- **Decision:**
  - **bubblewrap:** `--unshare-all`, `--die-with-parent`, the root bound read-only, a private `/tmp`, the workspace and `write-paths` bound read-write, credentials and the runtime folder (D-Bus, ssh-agent and TermOS sockets) hidden behind empty folders (files behind `/dev/null`), TermOS's conversations, audit logs and secrets file hidden; the network shared only when the call asked for it and the user agreed; the start folder bound read-only if nothing else makes it exist.
  - **Landlock** (ABI 6, best effort): `termos sandbox-exec` — the same binary, handled before argument parsing — restricts itself (writes beneath the writable folders and a few devices, no TCP without the network, scoped signals and abstract sockets), drops environment variables named like credentials, and `exec`s the command. No code runs between `fork` and `exec`, so no `unsafe` is needed.
  - **Less:** Landlock alone (no bubblewrap) confines writes and TCP but hides nothing and leaves UDP, so it counts as partial and every command asks; the system `/tmp` is then replaced by a private `TMPDIR`. Without either, commands run unconfined and every one asks, with a warning in the window. macOS is confirm-only by design; Windows is `TODO(M9)`. `sandbox = "required"` refuses without bubblewrap.
  - **PTY:** `run_command` runs on a PTY (termos-term) so programs behave as in a pane; the window shows a terminal of the output, the model gets plain text (escape sequences stripped, carriage returns applied, the last 30 KiB, redacted). Two minutes by default, at most thirty.
  - **`termos sandbox run|status`:** the same sandbox from the shell, used by external agents and plugins (D-063, D-064). `termos debug connect HOST:PORT` checks network reach.
  - **CI** installs bubblewrap on the Linux runners, allows unprivileged user namespaces (Ubuntu 24.04 restricts them through AppArmor) and sets `TERMOS_REQUIRE_SANDBOX=1`, so the sandbox tests fail rather than skip there.
- **Alternatives:** Landlock in a `pre_exec` hook (unsafe, and not async-signal-safe in a threaded server); seccomp now (after 1.0, ROADMAP); firejail or nsjail (more to install; bubblewrap is on most desktops already for Flatpak).

## D-062: Audit logs are hash-chained JSONL; secrets are redacted before anything is written
- **Status:** Accepted (lead) · 2026-11-02
- **Decision:**
  - **Format:** `$XDG_STATE_HOME/termos/audit/<session>.jsonl`, mode 0600, one JSON object per line: `seq`, `time`, `prev` (the SHA-256 of the previous line; 64 zeros first), `session`, and the event: the session (profile, provider, model, workspace, sandbox), a tool call (redacted arguments, `allowed`/`approved`/`denied`/`refused`/`invalid`/`cancelled`, why, a redacted result summary, success) or a note. Lines are synced as written.
  - **Verification:** `termos audit verify` walks the chain; the session's conversation file holds the last hash, so lines cut off the end show too. A log that does not verify is never continued: it is renamed `.broken` and a new chain starts with a note saying so. Without a working audit log, no tool runs.
  - **Redaction:** the values of the keys the session resolved, and text shaped like a credential (provider key prefixes, bearer tokens, private key blocks, `password=…`), become `[redacted]` in audit records, tool results the model sees, and saved conversations. What the user types loses only the session's own keys.
  - **Canary tests:** an agent session against a mock provider with a canary key in the secrets file and in the server's environment (the real desktop on a PTY) must leave it in no log, audit record, conversation, snapshot, config or runtime file, and in no request to the provider; and the library's session test checks the same for a key the user pasted.
- **Alternatives:** a log signed with a key (where would the key live?); hashing entries without the line bytes (the JSON would need canonicalizing).

## D-063: MCP servers through rmcp; external agents through presets
- **Status:** Accepted (lead) · 2026-11-02
- **Decision:**
  - **MCP:** the `rmcp` 3.8 client, without its default features or aws-lc (`reqwest-tls-no-provider`). `[agents.mcp.<name>]` with `command` (stdio, started in the sandbox like commands, network only with `network = true`) or `url` (streamable HTTP over TermOS's own HTTP client, a bearer token from the secret store). Only servers the profile lists start, with the session, and they stop with it. Tools reach the model as `mcp__<server>__<tool>` and follow the `mcp` rule.
  - **External agents:** built-in presets for Claude Code, Codex CLI, Gemini CLI, aider and opencode; `[agents.external.<name>]` adds or changes them. `agent.external NAME` opens a float running `termos agent external NAME` in the focused pane's folder; a preset with `sandbox = true` runs under `termos sandbox`, with its environment kept (its own keys), the network on, and its settings folders writable. A missing program is reported with where to get it.
- **Alternatives:** our own MCP client (the protocol moves quickly; rmcp is the reference SDK); external agents as in-process apps (they are terminal programs already).

## D-064: Launched plugins run in the agent sandbox
- **Status:** Accepted (lead; completes D-034's deferred item) · 2026-11-02
- **Decision:** with `[plugins] sandbox = true` (the default), the server starts each plugin through `termos sandbox run --keep-env --workspace <state>/plugins/<name> --keep <session socket> [--net] -- <exec…>` in the plugin's directory. The plugin's own data folder (`TERMOS_PLUGIN_DATA`, made for every plugin, sandboxed or not) is the only writable one besides a temporary folder; the session socket is bound back into the hidden runtime folder; the network needs the manifest's new `network` permission. `termos sandbox run` replaces itself with the sandbox, so stopping a plugin stops what runs inside. Without a sandbox (macOS, Windows, no bubblewrap or Landlock) plugins run as before.
- **Alternatives:** a sandbox library call inside termos-shell (it would depend on termos-ai); off by default (plugins written before M6 needing the network now have to ask for it, which is the point).

## D-065: Local model backends: llama.cpp as `llama-server` subprocesses, candle for embeddings
- **Status:** Accepted (owner: O-6 and O-7, the recommended answers) · 2026-11-02
- **Context:** M7 asked the owner to pick the llama.cpp integration (O-6) and the Rust-native runtime (O-7), and to confirm D-021.
- **Decision:**
  - **O-6, a subprocess.** Each loaded GGUF model runs in its own `llama-server`, on a 127.0.0.1 port picked at start, with a per-start key the router sends. The key goes in `LLAMA_API_KEY`, not on the command line where every local user could read it. After start, the router checks that a request without the key is refused, and stops a server that does not check it.
  - **Which llama-server:** `[models] llama-server` (a build of your own, e.g. CUDA on Linux), else `TERMOS_LLAMA_SERVER`, else llama.cpp's official release for this machine, installed by mise's `github:` backend (`github:ggml-org/llama.cpp[asset_pattern=…]@latest`), as D-053 does for other tools. `[models] llama-cpp = "auto"` picks Metal on Apple Silicon, Vulkan where a GPU and the Vulkan loader are present (not on a Raspberry Pi), and the CPU otherwise; `cpu`, `vulkan`, `cuda`, `rocm` and `metal` override it. CUDA and ROCm are never automatic: the official releases have no Linux builds of either, and the Windows CUDA build needs CUDA's runtime libraries (a separate download) besides the driver, while Vulkan runs on the NVIDIA and AMD drivers alone. `termos model recommend` says so.
  - **O-7, candle first,** for embedding models only: BERT-family safetensors (MiniLM, BGE, E5) in-process on the CPU, mean-pooled over the attention mask and normalized (feature `candle`, on by default). Chat models are llama.cpp's job on every platform; candle chat models would duplicate it with fewer quantizations and no GPU offload, so they are not included. ONNX Runtime is not included.
  - **D-021 is followed as proposed** (the `huggingface_hub` cache, a separate router process). The owner confirms it at the M7 review.
  - **`[models]`** in config.toml: `cache`, `cache-limit`, `token` (`keyring:<name>`), `context` (4096), `llama-cpp`, `llama-server`, `memory-budget`, `port`, and `[models.servers.<name>]`.
  - **Correction to D-058:** reqwest built without a TLS crypto provider panics when a client is built, so a build without the `tls` feature builds no HTTP client at all, plain HTTP included, and says it has no TLS support. Only the clippy check for Windows run from Linux builds without it; termos-models has the same feature.
- **Alternatives:** FFI through `llama-cpp-2` (a C++ build inside ours, one accelerator per build, and a crash takes the router down); Ollama as the backend (another daemon with its own model store instead of the Hub's cache); ONNX Runtime (a C++ runtime to ship or download, for the same embedding models).

## D-066: Downloads, the cache, hardware detection and the fit estimate
- **Status:** Accepted (lead) · 2026-11-02
- **Decision:**
  - **The Hub:** its JSON API (`/api/models` search, a revision's commit, the recursive file tree with LFS SHA-256s), `HF_ENDPOINT` for another address. The token is `[models] token` from the secret store, else `HF_TOKEN`, and is sent only to the Hub's own address; gated and missing repositories get their own errors.
  - **The cache** is `huggingface_hub`'s layout (`[models] cache`, else `HF_HUB_CACHE`, else `HF_HOME/hub`, else `~/.cache/huggingface/hub`). A download goes to `blobs/<id>.incomplete`, resumes with a Range request, and is kept only if its size and SHA-256 (LFS) or git blob SHA-1 match; snapshots link to blobs (symlink, else hard link, else copy). Pulls pin a commit, so a moving branch never mixes files. Each snapshot records when it was last used. `termos model gc` removes interrupted downloads, unreferenced blobs and, over `--max` or `[models] cache-limit`, the least recently used snapshots.
  - **Hardware** (`termos_hw::accel`): CPU features, RAM, GPUs with free memory (NVML, then `nvidia-smi`; AMD through sysfs, ROCm when `/dev/kfd` exists), the Vulkan loader, Metal, the Pi model, Coral (its USB IDs, `/dev/apex_*`), Hailo (`/dev/hailo*`) and Cloud TPU chips (PCI devices with Google's vendor ID and an accelerator class).
  - **The fit estimate,** shown before any download: weights (file sizes) + KV cache (layers × context × KV heads × (key + value dims) × 2 bytes, from the GGUF header, which is read with Range requests, or `config.json`) + overhead (256 MiB + 5% of the weights), with 10% headroom. A model runs fully on the GPU, split across GPU and CPU (with the layer count llama-server gets), on the CPU, or not at all. `pull` without a file picks the best placement and, within it, the largest quantization. The arithmetic saturates: headers are untrusted input (fuzzed by `gguf_parse`).
- **Alternatives:** the `hf-hub` crate (its own client setup; the resume, checksum and header-reading logic was needed on top of it anyway); estimating from parameter counts in names (often absent or wrong).

## D-067: Embedding models run in-process with candle
- **Status:** Accepted (lead; implements O-7) · 2026-11-02
- **Decision:** candle 0.11 (`candle-core`, `candle-nn`, `candle-transformers`) and `tokenizers` 0.22 without default features (`fancy-regex` instead of Oniguruma, so no C library). A repository with `config.json` and safetensors weights is an embedding model; the router loads it on its first `/v1/embeddings` request on a blocking thread and keeps it like any other backend. The `paste` advisory (RUSTSEC-2024-0436, unmaintained, a build-time macro used by `gemm`) is ignored in deny.toml. The feature is off only in the clippy check for Windows run from Linux.
- **Alternatives:** llama-server with `--embeddings` (it needs GGUF conversions, which most embedding models lack); `fastembed` (ONNX Runtime underneath).

## D-068: The model router, its endpoint file, and agents' `local` provider
- **Status:** Accepted (lead; implements D-021) · 2026-11-02
- **Decision:**
  - **`termos models serve`** is hyper (HTTP/1) on 127.0.0.1 (`--port`, `[models] port`, else a free port) and optionally a Unix socket. Every request but `/health` needs the per-start bearer token (128 bits from the OS generator, compared in constant time). It serves `/v1/models`, `/v1/chat/completions`, `/v1/completions`, `/v1/embeddings` and `/admin/stop`, and streams responses through as they arrive.
  - **Model ids:** the repository for a repository with one model; `repo:quant` (`org/x-GGUF:q4_k_m`) or `repo:path` when it has several.
  - **Backends start on the first request** for their model, one at a time. Over the memory budget (`[models] memory-budget`, default 90% of RAM plus discrete VRAM), the least recently used are stopped first, and their processes are waited for so their memory is free. `[models.servers.<name>]` runs a command for the repositories it lists, with `{model}`, `{repo}`, `{port}`, `{token}` and `{cache}` filled in and `HF_HUB_CACHE`/`HF_HUB_OFFLINE=1` set; `termos models sidecar vllm|transformers` makes a uv venv (through mise) and prints such an entry. That is the Python sidecar for big NVIDIA and AMD GPUs and for TPU VMs.
  - **The endpoint file** `<runtime>/models.json` (0600, in the owner-only runtime folder, hidden from agents' commands) holds the URL, the token and the pid; the router writes it when listening and removes it when stopping.
  - **Agents:** a `local` provider without `base-url` reads the file, checks `/health`, and otherwise starts `termos --config <file> models serve --daemon` and waits for it (30 s). The token is added to the session's redactor. termos-ai depends on termos-core for the file, not on termos-models.
  - **Accelerator limits,** documented in docs/models.md and printed by `termos model recommend`: Coral runs only small int8 TFLite models (no LLMs); Hailo runs compiled HEF models through its own runtime (TermOS runs no LLMs on it); Cloud TPUs need the vLLM sidecar with TPU support.
- **Alternatives:** llama-server's own multi-model mode (recent, GGUF only: no candle models or sidecars behind the same endpoint); agents talking to each llama-server directly (no eviction, no embeddings, a port per model).

## D-069: D-021 is confirmed
- **Status:** Accepted (owner, at the M7 review) · 2026-11-02
- **Decision:** D-021 stands as implemented in M7 (D-065, D-068): models live in the `huggingface_hub` cache, and `termos models serve` is a separate process with one OpenAI-compatible endpoint that agents use like any other provider.

## D-070: A patched `gemm-common` until upstream fixes aarch64 Linux
- **Status:** Accepted (lead) · 2026-11-02
- **Context:** CI's aarch64 Linux job could not build candle (D-067): `gemm-common` 0.19.0's FP16 inline-assembly helpers lack the `fp16` target feature that the kernels calling them have, and where LLVM emits them out of line (debug builds) it rejects the instructions. No newer release exists. Turning FP16 on for the whole target is not an option: the Raspberry Pi 4's Cortex-A72 has no FP16.
- **Decision:** `third_party/gemm-common` is a copy of 0.19.0 with `#[target_feature(enable = "fp16")]` on those seven helpers, used through `[patch.crates-io]`. The kernels are chosen at run time only where the CPU has FP16, as before. `third_party/` is outside the workspace and the repo checks (its own licenses and markers), and its README lists each patch and when it can go: as soon as a `gemm-common` release has the fix.
- **Alternatives:** leaving candle out on aarch64 Linux (no embeddings on a Pi); `-C target-feature=+fp16` (binaries that can fault on ARMv8.0 CPUs).

## D-071: llama.cpp is installed from a pinned nightly build
- **Status:** Accepted (lead; supersedes the `@latest` in D-065) · 2026-11-02
- **Context:** CI showed that llama.cpp's latest GitHub release is now a versioned one (`v0.8.0`) with no binaries: since its `v0.x` releases, each names the nightly build (`b…`) that carries them.
- **Decision:** TermOS installs `github:ggml-org/llama.cpp[asset_pattern=…]@<build>` with the build pinned in `termos_models::llama::VERSION` (`b11960`, v0.8.0's), which TermOS moves as it is tested, as for jdtls (D-057). `[models] llama-cpp-version` installs another build. CI prints the pinned build's assets.
- **Alternatives:** resolving "the newest nightly with our asset" at run time (an untested llama.cpp on every machine, and a GitHub API call TermOS would make itself); reading `nightly-tag.txt` from the latest release (the same, one step removed).

## D-072: Bare-metal choices for M8: swash, BusyBox init, iwd + dhcpcd, GRUB2
- **Status:** Accepted (owner: O-9, O-10, O-11, O-14, the recommended answers) · 2026-11-02
- **Decision:**
  - **O-9, font rasterizer: swash.** Hinted outlines from TrueType/OpenType fonts, in pure Rust; bold from a bold face or by emboldening, italic by slanting. Box drawing, block elements and braille are drawn from geometry, so borders join exactly at cell edges. DejaVu Sans Mono (regular and bold, unmodified, Bitstream Vera license) is built into the KMS build so the console always has a font.
  - **O-10, init: BusyBox init.** `/etc/inittab` starts the boot scripts, then respawns TermOS's console on tty1; seatd, eudev and the network are plain init scripts. Moving to s6 later stays possible.
  - **O-11, networking: iwd + dhcpcd.** iwd (over D-Bus) for Wi-Fi, dhcpcd for addresses on wired and Wi-Fi links. A TermOS network settings page drives them.
  - **O-14, aarch64 UEFI bootloader: GRUB2,** as on x86_64, so both share one boot configuration and, in M9, one A/B fallback scheme (the GRUB environment).

## D-073: The bare-metal console is `termos --kms`, a client drawing with a CPU rasterizer
- **Status:** Accepted (lead; implements ARCHITECTURE.md §5.9) · 2026-11-02
- **Decision:**
  - **A client, not part of the server.** `termos --kms` connects to the server like the terminal client (starting one if needed) and attaches to the session `main`. If it crashes, the session goes on, and the next one (init respawns it) picks it up.
  - **Drawing:** `termos_render::raster` (feature `raster`) paints the cells a frame changed into a CPU framebuffer (`0x00RRGGBB`), with a glyph cache of cell-sized coverage masks. Two DRM dumb buffers take turns; each records the rows it missed, and only those are copied before a page flip. That works with every KMS driver (virtio-gpu, simpledrm, i915, amdgpu, vc4/v3d) and needs no GPU code. A full 1080p repaint takes about 7 ms on one x86 core; typing repaints one row. The mouse pointer uses the legacy hardware cursor ioctls where the driver has a cursor plane.
  - **Devices:** libseat (seatd in the images, logind elsewhere) opens the DRM and input devices without root, takes them away on a VT switch (Ctrl+Alt+F<n>) and gives them back (a full repaint). libinput reads keyboards and mice (through udev); XKB turns keys into characters with `[kms] keyboard-layout`, `-variant` and `-options`; key repeat is ours (`repeat-delay`, `repeat-rate`), on a timer only while a key is held.
  - **Fonts and size:** `[kms] font`, `bold-font` and `fallback-fonts`, else the built-in DejaVu Sans Mono (D-072). `font-size` is in pixels at 96 dpi, scaled in quarter steps for the monitor's density; Ctrl+= and Ctrl+- change it while running (Ctrl+0 resets), and the desktop is resized to fit.
  - **Colours:** default foreground `#d0d0d0`, background `#14161b` (reported to the server as the background, so the `auto` theme is dark), the VGA palette.
  - **Ready marker:** after its first frame is on the screen, the console prints `TERMOS-READY` on its standard output; the image's session script sends that to the system console, where the QEMU boot tests wait for it (D-076).
  - **The feature:** termos-cli's `kms` feature (Linux only) links libinput, libudev, libxkbcommon and libseat, so it is off in the hosted builds and on in the images; CI installs the libraries so `--all-features` covers it. `--kms` in a build without it says so.
  - **Not yet:** images are shown as blocks rather than pixels, there is no clipboard of the console's own and the bell is silent (`TODO(M9)`).
- **Alternatives:** the server driving the display itself (a display bug would take every session down); OpenGL ES through EGL/GBM (a GPU stack in the image for drawing text, and no output on simpledrm); the kernel's fbdev (deprecated, no page flips); a pure-Rust evdev reader instead of libinput (no pointer acceleration, touchpads or quirks).

## D-074: How images are built: Buildroot board defconfigs plus TermOS fragments, termos built outside Buildroot
- **Status:** Accepted (lead; refines D-014, D-015 for images) · 2026-11-02
- **Decision:**
  - **Buildroot 2026.02.1** (the 2026.02 LTS line), downloaded by `cargo xtask image build` and checked against a pinned SHA-256; until the hash is pinned the build stops and prints the download's hash next to buildroot.org's signed list.
  - **Each target is a Buildroot board defconfig plus TermOS fragments** (`crates/termos-image/br2-external/configs/`): `pc_x86_64_efi`, `aarch64_efi`, `raspberrypi5`, `raspberrypi4_64`, `raspberrypi3_64` and `raspberrypizero2w_64`. Buildroot keeps those boards building; the fragments add the Bootlin prebuilt glibc toolchain (D-014), BusyBox init, eudev, the packages, the overlay and the partition layout. Kconfig drops unknown or unmet settings silently, so after `olddefconfig` every fragment setting is checked and a dropped one stops the build by name.
  - **`termos` is built by cargo, not by Buildroot:** Buildroot's Rust is older than the workspace's. After `make termos-depends` (the toolchain and libinput, libxkbcommon, libseat and libudev in the sysroot), cargo builds `termos-cli` with the `kms` feature in release, with Buildroot's GCC as linker and C compiler and its pkg-config for the sysroot. A small `termos` package installs that binary. This replaces cargo-zigbuild (D-015) for images only: the console links C libraries that only the image's sysroot has.
  - **Tooling:** `crates/termos-image` is std-only (targets, the Buildroot driver, fragment checks, the QEMU harness), so `xtask` can depend on it and stay free of crates.io dependencies (D-020). `cargo xtask image list|build|boot`.
  - **CI:** `.github/workflows/images.yml` builds and boots the x86_64 and aarch64 virt images when the image, the console or the workflow changes, weekly and on demand (the Pi images on demand), and keeps the images, screenshots, serial logs and `legal-info` as artifacts.
- **Alternatives:** full TermOS defconfigs (copies of each board's settings to keep in step with Buildroot by hand); Buildroot's cargo package infrastructure (its Rust is too old); building with zigbuild against the sysroot (glibc symbol versions from two places).

## D-075: The image's layout and boot: a read-only root in slot A, a data partition, BusyBox init into the console
- **Status:** Accepted (lead; implements ARCHITECTURE.md §13 for M8) · 2026-11-02
- **Decision:**
  - **Partitions (MBR, disk signature `7e57c0de`):** 1. boot (the EFI system partition, FAT; on a Pi, the firmware's partition), 2. root slot A (squashfs, read-only), 3. root slot B (empty until M9's updates), 4. data (ext4, label `termos-data`). The kernel lives on the boot partition and mounts `root=PARTUUID=7e57c0de-02`.
  - **x86_64 boots both ways from one disk:** GRUB's `boot.img` in the MBR and its core image in the gap before the first partition for BIOS; the EFI system partition (type 0xEF, which UEFI firmware accepts on MBR disks) for UEFI. One `grub.cfg` serves both. aarch64 uses GRUB's EFI build (D-072). A hybrid ISO comes with the M9 releases.
  - **Boot:** BusyBox init runs the boot scripts (eudev, dbus, iwd, dhcpcd from their packages; TermOS's `S11termos-data` and `S70seatd`), then respawns `/usr/libexec/termos/session` on tty1: `termos --kms` as the user `termos` (groups `seat`, `video`, `input`), its output sent to the system console.
  - **Data:** `S11termos-data` mounts the data partition on `/data` and binds `/data/home` on `/home` and `/data/iwd` on `/var/lib/iwd`; it creates `dev-rootfs`, `models` and `containers` for later milestones. Without the partition it runs from memory rather than formatting a disk by guess. Growing the data partition to the whole disk on first boot is M9 work.
  - **Kernel:** a fragment on top of each board's configuration: KMS drivers (virtio-gpu, simpledrm, bochs; vc4/v3d on a Pi), virtio input, squashfs, ext4, FAT, `fw_cfg` in sysfs, user namespaces and Landlock, and the crypto iwd uses.
- **Alternatives:** GPT (BIOS boot from GPT needs `grub-bios-setup` to patch sector numbers, which an image generator cannot do); the kernel inside the root slot (GRUB reading squashfs; postponed to the A/B work, where it pays off); systemd (D-072).

## D-076: The QEMU boot test
- **Status:** Accepted (lead; implements ARCHITECTURE.md §13's tests) · 2026-11-02
- **Decision:** `cargo xtask image boot <target>` boots the image on a throwaway snapshot with virtio-gpu as the only display, a virtio keyboard and tablet, and the serial console in a log; x86_64 with OVMF and with SeaBIOS (KVM where available), aarch64 virt with AAVMF. It passes when:
  1. the console prints `TERMOS-READY` once its first frame is on the screen;
  2. a QMP `screendump` shows the desktop: not blank, with a status bar band at the bottom (kept as a PNG);
  3. `MemTotal − MemAvailable`, reported by the image after it settles, is under 150 MB (D-019). QEMU asks for that report through `fw_cfg` (`opt/termos/boottest`, the seconds to wait), so test runs use the same image as everyone else.
- **Not covered:** latency on a Pi 4 (measured on hardware by the owner); input in the image beyond the console starting.

## D-077: M9 is split; updates only on request; checksums and attestations; whole-disk installs
- **Status:** Accepted (owner) · 2026-11-02
- **Decision:**
  - **Split:** M9 becomes **M9, release** (A/B updates, the live ISO and installer, growing the data partition, the network settings page, the release pipeline with static musl builds, user docs, accessibility, the Pi latency work) and **M9b, platform gaps** (the Windows agent sandbox; VT input, images from programs in panes and saving at logoff on Windows; program names and directories for snapshots on macOS and Windows; GPU sensors on macOS and for non-NVIDIA GPUs on Windows; Kitty keyboard flags 2 and 4; app windows saved with sessions; the KMS console's own clipboard, bell and pixel images). Platform gaps are `TODO(M9b)` markers from now on; this supersedes the `TODO(M9)` convention of D-035.
  - **Updates on request only:** `termos update` and the Settings app check GitHub Releases for a newer bundle for this board, download it and install it into the other slot; the machine restarts when you say. Nothing checks, downloads or calls out in the background.
  - **Signing:** releases carry `SHA256SUMS` and GitHub's build provenance attestations (Sigstore, keyless; checked with `gh attestation verify`). No Apple or Windows code signing yet, so the quarantine and SmartScreen steps stay documented (D-041). Update bundles are signed with the owner's key, kept as CI secrets (D-078).
  - **Installer:** whole disks only.
- **Alternatives:** one M9; a check at every boot with a notification; bundle files only, without GitHub Releases; Developer ID notarization and Authenticode (a paid Apple account and a certificate); a minisign key; installing next to another system.

## D-078: A/B updates with RAUC (resolves O-12)
- **Status:** Accepted (owner: RAUC; lead: how it is wired) · 2026-11-02
- **Decision:**
  - **RAUC 1.15** (Buildroot's package) with its D-Bus service. Bundles use RAUC's `verity` format (dm-verity in the kernel) and are signed (X.509 CMS); an image accepts only bundles whose signature chains to its keyring and whose `compatible` is `termos-<target>`. TermOS downloads a bundle itself (rustls) to the data partition and hands RAUC the file, so RAUC needs no network support.
  - **Keys:** release builds sign with the owner's certificate and key (CI secrets) and put the owner's CA in the image's keyring. Without them, `cargo xtask image` makes a development CA, certificate and key in `target/image/keys/` (never committed); images built that way accept only bundles signed by that development key. `cargo xtask image bundle <target>` makes a target's bundle with Buildroot's host RAUC.
  - **Slots:** `rootfs.0` (boot name `A`) is partition 2 and `rootfs.1` (`B`) is partition 3, each a raw squashfs. **The kernel moves into the root file system** (`/boot`), the option D-075 put off, so an update replaces the kernel and the system together; the boot partition keeps the boot loader, its configuration and its state.
  - **GRUB (x86_64 and aarch64 UEFI):** RAUC's GRUB backend. `grub.cfg` is RAUC's documented selection script (`ORDER`, `A_OK`/`A_TRY`, `B_OK`/`B_TRY` in a `grubenv` on the boot partition, written on the system with `grub-editenv`); it loads the kernel from the chosen slot's squashfs and passes `rauc.slot=`. A slot that fails is tried once; then the other one boots.
  - **Raspberry Pi:** RAUC's custom backend over the firmware's tryboot. Each slot's kernel and device trees are on the boot partition under `slot-a/` and `slot-b/` (the firmware's `os_prefix`); a new slot is tried once through `tryboot.txt` (`reboot "0 tryboot"`) and becomes the default in `config.txt` when it is marked good. QEMU cannot run the Pi firmware, so this path is unit-tested only until the owner tries it on a board.
  - **Good or bad:** a boot script marks the booted slot good once the console's first frame is up (the console leaves a marker in its runtime folder). If that does not happen within five minutes, it marks the slot bad and restarts, and the boot loader falls back. The kernel restarts on a panic (`panic=10`).
  - **Privileges:** the desktop's user reaches root only through `termos system`, which sudo runs with one rule. It does a fixed set of things, with checked arguments: install a downloaded bundle, mark a slot good or bad, restart, power off, install to a disk (D-080), grow the data partition, add or forget a Wi-Fi network.
- **Tests:** in QEMU (x86_64): N+1 is installed from a local HTTP server and the machine restarts into slot B, running N+1; a broken bundle (a root file system without init) is tried once and the machine comes back on slot A.
- **Alternatives:** SWUpdate; RAUC's HTTP streaming (needs nbd and libcurl on the device); a D-Bus policy or a root daemon instead of sudo; kernels on the boot partition with per-slot names.

## D-079: Hosted Linux binaries in two flavours: glibc and static musl (resolves O-13)
- **Status:** Accepted (owner) · 2026-11-02
- **Decision:** test builds and releases add static `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` binaries (cargo-zigbuild) next to the glibc 2.31 ones (D-015), as long as the musl build keeps the performance budgets (D-019), measured in CI with `cargo xtask perf` against it. If musl's allocator makes it miss one, the musl build gets another global allocator; the measurements and the choice are recorded when that happens. A static binary cannot load libraries, so NVIDIA readings (NVML, loaded at run time) are missing there; the docs say so.
- **Alternatives:** glibc only.

## D-080: The live ISO and the installer; the data partition grows on first boot
- **Status:** Accepted (owner: whole disks; lead: how) · 2026-11-02
- **Decision:**
  - **The x86_64 ISO** is a hybrid image (CD, USB stick or virtual machine; BIOS and UEFI) that runs the same system from memory: the root file system as an initramfs, with the data in memory (`S11termos-data`'s fallback). It also carries the disk image (zstd). It is made by `xorriso` after the disk image, because Buildroot's own ISO step runs before the image exists.
  - **`termos install`** lists the disks it may use (never the one it booted from), asks you to type the disk's name, writes the disk image to it (streamed through zstd, through `termos system`) and asks you to restart without the ISO. `--erase <disk>` does it without asking, for automation.
  - **Growing:** on the first boot of any image (installed, written with `dd` or Raspberry Pi Imager, or a virtual disk made bigger), the data partition is grown to the end of the disk, and its file system with it, before it is mounted.
  - The aarch64 UEFI and Pi images are written to their disk or SD card directly.
- **Tests:** in QEMU: the ISO boots with an empty 8 GB disk, installs onto it with `--erase`, and the disk then boots alone to `TERMOS-READY` with its data partition grown.
- **Alternatives:** the live system from a squashfs on the ISO (a second, smaller initramfs to find and mount it); `dd` instructions instead of an installer.

## D-081: The network page is a Network app over sysfs and iwd's D-Bus interface
- **Status:** Accepted (lead; implements D-072's settings page) · 2026-11-02
- **Decision:**
  - **A Network app** (`network.open`, "Network" in the palette), next to Settings rather than inside it: Settings edits `config.toml`, the network is the system's.
  - **Links:** every interface but loopback, from `termos_hw::net`: wired or Wi-Fi and up or not from sysfs, addresses and hardware addresses from `sysinfo` (so it also works hosted, without Wi-Fi).
  - **Wi-Fi:** where iwd runs, its state, networks (strongest first) and scans through its D-Bus interface with zbus, already in the build for the keyring; the desktop's user is in `netdev`, which iwd's policy lets in. Joining a network writes iwd's file for it (`/var/lib/iwd/<name>.psk` or `.open`, root only, on the data partition) through `termos system wifi-add`, the passphrase on its standard input and in `secrecy` types throughout; iwd joins it whenever it is in range. Forgetting removes the file. Enterprise (802.1X) networks are listed but not joined from the page yet.
  - **Idle cost:** it reads on open and every five seconds only while on screen (D-028).
- **Tests:** a snapshot of the page over a fixed backend, passphrases masked and checked, saving and forgetting files in iwd's naming. QEMU has no Wi-Fi, so iwd itself is not exercised in CI.
- **Alternatives:** `iwctl`'s text output; an iwd agent answering passphrase requests over D-Bus; a page inside Settings.

## D-082: The accessibility review; the console measures its own latency
- **Status:** Accepted (lead; M9's accessibility and Pi tuning deliverables) · 2026-11-02
- **Decision:**
  - **Contrast:** every preset's text colours (text, muted, accent, warnings, errors, status text) are tested against the backgrounds they are drawn on (overlays, the status line): 7:1 (WCAG AAA) for high contrast, 4.5:1 (AA) for dark and light. The review moved three colours to pass: the dark preset's muted text `#7d8590` → `#8b949e` (4.0 → 4.9:1), the light preset's accent `#0969da` → `#0860ca` and warnings `#9a6700` → `#8a5c00` (on the status line 4.4 and 4.2 → 5.1 and 5.0:1).
  - **Selected rows** draw all their text in `fg` (`ThemeColors::selected`), in every app: on a selection colour still visible against black, no red or orange reaches 7:1, and `fg` does (9.5:1 in high contrast).
  - **The console's text size:** 6 to 96 pixels (×1 to ×3 for dense screens), checked in `config.toml`, changed with Ctrl+=/−/0.
  - **Input→frame time on a board:** the console times each key from its reading to the page flip after the desktop's next frame and logs the median, p99 and worst every 100 keys, so the Pi 4's budget (33 ms, D-019) is measured on the device by whoever has one, without a harness.
- **Alternatives:** a darker selection for high contrast only (still not 7:1 for red); measuring latency with an external camera rig.

## D-083: The Updates window; the console's latency is D-019's measure; release rehearsals
- **Status:** Accepted (lead; supersedes D-082's input→frame bullet) · 2026-11-03
- **Decision:**
  - **The Updates window:** `update.open` ("Updates" in the palette, and Settings → System → Updates) opens a floating pane running `termos update install --wait`: it says whether a newer release exists, asks before installing, offers the restart and waits for Enter, so the window is the same program as the command line rather than a second implementation. Updates stay on request only (D-077).
  - **The console's input→frame time** runs from reading a key to asking the display to flip in a buffer that holds the frame answering it, which is D-019's "flushing the frame"; the time until the flip completes (up to a refresh later) is logged beside it. D-082 timed to the completed flip, which adds a refresh interval the budget does not count.
  - **What the console copies:** the changed pixel rows as up to eight bands rather than one span, so the cursor at the top and the clock at the bottom do not copy the screen between them; on a little-endian machine each row is a plain memory copy.
  - **A slot that works but cannot be recorded:** when the console started but RAUC cannot mark the slot good (its boot loader's state is out of reach), S98 says so (`unrecorded`) and keeps running; restarting would not help. It restarts only when the console did not start.
  - **Release rehearsals:** the `release` workflow also runs by hand, and when a push changes it or a patched crate (`third_party/`) on any branch: the same builds and packages as a release, kept as the run's artifact, unattested, images only when asked for and with development keys when the owner's are not set.
- **Alternatives:** an update page drawn by the desktop itself (a second copy of `termos update`'s logic); timing latency with an external camera rig; a tag on a throwaway repository to test releases.

## D-084: M10 owner answers: a mouse-first desktop with `tos`, graphics, a command center, Claude Code and Codex
- **Status:** Accepted (owner) · 2026-11-03
- **Context:** after M9 the owner asked for a friendlier, mouse-first desktop with commands in every terminal on every OS, fancier graphics where they fit, a command center over the AI and the rest of the OS, and native Claude Code and Codex CLI.
- **Decision:**
  - **M10, in four parts, next** (before M5b and M9b), each with its own review stop: M10a the mouse-first desktop and pane commands, M10b graphics, M10c the command center, M10d Claude Code and Codex CLI.
  - **Pane commands:** one `tos` command with subcommands (`tos files .`, `tos monitor`, `tos notify …`), the same in PowerShell, cmd, bash, zsh and fish on every OS, so it never clashes with system commands.
  - **Command center:** observes and acts, with history: live views plus a local history on disk (secrets never stored), and actions such as stopping an agent, killing a process, approving a pending permission or opening the related window.
  - **Claude Code and Codex CLI:** TermOS installs them from their official sources on every platform, including bare metal, and runs a TermOS MCP server so they can use TermOS's features with permission; their usage shows in the command center. They are not shipped inside the images.
- **Alternatives:** short separate commands (clashes such as macOS `open`); a read-only or live-only command center; installing the agents without the MCP server; preinstalling them in the images (their licenses may not allow redistribution); M10 after M5b and M9b.

## D-085: Menus, hover and double clicks; `tos` is a name of the `termos` binary
- **Status:** Accepted (lead) · 2026-11-03
- **Decision:**
  - **The TermOS menu:** a `◆ Menu` start button at the left of the status line (also prefix `Enter`, the `menu` command and `tos menu`) opens one menu of everything: new terminals, the apps, AI agents (TermOS's own and the external presets), project tasks, layouts, themes, workspaces and sessions, help, detach and quit. Submenus open beside on hover. Every item is a named command and shows its key, so the menu also teaches the keyboard.
  - **Context menus:** a right-click in a terminal or app pane, on a floating window's title bar or on a workspace tab. In programs that use the mouse, Shift+right-click, as Shift already keeps selection for TermOS. Menu items act on the pane or tab clicked (it is focused or selected first).
  - **Behaviour:** menus are per client and modal; the pointer selects, a press runs (left or right button, so a context menu can be used in one gesture pair), a press elsewhere closes without passing the click on.
  - **Hover:** motion events (already reported, 1003) redraw only when what is under the pointer changes; the status line's targets and window buttons are highlighted. Idle cost stays at zero (D-028).
  - **Double clicks:** two presses on the same cell within 400 ms. A floating title maximizes and restores, a new `pane.maximize` command (the size before is kept in memory); a tab opens its rename prompt. `pane.rename` names panes (also `tos title`); the name is not saved with snapshots yet.
  - **`tos`:** the `termos` binary run under the name `tos` (still one binary: D-002 makes `termos-cli` the only one). The server makes `<data>/bin/tos` (a symbolic link on Unix; a hard link, else a copy with a stamp of the source's size and time, remade when stale, on Windows) and puts that folder first on panes' `PATH`. Verbs act on `$TERMOS_PANE` through new control methods (`pane.command`, `pane.open`, `pane.rename`, `clipboard.copy`, `clipboard.get`); unknown verbs run as `termos` subcommands. Web addresses go to the system's opener (`xdg-open`, `open`, `start`).
- **Alternatives:** shell functions injected into each shell (fragile across bash, zsh, fish, PowerShell and cmd); a separate `tos` binary (two binaries to ship and update); `.cmd` shims on Windows (cmd quoting breaks arguments from PowerShell); releasing a menu item on button release (a right-click's release would hit the first item of the menu it opened).

## D-086: On Windows the server runs from its own copy of termos.exe
- **Status:** Accepted (lead) · 2026-11-03
- **Context:** the owner's `cargo install` failed with "Access is denied" replacing `termos.exe`: Windows does not replace an executable a process runs from, and the server (and, since D-085, `tos.exe`, a hard link to the server's executable) outlives every terminal.
- **Decision:** on Windows the client starts the server from a copy in `<data>\bin\server\termos-<size>-<mtime>.exe`, made when that build has no copy yet; copies of older builds are removed when nothing runs from them. If the copy cannot be made, the server runs from `termos.exe` as before. `tos.exe` links to the server's copy, so it no longer holds `termos.exe` either. An attached client still runs from `termos.exe`; the Windows notes say to detach first, or to rename the running file aside (which Windows allows) before reinstalling from inside TermOS.
- **Alternatives:** the client relaunching itself from a copy (two processes per attached terminal); a TermOS installer command that renames the running file aside (users install with cargo or by unzipping, which TermOS does not run); leaving it to `termos kill-server` (it ends the sessions the user may be installing from).

## D-087: M10b owner answers: Unicode icons by default; a wallpaper by default
- **Status:** Accepted (owner) · 2026-11-03
- **Decision:** icons use Unicode symbols by default, so they work in every font; Nerd Font glyphs and plain text are selectable. Empty desktop areas show a wallpaper by default.
- **Alternatives:** Nerd Font glyphs by default (they show as boxes without a patched font); no wallpaper unless configured.

## D-088: Icons and gradients: one cell per icon, gradients only in truecolor, never at the cost of contrast
- **Status:** Accepted · 2026-11-03
- **Decision:** icons come from `termos_render::icons`, one `Icon` per thing (an app, a menu command, a kind of file) and one glyph for it per set: `unicode` (the default, D-087), `nerd` (Font Awesome 4 code points, which every Nerd Font has), `ascii` and `none`. Every glyph is one cell wide and none can turn into a colour emoji, so layouts never shift between terminals; tests check both. Each client has its own set, from `[ui] icons`. Menus line their labels up behind a two-column icon gutter when a menu has icons, and draw none with `none`. Gradients (`[ui] gradients`, on by default) fill the focused window's title bar, the start button and the active tab, but only for a client whose terminal shows truecolor: 256- and 16-colour clients get the same bars flat, in the theme's own colours, so the encoder's palette mapping stays exact. A gradient starts at the theme colour and moves away from the lightness of the text drawn on it (`Theme::sheen`), so the text's contrast at the far end is never below its start; a test checks every preset. The focused window's title bar is filled (the accent, its text in the panel colour), and unfocused windows keep the line border, which makes focus visible without colour alone.
- **Alternatives:** fading bars into the panel colour (it looked softer, but the dark preset's far end fell to about 2:1 with the title text); Nerd Font icons from the newer Material ranges (not in every Nerd Font version); emoji (two cells in most terminals, one in some); gradients dithered to 256 colours (banding, and many more bytes per frame).

## D-089: The wallpaper, the splash and About TermOS
- **Status:** Accepted · 2026-11-03
- **Decision:** the wallpaper is drawn wherever the active workspace has no tiled panes (only floating windows, or none showing). It holds a column of shortcuts at the top left (Terminal, Files, Monitor, AI, Settings, Help), which a click runs and the pointer lights up, and the logo and a hint at the bottom right; a floating window opens in the middle, so both stay in sight. A right-click on it opens the TermOS menu at the pointer. `[ui] wallpaper = "pattern"` (the default) adds a sparse dot lattice, with a faint accent glow in truecolor; `"none"` draws nothing; any other value is a picture's path (`~` and paths relative to the config file work). The picture is decoded once on its own thread, within the image viewer's limits, and shown by the image layer like any other image (pixels where the terminal can, blocks elsewhere), right of the shortcuts and under the floats; if it cannot be read, a warning says so and the pattern shows. The decode wakes the desktop through the PTY event queue with an id reserved for it, so nothing polls. The splash is the logo on a card for 900 ms when a client attaches, drawn in from the left; it shows only to that client, only where it animates (D-042), and any key, click or paste ends it at once without being swallowed. `[ui] splash = false` turns it off. About TermOS (Help menu, `about.open`, `tos about`) shows the version, the system, where the config, logs and data are, the licence and the source.
- **Alternatives:** empty workspaces as a desktop of their own (every workspace has a pane today, and changing that touches focus, sessions and snapshots); a centred logo and shortcuts (a new floating window covers them); a splash that waits for a key (it would get in the way of attaching from scripts); decoding the picture on the server loop (a large file would stall every pane).

## D-090: Pixel charts, spinners and shaded meters
- **Status:** Accepted · 2026-11-03
- **Decision:** an app can report an image as `pixels_only`: it has drawn a text version beneath (the monitor's braille CPU chart), and the picture is the same data in pixels (`termos_render::graphics::chart::area_chart`: a smooth line, a soft fill fading downwards, faint quarter guides, on a transparent background). For each client the desktop decides as for any image: where the picture shows in pixels, the cells beneath are cleared so the text version cannot show through; where it would fall back to blocks (block-only terminals, Sixel or iTerm2 when covered or moving), the picture is skipped and the text version stays, which reads better than blocks of a chart. The monitor makes the picture again only when a reading arrives, the plot changes size or its colour changes level, so an idle screen sends nothing. Apps say when they are `busy()` (the monitor before its first reading, a folder or picture loading, the agent waiting on the model); the desktop then draws a spinner in place of the window's icon. It turns every 100 ms only while a busy window is on screen and some client animates (D-042); others see it still. `App::catch_up` lets apps take in finished background work before the title is drawn. Meters (the monitor's bars) shade along their length in truecolor, from the accent through the warning colour to the error colour at the levels where a flat meter changes colour; 256-colour terminals keep the flat colours.
- **Alternatives:** charts in blocks where pixels are not available (coarser than braille); apps keeping their own spinner timers (each would tick even with no client animating, and over SSH); pixel charts at the terminal's own cell size (apps do not know each client's; the image layer scales the picture instead).

## D-091: Sessions start on the desktop (owner review of M10b)
- **Status:** Accepted (owner) · 2026-11-03
- **Context:** on Windows the owner saw one full-screen PowerShell and none of M10b's desktop: the wallpaper only showed where a workspace had no tiled panes, and every workspace began with one tiled terminal.
- **Decision:** `[ui] start = "desktop"` is the default: a new session's and a new workspace's first terminal opens as a floating window over the wallpaper, focused. The wallpaper's Terminal shortcut opens another floating window (it used to split, which tiled over the desktop). Splitting, Tile and `C-b W` still tile. `[ui] start = "terminal"` (also in Settings) gives the full-screen terminal of before. Restored sessions keep their saved layouts. Terminal titles that are a program's path, as Windows consoles set them (`C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe`), show as the program's name (`powershell`) in tabs, window titles and the terminal's own title.
- **Alternatives:** a full-screen terminal on the first workspace and the desktop on new ones; keeping the full-screen terminal with the desktop behind a setting.

## D-092: M10c owner answer: the machine is sampled every minute, also while the command center is closed
- **Status:** Accepted (owner) · 2026-11-04
- **Decision:** the command center's history records CPU, memory and network every minute in the background, so its charts cover the whole history (7 days by default). This is a deliberate exception to "no timers while idle" (D-028): one thread wakes once a minute for about a millisecond. `[center] sample = "5m"` samples less often and `"off"` not at all; `[center] keep-days` (1 to 365) sets how long history is kept. AI activity is recorded as it happens, with no timer.
- **Alternatives:** sampling only while the command center or the system monitor is open (no idle timers, gaps in the charts); every 5 minutes by default.

## D-093: The command center: a journal, a live agent registry, its own crate
- **Status:** Accepted · 2026-11-04
- **Decision:**
  - **History:** `termos_core::journal` writes one JSON line per record, one file per UTC day, under `<state>/center/` (private files), and deletes days past `keep-days` once a day. Records are numbers and names only: a system sample; one agent turn's tokens and cost (by provider, model and session); a tool call's tool and decision; a session's start and end; an action taken from the command center. Never prompts, tool arguments or anything typed, so no secret can be in it (the agent test's canary check covers the journal). Agent sessions write to it as things happen; the sampler thread every minute (D-092).
  - **Live agents:** `termos_ai::registry` lists the sessions running in the server, by weak reference, with their state, usage, cost and pending call. The command center stops a session, approves a pending call once, or denies it, through the session's own input channel, so the agent window shows what happened.
  - **Crate:** a new `termos-center` (depending on ai, models, hw, core and render) holds the window, its data and the sampler. The `termos` binary's launcher opens it (`LaunchKind::Center`); the shell stays free of the AI crates.
  - **Data:** the window gathers on a thread of its own every 2 s while it is on screen (the history every 30 s or when its range changes): the machine (termos-hw), sessions and panes through the server's own control API, the audit logs (each verified once per size), the newest log's last lines, the model cache and whether the router answers, MCP servers and CLI agents from the config. A new control method `pane.focus` brings a pane forward.
  - **Window:** tabs Overview, AI, System and History; every action is a button and a key: approve, deny, stop, end a process, go to or close a pane, open the related window (audit log, agent, network, updates, monitor, log). Ending a process and closing a pane ask first; ending sends SIGTERM (TermOS itself and pid 1 are refused; Windows terminates). Charts span the chosen range (1 h, 24 h, 7 days) and start where the data does; the CPU history is also a picture where the terminal shows pixels (D-090). It opens with `center.open`, prefix `C`, the menu and `tos center`.
  - **Cost** comes from the prices the agents already know (configured, or listed by the provider); no model or price is built in.
- **Alternatives:** SQLite for the history (a C dependency for a few megabytes of append-only records); deriving history from saved conversations and audit logs alone (no per-turn timing, no system load); the window reading the desktop's state directly (it runs in the shell's process but lives in a crate the shell cannot depend on; the control API is already public and tested).

## D-094: M10d owner answers: verified binaries, the CLI asks, their usage as numbers
- **Status:** Accepted (owner) · 2026-11-04
- **Decision:**
  - **Install:** TermOS downloads each vendor's official native binary itself and checks it before use: Claude Code from Anthropic's release site (the release's `manifest.json`, with a SHA-256 for every platform), Codex CLI from OpenAI's GitHub releases (GitHub's SHA-256 digest of each asset). They live in TermOS's data folder, are updated with `termos agent update`, and need no Node.js and no downloaded script; the same works on the bare-metal images.
  - **Permission:** Claude Code and Codex ask before each MCP tool call, as they already do. TermOS's MCP server enforces the user's allow and deny rules on top and audits every call; it does not ask a second time.
  - **Usage:** the command center reads token counts, models and timestamps from their local session logs (`~/.claude/projects`, `~/.codex/sessions`), never the conversation text; a setting turns it off.
- **Alternatives:** npm packages with Node.js from mise; running the vendors' install scripts; TermOS asking too (two prompts per action); showing only their windows and their calls to TermOS.

---

## D-095: Claude Code and Codex CLI in TermOS: installer, MCP server, usage
- **Status:** Accepted · 2026-11-04
- **Decision:**
  - **Installer:** the system's `curl` (as for mise, D-053), into `<data>/agents/<name>/<version>/` with a link in `<data>/bin`; older versions are removed. Downloads go through a `Fetch` trait so tests use recorded release data. Codex's Linux builds are the static musl ones, which also run on glibc. `NAME@VERSION` pins a release. Claude Code's manifest signature (GPG) is not checked: the manifest comes over HTTPS from Anthropic's site, as its own installer trusts it.
  - **MCP server written by hand:** MCP over stdio is a few JSON-RPC methods (`initialize`, `ping`, `tools/list`, `tools/call`); rmcp's server side would add schemars and its macros. Protocol compatibility is tested with rmcp's client against the real binary. It handles requests one at a time.
  - **Tools:** eight, chosen to work with TermOS rather than duplicate the CLIs' own file and shell tools. Pane text returned to the agent goes through the redactor. `send_to_pane` refuses the agent's own pane. `run_command` refuses quitting, detaching and closing by default (`deny-commands`).
  - **Decisions recorded:** the CLI's prompt is the user's approval (D-094), so calls TermOS lets through are `allowed`, and calls its rules stop are `denied`.
  - **Registration:** on the first install, if `[agents.termos-mcp] register` (default on); `termos mcp register|unregister` for CLIs installed otherwise. Claude Code through its own `mcp add --scope user`; Codex by editing `config.toml` (toml_edit keeps the rest of the file).
  - **Claude Code's updater** is turned off for TermOS's copy (in its window and in panes started after the install), so updates go through the checked path.
  - **Their usage:** their logs are internal formats, not interfaces: the reader takes only the fields it needs (message ids for de-duplication, model, usage numbers, timestamps), skips what it does not understand, and reads each file from where it stopped. Their costs are not computed (subscriptions have none per token); tokens are shown.
  - **CI** installs both for real on every platform (`agents.yml`) and checks each MCP handshake in TermOS's audit log; Codex connects through `codex exec` against a stand-in model server that refuses every request.
- **Alternatives:** rmcp's server side; a wrapper script for each CLI (to set its environment); asking the vendors' CLIs for usage (they have no such command for both).

---

## D-096: M11 owner answers: accounts and usage, skills, the multi-tool harness
- **Status:** Accepted (owner) · 2026-11-04
- **Context:** the owner asked for one app with the usage of every AI coding subscription, several accounts of the same provider (three Claude subscriptions, say) with quick switching, skills that teach Claude Code, Codex and the rest to use TermOS, and one coding tool over all of them that routes each task to the model that suits it, cheaply and well. Split into M11a (accounts, switching, the usage app), M11b (skills) and M11c (the harness).
- **Decision:**
  - **Usage (M11a):** tokens and models per account from each account's own session logs (exact, since every account has its own folder); plan-limit gauges where the CLI records them itself (Codex writes its 5-hour and weekly percentages into its logs); for Claude Code, use in 5-hour and weekly windows against a budget the user sets per account. TermOS never reads their login tokens and calls no vendor usage endpoint.
  - **Switching (M11a):** each account is a profile in its own folder (Claude Code's `CLAUDE_CONFIG_DIR`, Codex's `CODEX_HOME`), signed in once. The user switches (menu, palette, `termos account use`): new windows use that account, and a running agent window can restart under the other account and continue the same conversation. Switching is always the user's choice: no automatic rotation to get around plan limits, which can break the providers' terms.
  - **Harness (M11c):** one TermOS coding window that plans the work and hands each task to the backend that suits it: Claude Code or Codex headless under the user's subscriptions, or a model through an API key (TermOS's own agent). Routing rules are settings; every hand-off is visible and audited.
  - **Routing (M11c):** research on each provider's current models, with sources, goes into a guide. Defaults map task types (planning, coding, review, tests, search, small edits, docs) to tiers (frontier, balanced, fast) per provider; the model behind each tier comes from the provider's model list and the user's settings. Model names stay out of the code (CLAUDE.md).
- **Alternatives:** querying the vendors' limit endpoints with each account's login; swapping the one default login in place; a harness over CLIs only, or API models only; defaults that name models.

---

## D-097: AI accounts: profiles, shims, carried conversations, the usage app
- **Status:** Accepted · 2026-11-04
- **Decision:**
  - **Accounts are folders the CLIs already support:** `CLAUDE_CONFIG_DIR` and `CODEX_HOME`, each holding its own login (the macOS Keychain and Codex's keyring entries are keyed to the folder), settings and conversations, in `<data>/accounts/<cli>/<name>` (private). `default` is the CLI's own folder. An index (`<data>/accounts/accounts.toml`) keeps labels, budgets and the account in use. A new account links the default's settings, instructions and skills (copies where links are not allowed); Codex's `config.toml` is copied, as TermOS registers its server per account. Names and folders do not change after creation (a login can be tied to the folder's path); labels do.
  - **Shims:** `<data>/bin/claude` and `codex` are links to `termos`, which answers to its name: it picks the account (pinned with `TERMOS_ACCOUNT_<CLI>`, else the one in use), sets the folder variable and becomes the real program (TermOS's install, else the next on `PATH`, never itself). A folder variable the user set wins. The shims exist only while the CLI does. This replaces the pane-wide `DISABLE_AUTOUPDATER` of D-095 (the shim sets it for TermOS's copy).
  - **Carrying a conversation:** the newest conversation for the folder is copied into the other account (Claude Code: the transcript, its sub-agent and tool-result folder and its file snapshots, resumed by path; Codex: the session file, resumed by id). Copies, never moves.
  - **The shell lists and switches accounts through the launcher** (`accounts()`, `use_account()`), so termos-shell still does not depend on termos-devkit; account tasks run `termos account …` in windows. termos-center now depends on termos-devkit (downward).
  - **Limits:** Codex's from its logs (`rate_limits` on `token_count` lines, every shape Codex has used); Claude Code's only from what it passes its status line (`rate_limits.five_hour`, `seven_day`), recorded by `termos agent statusline` once the user turns it on. A status line of the user's own is chained, never replaced. Budgets are gauges; nothing is enforced.
  - **Tokens for budgets** are input, output and cache writes; cache reads are shown apart. Claude Code's 5-hour window is counted from the hour of the first message after the last window ended.
- **Alternatives:** swapping one login in and out of the default folder; reading login tokens to ask the vendors' usage endpoints (declined, D-096); a wrapper script per CLI instead of argv-dispatched links; moving conversations rather than copying.

---

## D-098: M11b: skills for the agent CLIs, and `termos agent ask`
- **Status:** Accepted (owner answers) · 2026-11-04
- **Context:** M11b teaches the coding CLIs what TermOS offers. The owner chose (2026-11-04): skills for Claude Code (each account's own skills folder) and for every CLI that reads the shared Agent Skills folder `~/.agents/skills` (Codex, Gemini CLI, opencode); installed and updated automatically; and a headless way for those CLIs to hand work to TermOS's own agents now, which M11c builds on.
- **Decision:**
  - **Skills, written by TermOS and built into the binary:** `termos` (working in TermOS: panes for servers and watchers, reading them, opening files, notifications, the `tos` command and the MCP tools), `termos-dev` (languages, project templates, build/run/test tasks, the editor and Git UI) and `termos-agents` (asking TermOS's agents and local models, accounts and usage). Each is a folder with a `SKILL.md` (`name` and `description` front matter, which every CLI that follows the Agent Skills format reads). They tell the CLI to check that it runs inside TermOS (`TERMOS` set) before using it.
  - **Where:** each Claude Code account's `skills` folder (an account that links the default's folder is covered once: folders are compared by their real path), and `~/.agents/skills`, read by Codex, Gemini CLI and opencode. opencode also reads `~/.claude/skills`, so it may list a TermOS skill twice; the copies are the same.
  - **Ownership:** each installed skill folder holds a `.termos-skill` file with the SHA-256 of every file TermOS wrote. TermOS updates a skill only when its files still match those sums (an unchanged copy of an older version), never touches a folder of the same name without that file (the user's own skill), and leaves a copy the user edited alone (`termos skills install --force` replaces it). Removing deletes only TermOS's unchanged copies.
  - **When:** automatically with `termos agent install` and `update`, when an account is added, and when the server starts (a few small file reads, once). Claude Code's folders only while Claude Code is there (TermOS's install or on `PATH`); the shared folder only while Codex, Gemini CLI or opencode is. `[agents.skills] install = false`, or `termos skills remove` (which TermOS remembers until `termos skills install`), turns this off. `termos skills status` shows every folder and copy.
  - **`termos agent ask`:** runs TermOS's agent once, in the calling process, and prints its answer: `--profile`, `--provider` and `--model` choose the model (a local one through the model router costs nothing), the prompt comes from the arguments or standard input, `--json` adds the model, tokens, cost and the saved conversation's id. It is read-only: the model is offered only reading, listing and searching files; any call that would need the user's approval is refused (no one is there to ask), and no MCP servers start. Every call is audited, the conversation is saved (it can be continued in a window), and its tokens and cost go into the command center's history. A question it would have to ask (which provider, which model, the secrets file's passphrase) is an error that says what to pass instead.
  - **No new MCP tool for asking:** an answer can take minutes, longer than Codex's MCP call timeout (60 s by default); the CLIs run `termos agent ask` as a command instead, which they can also run in the background.
  - **CI** (agents.yml) checks that the real Claude Code and Codex load the skills: each is run once against a stand-in model server on the runner (a fixture key, no account), which records the requests, and the requests must list the TermOS skills.
- **Alternatives:** each CLI's own folder for Gemini CLI and opencode (duplicates for opencode); installing only on request; asking through the server (it holds the unlocked secrets file, but would need deferred control replies across the launcher seam; M11c's harness runs inside the server anyway); skills generated from the user's configuration (they would change with every setting and leak its names into the CLIs' prompts).

---

## D-099: M11c owner answers: the harness
- **Status:** Accepted (owner) · 2026-11-04
- **Context:** M11c builds the harness of D-096: one coding window over Claude Code, Codex CLI and API models. Four choices were left open; the owner chose (2026-11-04):
- **Decision:**
  - **Planning:** a planning model splits the goal into typed tasks (planning, coding, review, tests, search, small edits, docs); the user sees each task with the backend and model it will go to, can edit, reorder or drop tasks, and starts the run. A setting can let small plans start without approval.
  - **Parallelism:** tasks that change files run one after another in the project folder; read-only tasks (review, search, planning, questions) run alongside them.
  - **Permissions of headless CLIs:** they may edit files in the project and run only the commands the settings allow (by default the project's build, test and lint tasks). Codex runs in its own workspace-write sandbox; Claude Code with `acceptEdits` and that allowlist. Anything else is refused and reported.
  - **Preference:** subscriptions first: Claude Code and Codex under the user's plans before API models, which are used when the rules say so, when no CLI is there, or as a fallback when a CLI fails or reaches its plan limit. Never another account of the same plan by itself (D-096).
- **Alternatives:** plans that start at once, or tasks written by the user; parallel coding in git worktrees, or one task at a time; asking the user for each tool call, or full auto in a sandbox; cheapest-first or rules-only routing.

---

## D-100: The harness: plans, routes, backends, the run
- **Status:** Accepted · 2026-11-04
- **Decision:**
  - **A crate of its own, `termos-harness`** (on ai, devkit, core, render, like termos-center). The binary hands it the CLIs through a `Programs` seam (the shims' logic: TermOS's install or `PATH`, as the account) and the shared agent pieces (the server's secrets slot, journal), and opens its window through the launcher (`LaunchKind::Harness`), so the shell still depends on neither.
  - **Plans are JSON from the planner:** the planning route's backend, read-only, is asked for `{"summary", "tasks": [{"id", "kind", "title", "prompt", "depends_on"}]}`. The first JSON object of its answer is taken (models add prose and fences), and checked: known kinds, unique ids, dependencies on earlier tasks only (others dropped), at most `max-tasks`. Edited plans are checked the same way. The parser and the CLI event readers are fuzzed (`plan_parse`, `cli_events`).
  - **Seven kinds, three tiers, no model names in code.** Defaults from the research ([docs/model-guide.md](docs/model-guide.md)): planning and review frontier; coding and tests balanced; search, small edits and docs fast. Backends fill tiers with the models the user sets; an unset tier is the CLI's own default model, or for an API backend the nearest tier that has one.
  - **Backends and order:** `claude`, `codex` and one API backend per provider (more under `[harness.backends]`, e.g. another account), tried in `[harness] order`, by default subscriptions first (D-099); a route may list its own. The route is worked out when a task starts, passing over a backend that cannot run, an API backend once the plan's budget is spent, and every backend of a CLI that reached its plan limit in the run: never another account of it by itself (D-096). The research found that a plan's limit is shared by all its models, so the fallback is the next backend (an API one, or the other CLI), not another model on the same plan.
  - **Reviews by another backend** (`cross-review`, on): a review tries first the backends that did not write the code it reviews. The evidence is indirect (models favour their own output; no code-review study was found), so the guide says so and the setting turns it off.
  - **Permissions without anyone to ask (D-099):** Claude Code `-p --output-format stream-json`, with `--permission-mode acceptEdits` and `--allowedTools Bash(…)` rules for the allowed commands when the task changes files, or editing and shell tools disallowed; Codex `exec --json` in its `workspace-write` or `read-only` sandbox; TermOS's agent with `Headless::Edits` (writes in the workspace, commands matching the patterns and not dangerous or networked) or `ReadOnly`, every other question refused. The allowed commands are the project's build, test, lint, check and format tasks (`termos task`) and `[harness] allow-commands`. Prompts go on standard input.
  - **The run:** a worker thread with a single-threaded runtime and a `LocalSet` (agent sessions are not `Send`). A task waits for its dependencies and every earlier task that changes files; writes one at a time, reads up to `parallel` alongside. A task's prompt carries its dependencies' answers (cut at 1,500 characters). The CLIs run in their own process group, stopped with what they started (Windows: the CLI only, `TODO(M9b)`), and after `task-minutes`.
  - **Records:** a hash-chained audit log per plan (`harness-<id>`, new `audit::Event::Handoff`: task, kind, backend, model, account, outcome, a redacted detail) and `journal::Event::Handoff` (names and outcomes). Tokens are not journalled again: the CLIs' logs and the agent's usage already reach the command center. Claude Code's cost estimate is shown as an estimate, never counted against the budget.
  - **CI** (`agents.yml`): a plan through the real CLIs on every platform, against stand-in model servers: Claude Code plans and writes, Codex is refused for the review, which falls back to Claude Code; the log verifies.
- **Alternatives:** the harness inside termos-ai or termos-center; plans as free text the harness interprets; Claude Code's `--json-schema` or Codex's `--output-schema` for plans (each CLI differently, and API models neither); falling back to another account; a permission prompt through the harness window (`--permission-prompt-tool`), which Codex cannot do headless (D-099).

---

## D-101: M9b owner answers: the Windows sandbox, images on Windows, GPU sources, delivery
- **Status:** Accepted (owner) · 2026-11-04 · when grants are taken back: superseded by D-108
- **Context:** M9b closes the platform gaps of D-077. Four choices were open; the owner chose (2026-11-04):
- **Decision:**
  - **Windows sandbox for agents' commands: an AppContainer** (closing D-077's "an AppContainer or a restricted token"). The project is writable, the rest of the profile is out of reach, and there is no network unless granted; TermOS grants the container the project (and, read-only, the toolchains a command needs) and takes the grants back.
  - **Images from programs in panes on Windows:** Microsoft's newer ConPTY (`conpty.dll` and `OpenConsole.exe`, MIT, from the `Microsoft.Windows.Console.ConPTY` package) ships in the Windows zip beside `termos.exe`, pinned and checksummed, with its licence. Without it TermOS uses the system's ConPTY, which drops the images, and says so.
  - **GPU readings:** the operating systems' own sources only. Windows: utilisation and video memory of every GPU from its performance counters; macOS: GPU temperature from the SMC and utilisation from IOKit. No vendor libraries, so no temperature or power for AMD and Intel GPUs on Windows.
  - **Delivery:** one milestone, committed in parts as each passes CI, with one review at the end.
- **Alternatives:** a restricted low-integrity token, or a job object only; downloading the ConPTY on first use, or no images on Windows; AMD's ADLX and Intel's IGCL as well; M9b split into Windows work and the rest.

## D-102: Terminal input: our own decoder on every platform; VT input on Windows
- **Status:** Accepted (lead; supersedes D-043's "Windows consoles are detected from the environment only") · 2026-11-04
- **Context:** On Windows, crossterm reads console key records, so pastes arrive as typed keys and the Kitty keyboard protocol is out of reach (D-077). Kitty release events and alternate keys (flags 2 and 4, also M9b) need more than crossterm's key events carry, such as the base-layout key.
- **Decision:**
  - **One decoder** (`termos_core::input::decode`) turns terminal bytes into keys, mouse reports, focus reports and bracketed pastes:
    - **keys:** legacy bytes, xterm's modifier forms, SS3, the Kitty protocol with event types and alternate keys, and xterm's `modifyOtherKeys`;
    - **mouse:** SGR, urxvt and X10 reports.
  - **Late replies are dropped:** DA1, `CSI ? u`, DECRQM, OSC, DCS and APC strings never become typed keys.
  - **Timeouts settle what is cut short:**
    - a lone ESC waits 50 ms and is then the Escape key;
    - a reply string that has content waits 500 ms and is then dropped;
    - a paste that goes quiet for 1 s is delivered as it is, so a lost end marker cannot capture the keyboard;
    - long pastes come in parts of at most 4 MiB.
  - **Tests:** a fuzz target (`input_decode`) and property tests check that splitting the input anywhere gives the same events.
  - **One reader on every platform:** a thread blocks until input arrives or it is told to stop, so idle costs nothing.
    - **Unix:** it reads stdin bytes. SIGWINCH reports resizes.
    - **Windows:** the console runs in VT input mode (`ENABLE_VIRTUAL_TERMINAL_INPUT`, plus window and mouse input, without QuickEdit). The characters of its key records are those same bytes. Resize and mouse records (from consoles that send records rather than VT reports) are read alongside. The console's mode is put back on exit.
    - **What Windows now has:** bracketed paste, Kitty keyboard detection, and the capability probe (`TERMOS_PROBE=0` still turns it off). AltGr characters arrive as text, so the old Ctrl+Alt guess is gone.
  - **crossterm keeps** raw mode, the alternate screen and the terminal size. The client writes the mouse, paste, focus and Kitty mode sequences itself, the same on every platform.
  - **`termos debug keys`** prints the bytes as they come and the events they decode to, through the same reader.
- **Alternatives:**
  - crossterm's event stream on Unix with our decoder on Windows only: two paths, and the one Windows needs would not get the Linux end-to-end tests;
  - reading Windows input with `ReadFile`, which skips resize records;
  - keeping key records and guessing pastes from timing.

## D-103: Kitty keyboard flags 2 and 4 for programs in panes
- **Status:** Accepted (lead; completes D-046, whose flags 2 and 4 were `TODO(M9)`) · 2026-11-04
- **Context:** Programs in panes can ask for event types (flag 2: repeats and releases) and alternate keys (flag 4: the shifted key and the key's place on a US layout). TermOS can only pass these on if the outer terminal reports them. Asking every terminal for them all the time would change how every key arrives, even when no program wants them.
- **Decision:**
  - **The terminal is asked only when a program asks.** Each client still pushes flag 1 when its terminal speaks the protocol. When its focused pane's program sets flags 2, 4, 8 or 16, the server tells that client (`ServerMsg::KeyboardFlags`). The client then sets exactly those flags plus 1 on its terminal (`CSI = flags ; 1 u`), and drops them again when focus moves on. The pop at exit restores the terminal.
  - **Keys carry what the terminal reported.** A repeat, a release, or a key with alternates travels as `ClientMsg::KeyDetail`; a plain press is still `ClientMsg::Key`. The frontend protocol becomes version 4. Shift, which tmux notation folds into the character, is kept in the alternates (`shifted`), so the key code a program gets is the key without Shift (`CSI 49:33;4u` for Alt+!). With flag 16 the terminal's text wins over a guess.
  - **Releases go where the press went.** Only presses sent to a pane whose program asked for releases are remembered, at most 32 per client. A release goes to the same pane even if focus moved, and a release with no press is dropped. Presses TermOS handled itself (the prefix, bindings) therefore never send a stray release.
  - **The encoder follows the specification:**
    - repeats and releases carry `:2` and `:3`;
    - keys sent as text, and unmodified Enter, Tab and Backspace without flag 8, have no release;
    - without flag 2 a repeat is a press and a release is dropped;
    - alternates appear only when they differ from the code.
  - **The `CSI ? u` reply** now lists all five flags.
  - **One gap stays:** modifier keys pressed alone (flag 8) are not passed on, since the desktop's keys have no such code.
- **Alternatives:** always asking the terminal for every flag (all keys as escape codes for everyone); always asking for 1, 2 and 4 (releases for a program asking for flag 8 would still miss text keys); a key message carrying every field every time.

## D-104: App windows are saved with the session as the command that reopens them
- **Status:** Accepted (lead; extends D-033) · 2026-11-04
- **Context:** Snapshots (D-033) saved terminals only, so a restored session lost its file manager, viewers and monitor. Apps keep their state in their own types, often with threads and caches, and that state is not worth serialising. The useful part is the folder, file or view.
- **Decision:**
  - **What is saved:** a new `App::reopen` gives the palette command that opens the window again as it is now: `files.open DIR`, `image.open PATH` (the picture shown), `markdown.open PATH`, `monitor.open`, `network.open`, `settings.open`, `clipboard.open`, `center.open`, `usage.open`. A path that is not Unicode gives none, rather than a different path.
  - **The snapshot:** each app pane keeps that command (`reopen`, optional, so older snapshots still load; the format stays version 1).
  - **Restoring:** the desktop builds the app from the command without placing it and puts it under a new id where the old one was in the saved layout: same float rectangle, same tile, same focus. A command that fails to open is logged and the window left out.
  - **Not restored:**
    - agent windows, which `agent.resume` already reopens with their conversation (bringing one back on its own would start a provider session unasked);
    - the harness, whose runs are not resumable;
    - the tutorial, notifications, About and plugin windows.
  - **When it is saved:** the command is taken when the snapshot is written (after layout changes, and when the server stops). Browsing in a window without changing the layout is therefore saved at the next save.
- **Alternatives:** serialising each app's state, which is more code per app for little gain; restoring agent windows with their conversation; saving app windows by kind only, so a file manager would come back in the home folder.

## D-105: The KMS console does a terminal's jobs: clipboard, bell, pictures
- **Status:** Accepted (lead; extends D-073) · 2026-11-04
- **Context:** A terminal client hands the clipboard (OSC 52), the bell and pixel pictures to the terminal around it. The bare-metal console has no terminal around it, so it dropped all three and pictures showed as colour blocks.
- **Decision:**
  - **Pictures:** the console says it speaks Kitty graphics, so the server places pictures for it as for kitty: the PNG once per picture, then one placement per visible piece with the part of the picture it shows, windows on top already cut out. The console decodes each picture once, cuts and scales each placement to its cells (made once per placement), and blends it over the cells by its alpha. Cells painted over a picture bring it back on top. A new placement list repaints the cells where pictures were; a freed picture does the same. Picture data over 64 MiB is refused.
  - **Clipboard:** the text of the last `ServerMsg::Clipboard` (TermOS copies: copy mode, the mouse, programs' OSC 52) stays in the console's memory, never on disk or in the log. Ctrl+Shift+V or Shift+Insert sends it as a paste.
  - **Bell:** a frame in bright yellow around the screen's edge for 150 ms, then a repaint. A timer exists only while it shows; bells during the flash add nothing, so a loop of bells cannot strobe. `[kms] visual-bell = false` turns it off.
- **Alternatives:** the system's clipboard (there is none without a window system); a full-screen inverse flash (harsher to look at); block pictures as before.

## D-106: The foreground program on macOS and Windows; GPU readings from the OS
- **Status:** Accepted (lead; within D-101) · 2026-11-04
- **Context:** Tabs, clipboard sources and snapshots name the program running in a pane, and snapshots save its folder and command line so restore can start it again. Linux reads `/proc`; macOS and Windows had nothing. The GPU readings D-101 asks for were partly there already: sysinfo's GPU list takes utilisation and memory from Windows's performance counters and DXGI, and from macOS's IOKit, for every vendor.
- **Decision:**
  - **macOS:** the terminal's foreground process group leader, as on Linux. Its name, folder and command line come from `sysinfo`, which uses libproc and sysctl, for that one process.
  - **Windows** has no foreground process group. The pane's program is therefore the newest process started under its shell, skipping console hosts, found through `sysinfo`'s process list. Its command line and folder come from the process's own parameters. With nothing running under the shell, the shell itself counts, as at a Unix prompt.
  - **Cost:** tabs ask on every frame, so on these two systems the answer is kept for a second. There is no timer; a stale answer is replaced when asked for.
  - **Restoring types the command into the pane's shell, quoted for it:**
    - PowerShell: `& 'prog' 'arg'` with `''` for a quote;
    - cmd: double quotes, and a command with `"`, `%` or `!` is not typed at all but named as not restarted;
    - POSIX as before.
  - **GPU temperature on macOS** comes from the system's own sensors, through sysinfo: the SMC's `TG0P` on Intel Macs, and the HID temperature sensors named for the GPU on Apple silicon. The hottest GPU sensor goes to the only GPU, or to the one discrete GPU beside Intel's built-in graphics. On Apple silicon that is the HID sensor service rather than the SMC D-101 named; it is the source the system itself exposes there.
  - **Windows:** utilisation and memory from the performance counters, through sysinfo, for every GPU. No temperature or power for AMD and Intel GPUs, as D-101 said.
- **Alternatives:** our own libproc and process-snapshot FFI (more unsafe code for what sysinfo already does); the console's process list on Windows (`GetConsoleProcessList`, which needs attaching to each pane's console); raw SMC calls on every Mac.

## D-107: Windows: job objects, saving at logoff, the ConPTY in the zip
- **Status:** Accepted (lead; carries out D-101) · 2026-11-04
- **Decision:**
  - **Job objects for the harness's CLIs.** A CLI starts suspended, goes into a new job object, and only then has its thread resumed (found through a Toolhelp snapshot). Not even its first child can start outside the job. Stopping a task ends the whole job, as stopping its process group does on Unix. The job has no kill-on-close limit, so a CLI that finishes normally leaves what it started alone, as on Unix. Without a job (one could not be made) the CLI still runs, and stopping it stops only the CLI; a warning is logged. A CLI that cannot be resumed is stopped and reported. Windows test: a background script started by the CLI is ended with the job, and finishes when nothing stops it.
  - **Logoff and shutdown.**
    - A process that has loaded user32 gets no console events for these. A hidden top-level window (message-only windows miss the broadcast) on its own thread answers `WM_QUERYENDSESSION` with yes.
    - On `WM_ENDSESSION` it hands the server an acknowledgement and waits, up to five seconds, until the server has written the snapshots and sends it.
    - The console events stay for processes that get them. Windows test: the window waits for the acknowledgement, and asks nothing when the session is not ending after all.
  - **The ConPTY in the zip.**
    - Microsoft.Windows.Console.ConPTY 1.24.261001001 (MIT) is pinned by version and NuGet's SHA-512. `.github/scripts/fetch-conpty.sh` puts its x64 `conpty.dll` and `OpenConsole.exe` beside `termos.exe` in the test-build and release zips, with the licence (`THIRD-PARTY-CONPTY.txt`).
    - portable-pty already loads a `conpty.dll` found beside the program, and this ConPTY passes programs' output through, pictures included: it has no pass-through flag to set and portable-pty needs no change.
    - The Windows CI job puts it beside the test binaries; a test sends a Sixel and a Kitty picture through it. The `termos.exe` the end-to-end tests start keeps the system's ConPTY, as `cargo install` users have.
    - Without the files TermOS uses the system's ConPTY, which drops the pictures, and says so in its log and in `termos debug caps`.
- **Alternatives:**
  - assigning the job after the CLI runs (its first children could escape);
  - ending the CLI's job when its handle closes (it would also end what a finished CLI meant to leave running);
  - downloading the ConPTY on first use.

## D-108: The Windows sandbox: an AppContainer, with grants kept until reset
- **Status:** Accepted (owner on grants, 2026-11-04; supersedes D-101's "takes the grants back") · 2026-11-04
- **Context:**
  - D-101 chose an AppContainer for agents' commands on Windows.
  - An AppContainer reads only what is granted to it or to all apps. Granting a folder means Windows rewrites the permissions of every file in it, which takes seconds to minutes for a big project or `~/.cargo`, and taking the grant back costs the same.
  - The owner was asked when grants should be taken back, and chose to keep them until a reset.
- **Decision:**
  - **The container:** one per TermOS installation (named from a hash of its state folder, so tests never touch the user's), made on first use. `termos sandbox-exec` (the helper the agent already wraps commands in) runs the command in it, and `termos sandbox status` reports "sandbox: AppContainer", which counts as full confinement.
  - **What the command reaches:**
    - It writes the workspace and the profile's `write-paths`.
    - It reads what Windows lets every app read, plus, read-only:
      - the folders on `PATH` inside the user's profile (a `bin` or `shims` folder's parent, so `~/.cargo` and mise's data come along) and `~/.rustup`;
      - the program itself when named by path;
      - `--read` folders.
    - The rest of the profile (keys, `.ssh`, browsers) is out of reach without being listed, unlike bubblewrap's hiding.
    - The network is `internetClient`, only when the call asked for it and the user agreed. AppContainers never reach loopback.
  - **Grants are kept:** each grant is recorded in `<state>/sandbox/grants` and not repeated. `termos sandbox reset` takes every one back and deletes the container. A workspace that cannot be granted stops the command; a toolchain folder that cannot be granted is left out with a warning.
  - **Running:**
    - The command starts suspended in a kill-on-close job, so it ends with the helper (as bubblewrap's `--die-with-parent`).
    - It gets inheritable copies of the helper's console handles.
    - Its environment loses credential-like variables (D-061), and TEMP and TMP point at the container's own folder.
    - The helper ignores Ctrl+C (the command gets it), waits, and passes on the exit code.
  - **Tests:** CI requires the sandbox on the Windows runner (`TERMOS_REQUIRE_SANDBOX`). The end-to-end test shows that:
    - a write lands in the workspace;
    - a write outside it and a read of a file in the profile fail;
    - a public address is unreachable without `--net` and reachable with it;
    - `termos sandbox reset` succeeds.
  - **Plugins** still run without a sandbox on Windows. They need the session's named pipe, which an AppContainer cannot open without its own grant.
- **Alternatives:**
  - taking grants back when each agent session ends, or after each command (the rewrite every time);
  - a restricted low-integrity token, which cannot keep reads out of the profile.

## D-109: Owner answers for M5b and the images' curl
- **Status:** Accepted (owner, 2026-11-04) · 2026-11-04
- **Context:** M10d left `termos agent install` without a downloader on the bare-metal images, pending the choice of curl's TLS library. M5b needs a dev rootfs, a way to be root inside it, and a place for Podman. The owner was asked all four.
- **Decision:**
  - **curl on the images:** curl and libcurl built with **mbedTLS** (small, Apache-2.0; OpenSSL is ruled out by D-003), trusting the image's CA certificates. Installs on the images use the same code path as everywhere else. The boot tests check that the image's curl is the mbedTLS build.
  - **The dev rootfs:** **Debian 13 (trixie) from the official `debian` image**, pinned by digest per architecture and checked by SHA-256 before anything is unpacked. TermOS downloads the image's layer from the repository that builds the official image (the same bytes as the registry, which rate-limits anonymous pulls). `termos devroot update` runs apt's upgrade inside it; a newer pinned image comes with a TermOS release.
  - **Root inside it:** **rootless, with subordinate IDs.** The rootfs is unpacked and entered in a user namespace that maps the user to root and the user's `/etc/subuid` and `/etc/subgid` ranges to the other IDs (shadow's `newuidmap`), so apt and dpkg work and files keep their owners. Nothing runs as the host's root. The images gain shadow's ID helpers and a range for the user `termos`.
  - **Podman:** **the host's, hosted only.** Where the host has Podman, the package manager app lists its images and containers and `termos devroot` can run the rootfs in a Podman container instead of bubblewrap. The images carry no Podman.
- **Alternatives:**
  - curl with GnuTLS (larger, with nettle and GMP) or OpenSSL (D-003); a downloader inside TermOS (a TLS stack in the binary; D-054 chose the system's curl);
  - Ubuntu or Alpine as the rootfs; debootstrap at create time (slower, needs a mirror and root);
  - fakeroot-style root (no real owners; apt's sandbox user fails); a setuid helper;
  - Podman on the images (its runtime, networking and storage drivers add tens of megabytes and a daemon-like service for one feature).


## D-110: The dev rootfs and the package manager
- **Status:** Accepted · 2026-11-04
- **Context:** D-109's answers: Debian 13 from the official image pinned by digest, rootless with subordinate IDs, the host's Podman. M5b also asks for a package manager over mise, the dev rootfs and the host.
- **Decision:**
  - **The image:** the official `debian:trixie` image's one layer, per architecture (amd64, arm64), downloaded with `curl` from the repository the official image is built from (by commit; the same bytes as the registry's blob) and checked by size and SHA-256 against values compiled into TermOS before anything is unpacked. `--from FILE` takes a copy already here; any other file needs its SHA-256 (`--sha256`). Podman pulls the same image by its index digest.
  - **The namespace:** util-linux 2.38's `unshare --user --map-user=0 --map-group=0 --map-users=1:SUBUID:65536 --map-groups=1:SUBGID:65536`, which runs shadow's `newuidmap` and `newgidmap`: the user is root inside, the first 65 536 subordinate IDs are 1 to 65 536 (`nobody` included), and `setgroups` stays allowed, so apt's `_apt` sandbox works. TermOS reads the ranges itself (by name or number, as `newuidmap` does) and `termos devroot status` names what is missing or which kernel setting refuses (Ubuntu's AppArmor restriction, `unprivileged_userns_clone`, `max_user_namespaces`).
  - **Unpacking and removing** run as `termos` itself inside that namespace (hidden `devroot unpack` and `devroot wipe`): the `tar` crate keeps owners, modes, setuid bits and times; device nodes and pipes are left out (bubblewrap gives `/dev`). The files then belong to subordinate IDs the user cannot delete from outside, so `remove` deletes from inside, and only a `rootfs` folder in a `devroot` folder.
  - **Entering:** bubblewrap inside the namespace: the rootfs as `/`; its own `/dev`, `/proc` (a PID namespace), and tmpfs `/tmp` and `/run`; the host's `/sys`, `resolv.conf` and `hosts` read-only; the home read-write at its own path, and the current folder too when it is outside the home and not a system folder; the host's network; a clean environment (root's `HOME`, Debian's `PATH`, `TERM`, the proxies, `TERMOS_DEVROOT=1`, `debian_chroot=devroot` for the prompt). `enter` becomes that process, so the terminal, signals and exit code are the shell's.
  - **Updating** is `apt-get update` and `apt-get -y upgrade`, keeping changed configuration files. A newer pinned image comes with a TermOS release and is used by the next `create`.
  - **Podman** (`--podman`; the only way off Linux): a container `termos-devroot` idling on `sleep infinity`, the home bound at its path (`/mnt/c/…` from Windows), SELinux labels off for it; `enter` starts it when stopped and runs `podman exec`; `stop` stops it; `remove` deletes the container and leaves the image.
  - **The images:** bubblewrap, util-linux's `unshare`, shadow with subordinate IDs (its `newuidmap` and `newgidmap` setuid, its `su` not built), and `/etc/subuid` and `/etc/subgid` giving `termos` 100000–165535. The QEMU boot test checks that `termos devroot status` reports bubblewrap ready.
  - **The package manager** (`packages.open`, in termos-devkit, opened through the launcher like the command center): a tab per source. mise lists with `mise ls --json` and searches its registry; the dev rootfs is listed from dpkg's status file read directly and searched with `apt-cache search` inside; the host's manager (apt, dnf, pacman, zypper, apk, Homebrew, winget, Scoop, the first found) with its own list and search commands; Podman with its images, containers and `podman search`. Lists and searches run on threads; while the window is drawn it lists again when the source's package database changes (dpkg's status, rpm's or pacman's folder, apk's database, mise's installs), so nothing runs while it is idle (D-028). Installing, removing and upgrading are confirmed in the window, then run as `termos pkg install|remove|upgrade SOURCE NAME --wait` in a window of their own, where apt asks its questions and `sudo` (or `doas`) its password. A package name is one argument: no option, space or shell character. `termos pkg` does the same from any terminal, with `list`, `search`, `setup` (fetching mise, or creating the dev rootfs) and `sources`.
- **Alternatives:**
  - util-linux's `--map-auto` (it maps the first block whatever its size); setting the namespace up ourselves with `newuidmap` and a pipe (works with any `unshare`, but more moving parts, while 2.38 is in Debian 12, Ubuntu 23.04, Fedora 36 and the images; older systems use Podman);
  - unpacking with the system's `tar` (BusyBox's and GNU's differ on owners and options);
  - the host's `/dev/pts` bound in (opening new terminals inside then depends on the host's `ptmx`);
  - PackageKit for the host's packages (missing on many systems, and on macOS and Windows);
  - installing from the app without a window (no place for apt's questions or the password).


## D-111: M11d owner answers: sign-in through the vendors, API keys, Gemini CLI; what comes next
- **Status:** Accepted (owner, 2026-11-05) · 2026-11-05
- **Context:** the owner asked to enter credentials for Claude Code, Codex and the rest so that each starts signed in to the right subscription and account switching (D-097) keeps working, and for research into other improvements. Anthropic's terms say third-party developers "may not collect, store, or intermediate Claude.ai credentials or session tokens — sign-in to a Claude account must complete through Anthropic's own flow", while an end user may sign in to the unmodified Claude Code binary with their own subscription; Google's terms say the same of Gemini CLI's login. Each CLI supports a folder per account (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `GEMINI_CLI_HOME`). The owner was asked four questions.
- **Decision:**
  - **Sign-in:** TermOS runs each vendor's own sign-in in the account's folder, from its windows and commands (Claude Code's `claude auth login`, which takes a pasted code where there is no browser; Codex's `codex login`, with its device code where there is no browser; Gemini CLI's own login). The CLI keeps its login; TermOS stores no subscription credential, so D-096's "TermOS never reads their login tokens" stands. Every TermOS window then starts each CLI signed in to the account in use, switching included.
  - **API keys:** an account may use an API key instead (Anthropic Console, OpenAI, Gemini API), kept in TermOS's secret store (D-058) and given to that CLI when it starts; Codex's is handed to `codex login --with-api-key`, which keeps it in the account's folder.
  - **Gemini CLI** gets accounts like Claude Code and Codex: installed by TermOS, its own folder per account, sign-in, switching, TermOS's MCP server, usage. TermOS only ever launches the official binary.
  - **Next, in this order, each its own milestone after M11d:** parallel agents in git worktrees with a review window that answers the agent (M12a); attention routing and resuming agent panes (M12b); remote access (M12c); hardening (M12d).
- **Alternatives:** TermOS keeping a Claude `setup-token` or Codex access token in its keyring and handing it to the CLI (the same convenience, but against Anthropic's wording, and it reverses D-096); accounts for subscriptions only; opencode as well (it keeps every login in one file outside its config folder).


## D-112: Signing in from TermOS, API keys per account, Gemini CLI
- **Status:** Accepted · 2026-11-05
- **Context:** D-111: TermOS runs each vendor's own sign-in and stores no subscription credential; accounts may use API keys; Gemini CLI gets accounts. The CLIs' current interfaces were read from their documentation and source (Claude Code's `claude auth login|status`, Codex's `codex login`, `login status` and `--with-api-key`, Gemini CLI's `GEMINI_CLI_HOME`, its settings and its session files).
- **Decision:**
  - **Sign-in** (`termos account sign-in CLI NAME`, the usage window's **[Sign in]**, a new account's offer) runs the CLI's own sign-in with the account's folder:
    - Claude Code: `claude auth login` (`--console`, `--sso`), which shows a link and reads the pasted code, so it works without a browser on the machine.
    - Codex: `codex login`, or `codex login --device-auth` where no browser can open (over SSH, or on Linux without a graphical session) or with `--device`.
    - Gemini CLI: its interactive start with `security.auth.selectedType = "oauth-personal"` in the account's settings, and `NO_BROWSER=true` where no browser can open (a link and a pasted code).
    - An account with an API key refuses a sign-in until the key is removed (the key would come first).
  - **Who is signed in** (`termos account status`, after each sign-in): Claude Code's `claude auth status` JSON (`loggedIn`, `authMethod`, `email`, `subscriptionType`); Codex's `codex login status` (exit status and its first line; a configured key is only named, never echoed). Gemini CLI has no status command: its `google_accounts.json` (an email, no token) and the presence of its login file. Each command runs with a 30-second limit. Accounts with a key in TermOS's store report it without reading it.
  - **API keys** (`termos account key CLI NAME [--remove]`, **[API key]**): typed without echo, or read from standard input.
    - Claude Code and Gemini CLI: kept in TermOS's secret store (D-058) as `account.<cli>.<name>`, named in the account's `api-key` in `accounts.toml`, and given by the shim as `ANTHROPIC_API_KEY` or `GEMINI_API_KEY` (held as `SecretEnv`, a `secrecy` type, until the program starts). Gemini CLI's account settings select `gemini-api-key`. Claude Code asks once whether to use it; TermOS says to choose Yes and does not write the approval itself, which would put part of the key in Claude Code's settings.
    - Codex: piped to `codex login --with-api-key` with `CODEX_HOME` set; Codex keeps it; `--remove` is `codex logout`.
  - **Isolation:** for TermOS's accounts and any account with a key, the shims (and agent windows and the harness, which plan launches the same way) drop the variables each CLI signs in with: Claude Code's `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `CLAUDE_CODE_OAUTH_TOKEN`, `CLAUDE_CODE_OAUTH_REFRESH_TOKEN`, `CLAUDE_CODE_USE_BEDROCK|VERTEX|FOUNDRY`; Codex's `CODEX_API_KEY`, `CODEX_ACCESS_TOKEN`; Gemini CLI's `GEMINI_API_KEY`, `GOOGLE_API_KEY`, `GOOGLE_GENAI_USE_VERTEXAI|GCA`, `GOOGLE_GEMINI_BASE_URL`. Variables the CLIs do not sign in with (`OPENAI_API_KEY`) stay for the project's own tools. `default` keeps the environment as it is.
  - **Gemini CLI:**
    - **Install:** the GitHub release's `gemini-cli-bundle.zip`, checked against GitHub's SHA-256 digest (none: refused), unpacked with the `zip` crate (deflate only; names that would leave the folder are refused), run by `node@22` from mise (installed, not made the user's default) through a launcher TermOS writes (`gemini`, or `gemini.cmd` on Windows). Zip downloads of other CLIs use the crate too, rather than `tar`.
    - **Accounts:** `GEMINI_CLI_HOME` is a home folder; Gemini CLI keeps its files in `.gemini` in it, so an account's folder holds `.gemini/` and a link to the user's `~/.agents` (Agent Skills, D-098). `GEMINI.md`, `commands`, `skills` and `extensions` are linked; `settings.json` is copied. `GEMINI_FORCE_FILE_STORAGE=true` keeps logins in the account's files, since Gemini CLI's keychain entries do not depend on its home.
    - **TermOS's MCP server** is written into the account's `settings.json` (`mcpServers.termos`), as Codex's `config.toml` is, rather than with `gemini mcp add`. For TermOS's install, `general.enableAutoUpdate` and `enableAutoUpdateNotification` are turned off. A settings file that is not plain JSON (comments) is left unchanged, with an error.
    - **Usage:** `.gemini/tmp/<project>/chats/session-*.jsonl` (and sub-agents' sessions below them): a `gemini` message with `tokens` and `model`, counted once by session and id (Gemini CLI writes a message again once its tokens are known); input is the prompt less its cached part, thinking counts as output, cached reads apart. No text is kept.
    - **Carrying a conversation:** the newest session for the folder (found through `projects.json`) is copied into the other account under the project name that account uses for the folder (recorded in its `projects.json` when it has none) and resumed with `gemini --resume <id>`.
    - The harness (D-100) still routes to Claude Code and Codex only.
  - **The desktop:** `account.key CLI NAME` (palette, the usage window's **[API key]**) opens a window running `termos account key`; the usage window's account rows shorten "signed in as …" to fit before their buttons.
- **Alternatives:** pre-approving the key in Claude Code's `.claude.json` (an undocumented field holding part of the key); `gemini mcp add` (its `-e` drops values with `=`, and its scope depends on the folder); installing Gemini CLI with mise's npm backend (it needs a global Node.js choice and leaves updates to npm); reading the CLIs' login files for status (their formats are undocumented, and they are credentials).


## D-113: M12a owner answers: worktree workspaces, port blocks, harness results on a branch, pull requests through gh and glab
- **Status:** Accepted (owner, 2026-11-05) · 2026-11-05
- **Context:** M12a (D-111) gives each task a workspace of its own: a branch and a git worktree, its setup, ports, and panes for the agent, a shell and servers, reviewed before it is merged. D-100 runs the harness's editing tasks one at a time in the project and named worktrees as the alternative. The owner was asked four questions.
- **Decision:**
  - **Isolation: the worktree only.** A workspace's panes run on the host in its worktree; the agent CLIs keep their own sandboxes (and TermOS's agent its, D-061). No container per workspace; the dev rootfs (D-109) stays a separate choice.
  - **Ports: a block per workspace.** Each workspace reserves a block of consecutive ports from a configurable range, checked free when it is made and recorded so no two workspaces share one, and passed to its panes as `PORT` and numbered variables. It keeps its ports across restarts.
  - **The harness's result lands on a branch, then review.** In a git repository each editing task runs in its own worktree, branched from the plan's branch, and is merged into that branch when it finishes; a conflict reruns the task once on top of the branch as it is, then stops for the user. The plan ends in the review window (commit, pull request, merge or discard). Outside git the harness edits in place, one task at a time, as before. This supersedes D-100's "edits one at a time in the project" for git repositories.
  - **Pull requests through `gh` or `glab`** when installed: the branch is pushed and `gh pr create` or `glab mr create` runs with the title and body filled in; without them TermOS pushes and shows the compare link. TermOS stores no forge token.
- **Alternatives:** the dev rootfs or a Podman container per workspace (isolated, but slower, Linux-centred or needing Podman, and the CLIs and their accounts would have to be mounted in); a free port at each start (changes after a restart) or a network namespace per workspace (Linux only); harness edits in place as now, or chosen per run; GitHub's API with a token in TermOS's secret store, or no pull requests.


## D-114: Worktrees, their workspaces and review, parallel harness edits, best-of-N
- **Status:** Accepted · 2026-11-05
- **Context:** D-113's answers: a worktree per task without a container, a block of ports each, the harness's edits in parallel worktrees with the result on a branch, pull requests through `gh` and `glab`.
- **Decision:**
  - **Worktrees (`termos_devkit::worktrees`)** go through the user's `git` (their configuration, hooks, editor and credentials):
    - **Where:** `<root>/<repository folder name>-<8 hex of its path's SHA-256>/<name>`; root is `[worktrees] root`, default `worktrees` in TermOS's data folder, outside the repository so tools that look upwards do not find the main checkout's settings.
    - **The record:** `worktrees/registry.toml` there, changed under an exclusive file lock (std's `File::lock`) and written by rename. It holds each worktree's repository, branch (`[worktrees] branch-prefix` plus its name), base and base commit, title, ports, origin (the user, `harness:<plan>`, `best:<run>`), state, last commit and pull request.
    - **Ports:** a block of `[worktrees] ports` (10) from `port-range` (20000–29999, below every system's range for outgoing connections), the first block that overlaps no active worktree's and whose ports can all be bound on 127.0.0.1 when it is made. Programs get `PORT`, `TERMOS_PORT_<n>`, `TERMOS_PORTS` and `TERMOS_WORKTREE*`.
    - **Setting up:** a project's `termos.toml` has `[worktree]`: `setup` commands (run in a shell with the ports, stopping at the first failure), `copy` globs (`globset`) over what `git ls-files --others --ignored --exclude-standard --directory` lists (ignored files, and ignored folders as a whole; default: `.env` files at any depth), and `servers`.
    - **Finishing:**
      - review data is the diff against the merge base, edits not committed included, with untracked files listed and shown whole;
      - commit is `git add -A` and git's own commit;
      - merge happens in the folder where the base is checked out (which must be clean), else without a checkout (`git merge-tree --write-tree`, git 2.38+, then `commit-tree` and `update-ref` against the old value), and a conflict changes nothing;
      - archive removes the folder and keeps the branch; discard removes both; both keep the last commit as `refs/termos/archive/<name>`, so restore works even after a discard; forget drops the record and the ref.
    - **`termos worktree`:** `new list status diff env setup commit merge pr archive discard restore forget open`.
  - **Pull requests:** `pr` pushes the branch to its upstream's remote (else `origin`, else the only one), then runs `gh pr create` or `glab mr create` with the title and the commits as body. The forge is told from the remote's URL. Without the tool, TermOS shows the forge's page for a new pull request. The pull request's address is recorded.
  - **The desktop:**
    - **`worktree.open`** makes a workspace named after the worktree (its name no longer follows its programs). The agent goes on the left: `[worktrees] agent` (`auto`: the first installed of Claude Code, Codex and Gemini CLI as the account in use; or `termos` for TermOS's agent window). A shell goes on the right, and each `servers` command is typed into a shell below it, so it can be run again.
    - **The worktree's environment** reaches every pane started in its folder: the launcher's `worktree_env` looks the folder up when a pane is made, so splits and restored panes get it too.
    - **`worktree.new`** runs `termos worktree new --open` in a window that shows the setup and stays open on an error. `worktree.run` runs any `termos worktree` command in a window.
  - **The review window (`termos_devkit::review_app`):**
    - **What it shows:** files and numbered diff lines, refreshed every 3 s while on screen (the app's deadline; git runs on threads).
    - **Comments** on lines or files become one message with paths, lines and the lines' text. A new `AppEffect::TellAgent` gives it to the desktop, which pastes it (bracketed when the program asks) and presses Enter in the worktree's agent pane: the one in its workspace, else one whose folder is inside it.
    - **No agent running:** a new agent pane starts in the worktree with the message as its first prompt (Claude Code's and Codex's prompt argument, Gemini CLI's `--prompt-interactive`). With no CLI agent, the message goes to the clipboard.
    - **Its life:** it closes when it sends, so the agent is in view. A second `worktree.review` of the same worktree brings the open one forward.
    - **Actions** run `termos worktree …` in windows.
  - **The harness (supersedes D-100's order for git repositories):**
    - **The plan's branch:** `[harness] worktrees` (on) gives a plan in a git repository the worktree `plan-<id>` from the branch the project is on.
    - **Editing tasks:**
      - each runs in `plan-<id>-<task>`, made from the plan's branch when it starts;
      - when it succeeds, its changes are committed there (`--no-verify`, as TermOS when git has no name set) and merged into the plan's branch under an async lock, then the worktree is discarded and forgotten;
      - git's work runs in `spawn_blocking`.
    - **Scheduling:** tasks wait only for their dependencies, up to `[harness] parallel` at once, and read-only tasks look at the plan's worktree. The planner is told that edits may run at once, so a task that builds on another must depend on it.
    - **Conflicts:** a conflict reruns the task once in a new worktree from the branch as it is, its prompt saying what conflicted. A second marks the outcome `conflict` and stops the plan from starting more tasks.
    - **The result:** the view carries the plan's worktree; the window's `R` reviews it and `termos harness run` prints it. A project with changes not committed gets a note.
    - **Fallback:** outside git, or when the worktree cannot be made (with a note), tasks edit in place as before.
  - **Best-of-N (`termos_harness::runner::best`):**
    - **The spec:** `claude:2,codex:1` (backends from `route::backends`, 2 to 8 attempts in all). Each attempt is a coding task pinned to its backend, in `best-<id>-a<n>` with ports, from the project's branch, all at once.
    - **After an attempt:** a successful one is committed, its diff counted, and the project's test task (`tasks::find`'s `test`) run in its worktree with its environment and `task-minutes` as the limit. The last 30 lines are kept. A failed attempt's worktree goes.
    - **Keeping:** discards and forgets the others. In the window Enter keeps and `R` reviews; `harness.best SPEC PROMPT` and `termos harness best --with SPEC` start it.
- **Alternatives:** libgit2 or gitoxide (no user hooks, editor or credential helpers; merges and worktrees less complete); worktrees inside the repository (`.git`-ignored) or beside it; the ports set only on the workspace's first panes; a merge-resolving agent for conflicts (M12e's checkpoints and hand-offs would carry it); keeping best-of-N attempts' conversations to continue them.

## D-115: M12b owner answers: needs-you loud and done quiet, shell integration injected, system notifications, agents resume their conversation
- **Status:** Accepted (owner, 2026-11-05) · 2026-11-05
- **Context:** M12b (D-111) marks agents that wait for the user, brings agent panes back after a restore and adds command blocks (OSC 133). The owner was asked four questions.
- **Decision:**
  - **Needs you loud, done quiet.** An agent that waits for the user (a permission prompt, a question, a program's OSC 9/99/777 notification) gets a ring on its pane, a badge on its tab, a toast and an entry in the notification center. A finished turn only marks its tab and adds a center entry, without a toast.
  - **Shell integration is injected automatically.** Panes start bash, zsh, fish and PowerShell with an integration script that loads the user's own configuration first and then adds the command marks. `[shell] integration = false` turns it off. The user's dotfiles are not changed.
  - **System notifications where supported.** Needs-you events also go to the outer terminal as OSC 9, 777 or 99 (which the terminal or the desktop around it shows as a system notification), only for needs-you events. `[notifications] system = false` turns it off.
  - **Agents resume their conversation.** After a restore, Claude Code, Codex and Gemini CLI panes restart in the same account and folder and continue that folder's latest conversation, the exact one when the CLI's hooks reported it.
- **Alternatives:** a ring and toast for finished turns too, or only a badge for both; a `termos shell-integration` line the user adds to their own startup file, or only shells that mark prompts themselves (fish 4, Nushell); no system notifications, or the operating system's own notification service (D-Bus, macOS and Windows APIs) instead of the outer terminal; restarting agents fresh, or only offering a resume button.

## D-116: Agent attention, command blocks and agents restored
- **Status:** Accepted · 2026-11-05
- **Context:** D-115's answers: an agent that needs you is loud and a finished turn quiet, shells marked by injected scripts, system notifications through the outer terminal, agents continuing their conversation after a restore.
- **Decision:**
  - **What programs signal (`termos_term::intercept`, `signals`):**
    - The interceptor also takes out OSC 9, 99, 133, 633 and 777 (bounded at 64 KiB); `alacritty_terminal` ignores them. Other OSCs pass through as before.
    - **Notifications:** iTerm2's OSC 9 (but not ConEmu's numbered commands, `9;4;…`), rxvt's `777;notify;title;body`, kitty's OSC 99 assembled from its parts (`d=0`, `p=title|body`, `e=1` base64; queries, closes and buttons ignored). Each becomes `VtEvent::Notify`; text is cleaned of control characters and cut (256-character titles, 1024-character bodies).
    - **Marks:** OSC 133 `A`/`B`/`C`/`D;status` (`A;k=s` ignored) and VS Code's 633 `A`–`D` and `E;command line` (`\\` and `\xAB` escapes).
  - **Command blocks (`termos_term::blocks`):**
    - **Anchoring:** like images (D-048), blocks are anchored in the emulator's own text with hidden OSC 8 links: `termos-block:<n>:p` from `A` on the prompt, `termos-block:<n>:c` from `B` on the command line, closed when the output starts. They scroll, reflow and clear with the text.
    - **Output start:** `C`, or the end of the command line (a newline after `B`).
    - **Exit status:** `D;status` counts only after a `C`, so an empty line's `D` is not a command's status. Statuses are kept for the last 2000 blocks.
    - **Output:** the text between a command line and the next block's first marked cell. Wrapped rows are joined; trailing blank lines are left out.
    - **Command line:** 633's `E` when the shell sent it, else the command line's cells.
    - **On screen:** a failed command shows `✗ status` at the right end of its line, drawn by TermOS only over blank cells.
  - **Shell integration (`termos_shell::integration`):**
    - The scripts are built in (`crates/termos-shell/shell-integration/`). They are written to `<data>/shell-integration/` when they differ, and are used when a pane starts its shell (`[shell] integration`, default on). `shell` takes a program or a `[shell]` table with `program` and `integration`.
    - **bash:** `--rcfile termos.bash`, which sources `~/.bashrc`, then puts a status function first and a prompt-wrapping function last in `PROMPT_COMMAND` (a string or an array; the user's functions still see `$?`). It sets `PS0` for `C`; bash before 4.4 has no `PS0`, so no statuses there.
    - **zsh:** `ZDOTDIR` pointing at TermOS's `.zshenv`, which puts the user's `ZDOTDIR` back (`TERMOS_ZDOTDIR`) and sources theirs. A first `precmd` sends `D` and `A`, a `line-init` widget hook sends `B`, and `preexec` sends `E` and `C`.
    - **fish:** TermOS's folder first in `XDG_DATA_DIRS` (put back for children); `vendor_conf.d/termos.fish` reports `E` always, and `A`/`C`/`D` before fish 4, which marks prompts itself.
    - **PowerShell:** `-NoExit -Command ". 'termos.ps1'"` after the profile. It wraps `prompt` (`D` from `$?`/`$LASTEXITCODE` when the history moved, then `A` … `B`) and `PSConsoleHostReadLine` (`E`, `C`), installing the latter at the first prompt if PSReadLine loads late.
    - Other shells (`sh`, cmd) start as before.
  - **Using blocks:**
    - **Copy mode:** `[` and `]` jump between prompts, and `o` copies the output of the command under the cursor.
    - **Commands:** `pane.prompt-prev`/`next`, and `pane.copy-output` (prefix `y`) for the last finished command's output.
    - **`pane.read`:** returns `last_command {command, output, exit, state}` (`command: true` for only that; `finished` skips the reading command itself). `tos read [PANE] --command` prints it, and the MCP tool `read_pane` takes `last_command` (keys masked as before).
  - **Attention (`termos_shell::attention`, `desktop/attend.rs`):**
    - **Needs you:** a pane's `Attention {need: You|Done, since, who, message}`.
    - **Ring and badge:** a pane that needs you gets a ring in the theme's warn colour. Separators around a tiled pane use it over the focus colour, and a float's border uses it; `compose` takes the style per pane. The pane also gets a `● needs you` badge at its top right, which shows even where no ring can be drawn.
    - **Tab, toast, center:** its tab gets `●` in the warn colour. A toast and a center entry say who and where, and `ServerMsg::Notify` goes to every client (protocol 5) when `[notifications] system`. The same agent asking again updates the message without a second toast.
    - **Done:** a `✓` on its tab (not the active one) and a center entry (`Notifications::record`, no toast); seeing its workspace clears it.
    - **Clearing:** input to the pane (keys, paste, `pane.send-*`; not the wheel's arrow keys) clears both. So does the agent moving on (`working`, `ended`).
    - **Jumping:** `agent.waiting` (prefix `j`, and the first item of the AI agents menu while one waits) goes to the one waiting longest: those that need you first, then finished turns. `pane.list` reports `waiting`.
    - **Programs' notifications:** a program's OSC notification makes its pane need you, named after the foreground program.
  - **System notifications (`Caps::notify_style`, client):** the client picks the sequence from its terminal (XTVERSION, `$TERM`, `$TERM_PROGRAM`):
    - kitty: OSC 99, base64 parts;
    - iTerm2, WezTerm, Ghostty and Warp: OSC 9 (prefixed when the text starts with a digit);
    - foot and rxvt: OSC 777;
    - any other terminal, and anything inside tmux: nothing.
  - **Hooks (`termos_devkit::cli_agents::with_hooks`, `termos-cli/src/agent_hooks.rs`):**
    - **What is registered:** `termos agent event --cli NAME` (a 10-second timeout for Claude Code, 3 s for Codex, which allows its `SessionEnd` hooks no more) for these events:
      - Claude Code's `SessionStart`, `SessionEnd`, `UserPromptSubmit`, `PermissionRequest`, `Notification`, `Stop` and `StopFailure`, in the account's `settings.json`;
      - Codex's `SessionStart`, `SessionEnd`, `UserPromptSubmit`, `PermissionRequest` and `Stop`, in `$CODEX_HOME/hooks.json`. Codex asks the user to trust them once; TermOS says so and never writes the trust itself;
      - Gemini CLI's `SessionStart`, `SessionEnd`, `BeforeAgent`, `Notification` and `AfterAgent`, in its `settings.json`.
    - **Keeping the files:** older entries are replaced (found by the command), the user's hooks and other settings are kept, and linked settings files are written through the link.
    - **When:** `termos agent install`, `termos account add` and every agent window register them (`[agents.hooks] register`); `termos agent hooks [--remove] [NAME]` does it by hand.
    - **What the command does:** it reads the hook's JSON (stdin, or the last argument for Codex's `notify`), maps it to `needs-you`, `done`, `working`, `started` or `ended`, and calls the server's `agent.event` with the pane (`$TERMOS_PANE`), conversation id, account (`TERMOS_ACCOUNT_<CLI>`) and folder.
    - Claude Code's `idle_prompt` reminder and quota notices are ignored. Outside TermOS it does nothing.
    - It prints nothing and always exits 0.
  - **Restore (`snapshot::SavedAgent`, `[session] restore-agents`):**
    - **Saving:** a pane saves the agent it runs: its own agent window, or a CLI its hooks reported running in a shell. With it go the CLI, account and conversation.
    - **Restoring a window:** an agent window comes back as `termos agent external --account A --resume [--conversation ID] CLI`.
    - **Restoring a shell's agent:** a shell's agent is typed into the restored shell as `tos agent external …`, so the shell is there after it.
    - **Which conversation:** `--resume` continues `--conversation` when `conversations::find` still finds it (a Claude Code file by id, a Codex rollout by its name's id, a Gemini session by its first line), else the folder's latest, else a new one.
- **Alternatives:**
  - **Anchoring:** absolute line numbers for marks (wrong after the scrollback fills and after reflow), or the per-row flags alacritty does not have.
  - **Hooks:** the `--settings` flag per launch (missed outside agent windows; subcommands do not take it); Codex's `notify` alone (finished turns only, no approvals, and it is going away).
  - **Notifications:** the operating system's notification APIs (a D-Bus, macOS and Windows dependency each, and nothing over SSH).
  - **Restoring a shell's agent:** the CLI started in place of the shell (gone when the agent exits).

## D-117: The next milestones for the OS itself (M13a to M13g, proposed)
- **Status:** Proposed (owner asked for the plan, 2026-11-05) · 2026-11-05
- **Context:** The owner asked which improvements to TermOS are not about AI, then for a plan to build all of them and for them to be in the ROADMAP. What TermOS does today was checked first:
  - Links that programs print are dropped before the screen.
  - The monitor cannot act on a process.
  - The file manager has one pane and no search, archives or bookmarks.
  - The bare-metal image logs in with no password and nothing locks it.
  - The image has no power menu, battery, brightness, clock sync, time zone, USB mounting, second display, Bluetooth or sound.
  - There is no screen-reader mode or translation.
  - TermOS is installed only from release archives.
- **Decision:**
  - **Seven proposed milestones**, planned in [docs/plans/m13-os.md](docs/plans/m13-os.md):
    - **M13a:** links, hints, search across panes, and alerts when long commands finish;
    - **M13b:** layout files, a drop-down terminal, typing into several panes, saved commands, SSH hosts, pane recording;
    - **M13c:** a process manager, and the file manager's bookmarks, two panes, search, archives, git status, bulk rename and disk usage;
    - **M13d:** on the image, a lock screen, power menu, battery, brightness, lid, clock, USB drives, GPT disks and AMD graphics;
    - **M13e:** on the image, several displays, Bluetooth and sound;
    - **M13f:** accessibility and translations;
    - **M13g:** packages, a gallery, settings export and import.
  - **The suggested order:** M13a, M13b, M13c, then M13d and M13g, then M13e and M13f. The owner may reorder them or interleave them with M12c to M12e.
  - **Not decided:** the plan's designs are starting points. Each milestone's "Ask first" questions go to the owner before it starts, and the answers and the final design are logged then.
  - **Rules for every M13 milestone:**
    - privileged actions on the image are new `termos system` verbs, run through the existing sudo rule, with checked arguments;
    - devices, lids and displays arrive as events, not polling (D-028);
    - links open only `http`, `https`, `mailto` and `file` addresses;
    - exports never hold secrets;
    - recordings are readable only by the user.
  - **Moved out of "After 1.0":**
    - the lock screen, now in M13d;
    - the screen-reader mode, now in M13f.
  - **Still unscheduled:** multi-user seats, a console input method for Chinese, Japanese and Korean, printing, and a GPU-drawn console.
- **Alternatives:**
  - **Fewer, larger milestones:** harder to review and ship in pieces.
  - **The bare-metal work first:** it fixes the security gap sooner, but helps fewer users than the hosted features.
  - **Leaving accessibility and translations after 1.0:** they are cheaper before more UI is added.

---

## D-118: M13a: links, hints, search and finished commands
- **Status:** Accepted · 2026-11-05
- **Context:** D-117 planned M13a. The owner chose the plan's answers to its "Ask first" questions: links open on the client's machine; a finished long command is loud only when it failed; `u` for hints and `/` for search.
- **Decision:**
  - **Links in the grid and on the wire (`termos_render::link`, protocol 6):**
    - `Cell` gains `link: u32`, an id in its grid's `LinkTable` (0: none). The diff compares it with the rest of the cell; a wide character's two halves carry it.
    - **Stable ids:** ids are never reused for another address within a table. The server carries each client's table from frame to frame, and drops unused addresses only past 256 (at most 1024 at once). So an unchanged id still means the same address, and the diff can compare ids alone.
    - `FrameUpdate::links` sends the whole table when it changed, as `images` does. The client's encoder writes linked runs inside OSC 8 (closing at a change of link and at the end of each frame), so the outer terminal shows and opens them.
    - **What is kept:** `http`, `https`, `mailto` and `file` addresses, without control characters, at most 2 KiB. TermOS's own anchors (`termos-image:`, `termos-block:`) and every other scheme are left out.
    - **Where:** `[ui] links` (default on). Off for clients whose terminal is `linux` or `dumb`, which might print the sequence, and for the KMS console, which underlines a program's link while the pointer is over it.
  - **Finding links and paths (`termos_term::links`):**
    - **What it finds:** web and email addresses, git hashes (7–40 lowercase hex digits with a letter), IPv4 and IPv6 addresses, and paths with `:line[:col]`, `(line[,col])` or Python's `", line N"`.
    - **Patterns:** `regex` over the rows in view, with rows the terminal wrapped joined (`VtBackend::view_rows`, which also gives the programs' links and each character's column).
    - **Paths:** a path counts only if it exists, relative to the pane's folder (OSC 7, else its foreground program's); a diff's `a/` and `b/` are tried without. At most 256 lookups per scan, remembered for 10 s.
    - **When:** only on Ctrl-hover, Ctrl+click and hint mode, never per frame.
    - Fuzzed as `link_detect`.
  - **Clicking:** Ctrl+click opens the program's link or the match under the pointer; with Ctrl held, it is underlined (`ClientUi::link_hover`, drawn over the pane). Over a program that reads the mouse, Ctrl+Shift+click (Shift keeps the mouse for TermOS, D-085).
  - **Opening (owner: on the client's machine):**
    - **Web and mail addresses:** `ServerMsg::Open {url}` to the client that clicked, which runs `xdg-open`, `open` or `explorer.exe` with the address as one argument (no shell) after checking its scheme again.
    - **Where that cannot work, the address is copied and a toast says why:** the KMS console (no browser) and a client over SSH (`Caps::remote`: the client runs on the remote host; the outer terminal still opens the OSC 8 link itself).
    - **`file:` addresses** that name this machine (no host, `localhost`, or its name) open in TermOS; others are copied.
    - **Paths:** with a line, `file.edit PATH:LINE[:COL]`; without, `file.open` (folders in Files, pictures and Markdown in their viewers, the rest in the editor).
    - **Hashes and IP addresses** are copied.
  - **The editor at a line:** `file.edit` splits `PATH:LINE[:COL]` unless the whole names a file. The arguments come from the editor's name: Vim and Neovim `+LINE` (`+call cursor(L,C)` with a column), Helix, Sublime Text and Zed `FILE:L:C`, VS Code and its kin `-g FILE:L:C`, nano `+L,C`, Emacs, micro and Kakoune `+L:C`, others the file alone. `termos edit` (TermOS's Neovim or Helix, by `[dev] editor`) gets the same.
  - **Hint mode (`pane.hints [url|path|hash|ip|any]`, prefix `u`, `termos_shell::hints`):**
    - Labels from `[hints] alphabet` (default `asdfghjkl`): one letter each while there are enough letters, else all of the same length so none starts another. The bottom match gets the first label.
    - A label opens its match, Shift and the label copies it, Ctrl and the label pastes it into the pane; Esc leaves.
    - Drawn over the pane like copy mode, per client.
  - **Search across panes (`search.all [--all-sessions]`, prefix `/`, `termos_shell::search`):**
    - A box like the palette over every terminal pane of the session, history and screen.
    - **Matching:** plain text, or a regular expression after a leading `/`; case-folded unless the query has a capital.
    - **How it runs:** the panes' lines are copied when the box opens (`VtBackend::logical_lines_at`: wrapped rows joined, with the row each starts on), and each query runs on a thread over that copy, stopping at 500 matches and waking the desktop.
    - **Going there:** results are grouped by pane. Enter focuses the pane and enters copy mode at the match's row and column (worked out from the line's width), with the match selected. With `--all-sessions` the server gathers every session's panes and moves the client to the match's session.
  - **When a long command finishes (owner: loud for failures only):**
    - Blocks record when output started (`C`) and when the command finished (`D`); the emulator reports `VtEvent::Finished {block, exit, ran}`.
    - **When:** a command that ran `[notifications] long-command` seconds or more (default 15; 0 turns it off) and finishes where no client is looking: off the active workspace, or every client's terminal has lost focus (focus reports), or no client is attached.
    - **What:** the pane's attention becomes `Done` (✓ on its tab) or the new `Failed` (✗ in the error colour). It is quiet like `Done` and goes once its workspace is seen; an agent waiting for the user stays the louder mark. A toast and a center entry say what ran, where, its status and how long (``"`make test` in 1:api failed (2) after 3m12s"``), with the command line from the block.
    - **Failures:** a system notification too, if `[notifications] system`. Successes stay quiet.
  - **`pane.notify-when-done`** (pane menu, `tos notify --when-done`): the command running in the pane, or the next one, is announced whatever its length and wherever the user looks. `tos notify --when-done` typed alone waits for the next command. Without shell integration (no OSC 133 seen in the pane) it fires once the pane has been quiet for 5 s after output: a deadline in the desktop's timers, only while armed.
- **Alternatives:**
  - **The link table:** a link's address in every cell (16 bytes more per cell and the address on every changed cell), or ids renumbered each frame (every linked cell resent whenever a link appears above it).
  - **Opening over SSH:** running the opener on the remote host anyway (no browser there, or the wrong machine's).
  - **Finished commands:** loud for both (a toast for every build is noise); detection without shell integration by output timing alone (wrong for quiet long commands).

---

## D-119: M13b owner answers: TermOS's TOML layouts, prefix ` for the drop-down, recordings of output only
- **Status:** Accepted · 2026-11-05
- **Context:** M13b's "Ask first" questions (D-117, docs/plans/m13-os.md).
- **Decision:** the plan's answers, chosen by the owner:
  - **Layout files** are TermOS's own TOML, like the rest of its settings, not Zellij's KDL.
  - **The drop-down terminal** opens with prefix `` ` ``; `[quick] key` can add a key without the prefix, none by default.
  - **Recordings** hold output only, never input. Echoed input is output, and the docs say so.
- **Alternatives:** KDL (compatible with Zellij, but a second format and parser); F12 without the prefix (taken from every program); input recorded behind a setting (passwords typed at prompts would land in files).

---

## D-120: Layouts and workspace tools
- **Status:** Accepted · 2026-11-05
- **Context:** M13b (D-117) with D-119's answers.
- **Decision:**
  - **Layout files (`termos_core::layouts`):**
    - **Where:** `layouts/NAME.toml` in the config folder (its tables at the top), or `[layout.NAME]` in a project's `termos.toml` found from the folder up; the project's comes first. Names are letters, digits, `-`, `_` and `.`.
    - **The format:** `workspace` (a tab name, or `[[workspace]]` tables for several), `cwd`, and `[[pane]]`s. Each pane after the first is split from the one before, or from `of` (1-based), with `split = right | down | float`, `size = "N%"` (its share of the room it splits with that pane), `command`, `exec`, `cwd`, `name` and `focus`.
    - **Checks:** TOML errors carry the file, line and column; others name the pane (a split first pane, an `of` that names no pane before, a bad size, `exec` without a command, two focused panes). At most 16 workspaces and 32 panes each. Fuzzed as `layout_parse` (parsed layouts write out and read back the same).
    - **Opening (`layout.open NAME [DIR]`, `termos layout open`):** one function (`Desktop::build_workspace`) builds every workspace, and worktree workspaces (D-114) use it too as the built-in `worktree` layout, so they cannot drift apart. Commands are typed into the pane's shell; `exec` runs one through the shell as the pane's program. The share comes from the new `termos_wm::layout::Layout::set_share`.
    - **Saving (`layout.save NAME`, `termos layout save`):** the active workspace's tiling tree becomes panes split one from another, in the order they reopen (each split's children after its first are split from the one before, with the share of the room left), then its floats. Folders relative to the first pane's, names, the focus, and the programs on `[session] restore-programs` as commands; other programs are named in a comment only, as snapshots do (D-033), and app windows become shells.
    - `termos layout list|edit` read and edit the files.
  - **The drop-down terminal (`quick.toggle`, prefix `` ` ``):** one shell per session, a float as wide as the screen and `[quick] height` (default 40%) tall from the top.
    - **Hiding** minimizes it (left out of the status line's minimized windows) and gives the focus back to the pane that had it.
    - **Showing** it on another workspace moves it there.
    - **Saved:** sessions save which pane it is (`SessionSnapshot::quick`).
    - **Animation:** it opens with the floats' animation, not a slide (a new animation was not worth it).
  - **Typing into several panes:** one set of panes per session. `workspace.sync` (prefix `S`) fills it with the workspace's terminal panes, or empties it; `pane.sync` adds or removes the focused pane.
    - **What goes to them:** keys and pastes into a pane of the set go to the others too, each encoded for its own program's modes, through `write_to_pane` (which also answers a waiting pane). Mouse events and app windows never.
    - **What shows:** a `SYNC` badge and an accent ring on the set's panes.
  - **Saved commands (`termos_core::snippets`):** `[[snippets]]` in the config and in a project's `termos.toml` (`name`, `command`, `run`, `where = pane | new-pane | float`).
    - **Parameters:** `{param}` and `{param:default}` are asked for one at a time in the status-line prompt (`PromptKind::SnippetParam`), the default typed in; `{{` and `}}` are braces.
    - **Typing:** the line is pasted (bracketed where the program asked), then Enter with `run`.
    - **The palette** lists `snippet: NAME`; `termos snippet list|run NAME [k=v…]` types into the calling pane through `pane.send-text`.
  - **SSH hosts (`termos_core::ssh_config`):** `Host` names in `~/.ssh/config` and its `Include`s (globbed, at most 8 deep, each file once), leaving out patterns and `Match` blocks.
    - **Where:** an SSH submenu in the TermOS menu and `ssh.open HOST` in the palette, recent hosts first (`<state>/ssh-recent`).
    - **Running:** each runs the user's own `ssh HOST` in a new pane, the host checked first: no option, no spaces. Fuzzed as `ssh_config`.
  - **Recording (`pane.record`, prefix `R`, `termos_core::cast`):** asciinema v2.
    - **What is written:** a header, `[t, "o", text]` for output (a UTF-8 sequence cut between reads waits for its end) and `[t, "r", "COLSxROWS"]` for resizes.
    - **Where:** in `[record] dir` or `<state>/recordings/`, the folder 0700 and the file 0600.
    - **While it runs:** `● REC` on the pane; the toast names the file when it stops.
    - **Playing:** `termos play FILE [--speed N] [--idle-limit S]` plays one to its terminal at the recorded times.
  - **Keys:** prefix `` ` `` (`quick.toggle`), `S` (`workspace.sync`) and `R` (`pane.record`). The pane menu has "Type here with the others" and "Record this pane", and the TermOS menu has "Drop-down terminal" and SSH.
- **Alternatives:**
  - **Saving layouts as snapshots' JSON:** exact, but not a file people write.
  - **A slide-in animation:** deferred.
  - **Sync per workspace:** one set per session is simpler and covers `pane.sync` across workspaces.
  - **Snippets' parameters in a form window:** the prompt is what renames already use.

---

## D-121: M13c owner answers: processes in the monitor, bulk rename in the editor, four archive formats
- **Status:** Accepted · 2026-11-05
- **Context:** M13c's "Ask first" questions (D-117, docs/plans/m13-os.md).
- **Decision:** the plan's answers, chosen by the owner:
  - **The process manager** is a view of the system monitor (`Tab`, `processes.open`), not a window of its own.
  - **Bulk rename** happens in the user's editor, as `vidir` does.
  - **Archives:** zip, tar, tar.gz and tar.zst only.
- **Alternatives:** a separate process window (a second sampler); a rename window with patterns; 7z and rar (more dependencies, and rar's decoder licence).

---

## D-122: Processes and files
- **Status:** Accepted · 2026-11-05
- **Context:** M13c (D-117) with D-121's answers.
- **Decision:**
  - **Process data (`termos_hw::procs`):** `ProcessInfo` (pid, parent, name, user, CPU, memory, run time, state, threads, command line, ports) beside the unchanged `ProcessSample`, through a new `Source::processes(ports)` (empty by default, so fixtures need nothing).
    - **Where it comes from:** the sampler's refresh also reads each process's command line once, and the table is read from that same refresh, so CPU figures stay right.
    - **User and threads:** from `/proc/PID/status` and `/etc/passwd` on Linux; elsewhere left out (the docs say so).
    - **Ports:** listening TCP sockets from `/proc/net/tcp` and `tcp6`, matched to processes through `/proc/PID/fd`; Linux only, and only while the view is shown.
  - **Process actions:** `act(pid, End | Kill | Pause | Resume)` through sysinfo's signals (Windows: terminate for end and kill; pause and resume refused), `renice(pid, ±1)` through rustix on Unix.
    - **Refused:** pid 0 and 1 and TermOS's own process.
    - **Not allowed:** another user's process, or raising a priority without privileges, gives the system's error; there is no sudo from here.
  - **The view (`termos_apps::procs`, the monitor):** `Tab` switches the monitor between its overview (unchanged) and the process view. The view has a filter (name, command line or pid), sorting (CPU, memory, time, pid, name), a tree by parent (keeping the parents of what matches) and a root process with its descendants.
    - **Asking first:** each action asks a y/n question unless `[processes] confirm = false`.
    - **Opening it:** `processes.open [PID]` opens the view; the pane menu's "Show its processes" passes the pane's PTY child.
    - **Readings:** the monitor's thread reads the table only when asked, while the view is shown.
  - **Files (`termos_apps::files`, with `archive`, `rename`, `du`, `gitstatus`, `findfiles`):** `FileManager` stays one pane. The new `Files` window holds one or two of them and a sidebar of places, and the desktop's `files.open` opens a `Files`.
    - **Marks:** Space marks, `*` inverts; copy, cut, trash, zip and `F5`/`F6` act on the marks, else the selected entry.
    - **Two panes:** `Tab` opens the second and switches; `F5` copies and `F6` moves the active pane's selection into the other's folder (through the other pane's worker). Extracting and zipping write into the other pane's folder when there is one, which then reloads. `[files] two-pane` starts with two.
    - **Places:** Home, `[files] bookmarks`, the trash (XDG on Linux, `~/.Trash` on macOS) in a sidebar from 100 columns, and in the `'` list. `b` adds or removes the folder, written into the config with toml_edit.
    - **Searches:** `?` (names) and `Ctrl+F` (text, the `grep` crates) run on the worker with the `ignore` walker. They honour `.gitignore`, look in hidden files only when shown, and stop at 1000. Results replace the listing; Enter opens a text match with `file.edit PATH:LINE` (D-118).
    - **Archives:** zip (the `zip` crate), tar, tar.gz (`flate2`) and tar.zst (`ruzstd`, new, MIT). They are listed with the folders they only imply and browsed read-only, and opening a member extracts it to a temporary folder first (then opened at the app's next timer). `X` extracts the archive, or the marked members below the folder shown; `z` zips. Absolute names and `..` are refused (counted as left out), and links and devices are skipped.
    - **Git:** `git status --porcelain=v2 -z --ignored=matching` on the worker after each listing of a folder in a repository, relative to `git rev-parse --show-prefix`. One mark per entry (`M A R D U ? !`), a folder taking the strongest mark inside it.
    - **Bulk rename:** `F2` on two or more marks writes `N<tab>name` lines to a temporary file and runs `file.edit` on it; `Enter` in Files reads it back. The checks: every line once, no empty name or separator, no two the same, no existing name unless it is renamed too (case-only renames allowed). The changes are shown, and on `y` they are done in two phases through temporary names, all or none. `u` undoes the last one. A property test checks that no file is ever lost.
    - **Disk usage (`D`):** its own thread, without following links, sizes each entry of the folder, biggest first with bars. A count shows through a half-second timer while it runs, and only then. Enter goes in, `h` up, `d` trashes.
    - **Open with (`O`):** TermOS's viewer (pictures, markdown), the editor, or the system's program. The system's program is the new `file.open-system PATH`, run by the server: `xdg-open`, `open` or `explorer`, the path as one argument. The choice is remembered per extension in `<state>/open-with` and used by Enter.
  - **Settings:** `[files] bookmarks`, `two-pane`; `[processes] confirm`.
- **Alternatives:**
  - **Two panes inside `FileManager`:** a larger rewrite, and its tests and snapshot would change.
  - **Bulk renames in one pass:** swaps would overwrite.
  - **A zstd binding to the C library:** C in the build.
  - **Disk usage on the worker:** it would hold up listings.

---

## D-123: M13d owner answers: the system user's password, no hosted lock, AMD firmware everywhere, 10 and 20 minutes
- **Status:** Accepted · 2026-11-05
- **Context:** M13d's "Ask first" questions (D-117, docs/plans/m13-os.md).
- **Decision:** the plan's answers, chosen by the owner:
  - **The lock's password** is the system user's (`/etc/shadow`), not a TermOS passphrase of its own.
  - **Hosted mode** has no TermOS lock: the host system's lock and power menu apply, and the commands say so.
  - **AMD graphics firmware** goes in every x86_64 image, not a separate image.
  - **Defaults:** lock after 10 minutes without input; on battery, suspend after 20.
- **Alternatives:** a TermOS passphrase (a second secret to keep and recover); a lock in terminals (it would only hide the desktop, the terminal around it stays open); an x86_64 image without AMD firmware (smaller, two images to test).

---

## D-124: The image as a computer: lock, power, laptops, clock, drives, GPT
- **Status:** Accepted · 2026-11-05
- **Context:** M13d (D-117) with D-123's answers. Supersedes D-075's MBR layout and fixed partition ids for x86_64 and aarch64-virt.
- **Decision:**
  - **The lock screen is the console's (`kms/lock.rs`, `kms/locking.rs`).** It draws the clock, the date and a password field into pixels of its own, so nothing of the desktop shows; frames from the server keep coming and are drawn again on unlocking. While locked the console sends the server no keys or mouse events and does not switch virtual terminals.
    - **When it locks:** the desktop's `session.lock` (prefix `L`, ◆ Menu → Power → Lock) sends the new `ServerMsg::Lock`; also the keyboard's lock key, `[lock] after-minutes` without input (one deadline, not a poll), the lid, and every suspend. `[lock] at-start` starts locked.
    - **Restarts:** a file in the runtime folder (`$XDG_RUNTIME_DIR/termos-locked`) marks the screen locked, so a console that init starts again comes back locked.
    - **The password check:** `sudo termos system check-password` reads the password on standard input and checks it against the user's `/etc/shadow` hash with RustCrypto's `sha-crypt` (sha512crypt and sha256crypt only). A wrong one waits a second in the verb; after three failures the lock waits 5 s, doubling up to five minutes. With no password set, Enter unlocks and the screen says so. The check runs on a thread; its answer comes back to the console's loop through a channel.
    - **Virtual terminals:** `termos system vt-lock on|off` (`VT_LOCKSWITCH`, which needs root) keeps the console's terminal while locked. The image has no getty, so the other terminals have no shell anyway.
    - **Not locked:** agents, servers, SSH sessions and other clients keep running and attached; the docs say so.
    - **Recovery:** the boot menu's "without the lock" entries add `termos.nolock`: nothing locks, and `termos password` sets a new password.
  - **Passwords:** `/etc/shadow` links to `/data/etc/shadow` (the root is read-only). A post-fakeroot script moves the built shadow to `/usr/share/termos/shadow`, and S11termos-data copies it to the data partition on first boot, adding entries for users a newer image brings.
    - **Setting one:** `termos system set-password` hashes with sha-crypt and replaces the file (written beside it, then renamed): `chpasswd` would write next to the link on the read-only root, so the plan's `chpasswd` is not used.
    - **First boot:** when no password is set, the console's lock screen asks for one (typed twice; Esc has none) before the desktop. A file in the state folder records that it asked. `termos password [--remove]` changes it later. The installer does not ask: the installed system's first boot does.
    - **Boot tests:** the QEMU tests' boot script leaves `/run/termos/boottest`, which turns off the idle lock and the first-boot question.
  - **Power (`desktop/power.rs`):** on an image, the TermOS menu's last entry is a Power submenu: Lock, Suspend, Restart, Shut down, Quit this session. Elsewhere the menu is unchanged and the commands say the host system has its own.
    - **Suspend:** the new `ServerMsg::Suspend` makes the console lock and then run `sudo termos system suspend`, which writes `mem` (or `freeze`) to `/sys/power/state`; the console sets its mode again when it wakes. With no console attached the server suspends directly.
    - **Restart and shut down** ask first; then the server saves every session's snapshot and runs `termos system restart` or `power-off`.
  - **Laptops:**
    - **Battery:** `termos_hw::power` reads `/sys/class/power_supply` (capacity, status, energy or charge, power or current, mains). It takes a directory for tests and skips devices' batteries (`scope` `Device`). macOS and Windows use `starship-battery` (new, ISC).
    - **The status line** shows `▮ 84%` or `⚡ 62%` before the clock, coloured when at 20% or less on battery. Only the real server reads it (`DesktopOptions::power`), so tests on a laptop match CI. It is read once a minute, and only on a machine with a battery.
    - **Low battery:** a toast at 20%, a system notification at 10%, and at `[power] critical` (5) a 30-second countdown to suspend (images only); plugging in starts again. The steps are a pure function, tested on readings.
    - **Brightness:** the console's brightness keys and `display.brightness ±N` write `/sys/class/backlight/*/brightness` (firmware, then platform, then raw; never below one step). A udev rule lets the group `video` write it. `[power] brightness` sets it at start.
    - **Lid and idle:** libinput's lid switch does `[power] lid` (`suspend`, `lock`, `nothing`). On battery the console suspends after `[power] suspend-after-minutes` without input; a machine without a battery never arms it, and on mains power it looks again after the same time.
  - **Clock:**
    - **Time zones:** the image carries tzdata. `/etc/localtime` links to `/data/etc/localtime` (UTC at first), which `termos system set-time-zone ZONE` replaces after checking the name and that the file is a compiled zone (`termos_core::timezones`).
    - **Settings:** the Clock group has the time format (`[clock] time-format`, 24h or 12h, also used by the lock screen) and a time zone list to search, which runs `clock.zone ZONE`.
    - **Time sync:** BusyBox `ntpd` from init (S48termos-ntpd), with the servers `termos system ntp-servers` reads from the user's `[clock] ntp-servers`, each checked.
  - **USB drives:**
    - **Listing them:** `termos_hw::drives` lists removable or USB block devices from sysfs, with labels and file systems from udev's database and mount points from `/proc/mounts`.
    - **Hearing them come and go:** a thread listens on udev's netlink group (rustix sockets; any user may), and the server posts a toast for a drive with a file system. The plan's udisks2 D-Bus API is not used; on another Linux, Files runs `udisksctl`.
    - **In Files:** drives are among the places; Enter mounts and goes there, `e` ejects (leaving the folder first). On the image `termos system mount DEV` mounts a listed drive at `/media/LABEL` (a tmpfs) with the user's ids, for vfat, exFAT, ext4 and NTFS (`ntfs3`), never set-user-id or devices; `unmount` syncs and unmounts only under `/media`. macOS's `/Volumes` and Windows's drive letters are places too.
  - **Disks:** x86_64 and aarch64-virt images are GPT, named `boot`, `rootfs-a`, `rootfs-b`, `data` (x86_64 adds `bios`, a BIOS boot partition for GRUB's core image). post-image.sh points boot.img and the core image at sector 2048, as grub-bios-setup would.
    - **Finding the partitions:** GRUB passes `root=PARTUUID=` of the slot on its own disk (`probe --part-uuid`). S11termos-data finds that disk's partitions by name, or on MBR disks (Pis, disks installed before) by number, and writes them to `/run/termos/disk`. S80rauc writes RAUC's configuration from a template with those devices.
    - **Ids:** fixed in the image, for reproducible builds. `termos system install-disk` gives a GPT disk new disk and partition GUIDs (`sfdisk --disk-id`, `--part-uuid`) after moving the backup table to the disk's end. The data partition grows as the last partition, whatever its number.
    - **Old disks:** existing installs keep their MBR disks and boot menus; updates change neither, and the new scripts find their partitions by number.
  - **AMD graphics:** the x86_64 kernel builds amdgpu as a module (its firmware is on the root, which a built-in driver would start before), with linux-firmware's amdgpu files and kmod, plus ACPI's battery, mains, lid and video. Every board's kernel adds suspend, the backlight class, exFAT, NTFS3 and GPT partitions.
  - **Protocol:** `ServerMsg::Lock` and `Suspend` join protocol 6 (M13a–M13d ship together); a terminal client ignores both.
  - **Settings:** `[lock] after-minutes`, `at-start`; `[power] lid`, `suspend-after-minutes`, `critical`, `brightness`; `[clock] ntp-servers`, `time-format`.
- **Alternatives:**
  - **The lock in the server:** the server would draw it, but keys would already have reached it, and another client could see the desktop.
  - **`chpasswd`:** it cannot write through the link on a read-only root.
  - **Kernel uevents instead of udev's:** they arrive before udev has read the label.
  - **udisks2 over D-Bus (`zbus`):** more code for what `udisksctl` already does, and not on the image.
  - **Partitions by `PARTLABEL` alone:** two TermOS disks in one machine would both match.
  - **grub-bios-setup at build time:** it wants a block device.

---

## Open questions (the owner will be asked before the milestone that needs each one)

| ID | Question | Needed by | Current lean |
| --- | --- | --- | --- |
| O-1 | Default WM keybinding scheme: a tmux-style leader key, or modifier chords (Alt/Super)? | M2 | **Resolved by D-023:** tmux prefix `C-b`. |
| O-2 | PTY layer: `portable-pty`, `pty-process` or direct `rustix` openpty with tokio `AsyncFd`? | M1 | **Resolved by D-025:** `portable-pty`. |
| O-3 | Git UI: embed lazygit/gitui in a window, or build our own panel? | M5 | **Resolved by D-053:** embed lazygit. |
| O-4 | LLM clients: provider SDK crates (async-openai, others) or our own thin reqwest clients behind `Provider`? | M6 | **Resolved by D-058:** our own thin clients with shared SSE parsing. |
| O-5 | Secret store on bare metal, where there's no Secret Service: an age-encrypted file unlocked by passphrase, a TPM-sealed key, or something else? | M6 | **Resolved by D-058:** an age file with a passphrase; TPM later. |
| O-6 | llama.cpp integration: a managed `llama-server` subprocess or FFI (`llama-cpp-2`)? | M7 | **Resolved by D-065:** `llama-server` subprocesses. |
| O-7 | Rust-native inference: ONNX Runtime (`ort`), candle, or both? | M7 | **Resolved by D-065:** candle first (embedding models, D-067). |
| O-8 | WASM engine for widgets: wasmtime (component model, JIT, larger) or wasmi (interpreter, small)? | M4 | **Resolved by D-042:** wasmtime. |
| O-9 | KMS font rasterizer: swash or fontdue? | M8 | **Resolved by D-072:** swash. |
| O-10 | Init system: BusyBox init, s6, or systemd? | M8 | **Resolved by D-072:** BusyBox init. |
| O-11 | Network management: connman, NetworkManager, or iwd + a small DHCP client? | M8 | **Resolved by D-072:** iwd + dhcpcd, with a TermOS settings page. |
| O-12 | A/B updates: RAUC or SWUpdate? | M9 | **Resolved by D-078:** RAUC. |
| O-13 | Hosted Linux binaries: glibc 2.31 floor only, or also static musl? | M9 | **Resolved by D-079:** both, while musl keeps the budgets. |
| O-14 | Bootloader for generic aarch64 UEFI: GRUB2 or systemd-boot? | M8 | **Resolved by D-072:** GRUB2, shared with x86_64. |
