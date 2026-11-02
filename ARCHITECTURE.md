# TermOS architecture

This is a living document. It describes the system we're building and is
updated whenever the design changes. "D-xxx" refers to entries in
[DECISIONS.md](DECISIONS.md) and "Mn" to milestones in
[ROADMAP.md](ROADMAP.md). Anything not built yet is described as *planned*.

**Current state (M1):** the hosted, tmux-style desktop works. That covers
the renderer, PTY panes, the window manager with floats, workspaces, the
palette, which-key, copy mode, config with hot reload, the tutorial and
notifications. Server and client run in one process; the socket arrives in
M3.

---

## 1. Goals and non-goals

**Goals**
- A desktop whose entire UI is a terminal and that feels like a GUI desktop: movable, resizable and overlapping windows, workspaces, a launcher, a command palette, notifications and built-in apps.
- High-end terminal graphics that degrade gracefully: truecolor, inline images, charts, animations, shadows and rounded borders.
- Developer-first: toolchains, editor, LSP/DAP, Git and containers are built in.
- AI-native: agents in windows, a strict permission model, local inference.
- One codebase that runs **hosted** (inside any terminal) and **bare metal** (Linux + DRM/KMS).
- Meet the performance budgets in §15.

**Non-goals**
- Writing a kernel, a display server, or GPU compositing (D-001).
- Checkpointing and restoring processes across reboots (D-011).
- Bare metal on anything but Linux. (Hosted mode runs natively on Windows since M3.5, D-035; WSL2 counts as Linux.)
- Being a general-purpose distro. The bare-metal base is an immutable appliance; general packages live in the dev rootfs (D-004).

---

## 2. Run modes

| | Hosted mode | Bare-metal mode |
| --- | --- | --- |
| Runs on | Any terminal on Linux, macOS, WSL2; Windows Terminal on Windows 10 1809+ (D-035) | Linux + Buildroot image, booting straight into TermOS |
| Frontend | Terminal client: escape sequences via crossterm | KMS client: pixels into a DRM framebuffer |
| Input | The terminal's input stream (keys, SGR mouse, Kitty keyboard protocol) | libinput + xkbcommon through libseat |
| Glyphs | Drawn by the host terminal | Our own font rasterizer (O-9), adjustable size |
| Images | Kitty → Sixel → iTerm2 → Unicode blocks/braille (§5.8) | Drawn natively as pixels |
| Packages/toolchains | Host package manager + mise | Debian dev rootfs + mise (D-004) |

Everything above the frontend is shared: server, window manager, shell, apps,
agents and models. A frontend is only a *client* of the server (D-007).

---

## 3. Process model

```
 ┌──────────────────┐  input ─▶          ┌───────────────────────────────────────────────┐
 │ terminal client  │◀─ frames ──────────│                 termos server                 │
 │ (hosted)         │                    │                                               │
 └──────────────────┘   Transport        │  shell/Desktop ─ wm ─ compositor ─ per-client │
 ┌──────────────────┐   M1: in-process   │        │          │                  diffs    │
 │ KMS client       │◀─ channel          │      apps       term: PTY + VT ──────────────┼──▶ child processes
 │ (bare metal)     │   or local socket  │   (in-process)                               │    (shells, nvim,
 └──────────────────┘                    │      ai: agent loops ─ permission engine ────┼──▶  CLI agents …)
 ┌──────────────────┐                    │                                               │
 │ headless client  │◀───────────────────│  JSON-RPC control API ◀── termos CLI, scripts │
 │ (tests)          │                    │                       ◀── out-of-process plugins
 └──────────────────┘                    └───────────┬──────────────────────┬────────────┘
                                                     │ OpenAI-compatible    │ stdio / HTTP
                                                     ▼ HTTP                 ▼
                                   ┌───────────────────────────┐   ┌──────────────────────┐
                                   │ termos models serve       │   │ MCP servers           │
                                   │ (router) ─▶ llama-server, │   │ sandboxed tool procs  │
                                   │   Python sidecar, …       │   │ (bwrap + landlock)    │
                                   └───────────────────────────┘   └──────────────────────┘
```

**Processes**
- **`termos`:** with no arguments it connects to the default server, spawning one if needed, and attaches a terminal client to the session `main` (D-029). Other subcommands (`new`, `attach`, `ls`, `msg`, …) are short-lived clients of the same server.
- **`termos server`:** the daemon, one per user per socket name (like tmux `-L`). It owns all sessions, panes and their programs (D-030, D-032).
  - It is started on demand with null stdio, calls `setsid`, and stops when its last session ends or on `kill-server` / SIGTERM, saving snapshots first (D-033).
  - On Windows it starts without a console window, in its own process group and outside the starting terminal's job, and treats console close, logoff and shutdown events like SIGTERM (D-037).
  - One server holds many sessions (one `Desktop` each). Each session can be shown by many clients; each client has its own overlays (palette, prompts, which-key).
- **PTY children:** anything that runs in a terminal window. On Windows they run in a ConPTY pseudo-console (D-039).
- **`termos models serve`:** a separate process (D-021) so inference can't crash or bloat the desktop. It manages backend worker processes.
- **MCP servers and agent tool executions:** spawned inside the sandbox (§11).
- **Out-of-process plugins:** launched by the server from their manifest, or connecting on their own (§9).

---

## 4. Crate map

