# Sessions, detaching, SSH and scripting

TermOS works like tmux. A background **server** owns your sessions and
everything running in them. The `termos` you start in a terminal is a
**client** that shows one session. Close the terminal, lose the SSH
connection or press `C-b d`, and the session keeps running. Attach again,
from any terminal, and you're back where you left off.

## The basics

```bash
termos            # attach to the session "main" (creating it if needed)
```

- `C-b d` detaches. The client prints `[detached (from session main)]`.
- Run `termos` again to reattach.
- The server starts on its own the first time. It stops when its last
  session ends, when the last shell in it exits or after `termos kill-server`.

| Command | What it does |
| --- | --- |
| `termos` | Attach to `main`, creating it if needed |
| `termos new [-s NAME] [-d]` | New session, started in the current directory. `-d` creates it without attaching |
| `termos attach [-t NAME]` (`a`) | Attach to a session; without `-t`, the one used most recently |
| `termos detach [-s NAME]` | Detach the clients of a session (inside a pane: this session) |
| `termos ls` | List sessions, and saved sessions that can be restored |
| `termos kill-session [-t NAME]` | End a session and everything in it (inside a pane: this session) |
| `termos kill-server` | Stop the server. Its sessions are saved and can be restored |
| `termos restore [NAMES…]` | Restore saved sessions |
| `termos msg METHOD [JSON]` | Call the control API (see below) |

Inside TermOS:

| Keys | What they do |
| --- | --- |
| `C-b d` | Detach |
| `C-b (` / `C-b )` | Previous / next session |
| `C-b $` | Rename the session |
| `Ctrl+Space` → "Switch to session …" | Jump to any session |
| `Ctrl+Space` → "New session" | Create a session and switch to it |

A session can be named or referred to by id: `$1`, `$2`, …, as shown by
`termos msg session.list`.

## Several terminals at once

Any number of terminals can show the same session. Keys from any of them
go to the session, and each terminal has its own palette, prompts and
key hints. When terminals differ in size, `[session] window-size` decides:

```toml
[session]
window-size = "latest"   # the terminal used most recently (default)
# window-size = "smallest"  # everyone sees everything
# window-size = "largest"   # smaller terminals see the top-left part
```

## Over SSH

Run TermOS on the remote machine and attach to it there:

```bash
ssh -t myserver termos
```

- `-t` gives the remote `termos` a terminal.
- When the connection drops, your session keeps running on `myserver`.
  Reconnect with the same command.
- **Slow links:** TermOS sends only the cells that changed. It never queues
  frames for a slow connection: the next frame waits until the previous one
  was written, and then shows everything that changed since.
- **Copying:** text copied in copy mode reaches your local clipboard through
  OSC 52, if your local terminal allows it. Many do; tmux users may need
  `set -g set-clipboard on` if a local tmux sits in between.
- **Several independent servers** on one machine: use `-L NAME` (for example
  `termos -L work`). Each name has its own socket, sessions and snapshots.

The socket is `$XDG_RUNTIME_DIR/termos/<name>.sock`, or
`$TMPDIR/termos-<uid>/<name>.sock` where there is no runtime directory. It
is readable only by you, and the server refuses connections from other
users. `-S PATH` uses a socket path of your choice.

## After a crash or a reboot

TermOS saves each session's layout to
`~/.local/state/termos/sessions/<server>/<session>.json`:

- the workspaces, splits and floating windows;
- each pane's directory and the program running in front.

It saves a couple of seconds after the layout changes, and when the server
stops. The file is deleted when you end the session yourself, so only
sessions that were interrupted come back.

When a new server finds saved sessions, it asks in the status line:

```
Restore session main saved at 14:02? (y/n)
```

- **`y`:** each pane gets a shell in its old directory.
  - Programs on the restore list are started again in that shell, so
    quitting them leaves you at a prompt.
  - Other programs are named in a notification ("not restarted: cargo"),
    so you know what to start yourself.
- **`n`:** the saved sessions are kept. Restore them later with `termos restore`
  or the palette command "Restore saved sessions". `termos ls` shows them.

Programs are started again, not resumed: a long build or a REPL's state is
gone, as with tmux-resurrect.

```toml
[session]
restore = "ask"          # always | ask | never
restore-programs = ["vim", "nvim", "less", "man", "htop", "top"]
save-scrollback = false  # also save each pane's text
scrollback-lines = 2000
snapshot-delay-ms = 2000
```

**Privacy:**
- Snapshot files are readable only by you.
- A command line is saved only for programs on `restore-programs`. For
  anything else only the program's name is kept, so a password typed as a
  command-line argument never reaches the disk.
- Pane text is saved only with `save-scrollback = true`.

## Scripting with `termos msg`

`termos msg METHOD [JSON]` calls the server's JSON-RPC control API and
prints the result as JSON.

**Inside a TermOS pane** it talks to that pane's server and acts on that
pane's session unless you pass `"session"` yourself. TermOS sets
`$TERMOS=<socket>,<server pid>,$<session id>` in every pane for this.

```bash
termos msg session.list
termos msg pane.list
termos msg pane.split-right                    # any palette command is a method
termos msg layout.select '{"arg": "tiled"}'    # commands with an argument
termos msg command.run '{"command": "workspace.select 2"}'
termos msg notify.send '{"text": "build finished", "level": "info"}'
termos msg pane.send-text '{"pane": 3, "text": "make test\r"}'
termos msg pane.send-keys '{"keys": ["C-c"]}'
termos msg session.new '{"name": "logs"}'
termos msg session.rename '{"session": "logs", "name": "tail"}'
termos msg client.detach '{"session": "main"}'
termos msg server.info
```

| Method | Parameters | Result |
| --- | --- | --- |
| `server.info` | — | pid, version, protocol, sockets, counts, restorable sessions |
| `server.kill` | — | stops the server (sessions are saved) |
| `config.reload` | — | re-reads config.toml |
| `session.list` | — | id, name, workspaces, panes, clients, created |
| `session.new` | `name?`, `cwd?` | id, name |
| `session.kill` | `session?` | the name |
| `session.rename` | `session?`, `name` | the new name |
| `session.saved` | — | saved sessions not running |
| `session.restore` | `names?` | restored and failed names |
| `client.list` | — | id, session, size |
| `client.detach` | `session?` | how many were detached |
| `pane.list` | `session?` | id, session, workspace, name, title, kind, cwd, size, floating, focused |
| `pane.send-text` | `pane?`, `session?`, `text` | types text into a pane (default: the focused one) |
| `pane.send-keys` | `pane?`, `session?`, `keys` | sends keys in config notation (`C-c`, `Enter`, `M-x`) |
| `pane.kill` | `pane` | closes a pane without asking |
| `notify.send` | `text`, `level?`, `session?` | shows a notification |
| `command.run` | `command`, `session?` | runs a named command |
| `plugin.list` | — | plugins, their windows, commands and widgets |

`session` is a name or an id (`$1`). Without it, a method acts on the
session used most recently.

**Errors** are printed to stderr with a non-zero exit status. They are
JSON-RPC errors with these codes:
- `-32601`: unknown method;
- `-32602`: bad parameters;
- `-32004`: no such session or pane;
- `-32003`: forbidden;
- `-32001`: protocol version mismatch.

**Other languages:** the protocol is plain JSON lines over the Unix socket.
1. Connect.
2. Send `{"jsonrpc":"2.0","id":0,"method":"hello","params":{"version":1,"kind":"control"}}`.
3. Then send one request per line.
