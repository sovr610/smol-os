// SPDX-License-Identifier: GPL-3.0-or-later
//! WASM status-line widgets (D-013, D-042, D-051): components of the
//! `termos:widget` WIT world (`crates/termos-plugin/wit/widget.wit`), run by
//! wasmtime.
//!
//! The engine exists only while a widget is configured: without widgets no
//! thread starts and nothing is loaded. With widgets, one thread runs them
//! all on its own current-thread runtime. A widget is called only when it
//! asked to be (`next-tick-ms`), when it is clicked, or once at start, so a
//! widget that does not tick costs nothing at rest.
//!
//! Every call is bounded: fuel (instructions), memory, a wall-clock limit,
//! and the size of what it draws. A widget that breaks a bound, traps or
//! fails to load is stopped and reported; the others carry on. Widgets get
//! no network, no environment and no files except the folders their config
//! grants, read-only.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use termos_core::config::WidgetConfig;
use termos_render::cell::{Attrs, Style};
use termos_render::color::Color;
use tokio::sync::mpsc;
use tokio::time::Instant;
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Engine, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::plugins::Cells;

wasmtime::component::bindgen!({
    path: "../termos-plugin/wit",
    world: "widget",
    exports: { default: async },
});

use termos::widget::types::{Color as WColor, Event as WEvent, Span as WSpan};

/// Instructions `init` may run (it may set up a language runtime).
const INIT_FUEL: u64 = 500_000_000;
/// Instructions any other call may run.
const CALL_FUEL: u64 = 50_000_000;
/// Wall-clock time any call may take (waiting included).
const CALL_TIME: Duration = Duration::from_secs(2);
/// Memory a widget may use.
const MEMORY: usize = 32 * 1024 * 1024;
/// The fastest a widget may tick.
const MIN_TICK: Duration = Duration::from_millis(100);
/// The most spans one render may return.
const MAX_SPANS: usize = 64;

/// What the server sees of the widgets.
#[derive(Debug, Default)]
struct Shared {
    /// The latest drawing of each running widget, by name.
    drawn: BTreeMap<String, Cells>,
    /// Problems to show the user (a widget failed or stopped).
    problems: Vec<String>,
}

enum Cmd {
    Click { name: String, column: u16 },
}

/// The running widgets. Dropping it stops them.
pub struct WidgetHost {
    tx: mpsc::UnboundedSender<Cmd>,
    shared: Arc<Mutex<Shared>>,
    names: Vec<String>,
}

impl std::fmt::Debug for WidgetHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WidgetHost")
            .field("widgets", &self.names)
            .finish_non_exhaustive()
    }
}

