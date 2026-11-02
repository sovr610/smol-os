// SPDX-License-Identifier: GPL-3.0-or-later
//! What local models can run on (ARCHITECTURE.md §12, D-066): CPU
//! features, memory, GPUs with their free memory, the GPU runtimes
//! (CUDA, ROCm, Vulkan, Metal), the Raspberry Pi model, and accelerators
//! (Coral, Hailo, Cloud TPU).
//!
//! The Linux probes read below a root folder (normally `/`), so tests
//! use fake trees.

use std::path::Path;

use crate::{GpuVendor, Sampler, Source as _};

/// Everything model placement needs to know.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Accelerators {
    /// e.g. `x86_64`, `aarch64`.
    pub arch: String,
    pub os: String,
    /// Instruction-set extensions inference uses: AVX2, AVX-512, FMA,
    /// F16C (x86); NEON, SVE, dotprod, i8mm (Arm).
    pub cpu_features: Vec<&'static str>,
    pub threads: usize,
    /// Bytes.
    pub memory_total: u64,
    /// Bytes free for a new model now.
    pub memory_available: u64,
    pub gpus: Vec<GpuMemory>,
    /// GPUs share system memory (Apple Silicon, SoCs).
    pub unified_memory: bool,
    /// An NVIDIA driver (NVML or `nvidia-smi`).
    pub cuda: bool,
    /// The ROCm kernel interface (`/dev/kfd`) with an AMD GPU.
    pub rocm: bool,
    /// A Vulkan loader with at least one installed driver.
    pub vulkan: bool,
    /// Metal (macOS on Apple Silicon).
    pub metal: bool,
    /// e.g. "Raspberry Pi 5 Model B Rev 1.0".
    pub pi_model: Option<String>,
    /// Coral Edge TPUs: "USB (1a6e:089a)", "PCIe (/dev/apex_0)".
    pub coral: Vec<String>,
    /// Hailo devices: "/dev/hailo0".
    pub hailo: Vec<String>,
    /// Cloud TPU chips on this VM (Google's PCI vendor id).
    pub tpu_chips: usize,
}

/// A GPU's memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuMemory {
    pub name: String,
    pub vendor: GpuVendor,
    /// Bytes.
    pub total: Option<u64>,
    pub free: Option<u64>,
}

impl Accelerators {
    /// The GPU memory a model can use: the largest card's free memory
    /// (llama.cpp can split across cards, but one card is the safe
    /// estimate). `None` without a dedicated GPU.
    pub fn best_gpu_free(&self) -> Option<u64> {
        if self.unified_memory {
            return None;
        }
        self.gpus.iter().filter_map(|g| g.free.or(g.total)).max()
    }

    /// A one-line summary per item, for `termos debug hw` and
    /// `termos model recommend`.
    pub fn describe(&self) -> Vec<String> {
        let gib = |b: u64| format!("{:.1} GiB", b as f64 / f64::from(1u32 << 30));
        let mut out = vec![format!(
            "CPU: {} threads, {} ({})",
            self.threads,
            self.arch,
            if self.cpu_features.is_empty() {
                "no SIMD extensions found".to_owned()
            } else {
                self.cpu_features.join(", ")
            }
        )];
        out.push(format!(
            "memory: {} free of {}{}",
            gib(self.memory_available),
            gib(self.memory_total),
            if self.unified_memory {
                " (shared with the GPU)"
            } else {
                ""
            }
        ));
        for g in &self.gpus {
            let mem = match (g.free, g.total) {
                (Some(f), Some(t)) => format!("{} free of {}", gib(f), gib(t)),
                (None, Some(t)) => gib(t),
                _ => "memory unknown".into(),
            };
            out.push(format!("GPU: {} ({}), {mem}", g.name, g.vendor));
        }
        let runtimes: Vec<&str> = [
            (self.cuda, "CUDA (NVIDIA driver)"),
            (self.rocm, "ROCm"),
            (self.vulkan, "Vulkan"),
            (self.metal, "Metal"),
        ]
        .iter()
        .filter(|(on, _)| *on)
        .map(|(_, n)| *n)
        .collect();
        out.push(format!(
            "GPU runtimes: {}",
            if runtimes.is_empty() {
                "none".to_owned()
            } else {
                runtimes.join(", ")
            }
        ));
        if let Some(pi) = &self.pi_model {
            out.push(format!("board: {pi}"));
        }
        for c in &self.coral {
            out.push(format!("Coral Edge TPU: {c}"));
        }
        for h in &self.hailo {
            out.push(format!("Hailo: {h}"));
        }
        if self.tpu_chips > 0 {
            out.push(format!("Cloud TPU: {} chips", self.tpu_chips));
        }
        out
    }
}

