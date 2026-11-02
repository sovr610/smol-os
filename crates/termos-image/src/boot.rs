// SPDX-License-Identifier: GPL-3.0-or-later
//! `cargo xtask image boot <target>`: the QEMU boot test (D-076).
//!
//! The image boots (on a throwaway snapshot) with virtio-gpu as its only
//! display, a virtio keyboard and tablet, and the serial console in a log.
//! The test passes when:
//! 1. the console prints `TERMOS-READY` (its first frame is on the screen);
//! 2. a screenshot of the display shows the desktop: not blank, with the
//!    status bar row at the bottom;
//! 3. the image reports `MemTotal − MemAvailable` after settling, under
//!    the budget (150 MB, D-019). QEMU asks for that report through
//!    `fw_cfg` (`opt/termos/boottest`, the seconds to wait), so the image
//!    itself is not changed for the test.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::qmp::Qmp;
use crate::screen;
use crate::serial;
use crate::targets::{Firmware, Qemu, Target};

/// The idle-RAM budget (D-019): 150 MB.
pub const RAM_BUDGET_KB: u64 = 150 * 1024;

/// Where QEMU's firmware files usually are (Debian and Ubuntu packages
/// `ovmf` and `qemu-efi-aarch64`); `TERMOS_OVMF` / `TERMOS_AAVMF` win.
pub fn firmware_file(q: &Qemu, f: Firmware) -> Result<Option<PathBuf>, String> {
    if f == Firmware::Bios {
        return Ok(None);
    }
    let (var, candidates): (&str, &[&str]) = if q.system.ends_with("aarch64") {
        (
            "TERMOS_AAVMF",
            &[
                "/usr/share/qemu-efi-aarch64/QEMU_EFI.fd",
                "/usr/share/AAVMF/AAVMF_CODE.fd",
            ],
        )
    } else {
        (
            "TERMOS_OVMF",
            &["/usr/share/ovmf/OVMF.fd", "/usr/share/qemu/OVMF.fd"],
        )
    };
    if let Some(p) = std::env::var_os(var) {
        return Ok(Some(PathBuf::from(p)));
    }
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
        .map(Some)
        .ok_or_else(|| format!("no UEFI firmware for {}: install it or set {var}", q.system))
}

/// QEMU's command line.
pub fn qemu_args(
    q: &Qemu,
    firmware: Option<&Path>,
    disk: &Path,
    serial_log: &Path,
    dir: &Path,
    settle_secs: u32,
    kvm: bool,
) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "-M".into(),
        q.machine.into(),
        "-cpu".into(),
        if kvm { "host".into() } else { q.cpu.into() },
        "-m".into(),
        "1024".into(),
        "-smp".into(),
        "2".into(),
    ];
    if kvm {
        a.extend(["-accel".into(), "kvm".into()]);
    }
    if let Some(f) = firmware {
        a.extend(["-bios".into(), f.display().to_string()]);
    }
    a.extend([
        "-drive".into(),
        format!("file={},format=raw,if=virtio,snapshot=on", disk.display()),
        "-vga".into(),
        "none".into(),
        "-device".into(),
        "virtio-gpu-pci".into(),
        "-device".into(),
        "virtio-keyboard-pci".into(),
        "-device".into(),
        "virtio-tablet-pci".into(),
        "-nic".into(),
        "user,model=virtio-net-pci".into(),
        "-display".into(),
        "none".into(),
        "-serial".into(),
        format!("file:{}", serial_log.display()),
        "-qmp".into(),
        format!("unix:{},server=on,wait=off", dir.join("qmp.sock").display()),
        "-fw_cfg".into(),
        format!("name=opt/termos/boottest,string={settle_secs}"),
        "-no-reboot".into(),
    ]);
    a
}

/// Whether QEMU can use KVM for this machine here.
fn kvm_usable(q: &Qemu) -> bool {
    let same_arch = (q.system.ends_with("x86_64") && cfg!(target_arch = "x86_64"))
        || (q.system.ends_with("aarch64") && cfg!(target_arch = "aarch64"));
    same_arch
        && std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/kvm")
            .is_ok()
}

/// What a boot showed.
#[derive(Debug)]
pub struct Report {
    pub firmware: Firmware,
    pub ready_after: Duration,
    pub screen: screen::Findings,
    pub memory: serial::Memory,
}

struct Vm(Child);

