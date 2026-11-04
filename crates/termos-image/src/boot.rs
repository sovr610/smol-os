// SPDX-License-Identifier: GPL-3.0-or-later
//! `cargo xtask image boot <target>`: the QEMU boot tests (D-076, D-078,
//! D-080).
//!
//! The image boots from a copy-on-write overlay one GiB bigger than itself,
//! with virtio-gpu as its only display, a virtio keyboard and tablet, and
//! the serial console in a log. The test passes when:
//! 1. the console prints `TERMOS-READY` (its first frame is on the screen);
//! 2. a screenshot of the display shows the desktop: not blank, with the
//!    status bar row at the bottom;
//! 3. the image reports `MemTotal − MemAvailable` after settling, under
//!    the budget (150 MB, D-019);
//! 4. the data partition has grown into the extra space (D-080).
//!
//! QEMU asks for those reports through `fw_cfg` (`opt/termos/boottest`, the
//! seconds to wait), so the image itself is not changed for the test.
//!
//! With `--update` ([`update_test`]), one disk then goes through an A/B
//! update (D-078): a bundle with a new version is served over HTTP, the
//! desktop's user installs it with `termos update`, and the next boot runs
//! it from slot B; then a broken bundle (no `init`) goes into slot A, its
//! boot panics, and the boot after that is back on slot B.

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::qmp::Qmp;
use crate::screen;
use crate::serial;
use crate::targets::{Firmware, Qemu, Target};

/// The idle-RAM budget (D-019): 150 MB.
pub const RAM_BUDGET_KB: u64 = 150 * 1024;

/// How much bigger than the image the test disk is: the data partition
/// (512 MiB in the image) must grow into it.
pub const GROW_BY_MIB: u64 = 1024;

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

/// A machine's disk (qcow2) and CD (the live ISO).
#[derive(Debug, Clone, Copy)]
pub struct Media<'a> {
    pub disk: &'a Path,
    pub cdrom: Option<&'a Path>,
}

/// QEMU's command line. `cfg` are the `fw_cfg` requests
/// (`opt/termos/<name>`).
pub fn qemu_args(
    q: &Qemu,
    firmware: Option<&Path>,
    media: Media<'_>,
    serial_log: &Path,
    dir: &Path,
    cfg: &[(&str, String)],
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
        format!("file={},format=qcow2,if=virtio", media.disk.display()),
    ]);
    // The live ISO, started from first (the disk is empty).
    if let Some(iso) = media.cdrom {
        a.extend([
            "-drive".into(),
            format!(
                "file={},media=cdrom,if=none,id=cd,readonly=on",
                iso.display()
            ),
            "-device".into(),
            "ide-cd,drive=cd,bootindex=0".into(),
        ]);
    }
    a.extend([
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
    ]);
    for (name, value) in cfg {
        // fw_cfg's option syntax doubles commas in values.
        a.extend([
            "-fw_cfg".into(),
            format!("name=opt/termos/{name},string={}", value.replace(',', ",,")),
        ]);
    }
    // A restart ends QEMU: the update test starts it again on the same disk.
    a.push("-no-reboot".into());
    a
}

