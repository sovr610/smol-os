// SPDX-License-Identifier: GPL-3.0-or-later
//! # termos-hw
//!
//! What the machine has and how busy it is: CPU, memory, temperatures,
//! GPUs, disks, network and processes (DECISIONS.md D-050). The system
//! monitor samples it; local models (M7) read the [`Inventory`].
//!
//! - CPU, memory, temperatures, disks, network, processes and the GPU list
//!   come from `sysinfo` on Linux, macOS and Windows.
//! - GPU temperature and power come from our own probes: Linux sysfs
//!   (`hwmon` of each DRM card; GPUs that are not on PCI, such as the
//!   Raspberry Pi's) and NVML for NVIDIA cards, loaded at run time.
//!
//! Nothing here runs by itself: a [`Sampler`] reads the system only when
//! asked, so it costs nothing while no one is looking.

use std::time::{Duration, Instant};

mod gpu;
#[cfg(any(target_os = "linux", windows))]
mod nvidia;
pub mod sysfs;

pub use gpu::GpuVendor;

// TODO(M9): GPU temperature and power on macOS (SMC), and for non-NVIDIA GPUs on Windows.

/// The machine, as it is at start: what local models (M7) and the system
/// monitor's header need.
#[derive(Debug, Clone, PartialEq)]
pub struct Inventory {
    /// e.g. "Ubuntu 24.04", "macOS 15.1", "Windows 11 Pro".
    pub os: String,
    pub kernel: Option<String>,
    /// e.g. "x86_64", "arm64".
    pub arch: String,
    pub host: Option<String>,
    pub cpu: CpuInfo,
    /// Bytes.
    pub memory_total: u64,
    pub swap_total: u64,
    pub gpus: Vec<GpuInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuInfo {
    pub model: String,
    /// Physical cores, when the system says.
    pub cores: Option<usize>,
    /// Logical processors.
    pub threads: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuInfo {
    pub name: String,
    pub vendor: GpuVendor,
    /// Dedicated memory in bytes, when known.
    pub memory_total: Option<u64>,
    /// The kernel driver (Linux), e.g. "amdgpu", "i915", "v3d".
    pub driver: Option<String>,
}

/// One reading of everything that changes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sample {
    pub cpu: CpuSample,
    pub memory: MemorySample,
    /// 1, 5 and 15 minute load averages (not on Windows).
    pub load: Option<[f64; 3]>,
    pub uptime: Duration,
    pub temps: Vec<Temp>,
    pub gpus: Vec<GpuSample>,
    pub disks: Vec<DiskSample>,
    pub net: NetSample,
    /// The busiest processes, by CPU then memory.
    pub processes: Vec<ProcessSample>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CpuSample {
    /// All CPUs, 0 to 100.
    pub total: f32,
    /// Each logical CPU, 0 to 100.
    pub cores: Vec<f32>,
    /// MHz of the first CPU, when known.
    pub freq_mhz: Option<u64>,
}

/// Bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemorySample {
    pub total: u64,
    pub used: u64,
    pub available: u64,
    pub swap_total: u64,
    pub swap_used: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Temp {
    pub label: String,
    pub celsius: f32,
    /// The temperature the hardware calls critical, when it says.
    pub critical: Option<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GpuSample {
    pub name: String,
    pub vendor: GpuVendor,
    /// 0 to 100.
    pub usage: Option<f32>,
    /// Bytes.
    pub memory_used: Option<u64>,
    pub memory_total: Option<u64>,
    pub celsius: Option<f32>,
    pub watts: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskSample {
    pub mount: String,
    pub fs: String,
    /// Bytes.
    pub total: u64,
    pub available: u64,
    pub removable: bool,
}

/// Bytes per second over every interface except loopback.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NetSample {
    pub rx_per_sec: u64,
    pub tx_per_sec: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProcessSample {
    pub pid: u32,
    pub name: String,
    /// Percent of one CPU (a busy multi-threaded process can exceed 100).
    pub cpu: f32,
    /// Resident bytes.
    pub memory: u64,
}

/// Something that can be sampled: the real machine, or fixtures in tests.
/// Not `Send`: the real one holds handles of the thread that made it, so
/// make it on the thread that samples.
pub trait Source {
    fn inventory(&mut self) -> Inventory;
    fn sample(&mut self) -> Sample;
}

/// How many processes a sample lists.
pub const TOP_PROCESSES: usize = 50;

/// Reads the real machine.
pub struct Sampler {
    system: sysinfo::System,
    components: Option<sysinfo::Components>,
    disks: Option<sysinfo::Disks>,
    networks: Option<sysinfo::Networks>,
    gpus: Option<sysinfo::Gpus>,
    #[cfg(any(target_os = "linux", windows))]
    nvidia: Option<nvidia::Nvidia>,
    last: Instant,
}

impl std::fmt::Debug for Sampler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sampler").finish_non_exhaustive()
    }
}

/// Why the machine cannot be read at all.
pub use sysinfo::Error;

/// Logs why an optional part (sensors, disks, GPUs…) is missing.
fn optional<T>(what: &str, r: Result<T, Error>) -> Option<T> {
    r.map_err(|e| tracing::debug!("no {what}: {e}")).ok()
}

impl Sampler {
    /// Lists CPUs, sensors, disks, interfaces and GPUs. This takes some
    /// milliseconds; do it off the event loop, on the thread that will
    /// sample.
    pub fn new() -> Result<Self, Error> {
        let mut system = sysinfo::System::new()?;
        system.refresh_cpu_all();
        system.refresh_memory();
        Ok(Self {
            system,
            components: optional("sensors", sysinfo::Components::new_with_refreshed_list()),
            disks: optional("disks", sysinfo::Disks::new_with_refreshed_list()),
            networks: optional("network", sysinfo::Networks::new_with_refreshed_list()),
            gpus: optional("GPU list", sysinfo::Gpus::new_with_refreshed_list()),
            #[cfg(any(target_os = "linux", windows))]
            nvidia: nvidia::Nvidia::load(),
            last: Instant::now(),
        })
    }

    fn gpu_samples(&mut self) -> Vec<GpuSample> {
        let mut out = Vec::new();
        if let Some(gpus) = &mut self.gpus {
            gpus.refresh(true);
            for g in gpus.iter() {
                let vendor = GpuVendor::from_name(g.vendor().unwrap_or(""));
                out.push(gpu::Probe {
                    pci: Some(g.pci().to_string()),
                    sample: GpuSample {
                        name: g
                            .model()
                            .map_or_else(|| vendor.to_string(), ToOwned::to_owned),
                        vendor,
                        usage: g.usage(),
                        memory_used: g.used_memory(),
                        memory_total: g.total_memory(),
                        celsius: None,
                        watts: None,
                    },
                });
            }
        }
        let mut probes: Vec<gpu::Probe> = out;
        #[cfg(target_os = "linux")]
        gpu::merge_sysfs(&mut probes, &sysfs::drm_cards(std::path::Path::new("/")));
        #[cfg(any(target_os = "linux", windows))]
        if let Some(nv) = &self.nvidia {
            gpu::merge_nvidia(&mut probes, &nv.readings());
        }
        probes.into_iter().map(|p| p.sample).collect()
    }
}

impl Source for Sampler {
    fn inventory(&mut self) -> Inventory {
        use sysinfo::System;
        let mut gpus: Vec<GpuInfo> = self
            .gpu_samples()
            .into_iter()
            .map(|g| GpuInfo {
                name: g.name,
                vendor: g.vendor,
                memory_total: g.memory_total,
                driver: None,
            })
            .collect();
        #[cfg(target_os = "linux")]
        gpu::add_drivers(&mut gpus, &sysfs::drm_cards(std::path::Path::new("/")));
        let cpus = self.system.cpus();
        Inventory {
            os: System::long_os_version()
                .or_else(|_| System::name())
                .unwrap_or_else(|_| std::env::consts::OS.to_owned()),
            kernel: System::kernel_version().ok(),
            arch: System::cpu_arch(),
            host: System::host_name().ok(),
            cpu: CpuInfo {
                model: cpus
                    .first()
                    .map(|c| c.brand().trim().to_owned())
                    .filter(|b| !b.is_empty())
                    .unwrap_or_else(|| "unknown CPU".to_owned()),
                cores: System::physical_core_count().ok(),
                threads: cpus.len(),
            },
            memory_total: self.system.total_memory(),
            swap_total: self.system.total_swap(),
            gpus: {
                gpus.dedup();
                gpus
            },
        }
    }

    fn sample(&mut self) -> Sample {
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
        let now = Instant::now();
        let elapsed = now.duration_since(self.last).as_secs_f64().max(0.001);
        self.last = now;

        self.system.refresh_cpu_usage();
        self.system.refresh_cpu_frequency();
        self.system.refresh_memory();
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_cpu().with_memory(),
        );
        if let Some(c) = &mut self.components {
            c.refresh(true);
        }
        if let Some(d) = &mut self.disks {
            d.refresh(true);
        }
        if let Some(n) = &mut self.networks {
            n.refresh(true);
        }

        let cpus = self.system.cpus();
        let load = System::load_average().ok().filter(|_| !cfg!(windows));
        let mut processes: Vec<ProcessSample> = self
            .system
            .processes()
            .values()
            .map(|p| ProcessSample {
                pid: p.pid().as_u32(),
                name: p.name().to_string_lossy().into_owned(),
                cpu: p.cpu_usage(),
                memory: p.memory(),
            })
            .collect();
        sort_processes(&mut processes);
        processes.truncate(TOP_PROCESSES);

        let (mut rx, mut tx) = (0u64, 0u64);
        for (name, data) in self.networks.iter().flat_map(|n| n.iter()) {
            if is_loopback(&name.to_string_lossy()) {
                continue;
            }
            rx = rx.saturating_add(data.received());
            tx = tx.saturating_add(data.transmitted());
        }
        let per_sec = |b: u64| (b as f64 / elapsed).round() as u64;

        Sample {
            cpu: CpuSample {
                total: self.system.global_cpu_usage(),
                cores: cpus.iter().map(sysinfo::Cpu::usage).collect(),
                freq_mhz: cpus.first().map(sysinfo::Cpu::frequency).filter(|f| *f > 0),
            },
            memory: MemorySample {
                total: self.system.total_memory(),
                used: self.system.used_memory(),
                available: self.system.available_memory(),
                swap_total: self.system.total_swap(),
                swap_used: self.system.used_swap(),
            },
            load: load.map(|l| [l.one, l.five, l.fifteen]),
            uptime: Duration::from_secs(System::uptime().unwrap_or(0)),
            temps: self
                .components
                .iter()
                .flat_map(|c| c.iter())
                .filter_map(|c| {
                    let celsius = c.temperature()?;
                    (celsius.is_finite() && celsius > -50.0).then(|| Temp {
                        label: c.label().to_owned(),
                        celsius,
                        critical: c.critical().filter(|t| t.is_finite() && *t > 0.0),
                    })
                })
                .collect(),
            gpus: self.gpu_samples(),
            disks: self.disks.as_ref().map(disks).unwrap_or_default(),
            net: NetSample {
                rx_per_sec: per_sec(rx),
                tx_per_sec: per_sec(tx),
            },
            processes,
        }
    }
}

/// Busiest first: by CPU, then memory, then pid for a stable order.
pub fn sort_processes(p: &mut [ProcessSample]) {
    p.sort_by(|a, b| {
        b.cpu
            .total_cmp(&a.cpu)
            .then(b.memory.cmp(&a.memory))
            .then(a.pid.cmp(&b.pid))
    });
}

fn is_loopback(name: &str) -> bool {
    name == "lo" || name.starts_with("lo0") || name.contains("Loopback")
}

/// Real disks: no pseudo file systems, each device once.
fn disks(list: &sysinfo::Disks) -> Vec<DiskSample> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for d in list.iter() {
        let fs = d.file_system().to_string_lossy().into_owned();
        if d.total_space() == 0 || matches!(fs.as_str(), "squashfs" | "overlay" | "tmpfs") {
            continue;
        }
        if !seen.insert(d.name().to_os_string()) {
            continue;
        }
        out.push(DiskSample {
            mount: d.mount_point().to_string_lossy().into_owned(),
            fs,
            total: d.total_space(),
            available: d.available_space(),
            removable: d.is_removable(),
        });
    }
    out.sort_by(|a, b| a.mount.cmp(&b.mount));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn processes_sort_busiest_first() {
        let p = |pid, cpu, memory| ProcessSample {
            pid,
            name: format!("p{pid}"),
            cpu,
            memory,
        };
        let mut v = vec![p(1, 0.0, 10), p(2, 50.0, 1), p(3, 0.0, 99), p(4, 50.0, 5)];
        sort_processes(&mut v);
        let order: Vec<u32> = v.iter().map(|p| p.pid).collect();
        assert_eq!(order, vec![4, 2, 3, 1]);
    }

    /// The real machine answers without panicking, and what it says is
    /// sane (this runs on every CI platform).
    #[test]
    fn the_real_machine_can_be_read() {
        let mut s = Sampler::new().expect("the test machine can be read");
        let inv = s.inventory();
        assert!(inv.cpu.threads >= 1, "{inv:?}");
        assert!(inv.memory_total > 0, "{inv:?}");
        assert!(!inv.arch.is_empty());
        let a = s.sample();
        assert_eq!(a.cpu.cores.len(), inv.cpu.threads);
        assert!(a.memory.used <= a.memory.total);
        assert!(a.processes.len() <= TOP_PROCESSES);
        assert!(!a.processes.is_empty(), "this test is a process");
        for c in &a.cpu.cores {
            assert!((0.0..=100.5).contains(c), "{c}");
        }
    }
}
