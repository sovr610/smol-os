# `tos`: TermOS from your shell

Every TermOS pane has a `tos` command. It works the same in PowerShell, cmd,
bash, zsh and fish, on Linux, macOS and Windows, and it acts on the pane you
type it in.

```bash
tos split right -- htop
```

```bash
echo "remember this" | tos copy
```

```bash
tos notify --level warn "the build is slow today"
```

`tos` on its own, or `tos --help`, lists everything.

## Panes and windows

| Command | What it does |
| --- | --- |
| `tos split [auto\|right\|down] [-- COMMAND…]` | Splits this pane; with a command, runs it in the new pane |
| `tos run [--float] COMMAND…` | Runs a command in a new pane beside this one, or in a floating window |
| `tos float [-- COMMAND…]` | Floats this pane (or tiles it again); with a command, opens a floating window running it |
| `tos panes` | Lists this session's panes: id, workspace, kind, title, folder (`*` marks this one) |
| `tos read ID [-n LINES]` | Prints the last lines of a pane's text, its scrollback and screen, with keys and passwords masked |
| `tos zoom` | Zooms this pane, or unzooms it |
| `tos close` | Closes this pane (after asking) |
| `tos title NAME…` | Names this pane; `tos title` with no name shows the program's own title again |
| `tos layout NAME` | Picks a layout: `tiled`, `even-horizontal`, `even-vertical`, `main-vertical` or `main-horizontal` |
| `tos workspace N\|new\|next\|prev` | Goes to a workspace, or makes a new one |
| `tos menu` | Opens the TermOS menu |
| `tos palette` | Opens the command palette |

## Files and apps

| Command | What it does |
| --- | --- |
| `tos open PATH` | Opens a file by kind: pictures and markdown in their viewers, folders in Files, the rest in your editor |
| `tos open https://…` | Opens a web address in your browser (where there is one) |
| `tos files [DIR]` | Browses a folder (this one by default) |
| `tos monitor` | The system monitor |
| `tos center` | The command center: AI agents and the system, live and over time ([center.md](center.md)) |
| `tos settings` | Settings |
| `tos clipboard` | The clipboard history |
| `tos network`, `tos updates` | The network and updates windows |
| `tos notifications`, `tos keys`, `tos tutorial` | The notification center, every key, the tutorial |
| `tos about` | About TermOS: the version, where its files are, the licence |
| `tos theme NAME` | Picks a theme: `dark`, `light`, `high-contrast` or `auto` |

Relative paths are relative to the folder you are in.

## The clipboard and messages

| Command | What it does |
| --- | --- |
| `tos copy TEXT…` | Copies the text, or what is piped in (`git log -1 \| tos copy`) |
| `tos paste` | Prints the last thing copied (`tos paste > notes.txt`) |
| `tos notify [--level info\|warn\|error] TEXT…` | Shows a message in the status line and the notification center |

`tos copy` puts the text in TermOS's clipboard history and, like any copy in
TermOS, in your terminal's clipboard.

`-p` on `tos run`, `tos split` and `tos float` prints the new pane's id, for
`tos read`:

```bash
id=$(tos run -p npm run dev)
tos read "$id" -n 40
```

Coding agents use these through TermOS's skills ([skills.md](skills.md)).

## AI agents

| Command | What it does |
| --- | --- |
| `tos ai` | A new conversation with TermOS's agent |
| `tos ai claude`, `tos ai codex`, … | Claude Code, Codex CLI and the other command-line agents of `termos agent presets`, in a window |
| `tos harness [GOAL…]` | The harness in this pane's folder; with a goal, it plans it ([harness.md](harness.md)) |

## Everything else

Any `termos` command works through `tos` too:

```bash
tos lang install go
```

```bash
tos task test
```

```bash
tos create rust-cli hello
```

`tos edit FILE`, `tos git`, `tos ls`, `tos agent …` and the rest behave like
their `termos` versions.

## How it works

TermOS keeps a `tos` link to itself in its data folder
(`~/.local/share/termos/bin/` on Linux and macOS,
`%LOCALAPPDATA%\termos\data\bin\` on Windows) and puts that folder first on
every pane's `PATH`. On Windows the link is a hard link or a copy of
`termos.exe`, remade when TermOS is updated. `tos` finds its server and pane
from the `TERMOS` and `TERMOS_PANE` variables every pane has, so it only works
inside TermOS. Outside, it says so.

Names given with `tos title` (or Rename in a pane's menu) are not saved with
the session yet.
