# Getting started with TermOS

TermOS is a desktop that runs inside your terminal. If you've used tmux,
you already know most of it: a prefix key, panes, windows in a status
line, copy mode. On top of that it adds floating windows you can drag
around, a command palette, key hints, and a mouse that works everywhere.

> **Status:** milestone M9 (releases: TermOS in a terminal on Linux, macOS
> and Windows, and as a whole system on PCs, Arm machines and Raspberry
> Pis, with A/B updates). See [ROADMAP.md](../ROADMAP.md).

## Install and run

TermOS comes two ways:

- **In your terminal**, on Linux, macOS, Windows or WSL2: a download from the
  repository's **Releases** page (below), a [test build](#test-builds), or
  built yourself.
- **As the whole system** on a PC, an Arm machine or a Raspberry Pi: it
  boots straight into the desktop and updates itself in two halves, so a
  bad update falls back. See [bare-metal.md](bare-metal.md): try the live
  ISO, then `termos install`.

From a release, pick the file for your system:

| File | System |
| --- | --- |
| `termos-<version>-linux-x86_64.tar.gz` | Linux on x86_64, glibc 2.31 or newer (Ubuntu 20.04+, Debian 11+, WSL2) |
| `termos-<version>-linux-aarch64.tar.gz` | Linux on arm64, glibc 2.31 or newer (Raspberry Pi OS 64-bit, …) |
| `termos-<version>-linux-x86_64-musl.tar.gz`, `…-aarch64-musl.tar.gz` | Any Linux, older or without glibc (Alpine, …): static, without NVIDIA readings in the system monitor |
| `termos-<version>-macos-arm64.tar.gz`, `…-macos-x86_64.tar.gz` | macOS 11 or newer |
| `termos-<version>-windows-x86_64.zip` | Windows 10 1809 or newer, Windows 11 |

