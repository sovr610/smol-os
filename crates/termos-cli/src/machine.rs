// SPDX-License-Identifier: GPL-3.0-or-later
//! The image's machine verbs for `termos system` (M13d, D-124): keeping
//! the lock screen on its virtual terminal, suspending, the time zone, USB
//! drives and the NTP servers. Each checks its arguments; the root side of
//! everything the desktop's user may ask.

// Only TermOS images (Linux) run these; the tests run everywhere.
#![cfg_attr(not(target_os = "linux"), allow(dead_code, unused_imports))]

use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};

pub(crate) use termos_core::timezones::ZONEINFO;
use termos_core::timezones::{is_tzif, valid_zone};
/// `/etc/localtime` links here; `set-time-zone` replaces it.
pub(crate) const LOCALTIME: &str = "/data/etc/localtime";
/// Where USB drives are mounted (a tmpfs on the image).
pub(crate) const MEDIA: &str = "/media";

/// What to write to `/sys/power/state` given what it offers: `mem` (the
/// kernel picks deep sleep or s2idle by `mem_sleep`), else `freeze`.
pub(crate) fn sleep_state(offered: &str) -> Option<&'static str> {
    let words: Vec<&str> = offered.split_whitespace().collect();
    if words.contains(&"mem") {
        Some("mem")
    } else if words.contains(&"freeze") {
        Some("freeze")
    } else {
        None
    }
}

/// Suspends the machine through `power` (`/sys/power`); returns once it
/// has woken up.
pub(crate) fn suspend(power: &Path) -> anyhow::Result<()> {
    let offered = std::fs::read_to_string(power.join("state"))
        .context("this kernel cannot suspend (no /sys/power/state)")?;
    let Some(state) = sleep_state(&offered) else {
        bail!(
            "this machine offers no sleep state (it offers: {})",
            offered.trim()
        );
    };
    // Written in one go: the write blocks until the machine wakes.
    std::fs::write(power.join("state"), state)
        .with_context(|| format!("the machine would not suspend ({state})"))?;
    Ok(())
}

/// Points `localtime` at `zone` in `zoneinfo` (a link made beside it and
/// renamed over it). `target_base` is the zoneinfo directory as the link
/// names it (`/usr/share/zoneinfo`; tests use their own tree).
#[cfg(unix)]
pub(crate) fn set_time_zone(
    zoneinfo: &Path,
    target_base: &Path,
    localtime: &Path,
    zone: &str,
) -> anyhow::Result<()> {
    if !valid_zone(zone) {
        bail!("`{zone}` is not a time zone's name (like Europe/Paris)");
    }
    if !is_tzif(&zoneinfo.join(zone)) {
        bail!("no time zone `{zone}` (the Clock page in Settings lists them)");
    }
    let dir = localtime.parent().context("the localtime link's folder")?;
    std::fs::create_dir_all(dir).with_context(|| dir.display().to_string())?;
    let tmp = dir.join(".localtime.termos-new");
    let _ = std::fs::remove_file(&tmp);
    std::os::unix::fs::symlink(target_base.join(zone), &tmp)
        .with_context(|| tmp.display().to_string())?;
    std::fs::rename(&tmp, localtime).with_context(|| localtime.display().to_string())?;
    Ok(())
}

/// The type `mount -t` takes for a file system udev found, among those
/// TermOS mounts: FAT, exFAT, ext4 and NTFS (through the kernel's ntfs3).
pub(crate) fn mount_type(fs: &str) -> Option<&'static str> {
    match fs {
        "vfat" => Some("vfat"),
        "exfat" => Some("exfat"),
        "ext4" => Some("ext4"),
        "ntfs" | "ntfs3" => Some("ntfs3"),
        _ => None,
    }
}

/// Mount options: the user owns what is on FAT, exFAT and NTFS (they keep
/// no owners); ext4 keeps its own. Never set-user-id programs or devices.
pub(crate) fn mount_options(fs: &str, uid: u32, gid: u32) -> String {
    match fs {
        "vfat" => format!("nosuid,nodev,uid={uid},gid={gid},umask=0022,utf8"),
        "exfat" | "ntfs3" => format!("nosuid,nodev,uid={uid},gid={gid},umask=0022"),
        _ => "nosuid,nodev".to_owned(),
    }
}

