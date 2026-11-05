# Layouts and workspace tools

A project's workspaces in one file, a terminal one key away, the same keys
to several panes, saved commands, SSH hosts and recordings (M13b, D-120).

## Layout files

A layout is the workspaces of a project: their splits, folders and the
commands they start with.

**Where they live:**
- `~/.config/termos/layouts/NAME.toml` (Windows:
  `%APPDATA%\termos\layouts\NAME.toml`), its tables at the top;
- `[layout.NAME]` in a project's `termos.toml`, found from the folder you
  open it in or any folder above. A project's layout comes before one of
  yours with the same name.

```toml
# ~/.config/termos/layouts/dev.toml
workspace = "api"             # its tab's name (default: the layout's)
cwd = "."                     # relative to the folder it opens in

[[pane]]                      # the first pane
command = "nvim ."

[[pane]]                      # then each one split from the one before…
split = "right"               # right | down | float
size = "35%"                  # its share of the room it splits
command = "cargo watch -x test"

[[pane]]
split = "down"
of = 2                        # …or from pane 2
command = "npm run dev"
focus = true                  # the pane that has the focus
```

In a `termos.toml` the same tables are under `[layout.dev]`:
`[layout.dev]`, `[[layout.dev.pane]]`.

| Key | Where | What |
| --- | --- | --- |
| `workspace` | layout | The tab's name |
| `cwd` | layout, workspace, pane | A folder, relative to the one before (`~` is your home) |
| `split` | pane | `right`, `down` or `float`; not on the first pane |
| `of` | pane | The pane it splits, counting from 1 (default: the one before) |
| `size` | pane | Its share, `"35%"` |
| `command` | pane | Typed into the pane's shell, so you can run it again after it stops |
| `exec` | pane | `true`: run `command` as the pane's program instead (the pane closes when it ends) |
| `name` | pane | The pane's name, as Rename gives |
| `focus` | pane | `true` on the pane that gets the focus |

**Several workspaces:** `[[workspace]]` tables, each with `name`, `cwd`
and its `[[workspace.pane]]`s, give a layout more than one; it opens them
all and shows the first.

```toml
[[workspace]]
name = "code"
[[workspace.pane]]
command = "nvim"

[[workspace]]
name = "logs"
cwd = "/var/log"
[[workspace.pane]]
command = "tail -f syslog"
```

### Opening and saving

| | |
| --- | --- |
| `layout.open NAME [DIR]` (the palette lists `layout: NAME`) | Opens it, relative to `DIR` or the focused pane's folder |
| `termos layout open NAME [DIR]` | The same from a shell (`DIR` defaults to where you are) |
| `layout.save NAME`, `termos layout save NAME` | Writes the active workspace to `layouts/NAME.toml` |
| `termos layout list [DIR]` | The layouts found from here |
| `termos layout edit NAME` | Opens the file in your editor, started from an example when new |

**What saving keeps:** the tree of splits and floats, each pane's folder
and name, the focus, and the programs on `[session] restore-programs`
(vim, less, htop, …) as commands. Other programs, which may have a
password on their command line, are only named in a comment at the top,
as session snapshots do. App windows (Files, the monitor) become shells.

A layout that cannot be opened says why, with the file and line
(`dev.toml:4:9: …`), or the pane (`pane 3: of = 5 names no pane before
it`).

**Worktrees:** the workspace `worktree.open` opens (the agent, a shell
beside it, the servers below) is built the same way: it is TermOS's own
`worktree` layout.

## The drop-down terminal

**Prefix `` ` ``** (`quick.toggle`, or ◆ Menu → Drop-down terminal) shows
a terminal over the top of the screen, full width; again hides it and
gives the focus back. It keeps running while hidden, follows you to the
workspace you are on, and comes back after a restore.

```toml
[quick]
height = "40%"     # of the screen
key = "F12"        # a key that opens it without the prefix (none by default)
```

A key without the prefix never reaches the programs in your panes, which
is why there is none by default.

## Typing into several panes

**Prefix `S`** (`workspace.sync`) sends what you type and paste into the
focused pane to every terminal pane of the workspace; again stops.
`pane.sync` (right-click → Type here with the others) adds the focused pane
to a smaller set, or takes it out.

- The status line shows **SYNC**, and the synced panes are ringed in the
  accent colour.
- Each pane gets the keys in its own program's encoding.
- Mouse events and app windows are never synced.
- A synced pane waiting for you (an agent's question) is answered too.

## Saved commands

`[[snippets]]` in your config, or in a project's `termos.toml`:

```toml
[[snippets]]
name = "Tail a service's log"
command = "journalctl -fu {service:termos}"
run = true                 # press Enter after typing it (default false)
where = "pane"             # pane (the focused one) | new-pane | float
```

- **The palette** lists them as `snippet: NAME` (`snippet.run NAME`).
- Each `{param}` or `{param:default}` is asked for in the status line, in
  order, its default typed in; then the line is pasted (so the shell
  takes it as text) and, with `run`, Enter is pressed. `{{` and `}}` are
  braces.
- `termos snippet list` and `termos snippet run NAME [param=value…]` do it
  from a shell, typing into the pane they run in.

Parameters are plain text, typed into a shell: **do not put secrets in
them** or in the commands.

## SSH hosts

◆ Menu → **SSH** lists the hosts in `~/.ssh/config` and the files it
`Include`s, the ones you opened lately first; the palette has
`ssh HOST` (`ssh.open HOST`). Each opens a pane running your own
`ssh HOST`, so everything else in your SSH config still applies.

Only plain `Host` names are listed: patterns (`*.internal`, `!bad`) are not
hosts to open, and `Match` blocks are skipped. Recent hosts are kept in
`~/.local/state/termos/ssh-recent`.

## Recording a pane

**Prefix `R`** (`pane.record`, or right-click → Record this pane) records
the pane's output; again stops, and the toast names the file.

- **The file:** asciinema's v2 format (`.cast`): a header, then each piece
  of output with its time, and the pane's resizes. `asciinema play` and
  the asciinema player read it.
- **Where:** `~/.local/state/termos/recordings/` (`[record] dir`), readable
  only by you (mode 0600).
- **While it records** the pane shows **● REC**.
- **Only output is recorded,** never the keys you press. But what a shell
  echoes as you type is output, so typed commands are in the file, and so
  is anything a program prints. A password typed at a prompt that does not
  echo it is not.
- **Playing:** `termos play FILE [--speed 2] [--idle-limit 2]` plays one
  back in the terminal it runs in (in a TermOS pane: through the same
  emulator), at the recorded pace, faster with `--speed`, with pauses cut
  to `--idle-limit` seconds.

## Settings

```toml
[quick]
height = "40%"
# key = "F12"

[record]
# dir = "~/recordings"

[[snippets]]
name = "…"
command = "…"
```
