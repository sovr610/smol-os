// SPDX-License-Identifier: GPL-3.0-or-later
//! GPUs as Linux sysfs shows them: each DRM card's driver, PCI slot,
//! load and memory (amdgpu), and its `hwmon` temperature and power.
//!
//! Everything is read below a root directory, so tests use a fake tree.
//! Only `uevent` and plain value files are read (no symlinks), which keeps
//! the tests portable.

use std::path::Path;

/// One `/sys/class/drm/cardN`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DrmCard {
    /// `card0`, `card1`, …
    pub card: String,
    /// The PCI slot (`0000:03:00.0`); `None` for GPUs on a SoC.
    pub pci: Option<String>,
    /// The PCI vendor id (`0x1002` is AMD).
    pub vendor_id: Option<u16>,
    /// The kernel driver: `amdgpu`, `i915`, `xe`, `nouveau`, `v3d`, …
    pub driver: Option<String>,
    /// The device-tree name of a SoC GPU (`brcm,2711-v3d`).
    pub compatible: Option<String>,
    /// 0 to 100 (amdgpu).
    pub busy: Option<f32>,
    /// Bytes (amdgpu).
    pub vram_used: Option<u64>,
    pub vram_total: Option<u64>,
    pub celsius: Option<f32>,
    pub watts: Option<f32>,
}

/// SoC GPU drivers (no PCI): these cards are GPUs; other non-PCI cards
/// (`simpledrm`, display controllers) are not.
const SOC_GPU_DRIVERS: [&str; 8] = [
    "v3d", "vc4", "panfrost", "panthor", "lima", "etnaviv", "msm", "asahi",
];

/// Every GPU card under `root` (normally `/`).
pub fn drm_cards(root: &Path) -> Vec<DrmCard> {
    let Ok(entries) = std::fs::read_dir(root.join("sys/class/drm")) else {
        return Vec::new();
    };
    let mut cards: Vec<DrmCard> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let is_card = name
                .strip_prefix("card")
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
            is_card.then(|| read_card(&e.path().join("device"), name))?
        })
        .collect();
    cards.sort_by_key(|c| {
        c.card
            .trim_start_matches("card")
            .parse::<u32>()
            .unwrap_or(u32::MAX)
    });
    // A Pi 4 has a display card (vc4) next to its GPU (v3d): one GPU.
    if cards.iter().any(|c| c.driver.as_deref() == Some("v3d")) {
        cards.retain(|c| !c.driver.as_deref().is_some_and(|d| driver_is(d, "vc4")));
    }
    cards
}

/// `driver` is `name` or a variant of it (`vc4-drm`).
fn driver_is(driver: &str, name: &str) -> bool {
    driver == name
        || driver
            .strip_prefix(name)
            .is_some_and(|r| r.starts_with('-'))
}

fn is_soc_gpu(driver: &str) -> bool {
    SOC_GPU_DRIVERS.iter().any(|d| driver_is(driver, d))
}

fn read_card(device: &Path, card: String) -> Option<DrmCard> {
    let uevent = read(&device.join("uevent")).unwrap_or_default();
    let field = |key: &str| {
        uevent.lines().find_map(|l| {
            l.strip_prefix(key)
                .and_then(|r| r.strip_prefix('='))
                .map(str::to_owned)
        })
    };
    let driver = field("DRIVER");
    let pci = field("PCI_SLOT_NAME");
    let compatible = field("OF_COMPATIBLE_0");
    let vendor_id = field("PCI_ID")
        .and_then(|id| id.split(':').next().map(str::to_owned))
        .or_else(|| read(&device.join("vendor")))
        .and_then(|v| u16::from_str_radix(v.trim().trim_start_matches("0x"), 16).ok());
    if pci.is_some() {
        // Only display controllers (PCI class 0x03xxxx).
        let class = field("PCI_CLASS")
            .or_else(|| read(&device.join("class")))
            .and_then(|c| u32::from_str_radix(c.trim().trim_start_matches("0x"), 16).ok());
        if class.is_none_or(|c| c >> 16 != 0x03) {
            return None;
        }
    } else if !driver.as_deref().is_some_and(is_soc_gpu) {
        return None;
    }
    let number = |file: &str| read(&device.join(file)).and_then(|v| v.trim().parse::<u64>().ok());
    let (celsius, watts) = hwmon(&device.join("hwmon"));
    Some(DrmCard {
        card,
        pci,
        vendor_id,
        driver,
        compatible,
        busy: number("gpu_busy_percent").map(|b| b.min(100) as f32),
        vram_used: number("mem_info_vram_used"),
        vram_total: number("mem_info_vram_total").filter(|t| *t > 0),
        celsius,
        watts,
    })
}

