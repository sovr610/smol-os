// SPDX-License-Identifier: GPL-3.0-or-later
//! Power on the desktop (M13d, D-123, D-124): locking, suspending,
//! restarting and shutting down a TermOS image; brightness; the battery
//! in the status line and its low-battery steps; USB drives coming and
//! going.
//!
//! The console locks and suspends (`ServerMsg::Lock`, `ServerMsg::Suspend`),
//! so the lock is up before the machine sleeps. In a terminal the host
//! system has its own lock and power menu: the commands say so instead.

use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use termos_core::proto::ServerMsg;
use termos_hw::power::Power;

use super::{Desktop, SessionRequest};
use crate::commands::Command;
use crate::menu::Entry;
use crate::notify::Level;
use crate::prompt::{Confirmed, PowerOff, Prompt};

/// Whether this server runs on a TermOS image (`/etc/termos/board`).
pub(crate) fn on_image() -> bool {
    static IMAGE: OnceLock<bool> = OnceLock::new();
    *IMAGE.get_or_init(|| Path::new("/etc/termos/board").exists())
}

/// How far the low-battery steps have gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, PartialOrd, Ord)]
pub(crate) enum Low {
    #[default]
    Fine,
    /// The toast at 20%.
    Warned,
    /// The system notification at 10%.
    Notified,
    /// The countdown to suspend at `[power] critical`.
    Critical,
}

/// What a battery reading asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Alert {
    Toast(String),
    Notify(String),
    /// Suspend after the countdown, unless power comes back.
    Countdown(String),
}

/// The next low-battery step for a reading: each step once, on battery
/// only; plugging in (or charging back above 25%) starts again.
pub(crate) fn low_battery(prev: Low, power: &Power, critical: u8) -> (Low, Option<Alert>) {
    let Some(pct) = power.percent() else {
        return (Low::Fine, None);
    };
    if !power.on_battery() || pct > 25.0 {
        return (Low::Fine, None);
    }
    let pct_text = format!("{}%", pct.round() as u32);
    if critical > 0 && pct <= f32::from(critical) && prev < Low::Critical {
        return (
            Low::Critical,
            Some(Alert::Countdown(format!(
                "battery at {pct_text}: suspending in 30 s (plug in to stop it)"
            ))),
        );
    }
    if pct <= 10.0 && prev < Low::Notified {
        return (
            Low::Notified,
            Some(Alert::Notify(format!("battery at {pct_text}: plug in now"))),
        );
    }
    if pct <= 20.0 && prev < Low::Warned {
        return (
            Low::Warned,
            Some(Alert::Toast(format!("battery at {pct_text}: plug in soon"))),
        );
    }
    (prev, None)
}

/// How long the critical battery's countdown lasts.
pub(crate) const COUNTDOWN: Duration = Duration::from_secs(30);

/// How often a battery is read: once a minute, with the clock.
const BATTERY_EVERY: Duration = Duration::from_secs(60);

impl Desktop {
    /// Whether one of the clients is the image's console (which locks and
    /// suspends).
    fn has_console(&self) -> bool {
        self.clients
            .values()
            .chain(self.current.map(|_| &self.ui))
            .any(|u| u.caps.term == super::find::CONSOLE_TERM)
    }

    /// `session.lock`.
    pub(super) fn lock_screen(&mut self) {
        if self.has_console() {
            self.outbox.push((None, ServerMsg::Lock));
        } else {
            self.post(
                Level::Info,
                "TermOS locks only on its own images: lock this computer as its system does",
            );
        }
    }

    /// `system.suspend`.
    pub(super) fn suspend(&mut self) {
        if !on_image() {
            self.post(
                Level::Info,
                "TermOS suspends only its own images: suspend this computer as its system does",
            );
        } else if self.has_console() {
            self.outbox.push((None, ServerMsg::Suspend));
        } else {
            // No screen to lock: suspend from here.
            run_system(&["suspend"]);
        }
    }

    /// `system.restart`, `system.power-off`: asks first.
    pub(super) fn ask_power_off(&mut self, what: PowerOff) {
        if !on_image() {
            self.post(
                Level::Info,
                "TermOS restarts and shuts down only its own images: use this computer's system",
            );
            return;
        }
        let question = match what {
            PowerOff::Restart => "restart the computer (sessions are saved first)",
            PowerOff::PowerOff => "shut the computer down (sessions are saved first)",
        };
        self.ui.prompt = Some(Prompt::confirm(Confirmed::Power(what), question));
    }

    /// The answer was yes: the server saves every session, then restarts
    /// or shuts down.
    pub(super) fn power_off(&mut self, what: PowerOff) {
        self.requests.push(SessionRequest::PowerOff(what));
    }

    /// `display.brightness ±N`.
    pub(super) fn change_brightness(&mut self, delta: i8) {
        if !on_image() {
            self.post(Level::Info, "the screen's brightness is the host system's");
            return;
        }
        match termos_hw::power::change_brightness(Path::new(termos_hw::power::BACKLIGHT_DIR), delta)
        {
            Ok(p) => self.post(Level::Info, format!("brightness {p}%")),
            Err(e) => self.post(Level::Warn, format!("brightness: {e}")),
        }
    }

    /// `clock.zone ZONE`: the image's time zone (the status clock follows
    /// once the time library sees the new link).
    pub(super) fn set_time_zone(&mut self, zone: &str) {
        if !on_image() {
            self.post(Level::Info, "the time zone is the host system's");
            return;
        }
        run_system(&["set-time-zone", zone]);
        self.post(Level::Info, format!("time zone: {zone}"));
        self.dirty = true;
    }