/// A folder name under `/media` for a drive: its label made safe (letters,
/// digits, `-`, `_`, `.`; spaces become `_`), else its device name; with
/// `-2`, `-3`… when taken.
pub(crate) fn mount_name(
    label: Option<&str>,
    device: &str,
    taken: impl Fn(&str) -> bool,
) -> String {
    let clean: String = label
        .unwrap_or("")
        .chars()
        .map(|c| if c == ' ' { '_' } else { c })
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .take(48)
        .collect();
    let base = if clean.is_empty() || clean.chars().all(|c| c == '.') {
        device.to_owned()
    } else {
        clean
    };
    if !taken(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|n| !taken(n))
        .unwrap_or(base)
}

fn sudo_ids() -> anyhow::Result<(u32, u32)> {
    let id = |k: &str| -> anyhow::Result<u32> {
        std::env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .with_context(|| format!("run through sudo ({k})"))
    };
    Ok((id("SUDO_UID")?, id("SUDO_GID")?))
}

/// `termos system mount DEV`: a USB drive the system lists as removable,
/// not mounted yet, with a file system TermOS mounts, onto
/// `/media/LABEL`. Prints where.
#[cfg(target_os = "linux")]
pub(crate) fn mount_drive(device: &Path) -> anyhow::Result<PathBuf> {
    let (uid, gid) = sudo_ids()?;
    let drives = termos_hw::drives::current();
    let Some(drive) = drives.iter().find(|d| d.device == device) else {
        bail!("{}: not a USB drive or card", device.display());
    };
    if let Some(at) = &drive.mount {
        bail!(
            "{} is mounted already, on {}",
            device.display(),
            at.display()
        );
    }
    let fs = drive.fs.as_deref().unwrap_or("");
    let Some(kind) = mount_type(fs) else {
        bail!(
            "{}: TermOS mounts FAT, exFAT, ext4 and NTFS drives, not `{}`",
            device.display(),
            if fs.is_empty() { "unknown" } else { fs }
        );
    };
    let media = Path::new(MEDIA);
    let name = mount_name(drive.label.as_deref(), &drive.name, |n| {
        media.join(n).exists()
    });
    let at = media.join(name);
    std::fs::create_dir(&at).with_context(|| at.display().to_string())?;
    let status = std::process::Command::new("mount")
        .args(["-t", kind, "-o", &mount_options(kind, uid, gid)])
        .arg(device)
        .arg(&at)
        .status()
        .context("running mount")?;
    if !status.success() {
        let _ = std::fs::remove_dir(&at);
        bail!("{} would not mount", device.display());
    }
    writeln!(std::io::stdout(), "{}", at.display())?;
    Ok(at)
}

/// `termos system unmount DEV|DIR`: writes out and unmounts a drive
/// mounted under `/media`, and removes its folder.
#[cfg(target_os = "linux")]
pub(crate) fn unmount_drive(what: &Path) -> anyhow::Result<()> {
    let mounts =
        termos_hw::drives::mounts(&std::fs::read_to_string("/proc/mounts").unwrap_or_default());
    let found = mounts
        .iter()
        .find(|(dev, at)| dev.as_path() == what || at.as_path() == what);
    let Some((_, at)) = found else {
        bail!("{} is not mounted", what.display());
    };
    if at.parent() != Some(Path::new(MEDIA)) {
        bail!("{} is not a drive TermOS mounted", at.display());
    }
    let _ = std::process::Command::new("sync").status();
    let status = std::process::Command::new("umount")
        .arg(at)
        .status()
        .context("running umount")?;
    if !status.success() {
        bail!(
            "{} is in use: close what has files open there (a pane's shell \
             in that folder counts) and eject again",
            at.display()
        );
    }
    let _ = std::fs::remove_dir(at);
    Ok(())
}

/// Turns switching away from the console's virtual terminal off (`true`)
/// or on: while the screen is locked, Ctrl+Alt+F<n> must not leave it.
/// `VT_LOCKSWITCH` needs privileges the console does not have.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
pub(crate) fn vt_lock(on: bool) -> anyhow::Result<()> {
    use std::os::fd::AsRawFd as _;
    // linux/vt.h.
    const VT_LOCKSWITCH: libc::c_ulong = 0x560B;
    const VT_UNLOCKSWITCH: libc::c_ulong = 0x560C;
    let tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty0")
        .context("/dev/tty0")?;
    let request = if on { VT_LOCKSWITCH } else { VT_UNLOCKSWITCH };
    // SAFETY: the descriptor is open for the call's length (`tty` lives
    // until the end of the function); these two requests take no argument
    // and read no memory.
    let r = unsafe { libc::ioctl(tty.as_raw_fd(), request as _, 0) };
    if r != 0 {
        return Err(std::io::Error::last_os_error()).context("VT_LOCKSWITCH");
    }
    Ok(())
}

