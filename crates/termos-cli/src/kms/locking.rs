// SPDX-License-Identifier: GPL-3.0-or-later
//! The console's side of locking and power (M13d, D-123, D-124).
//!
//! - **Locking:** on the desktop's `session.lock` (`ServerMsg::Lock`), the
//!   lock key, `[lock] after-minutes` without input, the lid, and before
//!   every suspend. A file in the runtime folder says the screen is
//!   locked, so a console that restarts (it is respawned) comes back
//!   locked.
//! - **Root's part:** the password check, the VT switch lock and suspend
//!   are `sudo termos system` verbs, run on threads; their answers come
//!   back through a channel to the console's loop.
//! - **Idle suspend:** on battery, after `[power] suspend-after-minutes`
//!   without input. A machine with no battery never arms it; on mains
//!   power it looks again after the same time.
//! - **Brightness:** the keys write the backlight (the group `video` may).

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use secrecy::{ExposeSecret as _, SecretString};
use termos_core::config::{Config, LidAction, TimeFormat};
use termos_core::input::KeyEvent;
use termos_render::grid::Grid;
use termos_render::raster::Canvas;
use tokio::sync::mpsc::UnboundedSender;

use super::Console;
use super::damage::Damage;
use super::lock::{Lock, Mode, Step};

/// The binary sudo's rule names.
const SYSTEM_TERMOS: &str = "/usr/bin/termos";

/// What a root job was for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Job {
    /// Whether the user has a password; at first boot, to ask for one.
    HasPassword {
        first_boot: bool,
    },
    Check,
    Save,
    VtLock,
    Suspend,
}

/// A root job's answer.
#[derive(Debug)]
pub(super) struct Done {
    pub(super) job: Job,
    pub(super) ok: bool,
}

/// The console's power and lock settings.
#[derive(Debug, Clone)]
pub(super) struct Settings {
    pub(super) lock_after: Option<Duration>,
    pub(super) suspend_after: Option<Duration>,
    pub(super) lid: LidAction,
    pub(super) has_battery: bool,
    pub(super) time_format: TimeFormat,
    /// Present while the screen is locked (`$XDG_RUNTIME_DIR/termos-locked`).
    pub(super) locked_file: Option<PathBuf>,
    /// Present once the first boot asked for a password.
    pub(super) asked_file: PathBuf,
    /// A QEMU boot test runs (`/run/termos/boottest`): no idle lock and
    /// no first-boot question, which would hide the desktop it looks at.
    pub(super) boottest: bool,
    /// Started from the boot menu's "without the lock" entry
    /// (`termos.nolock` on the kernel's command line): the way back in when
    /// the password is lost. Nothing locks; `termos password` sets a new
    /// one.
    pub(super) nolock: bool,
}

impl Settings {
    pub(super) fn new(cfg: &Config, state_dir: &Path) -> Self {
        let minutes = |m: u32| (m > 0).then(|| Duration::from_secs(u64::from(m) * 60));
        Self {
            lock_after: minutes(cfg.lock.after_minutes),
            suspend_after: minutes(cfg.power.suspend_after_minutes),
            lid: cfg.power.lid,
            has_battery: !termos_hw::power::read().batteries.is_empty(),
            time_format: cfg.clock.time_format,
            locked_file: std::env::var_os("XDG_RUNTIME_DIR")
                .map(|d| PathBuf::from(d).join("termos-locked")),
            asked_file: state_dir.join("password-asked"),
            boottest: Path::new("/run/termos/boottest").exists(),
            nolock: std::fs::read_to_string("/proc/cmdline")
                .is_ok_and(|c| c.split_whitespace().any(|w| w == "termos.nolock")),
        }
    }
}

/// Runs `sudo -n termos system ARGS` on a thread, `input` on its standard
/// input, and sends whether it succeeded.
pub(super) fn root(
    tx: &UnboundedSender<Done>,
    job: Job,
    args: &[&str],
    input: Option<SecretString>,
) {
    let tx = tx.clone();
    let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    let spawned = std::thread::Builder::new()
        .name("termos-root".into())
        .spawn(move || {
            let ok = run_root(&args, input.as_ref());
            let _ = tx.send(Done { job, ok });
        });
    if let Err(e) = spawned {
        tracing::warn!("cannot start a thread for `termos system`: {e}");
    }
}

