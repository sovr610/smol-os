# The built-in apps

TermOS has a few apps of its own. Each opens in a floating window you can
move, resize, minimize or tile like any other. Open them from the palette
(`Ctrl+Space`, then type a few letters of the name) or with `termos msg`:

| App | Palette | Command | Key |
| --- | --- | --- | --- |
| Command center ([center.md](center.md)) | "Command center" | `center.open` | prefix `C` |
| Files | "Files" | `files.open [FOLDER]` | |
| System monitor | "System monitor" | `monitor.open` | |
| Clipboard history | "Clipboard history" | `clipboard.open` | prefix `=` |
| Network | "Network: connections and Wi-Fi" | `network.open` | |
| Updates (a window running `termos update install`) | "Updates: install a newer TermOS" | `update.open` | |
| Settings | "Settings" | `settings.open` | |
| Packages ([packages.md](packages.md)) | "Packages" | `packages.open` | |
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
| `R` or `F5` | Reload (`F5` copies to the other pane when there are two) |
| `Space`, `*` | Mark the entry (and move on), or invert the marks |
| `?` | Find files and folders by name, below this folder |
| `Ctrl+F` | Find text in the files below this folder |
| `X` | Extract an archive (or, inside one, what is marked) |
| `z` | Make a zip of what is marked |
| `F2` on marks | Rename them in your editor (see below); `u` undoes |
| `D` | Disk usage: what takes the room in this folder |
| `O` | Open with: TermOS's viewer, the editor or the system's program |
| `Tab` | A second pane (and then switch between the two) |
| `F5`, `F6` | Copy or move to the other pane |
| `b`, `'` | Bookmark this folder (again: take it away); list the places |

A click selects; a click on the selected entry opens it.

**Marks:** copying (`y`, `x`, `F5`, `F6`), trashing (`d`) and zipping work on
the marked entries, or on the selected one when nothing is marked.

**Two panes:** `Tab` opens a second pane on the same folder, then switches
between them; `F5` copies and `F6` moves what is marked to the other pane,
as Midnight Commander does. `[files] two-pane = true` starts with two.
Extracting and zipping put their result in the other pane's folder when
there is one.

**Places:** with one pane and room for it, a sidebar lists Home, your
bookmarks and the trash (Linux and macOS); `'` lets you choose one with the
keys. `b` adds the folder shown to `[files] bookmarks` in your config file
(the rest of the file is kept as it is), or takes it away.

**Searching** lists the results in place of the folder: `Enter` opens one
(a text match opens your editor at its line), `Esc` goes back. Both kinds
honour `.gitignore`, and look in hidden files only when they are shown
(`.`). At most 1000 results.

**Archives** (zip, tar, tar.gz, tar.zst) open like folders, read-only:
`Enter` goes in, `h` goes up and out. Opening a file inside one extracts it
to a temporary folder first. Names that would leave the folder they are
extracted into (`../`, absolute paths) are left out, and so are links.

**Git:** in a repository, a column shows each entry's state: `M` changed,
`A` added, `R` renamed, `D` deleted, `U` in conflict, `?` untracked, `!`
ignored. A folder shows the strongest state inside it.

**Bulk rename:** mark the entries, press `F2`: their names open in your
editor, one numbered line each. Change the names (keep every line and its
number), save, close the editor, and press `Enter` in Files. It checks the
new names (two the same, one that exists already, a `/`), shows what will
change and asks; renaming goes through temporary names, so swaps work and
nothing is overwritten. `u` undoes the last bulk rename.

**Disk usage** (`D`) sizes each entry of the folder with everything inside
it, biggest first, with bars. `Enter` goes into a folder, `h` up, `d` moves
an entry to the trash (asks first), `Esc` goes back to the list. It counts
while it works, on its own thread.

**Open with** (`O`) remembers your choice for that kind of file (by
extension, in `~/.local/state/termos/open-with`): `Enter` on such a file
opens it that way from then on.

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

### Processes

`Tab` (or `processes.open`, ◆ Menu → Apps → Processes) switches to every
process: PID, user, CPU, memory, time, the TCP ports it listens on (Linux)
and its command line. Right-click a terminal → Show its processes opens it
with only that pane's program and what it started.

| Key | What it does |
| --- | --- |
| `↑` `↓`, `j` `k`, `PgUp` `PgDn`, `g` `G` | Move |
| `/` | Filter by name, command line or PID (`Esc` clears) |
| `s` | Sort by CPU, memory, time, PID or name |
| `t` | A tree by parent |
| `a` | All processes again, after a pane's |
| `e` | End (SIGTERM; on Windows, terminate) |
| `K` | Kill (SIGKILL) |
| `z` | Pause or resume (SIGSTOP, SIGCONT; not on Windows) |
| `+` `-` | Raise or lower the priority (renice; not on Windows) |
| `Tab` | Back to the overview |

Each action asks first; `[processes] confirm = false` stops asking.
Another user's process gives the system's refusal: there is no sudo from
here. Raising a priority usually needs that too. TermOS never touches its
own server or the system's first process.

The user column is filled on Linux; elsewhere it shows `-`.

It reads the machine once a second, and only while you can see it: a
minimized monitor, or one on another workspace, costs nothing.

What it can read depends on the system:

| | Linux | macOS | Windows |
| --- | --- | --- | --- |
| CPU, memory, disks, network, processes | ✓ | ✓ | ✓ |
| Temperatures | ✓ (hwmon sensors) | ✓ | where the firmware reports them |
| GPU load and memory | ✓ (AMD; NVIDIA with its driver; others where the driver reports them) | ✓ | ✓ |
| GPU temperature | ✓ (AMD, Intel, nouveau; NVIDIA with its driver) | ✓ (the system's sensors) | NVIDIA only |
| GPU power | ✓ (AMD, Intel, nouveau; NVIDIA with its driver) | not yet | NVIDIA only |

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

## Network

Connections and Wi-Fi (`network.open`, "Network" in the palette). Each
network interface is listed with whether it is wired or Wi-Fi, connected,
and its addresses; on a TermOS machine (bare metal) the Wi-Fi networks in
range follow, strongest first, with their security and signal.

| Key | Does |
| --- | --- |
| `↑` `↓` | choose a network |
| `Enter` | join it: a passphrase is asked for (shown as dots) unless it is open |
| `d` | forget a saved network |
| `r` | scan again |
| `Esc`, `q` | close |

A saved network is joined whenever it is in range. Elsewhere (TermOS in a
terminal) Wi-Fi belongs to the system's own network tools, and the page
shows the connections only.

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
