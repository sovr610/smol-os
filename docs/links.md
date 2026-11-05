# Links, hints, search and finished commands

What is on the screen can be acted on. Links open, paths open in the
editor at their line, any pane's text can be found, and a long command
that finishes while you look elsewhere tells you (M13a, D-118).

## Links programs print

Programs can print links (OSC 8): `ls --hyperlink`, gcc's and cargo's
diagnostics, systemd, `gh`. TermOS keeps them and passes them on to the
terminal it runs in, which underlines and opens them as it does for any
program. TermOS's own hidden anchors (pictures, command blocks) are left
out, and so is any address that is not `http`, `https`, `mailto` or
`file`.

- `[ui] links = false` turns this off. TermOS also leaves it off for the
  Linux console and dumb terminals, which might print the sequence.
- On the bare-metal console a program's link is underlined while the
  pointer is over it.

## Ctrl+click

**Ctrl+click** a link, or something TermOS recognizes, to open it:

| What | What opening does |
| --- | --- |
| A web address (`https://…`, `http://…`) | Opens in your browser |
| An email address, or `mailto:` | Opens your mail program |
| A file path, with `:line` or `:line:col` (`src/main.rs:10:5`, `main.c:7`, Python's `File "x.py", line 42`, `a.cpp(12,3)`) | Opens the editor at that line |
| A file path without a line | Folders open in Files, pictures and Markdown in their viewers, other files in the editor |
| A git hash, an IP address | Copied (there is nothing to open) |

- **Hold Ctrl** and point: what Ctrl+click would open is underlined.
- **Programs that use the mouse** (vim, htop) get a plain Ctrl+click.
  Add Shift: **Ctrl+Shift+click** is TermOS's, as Shift always is.
- **Paths** count only if they exist, relative to the pane's folder (what
  the shell reported, else the program's). A path in a diff (`a/src/x.rs`)
  is found without its `a/` or `b/`.
- **Where addresses open:** on the machine your terminal client runs on
  (`xdg-open`, `open` on macOS, `explorer` on Windows). Where TermOS cannot
  open one for you, it copies it and says so: on the bare-metal console
  (no browser), and when your terminal reaches TermOS over SSH (open it
  from your own terminal, where the link also works).
- **`file:` links** (as `ls --hyperlink` prints) open in TermOS when they
  name this machine. A file on another machine has its address copied.

### The editor at a line

`file.edit PATH:LINE[:COL]` (and Ctrl+click, and hints) start the editor
at that line:

| Editor | How |
| --- | --- |
| TermOS's Neovim, Neovim, Vim | `+LINE`, or `+call cursor(LINE,COL)` |
| Helix, Sublime Text, Zed | `FILE:LINE:COL` |
| VS Code, VSCodium, Cursor, Windsurf | `-g FILE:LINE:COL` |
| nano | `+LINE,COL` |
| Emacs, micro, Kakoune | `+LINE:COL` |
| Anything else | the file alone |

## Hints

**Prefix `u`** (`pane.hints`, or right-click → Open a link or path) puts
a label of one or two letters on every link, path, hash and address in
the focused pane's view, the bottom one first:

- **the label** opens it;
- **Shift and the label** (capital letters) copies it;
- **Ctrl and the label** pastes it into the pane;
- **Esc** leaves.

`pane.hints url`, `path`, `hash` or `ip` labels only those; bind them to
keys of your own:

```toml
[keys.prefix]
"U" = "pane.hints path"
```

`[hints] alphabet` sets the letters, the easiest first (default
`asdfghjkl`).

## Search across panes

**Prefix `/`** (`search.all`, or ◆ Menu → Help → Search all panes) opens
a box like the palette and searches the text of every terminal pane in
the session: what is on screen and the history.

- **Type** plain text, or a regular expression after a leading `/`
  (`/error\[E[0-9]+\]`). Case is ignored unless you type a capital.
- **Results** are grouped by pane, each with its line. Up and Down (or
  `C-p`, `C-n`) choose; **Enter** goes there: the pane gets focus and
  copy mode, at the line, with the match selected (`y` copies it).
- `search.all --all-sessions` searches every session on the server, and
  Enter takes you to the session of the match.
- The search runs on its own thread over a copy of the text taken when
  the box opened, and stops at 500 matches.

## When a long command finishes

With TermOS's shell integration (bash, zsh, fish, PowerShell; see
[attention.md](attention.md#command-blocks)), TermOS knows when each
command starts and ends. A command that ran at least **15 seconds** and
finishes in a pane no one is looking at (another workspace, or your
terminal is in the background) says so:

- **its tab** gets a **✓**, or a **✗** in the error colour when it failed;
- **a toast** and the **notification center** say what ran, where, how it
  ended and how long it took: `` `make test` in 1:api failed (2) after
  3m12s ``;
- **a failure** is also a **system notification**, where your terminal
  shows them (`[notifications] system`). A success is not.

`[notifications] long-command` sets the seconds (`0` turns it off).

### Tell me when it is done

**`pane.notify-when-done`** (right-click → Tell me when it finishes, or
`tos notify --when-done` typed in the pane) asks about the command running
in the pane now, or if none is, the next one: however long it runs, and
even while you look at it.

```sh
tos notify --when-done; cargo build --release   # this line, when it ends
tos notify --when-done                          # alone: the next command
```

In a shell without TermOS's integration (over SSH, in a container) it
fires once the pane has been quiet for 5 seconds after printing
something.

## Settings

```toml
[ui]
links = true             # links programs print, shown and opened by your terminal

[hints]
alphabet = "asdfghjkl"   # hint labels, the easiest letters first

[notifications]
long-command = 15        # seconds; 0: off
system = true            # failures (and agents that need you) as system notifications
```
