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
  - On Windows it starts without a console window, in its own process group and outside the starting terminal's job, and treats console close, logoff and shutdown events like SIGTERM (D-037). A hidden top-level window also hears `WM_ENDSESSION` (console events miss logoff and shutdown once user32 is loaded) and holds Windows back, up to five seconds, until the snapshots are written (D-107).
  - One server holds many sessions (one `Desktop` each). Each session can be shown by many clients; each client has its own overlays (palette, prompts, which-key).
- **PTY children:** anything that runs in a terminal window. On Windows they run in a ConPTY pseudo-console (D-039).
- **`termos models serve`:** a separate process (D-021) so inference can't crash or bloat the desktop. It manages backend worker processes.
- **MCP servers and agent tool executions:** spawned inside the sandbox (§11).
- **Out-of-process plugins:** launched by the server from their manifest, or connecting on their own (§9).

---

## 4. Crate map

| Crate | Responsibility | First milestone |
| --- | --- | --- |
| `termos-render` | `Grid`/`Cell`/colour, frame diffing, themes, capability detection, the terminal encoder, the software rasterizer (feature `raster`) | M1 |
| `termos-core` | tokio event loop and frame scheduler, in-process bus, `Transport`, protocol, config, paths; layout files, saved commands, SSH hosts and the recording format (D-120) | M1 |
| `termos-term` | PTY spawning/resizing, `VtBackend` over alacritty_terminal, blitting VT cells into the grid | M1 |
| `termos-wm` | Window tree, floating + tiling layouts, workspaces, z-order, hit testing, composition of window chrome | M1 |
| `termos-shell` | `Desktop` state, the session server (socket, sessions, clients, control API), key tables, command palette, which-key, status bar, copy mode, notifications, tutorial, snapshots, plugin host | M1 |
| `termos-apps` | Built-in apps on the in-process `App` trait: image and markdown viewers, file manager, system monitor, clipboard manager, settings | M4 |
| `termos-devkit` | mise-backed toolchains, Neovim/Helix + LSP/DAP, dev rootfs, the package manager app, Git/build panels | M5 |
| `termos-ai` | Providers, agent loop, tools, permission engine, sandbox, audit, MCP, secrets | M6 |
| `termos-models` | HF adapter, fit estimation, inference backends, OpenAI-compatible router | M7 |
| `termos-image` | Buildroot external tree, image build driver, QEMU boot tests (std-only, used by `xtask`) | M8 |
| `termos-cli` | The `termos` binary: server, clients, every subcommand | M0 |
| `termos-plugin` | Plugin protocol types and a blocking Rust SDK (D-034); WIT definitions for widgets later (M4) | M3 |
| `termos-hw` | CPU, GPU, memory, temperature, disk, network and process readings (sysinfo plus sysfs and NVML probes, D-050), shared by the system monitor and models | M4 |
| `termos-center` | The command center: its window, the data it gathers and the sampler that records the machine's load in the journal (D-093); the usage app of every AI account (D-097) | M10 |
| `termos-harness` | The coding harness: plans of typed tasks, routing to tiers and backends, Claude Code and Codex headless and TermOS's agent unattended, the run with its fallbacks and hand-off log, and its window (D-100) | M11 |
| `xtask` | Repo automation: `ci`, `check-todos`, `todos` | M0 |

**Dependency direction.** These are the edges we intend. There are no cycles, and a
crate never depends on one listed above it.

