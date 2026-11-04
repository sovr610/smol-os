// SPDX-License-Identifier: GPL-3.0-or-later
//! Putting GPU readings together: sysinfo's list (name, load, memory),
//! sysfs (temperature, power, SoC GPUs) and NVML (NVIDIA temperature and
//! power), matched by PCI slot.

use std::fmt;

use crate::GpuSample;
#[cfg(target_os = "linux")]
use crate::sysfs::DrmCard;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    Apple,
    Broadcom,
    Arm,
    Qualcomm,
    Other(String),
}

impl GpuVendor {
    /// From a vendor name as the system reports it ("NVIDIA Corporation",
    /// "Advanced Micro Devices, Inc. [AMD/ATI]", …).
    pub fn from_name(name: &str) -> Self {
        let n = name.to_ascii_lowercase();
        if n.contains("nvidia") {
            Self::Nvidia
        } else if n.contains("amd") || n.contains("advanced micro") || n.contains("ati ") {
            Self::Amd
        } else if n.contains("intel") {
            Self::Intel
        } else if n.contains("apple") {
            Self::Apple
        } else if n.contains("broadcom") {
            Self::Broadcom
        } else if n.contains("arm") || n.contains("mali") {
            Self::Arm
        } else if n.contains("qualcomm") || n.contains("adreno") {
            Self::Qualcomm
        } else {
            Self::Other(name.trim().to_owned())
        }
    }

    /// From a PCI vendor id.
    pub fn from_pci(id: u16) -> Option<Self> {
        Some(match id {
            0x10de => Self::Nvidia,
            0x1002 | 0x1022 => Self::Amd,
            0x8086 => Self::Intel,
            0x106b => Self::Apple,
            _ => return None,
        })
    }

    /// From a SoC GPU's kernel driver.
    pub fn from_driver(driver: &str) -> Option<Self> {
        Some(match driver.split('-').next().unwrap_or(driver) {
            "v3d" | "vc4" => Self::Broadcom,
            "panfrost" | "panthor" | "lima" => Self::Arm,
            "msm" => Self::Qualcomm,
            "asahi" => Self::Apple,
            "etnaviv" => Self::Other("Vivante".into()),
            "amdgpu" | "radeon" => Self::Amd,
            "i915" | "xe" => Self::Intel,
            "nouveau" | "nvidia" => Self::Nvidia,
            _ => return None,
        })
    }
}

impl fmt::Display for GpuVendor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Nvidia => "NVIDIA",
            Self::Amd => "AMD",
            Self::Intel => "Intel",
            Self::Apple => "Apple",
            Self::Broadcom => "Broadcom",
            Self::Arm => "Arm",
            Self::Qualcomm => "Qualcomm",
            Self::Other(s) => s,
        })
    }
}

/// A GPU reading with the PCI slot it was found at, for matching.
#[derive(Debug, Clone)]
pub(crate) struct Probe {
    /// For matching sysfs and NVML readings (Linux and Windows).
    #[cfg_attr(not(any(target_os = "linux", windows)), allow(dead_code))]
    pub pci: Option<String>,
    pub sample: GpuSample,
}

