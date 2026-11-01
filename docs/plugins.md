# Writing TermOS plugins

A plugin is a separate program that TermOS starts and that talks to it
over the session socket. It can:

- open **windows** and draw styled text in them, and receive the keys and
  clicks they get;
- show a **status-line widget**;
- add **commands** to the palette (and to `termos msg`);
- post **notifications**.

Plugins can be written in any language: the protocol is newline-delimited
JSON-RPC 2.0. Rust plugins can use the `termos-plugin` crate.

## Installing and enabling

A plugin lives in its own directory:

```text
~/.config/termos/plugins/clock/
├── clock           the program
└── plugin.toml
```

```toml
name = "clock"                  # must match the directory name
version = "0.1.0"
description = "A clock window and status widget"
exec = ["./clock"]              # with a "/": relative to this directory;
                                # a bare name ("python3") is found in PATH

[permissions]                   # everything is off unless asked for
windows = true                  # open, draw and close windows
status = true                   # a status-line widget
commands = true                 # palette commands
notify = true                   # notifications
```

A plugin runs **only** while it is listed in your config:

```toml
[plugins]
enabled = ["clock"]
```

- Saving the config starts newly listed plugins and stops removed ones.
- Plugins start with the server's first session.
- `termos msg plugin.list` shows which are running, with their windows,
  commands and widgets.
- A plugin's stderr goes to the server log
  (`~/.local/state/termos/log/server.log.*`).

If a plugin exits or crashes, its windows, widget and commands disappear and
you get a notification. It is not restarted automatically; save the config
(or run `termos msg config.reload`) to start it again.

## The example plugin

```bash
cargo build --release -p termos-plugin --example clock
mkdir -p ~/.config/termos/plugins/clock
cp target/release/examples/clock ~/.config/termos/plugins/clock/
```

Add the `plugin.toml` above and `enabled = ["clock"]`. A clock window
appears, the status line shows `⏱ HH:MM`, and the palette gets "Show or
hide the clock (clock)". The clock shows UTC.

## Rust: the `termos-plugin` crate

```rust
use termos_plugin::{Event, Line, Plugin, Span};

fn main() -> Result<(), termos_plugin::Error> {
    // TERMOS_SOCKET, TERMOS_PLUGIN_TOKEN and TERMOS_PLUGIN_NAME are set by
    // the server when it starts the plugin.
    let mut p = Plugin::connect_from_env()?;
    let w = p.open_window("hello", &[
        Line::from("Hello!"),
        Line::Styled(vec![Span::new("green").fg("green").bold()]),
    ])?;
    p.set_status("hi")?;
    p.register_command("greet", "Say hello")?;
    while let Some(event) = p.next_event()? {
        match event {
            Event::Command { name, session } if name == "greet" => {
                p.notify(&format!("hello, {}", session.unwrap_or_default()))?;
            }
            Event::Key { window, key } if key == "q" => p.close_window(window)?,
            Event::Closed { window } if window == w => break,
            _ => {}
        }
    }
    Ok(())
}
```

- `next_event` blocks until something happens. It returns `None` when the
  server stops; the plugin should then exit.
- `next_event_timeout(d)` returns `Ok(None)` after `d` without an event,
  which suits clocks and monitors.
- The SDK depends only on serde, and works without an async runtime.

## The protocol

**Connecting:**
1. Connect to the Unix socket in `TERMOS_SOCKET`.
2. Send one JSON object per line. The first must be:

```json
{"jsonrpc": "2.0", "id": 0, "method": "hello",
 "params": {"version": 1, "kind": "plugin", "client": "myplugin 1.0",
            "token": "<TERMOS_PLUGIN_TOKEN>"}}
```

The token changes every time the server starts the plugin and admits one
connection at a time. Keep it secret.

**Requests (plugin → server)** need a `"id"`. Each gets a result or an
error.

| Method | Parameters | Result | Permission |
| --- | --- | --- | --- |
| `window.open` | `title`, `lines?`, `session?` | `{"window": id}` | windows |
| `window.draw` | `window`, `lines`, `title?` | `{}` | windows |
| `window.close` | `window` | `{}` | windows |
| `status.set` | `text` (a line, or `null` to remove) | `{}` | status |
| `command.register` | `name`, `description` | `{"command": "<plugin>.<name>"}` | commands |
| `notify.send` | `text`, `level?` (`info`, `warn`, `error`) | `{}` | notify |

- `window.open` opens a floating window in `session`, or by default in the
  session used most recently.
- A request without the permission fails with code `-32003`, and the
  message says what to add to `plugin.toml`.
- Command names use lowercase letters, digits, `-`, `_` and `.`. They can't
  shadow built-in commands.

**Notifications (server → plugin)** have no `"id"`:

| Method | Parameters |
| --- | --- |
| `window.key` | `window`, `key` in config notation: `a`, `Enter`, `C-x`, `M-Left` |
| `window.mouse` | `window`, `kind` (`down`, `up`, `drag`, `scroll-up`, `scroll-down`), `x`, `y` (cells, from the window's top left) |
| `window.resize` | `window`, `cols`, `rows` (also sent when the window first appears) |
| `window.closed` | `window`: the user closed it |
| `command.invoked` | `name` (without the plugin prefix), `session` |

**Text.** A line is either a string or a list of spans:

```json
["plain text",
 [{"text": "bold red", "fg": "red", "bold": true},
  {"text": " on blue", "bg": "#1e3a8a", "italic": true}]]
```

- Span fields: `text`, `fg`, `bg`, `bold`, `dim`, `italic`, `underline`,
  `reverse`.
- Colours are written as in the config: `"default"`, `"#rrggbb"`,
  `"colourN"`, or names like `"brightblue"`.
- Control characters are removed.

**Limits:**
- 16 windows per plugin;
- 1000 lines per window, 1000 columns per line;
- titles of 64 characters;
- 40-column status widgets;
- 64 commands.

## A complete plugin in Python

```python
import json, os, socket

sock = socket.socket(socket.AF_UNIX)
sock.connect(os.environ["TERMOS_SOCKET"])
lines = sock.makefile("rb")
next_id = 0

def call(method, params):
    global next_id
    next_id += 1
    sock.sendall((json.dumps({"jsonrpc": "2.0", "id": next_id,
                              "method": method, "params": params}) + "\n").encode())
    while True:  # skip notifications that arrive before the answer
        msg = json.loads(lines.readline())
        if msg.get("id") == next_id:
            return msg

call("hello", {"version": 1, "kind": "plugin",
               "token": os.environ["TERMOS_PLUGIN_TOKEN"]})
call("window.open", {"title": "py", "lines": ["Hello from Python"]})
call("command.register", {"name": "ping", "description": "Ping"})
for raw in lines:  # notifications, until the server goes away
    msg = json.loads(raw)
    if msg.get("method") == "command.invoked":
        call("notify.send", {"text": "pong"})
```

The test suite runs a fuller version:
[`crates/termos-shell/tests/fixtures/hello_plugin.py`](../crates/termos-shell/tests/fixtures/hello_plugin.py).

## Security

- Plugins run as you, with your files. Enable only plugins you trust.
- The manifest's permissions limit what a plugin can do *in TermOS*: no
  windows without `windows`, and so on. They do not sandbox the program
  itself yet; that comes with the agent sandbox in milestone M6.
- Plugins never see your keys unless one of their windows has focus.