| Crate | Responsibility | First milestone |
| --- | --- | --- |
| `termos-render` | `Grid`/`Cell`/colour, frame diffing, themes, capability detection, backends (terminal, headless, KMS) | M1 |
| `termos-core` | tokio event loop and frame scheduler, in-process bus, `Transport`, protocol, config, paths | M1 |
| `termos-term` | PTY spawning/resizing, `VtBackend` over alacritty_terminal, blitting VT cells into the grid | M1 |
| `termos-wm` | Window tree, floating + tiling layouts, workspaces, z-order, hit testing, composition of window chrome | M1 |
| `termos-shell` | `Desktop` state, the session server (socket, sessions, clients, control API), key tables, command palette, which-key, status bar, copy mode, notifications, tutorial, snapshots, plugin host | M1 |
| `termos-apps` | Built-in apps on the in-process `App` trait: image and markdown viewers, file manager, system monitor, clipboard manager, settings | M4 |
| `termos-devkit` | mise-backed toolchains, Neovim/Helix + LSP/DAP, dev rootfs, Git/build panels | M5 |
| `termos-ai` | Providers, agent loop, tools, permission engine, sandbox, audit, MCP, secrets | M6 |
| `termos-models` | HF adapter, fit estimation, inference backends, OpenAI-compatible router | M7 |
| `termos-image` | Buildroot external tree, image build driver, QEMU boot tests | M8 |
| `termos-cli` | The `termos` binary: server, clients, every subcommand | M0 |
| `termos-plugin` | Plugin protocol types and a blocking Rust SDK (D-034); WIT definitions for widgets later (M4) | M3 |
| `termos-hw` | CPU, GPU, memory, temperature, disk, network and process readings (sysinfo plus sysfs and NVML probes, D-050), shared by the system monitor and models | M4 |
| `xtask` | Repo automation: `ci`, `check-todos`, `todos` | M0 |

**Dependency direction.** These are the edges we intend. There are no cycles, and a
crate never depends on one listed above it.

| Crate | May depend on |
| --- | --- |
| `termos-cli` | everything below. It wires the window factories from apps, ai and devkit into the shell. |
| `termos-shell` | wm, term, apps, plugin, core, render |
| `termos-ai` | term (PTY-backed `run_command`), core, render. It reaches `termos-models` only over HTTP. |
| `termos-devkit` | term, core, render |
| `termos-apps` | hw, core, render |
| `termos-models` | hw, core |
| `termos-wm`, `termos-term` | core, render |
| `termos-hw` | (system crates only) |
| `termos-core` | render |
| `termos-render` | (leaf) |
| `termos-plugin` | (leaf: serde only, so plugins stay small) |
| `termos-image` | build tooling; not linked into `termos` |

The in-process `App` trait (§9) lives in `termos-core`. That lets apps,
agents and devkit panels each provide windows without depending on the shell.
Agent windows reach the desktop through `termos_core::app::Launcher`, which
the `termos` binary gives the server (D-059), so the shell never depends on
termos-ai.
Frontends depend only on `termos-render` and `termos-core`, which keeps the
client small.

---

## 5. Rendering pipeline

```
sources ──▶ compose (per client) ──▶ Grid ──▶ diff vs last-sent Grid ──▶ encoder ──▶ output
 PTY VT grids                          │                                   terminal: escape seqs (sync update)
 app ratatui buffers                   └── same Grid feeds ──────────────▶ KMS: rasterize changed cells
 plugin surfaces, shell chrome
```

### 5.1 Cell model
A `Cell` holds:
- a grapheme cluster, stored inline for the common short case;
- a display width (1, or 2 with a continuation cell);
- `fg`, `bg` and underline colour;
- attribute flags: bold, dim, italic, underline style, blink, reverse, hidden, strikethrough;
- an optional hyperlink ID (OSC 8).

Colours are `Default | Indexed(u8) | Rgb(u8,u8,u8)`. The terminal default is
kept as its own value so transparent or themed terminal backgrounds still work.

### 5.2 Sources
- **PTY windows:** the VT grid from `alacritty_terminal` (D-008).
- **Built-in apps and shell overlays:** ratatui buffers (D-009).
- **Plugin windows:** cells sent over JSON-RPC.
- **Images:** placed as objects (§5.8), not as cells.

### 5.3 Composition
For each attached client, at its size:
1. Desktop background.
2. The tiled layer of the active workspace.
3. Floating windows, bottom to top.
4. Drop shadows. With truecolor, the cells under a floating window's offset rectangle are blended toward the theme's shadow colour; otherwise a dim attribute is used.
5. Borders and title bars, drawn with rounded box characters (`╭╮╰╯`).
6. Overlays: palette, which-key, toasts.
7. The cursor.

Each window is clipped to its rectangle, and occluded regions are never drawn.

### 5.4 Damage tracking (two levels)
1. **Source damage.** Each window reports dirty rows (from alacritty's damage API or the app's redraw flag). A clean window whose geometry hasn't changed is not recomposed.
2. **Output damage.** The new grid is diffed against the last grid sent to *that* client, producing row spans. The encoder emits the shortest cursor moves and SGR changes. A full redraw happens only on attach, resize or theme change.

Snapshot tests assert both levels. For example, a single echoed character must produce output for exactly one cell.

### 5.5 Frame scheduling (D-026, D-028)
Rendering is event-driven, with no fixed tick.
- Any change marks the desktop dirty.
- A small change (keys, mouse, typing echo, or under 4 KB of pane output since the last frame) is drawn **immediately**.
- During an output flood (`cat` of a huge file), PTY output is parsed continuously but drawn at most once per 8 ms. The reader's bounded channel slows the child instead of buffering without limit.
- Output from panes that aren't visible updates their VT but never triggers a redraw.
- While idle no timers are armed, except the optional once-a-minute status clock. Which-key, toast and sync-update timers exist only while active.
- Animations (M4) will register short-lived tickers that stop when they finish, and a reduced-motion setting will disable them.

### 5.6 Terminal encoder
- crossterm handles raw mode, the alternate screen, mouse and keyboard modes, and bracketed paste.
- Each frame is one buffered write wrapped in synchronized-update brackets (DEC mode 2026) when the terminal supports them.
- Colours are downsampled truecolor → 256 → 16 according to the client's capabilities.

