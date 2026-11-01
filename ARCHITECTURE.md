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
| `termos-apps` | Built-in apps on the in-process `App` trait | M4 |
| `termos-devkit` | mise-backed toolchains, Neovim/Helix + LSP/DAP, dev rootfs, Git/build panels | M5 |
| `termos-ai` | Providers, agent loop, tools, permission engine, sandbox, audit, MCP, secrets | M6 |
| `termos-models` | HF adapter, fit estimation, inference backends, OpenAI-compatible router | M7 |
| `termos-image` | Buildroot external tree, image build driver, QEMU boot tests | M8 |
| `termos-cli` | The `termos` binary: server, clients, every subcommand | M0 |
| `termos-plugin` | Plugin protocol types and a blocking Rust SDK (D-034); WIT definitions for widgets later (M4) | M3 |
| `termos-hw` *(planned)* | CPU/GPU/accelerator/RAM/thermal probing, shared by the system monitor and models | M4 |
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

### 5.7 Capability detection (planned, M4; basic env detection in M1)
1. **Environment heuristics:** `COLORTERM`, `TERM`, `TERM_PROGRAM`, `KITTY_WINDOW_ID`, `WT_SESSION`, `VTE_VERSION`, and whether we're inside tmux or screen.
2. **Active queries,** sent as one batch and terminated by DA1. Terminals answer in order, so the DA1 reply marks the end. Timeout ≈ 200 ms. The batch contains:
   - XTVERSION (`CSI > q`);
   - the Kitty graphics query (`APC G a=q`);
   - DECRQM `?2026`;
   - the Kitty keyboard query (`CSI ? u`);
   - cell and window pixel size (`CSI 16 t`, `CSI 14 t`);
   - the background colour (OSC 11, used to choose a light or dark theme);
   - DA1 itself, whose attribute 4 means Sixel support.

The result is a `Caps` struct that each client sends in its handshake.
Recorded responses from real terminals become unit-test fixtures.

### 5.8 Graphics fallback ladder
Kitty graphics → Sixel → iTerm2 inline images → Unicode half-blocks and
quadrants/sextants (2+ pixels per cell, coloured with fg/bg) → braille (for
line art and charts).

Images are compositor objects with a rectangle and occlusion state:
- Kitty supports placements and z-index.
- Sixel and iTerm2 images can't be partially occluded. When one is, that image falls back to blocks. We choose correctness over fidelity.
- The KMS backend draws images natively.
- `ratatui-image` will be evaluated for protocol encoding (M4).

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
  - Server → client: `Frame(FrameUpdate { cols, rows, clear, spans, cursor })`, `Clipboard`, `Bell`, `Title`, `Detached { session }`, `Exit { reason }`.
  - Client → server: `Hello { version, caps, cols, rows }` (first), `Key`, `Mouse`, `Paste`, `Resize`, `Focus`.
  - **Backpressure:** a client gets its next frame only after the previous one was written to it (D-032).
  - Planned for M4: `Image(op)`.
- **Control stream:** newline-delimited JSON-RPC 2.0 (lines up to 4 MiB). It's used by `termos msg …`, `termos ls` and the other subcommands, and by scripts. The v1 methods:
  - server: `server.info`, `server.kill`, `config.reload`;
  - sessions: `session.list/new/kill/rename/restore/saved`;
  - clients: `client.list/detach`;
  - panes: `pane.list/send-text/send-keys/kill`;
  - other: `notify.send`, `command.run`, `plugin.list`.
  - Every named command and plugin command is also a method of its own name.
  - Event subscriptions (`subscribe`) are planned for when agents need them (M6).
- **Plugin stream:** JSON-RPC as well; see §9 and D-034.
- **Inside panes:** `TERMOS=<socket>,<server pid>,$<session id>` lets CLI commands run in a pane find their server and session.
- **Over SSH:** `ssh -t host termos` runs a client on the remote host and attaches to (or starts) that host's server; detaching leaves everything running there. Later, a local client could attach to a remote server through a forwarded socket so that local capabilities such as Kitty graphics apply; the protocol is transport-agnostic.

---

## 8. Configuration, themes, persistence

- **Configuration:** `$XDG_CONFIG_HOME/termos/config.toml`, plus `themes/*.toml`.
  - Validation errors point at the exact span.
  - Hot reload uses `notify` with debouncing. An invalid edit keeps the last good config and posts a notification with the error.
  - `termos config check` validates without running.
- **Themes:** semantic tokens such as `bg`, `fg`, `accent`, `border.active`, `shadow` and `status.*`, plus the 16-colour ANSI palette given to child terminals. Built-in themes are dark, light and high-contrast (M4). The default follows the terminal background (OSC 11).
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
  `App { title, handle(event) -> effects, render(area, &mut ratatui::Buffer), wants_tick, save_state, restore_state }`.
  They run on the server loop and push heavy work to tasks.
