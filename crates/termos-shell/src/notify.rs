// SPDX-License-Identifier: GPL-3.0-or-later
//! Notifications: a toast in the status line (like tmux `display-message`)
//! plus a history shown by the notification center.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Importance of a notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

/// One notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// Local wall-clock time, `HH:MM:SS`.
    pub time: String,
    pub level: Level,
    pub text: String,
}

/// History shared with the notification-center app.
pub type SharedHistory = Arc<Mutex<VecDeque<Notice>>>;

const HISTORY_LIMIT: usize = 200;

/// The toast plus history.
#[derive(Debug, Default)]
pub struct Notifications {
    history: SharedHistory,
    toast: Option<(String, Level, Instant)>,
}

fn local_time() -> String {
    jiff::Zoned::now().strftime("%H:%M:%S").to_string()
}

impl Notifications {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a notification and shows it as a toast until `now` + a few
    /// seconds (longer for errors).
    pub fn push(&mut self, level: Level, text: impl Into<String>, now: Instant) {
        let text = text.into();
        let secs = match level {
            Level::Info => 3,
            Level::Warn => 5,
            Level::Error => 8,
        };
        self.toast = Some((text.clone(), level, now + Duration::from_secs(secs)));
        if let Ok(mut h) = self.history.lock() {
            h.push_back(Notice {
                time: local_time(),
                level,
                text,
            });
            while h.len() > HISTORY_LIMIT {
                h.pop_front();
            }
        }
    }

    /// The current toast, if any.
    pub fn toast(&self) -> Option<(&str, Level)> {
        self.toast.as_ref().map(|(t, l, _)| (t.as_str(), *l))
    }

    /// When the toast must disappear.
    pub fn deadline(&self) -> Option<Instant> {
        self.toast.as_ref().map(|(_, _, until)| *until)
    }

    /// Drops an expired toast; returns whether something changed.
    pub fn expire(&mut self, now: Instant) -> bool {
        if self.deadline().is_some_and(|d| d <= now) {
            self.toast = None;
            return true;
        }
        false
    }

    pub fn history(&self) -> SharedHistory {
        Arc::clone(&self.history)
    }

    pub fn len(&self) -> usize {
        self.history.lock().map_or(0, |h| h.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toasts_expire_and_history_is_bounded() {
        let mut n = Notifications::new();
        let t0 = Instant::now();
        n.push(Level::Info, "hello", t0);
        assert_eq!(n.toast(), Some(("hello", Level::Info)));
        assert!(!n.expire(t0 + Duration::from_secs(1)));
        assert!(n.expire(t0 + Duration::from_secs(4)));
        assert_eq!(n.toast(), None);
        for i in 0..250 {
            n.push(Level::Warn, format!("n{i}"), t0);
        }
        assert_eq!(n.len(), HISTORY_LIMIT);
        let h = n.history();
        let first = h.lock().expect("lock").front().map(|x| x.text.clone());
        assert_eq!(first.as_deref(), Some("n50"));
    }
}
