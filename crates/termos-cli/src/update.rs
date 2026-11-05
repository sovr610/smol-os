// SPDX-License-Identifier: GPL-3.0-or-later
//! `termos update` and `termos system` (D-077, D-078): A/B updates on TermOS
//! images.
//!
//! `termos update` runs as the desktop's user. It reads what the image is
//! (`/etc/termos/board`, `/etc/termos/version`), asks GitHub Releases for a
//! newer bundle only when told to, downloads it into the user's data folder
//! and hands it to `termos system`, which sudo runs as root with one rule
//! (`termos ALL=(root) NOPASSWD: /usr/bin/termos system *`). `termos system`
//! does a fixed set of things with checked arguments: RAUC installs the
//! bundle into the other root slot, after checking its signature against the
//! image's keyring and its `compatible` against this board.

use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context as _, bail};
use clap::Subcommand;
use futures_util::StreamExt as _;
use termos_core::paths::Paths;

macro_rules! out {
    ($($arg:tt)*) => {
        writeln!(std::io::stdout(), $($arg)*)?
    };
}

/// Where releases come from (D-077).
const REPOSITORY: &str = "sovr610/smol-os";
/// The binary sudo's rule names; `termos update` asks for it by this path.
const SYSTEM_TERMOS: &str = "/usr/bin/termos";

#[derive(Debug, Subcommand)]
pub(crate) enum UpdateCmd {
    /// What runs now: the version, the board, the slot, and RAUC's view of
    /// both slots.
    Status,
    /// Ask GitHub Releases whether a newer TermOS exists for this board.
    Check,
    /// Download an update and install it into the other slot.
    Install {
        /// A bundle (`.raucb`): a file or a URL. By default, the newest
        /// release's bundle for this board.
        bundle: Option<String>,
        /// Install without asking first.
        #[arg(long)]
        yes: bool,
        /// Restart into the update once it is installed.
        #[arg(long)]
        restart: bool,
        /// Wait for Enter at the end (the desktop's Updates window).
        #[arg(long)]
        wait: bool,
    },
    /// Restart the machine (into a newly installed update, if any).
    Restart,
}

#[derive(Debug, Subcommand)]
pub(crate) enum SystemCmd {
    /// Install a downloaded bundle with RAUC.
    InstallBundle { bundle: PathBuf },
    /// RAUC's view of the slots.
    Status,
    /// Restart the machine (on a Pi with an update to try, into it once).
    Restart,
    /// Turn the machine off.
    PowerOff,
    /// From the live ISO: its disk image onto a disk (D-080).
    InstallDisk { disk: PathBuf },
    /// Save a Wi-Fi network for iwd; its passphrase (none for an open
    /// network) is read from standard input (D-081).
    WifiAdd { ssid: String },
    /// Forget a saved Wi-Fi network.
    WifiForget { ssid: String },
    /// RAUC's custom boot-loader backend on a Raspberry Pi (tryboot).
    RaucTryboot {
        /// get-primary, set-primary SLOT, get-state SLOT, set-state SLOT
        /// good|bad, get-current.
        #[arg(num_args = 1..=3)]
        args: Vec<String>,
    },
    /// RAUC's post-install handler on a Raspberry Pi: the new slot's boot
    /// files onto the boot partition.
    RaucTrybootInstall,
    /// The lock screen (D-124): the user's new password, read from standard
    /// input (an empty line removes it).
    SetPassword,
    /// The password on standard input against the user's; exits 1 when
    /// wrong.
    CheckPassword,
    /// Exits 0 when the user has a password, 1 when not.
    HasPassword,
    /// Keep the console's virtual terminal (`on`) while locked, or let
    /// Ctrl+Alt+F<n> switch again (`off`).
    VtLock {
        #[arg(value_parser = ["on", "off"])]
        state: String,
    },
    /// Suspend to RAM (or to idle where the machine has no deeper sleep).
    Suspend,
    /// Point /etc/localtime at a zone (`Europe/Paris`).
    SetTimeZone { zone: String },
    /// Mount a USB drive on /media/LABEL; prints where.
    Mount { device: PathBuf },
    /// Write out and unmount a drive mounted under /media.
    Unmount { what: PathBuf },
    /// The time sync's servers, one per line, from the user's `[clock]`.
    NtpServers,
}

