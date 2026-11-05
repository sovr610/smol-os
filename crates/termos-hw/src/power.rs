// SPDX-License-Identifier: GPL-3.0-or-later
//! Batteries, mains power and the screen's backlight (M13d, D-124).
//!
//! On Linux everything is read from sysfs below a directory the caller
//! names (`/sys/class/power_supply`, `/sys/class/backlight`), so tests use
//! fake trees. On macOS and Windows the `starship-battery` crate gives the
//! same figures; the backlight is the host system's there.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// What a battery is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Charge {
    Charging,
    Discharging,
    Full,
    /// Plugged in but not charging (a charge limit, or the system says
    /// "Not charging").
    Idle,
    Unknown,
}

/// One battery.
#[derive(Debug, Clone, PartialEq)]
pub struct Battery {
    pub name: String,
    /// 0 to 100.
    pub percent: f32,
    pub charge: Charge,
    /// Energy now and when full, in watt-hours, when the system says.
    pub energy: Option<(f32, f32)>,
    /// Watts going in or out, when the system says.
    pub watts: Option<f32>,
}

/// The machine's power: its batteries (none on a desktop) and whether it
/// is on mains power.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Power {
    pub batteries: Vec<Battery>,
    /// Mains power, when the system has a `Mains` supply to ask.
    pub ac: Option<bool>,
}

impl Power {
    /// Whether the machine runs on its batteries now.
    pub fn on_battery(&self) -> bool {
        match self.ac {
            Some(ac) => !ac && !self.batteries.is_empty(),
            None => self
                .batteries
                .iter()
                .any(|b| b.charge == Charge::Discharging),
        }
    }

    /// The charge of all batteries together, 0 to 100: weighted by their
    /// size when known, else the mean.
    pub fn percent(&self) -> Option<f32> {
        if self.batteries.is_empty() {
            return None;
        }
        let sized: Vec<(f32, f32)> = self.batteries.iter().filter_map(|b| b.energy).collect();
        if sized.len() == self.batteries.len() {
            let (now, full) = sized
                .iter()
                .fold((0.0, 0.0), |(n, f), (bn, bf)| (n + bn, f + bf));
            if full > 0.0 {
                return Some((now / full * 100.0).clamp(0.0, 100.0));
            }
        }
        let sum: f32 = self.batteries.iter().map(|b| b.percent).sum();
        Some(sum / self.batteries.len() as f32)
    }

    /// Whether any battery charges.
    pub fn charging(&self) -> bool {
        self.batteries.iter().any(|b| b.charge == Charge::Charging)
    }

    /// Time until empty (discharging) or full (charging), from the energy
    /// and the power drawn, when the system gives both.
    pub fn time_left(&self) -> Option<Duration> {
        let mut now = 0.0f32;
        let mut full = 0.0f32;
        let mut watts = 0.0f32;
        for b in &self.batteries {
            let (n, f) = b.energy?;
            now += n;
            full += f;
            watts += b.watts?.abs();
        }
        if watts < 0.05 {
            return None;
        }
        let hours = if self.charging() {
            (full - now).max(0.0) / watts
        } else if self.on_battery() {
            now / watts
        } else {
            return None;
        };
        // Hours to seconds; past a day it is not worth saying.
        (hours < 24.0).then(|| Duration::from_secs((hours * 3600.0) as u64))
    }
}

fn read(dir: &Path, name: &str) -> Option<String> {
    std::fs::read_to_string(dir.join(name))
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

fn number(dir: &Path, name: &str) -> Option<f64> {
    read(dir, name)?.parse().ok()
}

/// One sysfs battery, `None` when it is absent or a device's (a mouse's
/// or a headset's battery has `scope` `Device`).
fn battery(dir: &Path, name: String) -> Option<Battery> {
    if read(dir, "present").is_some_and(|p| p == "0")
        || read(dir, "scope").is_some_and(|s| s.eq_ignore_ascii_case("device"))
    {
        return None;
    }
    // Energy in µWh and power in µW; or charge in µAh, current in µA and
    // the voltage in µV, which make the same.
    let volts = number(dir, "voltage_now").or_else(|| number(dir, "voltage_min_design"));
    let energy = match (number(dir, "energy_now"), number(dir, "energy_full")) {
        (Some(n), Some(f)) => Some((n / 1e6, f / 1e6)),
        _ => match (number(dir, "charge_now"), number(dir, "charge_full"), volts) {
            (Some(n), Some(f), Some(v)) => Some((n * v / 1e12, f * v / 1e12)),
            _ => None,
        },
    };
    let watts = number(dir, "power_now").map(|p| p / 1e6).or_else(|| {
        let a = number(dir, "current_now")?;
        Some(a * volts? / 1e12)
    });
    let percent = number(dir, "capacity").or_else(|| {
        let (n, f) = energy?;
        (f > 0.0).then(|| n / f * 100.0)
    })?;
    let charge = match read(dir, "status").as_deref() {
        Some("Charging") => Charge::Charging,
        Some("Discharging") => Charge::Discharging,
        Some("Full") => Charge::Full,
        Some("Not charging") => Charge::Idle,
        _ => Charge::Unknown,
    };
    Some(Battery {
        name,
        percent: percent.clamp(0.0, 100.0) as f32,
        charge,
        energy: energy.map(|(n, f)| (n as f32, f as f32)),
        watts: watts.map(|w| w as f32),
    })
}

/// The power supplies under `dir` (`/sys/class/power_supply`).
pub fn read_sysfs(dir: &Path) -> Power {
    let mut power = Power::default();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return power;
    };
    let mut entries: Vec<(String, PathBuf)> = entries
        .flatten()
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .collect();
    entries.sort();
    for (name, path) in entries {
        match read(&path, "type").as_deref() {
            Some("Battery") => {
                if let Some(b) = battery(&path, name) {
                    power.batteries.push(b);
                }
            }
            // Mains power: a laptop's charger. USB supplies (USB-C chargers
            // on some laptops) count when online.
            Some("Mains" | "USB") => {
                let online = read(&path, "online").is_some_and(|o| o == "1");
                power.ac = Some(power.ac.unwrap_or(false) || online);
            }
            _ => {}
        }
    }
    power
}