- **Plain TUI programs** are first-class without any integration: they run in PTY windows.
- **Out-of-process plugins (D-034):** a directory under `~/.config/termos/plugins/<name>/` with an executable and a `plugin.toml` (name, version, exec, `[permissions] windows/status/commands/notify`). Plugins run only while listed in `[plugins] enabled`.
  - **Launch:** the server launches them with its first session, passing `TERMOS_SOCKET`, `TERMOS_PLUGIN_NAME` and a per-launch `TERMOS_PLUGIN_TOKEN`.
  - **Connection:** the plugin connects with `kind = plugin` and its token.
    - It can open, draw and close floating windows, set a status widget, register `<plugin>.<name>` commands (palette and control API), and post notifications, each only with the matching permission.
    - Keys, mouse, resizes and closes in its windows, and command invocations, come back as notifications.
  - **Windows:** plugin windows are in-process `App`s whose content the plugin draws remotely.
  - **Failure:** a plugin that exits or disconnects loses its UI and the user is notified. There's no automatic restart in v1.
  - **Later:** sandboxing launched plugins comes with the agent sandbox (M6).
  - **Languages:** `termos-plugin` is the Rust SDK; other languages speak JSON-RPC directly (the tests include a plain Python plugin).
- **WASM widgets (M4):** status-bar widgets implementing a WIT world (`render(width) -> styled spans`, `on-event`, tick interval).
  - The runtime is behind the `wasm-widgets` cargo feature and loads lazily, only when such a widget is configured.
  - Fuel/epoch and memory limits apply. There's no filesystem or network access unless granted.
  - The engine is Open (O-8).

---

## 10. Developer environment (M5; D-004, D-012)

- **Toolchains:** `termos lang install|use|list|remove <lang>[@version]` wraps mise. Project versions come from `mise.toml` or `.tool-versions`, and TermOS shells activate mise.
- **Dev rootfs (bare metal):** a Debian rootfs on the data partition, entered through bubblewrap with user namespaces. It binds `$HOME`, the project and `/dev/pts`. Compilers and system packages are installed there with apt, and mise also works inside it. Podman is available where the hardware allows. Hosted mode doesn't need a dev rootfs.
- **Editor:** Neovim, managed under `NVIM_APPNAME=termos-nvim`, with the built-in LSP client, nvim-dap and pinned plugins. Helix is selectable.
- **Planned LSP/DAP coverage** (verified per architecture in M5):

  | Language | LSP | DAP |
  | --- | --- | --- |
  | Rust | rust-analyzer | codelldb / lldb-dap |
  | C, C++ | clangd | codelldb / lldb-dap |
  | Zig | zls | codelldb / lldb-dap |
  | Go | gopls | delve |
  | Python | basedpyright (+ ruff) | debugpy |
  | Java | jdtls | java-debug |
  | Kotlin | kotlin-lsp | kotlin-debug-adapter |
  | C#/.NET | csharp-ls or Roslyn LSP | netcoredbg |
  | JS/TS (Node, Bun) | vtsls / typescript-language-server | js-debug |
  | Deno | `deno lsp` | js-debug |
  | HTML/CSS | vscode-html/css-language-server | — |
  | Lua | lua-language-server | local-lua-debugger |
  | Shell | bash-language-server + shellcheck | bash-debug-adapter |

  Servers are installed through mise backends (aqua, npm, pipx, cargo, go, github) wherever possible.
- **Panels:**
  - A Git UI (O-3).
  - `termos new <template>` project templates.
  - Build, run and test panels driven by project detection (`Cargo.toml`, `package.json`, `go.mod`, `pyproject.toml`, `CMakeLists.txt`, `*.csproj`, Gradle/Maven, `build.zig`) and overridable through `[tasks]` in a project `termos.toml`.
- **Architecture limits:** published in `docs/languages.md` (M5). Known up front: on a Pi 3 or Zero 2 W (512 MB–1 GB RAM), jdtls, kotlin-lsp, Roslyn and Podman are impractical.

---

## 11. AI agent runtime (M6)

- **Providers:**
  ```rust
  trait Provider {
      fn id(&self) -> &ProviderId;
      async fn list_models(&self) -> Result<Vec<ModelInfo>>;
      fn chat(&self, req: ChatRequest) -> BoxStream<'_, Result<ChatEvent>>; // TextDelta, ToolCall*, Usage, Stop
  }
  ```
  - Implementations: Anthropic (native Messages API), OpenAI, Moonshot Kimi (an OpenAI-compatible preset with its own base URL and listing behaviour), a generic OpenAI-compatible provider (Ollama, vLLM, LM Studio, OpenRouter), and local (the router from §12).
  - **Model names are never hardcoded.** They're listed from the provider APIs; when a provider can't list models, the user types one in. Agent profiles store the chosen model.