fn run_root(args: &[String], input: Option<&SecretString>) -> bool {
    let child = Command::new("sudo")
        .arg("-n")
        .arg(SYSTEM_TERMOS)
        .arg("system")
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("cannot run sudo: {e}");
            return false;
        }
    };
    if let (Some(text), Some(mut stdin)) = (input, child.stdin.take()) {
        use std::io::Write as _;
        let _ = stdin.write_all(text.expose_secret().as_bytes());
        let _ = stdin.write_all(b"\n");
    }
    match child.wait_with_output() {
        Ok(out) => {
            if !out.status.success() && args.first().is_some_and(|a| a != "check-password") {
                tracing::warn!(
                    "`termos system {}` failed: {}",
                    args.join(" "),
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            out.status.success()
        }
        Err(e) => {
            tracing::warn!("`termos system {}`: {e}", args.join(" "));
            false
        }
    }
}

impl Console {
    /// Locks the screen (nothing if it is already).
    pub(super) fn lock_now(&mut self) -> io::Result<()> {
        if matches!(self.lock.as_ref().map(|l| &l.mode), Some(Mode::Unlock)) {
            return Ok(());
        }
        if self.settings.nolock {
            tracing::warn!("not locking: started without the lock (termos.nolock)");
            return Ok(());
        }
        tracing::info!("locking the screen");
        self.lock = Some(Lock::new(Mode::Unlock));
        self.repeat = None;
        if let Some(f) = &self.settings.locked_file
            && let Err(e) = std::fs::write(f, b"")
        {
            tracing::warn!("cannot write {}: {e}", f.display());
        }
        root(
            &self.root_tx,
            Job::HasPassword { first_boot: false },
            &["has-password"],
            None,
        );
        root(&self.root_tx, Job::VtLock, &["vt-lock", "on"], None);
        self.draw_lock();
        self.show()
    }

    /// The first boot's question: shown when no password is set yet.
    pub(super) fn ask_for_password(&mut self) -> io::Result<()> {
        if self.lock.is_some() {
            return Ok(());
        }
        self.lock = Some(Lock::new(Mode::Choose { first: None }));
        self.draw_lock();
        self.show()
    }

    fn unlock(&mut self) -> io::Result<()> {
        let was_locked = matches!(self.lock.as_ref().map(|l| &l.mode), Some(Mode::Unlock));
        self.lock = None;
        if was_locked {
            tracing::info!("unlocked");
            root(&self.root_tx, Job::VtLock, &["vt-lock", "off"], None);
            if let Some(f) = &self.settings.locked_file {
                let _ = std::fs::remove_file(f);
            }
        }
        self.last_input = Instant::now();
        self.screen.repaint();
        self.show()
    }

    fn mark_asked(&self) {
        let f = &self.settings.asked_file;
        if let Some(dir) = f.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(f, b"") {
            tracing::warn!("cannot write {}: {e}", f.display());
        }
    }

    /// A key while the lock screen shows.
    pub(super) fn lock_key(&mut self, k: KeyEvent) -> io::Result<()> {
        let now = Instant::now();
        let Some(lock) = self.lock.as_mut() else {
            return Ok(());
        };
        match lock.key(k, now) {
            Step::Nothing => return Ok(()),
            Step::Redraw => {}
            Step::Check(p) => root(&self.root_tx, Job::Check, &["check-password"], Some(p)),
            Step::Save(p) => root(&self.root_tx, Job::Save, &["set-password"], Some(p)),
            Step::Skip => {
                self.mark_asked();
                return self.unlock();
            }
        }
        self.draw_lock();
        self.show()
    }

    /// A root job finished.
    pub(super) fn root_done(&mut self, d: Done) -> io::Result<()> {
        let now = Instant::now();
        match d.job {
            Job::Check => {
                if d.ok {
                    return self.unlock();
                }
                if let Some(l) = self.lock.as_mut() {
                    l.checked(false, now);
                }
            }
            Job::Save => {
                if d.ok {
                    self.mark_asked();
                    return self.unlock();
                }
                if let Some(l) = self.lock.as_mut() {
                    l.mode = Mode::Choose { first: None };
                    l.message =
                        Some("The password could not be saved (see the log); Esc skips.".into());
                }
            }
            Job::HasPassword { first_boot: true } => {
                if d.ok {
                    // A password exists (set by `termos password`): no question.
                    self.mark_asked();
                } else {
                    return self.ask_for_password();
                }
                return Ok(());
            }
            Job::HasPassword { first_boot: false } => {
                if let Some(l) = self.lock.as_mut() {
                    l.has_password = Some(d.ok);
                }
            }
            Job::VtLock => return Ok(()),
            Job::Suspend => {
                self.suspending = false;
                self.last_input = now;
                self.suspend_checked = now;
                tracing::info!("awake again");
                // The display may have lost its mode while asleep.
                self.display.modeset()?;
                self.lock_dirty = true;
                self.screen.repaint();
            }
        }
        self.draw_lock();
        self.show()
    }

    /// Draws the lock screen into its own pixels; the desktop's stay as
    /// they are, out of sight.
    pub(super) fn draw_lock(&mut self) {
        let Some(lock) = &self.lock else { return };
        let layout = self.screen.layout;
        if self.lock_grid.cols() != layout.cols || self.lock_grid.rows() != layout.rows {
            self.lock_grid = Grid::new(layout.cols, layout.rows);
        }
        let len = self.screen.width as usize * self.screen.height as usize;
        if self.lock_pixels.len() != len {
            self.lock_pixels = vec![0; len];
        }
        let now = jiff::Zoned::now();
        let time = now
            .strftime(self.settings.time_format.pattern())
            .to_string();
        let date = now.strftime("%A %-d %B").to_string();
        lock.draw(&mut self.lock_grid, &time, &date);
        let mut canvas = Canvas {
            pixels: &mut self.lock_pixels,
            stride: self.screen.width as usize,
            width: self.screen.width,
            height: self.screen.height,
        };
        self.screen
            .raster
            .paint_all(&mut canvas, layout, &self.lock_grid);
        self.lock_dirty = true;
        self.lock_minute = Some(super::next_minute());
    }

    /// Puts the lock screen on the display (instead of the desktop).
    pub(super) fn show_lock(&mut self) -> io::Result<()> {
        let damage = if std::mem::take(&mut self.lock_dirty) {
            Damage::all(self.screen.height)
        } else {
            Damage::default()
        };
        self.display.present(&self.lock_pixels, &damage)?;
        Ok(())
    }

    /// Locks, then suspends; the console wakes up locked.
    pub(super) fn suspend_now(&mut self) -> io::Result<()> {
        if self.suspending {
            return Ok(());
        }
        // The first boot's question stays: there is no password to ask.
        if !matches!(
            self.lock.as_ref().map(|l| &l.mode),
            Some(Mode::Choose { .. })
        ) {
            self.lock_now()?;
        }
        tracing::info!("suspending");
        self.suspending = true;
        root(&self.root_tx, Job::Suspend, &["suspend"], None);
        Ok(())
    }

    /// The lid closed (`true`) or opened.
    pub(super) fn lid(&mut self, closed: bool) -> io::Result<()> {
        if !closed {
            return Ok(());
        }
        match self.settings.lid {
            LidAction::Suspend => self.suspend_now(),
            LidAction::Lock => self.lock_now(),
            LidAction::Nothing => Ok(()),
        }
    }

    /// A brightness key.
    pub(super) fn brightness(&mut self, delta: i8) {
        match termos_hw::power::change_brightness(Path::new(termos_hw::power::BACKLIGHT_DIR), delta)
        {
            Ok(p) => tracing::debug!("brightness {p}%"),
            Err(e) => tracing::info!("brightness: {e}"),
        }
    }

    /// The next moment a lock or power timer is due: the idle lock, idle
    /// suspend, the lock screen's clock and its wait.
    pub(super) fn power_deadline(&self) -> Option<Instant> {
        let mut t: Vec<Instant> = Vec::new();
        if self.lock.is_none()
            && !self.settings.boottest
            && !self.settings.nolock
            && let Some(d) = self.settings.lock_after
        {
            t.push(self.last_input + d);
        }
        if self.settings.has_battery
            && !self.suspending
            && let Some(d) = self.settings.suspend_after
        {
            t.push(self.last_input.max(self.suspend_checked) + d);
        }
        if let Some(l) = &self.lock {
            t.extend(l.deadline());
            t.extend(self.lock_minute);
        }
        t.into_iter().min()
    }

    pub(super) fn on_power_timer(&mut self, now: Instant) -> io::Result<()> {
        if let Some(l) = self.lock.as_mut()
            && l.on_timer(now)
        {
            self.draw_lock();
            self.show()?;
        }
        if self.lock.is_some() && self.lock_minute.is_some_and(|t| t <= now) {
            self.draw_lock();
            self.show()?;
        }
        if self.lock.is_none()
            && !self.settings.boottest
            && !self.settings.nolock
            && self
                .settings
                .lock_after
                .is_some_and(|d| now >= self.last_input + d)
        {
            self.lock_now()?;
        }
        if self.settings.has_battery
            && !self.suspending
            && let Some(d) = self.settings.suspend_after
            && now >= self.last_input.max(self.suspend_checked) + d
        {
            self.suspend_checked = now;
            if termos_hw::power::read().on_battery() {
                self.suspend_now()?;
            }
        }
        Ok(())
    }
}