/// The boot partition, where S11termos-data mounts it.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const ESP: &str = "/boot/esp";

/// What this TermOS image is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Image {
    pub(crate) board: String,
    pub(crate) version: String,
    /// The root slot it booted from (`A` or `B`); none when it runs from
    /// the live ISO.
    pub(crate) slot: Option<String>,
}

impl Image {
    /// Reads `/etc/termos/*` and the kernel's command line under `root`.
    pub(crate) fn read(root: &Path) -> anyhow::Result<Self> {
        let read = |p: &str| std::fs::read_to_string(root.join(p)).map(|s| s.trim().to_owned());
        let board = read("etc/termos/board").map_err(|_| {
            anyhow::anyhow!(
                "this is not a TermOS image, so `termos update` has nothing to update; \
                 a TermOS installed in another system updates with a new release"
            )
        })?;
        let version = read("etc/termos/version").context("etc/termos/version")?;
        let slot = read("proc/cmdline").ok().and_then(|c| booted_slot(&c));
        Ok(Self {
            board,
            version,
            slot,
        })
    }

    /// The image this process runs in (`TERMOS_IMAGE_ROOT` moves `/`, for
    /// tests).
    fn detect() -> anyhow::Result<Self> {
        let root =
            std::env::var_os("TERMOS_IMAGE_ROOT").map_or_else(|| PathBuf::from("/"), PathBuf::from);
        Self::read(&root)
    }

    /// The slot an update goes into.
    fn other_slot(&self) -> Option<&'static str> {
        match self.slot.as_deref() {
            Some("A") => Some("B"),
            Some("B") => Some("A"),
            _ => None,
        }
    }
}

/// `rauc.slot=` from a kernel command line.
pub(crate) fn booted_slot(cmdline: &str) -> Option<String> {
    cmdline
        .split_whitespace()
        .find_map(|w| w.strip_prefix("rauc.slot="))
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// The numeric parts of a version (`v0.10.2+test` → `[0, 10, 2]`).
fn version_parts(v: &str) -> Vec<u64> {
    let v = v.trim().trim_start_matches('v');
    let core = v.split(['+', '-']).next().unwrap_or("");
    core.split('.').map_while(|p| p.parse().ok()).collect()
}

/// Whether `candidate` is newer than `current`.
pub(crate) fn newer(candidate: &str, current: &str) -> bool {
    version_parts(candidate) > version_parts(current)
}

/// A release's bundle for one board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Offer {
    pub(crate) version: String,
    pub(crate) name: String,
    pub(crate) url: String,
}

/// The bundle for `board` in a GitHub release (`releases/latest`'s JSON):
/// the asset named `termos-<board>-<version>.raucb`.
pub(crate) fn offer(release: &serde_json::Value, board: &str) -> Option<Offer> {
    let version = release["tag_name"]
        .as_str()?
        .trim_start_matches('v')
        .to_owned();
    let want = format!("termos-{board}-{version}.raucb");
    release["assets"].as_array()?.iter().find_map(|a| {
        let name = a["name"].as_str()?;
        if name != want {
            return None;
        }
        Some(Offer {
            version: version.clone(),
            name: name.to_owned(),
            url: a["browser_download_url"].as_str()?.to_owned(),
        })
    })
}

/// A bundle path `termos system` accepts: absolute, plain (no `..`), a
/// `.raucb` file.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn check_bundle_path(p: &Path) -> anyhow::Result<()> {
    if !p.is_absolute() || p.components().any(|c| matches!(c, Component::ParentDir)) {
        bail!("{}: give the bundle's full path", p.display());
    }
    if p.extension().is_none_or(|e| e != "raucb") {
        bail!("{}: not a RAUC bundle (.raucb)", p.display());
    }
    let meta = std::fs::metadata(p).with_context(|| p.display().to_string())?;
    if !meta.is_file() {
        bail!("{}: not a file", p.display());
    }
    Ok(())
}

fn rt() -> anyhow::Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?)
}

/// The newest release's bundle for `board`.
async fn latest(board: &str) -> anyhow::Result<Option<Offer>> {
    let api =
        std::env::var("TERMOS_UPDATE_API").unwrap_or_else(|_| "https://api.github.com".into());
    let url = format!(
        "{}/repos/{REPOSITORY}/releases/latest",
        api.trim_end_matches('/')
    );
    let resp = termos_models::hub::client()?
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .with_context(|| format!("asking {url}"))?;
    if resp.status().as_u16() == 404 {
        return Ok(None);
    }
    if !resp.status().is_success() {
        bail!("{url}: {}", resp.status());
    }
    let release: serde_json::Value = resp.json().await.context("reading the release")?;
    Ok(offer(&release, board))
}