- **Agent loop:** stream the response → collect tool calls → check the permission engine → confirm if needed → run in the sandbox → return the results → continue. It supports cancellation, persists conversation history, and accounts tokens and cost per session. Cost is shown only when prices are known, from the provider API or user configuration.
- **Tools:**
  - `read_file`, `write_file` / `apply_patch` (with a diff preview), `list_dir`;
  - `search_code`, using the ripgrep crates `grep` and `ignore`;
  - `run_command`, PTY-backed so its output is visible in the agent window;
  - `fetch_url`;
  - MCP tools.
- **Permissions:** each agent profile has filesystem scopes, command allow/deny patterns, a network domain allowlist and an MCP server allowlist, with ask/allow/deny per tool class. Destructive or network actions *always* ask: deleting files, force-pushing, writing outside the workspace, installing packages, piping curl into a shell, and the like.
- **Sandbox:**
  - Commands run under bubblewrap: new mount/pid/net namespaces, the system bound read-only, the workspace read-write, and no network unless granted.
  - Landlock rulesets add defence in depth, and seccomp is planned.
  - On macOS hosted mode the sandbox degrades to confirm-only, with an explicit warning.
- **Audit:** an append-only JSONL file per agent session in `$XDG_STATE_HOME/termos/audit/`. Entries are hash-chained (SHA-256 of the previous entry) and record the tool, redacted arguments, the decision, a result summary and timestamps. A viewer app displays them.
- **MCP:** an `rmcp` client over stdio and streamable HTTP. Each server must be allowlisted, and local servers launch inside the sandbox.
- **External CLI agents:** Claude Code, Codex CLI and others get launcher presets that open them in PTY windows, optionally under a sandbox profile.
- **Secrets:** stored with the `keyring` crate (Secret Service on Linux, Keychain on macOS), or in an encrypted file store where no keyring service exists, as on bare metal (O-5). They're handled as `secrecy` types, and config refers to them by name (D-017).

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
- **No plaintext secrets** anywhere: config, logs, snapshots or audit records.
  - Scrollback persistence is opt-in.
  - Snapshots keep command lines only for allowlisted programs.
  - Plugin tokens are never logged.
- **Agents and plugins:** least privilege, sandboxing, confirmation for destructive or network actions, and a tamper-evident audit log.
- **Listeners:** nothing listens on the network by default. The model router binds localhost or a Unix socket and needs a token.
- **Supply chain:** cargo-deny checks licenses, advisories and sources (crates.io only); `Cargo.lock` is committed; Actions are pinned to major versions (SHA pinning to be reconsidered in M9).
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
- **Snapshot tests** (`insta`): a `Grid` renders to text plus a style layer. The headless client drives the full desktop: inject input, then snapshot the screen. These cover chrome, layouts, apps and overlays.
- **Integration tests:** real PTYs (`sh -c …`), client/server over the socket (sessions, detach, snapshots and restore, plugins), the real daemon driven through its CLI, config hot reload, and recorded provider fixtures with mock servers. CI never uses live API keys.
- **Property tests** (`proptest`) for layout invariants: no overlaps or gaps, and sizes always sum to the parent.
- **Fuzzing** with `cargo-fuzz` (on a nightly toolchain, separate from the pinned one):
  - VT input (M1);
  - config parsing (M2);
  - protocol decoding (`proto_decode`: frames and JSON-RPC);
  - capability-reply parsing (M4).
  
  CI runs a timed smoke pass on every push and longer scheduled runs.
- **QEMU boot tests** (M8).
- **Gates:** fmt, clippy `-D warnings` on every platform, cargo-deny, and `xtask check-todos`.

---

## 17. Build, CI and distribution

- **Workspace and tooling:** a pinned toolchain (D-005); `cargo xtask ci` mirrors CI locally (D-020).
- **CI** (D-016, D-041):
  - lint: fmt, repo checks, cargo-deny, and clippy for the Windows target from Linux;
  - clippy, tests and a daemon smoke test (`new -d`, `ls`, `msg`, `kill-server`) on x86_64 Linux, aarch64 Linux, macOS arm64 and Windows x86_64;
  - zigbuild cross-builds; a fuzz smoke pass; the performance budgets.
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
| Windows 10 1809+ / 11 (x86_64, native) | Tier 2 | — | Windows Terminal or another VT console; named pipes and ConPTY. Gaps tracked as `TODO(M9)` (D-035) |
| WSL2 | Tier 2 | — | |
| Raspberry Pi 5 / 4 | Tier 2 (on Pi OS) | Tier 1 images | Latency budget 33 ms on Pi 4 |
| Raspberry Pi 3 / Zero 2 W | Best effort | Best effort | 512 MB–1 GB RAM limits dev tools and local models |