/// The NTP servers for the image's time sync: the desktop user's
/// `[clock] ntp-servers`, each checked, else `pool.ntp.org`.
pub(crate) fn ntp_servers(config: &Path) -> Vec<String> {
    let servers: Vec<String> = termos_core::config::Config::load(config)
        .map(|c| c.clock.ntp_servers)
        .unwrap_or_default()
        .into_iter()
        .filter(|s| termos_core::config::valid_ntp_server(s))
        .take(8)
        .collect();
    if servers.is_empty() {
        vec!["pool.ntp.org".to_owned()]
    } else {
        servers
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sleep_states() {
        assert_eq!(sleep_state("freeze mem disk\n"), Some("mem"));
        assert_eq!(sleep_state("freeze disk\n"), Some("freeze"));
        assert_eq!(sleep_state("disk\n"), None);
        assert_eq!(sleep_state(""), None);
        let dir = tempfile::tempdir().expect("dir");
        assert!(suspend(dir.path()).is_err(), "no state file");
        std::fs::write(dir.path().join("state"), "disk\n").expect("write");
        assert!(suspend(dir.path()).is_err(), "nothing to sleep in");
    }

    #[cfg(unix)]
    #[test]
    fn the_time_zone_link_is_replaced() {
        let dir = tempfile::tempdir().expect("dir");
        let zi = dir.path().join("zoneinfo");
        std::fs::create_dir_all(zi.join("Europe")).expect("mkdir");
        std::fs::create_dir_all(zi.join("posix/Europe")).expect("mkdir");
        std::fs::create_dir_all(zi.join("Etc")).expect("mkdir");
        std::fs::write(zi.join("Europe/Paris"), b"TZif2...").expect("write");
        std::fs::write(zi.join("posix/Europe/Paris"), b"TZif2...").expect("write");
        std::fs::write(zi.join("Etc/UTC"), b"TZif2...").expect("write");
        std::fs::write(zi.join("zone.tab"), b"# not a zone").expect("write");
        let lt = dir.path().join("data/etc/localtime");
        set_time_zone(&zi, &zi, &lt, "Europe/Paris").expect("set");
        assert_eq!(
            termos_core::timezones::current_zone(&lt).as_deref(),
            Some("Europe/Paris")
        );
        set_time_zone(&zi, &zi, &lt, "Etc/UTC").expect("set again");
        assert_eq!(
            termos_core::timezones::current_zone(&lt).as_deref(),
            Some("Etc/UTC")
        );
        assert!(set_time_zone(&zi, &zi, &lt, "Mars/Olympus").is_err());
        assert!(
            set_time_zone(&zi, &zi, &lt, "zone.tab").is_err(),
            "not TZif"
        );
        assert!(set_time_zone(&zi, &zi, &lt, "../zoneinfo/Europe/Paris").is_err());
    }

    #[test]
    fn drives_are_mounted_by_label() {
        assert_eq!(mount_type("vfat"), Some("vfat"));
        assert_eq!(mount_type("ntfs"), Some("ntfs3"));
        assert_eq!(mount_type("btrfs"), None);
        assert_eq!(
            mount_options("vfat", 1000, 1000),
            "nosuid,nodev,uid=1000,gid=1000,umask=0022,utf8"
        );
        assert_eq!(mount_options("ext4", 1000, 1000), "nosuid,nodev");
        let none = |_: &str| false;
        assert_eq!(mount_name(Some("MY STICK"), "sdb1", none), "MY_STICK");
        assert_eq!(mount_name(Some("../../etc"), "sdb1", none), "....etc");
        assert_eq!(mount_name(Some(".."), "sdb1", none), "sdb1");
        assert_eq!(mount_name(Some("ÉTÉ"), "sdb1", none), "T");
        assert_eq!(mount_name(None, "sdb1", none), "sdb1");
        let taken = |n: &str| n == "CARD" || n == "CARD-2";
        assert_eq!(mount_name(Some("CARD"), "mmcblk0p1", taken), "CARD-3");
    }

    #[test]
    fn ntp_servers_from_the_config() {
        let dir = tempfile::tempdir().expect("dir");
        let cfg = dir.path().join("config.toml");
        assert_eq!(ntp_servers(&cfg), ["pool.ntp.org"]);
        std::fs::write(
            &cfg,
            "[clock]\nntp-servers = [\"time.example.org\", \"-q\", \"10.0.0.1\"]\n",
        )
        .expect("write");
        assert_eq!(ntp_servers(&cfg), ["time.example.org", "10.0.0.1"]);
        std::fs::write(&cfg, "[clock]\nntp-servers = [\"a b\"]\n").expect("write");
        assert_eq!(ntp_servers(&cfg), ["pool.ntp.org"]);
    }
}
