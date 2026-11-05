// SPDX-License-Identifier: GPL-3.0-or-later
//! Sound (M13e, D-128): the outputs PipeWire offers (speakers, HDMI, a
//! Bluetooth headset), which one plays, and each one's volume.
//!
//! Choosing an output makes it the default, so what plays moves to it,
//! and saves it as `[sound] device` for the next start. Everything runs
//! `wpctl` on a thread; the page reads the outputs again while it is on
//! screen (a headset that connects appears), and after a change to the
//! default output it asks the desktop to read the volume again for the
//! status line (`sound.refresh`).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect as TuiRect;
use termos_core::app::{App, AppEffect, AppWaker, tui_style};
use termos_core::input::{KeyCode, KeyEvent};
use termos_hw::sound::{Sink, Volume};
use termos_render::cell::{Attrs, Style};
use termos_render::theme::Theme;

/// How often the outputs are read again while the page is on screen.
const INTERVAL: Duration = Duration::from_secs(5);

/// How much ← and → change a volume, in percent.
const STEP: i8 = 5;

/// Where the outputs come from, and what changes them.
pub trait Backend: Send + Sync {
    /// `None` where TermOS does not run the sound (no PipeWire).
    fn sinks(&self) -> Option<Result<Vec<Sink>, String>>;
    fn set_default(&self, id: u32) -> Result<(), String>;
    fn change(&self, id: u32, delta: i8) -> Result<Volume, String>;
    fn toggle_mute(&self, id: u32) -> Result<Volume, String>;
}

#[derive(Default)]
struct Shared {
    sinks: Option<Result<Vec<Sink>, String>>,
    loaded: bool,
    message: Option<String>,
    /// The default output changed: the status line reads it again.
    status_changed: bool,
}

/// A change to make on the thread: its message, and whether the default
/// output changed.
type Job = Box<dyn FnOnce(&dyn Backend) -> (String, bool) + Send>;

pub struct SoundPage {
    backend: Arc<dyn Backend>,
    shared: Arc<Mutex<Shared>>,
    waker: Arc<Mutex<Option<AppWaker>>>,
    selected: usize,
    next: Option<Instant>,
    /// Where `[sound] device` is saved (none: not saved).
    config: Option<PathBuf>,
}

impl std::fmt::Debug for SoundPage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SoundPage")
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

/// A volume as a bar `width` cells wide.
pub fn bar(level: f32, width: usize) -> String {
    let full = ((level.clamp(0.0, 1.0) * width as f32).round() as usize).min(width);
    format!("{}{}", "█".repeat(full), "░".repeat(width - full))
}

impl SoundPage {
    /// The page for this machine, saving the chosen output in `config`.
    pub fn open(config: Option<PathBuf>) -> Self {
        Self::with_backend(Arc::new(System), config)
    }

    pub fn with_backend(backend: Arc<dyn Backend>, config: Option<PathBuf>) -> Self {
        Self {
            backend,
            shared: Arc::new(Mutex::new(Shared::default())),
            waker: Arc::new(Mutex::new(None)),
            selected: 0,
            next: None,
            config,
        }
    }

    /// Reads the outputs on a thread, after `job` if there is one; the
    /// job's answer becomes the page's message.
    fn background(&self, job: Option<Job>) {
        let (backend, shared, waker) = (
            Arc::clone(&self.backend),
            Arc::clone(&self.shared),
            Arc::clone(&self.waker),
        );
        std::thread::spawn(move || {
            let done = job.map(|j| j(backend.as_ref()));
            let sinks = backend.sinks();
            if let Ok(mut s) = shared.lock() {
                s.sinks = sinks;
                s.loaded = true;
                if let Some((msg, status)) = done {
                    s.message = Some(msg);
                    s.status_changed |= status;
                }
            }
            if let Some(w) = waker.lock().ok().and_then(|w| w.clone()) {
                w.wake();
            }
        });
    }

    /// Reads the outputs now (tests).
    pub fn refresh_now(&self) {
        let sinks = self.backend.sinks();
        if let Ok(mut s) = self.shared.lock() {
            s.sinks = sinks;
            s.loaded = true;
        }
    }