/// Probes this machine. Takes some milliseconds (and runs `nvidia-smi`
/// when NVML is missing); call it off any event loop.
pub fn detect() -> Accelerators {
    let mut a = Accelerators {
        arch: std::env::consts::ARCH.to_owned(),
        os: std::env::consts::OS.to_owned(),
        cpu_features: cpu_features(),
        ..Accelerators::default()
    };
    if let Ok(mut s) = Sampler::new() {
        let inv = s.inventory();
        let sample = s.sample();
        a.threads = inv.cpu.threads;
        a.memory_total = inv.memory_total;
        a.memory_available = sample.memory.available;
        a.gpus = sample
            .gpus
            .iter()
            .map(|g| GpuMemory {
                name: g.name.clone(),
                vendor: g.vendor.clone(),
                total: g.memory_total,
                free: match (g.memory_total, g.memory_used) {
                    (Some(t), Some(u)) => Some(t.saturating_sub(u)),
                    _ => None,
                },
            })
            .collect();
        a.cuda = s.has_nvml();
    }
    a.metal = cfg!(target_os = "macos") && cfg!(target_arch = "aarch64");
    a.unified_memory = a.metal;
    // Without NVML, or with NVIDIA cards whose memory it did not report,
    // ask nvidia-smi.
    let unknown_nvidia = a
        .gpus
        .iter()
        .any(|g| g.vendor == GpuVendor::Nvidia && g.total.is_none());
    if (!a.cuda || unknown_nvidia)
        && let Some(cards) = nvidia_smi()
    {
        a.cuda = true;
        a.gpus.retain(|g| g.vendor != GpuVendor::Nvidia);
        a.gpus.extend(cards);
    }
    let root = Path::new("/");
    a.vulkan = vulkan_loader(root, &a.os);
    if cfg!(target_os = "linux") {
        a.rocm = root.join("dev/kfd").exists() && a.gpus.iter().any(|g| g.vendor == GpuVendor::Amd);
        a.pi_model = pi_model(root);
        a.coral = coral(root);
        a.hailo = hailo(root);
        a.tpu_chips = tpu_chips(root);
        // A SoC GPU (a Pi's) shares system memory.
        if a.pi_model.is_some() {
            a.unified_memory = true;
        }
    }
    a
}

/// The CPU's extensions that matter for inference.
pub fn cpu_features() -> Vec<&'static str> {
    #[allow(unused_mut)]
    let mut f: Vec<&'static str> = Vec::new();
    #[cfg(target_arch = "x86_64")]
    {
        for (on, name) in [
            (std::arch::is_x86_feature_detected!("avx"), "AVX"),
            (std::arch::is_x86_feature_detected!("avx2"), "AVX2"),
            (std::arch::is_x86_feature_detected!("fma"), "FMA"),
            (std::arch::is_x86_feature_detected!("f16c"), "F16C"),
            (std::arch::is_x86_feature_detected!("avx512f"), "AVX-512"),
            (
                std::arch::is_x86_feature_detected!("avx512vnni"),
                "AVX-512 VNNI",
            ),
        ] {
            if on {
                f.push(name);
            }
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        for (on, name) in [
            (std::arch::is_aarch64_feature_detected!("neon"), "NEON"),
            (
                std::arch::is_aarch64_feature_detected!("dotprod"),
                "dotprod",
            ),
            (std::arch::is_aarch64_feature_detected!("i8mm"), "i8mm"),
            (std::arch::is_aarch64_feature_detected!("sve"), "SVE"),
            (std::arch::is_aarch64_feature_detected!("fp16"), "FP16"),
        ] {
            if on {
                f.push(name);
            }
        }
    }
    f
}