Check a download against the release's `SHA256SUMS`, and, if you like,
where it was built: `gh attestation verify <file> --repo sovr610/smol-os`.
Then unpack it and run `termos`. The programs are not code-signed for macOS
or Windows yet; see [below](#test-builds) for opening them anyway.

To build it yourself you need Rust (the toolchain pinned in
`rust-toolchain.toml` installs automatically through rustup) on Linux,
macOS, Windows or WSL2.

```bash
cargo install --path crates/termos-cli
```

```bash
termos
```

Or run it straight from the repository:

```bash
cargo run --release -p termos-cli
```

On Windows, run it in **Windows Terminal** (or another console with VT
support, on Windows 10 1809 or newer). Panes start PowerShell 7 (`pwsh`)
if it is installed, else Windows PowerShell; set `shell = "cmd.exe"` (or any
program) in the config to change that. See [Windows notes](#windows-notes).

The first time, a short tutorial opens. Press `→` to page through it and `q`
to close it. Reopen it any time with `C-b F1`.

Like tmux, TermOS keeps running in the background: **`C-b d` detaches**, and
`termos` again brings you back to where you were, even from another terminal
or over SSH. A session ends when its last shell exits, or from the palette:
`Ctrl+Space`, type `quit`, then press `Enter` and `y`. See
[sessions.md](sessions.md) for sessions, `termos ls`, restoring after a
reboot and scripting with `termos msg`.

## The prefix key

Almost every command is **`C-b`** (Ctrl+B) followed by a key: press Ctrl+B,
let go, then press the key. If you pause after `C-b`, a panel lists every key.
`C-b ?` shows them all.

To send a real Ctrl+B to a program, press it twice: `C-b C-b`.

**`Ctrl+Space`** opens the **command palette**: type a few letters of what you
want ("split", "zoom", "rename", "tiled", …) and press Enter. It also shows the
key for each command.

## Keys

| Keys | What they do |
| --- | --- |
| `C-b %` or `C-b \|` | Split: new pane to the right |
| `C-b "` or `C-b -` | Split: new pane below |
| `C-b ←↑↓→` | Move to the pane in that direction |
| `C-b o` / `C-b ;` | Next pane / previously used pane |
| `C-b C-←↑↓→` | Resize by 1. Repeat the arrows without `C-b` for half a second |
| `C-b M-←↑↓→` | Resize by 5 |
| `C-b z` | Zoom the pane to full screen; again to restore |
| `C-b x` | Close the pane (asks first) |
| `C-b {` / `C-b }` | Swap with the previous / next pane |
| `C-b Space` | Next layout: even-horizontal, even-vertical, main-vertical, main-horizontal, tiled |
| `C-b M-1` … `C-b M-5` | Pick one of those layouts |
| `C-b f` | Float the pane into a movable window, or tile it again |
| `C-b F` | New floating shell |
| `C-b m` / `C-b M` | Minimize a floating window / restore the last minimized one |
| `C-b c` | New workspace (a tmux "window") |
| `C-b n` / `C-b p` / `C-b l` | Next / previous / last workspace |
| `C-b 0` … `C-b 9` | Go to a workspace |
| `C-b ,` | Rename the workspace |
| `C-b &` | Close the workspace and its panes (asks first) |
| `C-b [` | Copy mode (see below) |
| `C-b ]` | Paste what you copied |
| `C-b :` | Command palette (same as `Ctrl+Space`) |
| `C-b N` | Notification center |
| `C-b a` / `C-b A` | AI agent: a new conversation / resume one ([agents.md](agents.md)) |
| `C-b r` | Reload the config file |
| `C-b d` | Detach (everything keeps running; `termos` reattaches) |
| `C-b (` / `C-b )` | Previous / next session |
| `C-b $` | Rename the session |

## Mouse

Everything can be done with the mouse alone.

- **◆ Menu**, at the left of the status line, opens the TermOS menu: new
  terminals, the apps, AI agents, project tasks, layouts, themes, workspaces
  and sessions, help, detach and quit. Point at an item with `▸` to open it;
  each item shows the key that does the same. (Keyboard: `C-b Enter`, then
  the arrows and Enter.)
- **Right-click** for what can be done where you clicked: in a terminal
  (paste, select text, split, zoom, float, rename, browse its folder,
  close), on a floating window's title (maximize, snap to a half, minimize,
  tile, close) or on a workspace tab (rename, layout, close). In programs
  that use the mouse themselves, hold **Shift**.
- **Click** a pane to focus it. Click a tab in the status line to switch
  workspace, `+` for a new one, a message to open the notification center,
  and a `▾name` chip to restore a minimized window.
- **Double-click** a floating window's title to maximize it, and again to
  give it its size back; double-click a tab to rename it.
- In the **command palette** and the **key list** (`C-b ?`), click a command
  to run it; the wheel scrolls the palette.
- **Drag** the line between two panes to resize them.
- **The desktop:** TermOS starts with your terminal in a window on the
  wallpaper. Click the shortcuts on the left (Terminal, Files, Monitor, AI,
  Settings, Help), or right-click anywhere on the wallpaper for the TermOS
  menu. Splitting a window tiles it; `[ui] start = "terminal"` starts with
  one full-screen terminal instead.
- **Floating windows:**
  - drag the title bar to move one; drop it on the left or right edge to snap
    it to that half, or on the top edge to maximize it;
  - drag a side or bottom edge to resize;
  - `–` minimizes and `×` closes.
- **Scroll the wheel** to look back through a pane's output. Scrolling back
  down to the bottom leaves copy mode.
- **Drag over text** to select it. It's copied when you release the button.
- **Programs that use the mouse** themselves (vim, htop, …) get the mouse
  inside their pane. Hold **Shift** to select text there anyway.

## Copy mode

`C-b [` (or scrolling up) enters copy mode. A `[offset/history]` badge appears
in the pane and `COPY` in the status line.

| Keys | What they do |
| --- | --- |
| `h j k l`, arrows | Move |
| `w b e` | Next word / previous word / end of word |
| `0` `^` `$` | Start of line / first non-blank / end of line |
| `g` `G` | Top of history / bottom |
| `C-u` `C-d`, `C-b` `C-f`, PgUp/PgDn | Half page / full page up and down |
| `H` `M` `L` | Top / middle / bottom of the screen |
| `v` (or Space) / `V` | Start a character / line selection |
| `y` or `Enter` | Copy the selection and leave |
| `/` `?` then `n` `N` | Search forward / backward, then repeat. Lowercase searches ignore case |
| `Esc` | Clear the selection, then leave |
| `q` | Leave |

Copied text goes to TermOS's paste buffer (`C-b ]`) and to your system
clipboard through OSC 52, which most modern terminals support.

## Configuration

The config file is `~/.config/termos/config.toml` (or under
`$XDG_CONFIG_HOME`). Every setting is optional. Changes apply as soon as you
save the file; if it has a mistake, TermOS keeps the old settings and tells you
the line and column.

To print every option with its default:

```bash
termos config default
```

To check a file:

```bash
termos config check
```

Example:

```toml
prefix = "C-a"          # tmux notation: C- Ctrl, M- Alt, S- Shift
mouse = true

[status]
position = "top"        # or "bottom"
clock = false

[floats]
border = "square"       # rounded | square | heavy | double
shadow = true

[theme]
base = "auto"           # auto | dark | light | high-contrast
accent = "#ff8800"      # "#rrggbb", "colourN" (0-255), "brightblue", "default"

[ui]
animations = true       # off over SSH anyway; reduced-motion = true turns them off
start = "desktop"       # desktop | terminal (one full-screen terminal, like tmux)
icons = "unicode"       # unicode | nerd (needs a Nerd Font) | ascii | none
wallpaper = "pattern"   # pattern | none | a picture's path

[keys.prefix]
"v" = "pane.split-right"
"s" = "pane.split-down"
"h" = "pane.focus-left"   # vim-style focus keys, if you like them
"%" = "none"              # remove a default binding

[keys.root]                 # no prefix needed
"M-Left" = "pane.focus-left"
"M-Right" = "pane.focus-right"
```

Every command name, such as `pane.zoom`, `layout.select tiled` or
`workspace.select 3`, is listed in the command palette.

Images, the image viewer, themes, icons, gradients, the wallpaper and
animations are described in [graphics.md](graphics.md). The file manager, system monitor, clipboard
history, settings and markdown viewer are in [apps.md](apps.md), and
status-line widgets in WebAssembly in [widgets.md](widgets.md). Languages,
editors, project templates, build/run/test panels and the Git UI are in
[languages.md](languages.md). The `tos` command, which every pane has, is in
[tos.md](tos.md).

## Test builds

Every push to GitHub builds TermOS for Linux (x86_64 and aarch64), macOS
(Apple silicon) and Windows (x86_64). These are **test builds**, not
releases: unsigned, and kept for 14 days.

1. Open the repository's **Actions** tab, pick a **CI** run for the commit or
   branch you want, and scroll to **Artifacts** (you need to be signed in to
   GitHub).
2. Download the one for your system:

   | File | System |
   | --- | --- |
   | `termos-<commit>-linux-x86_64.tar.gz` | Linux on x86_64, glibc 2.31 or newer (Ubuntu 20.04+, Debian 11+, WSL2) |
   | `termos-<commit>-linux-aarch64.tar.gz` | Linux on arm64, glibc 2.31 or newer (Raspberry Pi OS 64-bit, …) |
   | `termos-<commit>-linux-x86_64-musl.tar.gz`, `…-aarch64-musl.tar.gz` | Any Linux (static) |
   | `termos-<commit>-macos-arm64.tar.gz` | macOS 11 or newer on Apple silicon |
   | `termos-<commit>-windows-x86_64.zip` | Windows 10 1809 or newer, Windows 11 |

   GitHub wraps every artifact in a zip, so the Linux and macOS downloads
   are a zip holding the `.tar.gz` (which keeps the executable bit).
3. Unpack it and run `termos`. `BUILDINFO.txt` next to it says which commit
   and CI run it came from; mention it when you report a problem.

```bash
unzip termos-*-linux-x86_64.zip && tar -xzf termos-*-linux-x86_64.tar.gz
./termos-*-linux-x86_64/termos
```

Neither test builds nor releases are code-signed for macOS or Windows yet
(releases carry checksums and build attestations instead), so:

- **macOS** blocks a downloaded program the first time. Clear the quarantine
  flag once:

  ```bash
  xattr -d com.apple.quarantine termos-*-macos-arm64/termos
  ```

- **Windows** SmartScreen may say "Windows protected your PC": choose
  **More info → Run anyway**, or unblock the file first in PowerShell:

  ```powershell
  Unblock-File .\termos.exe
  ```

  The Windows build includes the C runtime, so nothing else needs
  installing.

## Windows notes

TermOS runs natively on Windows (Tier 2: tested by CI on every push, with
known gaps listed below).

- **Terminal:** use Windows Terminal, or another console that understands VT
  escape sequences. `termos` refuses to start in a console without them.
- **Shell:** panes start `pwsh`, then `powershell`, then `%COMSPEC%`
  (usually `cmd.exe`). `$SHELL` is ignored, because Git Bash sets it to a
  path Windows programs can't run; set `shell` in the config instead.
- **Files:** the config is `%APPDATA%\termos\config.toml`; logs and
  snapshots are in `%LOCALAPPDATA%\termos`. The server listens on a named
  pipe that only your user can open.
- **Closing the terminal** leaves the server and its sessions running, as on
  Linux: `termos` brings them back. The server runs without a window; stop
  it with `termos kill-server`.
- **Reinstalling or updating:** the server runs from its own copy of
  `termos.exe` (in `%LOCALAPPDATA%\termos\data\bin\server`), so
  `cargo install` or unpacking a new build can replace `termos.exe` while
  your sessions keep running. A terminal still attached runs `termos.exe`
  itself, so detach (`C-b d`) or close it first. To reinstall from a pane
  inside TermOS, move the running file aside first; Windows allows that, and
  the old file can be deleted once TermOS restarts:

  ```powershell
  Rename-Item "$env:USERPROFILE\.cargo\bin\termos.exe" termos.old.exe
  ```

  The running server keeps its version until `termos kill-server` (sessions
  are saved and restored).
- **Not there yet on Windows** (planned for M9b):
  - bracketed paste: a pasted block arrives as typed keys;
  - the Kitty keyboard protocol from the outer terminal;
  - saving at logoff or shutdown is best effort: `termos kill-server` first
    if you want to be sure;
  - snapshots don't record which program ran in a pane, so restore starts
    plain shells.

## Differences from tmux

- `termos` alone attaches to the session `main`, creating it if needed (like
  `tmux new -A -s main`). Sessions are restored after a crash or reboot
  (asking first, by default).
- `C-b l` is "last workspace", as in tmux. Vim-style `h j k l` focus keys are
  not bound by default; add them in `[keys.prefix]`.
- Floating windows (`C-b f`), the palette (`Ctrl+Space`), which-key hints, the
  notification center and the tutorial are TermOS additions.
- Configuration is TOML, not tmux's command language.

## Troubleshooting

- **Colours look wrong:** set `COLORTERM=truecolor` if your terminal supports
  24-bit colour. Otherwise force a colour depth with `TERMOS_COLOR=256` or
  `TERMOS_COLOR=16`.
- **Keys repeat or go missing:** run `termos debug keys` in the same terminal
  (outside TermOS) and type. It prints every key event as TermOS receives it
  and what it sends on. If a key is missing or doubled there, the terminal
  delivered it that way; if it is right there but wrong in a pane, it is
  TermOS's side. Either way, send the output along with a bug report.
- **Ctrl+B is taken by tmux** when you run TermOS inside tmux: set another
  prefix, e.g. `prefix = "C-a"`.
- **Logs** are in `~/.local/state/termos/log/` (Windows:
  `%LOCALAPPDATA%\termos\log`). Use `TERMOS_LOG=debug termos` for more
  detail. When the server fails to start, `termos` names the log directory.
- **"Already running inside TermOS":** you started `termos` in a TermOS pane.
  Use `termos --force` if you really want to nest. Commands that don't
  attach (`termos ls`, `termos msg …`, `termos detach`) work fine in a pane.
- **"protocol version mismatch":** you upgraded TermOS while an old server
  was running. Stop it with `termos kill-server` (your sessions are saved
  and offered for restore), then run `termos` again.