### 5.7 Capability detection (D-043)
1. **Environment heuristics:** `COLORTERM`, `TERM`, `TERM_PROGRAM`, `KITTY_WINDOW_ID`, `WT_SESSION`, `COLORFGBG`, `SSH_CONNECTION`, and whether we're inside tmux or screen. `TERMOS_COLOR`, `TERMOS_SYNC`, `TERMOS_GRAPHICS` and `TERMOS_REDUCED_MOTION` override.
2. **Active queries** (`termos_render::probe`), sent at attach as one batch terminated by DA1: terminals answer in order, so the DA1 reply marks the end. The client reads stdin directly for up to 200 ms (800 ms over SSH); keys typed meanwhile are passed on. `TERMOS_PROBE=0` skips it, and Windows consoles use the environment only. The batch:
   - XTVERSION (`CSI > q`);
   - the Kitty graphics query (`APC G a=q`);
   - DECRQM `?2026`;
   - the Kitty keyboard query (`CSI ? u`);
   - cell and text-area pixel size (`CSI 16 t`, `CSI 14 t`);
   - the background colour (OSC 11, for the `auto` theme);
   - DA1 itself, whose attribute 4 means Sixel.

The result is `Caps` v2 (graphics, pixel sizes, background, terminal name,
`in_tmux`, `remote`, `reduced_motion`), sent in the client's `Hello`.
`termos debug caps --raw` shows it and the raw replies, which become test
fixtures.

### 5.8 Graphics fallback ladder (D-047, D-048)
Kitty graphics → Sixel → iTerm2 inline images → Unicode quadrant blocks
(2×2 pixels per cell, two colours) → braille (for line art and charts).

Images come from apps (`App::images`) and from programs in panes (Kitty
graphics and Sixel, intercepted before the emulator and anchored to their
text). For each client and image the desktop decides how to show it:
- **Blocks** are drawn into the cell grid, so windows and overlays cover them without any special handling.
- **Kitty** gets one placement per visible piece: the image minus the floats above it and the palette and which-key boxes, each a crop of a single upload.
- **Sixel and iTerm2** can't be partly covered. They're used only for an image that is wholly visible, not on the last row, while nothing moves; otherwise the visible pieces are drawn as blocks. We choose correctness over fidelity.
- The server uploads each image once per client and size; the client encodes and draws pixel images after the cells. A Sixel or iTerm2 image that moves is painted over by poisoning the client's previous grid, so the next diff redraws those cells.
- The KMS backend draws images natively (M8).

### 5.9 KMS backend (planned, M8)
- **Input and buffers:** it consumes the same `Grid` and rasterizes cells into DRM dumb buffers on the CPU (no GPU needed), double-buffered with page flips.
- **Glyphs:** a glyph atlas cache, a configurable font, and adjustable size for accessibility.
- **Damage:** only the rectangles of changed cells are re-rasterized.

---

## 6. Window manager

```
Server
 └─ Session  "main"                        ← what clients attach to
     └─ Workspace 1..n                     ← virtual desktops
         ├─ Tiling tree: Split{dir, ratios} | Tabs | Stack  → Window leaves
         │                layout per workspace: master-stack | grid | bsp | manual
         └─ Floating layer: [Window + rect] in z-order
Window = { id, title, source: Pty | App | Plugin | Agent, state: normal | minimized | maximized | fullscreen }
```

- **Z-order:** the tiled layer sits below the floating layer. Focusing a floating window raises it. Overlays and notifications are always on top.
- **Input routing:** decoded input goes, in order, to:
  1. open shell overlays (palette, which-key);
  2. global bindings;
  3. the focused window. Keys are re-encoded in the mode the child asked for: legacy or Kitty keyboard protocol.
- **Mouse:**
  - SGR mouse reporting (1006) with motion tracking (1003).
  - Hit tests run top-down. Dragging a title bar moves the window; dragging an edge or corner resizes it; clicking focuses; the wheel scrolls scrollback.
  - When a child has enabled mouse reporting, events inside its content are forwarded with translated coordinates, in the child's encoding.
- **Snapping:** drag a window to a screen edge or corner to get halves or quarters; snap commands do the same from the keyboard. Minimized windows go to a task strip in the status bar.
- **Commands:** every action is a named command, such as `pane.split-right`, `pane.resize-left 5` or `layout.select tiled`. Commands can be bound in config, run from the palette (Ctrl+Space or prefix `:`, fuzzy-matched) and called over JSON-RPC (`termos msg pane.split-right`). The which-key overlay lists the bindings available after the prefix.
- **tmux-style keys (D-023):** prefix `C-b`, then tmux's default keys (`%`, `"`, arrows, `z`, `x`, `c`, `n`, `p`, `0`–`9`, `[`, `]`, …), plus TermOS keys for floats (`f`, `F`, `m`, `M`). Resize keys repeat without the prefix for `repeat-time-ms`. A TermOS *workspace* is a tmux *window* (a tab in the status line); a *pane* is a tmux *pane*.
- **Tiling (D-024):** tmux-style panes separated by one-cell lines, sized by weights that respect each subtree's minimum extent. Presets are even-horizontal, even-vertical, main-vertical (master-stack), main-horizontal and tiled (grid). Splits that wouldn't fit are refused.
- **Copy mode:** vi motions over the scrollback, selection, search, and yank to the paste buffer and the system clipboard (OSC 52). The mouse wheel and drag-select use it too.
- **PTY resize:** resizing a terminal window issues `TIOCSWINSZ`, which sends SIGWINCH to the child.
- **Multiple clients:** when clients of different sizes attach, the session size follows `latest` (default), `smallest` or `largest`, as tmux's `window-size` does.

---

## 7. IPC and protocol (D-007, D-010, D-031)

- **Transport:** frontends see a `ClientEnd` (two channels). It is backed either by the server directly, in-process (tests, embedding), or by a local socket. Everything above `termos_core::local` (D-036) is the same on every platform:
  - **Unix:** a Unix socket at `$XDG_RUNTIME_DIR/termos/<server>.sock`, in a 0700 directory that is owner-checked, falling back to `$TMPDIR/termos-<uid>/`; the socket itself is mode 0600.
  - **Windows:** a named pipe `\\.\pipe\termos-<hash of the runtime directory>-<server>`, whose DACL admits only the current user and which rejects remote clients.
  - `-L NAME` picks the server by name and `-S PATH` by path (a pipe path on Windows).
