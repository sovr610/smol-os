---
name: termos-dev
description: Set up and work on projects with TermOS's developer tools when running inside TermOS (the TERMOS environment variable is set). Use it to install a programming language with its language servers and debugger (mise-based termos lang), pin a project's language version, start a new project from a template, find and run a project's build, run and test commands, and open the user's editor or the Git UI.
---

# Projects in TermOS

These commands work in any TermOS pane, as `termos …` or `tos …`. Check that
`TERMOS` is set first; outside TermOS, `termos` may still be on `PATH`, but
the pane features below are not there.

## Languages

TermOS installs languages through mise, per user, with their language servers
and debuggers. Nothing is installed system-wide.

| Command | What it does |
| --- | --- |
| `termos lang list` | Every language TermOS knows, the versions installed (`*` is the default) and whether its servers and debugger are there |
| `termos lang install go` | The runtime (as the user's default), its language servers and its debugger. `python@3.12` picks a version |
| `termos lang use python@3.12` | Pins a version for the project in this folder (writes `mise.toml`) |
| `termos lang remove LANG[@VERSION]` | Uninstalls a version, or the language |

Ask the user before installing a language or changing a project's pinned
version: it downloads and changes their setup. A language missing from
`termos lang list` can still be installed with `mise` directly.

## New projects

`termos create --list` shows the templates (`go-cli`, `rust-cli`,
`python-cli`, `node-cli`, `web-page`, …). `termos create TEMPLATE DIR` makes a
small working project in a new or empty folder, with the language pinned.

## Build, run and test

`termos task` lists the current project's tasks, which TermOS finds from its
files (`Cargo.toml`, `go.mod`, `package.json`, `pyproject.toml`,
`CMakeLists.txt`, …) and from `[tasks]` in a `termos.toml` beside them.
`termos task build`, `termos task test`, `termos task run` or
`termos task NAME` runs one in your shell and prints its output.

- For a check you need the result of (build, tests, lint), run the task in
  your own shell, so you see its output and exit status.
- For something that keeps running (`termos task run` of a server, a
  watcher), start it in a pane with `tos run -p termos task run` and read it
  with `tos read ID` (see the `termos` skill).
- The user's own palette commands `task.build`, `task.run` and `task.test`
  open a panel at the bottom of their screen. Use them (through the
  `run_command` tool) only when the user wants to see the run there.

## Editor and Git

These are for the user, in panes of their own. Never run `tos edit` or
`tos git` in your own shell: they take over the terminal they run in.

- To show the user a file in their editor, use `open_file` or
  `tos open FILE`. It opens in a new pane, in TermOS's Neovim (set up with
  the installed language servers) unless they chose another editor
  (`[dev] editor`). Make your own changes with your own tools.
- To give the user lazygit for the folder, run the palette command
  `git.open` (the `run_command` tool), or `tos float -- termos git`. Use
  `git` itself for your own work.