/// Downloads `url` into `dir`, showing progress; returns the file.
async fn download(url: &str, dir: &Path) -> anyhow::Result<PathBuf> {
    let name = url
        .rsplit('/')
        .next()
        .filter(|n| n.ends_with(".raucb") && !n.contains(['?', '#']))
        .unwrap_or("update.raucb");
    std::fs::create_dir_all(dir).with_context(|| dir.display().to_string())?;
    let file = dir.join(name);
    let part = dir.join(format!("{name}.part"));
    let resp = termos_models::hub::client()?
        .get(url)
        .send()
        .await
        .with_context(|| format!("downloading {url}"))?;
    if !resp.status().is_success() {
        bail!("{url}: {}", resp.status());
    }
    let total = resp.content_length();
    let mut out = std::fs::File::create(&part).with_context(|| part.display().to_string())?;
    let mut got = 0u64;
    let mut shown = 0u64;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.with_context(|| format!("downloading {url}"))?;
        out.write_all(&chunk)?;
        got += chunk.len() as u64;
        if let Some(total) = total.filter(|t| *t > 0) {
            let pct = got * 100 / total;
            if pct >= shown + 10 {
                shown = pct - pct % 10;
                out!("  {shown}% of {} MB", total / 1_000_000);
            }
        }
    }
    out.sync_all()?;
    std::fs::rename(&part, &file)?;
    Ok(file)
}

/// Runs `termos system <args>` as root, through sudo's rule.
fn as_root(args: &[&str]) -> anyhow::Result<()> {
    let status = Command::new("sudo")
        .arg("-n")
        .arg(SYSTEM_TERMOS)
        .arg("system")
        .args(args)
        .status()
        .context("running sudo")?;
    if !status.success() {
        bail!("`termos system {}` failed ({status})", args.join(" "));
    }
    Ok(())
}