- **Authentication:** the server checks the peer's identity and rejects anyone else with error `-32003`: the uid on Unix (`SO_PEERCRED` / `getpeereid` through `UnixStream::peer_cred`), the user SID of the client's process on Windows. On Windows the client also checks the server process's SID before sending anything, against pipe-name squatting (D-036).
- **Handshake:** every connection's first line is a JSON-RPC request:
  - `hello {version, kind: frontend | control | plugin, client, session?, create?, token?, cwd?}`.
  - The reply is `{version, server, pid, session}`, or an error: `-32001` version mismatch, `-32004` no such session, `-32003` forbidden.
- **Frontend stream:** after the reply, frames are a little-endian u32 length plus a postcard body (at most 16 MiB). These are the serde types in `termos-core::proto`:
  - Server → client: `Frame(FrameUpdate { cols, rows, clear, spans, cursor, images })`, `ImageData { id, chunk, last }`, `ImageFree { id }`, `Clipboard`, `Bell`, `Title`, `Detached { session }`, `Exit { reason }`. `images` is the client's whole placement list, sent only when it changed.
  - Client → server: `Hello { version, caps, cols, rows }` (first), `Key`, `Mouse`, `Paste`, `Resize`, `Focus`.
  - **Backpressure:** a client gets its next frame only after the previous one was written to it (D-032).
  - **Versions (D-043):** frontends speak protocol 3; control clients and plugins speak JSON-RPC version 1. Each is checked in the `hello` for its kind of connection.
- **Control stream:** newline-delimited JSON-RPC 2.0 (lines up to 4 MiB). It's used by `termos msg …`, `termos ls` and the other subcommands, and by scripts. The v1 methods:
  - server: `server.info`, `server.kill`, `config.reload`;
  - sessions: `session.list/new/kill/rename/restore/saved`;
  - clients: `client.list/detach`;
  - panes: `pane.list/send-text/send-keys/kill`;
  - other: `notify.send`, `command.run`, `plugin.list`.
  - Every named command and plugin command is also a method of its own name.
  - Event subscriptions (`subscribe`) are not built: nothing needs them yet (the agents of M6 run inside the server).
- **Plugin stream:** JSON-RPC as well; see §9 and D-034.
- **Inside panes:** `TERMOS=<socket>,<server pid>,$<session id>` lets CLI commands run in a pane find their server and session.
- **Over SSH:** `ssh -t host termos` runs a client on the remote host and attaches to (or starts) that host's server; detaching leaves everything running there. Later, a local client could attach to a remote server through a forwarded socket so that local capabilities such as Kitty graphics apply; the protocol is transport-agnostic.

---

## 8. Configuration, themes, persistence

- **Configuration:** `$XDG_CONFIG_HOME/termos/config.toml`, plus `themes/*.toml`.
  - Validation errors point at the exact span.
  - Hot reload uses `notify` with debouncing. An invalid edit keeps the last good config and posts a notification with the error.
  - `termos config check` validates without running.
- **Themes (D-044):** semantic tokens such as `fg`, `accent`, `border`, `status-*` and `overlay-bg` from which every style is derived. Presets dark, light and high-contrast; `[theme] base = "auto"` follows each client's terminal background (OSC 11, then `COLORFGBG`). Themes are per client; `theme.select` switches one. Programs asking for default colours (OSC 10/11) get the terminal's.
- **Motion (D-042, D-045):** `[ui] animations` and `reduced-motion`; never over SSH.
- **Graphics:** `[graphics] protocol = "auto" | "kitty" | "sixel" | "iterm2" | "blocks"`.
- **Clipboard:** `[clipboard] history` (entries kept in memory, default 50; 0 keeps none).
- **Widgets:** `[widgets.<name>]` for WASM status-line widgets (§9, D-051).
- **Agents (§11):** `[agents]` (providers, profiles, MCP servers, external agents) and `[secrets] store`; keys are named (`keyring:<name>`), never written.
- **Settings app:** writes config.toml itself (toml_edit, comments kept), only after the result parses as a config.
- **Session persistence (D-011, D-033):**
  - **Where:** snapshots go to `$XDG_STATE_HOME/termos/sessions/<server>/<session>.json`, with a versioned schema, mode 0600, written atomically.
  - **What:** the layout of every workspace and, per pane, the directory, the foreground program, and optionally (`save-scrollback`) the text. Command lines are kept only for allowlisted programs.
  - **When:** debounced after structural changes and at shutdown. They are deleted when a session ends on purpose.
  - **Restore:** a fresh server restores them by policy, `always | ask | never` (default ask). Shells restart in their directories and allowlisted programs (`restore-programs`) are re-run.
- **Logs:** `$XDG_STATE_HOME/termos/log/` (D-017).
- **On Windows** (D-038): config lives in `%APPDATA%\termos`, state (snapshots, logs) in `%LOCALAPPDATA%\termos`; the `XDG_*` variables still override both. The default shell is `pwsh`, then `powershell`, then `%COMSPEC%`. Snapshots name no foreground programs there yet.

---

## 9. Apps and plugins (D-013)

- **Built-in apps** (`termos-apps`) implement an in-process trait defined in `termos-core` (agent windows and devkit panels implement it too):
  `App { title, handle_key/handle_mouse/handle_paste -> effect, render(area, &mut ratatui::Buffer, theme), images, set_waker, deadline, on_timer }`.
  - They run on the server loop and push heavy work to their own threads, asking for a redraw with `AppWaker` when it is done.
  - **Timers (D-052):** `deadline`/`on_timer` are scheduled only while the app is on screen for some client, so hidden apps cost nothing (D-028).
  - **Effects:** redraw, close, notify, run a command, `Copy(text)` (clipboard and history), `PasteAndClose(text)`.
  - **Widgets:** `termos_core::widgets` has a braille line chart, sparklines, meters and an image slot.
  - Saving and restoring app state with sessions is not done yet; app windows are not part of snapshots.
