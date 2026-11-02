// SPDX-License-Identifier: GPL-3.0-or-later
//! The Raspberry Pi's A/B boot for RAUC (D-078): RAUC's custom boot-loader
//! backend over the firmware's tryboot.
//!
//! Both slots' kernels, device trees, overlays and command lines live on
//! the boot partition, under `slot-a/` and `slot-b/`; `config.txt`'s
//! `os_prefix` picks the default one. After RAUC installs a slot it makes
//! it primary: that writes `tryboot.txt` (the same configuration with the
//! new slot's prefix), which the firmware reads instead of `config.txt` for
//! one boot only, after a restart with `reboot "0 tryboot"`. When that slot
//! is marked good it becomes the default in `config.txt`. If it never comes
//! up, the next boot is the old default again, whose marking as good records
//! the tried slot as bad.
//!
//! `termos-ab.txt` on the boot partition keeps the state: `primary=` (the
//! default), `try=` (installed, not yet good), `tried=` (restarted into
//! once), and `A=`/`B=` (`good` or `bad`).
//!
//! The firmware cannot run in QEMU, so this is tested here on files only,
//! until it is tried on a board.
// Only `termos system` on Linux uses it; the tests run everywhere.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};

pub(crate) const STATE: &str = "termos-ab.txt";

/// The boot partition (`/boot/esp` on the system).
#[derive(Debug, Clone)]
pub(crate) struct Boot {
    pub(crate) dir: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct State(BTreeMap<String, String>);

impl State {
    fn parse(text: &str) -> Self {
        Self(
            text.lines()
                .filter_map(|l| l.split_once('='))
                .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
                .collect(),
        )
    }

    fn render(&self) -> String {
        self.0.iter().map(|(k, v)| format!("{k}={v}\n")).collect()
    }

    fn get(&self, k: &str) -> Option<&str> {
        self.0.get(k).map(String::as_str).filter(|v| !v.is_empty())
    }

    fn set(&mut self, k: &str, v: &str) {
        self.0.insert(k.to_owned(), v.to_owned());
    }