fn ask(question: &str) -> bool {
    print!("{question} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    let _ = std::io::stdin().read_line(&mut answer);
    matches!(answer.trim(), "y" | "Y" | "yes")
}

pub(crate) fn update_command(cmd: &UpdateCmd, paths: &Paths) -> anyhow::Result<ExitCode> {
    let result = run_update(cmd, paths);
    if let UpdateCmd::Install { wait: true, .. } = cmd {
        // The window closes when this ends: show what happened first.
        if let Err(e) = &result {
            out!("termos: {e:#}");
        }
        print!("\nPress Enter to close.");
        std::io::stdout().flush()?;
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
    }
    result
}

fn run_update(cmd: &UpdateCmd, paths: &Paths) -> anyhow::Result<ExitCode> {
    let image = Image::detect()?;
    match cmd {
        UpdateCmd::Status => {
            out!(
                "TermOS {} on {}, running from {}",
                image.version,
                image.board,
                image
                    .slot
                    .as_deref()
                    .map_or_else(|| "the live ISO".to_owned(), |s| format!("slot {s}"))
            );
            if image.slot.is_some() {
                as_root(&["status"])?;
            }
        }
        UpdateCmd::Check => match rt()?.block_on(latest(&image.board))? {
            Some(o) if newer(&o.version, &image.version) => {
                out!(
                    "TermOS {} is available (this is {}): termos update install",
                    o.version,
                    image.version
                );
            }
            Some(o) => out!(
                "TermOS {} is up to date (the newest release is {}).",
                image.version,
                o.version
            ),
            None => out!("No release has an update for {} yet.", image.board),
        },
        UpdateCmd::Install {
            bundle,
            yes,
            restart,
            wait: _,
        } => {
            let Some(other) = image.other_slot() else {
                bail!("this system runs from the live ISO: install it with `termos install` first");
            };
            let rt = rt()?;
            let (source, what) = match bundle {
                Some(b) => (b.clone(), format!("the update in {b}")),
                None => match rt.block_on(latest(&image.board))? {
                    Some(o) if newer(&o.version, &image.version) => (
                        o.url,
                        format!("TermOS {} (this is {})", o.version, image.version),
                    ),
                    Some(_) => {
                        out!("TermOS {} is up to date.", image.version);
                        return Ok(ExitCode::SUCCESS);
                    }
                    None => bail!("no release has an update for {} yet", image.board),
                },
            };
            if !yes && !ask(&format!("Install {what} into slot {other}?")) {
                return Ok(ExitCode::from(1));
            }
            let file = if source.starts_with("https://") || source.starts_with("http://") {
                out!("Downloading {source}");
                rt.block_on(download(&source, &paths.data_dir.join("updates")))?
            } else {
                std::path::absolute(&source).with_context(|| source.clone())?
            };
            out!("Installing into slot {other}");
            as_root(&["install-bundle", &file.to_string_lossy()])?;
            if source.starts_with("http") {
                let _ = std::fs::remove_file(&file);
            }
            out!("TERMOS-UPDATE installed into slot {other}");
            if *restart {
                as_root(&["restart"])?;
            } else {
                out!("Restart to use it: termos update restart");
            }
        }
        UpdateCmd::Restart => as_root(&["restart"])?,
    }
    Ok(ExitCode::SUCCESS)
}

/// `termos system`: the root side, run through sudo. `config` is the
/// config file of the user it runs for (`ntp-servers` reads it).
pub(crate) fn system_command(cmd: &SystemCmd, config: &Path) -> anyhow::Result<ExitCode> {
    #[cfg(unix)]
    if !rustix::process::geteuid().is_root() {
        bail!("`termos system` runs as root, through sudo; use `termos update`");
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (cmd, config);
        bail!("`termos system` exists on TermOS images (Linux)");
    }
    #[cfg(target_os = "linux")]
    {
        let run = |program: &str, args: &[&std::ffi::OsStr]| -> anyhow::Result<ExitCode> {
            let status = Command::new(program)
                .args(args)
                .status()
                .with_context(|| format!("running {program}"))?;
            Ok(if status.success() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            })
        };
        let boot = crate::tryboot::Boot::new(ESP);
        let on_pi = || Image::detect().is_ok_and(|i| i.board.starts_with("rpi"));
        // The machine verbs (D-124) are the image's: on another Linux they
        // would change a system TermOS does not own.
        let image_only = matches!(
            cmd,
            SystemCmd::SetPassword
                | SystemCmd::CheckPassword
                | SystemCmd::HasPassword
                | SystemCmd::VtLock { .. }
                | SystemCmd::Suspend
                | SystemCmd::SetTimeZone { .. }
                | SystemCmd::Mount { .. }
                | SystemCmd::Unmount { .. }
                | SystemCmd::NtpServers
        );
        if image_only {
            Image::detect()?;
        }
        let shadow = Path::new(crate::password::SHADOW);
        match cmd {
            SystemCmd::SetPassword => crate::password::set_password(shadow),
            SystemCmd::CheckPassword => crate::password::check_password(shadow),
            SystemCmd::HasPassword => crate::password::has_password(shadow),
            SystemCmd::VtLock { state } => {
                crate::machine::vt_lock(state == "on")?;
                Ok(ExitCode::SUCCESS)
            }
            SystemCmd::Suspend => {
                crate::machine::suspend(Path::new("/sys/power"))?;
                Ok(ExitCode::SUCCESS)
            }
            SystemCmd::SetTimeZone { zone } => {
                let zi = Path::new(crate::machine::ZONEINFO);
                crate::machine::set_time_zone(zi, zi, Path::new(crate::machine::LOCALTIME), zone)?;
                Ok(ExitCode::SUCCESS)
            }
            SystemCmd::Mount { device } => {
                crate::machine::mount_drive(device)?;
                Ok(ExitCode::SUCCESS)
            }
            SystemCmd::Unmount { what } => {
                crate::machine::unmount_drive(what)?;
                Ok(ExitCode::SUCCESS)
            }
            SystemCmd::NtpServers => {
                for s in crate::machine::ntp_servers(config) {
                    out!("{s}");
                }
                Ok(ExitCode::SUCCESS)
            }
            SystemCmd::InstallBundle { bundle } => {
                check_bundle_path(bundle)?;
                run("rauc", &["install".as_ref(), bundle.as_os_str()])
            }
            SystemCmd::Status => run("rauc", &["status".as_ref(), "--detailed".as_ref()]),
            SystemCmd::Restart => {
                if on_pi() && boot.pending()?.is_some() {
                    boot.mark_tried()?;
                    return Err(tryboot_restart());
                }
                run("reboot", &[])
            }
            SystemCmd::PowerOff => run("poweroff", &[]),
            SystemCmd::InstallDisk { disk } => {
                crate::install::install_disk(disk)?;
                Ok(ExitCode::SUCCESS)
            }
            SystemCmd::WifiAdd { ssid } => {
                let passphrase = crate::wifi::read_passphrase()?;
                crate::wifi::add(Path::new(crate::wifi::IWD_DIR), ssid, passphrase.as_ref())?;
                Ok(ExitCode::SUCCESS)
            }
            SystemCmd::WifiForget { ssid } => {
                if !crate::wifi::forget(Path::new(crate::wifi::IWD_DIR), ssid)? {
                    bail!("{ssid} was not saved");
                }
                Ok(ExitCode::SUCCESS)
            }
            SystemCmd::RaucTryboot { args } => {
                let arg = |i: usize| args.get(i).map(String::as_str).unwrap_or("");
                let booted = Image::detect().ok().and_then(|i| i.slot);
                match arg(0) {
                    "get-primary" => out!("{}", boot.get_primary()?),
                    "set-primary" => boot.set_primary(arg(1))?,
                    "get-state" => out!("{}", boot.get_state(arg(1))?),
                    "set-state" => {
                        let good = match arg(2) {
                            "good" => true,
                            "bad" => false,
                            other => bail!("set-state: `{other}` is not good or bad"),
                        };
                        boot.set_state(arg(1), good, booted.as_deref())?;
                    }
                    "get-current" => out!(
                        "{}",
                        booted.context("no rauc.slot= on the kernel's command line")?
                    ),
                    other => bail!("rauc-tryboot: no action `{other}`"),
                }
                Ok(ExitCode::SUCCESS)
            }
            SystemCmd::RaucTrybootInstall => {
                let slots = std::env::var("RAUC_TARGET_SLOTS").unwrap_or_default();
                let mnt = Path::new("/run/termos-slot");
                for i in slots.split_whitespace() {
                    let var =
                        |k: &str| std::env::var(format!("RAUC_SLOT_{k}_{i}")).unwrap_or_default();
                    let (device, slot) = (var("DEVICE"), var("BOOTNAME"));
                    if slot.is_empty() {
                        continue;
                    }
                    std::fs::create_dir_all(mnt)?;
                    let mounted = Command::new("mount")
                        .args(["-t", "squashfs", "-o", "ro", &device])
                        .arg(mnt)
                        .status()?;
                    if !mounted.success() {
                        bail!("cannot mount {device}");
                    }
                    let done = boot.install_slot(&slot, mnt);
                    let _ = Command::new("umount").arg(mnt).status();
                    done?;
                }
                Ok(ExitCode::SUCCESS)
            }
        }
    }
}

