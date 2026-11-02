// SPDX-License-Identifier: GPL-3.0-or-later
//! WASM status-line widgets with the sample pomodoro widget
//! (`examples/widgets/pomodoro`). The tests use the build kept in
//! `tests/fixtures/pomodoro.wasm`, or `TERMOS_TEST_WIDGET` when CI has just
//! built it from source.
#![cfg(feature = "wasm-widgets")]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use termos_core::config::WidgetConfig;
use termos_render::cell::Attrs;
use termos_shell::wasm::WidgetHost;

fn sample() -> PathBuf {
    std::env::var_os("TERMOS_TEST_WIDGET").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pomodoro.wasm"),
        PathBuf::from,
    )
}

fn host(settings: &[(&str, &str)]) -> WidgetHost {
    let mut widgets = BTreeMap::new();
    widgets.insert(
        "pomodoro".to_owned(),
        WidgetConfig {
            path: sample(),
            dirs: Vec::new(),
            settings: settings
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            max_width: 24,
        },
    );
    WidgetHost::start(&widgets, std::path::Path::new("/"), || {}).expect("the thread starts")
}

/// Waits until `pred` holds for the widget's text (and boldness).
fn wait_for(host: &WidgetHost, what: &str, pred: impl Fn(&str, bool) -> bool) {
    // Compiling the component can take a few seconds on a slow runner.
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        let drawn = host.drawn();
        if let Some((_, cells)) = drawn.iter().find(|(n, _)| n == "pomodoro") {
            let text: String = cells.iter().map(|(t, _)| t.as_str()).collect();
            let bold = cells.iter().any(|(_, s)| s.attrs.contains(Attrs::BOLD));
            if pred(&text, bold) {
                return;
            }
        }
        let problems = host.take_problems();
        assert!(problems.is_empty(), "waiting for {what}: {problems:?}");
        assert!(
            Instant::now() < until,
            "timed out waiting for {what}: {drawn:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_problem(host: &WidgetHost) -> String {
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(p) = host.take_problems().into_iter().next() {
            return p;
        }
        assert!(Instant::now() < until, "no problem reported");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn the_sample_draws_ticks_only_while_running_and_hears_clicks() {
    let h = host(&[("minutes", "1")]);
    wait_for(&h, "the first drawing", |t, bold| t == "🍅 1:00" && !bold);
    // Ready: it asked for no ticks, so nothing changes by itself.
    std::thread::sleep(Duration::from_millis(1500));
    wait_for(&h, "still ready", |t, _| t == "🍅 1:00");
    h.click("pomodoro", 0);
    wait_for(&h, "running", |_, bold| bold);
    wait_for(&h, "a tick a second later", |t, _| t == "🍅 0:59");
    h.click("pomodoro", 0);
    wait_for(&h, "paused", |t, bold| t.ends_with('‖') && !bold);
}

#[test]
fn a_runaway_widget_is_stopped() {
    let h = host(&[("spin", "true")]);
    let p = wait_problem(&h);
    assert!(p.contains("pomodoro") && p.contains("out of fuel"), "{p}");
    assert!(h.drawn().is_empty());
}

#[test]
fn bad_settings_and_bad_files_are_reported() {
    let h = host(&[("colour", "red")]);
    let p = wait_problem(&h);
    assert!(
        p.contains("refused its settings") && p.contains("colour"),
        "{p}"
    );

    let dir = tempfile::tempdir().expect("tempdir");
    let junk = dir.path().join("junk.wasm");
    std::fs::write(&junk, b"not wasm").expect("write");
    let mut widgets = BTreeMap::new();
    widgets.insert(
        "junk".to_owned(),
        WidgetConfig {
            path: junk,
            dirs: Vec::new(),
            settings: BTreeMap::new(),
            max_width: 24,
        },
    );
    let h = WidgetHost::start(&widgets, dir.path(), || {}).expect("starts");
    let p = wait_problem(&h);
    assert!(
        p.contains("junk") && p.contains("not a widget component"),
        "{p}"
    );
}