    fn clear(&mut self, k: &str) {
        self.0.remove(k);
    }
}

/// `A` or `B`.
fn check_slot(slot: &str) -> anyhow::Result<&str> {
    match slot {
        "A" | "B" => Ok(slot),
        other => bail!("no slot `{other}` (A or B)"),
    }
}

/// The folder a slot's boot files are in, `slot-a/`.
pub(crate) fn prefix(slot: &str) -> String {
    format!("slot-{}/", slot.to_ascii_lowercase())
}

/// `config` with its `os_prefix` set to `prefix` (added if missing).
pub(crate) fn with_prefix(config: &str, prefix: &str) -> String {
    let mut out = String::new();
    let mut done = false;
    for line in config.lines() {
        if line.trim_start().starts_with("os_prefix=") {
            if !done {
                out.push_str(&format!("os_prefix={prefix}\n"));
                done = true;
            }
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !done {
        out.push_str(&format!("os_prefix={prefix}\n"));
    }
    out
}

impl Boot {
    pub(crate) fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn state(&self) -> anyhow::Result<State> {
        let f = self.dir.join(STATE);
        match std::fs::read_to_string(&f) {
            Ok(t) => Ok(State::parse(&t)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(State::parse("primary=A\nA=good\n"))
            }
            Err(e) => Err(e).with_context(|| f.display().to_string()),
        }
    }

    /// Writes a file on the FAT partition: a new file, then renamed over.
    fn write(&self, name: &str, text: &str) -> anyhow::Result<()> {
        let f = self.dir.join(name);
        let tmp = self.dir.join(format!("{name}.new"));
        std::fs::write(&tmp, text).with_context(|| tmp.display().to_string())?;
        std::fs::rename(&tmp, &f).with_context(|| f.display().to_string())
    }

    fn save(&self, s: &State) -> anyhow::Result<()> {
        self.write(STATE, &s.render())
    }

    fn config(&self) -> anyhow::Result<String> {
        let f = self.dir.join("config.txt");
        std::fs::read_to_string(&f).with_context(|| f.display().to_string())
    }

    /// The slot the next restart boots: the one being tried, else the
    /// default.
    pub(crate) fn get_primary(&self) -> anyhow::Result<String> {
        let s = self.state()?;
        Ok(s.get("try").or(s.get("primary")).unwrap_or("A").to_owned())
    }

    /// Makes `slot` primary: the default itself, or the slot to try on
    /// the next (tryboot) restart.
    pub(crate) fn set_primary(&self, slot: &str) -> anyhow::Result<()> {
        let slot = check_slot(slot)?;
        let mut s = self.state()?;
        if s.get("primary") == Some(slot) {
            s.clear("try");
            s.clear("tried");
            let _ = std::fs::remove_file(self.dir.join("tryboot.txt"));
        } else {
            self.write("tryboot.txt", &with_prefix(&self.config()?, &prefix(slot)))?;
            s.set("try", slot);
            s.clear("tried");
        }
        s.set(slot, "good");
        self.save(&s)
    }

    pub(crate) fn get_state(&self, slot: &str) -> anyhow::Result<&'static str> {
        let slot = check_slot(slot)?;
        let s = self.state()?;
        Ok(match s.get(slot) {
            Some("good") => "good",
            Some(_) => "bad",
            None if s.get("primary") == Some(slot) => "good",
            None => "bad",
        })
    }

    /// Marks `slot`; `booted` is the slot running now.
    pub(crate) fn set_state(
        &self,
        slot: &str,
        good: bool,
        booted: Option<&str>,
    ) -> anyhow::Result<()> {
        let slot = check_slot(slot)?;
        let mut s = self.state()?;
        s.set(slot, if good { "good" } else { "bad" });
        let trying = s.get("try").map(str::to_owned);
        let tried = s.get("tried").is_some();
        let finish = |s: &mut State| {
            s.clear("try");
            s.clear("tried");
            let _ = std::fs::remove_file(self.dir.join("tryboot.txt"));
        };
        match trying.as_deref() {
            // The tried slot came up: it is the default from now on.
            Some(t) if good && t == slot && booted == Some(slot) => {
                self.write("config.txt", &with_prefix(&self.config()?, &prefix(slot)))?;
                s.set("primary", slot);
                finish(&mut s);
            }
            // We restarted into the tried slot and are back on the old one:
            // it failed.
            Some(t) if tried && booted != Some(t) => {
                s.set(t, "bad");
                finish(&mut s);
            }
            // The slot waiting to be tried is bad after all.
            Some(t) if !good && t == slot => finish(&mut s),
            _ => {}
        }
        self.save(&s)
    }

    /// A slot waiting to be tried, for the restart (D-078).
    pub(crate) fn pending(&self) -> anyhow::Result<Option<String>> {
        Ok(self.state()?.get("try").map(str::to_owned))
    }

    /// Records that the next restart is the try.
    pub(crate) fn mark_tried(&self) -> anyhow::Result<()> {
        let mut s = self.state()?;
        if let Some(t) = s.get("try").map(str::to_owned) {
            s.set("tried", &t);
        }
        self.save(&s)
    }

    /// Puts a slot's boot files, from its root file system mounted at
    /// `root`, under `slot-<x>/`: the kernel, device trees, overlays, and
    /// the command line from the image's template (`@PART@`, `@SLOT@`).
    pub(crate) fn install_slot(&self, slot: &str, root: &Path) -> anyhow::Result<()> {
        let slot = check_slot(slot)?;
        let boot = root.join("boot");
        let dst = self.dir.join(prefix(slot).trim_end_matches('/'));
        let new = self
            .dir
            .join(format!("{}.new", prefix(slot).trim_end_matches('/')));
        let _ = std::fs::remove_dir_all(&new);
        std::fs::create_dir_all(&new).with_context(|| new.display().to_string())?;
        let mut kernel = false;
        for entry in std::fs::read_dir(&boot).with_context(|| boot.display().to_string())? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let from = entry.path();
            if name == "overlays" && from.is_dir() {
                copy_dir(&from, &new.join("overlays"))?;
            } else if name == "Image" || name.ends_with(".dtb") {
                std::fs::copy(&from, new.join(&*name))
                    .with_context(|| from.display().to_string())?;
                kernel |= name == "Image";
            }
        }
        if !kernel {
            bail!("{}: no kernel (Image) in the new slot", boot.display());
        }
        let template = root.join("usr/share/termos/rpi-cmdline.txt");
        let cmdline = std::fs::read_to_string(&template)
            .with_context(|| template.display().to_string())?
            .replace("@PART@", if slot == "A" { "02" } else { "03" })
            .replace("@SLOT@", slot);
        std::fs::write(new.join("cmdline.txt"), cmdline)?;
        let _ = std::fs::remove_dir_all(&dst);
        std::fs::rename(&new, &dst).with_context(|| dst.display().to_string())
    }
}

fn copy_dir(from: &Path, to: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boot() -> (tempfile::TempDir, Boot) {
        let dir = tempfile::tempdir().expect("dir");
        std::fs::write(
            dir.path().join("config.txt"),
            "start_file=start4.elf\nkernel=Image\nos_prefix=slot-a/\ndtoverlay=vc4-kms-v3d\n",
        )
        .expect("config");
        let b = Boot::new(dir.path());
        (dir, b)
    }

    fn read(b: &Boot, f: &str) -> String {
        std::fs::read_to_string(b.dir.join(f)).unwrap_or_default()
    }

    #[test]
    fn prefixes_are_replaced_or_added() {
        assert_eq!(
            with_prefix("a=1\nos_prefix=slot-a/\nb=2\n", "slot-b/"),
            "a=1\nos_prefix=slot-b/\nb=2\n"
        );
        assert_eq!(with_prefix("a=1\n", "slot-a/"), "a=1\nos_prefix=slot-a/\n");
    }

    #[test]
    fn a_good_try_becomes_the_default() {
        let (_d, b) = boot();
        assert_eq!(b.get_primary().expect("p"), "A");
        assert_eq!(b.get_state("A").expect("s"), "good");
        assert_eq!(b.get_state("B").expect("s"), "bad");
        // RAUC installs into B: bad while written, then primary.
        b.set_state("B", false, Some("A")).expect("bad");
        b.set_primary("B").expect("primary");
        assert_eq!(b.get_primary().expect("p"), "B");
        assert_eq!(b.pending().expect("p").as_deref(), Some("B"));
        assert!(read(&b, "tryboot.txt").contains("os_prefix=slot-b/\n"));
        assert!(
            read(&b, "config.txt").contains("os_prefix=slot-a/\n"),
            "the default stays"
        );
        b.mark_tried().expect("tried");
        // B came up and is marked good.
        b.set_state("B", true, Some("B")).expect("good");
        assert!(read(&b, "config.txt").contains("os_prefix=slot-b/\n"));
        assert!(!b.dir.join("tryboot.txt").exists());
        assert_eq!(b.get_primary().expect("p"), "B");
        assert_eq!(b.pending().expect("p"), None);
    }

    #[test]
    fn a_failed_try_is_bad_and_a_power_off_is_not_a_try() {
        let (_d, b) = boot();
        b.set_primary("B").expect("primary");
        // Turned off instead of restarted: A boots and is good; B still waits.
        b.set_state("A", true, Some("A")).expect("good");
        assert_eq!(b.pending().expect("p").as_deref(), Some("B"));
        assert_eq!(b.get_state("B").expect("s"), "good");
        // Restarted into B, which never came up: back on A.
        b.mark_tried().expect("tried");
        b.set_state("A", true, Some("A")).expect("good");
        assert_eq!(b.pending().expect("p"), None);
        assert_eq!(b.get_state("B").expect("s"), "bad");
        assert_eq!(b.get_primary().expect("p"), "A");
        assert!(read(&b, "config.txt").contains("os_prefix=slot-a/\n"));
        assert!(b.set_primary("C").is_err());
    }

    #[test]
    fn a_new_slot_gets_its_boot_files() {
        let (_d, b) = boot();
        let root = tempfile::tempdir().expect("root");
        let r = root.path();
        std::fs::create_dir_all(r.join("boot/overlays")).expect("mkdir");
        std::fs::create_dir_all(r.join("usr/share/termos")).expect("mkdir");
        std::fs::write(r.join("boot/Image"), b"kernel").expect("w");
        std::fs::write(r.join("boot/bcm2711-rpi-4-b.dtb"), b"dtb").expect("w");
        std::fs::write(r.join("boot/overlays/vc4-kms-v3d.dtbo"), b"o").expect("w");
        std::fs::write(r.join("boot/System.map"), b"no").expect("w");
        std::fs::write(
            r.join("usr/share/termos/rpi-cmdline.txt"),
            "root=PARTUUID=7e57c0de-@PART@ rauc.slot=@SLOT@ ro\n",
        )
        .expect("w");
        b.install_slot("B", r).expect("install");
        let s = b.dir.join("slot-b");
        assert_eq!(std::fs::read(s.join("Image")).expect("k"), b"kernel");
        assert!(s.join("bcm2711-rpi-4-b.dtb").is_file());
        assert!(s.join("overlays/vc4-kms-v3d.dtbo").is_file());
        assert!(!s.join("System.map").exists());
        assert_eq!(
            std::fs::read_to_string(s.join("cmdline.txt")).expect("c"),
            "root=PARTUUID=7e57c0de-03 rauc.slot=B ro\n"
        );
        std::fs::remove_file(r.join("boot/Image")).expect("rm");
        assert!(b.install_slot("A", r).is_err(), "no kernel, no slot");
        assert!(!b.dir.join("slot-a").exists());
    }
}