impl Drop for Vm {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_for<T>(
    what: &str,
    timeout: Duration,
    vm: &mut Vm,
    mut check: impl FnMut() -> Option<T>,
) -> Result<T, String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(v) = check() {
            return Ok(v);
        }
        if let Ok(Some(status)) = vm.0.try_wait() {
            return Err(format!("QEMU stopped ({status}) before {what}"));
        }
        if Instant::now() > deadline {
            return Err(format!("no {what} within {}s", timeout.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Takes a screenshot through QMP: `screen-<name>.png` in `dir`.
fn screenshot(dir: &Path, name: &str) -> Result<screen::Picture, String> {
    let mut qmp = Qmp::connect(&dir.join("qmp.sock"))?;
    let ppm = dir.join(format!("screen-{name}.ppm"));
    qmp.run("screendump", &[("filename", &ppm.display().to_string())])?;
    let picture = screen::parse_ppm(&std::fs::read(&ppm).map_err(|e| e.to_string())?)?;
    let _ = std::fs::remove_file(&ppm);
    std::fs::write(
        dir.join(format!("screen-{name}.png")),
        screen::png(&picture),
    )
    .map_err(|e| e.to_string())?;
    Ok(picture)
}

/// Boots `t`'s image with `firmware`; artifacts (serial log, screenshot)
/// go to `dir`.
pub fn boot(images: &Path, t: &Target, firmware: Firmware, dir: &Path) -> Result<Report, String> {
    let q = t
        .qemu
        .ok_or_else(|| format!("{} has no QEMU machine (a real board)", t.name))?;
    let disk = images.join("disk.img");
    if !disk.is_file() {
        return Err(format!(
            "no image at {} (`cargo xtask image build {}`)",
            disk.display(),
            t.name
        ));
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let log = dir.join(format!("serial-{}.log", firmware.name()));
    let _ = std::fs::remove_file(&log);
    let _ = std::fs::remove_file(dir.join("qmp.sock"));
    let fw = firmware_file(&q, firmware)?;
    let kvm = kvm_usable(&q);
    // Emulated (no KVM) boots are slower: give the desktop longer to settle.
    let (settle, ready_timeout) = if kvm { (30, 180) } else { (60, 600) };
    let args = qemu_args(&q, fw.as_deref(), &disk, &log, dir, settle, kvm);
    println!("image: {} {}", q.system, args.join(" "));
    let started = Instant::now();
    let mut vm = Vm(Command::new(q.system)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", q.system))?);
    let read_log = || {
        std::fs::read(&log)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default()
    };
    // Whatever goes wrong, the end of the serial log says the most, and
    // the screen shows what the machine got to.
    let failed = |e: String| {
        let shown = match screenshot(dir, &format!("{}-failed", firmware.name())) {
            Ok(_) => format!("screen-{}-failed.png", firmware.name()),
            Err(s) => format!("no screenshot: {s}"),
        };
        format!(
            "{e}\n({shown}; the serial log ends with:)\n{}",
            serial::tail(&read_log(), 60)
        )
    };
    wait_for(
        "TERMOS-READY on the serial console",
        Duration::from_secs(ready_timeout),
        &mut vm,
        || serial::ready(&read_log()),
    )
    .map_err(failed)?;
    let ready_after = started.elapsed();
    // A moment for the frames after the first.
    std::thread::sleep(Duration::from_secs(5));
    let picture = screenshot(dir, firmware.name()).map_err(failed)?;
    let findings = screen::inspect(&picture);
    let memory = wait_for(
        "TERMOS-MEM report",
        Duration::from_secs(u64::from(settle) + 120),
        &mut vm,
        || serial::memory(&read_log()),
    )
    .map_err(failed)?;
    if let Ok(mut qmp) = Qmp::connect(&dir.join("qmp.sock")) {
        let _ = qmp.run("quit", &[]);
    }
    Ok(Report {
        firmware,
        ready_after,
        screen: findings,
        memory,
    })
}

impl Report {
    /// What failed, if anything.
    pub fn problems(&self) -> Vec<String> {
        let mut p = self.screen.problems();
        if self.memory.used_kb > RAM_BUDGET_KB {
            p.push(format!(
                "{} MiB in use after settling, over the {} MiB budget",
                self.memory.used_kb / 1024,
                RAM_BUDGET_KB / 1024
            ));
        }
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::targets::find;

    #[test]
    fn qemu_command_lines() {
        let q = find("aarch64-virt").expect("t").qemu.expect("qemu");
        let a = qemu_args(
            &q,
            Some(Path::new("/fw/QEMU_EFI.fd")),
            Path::new("/i/disk.img"),
            Path::new("/w/serial-uefi.log"),
            Path::new("/w"),
            60,
            false,
        );
        let s = a.join(" ");
        assert!(s.starts_with("-M virt -cpu cortex-a72 -m 1024"), "{s}");
        assert!(s.contains("-bios /fw/QEMU_EFI.fd"), "{s}");
        assert!(
            s.contains("file=/i/disk.img,format=raw,if=virtio,snapshot=on"),
            "{s}"
        );
        assert!(s.contains("-vga none -device virtio-gpu-pci"), "{s}");
        assert!(s.contains("-serial file:/w/serial-uefi.log"), "{s}");
        assert!(
            s.contains("-fw_cfg name=opt/termos/boottest,string=60"),
            "{s}"
        );
        assert!(!s.contains("kvm"));
        let x = find("x86_64").expect("t").qemu.expect("qemu");
        let b = qemu_args(
            &x,
            None,
            Path::new("/d"),
            Path::new("/w/serial-bios.log"),
            Path::new("/w"),
            30,
            true,
        );
        let bs = b.join(" ");
        assert!(
            bs.contains("-cpu host") && bs.contains("-accel kvm"),
            "{bs}"
        );
        assert!(!b.iter().any(|a| a == "-bios"), "SeaBIOS is the default");
        assert_eq!(firmware_file(&x, Firmware::Bios), Ok(None));
    }
}