/// NVIDIA cards from `nvidia-smi`, when NVML could not be loaded.
fn nvidia_smi() -> Option<Vec<GpuMemory>> {
    let out = std::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,memory.total,memory.free",
            "--format=csv,noheader,nounits",
        ])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let cards = parse_nvidia_smi(&String::from_utf8_lossy(&out.stdout));
    (!cards.is_empty()).then_some(cards)
}

/// `name, total MiB, free MiB` lines.
pub fn parse_nvidia_smi(text: &str) -> Vec<GpuMemory> {
    text.lines()
        .filter_map(|l| {
            let mut parts = l.rsplitn(3, ',').map(str::trim);
            let free = parts.next()?.parse::<u64>().ok()?;
            let total = parts.next()?.parse::<u64>().ok()?;
            let name = parts.next()?.to_owned();
            Some(GpuMemory {
                name,
                vendor: GpuVendor::Nvidia,
                total: Some(total << 20),
                free: Some(free << 20),
            })
        })
        .collect()
}

/// A Vulkan loader and at least one installed driver (ICD).
pub fn vulkan_loader(root: &Path, os: &str) -> bool {
    match os {
        "linux" => {
            let loader = [
                "usr/lib/x86_64-linux-gnu/libvulkan.so.1",
                "usr/lib/aarch64-linux-gnu/libvulkan.so.1",
                "usr/lib64/libvulkan.so.1",
                "usr/lib/libvulkan.so.1",
                "usr/local/lib/libvulkan.so.1",
            ]
            .iter()
            .any(|p| root.join(p).exists());
            let icd = [
                "usr/share/vulkan/icd.d",
                "etc/vulkan/icd.d",
                "usr/local/share/vulkan/icd.d",
            ]
            .iter()
            .any(|d| {
                std::fs::read_dir(root.join(d)).is_ok_and(|mut it| {
                    it.any(|e| e.is_ok_and(|e| e.path().extension().is_some_and(|x| x == "json")))
                })
            });
            loader && icd
        }
        "windows" => {
            let sysroot = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
            Path::new(&sysroot)
                .join("System32")
                .join("vulkan-1.dll")
                .exists()
        }
        // macOS has Metal; Vulkan there is MoltenVK, which llama.cpp's
        // macOS build does not use.
        _ => false,
    }
}

/// The Raspberry Pi model from the device tree.
pub fn pi_model(root: &Path) -> Option<String> {
    let raw = std::fs::read(root.join("proc/device-tree/model")).ok()?;
    let model = String::from_utf8_lossy(&raw)
        .trim_end_matches('\0')
        .trim()
        .to_owned();
    model.starts_with("Raspberry Pi").then_some(model)
}