impl WidgetHost {
    /// Starts the configured widgets; `base` is the directory relative
    /// paths start from. `changed` is called (from the widgets' thread)
    /// whenever a drawing or a problem is new.
    pub fn start(
        widgets: &BTreeMap<String, WidgetConfig>,
        base: &Path,
        changed: impl Fn() + Send + Sync + 'static,
    ) -> std::io::Result<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        let shared = Arc::new(Mutex::new(Shared::default()));
        let configs: Vec<(String, WidgetConfig)> = widgets
            .iter()
            .map(|(name, c)| {
                let mut c = c.clone();
                c.path = resolve(base, &c.path);
                c.dirs = c.dirs.iter().map(|d| resolve(base, d)).collect();
                (name.clone(), c)
            })
            .collect();
        let names = configs.iter().map(|(n, _)| n.clone()).collect();
        let s = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("wasm-widgets".into())
            .spawn(move || run(configs, rx, &s, &changed))?;
        Ok(Self { tx, shared, names })
    }

    /// The widgets' drawings, in name order (stopped widgets are left out).
    pub fn drawn(&self) -> Vec<(String, Cells)> {
        self.shared
            .lock()
            .map(|s| {
                s.drawn
                    .iter()
                    .map(|(n, c)| (n.clone(), c.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Problems since last asked.
    pub fn take_problems(&self) -> Vec<String> {
        self.shared
            .lock()
            .map(|mut s| std::mem::take(&mut s.problems))
            .unwrap_or_default()
    }

    pub fn click(&self, name: &str, column: u16) {
        let _ = self.tx.send(Cmd::Click {
            name: name.to_owned(),
            column,
        });
    }
}

fn resolve(base: &Path, p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_owned()
    } else {
        base.join(p)
    }
}

/// A widget's store data: its WASI context and limits.
struct State {
    wasi: WasiCtx,
    table: ResourceTable,
    limits: StoreLimits,
}

impl WasiView for State {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

struct Running {
    name: String,
    max_width: u16,
    store: Store<State>,
    bindings: Widget,
    next_tick: Option<Instant>,
}

/// The widgets' thread.
fn run(
    configs: Vec<(String, WidgetConfig)>,
    mut rx: mpsc::UnboundedReceiver<Cmd>,
    shared: &Mutex<Shared>,
    changed: &(dyn Fn() + Send + Sync),
) {
    let report = |problem: String| {
        tracing::warn!("{problem}");
        if let Ok(mut s) = shared.lock() {
            s.problems.push(problem);
        }
        changed();
    };
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => return report(format!("widgets: cannot start: {e}")),
    };
    rt.block_on(async {
        let engine = match engine() {
            Ok(e) => e,
            Err(e) => return report(format!("widgets: cannot start the engine: {e}")),
        };
        let mut linker: Linker<State> = Linker::new(&engine);
        if let Err(e) = wasmtime_wasi::p2::add_to_linker_async(&mut linker) {
            return report(format!("widgets: cannot set up WASI: {e}"));
        }
        let mut running = Vec::new();
        for (name, config) in configs {
            match start_one(&engine, &linker, &name, &config).await {
                Ok(mut w) => {
                    draw(&mut w, shared).await.map_or_else(
                        |e| report(format!("widget {name} stopped: {e}")),
                        |()| running.push(w),
                    );
                    changed();
                }
                Err(e) => report(format!("widget {name} did not start: {e}")),
            }
        }
        loop {
            let next = running.iter().filter_map(|w| w.next_tick).min();
            let cmd = match next {
                Some(at) => {
                    tokio::select! {
                        cmd = rx.recv() => Some(cmd),
                        () = tokio::time::sleep_until(at) => None,
                    }
                }
                None => Some(rx.recv().await),
            };
            match cmd {
                // The host was dropped.
                Some(None) => break,
                Some(Some(Cmd::Click { name, column })) => {
                    if let Some(i) = running.iter().position(|w| w.name == name) {
                        let w = &mut running[i];
                        let r = call(w, |b, s| {
                            Box::pin(b.call_on_event(s, WEvent::Click(column)))
                        })
                        .await;
                        if let Err(e) = settle(w, r, shared).await {
                            let w = running.remove(i);
                            stop(&w.name, shared);
                            report(format!("widget {} stopped: {e}", w.name));
                        }
                        changed();
                    }
                }
                None => {
                    let now = Instant::now();
                    let mut i = 0;
                    while i < running.len() {
                        if running[i].next_tick.is_some_and(|t| t <= now) {
                            let w = &mut running[i];
                            w.next_tick = None;
                            let r = call(w, |b, s| Box::pin(b.call_tick(s))).await;
                            if let Err(e) = settle(w, r, shared).await {
                                let w = running.remove(i);
                                stop(&w.name, shared);
                                report(format!("widget {} stopped: {e}", w.name));
                                continue;
                            }
                            changed();
                        }
                        i += 1;
                    }
                }
            }
        }
    });
}

fn engine() -> wasmtime::Result<Engine> {
    let mut config = wasmtime::Config::new();
    config.consume_fuel(true);
    // Small stacks: widgets draw a few words.
    config.max_wasm_stack(512 * 1024);
    Engine::new(&config)
}

fn stop(name: &str, shared: &Mutex<Shared>) {
    if let Ok(mut s) = shared.lock() {
        s.drawn.remove(name);
    }
}

async fn start_one(
    engine: &Engine,
    linker: &Linker<State>,
    name: &str,
    config: &WidgetConfig,
) -> Result<Running, String> {
    let bytes = std::fs::read(&config.path)
        .map_err(|e| format!("cannot read {}: {e}", config.path.display()))?;
    // Compiling can take a moment; it happens on this thread only.
    let component =
        Component::new(engine, &bytes).map_err(|e| format!("not a widget component: {e}"))?;
    let mut wasi = WasiCtxBuilder::new();
    wasi.allow_tcp(false)
        .allow_udp(false)
        .allow_ip_name_lookup(false);
    for dir in &config.dirs {
        let guest = dir.to_string_lossy().into_owned();
        wasi.preopened_dir(dir, guest, wasmtime_wasi::FsPerms::ReadOnly)
            .map_err(|e| format!("cannot grant {}: {e}", dir.display()))?;
    }
    let state = State {
        wasi: wasi.build(),
        table: ResourceTable::new(),
        limits: StoreLimitsBuilder::new()
            .memory_size(MEMORY)
            .instances(16)
            .tables(16)
            .memories(4)
            .build(),
    };
    let mut store = Store::new(engine, state);
    store.limiter(|s| &mut s.limits);
    store
        .set_fuel(INIT_FUEL)
        .map_err(|e| format!("cannot limit it: {e}"))?;
    let bindings = tokio::time::timeout(
        CALL_TIME,
        Widget::instantiate_async(&mut store, &component, linker),
    )
    .await
    .map_err(|_| "it took too long to start".to_owned())?
    .map_err(|e| format!("cannot start it: {e}"))?;
    let settings: Vec<(String, String)> = config
        .settings
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let mut w = Running {
        name: name.to_owned(),
        max_width: config.max_width,
        store,
        bindings,
        next_tick: None,
    };
    let update = tokio::time::timeout(CALL_TIME, w.bindings.call_init(&mut w.store, &settings))
        .await
        .map_err(|_| "init took too long".to_owned())?
        .map_err(|e| trap_text(&e))?
        .map_err(|e| format!("it refused its settings: {e}"))?;
    schedule(&mut w, &update);
    Ok(w)
}

type CallFuture<'a, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = wasmtime::Result<T>> + Send + 'a>>;

/// One bounded call into a widget.
async fn call<T>(
    w: &mut Running,
    f: impl for<'a> FnOnce(&'a Widget, &'a mut Store<State>) -> CallFuture<'a, T>,
) -> Result<T, String> {
    w.store
        .set_fuel(CALL_FUEL)
        .map_err(|e| format!("cannot limit it: {e}"))?;
    tokio::time::timeout(CALL_TIME, f(&w.bindings, &mut w.store))
        .await
        .map_err(|_| "a call took too long".to_owned())?
        .map_err(|e| trap_text(&e))
}

/// Applies an update: schedules the next tick and redraws if asked.
async fn settle(
    w: &mut Running,
    update: Result<Update, String>,
    shared: &Mutex<Shared>,
) -> Result<(), String> {
    let update = update?;
    schedule(w, &update);
    if update.redraw {
        draw(w, shared).await?;
    }
    Ok(())
}

fn schedule(w: &mut Running, u: &Update) {
    w.next_tick = u
        .next_tick_ms
        .map(|ms| Instant::now() + Duration::from_millis(u64::from(ms)).max(MIN_TICK));
}

async fn draw(w: &mut Running, shared: &Mutex<Shared>) -> Result<(), String> {
    let width = w.max_width;
    let spans = call(w, |b, s| Box::pin(b.call_render(s, width))).await?;
    let cells = cells(&spans, width);
    if let Ok(mut s) = shared.lock() {
        s.drawn.insert(w.name.clone(), cells);
    }
    Ok(())
}

fn trap_text(e: &wasmtime::Error) -> String {
    if let Some(trap) = e.downcast_ref::<wasmtime::Trap>() {
        return match trap {
            wasmtime::Trap::OutOfFuel => "it ran too long (out of fuel)".into(),
            other => format!("it crashed ({other})"),
        };
    }
    format!("it failed: {e}")
}

/// Spans as status-line cells: control characters removed, at most
/// `max_width` columns and [`MAX_SPANS`] spans.
fn cells(spans: &[WSpan], max_width: u16) -> Cells {
    let mut out = Vec::new();
    let mut left = usize::from(max_width);
    for s in spans.iter().take(MAX_SPANS) {
        let mut text = String::new();
        for ch in s.text.chars().filter(|c| !c.is_control()) {
            let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if w > left {
                left = 0;
                break;
            }
            left -= w;
            text.push(ch);
        }
        if !text.is_empty() {
            out.push((text, style(&s.style)));
        }
        if left == 0 {
            break;
        }
    }
    out
}

fn color(c: &WColor) -> Color {
    match c {
        WColor::Default => Color::Default,
        WColor::Indexed(i) => Color::Indexed(*i),
        WColor::Rgb((r, g, b)) => Color::rgb(*r, *g, *b),
    }
}

fn style(s: &termos::widget::types::Style) -> Style {
    let mut out = Style::new().fg(color(&s.fg)).bg(color(&s.bg));
    let mut attrs = Attrs::empty();
    for (on, a) in [
        (s.bold, Attrs::BOLD),
        (s.italic, Attrs::ITALIC),
        (s.underline, Attrs::UNDERLINE),
        (s.dim, Attrs::DIM),
    ] {
        if on {
            attrs |= a;
        }
    }
    out.attrs = attrs;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(text: &str) -> WSpan {
        WSpan {
            text: text.into(),
            style: termos::widget::types::Style {
                fg: WColor::Rgb((1, 2, 3)),
                bg: WColor::Default,
                bold: true,
                italic: false,
                underline: false,
                dim: false,
            },
        }
    }

    #[test]
    fn drawings_are_cleaned_and_cut() {
        let c = cells(&[span("ab\x1b[31mc"), span("界界界")], 5);
        let texts: Vec<&str> = c.iter().map(|(t, _)| t.as_str()).collect();
        // The escape is gone ("[31m" stays as text); the width stops at 5.
        assert_eq!(texts, ["ab[31"]);
        let c = cells(&[span("hi "), span("界界")], 6);
        assert_eq!(c.len(), 2);
        assert_eq!(c[1].0, "界");
        assert_eq!(c[0].1.fg, Color::rgb(1, 2, 3));
        assert!(c[0].1.attrs.contains(Attrs::BOLD));
    }
}
