---
name: termos
description: Use the TermOS terminal desktop you are running in (the TERMOS environment variable is set). Use it to start dev servers, watchers, log tails and REPLs in their own TermOS pane instead of blocking your shell, to read those panes' output later, to show the user a file, picture or Markdown document, to post a desktop notification when long work finishes, and to copy text to the user's clipboard.
---

# Working inside TermOS

TermOS is the terminal desktop you are running in: tiled and floating panes,
workspaces, apps such as a file manager and Markdown viewer, and AI agents of
its own. First check that `TERMOS` is set in your environment
(`TERMOS_PANE` is the id of your own pane). If it is not, you are not in
TermOS and nothing here applies.

You reach TermOS in two ways. Use whichever you have:

- **Its MCP server** (named `termos`), when it is registered with you: the
  tools `list_panes`, `read_pane`, `open_pane`, `send_to_pane`, `open_file`,
  `run_command`, `notify` and `copy_to_clipboard`.
- **The `tos` command**, in any shell inside TermOS. `tos --help` lists every
  verb; every `termos` subcommand works through it too.

## Long-running programs get their own pane

Do not block your shell on a program that keeps running: a dev server, a file
watcher, `tail -f`, a REPL, a long benchmark. Start it in a pane beside
yours, where the user can watch it, and read its output when you need it.

| To | MCP tool | Shell |
| --- | --- | --- |
| Start a program in a new pane | `open_pane` with `command: ["npm", "run", "dev"]` (`place`: `auto`, `right`, `down`, `float`) | `tos run -p npm run dev` (prints the new pane's id; `--float` for a floating window) |
| List the panes | `list_panes` | `tos panes` (`*` marks yours) |
| Read a pane's last lines | `read_pane` with `pane` and `lines` | `tos read ID -n 100` |
| Type into a pane (a REPL, a prompt) | `send_to_pane` with `pane`, `text` and `enter: true` | (none) |

Keys and passwords a pane shows come back masked. Read a pane again after a
change (a rebuild, a restart) rather than assuming what it shows. A pane
closes when its program exits, so its output is gone after that: for a build
or a test run that finishes, run it in your own shell instead.

Never type into a pane that you did not start, unless the user asked you to.
Never close the user's panes.

## Showing the user things

- **A file, by its kind**, opened in TermOS's viewer: pictures, Markdown,
  folders (in the file manager), text (in the user's editor). Use the
  `open_file` tool, or run `tos open PATH`. `tos open https://…` opens a web
  address in the user's browser.
- **A notification** when something long finishes or needs their attention:
  `notify` (with `level` `info`, `warn` or `error`), or
  `tos notify --level warn "the build is slow today"`. Keep it to one short
  line, and do not notify for every step.
- **The clipboard:** `copy_to_clipboard`, or `echo text | tos copy`, when the
  user asked for something to paste elsewhere.

## TermOS commands

`run_command` (or `tos` verbs) runs the commands of TermOS's palette, e.g.
`layout.select tiled`, `monitor.open`, `markdown.open README.md`,
`files.open .`, `center.open` (AI agents and the system, live). TermOS
refuses some, such as quitting or closing panes. Change the user's layout or
workspaces only when they ask.

Useful `tos` verbs: `tos split right -- CMD`, `tos float -- CMD`, `tos zoom`,
`tos title NAME`, `tos layout NAME`, `tos workspace new`, `tos files [DIR]`,
`tos monitor`, `tos center`, `tos usage`, `tos info` (this pane and session).
