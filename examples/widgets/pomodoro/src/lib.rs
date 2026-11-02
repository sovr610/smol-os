// SPDX-License-Identifier: GPL-3.0-or-later
//! A sample TermOS status-line widget: a pomodoro timer.
//!
//! Click it to start; it counts down (ticking once a second only while it
//! runs), and says when the time is up. Click again to pause, and once
//! more after it ends to reset. Settings: `minutes` (default 25).
//!
//! `spin = "true"` makes `init` loop forever: TermOS's own tests use it to
//! check that a runaway widget is stopped.

use std::cell::RefCell;
use std::time::{Duration, Instant};

wit_bindgen::generate!({
    path: "../../../crates/termos-plugin/wit",
    world: "widget",
});

use termos::widget::types::{Color, Style};

enum Phase {
    Ready,
    Running { until: Instant },
    Paused { left: Duration },
    Done,
}

struct State {
    length: Duration,
    phase: Phase,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State {
        length: Duration::from_secs(25 * 60),
        phase: Phase::Ready,
    });
}

fn plain() -> Style {
    Style {
        fg: Color::Default,
        bg: Color::Default,
        bold: false,
        italic: false,
        underline: false,
        dim: false,
    }
}

fn clock(d: Duration) -> String {
    let s = d.as_secs() + u64::from(d.subsec_nanos() > 0);
    format!("{}:{:02}", s / 60, s % 60)
}

/// The next tick: at the next whole second left, while running.
fn ticking(state: &State) -> Option<u32> {
    match state.phase {
        Phase::Running { until } => {
            let left = until.saturating_duration_since(Instant::now());
            let ms = left.subsec_millis();
            Some(if ms == 0 { 1000 } else { ms })
        }
        _ => None,
    }
}

struct Pomodoro;

impl Guest for Pomodoro {
    fn init(settings: Vec<(String, String)>) -> Result<Update, String> {
        let mut minutes = 25u64;
        for (k, v) in settings {
            match k.as_str() {
                "minutes" => {
                    minutes = v
                        .parse()
                        .ok()
                        .filter(|m| (1..=600).contains(m))
                        .ok_or_else(|| format!("minutes: {v:?} is not 1 to 600"))?;
                }
                "spin" if v == "true" => {
                    let mut n = 0u64;
                    loop {
                        n = std::hint::black_box(n.wrapping_add(1));
                    }
                }
                _ => return Err(format!("unknown setting {k:?}")),
            }
        }
        STATE.with_borrow_mut(|s| s.length = Duration::from_secs(minutes * 60));
        Ok(Update {
            redraw: true,
            next_tick_ms: None,
        })
    }

    fn render(width: u16) -> Vec<Span> {
        STATE.with_borrow(|s| {
            let (text, bold) = match s.phase {
                Phase::Ready => (format!("🍅 {}", clock(s.length)), false),
                Phase::Running { until } => (
                    format!("🍅 {}", clock(until.saturating_duration_since(Instant::now()))),
                    true,
                ),
                Phase::Paused { left } => (format!("🍅 {} ‖", clock(left)), false),
                Phase::Done => ("🍅 done!".to_owned(), true),
            };
            let text = if usize::from(width) < text.chars().count() + 1 {
                "🍅".to_owned()
            } else {
                text
            };
            vec![Span {
                text,
                style: Style {
                    bold,
                    ..plain()
                },
            }]
        })
    }

    fn tick() -> Update {
        STATE.with_borrow_mut(|s| {
            if let Phase::Running { until } = s.phase
                && until <= Instant::now()
            {
                s.phase = Phase::Done;
            }
            Update {
                redraw: true,
                next_tick_ms: ticking(s),
            }
        })
    }

    fn on_event(event: Event) -> Update {
        let Event::Click(_) = event;
        STATE.with_borrow_mut(|s| {
            s.phase = match s.phase {
                Phase::Ready => Phase::Running {
                    until: Instant::now() + s.length,
                },
                Phase::Running { until } => Phase::Paused {
                    left: until.saturating_duration_since(Instant::now()),
                },
                Phase::Paused { left } => Phase::Running {
                    until: Instant::now() + left,
                },
                Phase::Done => Phase::Ready,
            };
            Update {
                redraw: true,
                next_tick_ms: ticking(s),
            }
        })
    }
}

export!(Pomodoro);