    /// The Power submenu of the TermOS menu, on an image.
    pub(super) fn power_entries(&self) -> Vec<Entry> {
        vec![
            Entry::item("Lock", Command::SessionLock),
            Entry::item("Suspend", Command::SystemSuspend),
            Entry::item("Restart", Command::SystemRestart),
            Entry::item("Shut down", Command::SystemPowerOff),
            Entry::Separator,
            Entry::item("Quit this session", Command::Quit),
        ]
    }

    /// Reads the battery (if the machine has one) and takes the next
    /// low-battery step.
    pub(super) fn read_battery(&mut self, now: Instant) {
        if !self.battery_present {
            return;
        }
        let power = termos_hw::power::read();
        if power.batteries.is_empty() {
            self.battery_present = false;
            self.battery = None;
            self.battery_deadline = None;
            self.dirty = true;
            return;
        }
        self.battery_deadline = Some(now + BATTERY_EVERY);
        let (step, alert) = low_battery(self.low, &power, self.config.power.critical);
        if step == Low::Fine && self.low == Low::Critical {
            self.critical_at = None;
            self.post(Level::Info, "power is back: not suspending");
        }
        self.low = step;
        match alert {
            Some(Alert::Toast(t)) => self.post(Level::Warn, t),
            Some(Alert::Notify(t)) => {
                self.post(Level::Warn, t.clone());
                self.outbox.push((
                    None,
                    ServerMsg::Notify {
                        title: "Low battery".into(),
                        body: t,
                    },
                ));
            }
            Some(Alert::Countdown(t)) => {
                self.post(Level::Error, t);
                // Only an image suspends itself; a laptop's own system
                // does elsewhere.
                if on_image() {
                    self.critical_at = Some(now + COUNTDOWN);
                }
            }
            None => {}
        }
        let text = termos_hw::power::status_text(&power);
        if text != self.battery.as_ref().map(|b| b.0.clone()) {
            self.dirty = true;
        }
        self.battery = text.map(|t| {
            let low = !power.charging() && power.percent().is_some_and(|p| p <= 20.0);
            (t, low)
        });
    }

    /// The critical countdown ended: suspend unless power came back.
    pub(super) fn critical_timer(&mut self, now: Instant) {
        if self.critical_at.is_none_or(|t| t > now) {
            return;
        }
        self.critical_at = None;
        if termos_hw::power::read().on_battery() {
            self.suspend();
        }
    }

    /// A USB drive came or went (udev, D-124): a toast for one with a file
    /// system TermOS mounts.
    pub fn drive_event(&mut self, ev: &termos_hw::drives::Uevent) {
        if ev.action != "add" {
            return;
        }
        let Some(fs) = ev.properties.get("ID_FS_TYPE").filter(|f| !f.is_empty()) else {
            return;
        };
        if ev.properties.get("ID_BUS").map(String::as_str) != Some("usb")
            && !ev.properties.contains_key("ID_DRIVE_FLASH_SD")
        {
            return;
        }
        let name = ev
            .properties
            .get("ID_FS_LABEL")
            .filter(|l| !l.is_empty())
            .cloned()
            .or_else(|| ev.devname.clone())
            .unwrap_or_else(|| "a drive".into());
        self.post(
            Level::Info,
            format!("USB drive {name} ({fs}): open it in Files, under Drives"),
        );
    }
}

/// Runs `sudo -n termos system ARGS` without waiting (a thread reaps it).
pub(crate) fn run_system(args: &[&str]) {
    let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    let spawned = std::thread::Builder::new()
        .name("termos-system".into())
        .spawn(move || {
            match std::process::Command::new("sudo")
                .args(["-n", "/usr/bin/termos", "system"])
                .args(&args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
            {
                Ok(s) if s.success() => {}
                Ok(s) => tracing::warn!("`termos system {}` failed ({s})", args.join(" ")),
                Err(e) => tracing::warn!("cannot run sudo: {e}"),
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("cannot start a thread for `termos system`: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termos_hw::power::{Battery, Charge};

    fn at(pct: f32, charge: Charge) -> Power {
        Power {
            batteries: vec![Battery {
                name: "BAT0".into(),
                percent: pct,
                charge,
                energy: None,
                watts: None,
            }],
            ac: Some(charge != Charge::Discharging),
        }
    }

    #[test]
    fn low_battery_steps_once_each() {
        let mut low = Low::Fine;
        let mut alerts = Vec::new();
        // Draining a minute at a time.
        for pct in [30.0, 21.0, 20.0, 19.0, 15.0, 10.0, 9.0, 6.0, 5.0, 4.0] {
            let (next, alert) = low_battery(low, &at(pct, Charge::Discharging), 5);
            low = next;
            alerts.extend(alert);
        }
        assert_eq!(
            alerts,
            [
                Alert::Toast("battery at 20%: plug in soon".into()),
                Alert::Notify("battery at 10%: plug in now".into()),
                Alert::Countdown("battery at 5%: suspending in 30 s (plug in to stop it)".into()),
            ]
        );
        // Plugged in: the steps start again.
        let (low, alert) = low_battery(low, &at(4.0, Charge::Charging), 5);
        assert_eq!((low, alert), (Low::Fine, None));
        // A sudden drop goes straight to the worst step.
        let (low, alert) = low_battery(Low::Fine, &at(3.0, Charge::Discharging), 5);
        assert_eq!(low, Low::Critical);
        assert!(matches!(alert, Some(Alert::Countdown(_))));
        // `critical = 0`: never suspends.
        let (low, alert) = low_battery(Low::Notified, &at(3.0, Charge::Discharging), 0);
        assert_eq!((low, alert), (Low::Notified, None));
        // No battery, nothing.
        assert_eq!(
            low_battery(Low::Warned, &Power::default(), 5),
            (Low::Fine, None)
        );
    }
}