/// `0000:03:00.0` and `00000000:03:00.0` are the same slot.
#[cfg_attr(not(any(target_os = "linux", windows)), allow(dead_code))]
pub(crate) fn same_slot(a: &str, b: &str) -> bool {
    fn parts(s: &str) -> Option<(u32, u32, u32, u32)> {
        let (domain, rest) = s.split_once(':')?;
        let (bus, rest) = rest.split_once(':')?;
        let (dev, func) = rest.split_once('.').unwrap_or((rest, "0"));
        Some((
            u32::from_str_radix(domain, 16).ok()?,
            u32::from_str_radix(bus, 16).ok()?,
            u32::from_str_radix(dev, 16).ok()?,
            u32::from_str_radix(func, 16).ok()?,
        ))
    }
    match (parts(a), parts(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// Adds sysfs temperature, power and load to the GPUs found, and the SoC
/// GPUs that sysinfo does not list.
#[cfg(target_os = "linux")]
pub(crate) fn merge_sysfs(probes: &mut Vec<Probe>, cards: &[DrmCard]) {
    for card in cards {
        let found = card.pci.as_deref().and_then(|slot| {
            probes
                .iter_mut()
                .find(|p| p.pci.as_deref().is_some_and(|q| same_slot(q, slot)))
        });
        match found {
            Some(p) => {
                let s = &mut p.sample;
                s.celsius = s.celsius.or(card.celsius);
                s.watts = s.watts.or(card.watts);
                s.usage = s.usage.or(card.busy);
                s.memory_used = s.memory_used.or(card.vram_used);
                s.memory_total = s.memory_total.or(card.vram_total);
            }
            None if card.pci.is_none() => probes.push(Probe {
                pci: None,
                sample: soc_sample(card),
            }),
            // A PCI GPU sysinfo did not list (no name database): still
            // show what sysfs knows.
            None => probes.push(Probe {
                pci: card.pci.clone(),
                sample: soc_sample(card),
            }),
        }
    }
}

#[cfg(target_os = "linux")]
fn soc_sample(card: &DrmCard) -> GpuSample {
    let vendor = card
        .vendor_id
        .and_then(GpuVendor::from_pci)
        .or_else(|| card.driver.as_deref().and_then(GpuVendor::from_driver))
        .unwrap_or_else(|| GpuVendor::Other("GPU".into()));
    let name = match (&card.compatible, &card.driver) {
        (Some(c), _) => format!("{vendor} {}", c.rsplit(',').next().unwrap_or(c)),
        (None, Some(d)) => format!("{vendor} GPU ({d})"),
        (None, None) => format!("{vendor} GPU"),
    };
    GpuSample {
        name,
        vendor,
        usage: card.busy,
        memory_used: card.vram_used,
        memory_total: card.vram_total,
        celsius: card.celsius,
        watts: card.watts,
    }
}

/// The kernel driver of each GPU, matched by name order of vendor.
#[cfg(target_os = "linux")]
pub(crate) fn add_drivers(gpus: &mut [crate::GpuInfo], cards: &[DrmCard]) {
    let mut used = vec![false; cards.len()];
    for g in gpus.iter_mut() {
        let hit = cards.iter().enumerate().position(|(i, c)| {
            !used[i]
                && c.vendor_id
                    .and_then(GpuVendor::from_pci)
                    .or_else(|| c.driver.as_deref().and_then(GpuVendor::from_driver))
                    .as_ref()
                    == Some(&g.vendor)
        });
        if let Some(i) = hit {
            used[i] = true;
            g.driver = cards[i].driver.clone();
        }
    }
}

/// One NVIDIA GPU's NVML readings.
#[cfg(any(target_os = "linux", windows))]
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NvReading {
    pub pci: Option<String>,
    pub celsius: Option<f32>,
    pub watts: Option<f32>,
}

/// Adds NVML temperature and power to the NVIDIA GPUs: by PCI slot, and
/// readings without a matching slot to the remaining NVIDIA GPUs in order.
#[cfg(any(target_os = "linux", windows))]
pub(crate) fn merge_nvidia(probes: &mut [Probe], readings: &[NvReading]) {
    let mut taken = vec![false; probes.len()];
    let mut unmatched = Vec::new();
    for r in readings {
        let by_slot = r.pci.as_deref().and_then(|slot| {
            probes
                .iter()
                .position(|p| p.pci.as_deref().is_some_and(|q| same_slot(q, slot)))
        });
        match by_slot {
            Some(i) if !taken[i] => {
                taken[i] = true;
                apply(&mut probes[i].sample, r);
            }
            _ => unmatched.push(r),
        }
    }
    let mut free = (0..probes.len())
        .filter(|&i| !taken[i] && probes[i].sample.vendor == GpuVendor::Nvidia)
        .collect::<Vec<_>>()
        .into_iter();
    for r in unmatched {
        let Some(i) = free.next() else { break };
        apply(&mut probes[i].sample, r);
    }
}

/// Adds the GPU temperature the system's own sensors report (macOS: the
/// SMC's `TG0P` on Intel Macs, labelled "GPU"; the HID temperature
/// sensors named for the GPU on Apple silicon): the hottest of them. With
/// one GPU it is that GPU's; with several, the one GPU that is not
/// Intel's built-in graphics (the discrete one the SMC measures), if
/// there is exactly one.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn merge_sensor_temps(probes: &mut [Probe], sensors: &[(String, f32)]) {
    let hottest = sensors
        .iter()
        .filter(|(label, c)| {
            label.to_ascii_lowercase().contains("gpu") && c.is_finite() && *c > 0.0
        })
        .map(|(_, c)| *c)
        .fold(None, |m: Option<f32>, c| Some(m.map_or(c, |m| m.max(c))));
    let Some(celsius) = hottest else { return };
    let target = if probes.len() == 1 {
        Some(0)
    } else {
        let discrete: Vec<usize> = (0..probes.len())
            .filter(|&i| probes[i].sample.vendor != GpuVendor::Intel)
            .collect();
        (discrete.len() == 1).then(|| discrete[0])
    };
    if let Some(i) = target {
        let s = &mut probes[i].sample;
        s.celsius = s.celsius.or(Some(celsius));
    }
}

#[cfg(any(target_os = "linux", windows))]
fn apply(s: &mut GpuSample, r: &NvReading) {
    s.celsius = s.celsius.or(r.celsius);
    s.watts = s.watts.or(r.watts);
}

#[cfg(test)]
mod tests {
    #[test]
    fn sensor_temperatures_go_to_the_gpu_they_measure() {
        let probe = |vendor: GpuVendor| Probe {
            pci: None,
            sample: GpuSample {
                name: "gpu".into(),
                vendor,
                usage: Some(10.0),
                memory_used: None,
                memory_total: None,
                celsius: None,
                watts: None,
            },
        };
        let sensors = vec![
            ("GPU MTR Temp Sensor1".to_owned(), 41.5),
            ("gpu MTR Temp Sensor4".to_owned(), 44.0),
            ("PMU tdie1".to_owned(), 70.0),
            ("GPU".to_owned(), f32::NAN),
        ];
        // Apple silicon: one GPU, the hottest GPU sensor.
        let mut one = vec![probe(GpuVendor::Apple)];
        merge_sensor_temps(&mut one, &sensors);
        assert_eq!(one[0].sample.celsius, Some(44.0));
        // An Intel Mac with a discrete GPU: the SMC's reading is the
        // discrete one's.
        let mut two = vec![probe(GpuVendor::Intel), probe(GpuVendor::Amd)];
        merge_sensor_temps(&mut two, &[("GPU".to_owned(), 55.0)]);
        assert_eq!(two[0].sample.celsius, None);
        assert_eq!(two[1].sample.celsius, Some(55.0));
        // No GPU sensor: nothing.
        let mut none = vec![probe(GpuVendor::Apple)];
        merge_sensor_temps(&mut none, &[("CPU".to_owned(), 50.0)]);
        assert_eq!(none[0].sample.celsius, None);
    }

    use super::*;

    #[cfg(any(target_os = "linux", windows))]
    fn probe(pci: Option<&str>, vendor: GpuVendor) -> Probe {
        Probe {
            pci: pci.map(str::to_owned),
            sample: GpuSample {
                name: format!("{vendor} card"),
                vendor,
                usage: Some(10.0),
                memory_used: None,
                memory_total: None,
                celsius: None,
                watts: None,
            },
        }
    }

    #[test]
    fn vendors_and_slots() {
        assert_eq!(
            GpuVendor::from_name("Advanced Micro Devices, Inc. [AMD/ATI]"),
            GpuVendor::Amd
        );
        assert_eq!(
            GpuVendor::from_name("NVIDIA Corporation"),
            GpuVendor::Nvidia
        );
        assert_eq!(GpuVendor::from_pci(0x8086), Some(GpuVendor::Intel));
        assert_eq!(GpuVendor::from_driver("vc4-drm"), Some(GpuVendor::Broadcom));
        assert!(same_slot("0000:03:00.0", "00000000:03:00.0"));
        assert!(!same_slot("0000:03:00.0", "0000:04:00.0"));
        assert!(!same_slot("junk", "0000:04:00.0"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sysfs_fills_in_and_adds_soc_gpus() {
        let mut probes = vec![probe(Some("0000:03:00.0"), GpuVendor::Amd)];
        let cards = vec![
            DrmCard {
                card: "card1".into(),
                pci: Some("0000:03:00.0".into()),
                celsius: Some(50.0),
                watts: Some(30.0),
                busy: Some(99.0),
                ..DrmCard::default()
            },
            DrmCard {
                card: "card0".into(),
                driver: Some("v3d".into()),
                compatible: Some("brcm,2711-v3d".into()),
                ..DrmCard::default()
            },
        ];
        merge_sysfs(&mut probes, &cards);
        assert_eq!(probes.len(), 2);
        let amd = &probes[0].sample;
        assert_eq!((amd.celsius, amd.watts), (Some(50.0), Some(30.0)));
        assert_eq!(amd.usage, Some(10.0), "sysinfo's reading is kept");
        assert_eq!(probes[1].sample.name, "Broadcom 2711-v3d");
        assert_eq!(probes[1].sample.vendor, GpuVendor::Broadcom);
    }

    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn nvml_readings_go_to_nvidia_cards() {
        let mut probes = vec![
            probe(Some("0000:00:02.0"), GpuVendor::Intel),
            probe(Some("0000:01:00.0"), GpuVendor::Nvidia),
            probe(None, GpuVendor::Nvidia),
        ];
        let readings = vec![
            NvReading {
                pci: Some("00000000:01:00.0".into()),
                celsius: Some(61.0),
                watts: Some(120.5),
            },
            NvReading {
                pci: None,
                celsius: Some(40.0),
                watts: None,
            },
        ];
        merge_nvidia(&mut probes, &readings);
        assert_eq!(probes[0].sample.celsius, None);
        assert_eq!(probes[1].sample.celsius, Some(61.0));
        assert_eq!(probes[1].sample.watts, Some(120.5));
        // The second reading has no slot: it goes to the NVIDIA card
        // left over.
        assert_eq!(probes[2].sample.celsius, Some(40.0));
    }
}