/// Coral Edge TPUs: USB (Global Unichip `1a6e:089a` before its firmware
/// loads, Google `18d1:9302` after) and PCIe/M.2 (`/dev/apex_N`).
pub fn coral(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(root.join("sys/bus/usb/devices"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let read = |f: &str| {
            std::fs::read_to_string(e.path().join(f))
                .ok()
                .map(|s| s.trim().to_ascii_lowercase())
        };
        if let (Some(v), Some(p)) = (read("idVendor"), read("idProduct"))
            && matches!(
                (v.as_str(), p.as_str()),
                ("1a6e", "089a") | ("18d1", "9302")
            )
        {
            out.push(format!("USB ({v}:{p})"));
        }
    }
    for e in std::fs::read_dir(root.join("dev"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("apex_") {
            out.push(format!("PCIe (/dev/{name})"));
        }
    }
    out.sort();
    out
}

/// Hailo devices (`/dev/hailoN`).
pub fn hailo(root: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(root.join("dev"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("hailo"))
        .map(|n| format!("/dev/{n}"))
        .collect();
    out.sort();
    out
}

/// Cloud TPU chips: PCI devices with Google's vendor id (`0x1ae0`), which
/// TPU VMs expose.
pub fn tpu_chips(root: &Path) -> usize {
    std::fs::read_dir(root.join("sys/bus/pci/devices"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            let read = |f: &str| std::fs::read_to_string(e.path().join(f)).unwrap_or_default();
            read("vendor").trim() == "0x1ae0"
                // Processing accelerators (class 0x12xxxx), not Google's
                // virtual NICs or disks.
                && read("class").trim().starts_with("0x12")
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().expect("dir")).expect("mkdir");
        std::fs::write(p, text).expect("write");
    }

    #[test]
    fn boards_and_accelerators_in_a_fake_tree() {
        let dir = tempfile::tempdir().expect("tempdir");
        let r = dir.path();
        assert_eq!(pi_model(r), None);
        write(
            r,
            "proc/device-tree/model",
            "Raspberry Pi 5 Model B Rev 1.0\0",
        );
        assert_eq!(
            pi_model(r).as_deref(),
            Some("Raspberry Pi 5 Model B Rev 1.0")
        );

        write(r, "sys/bus/usb/devices/1-1/idVendor", "1a6e\n");
        write(r, "sys/bus/usb/devices/1-1/idProduct", "089a\n");
        write(r, "sys/bus/usb/devices/1-2/idVendor", "046d\n");
        write(r, "sys/bus/usb/devices/1-2/idProduct", "c52b\n");
        write(r, "dev/apex_0", "");
        write(r, "dev/hailo0", "");
        write(r, "dev/null", "");
        assert_eq!(coral(r), ["PCIe (/dev/apex_0)", "USB (1a6e:089a)"]);
        assert_eq!(hailo(r), ["/dev/hailo0"]);

        write(r, "sys/bus/pci/devices/0000:00:04.0/vendor", "0x1ae0\n");
        write(r, "sys/bus/pci/devices/0000:00:04.0/class", "0x120000\n");
        write(r, "sys/bus/pci/devices/0000:00:05.0/vendor", "0x1ae0\n");
        write(r, "sys/bus/pci/devices/0000:00:05.0/class", "0x020000\n");
        write(r, "sys/bus/pci/devices/0000:00:06.0/vendor", "0x10de\n");
        write(r, "sys/bus/pci/devices/0000:00:06.0/class", "0x030000\n");
        assert_eq!(tpu_chips(r), 1);

        assert!(!vulkan_loader(r, "linux"));
        write(r, "usr/lib/x86_64-linux-gnu/libvulkan.so.1", "");
        assert!(!vulkan_loader(r, "linux"), "a loader without a driver");
        write(r, "usr/share/vulkan/icd.d/nvidia_icd.json", "{}");
        assert!(vulkan_loader(r, "linux"));
    }

    #[test]
    fn nvidia_smi_lines() {
        let cards = parse_nvidia_smi(
            "NVIDIA GeForce RTX 4090, 24564, 23010\nNVIDIA A100-SXM4-80GB, 81920, 81000\n\njunk\n",
        );
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].name, "NVIDIA GeForce RTX 4090");
        assert_eq!(cards[0].total, Some(24564 << 20));
        assert_eq!(cards[1].free, Some(81000 << 20));
    }

    #[test]
    fn best_gpu_and_description() {
        let a = Accelerators {
            arch: "x86_64".into(),
            os: "linux".into(),
            cpu_features: vec!["AVX2"],
            threads: 16,
            memory_total: 64 << 30,
            memory_available: 48 << 30,
            gpus: vec![
                GpuMemory {
                    name: "small".into(),
                    vendor: GpuVendor::Amd,
                    total: Some(8 << 30),
                    free: Some(7 << 30),
                },
                GpuMemory {
                    name: "big".into(),
                    vendor: GpuVendor::Nvidia,
                    total: Some(24 << 30),
                    free: Some(20 << 30),
                },
            ],
            cuda: true,
            vulkan: true,
            ..Accelerators::default()
        };
        assert_eq!(a.best_gpu_free(), Some(20 << 30));
        let d = a.describe().join("\n");
        assert!(
            d.contains("GPU: big (NVIDIA), 20.0 GiB free of 24.0 GiB"),
            "{d}"
        );
        assert!(
            d.contains("GPU runtimes: CUDA (NVIDIA driver), Vulkan"),
            "{d}"
        );
        let unified = Accelerators {
            unified_memory: true,
            ..a
        };
        assert_eq!(unified.best_gpu_free(), None);
    }

    #[test]
    fn this_machine_does_not_panic() {
        let a = detect();
        assert!(a.memory_total > 0);
        assert!(!a.describe().is_empty());
    }
}