/// A copy-on-write overlay of `image`, `GROW_BY_MIB` bigger.
pub fn overlay(image: &Path, file: &Path) -> Result<(), String> {
    let size = std::fs::metadata(image)
        .map_err(|e| format!("{}: {e}", image.display()))?
        .len()
        + GROW_BY_MIB * 1024 * 1024;
    let _ = std::fs::remove_file(file);
    let image = std::path::absolute(image).map_err(|e| e.to_string())?;
    let out = Command::new("qemu-img")
        .args(["create", "-q", "-f", "qcow2", "-F", "raw", "-b"])
        .arg(&image)
        .arg(file)
        .arg(size.to_string())
        .output()
        .map_err(|e| format!("cannot run qemu-img (qemu-utils): {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "qemu-img create failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
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
    /// The data file system's size, KiB.
    pub data_kb: u64,
    /// How the booted slot was marked (D-078).
    pub slot: Option<serial::Slot>,
    /// curl's version line (D-109).
    pub curl: Option<String>,
    /// Whether the desktop's user can make the dev rootfs (D-110).
    pub devroot: Option<String>,
    /// The end of the serial log, shown when something failed.
    pub tail: String,
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

/// One QEMU machine for a target, on one overlay disk.
struct Machine<'a> {
    q: Qemu,
    firmware_file: Option<PathBuf>,
    disk: PathBuf,
    cdrom: Option<PathBuf>,
    dir: &'a Path,
    kvm: bool,
}

/// A started machine and its serial log.
struct Started {
    vm: Vm,
    log: PathBuf,
    started: Instant,
}

impl Started {
    fn log(&self) -> String {
        std::fs::read(&self.log)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default()
    }

    /// `e`, with the end of the serial log and a screenshot of what the
    /// machine got to (`screen-<name>-failed.png`).
    fn failed(&self, dir: &Path, name: &str, e: String) -> String {
        let shown = match screenshot(dir, &format!("{name}-failed")) {
            Ok(_) => format!("screen-{name}-failed.png"),
            Err(s) => format!("no screenshot: {s}"),
        };
        format!(
            "{e}\n({shown}; the serial log ends with:)\n{}",
            serial::tail(&self.log(), 60)
        )
    }

    /// Waits for `check` on the serial log.
    fn wait<T>(
        &mut self,
        what: &str,
        timeout: Duration,
        mut check: impl FnMut(&str) -> Option<T>,
    ) -> Result<T, String> {
        let log = self.log.clone();
        wait_for(what, timeout, &mut self.vm, || {
            let text = std::fs::read(&log)
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default();
            check(&text)
        })
    }

    /// Waits for QEMU to end (the guest restarted or panicked).
    fn wait_exit(&mut self, what: &str, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(Some(_)) = self.vm.0.try_wait() {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(format!("no {what} within {}s", timeout.as_secs()));
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    fn quit(&self, dir: &Path) {
        if let Ok(mut qmp) = Qmp::connect(&dir.join("qmp.sock")) {
            let _ = qmp.run("quit", &[]);
        }
    }
}

impl<'a> Machine<'a> {
    fn new(images: &Path, t: &Target, firmware: Firmware, dir: &'a Path) -> Result<Self, String> {
        let q = t
            .qemu
            .ok_or_else(|| format!("{} has no QEMU machine (a real board)", t.name))?;
        let image = images.join("disk.img");
        if !image.is_file() {
            return Err(format!(
                "no image at {} (`cargo xtask image build {}`)",
                image.display(),
                t.name
            ));
        }
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let disk = dir.join(format!("disk-{}.qcow2", firmware.name()));
        overlay(&image, &disk)?;
        Ok(Self {
            firmware_file: firmware_file(&q, firmware)?,
            kvm: kvm_usable(&q),
            q,
            disk,
            cdrom: None,
            dir,
        })
    }

    /// Another firmware, the same disk.
    fn with_firmware(&self, firmware: Firmware) -> Result<Self, String> {
        Ok(Self {
            firmware_file: firmware_file(&self.q, firmware)?,
            kvm: self.kvm,
            q: self.q,
            disk: self.disk.clone(),
            cdrom: self.cdrom.clone(),
            dir: self.dir,
        })
    }

    /// How long the desktop settles before the image reports, and how
    /// long to wait for the console: emulated (no KVM) boots are slower.
    fn timing(&self) -> (u32, Duration) {
        if self.kvm {
            (30, Duration::from_secs(180))
        } else {
            (60, Duration::from_secs(600))
        }
    }

    /// Starts QEMU; the serial log is `serial-<name>.log`.
    fn start(&self, name: &str, cfg: &[(&str, String)]) -> Result<Started, String> {
        let log = self.dir.join(format!("serial-{name}.log"));
        let _ = std::fs::remove_file(&log);
        let _ = std::fs::remove_file(self.dir.join("qmp.sock"));
        let args = qemu_args(
            &self.q,
            self.firmware_file.as_deref(),
            Media {
                disk: &self.disk,
                cdrom: self.cdrom.as_deref(),
            },
            &log,
            self.dir,
            cfg,
            self.kvm,
        );
        println!("image: {} {}", self.q.system, args.join(" "));
        let vm = Vm(Command::new(self.q.system)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("cannot start {}: {e}", self.q.system))?);
        Ok(Started {
            vm,
            log,
            started: Instant::now(),
        })
    }
}

/// Boots `t`'s image with `firmware`; artifacts (serial log, screenshot)
/// go to `dir`.
pub fn boot(images: &Path, t: &Target, firmware: Firmware, dir: &Path) -> Result<Report, String> {
    let m = Machine::new(images, t, firmware, dir)?;
    let name = firmware.name();
    let (settle, ready_timeout) = m.timing();
    let mut run = m.start(name, &[("boottest", settle.to_string())])?;
    run.wait(
        "TERMOS-READY on the serial console",
        ready_timeout,
        serial::ready,
    )
    .map_err(|e| run.failed(dir, name, e))?;
    let ready_after = run.started.elapsed();
    // A moment for the frames after the first.
    std::thread::sleep(Duration::from_secs(5));
    let picture = screenshot(dir, name).map_err(|e| run.failed(dir, name, e))?;
    let findings = screen::inspect(&picture);
    let memory = run
        .wait(
            "TERMOS-MEM report",
            Duration::from_secs(u64::from(settle) + 120),
            serial::memory,
        )
        .map_err(|e| run.failed(dir, name, e))?;
    let data_kb = run
        .wait(
            "TERMOS-DATA report",
            Duration::from_secs(30),
            serial::data_kb,
        )
        .map_err(|e| run.failed(dir, name, e))?;
    // The slot is marked a second or two after the first frame, long
    // before the reports above.
    let log = run.log();
    run.quit(dir);
    Ok(Report {
        firmware,
        ready_after,
        screen: findings,
        memory,
        data_kb,
        slot: serial::slot(&log),
        curl: serial::curl(&log),
        devroot: serial::devroot(&log),
        tail: serial::tail(&log, 40),
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
        // The image's data partition is 512 MiB; on a disk GROW_BY_MIB
        // bigger it must have taken the space.
        if self.data_kb < GROW_BY_MIB * 1024 {
            p.push(format!(
                "the data partition did not grow: {} MiB on a disk {GROW_BY_MIB} MiB bigger than the image",
                self.data_kb / 1024
            ));
        }
        // TermOS downloads with the system's curl, built on mbedTLS.
        match &self.curl {
            Some(c) if c.starts_with("curl ") && c.contains("mbedTLS/") => {}
            Some(c) => p.push(format!("curl is not the image's mbedTLS build: {c}")),
            None => p.push("no TERMOS-CURL report".into()),
        }
        // The dev rootfs needs user namespaces with subordinate IDs.
        match &self.devroot {
            Some(d) if d == "bubblewrap: ready" => {}
            Some(d) => p.push(format!("the dev rootfs cannot run: {d}")),
            None => p.push("no TERMOS-DEVROOT report".into()),
        }
        match &self.slot {
            Some(s) if s.good => {}
            Some(s) => p.push(format!("slot {} was not marked good", s.slot)),
            None => p.push("the booted slot was not marked (no TERMOS-SLOT line)".into()),
        }
        p
    }
}

/// Serves the files in `dir` over HTTP on a free local port (QEMU's guests
/// reach it as 10.0.2.2), for as long as the process runs.
pub fn serve(dir: &Path) -> Result<u16, String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("listen: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let dir = dir.to_owned();
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let dir = dir.clone();
            std::thread::spawn(move || {
                let _ = answer(&dir, conn);
            });
        }
    });
    Ok(port)
}

/// One request: `GET /<file>` for a file directly in `dir`.
fn answer(dir: &Path, conn: std::net::TcpStream) -> std::io::Result<()> {
    let mut reader = BufReader::new(conn.try_clone()?);
    let mut request = String::new();
    reader.read_line(&mut request)?;
    // Skip the headers.
    let mut line = String::new();
    while reader.read_line(&mut line)? > 2 {
        line.clear();
    }
    let mut out = conn;
    let name = request
        .strip_prefix("GET /")
        .and_then(|r| r.split(' ').next())
        .filter(|n| !n.is_empty() && !n.contains(['/', '\\']) && !n.starts_with('.'));
    match name.map(|n| dir.join(n)).filter(|p| p.is_file()) {
        Some(path) => {
            let mut file = std::fs::File::open(&path)?;
            let len = file.metadata()?.len();
            write!(
                out,
                "HTTP/1.1 200 OK\r\nContent-Length: {len}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n"
            )?;
            std::io::copy(&mut file, &mut out)?;
        }
        None => write!(
            out,
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )?,
    }
    out.flush()
}

/// The bundles the update test installs: one with a new version, one that
/// cannot boot.
#[derive(Debug, Clone)]
pub struct UpdateBundles {
    pub new: PathBuf,
    pub new_version: String,
    pub broken: PathBuf,
}

/// The A/B update test (D-078) on one disk, with `firmware`. Returns what
/// happened, step by step.
pub fn update_test(
    images: &Path,
    t: &Target,
    firmware: Firmware,
    dir: &Path,
    bundles: &UpdateBundles,
) -> Result<Vec<String>, String> {
    let m = Machine::new(images, t, firmware, dir)?;
    let (_, ready_timeout) = m.timing();
    let served = bundles.new.parent().ok_or("the bundle has no folder")?;
    let port = serve(served)?;
    let url = |b: &Path| -> Result<String, String> {
        let name = b.file_name().ok_or("no bundle name")?.to_string_lossy();
        if b.parent() != Some(served) {
            return Err(format!("{}: the bundles must share a folder", b.display()));
        }
        Ok(format!("http://10.0.2.2:{port}/{name}"))
    };
    let mut steps = Vec::new();
    let install = |name: &str, bundle: &Path| -> Result<String, String> {
        let mut run = m.start(
            name,
            &[("boottest", "5".into()), ("boottest-update", url(bundle)?)],
        )?;
        let into = run
            .wait(
                "TERMOS-UPDATE (termos update install)",
                ready_timeout + Duration::from_secs(300),
                |log| {
                    serial::update_installed(log).map(Ok).or_else(|| {
                        log.contains("TERMOS-UPDATE failed")
                            .then(|| Err("`termos update install` failed".to_owned()))
                    })
                },
            )
            .and_then(|r| r)
            .map_err(|e| run.failed(dir, name, e))?;
        run.wait_exit("restart after the update", Duration::from_secs(120))
            .map_err(|e| run.failed(dir, name, e))?;
        Ok(into)
    };
    let check = |name: &str| -> Result<serial::Slot, String> {
        let mut run = m.start(name, &[])?;
        let slot = run
            .wait(
                "TERMOS-SLOT (the booted slot marked)",
                ready_timeout + Duration::from_secs(60),
                serial::slot,
            )
            .map_err(|e| run.failed(dir, name, e))?;
        run.quit(dir);
        Ok(slot)
    };

    // 1. Slot A installs the new version into slot B and restarts.
    let into = install("update-1-install", &bundles.new)?;
    if into != "B" {
        return Err(format!("the update went into slot {into}, not B"));
    }
    steps.push(format!(
        "slot A installed {} into slot B",
        bundles.new_version
    ));
    // 2. Slot B runs it and is marked good.
    let b = check("update-2-new")?;
    if b.slot != "B" || b.version != bundles.new_version || !b.good {
        return Err(format!(
            "after the update: {b:?}, expected slot B with {} marked good",
            bundles.new_version
        ));
    }
    steps.push(format!("slot B booted {} and was marked good", b.version));
    // 3. A broken update goes into slot A.
    let into = install("update-3-broken", &bundles.broken)?;
    if into != "A" {
        return Err(format!("the broken update went into slot {into}, not A"));
    }
    steps.push("slot B installed a bundle without init into slot A".into());
    // 4. Slot A is tried once: the kernel finds no init, panics and
    //    restarts.
    let mut run = m.start("update-4-panic", &[])?;
    run.wait_exit("restart after the failed boot", ready_timeout)
        .map_err(|e| run.failed(dir, "update-4-panic", e))?;
    if run.log().contains("TERMOS-READY") {
        return Err("the broken slot brought the console up".into());
    }
    steps.push("slot A failed to boot (no init) and the machine restarted".into());
    // 5. The next boot falls back to slot B.
    let back = check("update-5-fallback")?;
    if back.slot != "B" || back.version != bundles.new_version || !back.good {
        return Err(format!(
            "after the failed update: {back:?}, expected slot B again"
        ));
    }
    steps.push("the next boot fell back to slot B".into());
    Ok(steps)
}

/// How big the empty disk of the install test is.
pub const INSTALL_DISK_GIB: u64 = 8;

/// The install test (D-080): the live ISO boots (UEFI) with an empty disk
/// and installs onto it; the ISO also boots with SeaBIOS; then the disk
/// boots alone, with UEFI and with BIOS, to the desktop, its data partition
/// grown to the disk and slot A marked good.
pub fn install_test(images: &Path, t: &Target, dir: &Path) -> Result<Vec<String>, String> {
    let iso = images.join(format!("termos-{}.iso", t.name));
    if !iso.is_file() {
        return Err(format!("no live ISO at {}", iso.display()));
    }
    let q = t
        .qemu
        .ok_or_else(|| format!("{} has no QEMU machine", t.name))?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let disk = dir.join("disk-install.qcow2");
    let _ = std::fs::remove_file(&disk);
    let made = Command::new("qemu-img")
        .args(["create", "-q", "-f", "qcow2"])
        .arg(&disk)
        .arg(format!("{INSTALL_DISK_GIB}G"))
        .status()
        .map_err(|e| format!("cannot run qemu-img (qemu-utils): {e}"))?;
    if !made.success() {
        return Err("qemu-img create failed".into());
    }
    let live = Machine {
        firmware_file: firmware_file(&q, Firmware::Uefi)?,
        kvm: kvm_usable(&q),
        q,
        disk: disk.clone(),
        cdrom: Some(iso),
        dir,
    };
    let (_, ready_timeout) = live.timing();
    let mut steps = Vec::new();
    // The ISO with SeaBIOS: the live desktop comes up.
    if q.firmwares.contains(&Firmware::Bios) {
        let bios = live.with_firmware(Firmware::Bios)?;
        let mut run = bios.start("live-bios", &[])?;
        run.wait(
            "TERMOS-READY from the ISO with SeaBIOS",
            ready_timeout,
            serial::ready,
        )
        .map_err(|e| run.failed(dir, "live-bios", e))?;
        screenshot(dir, "live-bios").map_err(|e| run.failed(dir, "live-bios", e))?;
        run.quit(dir);
        steps.push("the ISO booted to the live desktop with SeaBIOS".into());
    }
    // The ISO with UEFI installs onto the empty disk and turns off.
    let mut run = live.start(
        "live-install",
        &[("boottest", "5".into()), ("boottest-install", "vda".into())],
    )?;
    run.wait("TERMOS-READY from the ISO", ready_timeout, serial::ready)
        .map_err(|e| run.failed(dir, "live-install", e))?;
    screenshot(dir, "live-uefi").map_err(|e| run.failed(dir, "live-install", e))?;
    let done = run
        .wait(
            "TERMOS-INSTALL (termos install)",
            Duration::from_secs(600),
            serial::install_done,
        )
        .map_err(|e| run.failed(dir, "live-install", e))?;
    if !done {
        return Err(run.failed(dir, "live-install", "`termos install` failed".into()));
    }
    run.wait_exit("power-off after installing", Duration::from_secs(120))
        .map_err(|e| run.failed(dir, "live-install", e))?;
    steps.push(format!(
        "the ISO installed TermOS onto an empty {INSTALL_DISK_GIB} GiB disk (UEFI)"
    ));
    // The disk alone, with each firmware.
    let installed = Machine {
        cdrom: None,
        ..live
    };
    for f in q.firmwares {
        let m = installed.with_firmware(*f)?;
        let name = format!("installed-{}", f.name());
        let mut run = m.start(&name, &[("boottest", "10".into())])?;
        run.wait(
            "TERMOS-READY from the installed disk",
            ready_timeout,
            serial::ready,
        )
        .map_err(|e| run.failed(dir, &name, e))?;
        let slot = run
            .wait("TERMOS-SLOT", Duration::from_secs(120), serial::slot)
            .map_err(|e| run.failed(dir, &name, e))?;
        let data = run
            .wait("TERMOS-DATA", Duration::from_secs(120), serial::data_kb)
            .map_err(|e| run.failed(dir, &name, e))?;
        screenshot(dir, &name).map_err(|e| run.failed(dir, &name, e))?;
        run.quit(dir);
        if slot.slot != "A" || !slot.good {
            return Err(format!(
                "the installed disk ({}): {slot:?}, expected slot A marked good",
                f.name()
            ));
        }
        // The image's data partition is 512 MiB; the disk leaves it
        // about INSTALL_DISK_GIB minus the system's 1.6 GiB.
        if data < (INSTALL_DISK_GIB - 3) * 1024 * 1024 {
            return Err(format!(
                "the installed disk ({}): the data partition is {} MiB, not grown to the disk",
                f.name(),
                data / 1024
            ));
        }
        steps.push(format!(
            "the installed disk booted alone ({}): slot A good, data partition {} MiB",
            f.name(),
            data / 1024
        ));
    }
    Ok(steps)
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
            Media {
                disk: Path::new("/w/disk-uefi.qcow2"),
                cdrom: None,
            },
            Path::new("/w/serial-uefi.log"),
            Path::new("/w"),
            &[
                ("boottest", "60".into()),
                ("boottest-update", "http://10.0.2.2:1/a,b.raucb".into()),
            ],
            false,
        );
        let s = a.join(" ");
        assert!(s.starts_with("-M virt -cpu cortex-a72 -m 1024"), "{s}");
        assert!(s.contains("-bios /fw/QEMU_EFI.fd"), "{s}");
        assert!(
            s.contains("file=/w/disk-uefi.qcow2,format=qcow2,if=virtio"),
            "{s}"
        );
        assert!(
            s.contains(
                "-fw_cfg name=opt/termos/boottest-update,string=http://10.0.2.2:1/a,,b.raucb"
            ),
            "{s}"
        );
        assert!(s.ends_with("-no-reboot"), "{s}");
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
            Media {
                disk: Path::new("/d"),
                cdrom: Some(Path::new("/i/termos.iso")),
            },
            Path::new("/w/serial-bios.log"),
            Path::new("/w"),
            &[("boottest", "30".into())],
            true,
        );
        let bs = b.join(" ");
        assert!(
            bs.contains("-cpu host") && bs.contains("-accel kvm"),
            "{bs}"
        );
        assert!(!b.iter().any(|a| a == "-bios"), "SeaBIOS is the default");
        assert!(
            bs.contains("file=/i/termos.iso,media=cdrom,if=none,id=cd,readonly=on -device ide-cd,drive=cd,bootindex=0"),
            "{bs}"
        );
        assert_eq!(firmware_file(&x, Firmware::Bios), Ok(None));
    }

    #[test]
    fn the_bundle_server_serves_files_in_its_folder_only() {
        use std::io::Read as _;
        let dir = std::env::temp_dir().join(format!("termos-serve-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("u.raucb"), b"bundle bytes").expect("write");
        let port = serve(&dir).expect("serve");
        let get = |path: &str| {
            let mut c = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect");
            write!(c, "GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").expect("send");
            let mut text = String::new();
            c.read_to_string(&mut text).expect("read");
            text
        };
        let ok = get("/u.raucb");
        assert!(
            ok.starts_with("HTTP/1.1 200 OK\r\nContent-Length: 12\r\n"),
            "{ok}"
        );
        assert!(ok.ends_with("\r\n\r\nbundle bytes"), "{ok}");
        assert!(get("/../etc/passwd").starts_with("HTTP/1.1 404"));
        assert!(get("/missing").starts_with("HTTP/1.1 404"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