/// Restarts a Pi into the slot it is trying, once (D-078): the firmware
/// reads `tryboot.txt` for the next boot only when the restart says
/// `0 tryboot`, which BusyBox's `reboot` cannot. So this shuts down the way
/// init does (the services' stop scripts, every other process told to end,
/// then killed, the file systems synced and made read-only) and restarts
/// with that argument. It returns only if the restart failed.
#[cfg(target_os = "linux")]
fn tryboot_restart() -> anyhow::Error {
    ignore_ending_signals();
    let _ = Command::new("/etc/init.d/rcK").status();
    signal_everyone(libc::SIGTERM);
    std::thread::sleep(std::time::Duration::from_secs(2));
    signal_everyone(libc::SIGKILL);
    let _ = Command::new("sync").status();
    for fs in ["/data", ESP] {
        let _ = Command::new("mount")
            .args(["-o", "remount,ro", fs])
            .status();
    }
    let _ = Command::new("sync").status();
    let err = reboot_tryboot();
    anyhow::anyhow!("the tryboot restart failed: {err}")
}

/// sudo passes the signals it gets on to us; we must outlive them.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn ignore_ending_signals() {
    for sig in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT] {
        // SAFETY: SIG_IGN is a valid disposition for these signals, and
        // nothing in this process relies on handling them from here on.
        unsafe {
            libc::signal(sig, libc::SIG_IGN);
        }
    }
}

