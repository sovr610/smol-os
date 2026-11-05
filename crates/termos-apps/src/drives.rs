// SPDX-License-Identifier: GPL-3.0-or-later
//! USB drives in Files (M13d, D-124): mounting and ejecting.
//!
//! On a TermOS image `sudo termos system mount DEV` mounts on
//! `/media/LABEL` (and `unmount` ejects); on another Linux, udisks does it
//! for the user (`udisksctl`), where it runs. macOS and Windows mount
//! drives themselves: Files lists their volumes as places.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use termos_hw::drives::Drive;

/// Whether this is a TermOS image.
fn on_image() -> bool {
    Path::new("/etc/termos/board").exists()
}

/// Where `udisksctl mount` says it mounted: `Mounted /dev/sdb1 at
/// /media/me/STICK` (older versions end with a dot).
pub fn udisks_mounted_at(out: &str) -> Option<PathBuf> {
    let line = out.lines().find(|l| l.starts_with("Mounted "))?;
    let (_, at) = line.split_once(" at ")?;
    let at = at.trim().trim_end_matches('.');
    (!at.is_empty()).then(|| PathBuf::from(at))
}

fn output(program: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim();
        Err(if err.is_empty() {
            format!("{program} failed ({})", out.status)
        } else {
            err.lines().last().unwrap_or(err).to_owned()
        })
    }
}

/// Mounts `drive`; where it is now. Blocks: run it on a thread.
pub fn mount(drive: &Drive) -> Result<PathBuf, String> {
    let dev = drive.device.to_string_lossy().into_owned();
    if on_image() {
        let out = output("sudo", &["-n", "/usr/bin/termos", "system", "mount", &dev])?;
        return out
            .lines()
            .last()
            .map(|l| PathBuf::from(l.trim()))
            .filter(|p| p.is_absolute())
            .ok_or_else(|| "the drive mounted, but where was not said".to_owned());
    }
    let out = output("udisksctl", &["mount", "--no-user-interaction", "-b", &dev])
        .map_err(|e| format!("{e} (udisks mounts drives here)"))?;
    udisks_mounted_at(&out).ok_or_else(|| format!("udisksctl said: {}", out.trim()))
}

/// Writes out and unmounts `drive`, then (with udisks) powers it off, so
/// it can be pulled out. Blocks: run it on a thread.
pub fn eject(drive: &Drive) -> Result<(), String> {
    let dev = drive.device.to_string_lossy().into_owned();
    if on_image() {
        output(
            "sudo",
            &["-n", "/usr/bin/termos", "system", "unmount", &dev],
        )?;
        return Ok(());
    }
    if drive.mount.is_some() {
        output(
            "udisksctl",
            &["unmount", "--no-user-interaction", "-b", &dev],
        )?;
    }
    // The whole disk, so it can be pulled out.
    let _ = output(
        "udisksctl",
        &[
            "power-off",
            "--no-user-interaction",
            "-b",
            &format!("/dev/{}", drive.disk),
        ],
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn udisks_says_where() {
        assert_eq!(
            udisks_mounted_at("Mounted /dev/sdb1 at /media/me/MY STICK\n"),
            Some(PathBuf::from("/media/me/MY STICK"))
        );
        assert_eq!(
            udisks_mounted_at("Mounted /dev/sdb1 at /run/media/me/STICK.\n"),
            Some(PathBuf::from("/run/media/me/STICK"))
        );
        assert_eq!(udisks_mounted_at("Error mounting\n"), None);
    }
}