| Crate | May depend on |
| --- | --- |
| `termos-cli` | everything below. It wires the window factories from apps, ai and devkit into the shell. |
| `termos-shell` | wm, term, apps, plugin, hw (battery, brightness, drives: D-124), core, render |
| `termos-center` | ai, models, devkit, hw, core, render. The `termos` binary opens its windows through the launcher. |
| `termos-harness` | ai, devkit, core, render. The binary gives it the CLIs (through the shims' logic) and opens its window through the launcher. |
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
- a link id (D-118): an index into the `LinkTable` of the grid the cell is in (0: none). Programs' links (OSC 8) with `http`, `https`, `mailto` or `file` addresses are kept; TermOS's own anchors are not. Each client's table goes on from frame to frame and never reuses an id for another address, so the diff compares ids like the style; frames carry the table when it changed, and the terminal encoder writes linked runs inside OSC 8 (`[ui] links`).

Colours are `Default | Indexed(u8) | Rgb(u8,u8,u8)`. The terminal default is
kept as its own value so transparent or themed terminal backgrounds still work.

### 5.2 Sources
- **PTY windows:** the VT grid from `alacritty_terminal` (D-008).
- **Built-in apps and shell overlays:** ratatui buffers (D-009).
- **Plugin windows:** cells sent over JSON-RPC.
- **Images:** placed as objects (§5.8), not as cells.

### 5.3 Composition
For each attached client, at its size:
1. Desktop background: the wallpaper where the active workspace has no tiled panes (D-089): shortcuts down the left, the logo at the bottom right, a pattern or a picture (through the image layer).
2. The tiled layer of the active workspace.
3. Floating windows, bottom to top.
4. Drop shadows. With truecolor, the cells under a floating window's offset rectangle are blended toward the theme's shadow colour; otherwise a dim attribute is used.
5. Borders and title bars, drawn with the configured box characters. The focused window's title bar is filled with the accent (a gradient for truecolor clients, D-088), with the app's icon, or a spinner while the app is busy (D-090), before the title.
6. Overlays: palette, which-key, menus, toasts; the splash for a client that just attached (D-089).
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
- crossterm handles raw mode, the alternate screen and the terminal size. The client writes the mouse, bracketed-paste, focus and Kitty keyboard modes itself.
- **Input (D-102):** one decoder (`termos_core::input::decode`) handles keys, mouse and focus reports and pastes on every platform. It drops late query replies, and timeouts settle a lone ESC or a cut-off sequence. A reader thread blocks on stdin (Unix) or on the console in VT input mode (Windows); it costs nothing while idle.
- Each frame is one buffered write wrapped in synchronized-update brackets (DEC mode 2026) when the terminal supports them.
- Colours are downsampled truecolor → 256 → 16 according to the client's capabilities.

### 5.7 Capability detection (D-043)
1. **Environment heuristics:** `COLORTERM`, `TERM`, `TERM_PROGRAM`, `KITTY_WINDOW_ID`, `WT_SESSION`, `COLORFGBG`, `SSH_CONNECTION`, and whether we're inside tmux or screen. `TERMOS_COLOR`, `TERMOS_SYNC`, `TERMOS_GRAPHICS` and `TERMOS_REDUCED_MOTION` override.
2. **Active queries** (`termos_render::probe`), sent at attach as one batch terminated by DA1: terminals answer in order, so the DA1 reply marks the end. The client reads stdin directly for up to 200 ms (800 ms over SSH); keys typed meanwhile are passed on. On Windows it reads the console in VT input mode (D-102). `TERMOS_PROBE=0` skips it. The batch:
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
- **Pixels only (D-090):** an app may mark an image `pixels_only` when it has drawn a text version beneath (the monitor's braille CPU chart). Where the image shows in pixels the cells beneath are cleared; where it would be blocks it is skipped and the text stays.
- **Kitty** gets one placement per visible piece: the image minus the floats above it and the palette and which-key boxes, each a crop of a single upload.
- **Sixel and iTerm2** can't be partly covered. They're used only for an image that is wholly visible, not on the last row, while nothing moves; otherwise the visible pieces are drawn as blocks. We choose correctness over fidelity.
- The server uploads each image once per client and size; the client encodes and draws pixel images after the cells. A Sixel or iTerm2 image that moves is painted over by poisoning the client's previous grid, so the next diff redraws those cells.
- The KMS console takes Kitty placements and draws them as pixels itself (D-105).

### 5.9 KMS console (M8, D-073)
- **A client:** `termos --kms` (termos-cli's `kms` feature, Linux) attaches to the server like the terminal client and draws its frames itself. It consumes the same `Grid` spans and rasterizes the changed cells on the CPU (`termos_render::raster`) into a framebuffer; two DRM dumb buffers take turns, each copied only the rows it missed (as up to eight bands of rows, so a change at the top and one at the bottom do not copy the screen between them), and are shown with page flips. The console logs its input→frame times (D-019's measure: to asking for the flip) every 100 keys. No GPU code: any KMS driver works.
- **A terminal's jobs (D-105):** pictures arrive as Kitty placements and are blended over the cells as pixels; the last copied text stays in memory for Ctrl+Shift+V / Shift+Insert; the bell flashes a frame round the screen for 150 ms (a timer only while it shows).
- **Glyphs:** swash from the configured font (built in: DejaVu Sans Mono), a cache of cell-sized coverage masks, box drawing, blocks and braille drawn from geometry; the size is adjustable (Ctrl+= / Ctrl+-) and scaled for the screen's density.
- **Devices:** libseat for the display and input devices (VT switches give them up and take them back), libinput and XKB for the keyboard and mouse; the hardware cursor for the pointer.
- **Damage:** only changed cells are repainted, and only their rows are copied out.
- **The lock screen (M13d, D-124):** the console draws it into pixels of its own (the clock, the date, a password field), sends the server nothing while it shows, and refuses VT switches (`termos system vt-lock` sets `VT_LOCKSWITCH` too). It locks on `ServerMsg::Lock`, the lock key, `[lock] after-minutes` without input, the lid (libinput's switch) and before suspending (`ServerMsg::Suspend`), and a file in the runtime folder keeps it locked across restarts. Passwords are checked by `sudo termos system check-password` (sha512crypt against `/etc/shadow`) on a thread, with longer waits after three failures. The console also owns the brightness keys and, on battery, idle suspend: deadlines, not polls; none without a battery.
- **Several displays (M13e, D-127, D-128):** every connected display is driven, in the order and modes `[[kms.display]]` gives (`kms/outputs.rs`: primary first, preferred mode, text sized for each display's density). In `extend` mode (the default) the first display is the console's own client of the session and each further one opens a client of its own whose `Caps.own_workspace` gets it a workspace of its own (§6); in `mirror` mode the first display's changed rows are copied to the others. The pointer moves across them left to right and the keys follow it; the lock screen covers all of them. udev's DRM hotplug events (the drive watcher's netlink socket) and `ServerMsg::Reconfigure` after a config reload make it plan again; a display that goes says `ClientMsg::Bye`, and the console starts again if the first one goes or mirror and extend switch.
- **Media keys (D-128):** the volume keys become `sound.volume ±5` and `sound.mute` (`ClientMsg::Command`); `[kms] audible-bell` also beeps through PipeWire's `pw-play`.

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
  1. open shell overlays (prompts, menus, the palette, which-key);
  2. global bindings;
  3. the focused window. Keys are re-encoded in the mode the child asked for: legacy or Kitty keyboard protocol. Repeats, releases and alternate keys come from the outer terminal, which the client asks for them only while the focused program wants them (D-103); a release goes to the pane that got the press.
- **Mouse (D-085):**
  - SGR mouse reporting (1006) with motion tracking (1003).
  - Hit tests run top-down. Dragging a title bar moves the window; dragging an edge or corner resizes it; clicking focuses; the wheel scrolls scrollback.
  - When a child has enabled mouse reporting, events inside its content are forwarded with translated coordinates, in the child's encoding; Shift keeps them for TermOS.
  - **Menus** (`termos_shell::menu`): the TermOS menu behind the status line's start button (and prefix `Enter`, the `menu` command), and context menus on a right-click in a pane, on a floating title bar or on a tab. A menu is a stack of levels; the pointer selects, an item with a submenu opens it beside, a press runs an item and a press elsewhere closes. Every item is a named command and shows its key. Menus are per client and take all input while open.
  - **Hover:** each client keeps what the pointer is over (a menu row, a window button, a status-line target); a motion event redraws only when that changes, so a still or slowly moving pointer costs nothing.
  - **Double clicks** (two presses on one cell within 400 ms): a floating title maximizes or restores (the size before is remembered), a tab opens its rename prompt. The palette and the key list take clicks and the wheel; the status line has `+` (new workspace) and a clickable message (the notification center).
- **Clients and workspaces:** a session's clients share its active workspace and its size (the smallest client's, or the latest's), as tmux's do. A client whose capabilities say `own_workspace` (a further display of the console in extend mode, D-128) gets a workspace no other display shows, and its own size; the desktop swaps them in while it handles that client, and switching to a workspace another such display shows swaps the two.
- **Snapping:** drag a window to a screen edge or corner to get halves or quarters; snap commands do the same from the keyboard. Minimized windows go to a task strip in the status bar.
- **Commands:** every action is a named command, such as `pane.split-right`, `pane.resize-left 5` or `layout.select tiled`. Commands can be bound in config, run from the palette (Ctrl+Space or prefix `:`, fuzzy-matched) and called over JSON-RPC (`termos msg pane.split-right`). The which-key overlay lists the bindings available after the prefix.
- **tmux-style keys (D-023):** prefix `C-b`, then tmux's default keys (`%`, `"`, arrows, `z`, `x`, `c`, `n`, `p`, `0`–`9`, `[`, `]`, …), plus TermOS keys for floats (`f`, `F`, `m`, `M`). Resize keys repeat without the prefix for `repeat-time-ms`. A TermOS *workspace* is a tmux *window* (a tab in the status line); a *pane* is a tmux *pane*.
- **Tiling (D-024):** tmux-style panes separated by one-cell lines, sized by weights that respect each subtree's minimum extent. Presets are even-horizontal, even-vertical, main-vertical (master-stack), main-horizontal and tiled (grid). Splits that wouldn't fit are refused.
- **Copy mode:** vi motions over the scrollback, selection, search, and yank to the paste buffer and the system clipboard (OSC 52). The mouse wheel and drag-select use it too. Where the shell marks its commands, `[`/`]` jump between prompts and `o` copies one command's output (D-116).
- **Links and paths (D-118):** Ctrl+click (Ctrl+Shift over a program that reads the mouse) opens a program's link or what `termos_term::links` finds under the pointer in the rows in view: web and email addresses, paths that exist (with `:line[:col]`), git hashes, IP addresses. Holding Ctrl underlines it. Web and mail addresses open on the client's machine (`ServerMsg::Open`), or are copied on the console and over SSH; paths open in TermOS (`file.edit PATH:LINE[:COL]` at the line). Hint mode (`pane.hints`, prefix `u`) labels every match in view to open, copy or paste. The search across panes (`search.all`, prefix `/`) searches every terminal pane's history and screen on a thread and lands in copy mode at the match.
- **Workspace tools (D-120):**
  - **Layout files** (`termos_core::layouts`; `layout.open`, `layout.save`, `termos layout`) describe workspaces as panes split one from another. One function, `Desktop::build_workspace`, builds them, and worktree workspaces too.
  - **The drop-down terminal** (`quick.toggle`, prefix `` ` ``) is one full-width float per session; hiding it minimizes it, and it moves to the workspace it is shown on.
  - **Typing into several panes:** keys and pastes into a pane of the session's sync set (`workspace.sync`, prefix `S`; `pane.sync`) go to every pane in it, encoded per program.
  - **Saved commands** (`[[snippets]]`) fill their parameters through status-line prompts and are pasted.
  - **SSH hosts** from `~/.ssh/config` open `ssh HOST` panes.
  - **Recordings** (`pane.record`, prefix `R`) write the pane's output as asciinema v2 (`termos_core::cast`); `termos play` plays one back.
- **Attention (D-115, D-116):** a pane whose agent needs the user (its CLI's hook, or a program's OSC 9/99/777 notification) rings in the warn colour (`compose`'s per-pane ring style over the focus colour, and a float's border) and shows a `● needs you` badge; its tab gets `●`, and a toast, a notification-center entry and, where the terminal shows them, a system notification (`ServerMsg::Notify`) follow. A finished turn only marks its tab (`✓`) and the center. Input to the pane clears it; `agent.waiting` (prefix `j`) goes to the one waiting longest. A command that ran `[notifications] long-command` seconds and finishes where no client is looking marks its tab `✓`, or `✗` when it failed (`Need::Failed`), with a toast and a center entry; a failure also as a system notification (D-118). `pane.notify-when-done` asks for the next one whatever its length.
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
  - Server → client: `Frame(FrameUpdate { cols, rows, clear, spans, cursor, images, links })`, `ImageData { id, chunk, last }`, `ImageFree { id }`, `Clipboard`, `Bell`, `Notify { title, body }` (a system notification, which the client writes as its terminal's OSC 9, 99 or 777, D-116), `Open { url }` (a web or mail address the client opens with `xdg-open`, `open` or `explorer`, D-118), `Lock` and `Suspend` (the image's console locks, and suspends after locking; a terminal client ignores both, D-124), `Title`, `Detached { session }`, `Exit { reason }`. `images` is the client's whole placement list and `links` its whole link table, each sent only when it changed.
  - Client → server: `Hello { version, caps, cols, rows }` (first), `Key`, `Mouse`, `Paste`, `Resize`, `Focus`.
  - **Backpressure:** a client gets its next frame only after the previous one was written to it (D-032).
  - **Versions (D-043):** frontends speak protocol 6; control clients and plugins speak JSON-RPC version 1. Each is checked in the `hello` for its kind of connection.
- **Control stream:** newline-delimited JSON-RPC 2.0 (lines up to 4 MiB). It's used by `termos msg …`, `termos ls` and the other subcommands, and by scripts. The v1 methods:
  - server: `server.info`, `server.kill`, `config.reload`;
  - sessions: `session.list/new/kill/rename/restore/saved`;
  - clients: `client.list/detach`;
  - panes: `pane.list/send-text/send-keys/kill`, `pane.read` (text, and the last command's line, output and status, D-116), and for `tos`: `pane.command` (a named command on a given pane), `pane.open` (a program in a new split or float), `pane.rename`;
  - agents: `agent.event` (what the coding CLIs' hooks report through `termos agent event`: needs you, done, working, started, ended, with the conversation's id; D-116);
  - the clipboard: `clipboard.copy`, `clipboard.get` (the newest entry);
  - other: `notify.send`, `command.run`, `plugin.list`.
  - Every named command and plugin command is also a method of its own name.
  - Event subscriptions (`subscribe`) are not built: nothing needs them yet (the agents of M6 run inside the server).
- **Plugin stream:** JSON-RPC as well; see §9 and D-034.
- **Inside panes:** `TERMOS=<socket>,<server pid>,$<session id>` lets CLI commands run in a pane find their server and session, and `TERMOS_PANE` names the pane.
- **`tos` (D-084, D-085):** the `termos` binary answers to the name `tos` as well. The server keeps a `tos` link to itself in `<data>/bin` (a symbolic link on Unix; a hard link, else a copy, on Windows, remade when the executable changes) and puts that folder first on every pane's `PATH`, so one command works in every shell on every OS. Its verbs act on the calling pane through the methods above; anything else is a `termos` subcommand. Guide: [docs/tos.md](docs/tos.md).
- **Over SSH:** `ssh -t host termos` runs a client on the remote host and attaches to (or starts) that host's server; detaching leaves everything running there. Later, a local client could attach to a remote server through a forwarded socket so that local capabilities such as Kitty graphics apply; the protocol is transport-agnostic.

---

## 8. Configuration, themes, persistence

- **Configuration:** `$XDG_CONFIG_HOME/termos/config.toml`, plus `themes/*.toml`.
  - Validation errors point at the exact span.
  - Hot reload uses `notify` with debouncing. An invalid edit keeps the last good config and posts a notification with the error.
  - `termos config check` validates without running.
- **Themes (D-044):** semantic tokens such as `fg`, `accent`, `border`, `status-*` and `overlay-bg` from which every style is derived. Presets dark, light and high-contrast; `[theme] base = "auto"` follows each client's terminal background (OSC 11, then `COLORFGBG`). Themes are per client; `theme.select` switches one. Programs asking for default colours (OSC 10/11) get the terminal's.
- **Motion (D-042, D-045):** `[ui] animations` and `reduced-motion`; never over SSH. The splash on attach (`[ui] splash`) and the spinners of busy windows follow the same rule (D-089, D-090).
- **Icons and gradients (D-087, D-088):** `termos_render::icons` has one-cell glyphs per icon set (`[ui] icons = "unicode" | "nerd" | "ascii" | "none"`). Each client's `Theme` carries its icon set and whether it gets gradients (`[ui] gradients`, only for truecolor clients). `Theme::sheen` makes gradients that keep their text's contrast; `Theme::paint` falls back to the flat theme colour.
- **Wallpaper (D-089):** `[ui] wallpaper = "pattern" | "none" | <picture path>`; a picture is decoded on its own thread, which wakes the desktop through the PTY event queue.
- **Start (D-091):** `[ui] start = "desktop"` (the default) floats each new session's and workspace's first terminal over the wallpaper; `"terminal"` tiles it full screen. Restored sessions keep their saved layout.
- **Graphics:** `[graphics] protocol = "auto" | "kitty" | "sixel" | "iterm2" | "blocks"`.
- **Links and hints (D-118):** `[ui] links` (programs' links passed to the outer terminal), `[hints] alphabet`, `[notifications] long-command` (seconds; 0: off).
- **Workspace tools (D-120):** `[quick] height` and `key`, `[record] dir`, `[[snippets]]`; layout files in `<config>/layouts/` and a project's `termos.toml` (`[layout.NAME]`, `[[snippets]]`).
- **Files and processes (D-122):** `[files] bookmarks` and `two-pane`, `[processes] confirm`; open-with choices in `<state>/open-with`.
- **Clipboard:** `[clipboard] history` (entries kept in memory, default 50; 0 keeps none).
- **Widgets:** `[widgets.<name>]` for WASM status-line widgets (§9, D-051).
- **Agents (§11):** `[agents]` (providers, profiles, MCP servers, external agents) and `[secrets] store`; keys are named (`keyring:<name>`), never written.
- **Settings app:** writes config.toml itself (toml_edit, comments kept), only after the result parses as a config.
- **Network app (D-081):** links from `termos_hw::net` (sysfs and `sysinfo`); on images, Wi-Fi through iwd's D-Bus interface (zbus), networks saved as iwd's files through `termos system wifi-add`, the passphrase on its standard input.
  - **Bluetooth (D-128),** its second page: BlueZ over zbus (power, scan, pair, trust, connect, forget). While the page is open TermOS serves BlueZ's pairing agent (`org.bluez.Agent1`), whose questions are asked and answered on the page.
- **Sound and Displays (D-128):** the Sound page lists PipeWire's outputs through `wpctl` (`termos_hw::sound`) and makes one the default (`[sound] device`); the Displays page edits `[[kms.display]]` and `[kms] displays` from sysfs's connectors (`termos_apps::config_edit` keeps the file's comments). The desktop shows the volume in the status line on images, read at start and after each change, never on a timer.
- **Selected rows** draw their text in the theme's `fg` (`ThemeColors::selected`): every preset's text colours are tested against their backgrounds, AAA for high contrast and AA otherwise.
- **Session persistence (D-011, D-033):**
  - **Where:** snapshots go to `$XDG_STATE_HOME/termos/sessions/<server>/<session>.json`, with a versioned schema, mode 0600, written atomically.
  - **What:** the layout of every workspace and, per pane, the directory, the foreground program, and optionally (`save-scrollback`) the text. Command lines are kept only for allowlisted programs. Which pane is the drop-down terminal (D-120).
  - **When:** debounced after structural changes and at shutdown. They are deleted when a session ends on purpose.
  - **Restore:** a fresh server restores them by policy, `always | ask | never` (default ask). Shells restart in their directories and allowlisted programs (`restore-programs`) are re-run. Agent CLIs come back in their account and folder continuing their conversation (`SavedAgent`, `restore-agents`, D-116): an agent window as `termos agent external --resume [--conversation ID]`, an agent run in a shell typed into the restored shell with `tos`.
- **Shell integration (D-115, D-116):** bash, zsh, fish and PowerShell start with a built-in script (`termos_shell::integration`, written to `<data>/shell-integration/`) that loads the user's own configuration and then sets OSC 133 marks: `--rcfile` for bash, `ZDOTDIR` for zsh, `XDG_DATA_DIRS` (vendor configuration) for fish, `-NoExit -Command` for PowerShell. `[shell] integration = false` turns it off.
- **Logs:** `$XDG_STATE_HOME/termos/log/` (D-017).
- **On Windows** (D-038): config lives in `%APPDATA%\termos`, state (snapshots, logs) in `%LOCALAPPDATA%\termos`; the `XDG_*` variables still override both. The default shell is `pwsh`, then `powershell`, then `%COMSPEC%`. With no foreground process group there, the program a pane shows and saves is the newest one started under its shell; restored commands are quoted for PowerShell or cmd (D-106).

---

## 9. Apps and plugins (D-013)

- **Built-in apps** (`termos-apps`) implement an in-process trait defined in `termos-core` (agent windows and devkit panels implement it too):
  `App { title, icon, handle_key/handle_mouse/handle_paste -> effect, render(area, &mut ratatui::Buffer, theme), images, busy, catch_up, set_waker, deadline, on_timer, reopen }`.
  - `icon` goes before the window's title; `busy` turns it into a spinner, which moves only while some client animates (D-090); `catch_up` takes in finished background work before the title is drawn.
  - They run on the server loop and push heavy work to their own threads, asking for a redraw with `AppWaker` when it is done.
  - **Timers (D-052):** `deadline`/`on_timer` are scheduled only while the app is on screen for some client, so hidden apps cost nothing (D-028).
  - **Effects:** redraw, close, notify, run a command, `Copy(text)` (clipboard and history), `PasteAndClose(text)`.
  - **Widgets:** `termos_core::widgets` has a braille line chart, sparklines, meters (shaded along their length with `fill_at`) and an image slot; `termos_render::graphics::chart` draws a chart as a picture (D-090).
  - **Saved with the session (D-104):** `reopen` gives the palette command that opens the window again as it is (`files.open DIR`, `image.open PATH`, `monitor.open`...). Snapshots store it, and restore builds the app from it into the saved layout. Apps without one (agents, the harness, the tutorial) are left out.
- **The apps (D-049, D-052):**
  - image viewer (`image.open PATH`) and markdown viewer (`markdown.open PATH`, local pictures inline);
  - file manager (`files.open [DIR]`; `file.open PATH` opens by kind, `file.edit PATH` runs `$VISUAL`/`$EDITOR` in a new pane; deletes to the trash, D-050). Since M13c (D-122) a `Files` window of one or two panes with places and bookmarks; marks; archives browsed like folders, extracted and zipped (zip, tar, tar.gz, tar.zst); searches by name and text; a git column; bulk rename in the editor with undo; disk usage; open with (`file.open-system`);
  - system monitor (`monitor.open`, from `termos-hw`, sampling once a second only while visible); its process view (`processes.open [PID]`, D-122) lists every process (`termos_hw::procs`: user, time, state, command line, Linux ports), filtered, sorted or as a tree, and ends, kills, pauses and renices them, asking first;
  - clipboard manager (`clipboard.open`, prefix `=`): the server-wide history of copies, in memory only (D-050);
  - settings (`settings.open`): edits config.toml in place, keeping comments, and reloads;
  - About TermOS (`about.open`): version, system, file locations, licence (D-089).
  - the command center (`center.open`, prefix `C`; termos-center, opened through the launcher, §11, D-093).
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
- **The gallery (D-125, D-126):** a signed JSON index (`termos_apps::gallery`) of themes, layouts and WASM widgets. Every entry is signed with an Ed25519 key built into TermOS and every download matches its SHA-256 and kind before it is written where TermOS reads it (`themes/`, used by `[theme] name`; `layouts/`; `widgets/` plus a `[widgets.NAME]` table). `termos gallery` and the Gallery window (index and installs on threads). Native plugins and saved commands are not in it yet. docs/gallery.md.
- **Settings that move (D-126):** `termos settings export|import` (`termos_apps::settings_io`) archive the config, layouts, themes, the gallery's record and the AI accounts' names, never keys, histories, logs or snapshots, and scan what they collect with the redaction patterns. Import merges the config key by key with toml_edit and asks before replacing a file.
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
- **Dev rootfs (M5b, D-109, D-110):** `termos devroot create|enter|update|status|stop|remove`. Debian 13 from the official image's layer, pinned by digest per architecture (amd64, arm64), downloaded with `curl` from the repository the official image is built from and checked by size and SHA-256 before anything is unpacked into `<data>/devroot/rootfs` (on the images, the data partition). It is unpacked (the `tar` crate: owners, modes, setuid bits; no device nodes) and entered in a user namespace where the user is root and the user's subordinate IDs are 1–65 536 (util-linux 2.38's `unshare --map-users`, shadow's `newuidmap`), so apt and dpkg work and nothing runs as the host's root. Entering is bubblewrap in that namespace: the rootfs as `/`, its own `/dev`, `/proc` (a PID namespace), `/tmp` and `/run`, the host's `/sys`, DNS and hosts files read-only, the home (and a project folder outside it) read-write at their paths, the host's network, a clean environment. `update` is apt's upgrade. On hosts with Podman, `--podman` runs the same image by digest in the container `termos-devroot`, the only way off Linux. Hosted mode doesn't need a dev rootfs; it is there when wanted.
- **Packages (M5b, D-110):** `termos_devkit::packages` lists and searches four sources: mise (`mise ls --json`, its registry), the dev rootfs (dpkg's status file read directly, `apt-cache search` inside), the host's package manager (apt, dnf, pacman, zypper, apk, Homebrew, winget, Scoop: their own commands and output) and Podman (images, containers, `podman search`). The package manager app (`packages.open`) has a tab per source, lists and searches on threads, and lists again when the source's package database changes while it is drawn; installs, removals and upgrades are confirmed and then run as `termos pkg … --wait` in a window, where apt's questions and sudo's password are answered. `termos pkg list|search|install|remove|upgrade|setup|sources` is the same from any terminal.
- **Worktrees (M12a, D-113, D-114):** `termos_devkit::worktrees`, through the user's `git`. A worktree per task in `<root>/<repo>-<id>/<name>` on `termos/<name>`, recorded in a locked `registry.toml`, with a block of free ports (`PORT`, `TERMOS_PORT_<n>`) and a project's `[worktree]` setup, ignored files to copy and servers. Review data, commit, merge (in the base's checkout, or `git merge-tree` without one), archive, discard and restore (`refs/termos/archive/<name>`); pull requests through `gh` and `glab`. `termos worktree …`. The desktop opens a worktree as a workspace (`worktree.open`: agent, shell, servers; every pane in its folder gets its environment through the launcher's `worktree_env`), and `termos_devkit::review_app` is its review window: diffs, comments sent to its agent (`AppEffect::TellAgent`), and the actions. User guide: [docs/worktrees.md](docs/worktrees.md).
- **Architecture limits:** `docs/languages.md`. On a Pi 3 or Zero 2 W (512 MB–1 GB RAM), jdtls, kotlin-language-server and Podman are impractical.

---

## 11. AI agent runtime (M6; D-058–D-064)

termos-ai holds all of it; the CLI hands the server a launcher for its windows (§4), and `termos secret|agent|audit|sandbox` reach it from the shell. User guide: [docs/agents.md](docs/agents.md).

- **The command center (M10c, D-092, D-093):** sessions record each turn's tokens and cost, each tool decision and their start and end in `termos_core::journal` (one private JSON-lines file per UTC day under `<state>/center/`, numbers and names only, `[center] keep-days`), and list themselves in `termos_ai::registry` while they run (state, usage, cost, pending call; stop, approve, deny through the session's input). `termos-center` holds the window (Overview, AI, System, History), its data gathered on its own thread every 2 s while visible (termos-hw, the server's control API, audit chains, the log, the model cache and router, MCP servers, CLI agents) and the sampler thread that records CPU, memory and network every minute (`[center] sample`). User guide: [docs/center.md](docs/center.md).

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
  - On Windows the helper runs the command in an AppContainer (D-108): the system readable as for every app, the workspace and `write-paths` writable, toolchains on `PATH` inside the profile readable, the rest of the profile out of reach, `internetClient` only with the network. Grants are recorded and kept (Windows rewrites every file's permissions to make one) until `termos sandbox reset`; the command runs suspended into a kill-on-close job, so it ends with the helper.
  - On macOS hosted mode the sandbox degrades to confirm-only, with an explicit warning.
- **Audit (D-062):** an append-only JSONL file per agent session in `$XDG_STATE_HOME/termos/audit/`. Entries are hash-chained (SHA-256 of the previous line; the conversation keeps the last hash) and record the tool, redacted arguments, the decision and why, a redacted result summary and timestamps. `termos audit verify` checks them; the viewer app (`audit.open`) lists them.
- **MCP (D-063):** an `rmcp` client over stdio and streamable HTTP. Each server must be listed by the profile; local servers launch inside the sandbox.
- **External CLI agents (D-063):** Claude Code, Codex CLI, Gemini CLI, aider and opencode have launcher presets (`agent.external NAME`) that open them in PTY windows, optionally under the sandbox.
- **Their hooks (M12b, D-116):** each account's settings run `termos agent event --cli NAME` (Claude Code's and Gemini CLI's `settings.json`, Codex's `hooks.json`, which Codex asks the user to trust) at a permission request or question, the end of a turn, a prompt, and a conversation's start and end. The command maps the hook's JSON to `agent.event` for its pane (`$TERMOS_PANE`) and does nothing outside TermOS; it prints nothing and always succeeds. Registered by `termos agent install`, `termos account add`, every agent window and `termos agent hooks` (`[agents.hooks] register`). User guide: [docs/attention.md](docs/attention.md).
- **Claude Code and Codex CLI (M10d, D-094, D-095):**
  - **Installed by TermOS** (`termos_devkit::cli_agents`, `termos agent install|update|remove|status`): the vendors' native programs, fetched with the system's `curl` (HTTPS only) and checked before use. Claude Code: the `stable` channel's version, that release's `manifest.json`, the platform's SHA-256. Codex: the GitHub release's asset with the SHA-256 digest GitHub publishes (none: refused). They live in `<data>/agents/<name>/<version>/`, linked into `<data>/bin`, first on every pane's `PATH`. `agent.external claude|codex` runs TermOS's copy and offers to install when there is none; Claude Code's own updater is off for it (`DISABLE_AUTOUPDATER`).
  - **TermOS's MCP server** (`termos mcp serve`, `termos_ai::mcp_server`): MCP over stdio, written by hand (JSON-RPC lines; versions 2024-11-05 to 2025-11-25), reaching the server through the control API as the agent's own pane (`$TERMOS`, `$TERMOS_PANE`). Tools: `list_panes`, `read_pane` (the new `pane.read` method; keys masked), `send_to_pane` (never its own), `open_pane`, `open_file`, `run_command`, `notify`, `copy_to_clipboard`. The CLI asks before each call; `[agents.termos-mcp]` denies tools and commands on top; every call goes to an audit log (`mcp-<id>`) and to the journal. Installing registers it (`claude mcp add --scope user`; `[mcp_servers.termos]` in Codex's `config.toml`, edited with toml_edit).
  - **Their usage** (`termos_center::theirs`, `[center] cli-usage`): token counts, models and times from their session logs (`~/.claude/projects`, `~/.codex/sessions`), read incrementally into the command center's history; text fields are skipped by the deserializer.
- **AI accounts (M11a, D-096, D-097):** several accounts per CLI, each a folder the CLI already supports (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`) under `<data>/accounts/` (`termos_devkit::accounts`, `termos account`). `<data>/bin/claude` and `codex` are links to `termos`, which runs the real program as the pinned or active account (`crates/termos-cli/src/shim.rs`). Switching: the menu's Accounts, `account.use`, and "Continue as" on an agent window, which copies its folder's newest conversation into the other account and resumes it (`termos_devkit::conversations`). The shell reaches accounts through the launcher (`accounts()`, `use_account()`). The usage app (`termos_center::usage`, `usage.open`) shows each account's plan limits as the CLI reported them (Codex's logs; Claude Code's status line, recorded by `termos agent statusline`), its 5-hour window and week against the user's budgets, its tokens, and TermOS's own agents by provider. User guide: [docs/accounts.md](docs/accounts.md).
- **Sign-in, API keys, Gemini CLI (M11d, D-111, D-112):** `termos account sign-in` runs each CLI's own sign-in in the account's folder (`claude auth login`, `codex login` or `--device-auth` where no browser can open, Gemini CLI's Google login with `NO_BROWSER`); TermOS stores no subscription credential. `termos account status` asks the CLIs (`claude auth status`, `codex login status`; Gemini CLI's folder). An account may use an API key: Claude Code's and Gemini CLI's kept in the secret store (`account.<cli>.<name>`) and given by the shim as a `secrecy`-held variable (`termos_core::secret_env`), Codex's handed to `codex login --with-api-key`. The shims drop inherited sign-in variables for TermOS's accounts and keyed ones. Gemini CLI is the third CLI: its GitHub bundle (digest-checked, unpacked with the `zip` crate) run by mise's `node@22` through a launcher, accounts as `GEMINI_CLI_HOME` homes (`.gemini` inside, file logins), TermOS's server in its `settings.json`, usage from `tmp/*/chats/session-*.jsonl`, conversations carried through `projects.json`.
- **Skills and headless asks (M11b, D-098):** three skills (`termos`, `termos-dev`, `termos-agents`), built into the binary (`crates/termos-devkit/skills/`, `termos_devkit::skills`), are written into each Claude Code account's `skills` folder (one entry per real folder) and `~/.agents/skills` (Codex, Gemini CLI, opencode), with a `.termos-skill` file of checksums: TermOS updates only its own unchanged copies and never touches a skill of the user's. `termos skills` installs, removes and reports; the server's start, `termos agent install` and `termos account add` keep them current (`[agents.skills] install`). `termos agent ask` runs one turn of TermOS's agent in the calling process (`termos_ai::agent::ask`): `Launch.headless` offers only the reading tools, turns every question into a refusal or an error, starts no MCP servers, and still audits, saves and records usage. `tos panes`, `tos read` and `tos run -p` give CLIs without TermOS's MCP server the same pane access. User guide: [docs/skills.md](docs/skills.md).
- **The harness (M11c, D-099, D-100):** `termos-harness`. A goal is planned by the planning route's backend (read-only; the answer's first JSON object, checked: `plan::parse`, fuzzed as `plan_parse`) into tasks of seven kinds, each with a tier (`[harness.routes]`, defaults from [docs/model-guide.md](docs/model-guide.md)). `route::route` tries the backends in `[harness] order` (Claude Code, Codex, then one API backend per provider: subscriptions first), each filling the tier with its model, passing over what cannot run, has spent the plan's API budget, or whose CLI reached its plan limit in the run (no backend of that CLI again: no account rotation, D-096); failures fall back to the next. `runner::Harness` runs on its own thread (a single-threaded runtime and a `LocalSet`): in a git repository (D-113, D-114) each editing task works in a worktree of its own made from the plan's branch, at once with the others (`parallel`), and is committed and merged into that branch when it finishes (a conflict reruns it on top once, a second stops the plan), the result reviewed in the plan's worktree; elsewhere edits run one at a time in the project after every earlier edit, read-only tasks alongside; dependencies' answers passed on; reviews first to another backend than their authors (`cross-review`). `backends`: `claude -p --output-format stream-json` (`acceptEdits` and `--allowedTools` for the project's build/test/lint tasks and `allow-commands`, or editing and shell tools disallowed), `codex exec --json` (`workspace-write` or `read-only` sandbox), both in their own process group (Windows: a job object, the CLI started suspended until it is in it, D-107) so a stop ends what they started, and TermOS's agent with `Headless::Edits` (workspace writes, allowed commands, everything else refused) or `ReadOnly`; their JSON lines are read by `events` (fuzzed as `cli_events`). Each hand-off goes into a hash-chained audit log (`harness-<plan>`, `audit::Event::Handoff`) and the journal (`journal::Event::Handoff`). The window (`harness.open [goal]`, `tos harness`) and `termos harness plan|run|status`. Best-of-N (`runner::best`, `termos harness best`, `harness.best`): one prompt to several backends, each attempt in a worktree, committed, the project's test task run on it, compared, one kept. User guide: [docs/harness.md](docs/harness.md).
- **Secrets (D-017, D-058):** stored with the `keyring` crate (Secret Service on Linux, Keychain on macOS, Credential Manager on Windows), or in an age file locked with a passphrase where no keyring service exists, as on bare metal. They're handled as `secrecy` types, config refers to them as `keyring:<name>`, and they are redacted from everything TermOS writes.

---

## 12. Local models (M7; D-021, D-065–D-068)

- **CLI:** `termos model search|info|pull|list|rm|gc|recommend` and `termos models serve|status|stop|setup|sidecar` (docs/models.md).
- **Downloads (D-066):** the Hub's JSON API (`HF_ENDPOINT` for another address), into `huggingface_hub`'s cache layout (`[models] cache`, `HF_HUB_CACHE`, `HF_HOME`), so other tools share the files. A pull pins a commit, resumes from `blobs/<id>.incomplete` with Range requests, and keeps a file only if its size and SHA-256 (LFS) or git blob SHA-1 match. The token is `[models] token` (`keyring:<name>`) or `HF_TOKEN`, sent only to the Hub. `gc` removes interrupted downloads, unreferenced blobs and, over the size limit, the least recently used snapshots.
- **Hardware detection** (`termos_hw::accel`, D-066):
  - CPU features (AVX2, AVX-512, VNNI, FMA, F16C; NEON, SVE, dotprod, i8mm), RAM;
  - NVIDIA through NVML (loaded dynamically), then `nvidia-smi`; AMD through sysfs, with ROCm when `/dev/kfd` exists;
  - the Vulkan loader; Metal on Apple Silicon;
  - the Pi model from the device tree;
  - Coral (USB IDs, `/dev/apex_*`), Hailo (`/dev/hailo*`) and Cloud TPU chips (Google PCI accelerators).
- **Fit estimation (D-066):** weights (file sizes) + KV cache (layers × context × KV heads × (key + value dims) × 2 bytes, from the GGUF header read with Range requests, or `config.json`) + overhead (256 MiB + 5% of the weights), with 10% headroom, against free RAM and VRAM: fully on the GPU, split (with the GPU layer count), CPU, or too big. It is shown before downloading; `pull` without a file takes the best placement and the largest quantization within it.
- **Backends:**
  - **llama.cpp (D-065, O-6):** a `llama-server` subprocess per loaded GGUF model, on a localhost port with a per-start key passed in `LLAMA_API_KEY`. The binary is `[models] llama-server` or llama.cpp's official binaries for the machine from a pinned build (D-071; `[models] llama-cpp-version` overrides it), installed through mise; `[models] llama-cpp` picks the build (auto: Metal, Vulkan, CPU; CUDA and ROCm by hand). Linux CUDA and ROCm need a llama-server you built, or Vulkan. The right choice on a Raspberry Pi (CPU).
  - **candle (D-067, O-7):** BERT-family embedding models in-process, mean-pooled and normalized. No ONNX Runtime; chat stays with llama.cpp.
  - **Configured servers / the Python sidecar (D-068):** `[models.servers.<name>]` runs a command for the repositories it lists; `termos models sidecar vllm|transformers` sets up a uv venv and prints the entry. For high-end NVIDIA and AMD GPUs, and TPU VMs.
  - **Accelerators, with honest limits** (docs/models.md):
    - Cloud TPU works only on a TPU VM, through vLLM's TPU support in the sidecar.
    - Coral Edge TPU runs only small int8 TFLite models compiled for it, such as vision and audio classifiers. It can't run LLMs.
    - Hailo runs compiled HEF models (mostly vision) through Hailo's own runtime. TermOS runs no LLMs on it.
- **Router (D-068):** `termos models serve`, hyper on 127.0.0.1 (and optionally a Unix socket), token-protected: `/v1/models`, `/v1/chat/completions`, `/v1/completions`, `/v1/embeddings` (streamed through), `/health`, `/admin/stop`. Model ids are the repository, or `repo:quant` / `repo:path` when it holds several. Backends start on their first request; over the memory budget (default 90% of RAM + VRAM), the least recently used are stopped and waited for. It writes `<runtime>/models.json` (0600) with its URL and token; agents' `local` provider without `base-url` reads it, starting `termos models serve --daemon` when no router answers. termos-ai reads the file through termos-core and never links termos-models.

---

## 13. Bare-metal image (M8–M9, M13d–M13e; D-072, D-074–D-080, D-124, D-128)

- **Build (D-074):** Buildroot 2026.02.1 (pinned, checksum-checked) with the `BR2_EXTERNAL` tree in `crates/termos-image/br2-external/`. Each target is one of Buildroot's board defconfigs plus TermOS fragments (the Bootlin glibc toolchain, D-014; packages; overlay; layout); every fragment setting is checked after `olddefconfig`. `termos` (with `kms`) is built by cargo with the pinned Rust, linked by Buildroot's GCC against the image's sysroot, and installed by a small package. `cargo xtask image list|build|boot`.
- **Targets:**
  - x86_64: one disk image for BIOS and UEFI (GRUB2);
  - generic aarch64 UEFI and QEMU `virt` (GRUB2, D-072);
  - Raspberry Pi 5 and 4;
  - Raspberry Pi 3 and Zero 2 W, best effort (all aarch64).
- **Boot flow (D-075):**
  1. firmware → GRUB2 (a Pi: its firmware) → the kernel on the boot partition (KMS drivers: virtio-gpu, simpledrm, bochs, vc4/v3d; i915 and amdgpu as the board configs have them);
  2. BusyBox init (D-072);
  3. mounts: the read-only squashfs root in the chosen slot, the data partition on `/data` (`/home`, iwd's networks, D-Bus's machine ID and dhcpcd's leases bound from it; `/etc/shadow` and `/etc/localtime` link into `/data/etc`, D-124), a tmpfs on `/media` for USB drives;
  4. eudev, D-Bus, BusyBox ntpd (`[clock] ntp-servers`), iwd, dhcpcd, BlueZ's bluetoothd (pairings on the data partition, D-128), seatd;
  5. on tty1 as the user `termos`, respawned if it stops: a session bus and PipeWire, which starts WirePlumber (D-128), then `termos --kms`;
  6. first-run setup (the tutorial);
  7. once the console's first frame is up, the booted slot is marked good (D-078).
- **Partitions (D-124):** GPT on x86_64 (with a BIOS boot partition for GRUB's core image) and aarch64-virt, MBR on Pis (and on disks installed before M13d): boot, rootfs A, rootfs B, data (label `termos-data`). GRUB passes the `PARTUUID` of the slot on its own disk (`probe --part-uuid`); boot scripts find that disk's other partitions by GPT name or MBR number, and RAUC's configuration is written from them at boot. `termos install` gives an installed GPT disk new ids. The data partition holds home, Wi-Fi networks, the dev rootfs (under the home), and later models and containers. Updates (M9) are atomic A/B with RAUC (D-078): signed bundles, the kernel inside each slot, and boot-loader fallback (GRUB's environment; the Pi's tryboot).
- **Updates (D-078):** `termos update` checks GitHub Releases only when asked, downloads a RAUC bundle (verity, signed, `compatible=termos-<target>`) and has `termos system` (root, through one sudo rule) install it into the other slot. GRUB runs RAUC's slot-selection script over a `grubenv` on the boot partition and loads the kernel from the chosen slot's squashfs; a Pi uses RAUC's custom backend over the firmware's tryboot, with each slot's boot files under `slot-a/` and `slot-b/`. A slot that is not marked good within five minutes, or panics, is tried once and then left for the other.
- **Install (D-080):** the x86_64 live ISO (xorriso after the disk image; GRUB for BIOS, USB and UEFI) runs the root file system as an initramfs (`termos.live`: no disk touched) and carries the disk image, which `termos install` writes to a whole disk. Any image grows its data partition to the end of its disk on first boot.
- **The machine (D-124):** a lock screen in the console (§5.9); a Power menu (lock, suspend, restart, shut down); the battery in the status line (`termos_hw::power`) with low-battery steps; brightness through the backlight's sysfs file (group `video`, a udev rule); tzdata with `termos system set-time-zone`; USB drives from udev's netlink events and sysfs (`termos_hw::drives`), mounted by `termos system mount` on `/media/LABEL`; amdgpu as a module with its firmware on x86_64.
- **Displays, sound and Bluetooth (D-128):** every connected display (§5.9); PipeWire with WirePlumber on ALSA (HD Audio, USB, virtio and HDMI audio); BlueZ with its audio, HID and HoG plugins, so keyboards, mice and headsets pair; Intel, Realtek and MediaTek Bluetooth firmware on PCs and the Broadcom chip's on Pis, their drivers as modules.
- **QEMU boot tests (D-076):** x86_64 (OVMF and SeaBIOS) and aarch64 virt (AAVMF) with virtio-gpu (two heads, of which QEMU lights only the first without a window) and an HD Audio card with no host audio (D-128).
  - TermOS prints `TERMOS-READY` on the serial console after its first frame.
  - The harness takes a QMP `screendump` and checks the desktop's structure: not blank, and the status bar row present.
  - Idle RAM is reported by the image after it settles, asked for through `fw_cfg`, and must stay under 150 MB (D-019).
  - The console reports its displays; the desktop's user sets the volume and mutes through PipeWire and must read them back; BlueZ must answer that user (D-128).
  - The disk is a qcow2 overlay 1 GiB bigger than the image, so the data partition's growth is checked too. `--update` installs a newer bundle over HTTP and starts it, then a broken one and sees the fallback; `--install` installs from the ISO onto an empty disk, which then starts alone with each firmware.

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
- **Listeners:** nothing listens on the network by default. The model router (D-068) binds 127.0.0.1 or a 0600 Unix socket and needs a per-start token, which only the 0600 endpoint file in the runtime folder holds (hidden from agents' commands). Each llama-server gets its own key through its environment, not its command line, and is stopped if it does not check it.
- **Supply chain:** cargo-deny checks licenses, advisories and sources (crates.io only, plus patched copies of crates in `third_party/`, each listed with its change in its README, D-070); `Cargo.lock` is committed; Actions are pinned to major versions (SHA pinning to be reconsidered in M9).
- **Agent CLIs (D-094, D-095):** Claude Code and Codex CLI are downloaded over HTTPS only and checked against the SHA-256 their vendors publish (Claude Code's release manifest, GitHub's asset digest) before anything is kept; a mismatch installs nothing, and a release without a digest is refused. TermOS's MCP server acts only as the agent's pane, never types into that pane, is limited by `[agents.termos-mcp]`, masks keys in pane text it returns, and audits every call. Reading their usage logs keeps numbers only. TermOS's skills (D-098) are text built into the binary, written only into folders the CLIs read and only over TermOS's own unchanged copies; `tos read` masks keys as `read_pane` does. `termos agent ask` cannot write, run commands, fetch or reach MCP servers, refuses anything that would need approval, and is audited like a window. The harness's headless CLIs edit only within the project and run only the project's build, test and lint tasks and `[harness] allow-commands` (Claude Code's permission rules; Codex's own workspace-write sandbox, without network), refusals reported; TermOS's agent under it never asks and refuses what it would ask; every hand-off is audited.
- **Dev rootfs (D-110):** its image is downloaded over HTTPS and checked by size and SHA-256 against values compiled in before anything is unpacked; inside, root is the user's own user namespace (the user and the user's subordinate IDs), never the host's root, with the home and the project folder as the only host folders writable. `termos pkg` passes a package name as one argument, never an option or shell text, and runs the host's package manager through `sudo` only in a window where the user types the password.
- **Developer tools (D-053, D-056):** besides the agent CLIs above and the dev rootfs, the only download TermOS makes itself is mise, over HTTPS only (`curl --proto =https --tlsv1.2`), checked against SHA-256 sums compiled in before it is used. Everything else is mise's to download and verify. The `mise.toml` files TermOS writes list only tools and are marked trusted; TermOS never trusts a project's own mise config for you. Neovim plugins are pinned to commits.
- **The gallery (D-126):** nothing is installed unless every entry of the index verifies with a built-in Ed25519 key and the download matches its signed SHA-256 and its kind; only data (themes, layouts) and sandboxed WASM widgets at first, never granted folders. Settings exports never collect keys, the secrets file, histories, logs or snapshots, and refuse to write a line shaped like a key.
- **Bare metal:** a read-only root with signed A/B updates (bundles must chain to the image's keyring, checked for the code-signing purpose). The desktop's user reaches root only through `termos system`, which one sudo rule allows and which does a fixed set of things with checked arguments: install a bundle or the disk image, restart, power off, the Pi's boot-loader backend, save or forget a Wi-Fi network (D-078, D-081); set and check the user's password, lock VT switching, suspend, set the time zone, mount and unmount USB drives under `/media` (never set-user-id or devices), and list the NTP servers (D-124). These refuse to run outside a TermOS image. The live system touches no disk until `termos install` is told which.
- **The lock screen (D-123, D-124):** the system user's password, sha512crypt in `/etc/shadow` (on the data partition), checked only by root's verb, which waits after a wrong one; the console waits longer after three. It guards the keyboard and screen, not running programs or attached clients; the boot menu's "without the lock" entry is the way back in (as with any disk that is not encrypted).

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
- **Local models (M7):** downloads against a mock Hub (an interrupted transfer resumes, a checksum mismatch is rejected, gated repositories ask for a token, the cache layout and GC); the estimator against known model shapes; GGUF headers built in tests; the router end to end with a stand-in llama-server (a Python script), a configured server and a tiny random BERT for candle (on-demand starts, the token, eviction within the budget, streaming); the agents' router discovery against a stand-in `termos`. The CI `models` job does it for real (§17).
- **Developer environment:** `termos lang`, `edit`, `create`, `task` and `lang check` are tested against a stand-in mise script (`TERMOS_MISE`) that logs its arguments and runs what follows `exec --`; DAP framing against in-memory and socket adapters. Real installs, builds, LSP and DAP handshakes run in the language CI (§17).
- **Property tests** (`proptest`) for layout invariants (no overlaps or gaps, sizes sum to the parent), rectangle subtraction and the block raster.
- **Golden bytes** for the Kitty, Sixel and iTerm2 encoders and the Kitty keyboard protocol table, and a Sixel round trip.
- **Fuzzing** with `cargo-fuzz` (on a nightly toolchain, separate from the pinned one):
  - VT input (M1), and terminal input decoding (`input_decode`: the same events however the bytes are split);
  - config parsing (M2);
  - protocol decoding (`proto_decode`: frames and JSON-RPC);
  - capability replies (`caps_reply`), Sixel decoding (`sixel_decode`), the image interceptor (`apc_dcs_intercept`) and notifications and shell marks (`osc_signals`);
  - link and path detection (`link_detect`), layout files (`layout_parse`) and SSH configs (`ssh_config`);
  - markdown layout (`markdown_layout`);
  - provider event streams (`sse_parse`);
  - GGUF headers (`gguf_parse`), with the fit estimate over whatever they claim;
  - QEMU screenshots for the boot tests (`ppm_parse`).
  
  CI runs a timed smoke pass on every push and longer scheduled runs.
- **The KMS console (M8):** the rasterizer against the bundled font (metrics, glyphs, bold and italic, missing glyphs, box drawing joining across cells, attributes, the cursor, clipping, wide characters), a sample screen to look at (`TERMOS_RASTER_PNG`); the console's screen state (damage, the cursor, resizes), keys through real XKB keymaps (US, German), the mouse. A criterion benchmark for full and one-row repaints.
- **QEMU boot tests** (M8, D-076): `cargo xtask image boot`; the harness's own parts (PPM and screenshot checks, PNG, serial markers, QMP against a fake QEMU, fragment checks) have unit tests.
- **Gates:** fmt, clippy `-D warnings` on every platform, cargo-deny, and `xtask check-todos`.

---

## 17. Build, CI and distribution

- **Workspace and tooling:** a pinned toolchain (D-005); `cargo xtask ci` mirrors CI locally (D-020).
- **CI** (D-016, D-041):
  - lint: fmt, repo checks, cargo-deny, and clippy for the Windows target from Linux (without default features: wasmtime and ring compile C there); `cargo xtask ci` also runs clippy for macOS arm64 when that target is installed;
  - clippy, tests and a daemon smoke test (`new -d`, `ls`, `msg`, `kill-server`) on x86_64 Linux, aarch64 Linux, macOS arm64 and Windows x86_64; the Linux runners install bubblewrap and require the agent sandbox (D-061), the Windows runner requires the AppContainer (D-108);
  - zigbuild cross-builds; a fuzz smoke pass; the performance budgets;
  - the sample WASM widget built from source for `wasm32-wasip2`, with the widget tests run against it;
  - images (`images.yml`, D-074–D-076): the x86_64 and aarch64 virt images built with Buildroot and booted in QEMU, on changes to the image, the console or the workflow, weekly and on demand (the Pi images on demand), with the images, screenshots, serial logs and the `legal-info` report kept;
  - local models (`models`, D-065–D-068): real pulls from the Hugging Face Hub (a 260K-parameter GGUF and all-MiniLM-L6-v2), llama.cpp's release installed through mise, chat (plain and streamed) and embeddings (semantic similarity checked) through the router, the agents' `local` provider starting it, then stop, `rm` and `gc`;
  - the agent CLIs (`agents.yml`, D-095, D-098): Claude Code and Codex CLI installed by `termos agent install` on Linux x86_64 and aarch64, macOS and Windows, both run, and both complete an MCP handshake with `termos mcp serve` (Claude Code's `mcp list`; Codex's `exec` against a refusing stand-in model server), checked in TermOS's audit log; TermOS's skills are installed with them, and the requests each sends a recording stand-in model server (a fixture key for Claude Code, in two accounts) must list them; the harness runs a plan through both (the stand-in plans; Codex is refused and the review falls back to Claude Code) and its hand-off log verifies; nightly, on changes and on demand;
  - the dev rootfs and packages (`devroot`, D-110): on ubuntu-24.04 and ubuntu-24.04-arm, Debian 13's pinned layer is downloaded, checked, unpacked and entered with the runner's subordinate IDs, a package is searched, installed, run and removed with apt and a tool with mise through `termos pkg`, then upgraded and deleted; the Linux test runners add `uidmap` and a subordinate-ID range and require the dev rootfs test (`TERMOS_REQUIRE_DEVROOT`);
  - the language CI (D-053, D-056, `languages.yml`): `termos lang check` for every language on ubuntu-24.04 and ubuntu-24.04-arm, nightly, on developer-environment changes and on demand; Rust, Python and Go on x86_64 on every push.
- **Test builds** on every push (D-041): Linux x86_64/aarch64 (glibc 2.31) and macOS arm64 `.tar.gz`, Windows x86_64 `.zip` (static C runtime), each with a `BUILDINFO.txt`, kept 14 days. The Linux builds are downloaded and smoke-run on matching runners.
  - static musl builds for x86_64 and aarch64 Linux next to the glibc ones, smoke-run and held to the same performance budgets (`perf-musl`, D-079).
- **Releases** (`release.yml`, D-077): a `v<version>` tag matching the workspace version builds the hosted binaries (Linux glibc and musl, macOS arm64 and x86_64, Windows), every image with its update bundle and license report, signed with the owner's keys from CI secrets, then a draft GitHub release with `SHA256SUMS` and build provenance attestations. Run by hand, or changed on a branch, it rehearses: the same files as a workflow artifact, images optional, development keys when the owner's are missing. docs/releasing.md.
- **Packages (D-125, D-126):** the release also carries `.deb` and `.rpm` files (cargo-deb and cargo-generate-rpm over the release's glibc builds, with the completions and man page from `termos completions` and `termos man`), each installed on clean Ubuntu, Debian and Fedora containers on x86_64 and arm64 first. A published release starts `packages.yml`: the Homebrew formula, Scoop manifest, winget manifests and AUR PKGBUILD are filled from `packaging/` and `SHA256SUMS`, installed on clean runners, and published only where the owner's secret for that channel exists. `flake.nix` builds TermOS from source with the pinned toolchain. docs/installing.md.

---

## 18. Platform support (initial targets)

| Platform | Hosted | Bare metal | Notes |
| --- | --- | --- | --- |
| x86_64 Linux | Tier 1 | Tier 1 (UEFI + BIOS) | |
| aarch64 Linux | Tier 1 | Tier 1 (generic UEFI) | |
| macOS (arm64, x86_64) | Tier 1 | — | Agent sandbox is confirm-only (§11) |
| Windows 10 1809+ / 11 (x86_64, native) | Tier 2 | — | Windows Terminal or another VT console; named pipes and ConPTY (the zip ships a newer one, D-107). Agent sandbox: an AppContainer (D-108) |
| WSL2 | Tier 2 | — | |
| Raspberry Pi 5 / 4 | Tier 2 (on Pi OS) | Tier 1 images | Latency budget 33 ms on Pi 4 |
| Raspberry Pi 3 / Zero 2 W | Best effort | Best effort | 512 MB–1 GB RAM limits dev tools and local models |
