// SPDX-License-Identifier: GPL-3.0-or-later
//! `termos install` (D-080): from the live ISO, TermOS onto a whole disk.
//!
//! The desktop's user picks the disk by typing its name; `termos system
//! install-disk` (root, through sudo, D-078) then writes the disk image the
//! ISO carries onto it. The installed system grows its data partition to
//! the end of the disk on its first boot.
// Only Linux runs it; the tests run everywhere.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, bail};
use clap::Args;

macro_rules! out {
    ($($arg:tt)*) => {
        writeln!(std::io::stdout(), $($arg)*)?
    };
}

/// The live ISO's volume label (board/x86_64/live-*.cfg).
pub(crate) const LIVE_LABEL: &str = "TERMOS_LIVE";

#[derive(Debug, Args)]
pub(crate) struct InstallArgs {
    /// Erase this disk (`/dev/sda`, `nvme0n1`) without asking first.
    #[arg(long, value_name = "DISK")]
    erase: Option<String>,
}

/// A disk TermOS could go on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Disk {
    pub(crate) name: String,
    pub(crate) bytes: u64,
    pub(crate) model: String,
    pub(crate) removable: bool,
}

/// The disks under `sys` (`/sys/block`): not loop, RAM, CD, device-mapper,
/// RAID or network block devices, and not empty.
pub(crate) fn disks(sys: &Path) -> Vec<Disk> {
    let Ok(entries) = std::fs::read_dir(sys) else {
        return Vec::new();
    };
    let read = |p: PathBuf| {
        std::fs::read_to_string(p)
            .map(|s| s.trim().to_owned())
            .unwrap_or_default()
    };
    let mut out: Vec<Disk> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let skip = [
                "loop", "ram", "zram", "sr", "dm-", "md", "nbd", "fd", "mtdblock",
            ];
            if skip.iter().any(|p| name.starts_with(p)) {
                return None;
            }
            let dir = sys.join(&name);
            let bytes = read(dir.join("size")).parse::<u64>().ok()? * 512;
            (bytes > 0).then(|| Disk {
                model: read(dir.join("device/model")),
                removable: read(dir.join("removable")) == "1",
                name,
                bytes,
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The disk a device (`/dev/sdb1`, `/dev/sr0`) is on, by name.
pub(crate) fn disk_of(dev: &Path, class_block: &Path) -> Option<String> {
    let name = dev.file_name()?.to_string_lossy().into_owned();
    let entry = class_block.join(&name);
    if entry.join("partition").exists() {
        // A partition's sysfs folder is inside its disk's.
        let parent = std::fs::canonicalize(entry.join("..")).ok()?;
        return parent.file_name().map(|n| n.to_string_lossy().into_owned());
    }
    Some(name)
}

/// `/dev/sda`, `sda` → `sda`.
pub(crate) fn disk_name(arg: &str) -> &str {
    arg.trim().trim_start_matches("/dev/")
}

fn gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

fn live() -> bool {
    std::fs::read_to_string("/proc/cmdline")
        .is_ok_and(|c| c.split_whitespace().any(|w| w == "termos.live"))
}

/// The disks `termos install` offers: all but the one the live system
/// started from.
fn candidates() -> Vec<Disk> {
    let medium = std::fs::canonicalize(format!("/dev/disk/by-label/{LIVE_LABEL}"))
        .ok()
        .and_then(|d| disk_of(&d, Path::new("/sys/class/block")));
    disks(Path::new("/sys/block"))
        .into_iter()
        .filter(|d| Some(&d.name) != medium.as_ref())
        .collect()
}

pub(crate) fn install_command(args: &InstallArgs) -> anyhow::Result<ExitCode> {
    if !live() {
        bail!("`termos install` runs from the live ISO; this system is installed already");
    }
    let list = candidates();
    if list.is_empty() {
        bail!("no disk to install onto");
    }
    let name = match &args.erase {
        Some(d) => disk_name(d).to_owned(),
        None => {
            out!("TermOS goes onto a whole disk, which it erases. The disks here:");
            for d in &list {
                out!(
                    "  {:<10} {:>9}  {}{}",
                    d.name,
                    gb(d.bytes),
                    d.model,
                    if d.removable { " (removable)" } else { "" }
                );
            }
            print!(
                "Type the name of the disk to erase and install onto (or press Enter to stop): "
            );
            std::io::stdout().flush()?;
            let mut answer = String::new();
            std::io::stdin().read_line(&mut answer)?;
            let answer = disk_name(&answer).to_owned();
            if answer.is_empty() {
                return Ok(ExitCode::from(1));
            }
            answer
        }
    };
    let Some(disk) = list.iter().find(|d| d.name == name) else {
        bail!("`{name}` is not one of the disks listed");
    };
    out!(
        "Installing TermOS onto /dev/{} ({})",
        disk.name,
        gb(disk.bytes)
    );
    let status = std::process::Command::new("sudo")
        .args(["-n", "/usr/bin/termos", "system", "install-disk"])
        .arg(format!("/dev/{}", disk.name))
        .status()
        .context("running sudo")?;
    if !status.success() {
        bail!("the installation failed");
    }
    out!(
        "TermOS is on /dev/{}. Take out the ISO and restart: termos update restart",
        disk.name
    );
    Ok(ExitCode::SUCCESS)
}

/// `termos system install-disk <disk>` (root): the ISO's disk image onto
/// the disk, then the disk's last MiB zeroed (an old GPT's backup header
/// must not outlive the new MBR).
#[cfg(target_os = "linux")]
pub(crate) fn install_disk(arg: &Path) -> anyhow::Result<()> {
    use std::io::Read as _;
    use std::os::unix::fs::FileExt as _;
    if !live() {
        bail!("install-disk runs from the live ISO");
    }
    let name = disk_name(&arg.to_string_lossy()).to_owned();
    let Some(disk) = candidates().into_iter().find(|d| d.name == name) else {
        bail!("{}: not a disk to install onto", arg.display());
    };
    let mounts = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
    if mounts
        .lines()
        .any(|l| l.starts_with(&format!("/dev/{name}")))
    {
        bail!("/dev/{name} is in use (mounted)");
    }
    let iso = Path::new("/run/termos-live");
    std::fs::create_dir_all(iso)?;
    let medium = format!("/dev/disk/by-label/{LIVE_LABEL}");
    let mounted = std::process::Command::new("mount")
        .args(["-t", "iso9660", "-o", "ro", &medium])
        .arg(iso)
        .status()?;
    if !mounted.success() {
        bail!("cannot mount the live ISO ({medium})");
    }
    let result = (|| -> anyhow::Result<()> {
        let payload = iso.join("termos/disk.img.zst");
        let size: u64 = std::fs::read_to_string(iso.join("termos/disk.img.size"))
            .context("the ISO's disk.img.size")?
            .trim()
            .parse()
            .context("the ISO's disk.img.size")?;
        if size > disk.bytes {
            bail!("/dev/{name} is too small: TermOS needs {}", gb(size));
        }
        let mut zstd = std::process::Command::new("zstd")
            .arg("-dc")
            .arg(&payload)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .context("running zstd")?;
        let mut from = zstd.stdout.take().context("zstd's output")?;
        let dev = std::fs::OpenOptions::new()
            .write(true)
            .open(format!("/dev/{name}"))
            .with_context(|| format!("/dev/{name}"))?;
        let mut buf = vec![0u8; 4 << 20];
        let (mut done, mut shown) = (0u64, 0u64);
        loop {
            let n = from.read(&mut buf)?;
            if n == 0 {
                break;
            }
            dev.write_all_at(&buf[..n], done)?;
            done += n as u64;
            let pct = done * 100 / size.max(1);
            if pct >= shown + 10 {
                shown = pct - pct % 10;
                out!("  {shown}%");
            }
        }
        if !zstd.wait()?.success() || done != size {
            bail!("the disk image is damaged ({done} of {size} bytes)");
        }
        let tail: u64 = 1 << 20;
        if disk.bytes >= size + tail {
            dev.write_all_at(&vec![0u8; tail as usize], disk.bytes - tail)?;
        }
        dev.sync_all()?;
        Ok(())
    })();
    let _ = std::process::Command::new("umount").arg(iso).status();
    result?;
    out!("TERMOS-INSTALL done /dev/{name}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(sys: &Path, name: &str, sectors: u64, model: &str, removable: bool) {
        let d = sys.join(name);
        std::fs::create_dir_all(d.join("device")).expect("mkdir");
        std::fs::write(d.join("size"), format!("{sectors}\n")).expect("w");
        std::fs::write(d.join("device/model"), format!("{model}\n")).expect("w");
        std::fs::write(d.join("removable"), if removable { "1\n" } else { "0\n" }).expect("w");
    }

    #[test]
    fn disks_are_the_real_ones() {
        let dir = tempfile::tempdir().expect("dir");
        let sys = dir.path();
        block(sys, "sda", 1_000_215_216, "Samsung SSD", false);
        block(sys, "nvme0n1", 2_000_409_264, "WD Black", false);
        block(sys, "sdb", 61_440_000, "USB stick", true);
        block(sys, "loop0", 100, "", false);
        block(sys, "sr0", 300_000, "QEMU DVD", true);
        block(sys, "zram0", 100, "", false);
        block(sys, "sdc", 0, "Card reader", true);
        let found = disks(sys);
        let names: Vec<&str> = found.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["nvme0n1", "sda", "sdb"]);
        assert_eq!(found[1].bytes, 1_000_215_216 * 512);
        assert_eq!(found[1].model, "Samsung SSD");
        assert!(found[2].removable);
        assert!(disks(&sys.join("missing")).is_empty());
    }

    #[test]
    fn names_and_partitions() {
        assert_eq!(disk_name("/dev/sda\n"), "sda");
        assert_eq!(disk_name("nvme0n1"), "nvme0n1");
        let dir = tempfile::tempdir().expect("dir");
        let class = dir.path().join("class");
        let devices = dir.path().join("devices/sdb");
        std::fs::create_dir_all(devices.join("sdb1")).expect("mkdir");
        std::fs::write(devices.join("sdb1/partition"), "1\n").expect("w");
        std::fs::create_dir_all(&class).expect("mkdir");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(devices.join("sdb1"), class.join("sdb1")).expect("link");
            assert_eq!(
                disk_of(Path::new("/dev/sdb1"), &class).as_deref(),
                Some("sdb")
            );
        }
        assert_eq!(
            disk_of(Path::new("/dev/sr0"), &class).as_deref(),
            Some("sr0")
        );
    }
}