- **The apps (D-049, D-052):**
  - image viewer (`image.open PATH`) and markdown viewer (`markdown.open PATH`, local pictures inline);
  - file manager (`files.open [DIR]`; `file.open PATH` opens by kind, `file.edit PATH` runs `$VISUAL`/`$EDITOR` in a new pane; deletes to the trash, D-050);
  - system monitor (`monitor.open`, from `termos-hw`, sampling once a second only while visible);
  - clipboard manager (`clipboard.open`, prefix `=`): the server-wide history of copies, in memory only (D-050);
  - settings (`settings.open`): edits config.toml in place, keeping comments, and reloads.
- **Images (D-049):** apps report images with `App::images()` (a shared picture fitted into a cell rectangle, which may be scrolled partly above the app), shown through the ladder of §5.8.
- **Plain TUI programs** are first-class without any integration: they run in PTY windows, including the images they print (Kitty graphics, Sixel; D-048) and the Kitty keyboard protocol (D-046).
- **Out-of-process plugins (D-034):** a directory under `~/.config/termos/plugins/<name>/` with an executable and a `plugin.toml` (name, version, exec, `[permissions] windows/status/commands/notify`). Plugins run only while listed in `[plugins] enabled`.
  - **Launch:** the server launches them with its first session, passing `TERMOS_SOCKET`, `TERMOS_PLUGIN_NAME` and a per-launch `TERMOS_PLUGIN_TOKEN`.
  - **Connection:** the plugin connects with `kind = plugin` and its token.
    - It can open, draw and close floating windows, set a status widget, register `<plugin>.<name>` commands (palette and control API), and post notifications, each only with the matching permission.
    - Keys, mouse, resizes and closes in its windows, and command invocations, come back as notifications.
  - **Windows:** plugin windows are in-process `App`s whose content the plugin draws remotely.
  - **Failure:** a plugin that exits or disconnects loses its UI and the user is notified. There's no automatic restart in v1.
  - **Sandbox (D-064):** on Linux, launched plugins run in the agent sandbox (§11) through `termos sandbox run`: only their own data folder (`TERMOS_PLUGIN_DATA`) writable, credentials hidden, the session socket kept reachable, the network only with the manifest's `network` permission. `[plugins] sandbox = false` turns it off.
  - **Languages:** `termos-plugin` is the Rust SDK; other languages speak JSON-RPC directly (the tests include a plain Python plugin).
- **WASM widgets (D-051):** status-line widgets as components of the `termos:widget` WIT world (`crates/termos-plugin/wit/widget.wit`): `init(settings)`, `render(width) -> spans`, `tick`, `on-event(click)`; each call returns when the widget next wants to run, so a widget at rest is never called.
  - **Config:** `[widgets.<name>] path, settings, dirs, max-width`.
  - **Engine:** wasmtime (O-8, D-042), in every build (`wasm-widgets`, default on; D-050). One thread runs all widgets on a current-thread runtime, and only while some widget is configured.
  - **Sandbox:** WASI 0.2 without network, environment or files, except `dirs` read-only. Per call: fuel, 32 MiB of memory, 2 s, a bounded drawing. A widget that breaks a bound is stopped and reported.
  - **Sample:** `examples/widgets/pomodoro` (see [docs/widgets.md](docs/widgets.md)).

---

## 10. Developer environment (M5; D-004, D-012, D-053–D-056)

termos-devkit holds all of it; the CLI wires it in, and the desktop reaches it by running `termos` subcommands in panes, so termos-shell never depends on termos-devkit (§4).

- **mise (D-053):** the `mise` on `PATH` when it is 2025.10.0 or newer (or `TERMOS_MISE`), else a pinned release (2026.10.14) fetched with the system's `curl` into `<data>/mise/<version>/` and checked against SHA-256 sums compiled into TermOS. (TermOS links rustls since M6, for agents, D-058; mise is still fetched with `curl`.) Panes get mise's shims (and a fetched mise) at the front of `PATH`.
- **Toolchains (D-054):** `termos lang install|use|list|remove|check <lang>[@version]`. The runtime and its helpers become global defaults (`mise use --global`); language servers and debuggers are only installed (`mise install`), and editors start them through `mise exec` with their exact versions. Projects pin versions in `mise.toml` (or `.tool-versions`).
- **Registry (`langs`):** per language: the runtime, helpers, servers (mise spec, command, filetypes, root markers, whether a project root is required), the debugger (with port arguments for adapters that listen on a socket), Helix names, and platform limits naming the tool that has no build there (`termos lang install` leaves it out and says why).
- **Coverage** (each row checked on Linux x86_64 and aarch64 by the language CI):

  | Language | LSP | DAP |
  | --- | --- | --- |
  | Rust | rust-analyzer | codelldb |
  | C, C++ | clangd (none on aarch64 Linux) | codelldb |
  | Zig | zls | codelldb |
  | Go | gopls | delve |
  | Python | basedpyright + ruff | debugpy |
  | Java | jdtls (pinned milestone) | — |
  | Kotlin | kotlin-language-server | — |
  | C#/.NET | csharp-ls | netcoredbg |
  | JS/TS (Node, Bun) | vtsls | js-debug |
  | Deno | `deno lsp` | — |
  | HTML/CSS | vscode-html/css-language-server | — |
  | Lua | lua-language-server | — |
  | Shell | bash-language-server (+ shellcheck) | — |

  Servers come from mise backends: the registry, `npm:`, `pipx:` (through uv), `go:`, `dotnet:`, `github:` and `http:`.
