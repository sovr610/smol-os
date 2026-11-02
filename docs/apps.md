# The built-in apps

TermOS has a few apps of its own. Each opens in a floating window you can
move, resize, minimize or tile like any other. Open them from the palette
(`Ctrl+Space`, then type a few letters of the name) or with `termos msg`:

| App | Palette | Command | Key |
| --- | --- | --- | --- |
| Files | "Files" | `files.open [FOLDER]` | |
| System monitor | "System monitor" | `monitor.open` | |
| Clipboard history | "Clipboard history" | `clipboard.open` | prefix `=` |
| Settings | "Settings" | `settings.open` | |
| Markdown viewer | | `markdown.open FILE` | |
| Image viewer | | `image.open FILE` | |

```bash
termos msg files.open '{"arg": "~/Documents"}'
```

Relative paths are relative to the focused pane's folder, as in a shell,
and `~` is your home folder. Every app closes with `q` or `Esc`.

## Files

The folder on the left, a preview of the selected entry on the right:
pictures (in pixels where your terminal can show them), markdown, the start
of text files, or what is inside a folder.

| Key | What it does |
| --- | --- |
| `↑` `↓`, `j` `k`, `PgUp` `PgDn`, `g` `G` | Move |
| `Enter`, `→`, `l` | Open: enter a folder, or open a file (see below) |
| `←`, `h`, `Backspace` | Up to the parent folder |
| `~` | Your home folder |
| `/` | Filter by name (`Esc` clears) |
| `.` | Show or hide hidden files |
| `s` | Sort by name, size or date |
| `r` or `F2` | Rename |
| `n` | New folder |
| `y`, `x`, then `p` | Copy or cut, then paste into the folder shown |
| `d` or `Delete` | Move to the trash (asks first) |
| `e` | Edit in your editor |
| `R` or `F5` | Reload |

A click selects; a click on the selected entry opens it.

**Opening** picks the app by kind: folders open in Files, pictures in the
image viewer, markdown in the markdown viewer, and anything else in your
editor (`$VISUAL`, else `$EDITOR`, else `vi`; Notepad on Windows), in a new
pane next to the others. `termos msg file.open '{"arg": "notes.md"}'` does
the same from a script.

**Deleting** moves to your system's trash (the desktop trash on Linux, the
Recycle Bin, the macOS Trash), so a mistake can be undone there. Where there
is no trash (some servers, bare metal), Files says so and asks once more
before deleting for good.

Copying a folder copies everything in it. A copy into a folder that already
has the name gets a new one: `report (copy).txt`, `report (copy 2).txt`.

## Markdown viewer

Headings, emphasis, lists and task lists, quotes, code blocks, tables and
rules, wrapped to the window. Links to the web show their address. Pictures
next to the file are shown in place (in pixels where the terminal can);
pictures on the web are not downloaded.

Scroll with `↑` `↓` `j` `k`, `Space` and `b` for pages, `Ctrl+D` `Ctrl+U`
for half pages, `g` `G` for the start and end, or the mouse wheel. `r`
reloads the file. The window title shows how far down you are.

## System monitor

CPU (a chart of the last two minutes and a meter per core), memory, GPUs
(load, video memory, temperature and power where the system reports them),
network traffic, temperatures, disks and the busiest processes. `s` sorts the
processes by CPU or by memory.

It reads the machine once a second, and only while you can see it: a
minimized monitor, or one on another workspace, costs nothing.

What it can read depends on the system:

| | Linux | macOS | Windows |
| --- | --- | --- | --- |
| CPU, memory, disks, network, processes | ✓ | ✓ | ✓ |
| Temperatures | ✓ (hwmon sensors) | ✓ | where the firmware reports them |
| GPU load and memory | ✓ (AMD; NVIDIA with its driver; others where the driver reports them) | ✓ | ✓ |
| GPU temperature and power | ✓ (AMD, Intel, nouveau; NVIDIA with its driver) | not yet | NVIDIA only |

`termos debug hw` prints everything TermOS reads, which helps when something
looks wrong.

## Clipboard history

Everything you copy inside TermOS: copy mode, mouse selections, programs
that copy (OSC 52, as Neovim and tmux do), and apps. Newest first, each with
where it came from and how long ago.

| Key | What it does |
| --- | --- |
| `Enter` | Paste it into the pane you came from |
| `c` | Copy it again (to your terminal's clipboard, and to the top) |
| `p` | Pin it: pinned entries stay when the history fills up or is cleared |
| `d` | Delete it |
| `D` | Forget everything not pinned (asks first) |
| `/` | Filter |

The history is kept **in memory only**: never written to disk, logs or
session snapshots, and gone when the TermOS server stops. It holds 50 entries
by default (`[clipboard] history`; `0` keeps none), at most 1 MiB each.
`termos msg clipboard.clear` forgets it.

TermOS does not read your system's clipboard; it only knows what was copied
inside it.

## Settings

The common options of `config.toml`: theme, window borders and shadows,
animations, how images are shown, the status line, the mouse, key hints,
session restore, scrollback saving and the clipboard history. `↑` `↓`
choose, `←` `→` (or `Enter`) change.

Each change is saved into your `config.toml` at once, keeping your comments
and everything else in it, and applied right away. TermOS checks the file
would still be valid before writing it. If your config file has a mistake,
Settings shows it and changes nothing; `termos config check` points at the
line.
