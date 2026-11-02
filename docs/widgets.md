# Status-line widgets in WebAssembly

A widget is a small program that draws a few words in the status line: a
timer, a counter, the weather from a file another program writes. Widgets
are WebAssembly components, so you can write them in any language that
targets WASI 0.2, and they run sandboxed inside TermOS.

## Adding a widget

```toml
[widgets.pomodoro]
path = "widgets/pomodoro.wasm"   # relative to config.toml's folder
settings = { minutes = "25" }    # passed to the widget as text
dirs = []                        # folders it may read; nothing else
max-width = 24                   # columns it may take
```

Widgets appear on the right of the status line, in name order, before the
clock. A click on one goes to the widget. When the `[widgets]` part of the
config changes, TermOS restarts the widgets.

If a widget fails to load, or breaks one of its limits, it is stopped and a
message says why; the other widgets and TermOS carry on.

## The sample: a pomodoro timer

`examples/widgets/pomodoro` is a complete widget in Rust. Click it to start a
countdown, again to pause; it says when the time is up. A ready build is in
the repository (TermOS's tests use it), so trying it needs no toolchain:

```bash
mkdir -p ~/.config/termos/widgets && cp crates/termos-shell/tests/fixtures/pomodoro.wasm ~/.config/termos/widgets/
```

Then add the `[widgets.pomodoro]` section above to your config. To build it
yourself:

```bash
rustup target add wasm32-wasip2
```

```bash
cargo build --release --target wasm32-wasip2 --manifest-path examples/widgets/pomodoro/Cargo.toml
```

The widget is `examples/widgets/pomodoro/target/wasm32-wasip2/release/termos_widget_pomodoro.wasm`.
(`cargo xtask widgets` builds it and refreshes the copy the tests use.)

## What a widget can do

Everything a widget is and does is in
[`crates/termos-plugin/wit/widget.wit`](../crates/termos-plugin/wit/widget.wit):

| Function | Called | Returns |
| --- | --- | --- |
| `init(settings)` | once, with the `settings` table | an update, or an error message for the user |
| `render(width)` | when the widget asked to be redrawn | styled spans, at most `width` columns |
| `tick()` | after the delay it asked for | an update |
| `on-event(event)` | when it is clicked (with the column) | an update |

An **update** says whether to redraw and, optionally, when to call `tick`
next (at least 100 ms later). A widget that asks for no tick is not called
again until it is clicked: it costs nothing while it waits. The sample only
ticks while its countdown runs.

Spans have a foreground and background (the status line's own colours, a
palette index or RGB) and bold, italic, underline and dim.

## The sandbox

| | |
| --- | --- |
| Network | none |
| Files | only the `dirs` you list, read-only |
| Environment variables, arguments | none |
| Clocks, random numbers | yes |
| Memory | 32 MiB |
| Time per call | 2 seconds, and a budget of instructions (more for `init`) |
| What it draws | at most 64 spans and `max-width` columns; control characters are removed |

## Writing one in Rust

```toml
[lib]
crate-type = ["cdylib"]

[dependencies]
wit-bindgen = "0.63"
```

```rust
wit_bindgen::generate!({ path: "path/to/termos/crates/termos-plugin/wit", world: "widget" });

struct Hello;

impl Guest for Hello {
    fn init(_settings: Vec<(String, String)>) -> Result<Update, String> {
        Ok(Update { redraw: true, next_tick_ms: None })
    }
    fn render(_width: u16) -> Vec<Span> {
        let style = termos::widget::types::Style {
            fg: termos::widget::types::Color::Default,
            bg: termos::widget::types::Color::Default,
            bold: true, italic: false, underline: false, dim: false,
        };
        vec![Span { text: "hello".into(), style }]
    }
    fn tick() -> Update {
        Update { redraw: false, next_tick_ms: None }
    }
    fn on_event(_event: Event) -> Update {
        Update { redraw: false, next_tick_ms: None }
    }
}

export!(Hello);
```

Build it with `cargo build --release --target wasm32-wasip2`; the `.wasm` in
`target/wasm32-wasip2/release/` is the widget. Other languages work the same
way with their component tools (componentize-py, jco, TinyGo), using the same
WIT file.

## Builds without widgets

The engine (wasmtime) is part of every TermOS build, but it starts only when
a widget is configured: without `[widgets]`, no thread runs and no memory is
used for it. To build TermOS without it at all:

```bash
cargo build --release -p termos-cli --no-default-features
```