/// `kill(-1, sig)`: every process but init and this one.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn signal_everyone(sig: libc::c_int) {
    // SAFETY: kill(2) takes no pointers; -1 means every process we may
    // signal except init and the caller.
    unsafe {
        libc::kill(-1, sig);
    }
}

/// `reboot(2)` with `LINUX_REBOOT_CMD_RESTART2` and `0 tryboot`, which the
/// Pi's firmware driver passes on to the firmware.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn reboot_tryboot() -> std::io::Error {
    let arg = c"0 tryboot";
    // SAFETY: reboot(2) reads the NUL-terminated string `arg`, a static
    // that outlives the call; the other arguments are the kernel's
    // constants. On success the call does not return.
    unsafe {
        libc::syscall(
            libc::SYS_reboot,
            libc::LINUX_REBOOT_MAGIC1,
            libc::LINUX_REBOOT_MAGIC2,
            libc::LINUX_REBOOT_CMD_RESTART2,
            arg.as_ptr(),
        );
    }
    std::io::Error::last_os_error()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_and_versions() {
        assert_eq!(
            booted_slot("BOOT_IMAGE=/boot/bzImage root=PARTUUID=7e57c0de-03 rauc.slot=B quiet")
                .as_deref(),
            Some("B")
        );
        assert_eq!(booted_slot("quiet termos.live"), None);
        assert!(newer("0.2.0", "0.1.9"));
        assert!(newer("v0.10.0", "0.9.3"));
        assert!(newer("1.0", "0.99.99"));
        assert!(!newer("0.1.0", "0.1.0+test-update"));
        assert!(!newer("0.1.0", "0.2.0"));
    }

    #[test]
    fn the_image_is_read_from_its_files() {
        let dir = tempfile::tempdir().expect("dir");
        let err = Image::read(dir.path()).expect_err("not an image");
        assert!(err.to_string().contains("not a TermOS image"), "{err}");
        std::fs::create_dir_all(dir.path().join("etc/termos")).expect("mkdir");
        std::fs::create_dir_all(dir.path().join("proc")).expect("mkdir");
        std::fs::write(dir.path().join("etc/termos/board"), "x86_64\n").expect("write");
        std::fs::write(dir.path().join("etc/termos/version"), "0.1.0\n").expect("write");
        std::fs::write(dir.path().join("proc/cmdline"), "root=x rauc.slot=A\n").expect("write");
        let image = Image::read(dir.path()).expect("image");
        assert_eq!(
            image,
            Image {
                board: "x86_64".into(),
                version: "0.1.0".into(),
                slot: Some("A".into())
            }
        );
        assert_eq!(image.other_slot(), Some("B"));
    }

    #[test]
    fn a_release_offers_this_boards_bundle() {
        let release = serde_json::json!({
            "tag_name": "v0.2.0",
            "assets": [
                {"name": "termos-rpi4-0.2.0.raucb", "browser_download_url": "https://x/rpi4"},
                {"name": "termos-x86_64-0.2.0.raucb", "browser_download_url": "https://x/x86"},
                {"name": "termos-x86_64-0.2.0.iso", "browser_download_url": "https://x/iso"}
            ]
        });
        assert_eq!(
            offer(&release, "x86_64"),
            Some(Offer {
                version: "0.2.0".into(),
                name: "termos-x86_64-0.2.0.raucb".into(),
                url: "https://x/x86".into()
            })
        );
        assert_eq!(offer(&release, "aarch64-virt"), None);
        assert_eq!(
            offer(&serde_json::json!({"message": "Not Found"}), "x86_64"),
            None
        );
    }

    #[test]
    fn system_takes_only_full_paths_to_bundles() {
        let dir = tempfile::tempdir().expect("dir");
        let bundle = dir.path().join("u.raucb");
        std::fs::write(&bundle, b"x").expect("write");
        assert!(check_bundle_path(&bundle).is_ok());
        assert!(check_bundle_path(Path::new("u.raucb")).is_err());
        assert!(check_bundle_path(&dir.path().join("../x/u.raucb")).is_err());
        assert!(check_bundle_path(&dir.path().join("u.tar")).is_err());
        assert!(check_bundle_path(dir.path()).is_err());
        assert!(check_bundle_path(&dir.path().join("missing.raucb")).is_err());
    }
}