- **Editors (D-012, D-054):** Neovim 0.12.6 under `NVIM_APPNAME=termos-nvim`: `init.lua` and `lua/termos/` are embedded and rewritten on setup, `lua/termos/generated.lua` lists the installed servers and debuggers with their commands, and `lua/user.lua` belongs to the user. Servers use Neovim's own `vim.lsp.config`/`enable`; nvim-dap, nvim-dap-ui and nvim-nio come through `vim.pack`, pinned to commits. Each part loads on its own. Helix gets a `languages.toml` (or `languages.termos.toml` beside the user's). `[dev] editor = auto | termos-nvim | helix | env` picks what `termos edit` and the file manager open.
- **Projects (D-055):** `termos create <template> [dir]`: embedded templates, one per language (Zig and .NET through their generators), with a `mise.toml` pinning the installed version, marked trusted for mise (D-056).
- **Tasks (D-055):** build, run and test from the project's files in the nearest folder up (Cargo, Go, Deno, npm/Bun, Python, CMake, .NET, Gradle, Maven, Zig, single-file Java and Kotlin, Lua, shell), overridden and extended by `[tasks]` in a `termos.toml`. `task.build|run|test` and `task NAME` open a bottom panel running `termos task NAME --wait`; a task's next run replaces its panel.
- **Git UI (O-3 → D-053):** `git.open` (prefix `g`) opens `termos git`, lazygit through mise, in a float.
- **Acceptance (D-056):** `termos lang check <lang>`: install, create the template, build, run (output checked) and test through `mise exec`, headless Neovim until every server for the main file has completed LSP `initialize`, then the debugger's DAP `initialize`. CI runs it per language and architecture (§17).
- **Dev rootfs (bare metal, M5b):** a Debian rootfs on the data partition, entered through bubblewrap with user namespaces. It binds `$HOME`, the project and `/dev/pts`. Compilers and system packages are installed there with apt, and mise also works inside it. Podman is available where the hardware allows. Hosted mode doesn't need a dev rootfs.
- **Architecture limits:** `docs/languages.md`. On a Pi 3 or Zero 2 W (512 MB–1 GB RAM), jdtls, kotlin-language-server and Podman are impractical.

---

## 11. AI agent runtime (M6; D-058–D-064)

termos-ai holds all of it; the CLI hands the server a launcher for its windows (§4), and `termos secret|agent|audit|sandbox` reach it from the shell. User guide: [docs/agents.md](docs/agents.md).

- **Providers (D-058, D-059):**
  ```rust
  trait Provider {
      fn name(&self) -> &str;
      fn kind(&self) -> ProviderKind;
      fn list_models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, ProviderError>>;
      fn chat<'a>(&'a self, req: &'a ChatRequest, on_delta: &'a mut (dyn FnMut(Delta) + Send))
          -> BoxFuture<'a, Result<Reply, ProviderError>>; // deltas: text, thinking, tool start
  }
  ```
  - Our own thin clients over reqwest (rustls with ring) and one SSE parser (fuzzed: `sse_parse`): Anthropic's Messages API (version 2026-10-21, eager tool-input streaming, prompt caching, `thinking` blocks kept verbatim, `refusal` handled), and Chat Completions for OpenAI, Moonshot Kimi (its reasoning sent back with tool calls), any OpenAI-compatible server (Ollama, vLLM, LM Studio, OpenRouter) and the local router of §12 (`base-url` until M7).
  - **Model names are never hardcoded.** They're listed from the provider APIs (with context windows, output limits and, where listed, prices); when a provider can't list models, the user types one in. Profiles can store the chosen model.