/// The machine's power now.
#[cfg(target_os = "linux")]
pub fn read() -> Power {
    read_sysfs(Path::new("/sys/class/power_supply"))
}

/// The machine's power now, from the system's battery API.
#[cfg(any(target_os = "macos", windows))]
pub fn read() -> Power {
    use starship_battery::units::energy::watt_hour;
    use starship_battery::units::power::watt;
    use starship_battery::units::ratio::percent;
    let mut power = Power::default();
    let Ok(manager) = starship_battery::Manager::new() else {
        return power;
    };
    let Ok(batteries) = manager.batteries() else {
        return power;
    };
    for (i, b) in batteries.flatten().enumerate() {
        let charge = match b.state() {
            starship_battery::State::Charging => Charge::Charging,
            starship_battery::State::Discharging => Charge::Discharging,
            starship_battery::State::Full => Charge::Full,
            _ => Charge::Unknown,
        };
        power.batteries.push(Battery {
            name: format!("BAT{i}"),
            percent: b.state_of_charge().get::<percent>(),
            charge,
            energy: Some((
                b.energy().get::<watt_hour>(),
                b.energy_full().get::<watt_hour>(),
            )),
            watts: Some(b.energy_rate().get::<watt>()),
        });
    }
    power
}

/// Elsewhere there is nothing to ask.
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub fn read() -> Power {
    Power::default()
}

/// A short form for the status line: `▮ 84%`, `⚡ 62%` while charging;
/// `None` without a battery.
pub fn status_text(p: &Power) -> Option<String> {
    let pct = p.percent()?.round() as u32;
    let mark = if p.charging() { "⚡" } else { "▮" };
    Some(format!("{mark} {pct}%"))
}

/// `1:05` for a time left.
pub fn hours_minutes(d: Duration) -> String {
    let m = d.as_secs() / 60;
    format!("{}:{:02}", m / 60, m % 60)
}

/// The backlight to change under `dir` (`/sys/class/backlight`): the
/// firmware's first, then the platform's, then a raw one, as desktops pick.
pub fn backlight(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut all: Vec<(u8, String, PathBuf)> = entries
        .flatten()
        .map(|e| {
            let path = e.path();
            let rank = match read(&path, "type").as_deref() {
                Some("firmware") => 0,
                Some("platform") => 1,
                _ => 2,
            };
            (rank, e.file_name().to_string_lossy().into_owned(), path)
        })
        .filter(|(_, _, p)| number(p, "max_brightness").is_some_and(|m| m > 0.0))
        .collect();
    all.sort();
    all.into_iter().next().map(|(_, _, p)| p)
}

/// The backlight's brightness under `dir`, in percent.
pub fn brightness(dir: &Path) -> Option<u8> {
    let b = backlight(dir)?;
    let max = number(&b, "max_brightness")?;
    let now = number(&b, "brightness")?;
    Some((now / max * 100.0).round().clamp(0.0, 100.0) as u8)
}

/// Sets the backlight under `dir` to `percent` (at least one step, so the
/// screen never goes dark by a key); its new brightness. The image lets the
/// group `video` write it (a udev rule), so no root is needed.
pub fn set_brightness(dir: &Path, percent: u8) -> std::io::Result<u8> {
    let b = backlight(dir).ok_or_else(|| std::io::Error::other("this screen has no backlight"))?;
    let max = number(&b, "max_brightness").unwrap_or(0.0);
    let raw = ((f64::from(percent.min(100)) / 100.0) * max)
        .round()
        .max(1.0) as u64;
    std::fs::write(b.join("brightness"), raw.to_string())?;
    Ok((raw as f64 / max * 100.0).round() as u8)
}