    fn sinks(&self) -> Vec<Sink> {
        self.shared
            .lock()
            .ok()
            .and_then(|s| match &s.sinks {
                Some(Ok(v)) => Some(v.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    fn say(&self, msg: impl Into<String>) {
        if let Ok(mut s) = self.shared.lock() {
            s.message = Some(msg.into());
        }
    }

    /// Makes `sink` the default and saves it for the next start.
    fn choose(&mut self, sink: &Sink) {
        let (id, name) = (sink.id, sink.name.clone());
        let config = self.config.clone();
        self.say(format!("switching to {name}…"));
        self.background(Some(Box::new(move |b| {
            if let Err(e) = b.set_default(id) {
                return (format!("cannot switch to {name}: {e}"), false);
            }
            let saved = config.map(|path| {
                crate::config_edit::edit(&path, |d| {
                    crate::config_edit::table(d, "sound")
                        .insert("device", toml_edit::value(name.as_str()));
                })
            });
            match saved {
                Some(Err(e)) => (format!("{name} plays now; {e}"), true),
                _ => (format!("{name} plays now"), true),
            }
        })));
    }
}

impl App for SoundPage {
    fn reopen(&self) -> Option<String> {
        Some("sound.open".to_owned())
    }

    fn icon(&self) -> termos_render::icons::Icon {
        termos_render::icons::Icon::Sound
    }

    fn title(&self) -> String {
        "Sound".into()
    }

    fn handle_key(&mut self, key: KeyEvent) -> AppEffect {
        let sinks = self.sinks();
        let current = sinks.get(self.selected).cloned();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return AppEffect::Close,
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(sinks.len().saturating_sub(1));
            }
            KeyCode::Char('r') => self.background(None),
            KeyCode::Enter => {
                let Some(s) = current else {
                    return AppEffect::None;
                };
                if s.default {
                    self.say(format!("{} plays already", s.name));
                } else {
                    self.choose(&s);
                }
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char('-' | '+' | '=') => {
                let Some(s) = current else {
                    return AppEffect::None;
                };
                let delta = if matches!(key.code, KeyCode::Left | KeyCode::Char('-')) {
                    -STEP
                } else {
                    STEP
                };
                let (id, is_default) = (s.id, s.default);
                self.background(Some(Box::new(move |b| match b.change(id, delta) {
                    Ok(v) => (v.message(), is_default),
                    Err(e) => (format!("volume: {e}"), false),
                })));
            }
            KeyCode::Char('m') => {
                let Some(s) = current else {
                    return AppEffect::None;
                };
                let (id, is_default) = (s.id, s.default);
                self.background(Some(Box::new(move |b| match b.toggle_mute(id) {
                    Ok(v) => (v.message(), is_default),
                    Err(e) => (format!("mute: {e}"), false),
                })));
            }
            _ => return AppEffect::None,
        }
        AppEffect::Redraw
    }

    fn render(&mut self, area: TuiRect, buf: &mut Buffer, theme: &Theme) {
        if area.height < 4 || area.width < 30 {
            return;
        }
        let c = &theme.colors;
        let Ok(s) = self.shared.lock() else {
            return;
        };
        let width = usize::from(area.width.saturating_sub(2));
        let mut y = area.y;
        let bottom = area.y + area.height - 2;
        let line = |buf: &mut Buffer, y: u16, x: u16, text: &str, style: Style| {
            if y < bottom {
                buf.set_stringn(
                    area.x + x,
                    y,
                    text,
                    width.saturating_sub(usize::from(x)),
                    tui_style(style),
                );
            }
        };
        match (&s.sinks, s.loaded) {
            (_, false) => line(buf, y, 1, "Reading the sound…", Style::new().fg(c.muted)),
            (None, true) => line(
                buf,
                y,
                1,
                "Sound is managed by this system's own settings",
                Style::new().fg(c.muted),
            ),
            (Some(Err(e)), true) => {
                line(buf, y, 1, &format!("Sound: {e}"), Style::new().fg(c.error));
            }
            (Some(Ok(sinks)), true) => {
                line(
                    buf,
                    y,
                    1,
                    "Outputs",
                    Style::new().fg(c.fg).attrs(Attrs::BOLD),
                );
                y += 2;
                if sinks.is_empty() {
                    line(
                        buf,
                        y,
                        3,
                        "No outputs: plug in speakers or connect a headset",
                        Style::new().fg(c.muted),
                    );
                }
                let name_w = usize::from(area.width.saturating_sub(30)).clamp(10, 36);
                let bar_x = 5 + name_w as u16;
                for (i, sink) in sinks.iter().enumerate() {
                    let selected = i == self.selected;
                    let paint = |colour| {
                        if selected {
                            c.selected(Style::new())
                        } else {
                            Style::new().fg(colour)
                        }
                    };
                    if selected && y < bottom {
                        buf.set_style(
                            TuiRect::new(area.x, y, area.width, 1),
                            tui_style(paint(c.fg)),
                        );
                    }
                    line(
                        buf,
                        y,
                        1,
                        if sink.default { "●" } else { " " },
                        paint(c.accent),
                    );
                    let name: String = sink.name.chars().take(name_w).collect();
                    line(buf, y, 3, &name, paint(c.fg));
                    if let Some(v) = sink.volume {
                        let level = if v.muted { 0.0 } else { v.level };
                        line(buf, y, bar_x, &bar(level, 16), paint(c.accent));
                        let text = if v.muted {
                            "muted".to_owned()
                        } else {
                            format!("{}%", v.percent())
                        };
                        line(buf, y, bar_x + 17, &text, paint(c.muted));
                    }
                    y += 1;
                }
            }
        }
        buf.set_stringn(
            area.x + 1,
            bottom,
            s.message.as_deref().unwrap_or(""),
            width,
            tui_style(Style::new().fg(c.muted)),
        );
        buf.set_stringn(
            area.x + 1,
            bottom + 1,
            "↑↓ choose · Enter play here · ←→ volume · m mute · Esc close",
            width,
            tui_style(Style::new().fg(c.muted)),
        );
    }

    fn set_waker(&mut self, waker: AppWaker) {
        if let Ok(mut w) = self.waker.lock() {
            *w = Some(waker);
        }
        self.background(None);
        self.next = Some(Instant::now() + INTERVAL);
    }

    fn deadline(&self) -> Option<Instant> {
        let changed = self.shared.lock().is_ok_and(|s| s.status_changed);
        if changed {
            Some(Instant::now())
        } else {
            self.next
        }
    }

    fn on_timer(&mut self, now: Instant) -> AppEffect {
        let changed = self
            .shared
            .lock()
            .map(|mut s| std::mem::take(&mut s.status_changed))
            .unwrap_or(false);
        if self.next.is_some_and(|n| n <= now) {
            self.background(None);
            self.next = Some(now + INTERVAL);
        }
        if changed {
            AppEffect::Command("sound.refresh".into())
        } else {
            AppEffect::Redraw
        }
    }
}

/// This machine: PipeWire through `wpctl`.
struct System;

impl Backend for System {
    fn sinks(&self) -> Option<Result<Vec<Sink>, String>> {
        match termos_hw::sound::sinks() {
            Ok(s) => Some(Ok(s)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => Some(Err(e.to_string())),
        }
    }

    fn set_default(&self, id: u32) -> Result<(), String> {
        termos_hw::sound::set_default(id).map_err(|e| e.to_string())
    }

    fn change(&self, id: u32, delta: i8) -> Result<Volume, String> {
        termos_hw::sound::change_volume_of(&id.to_string(), delta).map_err(|e| e.to_string())
    }

    fn toggle_mute(&self, id: u32) -> Result<Volume, String> {
        termos_hw::sound::toggle_mute_of(&id.to_string()).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termos_core::input::Mods;

    /// Two outputs; changes are logged and applied.
    struct Fake(Mutex<(Vec<Sink>, Vec<String>)>);

    impl Fake {
        fn new() -> Arc<Self> {
            let sink = |id, name: &str, default, level, muted| Sink {
                id,
                name: name.into(),
                default,
                volume: Some(Volume { level, muted }),
            };
            Arc::new(Self(Mutex::new((
                vec![
                    sink(50, "Built-in Audio Analog Stereo", true, 0.4, false),
                    sink(51, "WH-1000XM4", false, 0.8, true),
                ],
                Vec::new(),
            ))))
        }

        fn log(&self) -> Vec<String> {
            self.0.lock().expect("fake").1.clone()
        }
    }

    impl Backend for Fake {
        fn sinks(&self) -> Option<Result<Vec<Sink>, String>> {
            Some(Ok(self.0.lock().expect("fake").0.clone()))
        }

        fn set_default(&self, id: u32) -> Result<(), String> {
            let mut s = self.0.lock().expect("fake");
            for k in &mut s.0 {
                k.default = k.id == id;
            }
            s.1.push(format!("default {id}"));
            Ok(())
        }

        fn change(&self, id: u32, delta: i8) -> Result<Volume, String> {
            let mut s = self.0.lock().expect("fake");
            s.1.push(format!("volume {id} {delta:+}"));
            let k =
                s.0.iter_mut()
                    .find(|k| k.id == id)
                    .ok_or("no such output")?;
            let v = k.volume.as_mut().ok_or("no volume")?;
            v.level = (v.level + f32::from(delta) / 100.0).clamp(0.0, 1.0);
            Ok(*v)
        }

        fn toggle_mute(&self, id: u32) -> Result<Volume, String> {
            let mut s = self.0.lock().expect("fake");
            s.1.push(format!("mute {id}"));
            let k =
                s.0.iter_mut()
                    .find(|k| k.id == id)
                    .ok_or("no such output")?;
            let v = k.volume.as_mut().ok_or("no volume")?;
            v.muted = !v.muted;
            Ok(*v)
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, Mods::empty())
    }

    /// Waits for the page's thread to finish a job.
    fn settle(page: &SoundPage, jobs: usize, fake: &Fake) {
        for _ in 0..200 {
            let done = page
                .shared
                .lock()
                .is_ok_and(|s| s.message.as_deref().is_some_and(|m| !m.ends_with('…')));
            if fake.log().len() >= jobs && done {
                // The thread reads the outputs again after the job.
                std::thread::sleep(Duration::from_millis(20));
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the job did not finish: {:?}", fake.log());
    }

    #[test]
    fn the_page_shows_the_outputs() {
        let fake = Fake::new();
        let mut page = SoundPage::with_backend(fake, None);
        assert!(crate::testing::render_text(&mut page, 72, 8).contains("Reading the sound"));
        page.refresh_now();
        insta::assert_snapshot!("sound-page", crate::testing::render_text(&mut page, 72, 8));
    }

    #[test]
    fn volumes_change_and_the_choice_is_saved() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = dir.path().join("config.toml");
        let fake = Fake::new();
        let mut page =
            SoundPage::with_backend(Arc::clone(&fake) as Arc<dyn Backend>, Some(config.clone()));
        page.refresh_now();
        // The default output, louder: the status line is read again.
        page.handle_key(key(KeyCode::Right));
        settle(&page, 1, &fake);
        assert_eq!(fake.log(), ["volume 50 +5"]);
        assert!(page.deadline().is_some_and(|d| d <= Instant::now()));
        assert_eq!(
            page.on_timer(Instant::now()),
            AppEffect::Command("sound.refresh".into())
        );
        assert_ne!(
            page.on_timer(Instant::now()),
            AppEffect::Command("sound.refresh".into()),
            "once"
        );
        // The headset: unmuted, then chosen and saved.
        page.handle_key(key(KeyCode::Down));
        page.handle_key(key(KeyCode::Char('m')));
        settle(&page, 2, &fake);
        assert!(
            !page.shared.lock().expect("s").status_changed,
            "not the default"
        );
        page.handle_key(key(KeyCode::Enter));
        settle(&page, 3, &fake);
        assert_eq!(fake.log()[1..], ["mute 51", "default 51"]);
        let saved = std::fs::read_to_string(&config).expect("saved");
        assert!(saved.contains("device = \"WH-1000XM4\""), "{saved}");
        let shown = crate::testing::render_text(&mut page, 72, 8);
        assert!(shown.contains("● WH-1000XM4"), "{shown}");
        assert!(shown.contains("WH-1000XM4 plays now"), "{shown}");
    }

    #[test]
    fn bars() {
        assert_eq!(bar(0.0, 4), "░░░░");
        assert_eq!(bar(0.5, 4), "██░░");
        assert_eq!(bar(1.7, 4), "████");
    }
}