/// Temperature (°C) and power (W) from the first `hwmonN` that has them.
fn hwmon(dir: &Path) -> (Option<f32>, Option<f32>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (None, None);
    };
    let mut dirs: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    dirs.sort();
    let (mut celsius, mut watts) = (None, None);
    for d in dirs {
        let num = |f: &str| read(&d.join(f)).and_then(|v| v.trim().parse::<i64>().ok());
        if celsius.is_none() {
            // Millidegrees.
            celsius = num("temp1_input").map(|m| m as f32 / 1000.0);
        }
        if watts.is_none() {
            // Microwatts.
            watts = num("power1_average")
                .or_else(|| num("power1_input"))
                .map(|u| u as f32 / 1_000_000.0);
        }
    }
    (celsius, watts)
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// A display connector (`/sys/class/drm/card0-HDMI-A-1`), for the
/// Displays page (D-128); the console itself asks the card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrmConnector {
    /// `HDMI-A-1`, as the kernel names it.
    pub name: String,
    pub connected: bool,
    /// `1920x1080`, …, the preferred first, each once.
    pub modes: Vec<String>,
}

/// Every connector of every card under `root` (normally `/`), by card,
/// then name.
pub fn drm_connectors(root: &Path) -> Vec<DrmConnector> {
    let Ok(entries) = std::fs::read_dir(root.join("sys/class/drm")) else {
        return Vec::new();
    };
    let mut found: Vec<(u32, DrmConnector)> = entries
        .flatten()
        .filter_map(|e| {
            let dir = e.file_name().to_string_lossy().into_owned();
            let (card, name) = dir.strip_prefix("card")?.split_once('-')?;
            let card: u32 = card.parse().ok()?;
            let status = read(&e.path().join("status"))?;
            let mut modes: Vec<String> = Vec::new();
            for m in read(&e.path().join("modes")).unwrap_or_default().lines() {
                let m = m.trim();
                if !m.is_empty() && !modes.iter().any(|x| x == m) {
                    modes.push(m.to_owned());
                }
            }
            Some((
                card,
                DrmConnector {
                    name: name.to_owned(),
                    connected: status.trim() == "connected",
                    modes,
                },
            ))
        })
        .collect();
    found.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.cmp(&b.1.name)));
    found.into_iter().map(|(_, c)| c).collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn connectors_are_read_with_their_modes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let drm = dir.path().join("sys/class/drm");
        let put = |c: &str, status: &str, modes: &str| {
            let d = drm.join(c);
            std::fs::create_dir_all(&d).expect("mkdir");
            std::fs::write(d.join("status"), status).expect("write");
            std::fs::write(d.join("modes"), modes).expect("write");
        };
        put(
            "card1-HDMI-A-1",
            "connected\n",
            "1920x1080\n1920x1080\n1280x720\n",
        );
        put("card1-DP-1", "disconnected\n", "");
        put("card0-Virtual-1", "connected\n", "1024x768\n");
        std::fs::create_dir_all(drm.join("card0")).expect("a card, not a connector");
        std::fs::create_dir_all(drm.join("renderD128")).expect("not a connector");
        let c = drm_connectors(dir.path());
        let names: Vec<&str> = c.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["Virtual-1", "DP-1", "HDMI-A-1"]);
        assert!(c[0].connected);
        assert!(!c[1].connected);
        assert_eq!(c[2].modes, ["1920x1080", "1280x720"]);
        assert!(drm_connectors(&dir.path().join("none")).is_empty());
    }

    /// Writes a fake `/sys/class/drm/<card>/device` tree.
    pub(crate) fn card(root: &Path, card: &str, files: &[(&str, &str)]) -> PathBuf {
        let dev = root.join("sys/class/drm").join(card).join("device");
        for (name, text) in files {
            let p = dev.join(name);
            std::fs::create_dir_all(p.parent().expect("has a parent")).expect("mkdir");
            std::fs::write(&p, text).expect("write");
        }
        dev
    }

    #[test]
    fn amd_intel_and_pi_cards() {
        let root = tempfile::tempdir().expect("tempdir");
        let r = root.path();
        card(
            r,
            "card1",
            &[
                (
                    "uevent",
                    "DRIVER=amdgpu\nPCI_CLASS=30000\nPCI_ID=1002:73BF\nPCI_SLOT_NAME=0000:03:00.0\n",
                ),
                ("gpu_busy_percent", "37\n"),
                ("mem_info_vram_used", "1073741824\n"),
                ("mem_info_vram_total", "17163091968\n"),
                ("hwmon/hwmon4/temp1_input", "52000\n"),
                ("hwmon/hwmon4/power1_average", "45000000\n"),
            ],
        );
        card(
            r,
            "card0",
            &[(
                "uevent",
                "DRIVER=i915\nPCI_CLASS=30000\nPCI_ID=8086:A780\nPCI_SLOT_NAME=0000:00:02.0\n",
            )],
        );
        // Connectors and render nodes are not cards.
        std::fs::create_dir_all(r.join("sys/class/drm/card0-HDMI-A-1")).expect("mkdir");
        std::fs::create_dir_all(r.join("sys/class/drm/renderD128")).expect("mkdir");
        // A network card that happens to be listed is not a GPU.
        card(
            r,
            "card7",
            &[(
                "uevent",
                "DRIVER=e1000e\nPCI_CLASS=20000\nPCI_ID=8086:15F3\nPCI_SLOT_NAME=0000:00:1f.6\n",
            )],
        );
        let cards = drm_cards(r);
        assert_eq!(cards.len(), 2, "{cards:?}");
        let intel = &cards[0];
        assert_eq!(intel.driver.as_deref(), Some("i915"));
        assert_eq!(intel.vendor_id, Some(0x8086));
        assert_eq!(intel.celsius, None);
        let amd = &cards[1];
        assert_eq!(amd.pci.as_deref(), Some("0000:03:00.0"));
        assert_eq!(amd.vendor_id, Some(0x1002));
        assert_eq!(amd.busy, Some(37.0));
        assert_eq!(amd.vram_total, Some(17_163_091_968));
        assert_eq!(amd.celsius, Some(52.0));
        assert_eq!(amd.watts, Some(45.0));

        // A Raspberry Pi 4: the display card is not a second GPU.
        let pi = tempfile::tempdir().expect("tempdir");
        card(
            pi.path(),
            "card0",
            &[(
                "uevent",
                "DRIVER=v3d\nOF_NAME=v3d\nOF_COMPATIBLE_0=brcm,2711-v3d\n",
            )],
        );
        card(
            pi.path(),
            "card1",
            &[(
                "uevent",
                "DRIVER=vc4-drm\nOF_COMPATIBLE_0=brcm,bcm2711-vc5\n",
            )],
        );
        card(
            pi.path(),
            "card2",
            &[("uevent", "DRIVER=simple-framebuffer\n")],
        );
        let cards = drm_cards(pi.path());
        assert_eq!(cards.len(), 1, "{cards:?}");
        assert_eq!(cards[0].driver.as_deref(), Some("v3d"));
        assert_eq!(cards[0].compatible.as_deref(), Some("brcm,2711-v3d"));
        assert_eq!(cards[0].pci, None);

        assert!(drm_cards(Path::new("/nonexistent-root")).is_empty());
    }
}