- **Agent windows and the loop (D-059):** each window runs its session on its own thread with a single-threaded tokio runtime and shares a `View` with the window. Stream the reply → collect tool calls → ask the permission engine → confirm in the window if needed (diffs for file changes) → run (commands in the sandbox) → return the results → continue. Esc cancels; requests that failed before any output are retried (4 tries); 60 rounds of tool calls per message at most. Conversations are append-only and persisted in `$XDG_STATE_HOME/termos/agents/` (resume with `agent.resume`); tokens and cost are counted per session, cost only where prices are known (config or the provider's list).
- **Tools:** `read_file`, `write_file` / `edit_file` (exact replacement, with a diff preview), `list_dir`; `search_code` (ripgrep's `grep` and `ignore` crates); `run_command`, PTY-backed so its output shows in the window as a terminal (the model gets plain text); `fetch_url` (redirects reported, not followed); MCP tools.
- **Permissions (D-060):** each profile has filesystem scopes (the workspace, `read-paths`, `write-paths`), command allow/deny patterns, a domain allowlist for `fetch_url`, an MCP server list, and ask/allow/deny per tool class. Destructive or network actions *always* ask: deleting files, discarding changes, force-pushing, installing packages, piping a download into a shell, running as another user, writing outside the workspace, touching credentials, and every command when the sandbox is missing or partial. "Allow for this session" is offered for the rest.
- **Sandbox (D-061):**
  - Commands run under bubblewrap: new mount/pid/ipc/uts/net namespaces, the system bound read-only, the workspace read-write, a private `/tmp`, credentials and the runtime folder hidden, and no network unless granted.
  - Landlock adds defence in depth from inside, through `termos sandbox-exec` (writes, TCP, signal and abstract-socket scopes), which also drops credential-like environment variables. Landlock alone counts as partial (every command asks). seccomp is planned (after 1.0).
  - On macOS hosted mode the sandbox degrades to confirm-only, with an explicit warning; on Windows too, until M9.
- **Audit (D-062):** an append-only JSONL file per agent session in `$XDG_STATE_HOME/termos/audit/`. Entries are hash-chained (SHA-256 of the previous line; the conversation keeps the last hash) and record the tool, redacted arguments, the decision and why, a redacted result summary and timestamps. `termos audit verify` checks them; the viewer app (`audit.open`) lists them.
- **MCP (D-063):** an `rmcp` client over stdio and streamable HTTP. Each server must be listed by the profile; local servers launch inside the sandbox.
- **External CLI agents (D-063):** Claude Code, Codex CLI, Gemini CLI, aider and opencode have launcher presets (`agent.external NAME`) that open them in PTY windows, optionally under the sandbox.
- **Secrets (D-017, D-058):** stored with the `keyring` crate (Secret Service on Linux, Keychain on macOS, Credential Manager on Windows), or in an age file locked with a passphrase where no keyring service exists, as on bare metal. They're handled as `secrecy` types, config refers to them as `keyring:<name>`, and they are redacted from everything TermOS writes.

---

## 12. Local models (M7; D-021)

- **CLI:** `termos model search|pull|list|rm|info|recommend`.
- **Downloads:** resumable through HTTP Range requests into `.incomplete` files and checked against the Hub's SHA-256 (LFS) metadata. They use a `huggingface_hub`-compatible cache, with the HF token taken from the keyring or `HF_TOKEN`. Cache management includes size limits and GC.
- **Hardware detection** (`termos-hw`):
  - CPU features (AVX2, AVX-512, NEON, SVE, dotprod, i8mm) and RAM;
  - NVIDIA through NVML, loaded dynamically, with `nvidia-smi` as a fallback;
  - AMD through ROCm SMI or sysfs;
  - Vulkan through the loader;
  - Metal on macOS;
  - the Pi model from the device tree;
  - Coral (USB/PCIe IDs), Hailo (`/dev/hailo*`) and Cloud TPU VMs.
- **Fit estimation:** weights size (exact from file sizes) + KV cache (2 × layers × kv_heads × head_dim × context × bytes per element, read from GGUF or `config.json` metadata) + overhead, compared with free RAM and VRAM, including partial offload. The estimate is shown *before* downloading, and TermOS recommends the largest quantization that fits with headroom.
- **Backends:**
  - **llama.cpp (default; also the right choice for Raspberry Pi):** GGUF on CPU, CUDA, ROCm/HIP, Vulkan and Metal. Integration is O-6.
  - **Rust-native:** ONNX Runtime and/or candle (O-7).
  - **Optional Python sidecar** (transformers or vLLM, in a uv-managed venv) for high-end NVIDIA and AMD GPUs.
  - **Accelerators, with honest limits:**
    - Cloud TPU works only on a TPU VM, through JAX or PyTorch-XLA (vLLM TPU) in the sidecar.
    - Coral Edge TPU runs only small int8 TFLite models compiled for the Edge TPU, such as vision and audio classifiers. It can't run LLMs.
    - Hailo runs compiled HEF models, mostly vision. LLM support depends on the specific Hailo part and SDK, and will be verified and documented in M7.
- **Router:** `termos models serve` exposes `/v1/models`, `/v1/chat/completions`, `/v1/completions` and `/v1/embeddings` on localhost or a Unix socket, protected by a token. It starts backends lazily and unloads the least recently used one when over the memory budget.

---

## 13. Bare-metal image (M8–M9)

- **Build:** Buildroot (a pinned release) as a `BR2_EXTERNAL` tree in `crates/termos-image/br2-external/`, with a glibc toolchain (D-014). TermOS binaries are cross-built with cargo-zigbuild (D-015) and installed by a Buildroot package.
- **Targets:**
  - x86_64: a hybrid UEFI + BIOS ISO (GRUB2) plus a disk image;
  - generic aarch64 UEFI (O-14);
  - Raspberry Pi 5 and 4;
  - Raspberry Pi 3 and Zero 2 W, best effort (all aarch64).
- **Boot flow:**
  1. firmware → bootloader → kernel (KMS drivers: i915, amdgpu, virtio-gpu, simpledrm, vc4/v3d);
  2. init (O-10);
  3. mounts: a read-only rootfs (squashfs or erofs) plus a read-write data partition;
  4. eudev, then seatd;
  5. `termos server --kms` on tty1;
  6. first-run setup.
- **Partitions:** boot, rootfs A, rootfs B, data. The data partition holds home, the dev rootfs, models and containers. Updates are atomic A/B (O-12), with signed bundles and bootloader fallback (GRUB environment, Pi tryboot).
- **QEMU boot tests:** x86_64 (OVMF and SeaBIOS) and aarch64 virt (AAVMF) with virtio-gpu.
  - TermOS prints `TERMOS-READY` on the serial console after its first frame.
  - The harness takes a QMP `screendump` and checks the desktop's structure: not blank, and the status bar row present.
  - Idle RAM is measured in the same run (D-019).

---

## 14. Security model

- **Socket:** one server per user, with an owner-checked 0700 socket directory, a 0600 socket and peer-uid checks. On Windows: a named pipe with an owner-only protected DACL, remote clients rejected, the first instance reserved, SID checks in both directions, and anonymous impersonation level for clients (D-036).
- **Plugins:** they run only when enabled in the config. Each identifies itself with a per-launch token (128 bits from the OS generator, D-040) and gets only the permissions its manifest asks for (D-034).
- **WASM widgets (D-051):** WASI without network or environment; files only where `dirs` grants, read-only; fuel, memory, time and output bounds on every call.
- **Clipboard history (D-050):** in the server's memory only; never in logs, `Debug` output or snapshots.
- **File manager:** deleting moves to the system trash; a permanent delete needs a second confirmation. Previews never run anything; pictures in markdown are read only from local files.
- **No plaintext secrets** anywhere: config, logs, snapshots, conversations or audit records. API keys live in the OS keyring or an age-encrypted file (D-058).
  - Scrollback persistence is opt-in.
  - Snapshots keep command lines only for allowlisted programs.
  - Plugin tokens are never logged.
- **Agents and plugins (D-060–D-064):** least privilege (profiles), the bubblewrap + Landlock sandbox for agents' commands, local MCP servers and launched plugins, confirmation for destructive or network actions, a tamper-evident audit log, and redaction of keys and credential-shaped text from everything written down. A canary-key test runs a whole session on the real desktop and checks logs, audit records, conversations and snapshots.
- **Listeners:** nothing listens on the network by default. The model router binds localhost or a Unix socket and needs a token.
- **Supply chain:** cargo-deny checks licenses, advisories and sources (crates.io only); `Cargo.lock` is committed; Actions are pinned to major versions (SHA pinning to be reconsidered in M9).
- **Developer tools (D-053, D-056):** the only download TermOS makes itself is mise, over HTTPS only (`curl --proto =https --tlsv1.2`), checked against SHA-256 sums compiled in before it is used. Everything else is mise's to download and verify. The `mise.toml` files TermOS writes list only tools and are marked trusted; TermOS never trusts a project's own mise config for you. Neovim plugins are pinned to commits.
- **Bare metal:** a read-only root with signed A/B updates.

---

## 15. Performance budgets (definitions: D-019)

| Budget | Target | How it's measured | Enforced from |
| --- | --- | --- | --- |
| Idle CPU | < 1% of one core | CPU time of server + client over 60 s with an idle shell window; automated test | M1 |
| Input→frame latency | < 16 ms x86_64, < 33 ms Pi 4 (p99) | Timestamps from reading input to flushing the frame, plus an end-to-end PTY harness | M1 (x86), M8/M9 (Pi) |
| Base desktop RAM | < 150 MB | Hosted: RSS of server + client. Bare metal: MemTotal − MemAvailable in QEMU | M1 (hosted), M8 (bare metal) |
| Throughput | No regressions | criterion benchmarks for compose, diff and encode; `cat` of a large file | M1 |

---

## 16. Testing strategy

- **Unit tests** in each crate, next to the code.
- **Apps** are snapshot-tested through a fixture source where the real one would vary (the system monitor) and against temporary folders and files (the file manager, settings).
- **WASM widgets** are tested with the sample's build kept in `crates/termos-shell/tests/fixtures/`; CI rebuilds it from source and runs the same tests against the fresh build.
- **Snapshot tests** (`insta`): a `Grid` renders to text plus a style layer. The headless client drives the full desktop: inject input, then snapshot the screen. These cover chrome, layouts, apps and overlays.
- **Integration tests:** real PTYs (`sh -c …`), client/server over the socket (sessions, detach, snapshots and restore, plugins), the real daemon driven through its CLI, config hot reload, and recorded provider fixtures with mock servers. CI never uses live API keys.
- **Agents (M6):** provider contract tests run the real HTTP clients against a mock server answering with fixtures in the documented formats (streams cut at arbitrary bytes); a whole session runs against a scripted provider (passphrase, model list, approvals, a PTY command, the audit chain, the history); the permission engine, classifier, redactor and audit chain have unit tests; through the real binary, the sandbox must refuse writes outside the workspace and the network, a sandboxed plugin must still reach the session socket, a tampered log must fail `termos audit verify`, and the canary key must be nowhere after a session on the desktop. No live keys anywhere.
- **Developer environment:** `termos lang`, `edit`, `create`, `task` and `lang check` are tested against a stand-in mise script (`TERMOS_MISE`) that logs its arguments and runs what follows `exec --`; DAP framing against in-memory and socket adapters. Real installs, builds, LSP and DAP handshakes run in the language CI (§17).
- **Property tests** (`proptest`) for layout invariants (no overlaps or gaps, sizes sum to the parent), rectangle subtraction and the block raster.
- **Golden bytes** for the Kitty, Sixel and iTerm2 encoders and the Kitty keyboard protocol table, and a Sixel round trip.
- **Fuzzing** with `cargo-fuzz` (on a nightly toolchain, separate from the pinned one):
  - VT input (M1);
  - config parsing (M2);
  - protocol decoding (`proto_decode`: frames and JSON-RPC);
  - capability replies (`caps_reply`), Sixel decoding (`sixel_decode`) and the image interceptor (`apc_dcs_intercept`);
  - markdown layout (`markdown_layout`);
  - provider event streams (`sse_parse`).
  
  CI runs a timed smoke pass on every push and longer scheduled runs.
- **QEMU boot tests** (M8).
- **Gates:** fmt, clippy `-D warnings` on every platform, cargo-deny, and `xtask check-todos`.

---

## 17. Build, CI and distribution

- **Workspace and tooling:** a pinned toolchain (D-005); `cargo xtask ci` mirrors CI locally (D-020).
- **CI** (D-016, D-041):
  - lint: fmt, repo checks, cargo-deny, and clippy for the Windows target from Linux (without default features: wasmtime compiles C there); `cargo xtask ci` also runs clippy for macOS arm64 when that target is installed;
  - clippy, tests and a daemon smoke test (`new -d`, `ls`, `msg`, `kill-server`) on x86_64 Linux, aarch64 Linux, macOS arm64 and Windows x86_64; the Linux runners install bubblewrap and require the agent sandbox (D-061);
  - zigbuild cross-builds; a fuzz smoke pass; the performance budgets;
  - the sample WASM widget built from source for `wasm32-wasip2`, with the widget tests run against it;
  - the language CI (D-053, D-056, `languages.yml`): `termos lang check` for every language on ubuntu-24.04 and ubuntu-24.04-arm, nightly, on developer-environment changes and on demand; Rust, Python and Go on x86_64 on every push.
- **Test builds** on every push (D-041): Linux x86_64/aarch64 (glibc 2.31) and macOS arm64 `.tar.gz`, Windows x86_64 `.zip` (static C runtime), each with a `BUILDINFO.txt`, kept 14 days. The Linux builds are downloaded and smoke-run on matching runners.
- **Later CI jobs:**
  - QEMU boots (M8);
  - releases (M9): hosted binaries for x86_64/aarch64 Linux and macOS, an x86_64 ISO, an aarch64 UEFI image, Raspberry Pi `.img` files, checksums and signatures.

---

## 18. Platform support (initial targets)

| Platform | Hosted | Bare metal | Notes |
| --- | --- | --- | --- |
| x86_64 Linux | Tier 1 | Tier 1 (UEFI + BIOS) | |
| aarch64 Linux | Tier 1 | Tier 1 (generic UEFI) | |
| macOS (arm64, x86_64) | Tier 1 | — | Agent sandbox is confirm-only (§11) |
| Windows 10 1809+ / 11 (x86_64, native) | Tier 2 | — | Windows Terminal or another VT console; named pipes and ConPTY. Agent sandbox confirm-only for now. Gaps tracked as `TODO(M9)` (D-035) |
| WSL2 | Tier 2 | — | |
| Raspberry Pi 5 / 4 | Tier 2 (on Pi OS) | Tier 1 images | Latency budget 33 ms on Pi 4 |
| Raspberry Pi 3 / Zero 2 W | Best effort | Best effort | 512 MB–1 GB RAM limits dev tools and local models |