/// Changes the backlight under `dir` by `delta` percent.
pub fn change_brightness(dir: &Path, delta: i8) -> std::io::Result<u8> {
    let now =
        brightness(dir).ok_or_else(|| std::io::Error::other("this screen has no backlight"))?;
    let next = (i16::from(now) + i16::from(delta)).clamp(1, 100) as u8;
    set_brightness(dir, next)
}

/// The system's backlights.
pub const BACKLIGHT_DIR: &str = "/sys/class/backlight";

#[cfg(test)]
mod tests {
    use super::*;

    fn supply(root: &Path, name: &str, files: &[(&str, &str)]) {
        let d = root.join(name);
        std::fs::create_dir_all(&d).expect("mkdir");
        for (f, v) in files {
            std::fs::write(d.join(f), format!("{v}\n")).expect("write");
        }
    }

    #[test]
    fn a_laptop_discharging() {
        let dir = tempfile::tempdir().expect("dir");
        supply(dir.path(), "AC", &[("type", "Mains"), ("online", "0")]);
        supply(
            dir.path(),
            "BAT0",
            &[
                ("type", "Battery"),
                ("present", "1"),
                ("status", "Discharging"),
                ("capacity", "84"),
                ("energy_now", "42000000"),
                ("energy_full", "50000000"),
                ("power_now", "10500000"),
            ],
        );
        // A mouse's battery is not the machine's.
        supply(
            dir.path(),
            "hidpp_battery_0",
            &[("type", "Battery"), ("scope", "Device"), ("capacity", "5")],
        );
        let p = read_sysfs(dir.path());
        assert_eq!(p.batteries.len(), 1);
        assert!(p.on_battery());
        assert!(!p.charging());
        assert_eq!(p.percent(), Some(84.0));
        // 42 Wh at 10.5 W: four hours.
        assert_eq!(p.time_left(), Some(Duration::from_secs(4 * 3600)));
        assert_eq!(status_text(&p).as_deref(), Some("▮ 84%"));
        assert_eq!(hours_minutes(Duration::from_secs(3900)), "1:05");
    }

    #[test]
    fn charging_in_charge_units() {
        let dir = tempfile::tempdir().expect("dir");
        supply(dir.path(), "ADP1", &[("type", "Mains"), ("online", "1")]);
        supply(
            dir.path(),
            "BAT1",
            &[
                ("type", "Battery"),
                ("status", "Charging"),
                ("charge_now", "2000000"),
                ("charge_full", "4000000"),
                ("current_now", "1000000"),
                ("voltage_now", "12000000"),
            ],
        );
        let p = read_sysfs(dir.path());
        assert!(!p.on_battery());
        assert!(p.charging());
        // No capacity file: from the charge.
        assert_eq!(p.percent(), Some(50.0));
        // 24 Wh to go at 12 W: two hours.
        assert_eq!(p.time_left(), Some(Duration::from_secs(2 * 3600)));
        assert_eq!(status_text(&p).as_deref(), Some("⚡ 50%"));
    }

    #[test]
    fn two_batteries_and_none() {
        let dir = tempfile::tempdir().expect("dir");
        for (name, now, full) in [
            ("BAT0", "10000000", "20000000"),
            ("BAT1", "30000000", "60000000"),
        ] {
            supply(
                dir.path(),
                name,
                &[
                    ("type", "Battery"),
                    ("status", "Discharging"),
                    ("energy_now", now),
                    ("energy_full", full),
                    ("capacity", "50"),
                ],
            );
        }
        let p = read_sysfs(dir.path());
        assert_eq!(p.batteries.len(), 2);
        assert_eq!(p.percent(), Some(50.0));
        assert!(p.on_battery(), "no mains supply: discharging says so");
        assert_eq!(p.time_left(), None, "no power figures");
        let empty = tempfile::tempdir().expect("dir");
        let none = read_sysfs(empty.path());
        assert_eq!(none.percent(), None);
        assert!(!none.on_battery());
        assert_eq!(status_text(&none), None);
        assert_eq!(read_sysfs(&empty.path().join("missing")), Power::default());
    }

    #[test]
    fn the_backlight_moves_in_percent() {
        let dir = tempfile::tempdir().expect("dir");
        supply(
            dir.path(),
            "acpi_video0",
            &[
                ("type", "firmware"),
                ("max_brightness", "15"),
                ("brightness", "15"),
            ],
        );
        supply(
            dir.path(),
            "intel_backlight",
            &[
                ("type", "raw"),
                ("max_brightness", "1000"),
                ("brightness", "500"),
            ],
        );
        assert_eq!(backlight(dir.path()), Some(dir.path().join("acpi_video0")));
        assert_eq!(brightness(dir.path()), Some(100));
        assert_eq!(change_brightness(dir.path(), -50).expect("set"), 53);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("acpi_video0/brightness")).expect("read"),
            "8"
        );
        // Never dark by a key.
        assert_eq!(set_brightness(dir.path(), 0).expect("set"), 7);
        let none = tempfile::tempdir().expect("dir");
        assert!(change_brightness(none.path(), 10).is_err());
    }
}
