// SPDX-License-Identifier: GPL-3.0-or-later
//! NVIDIA temperature and power through NVML, which the driver installs
//! (`libnvidia-ml.so.1`, `nvml.dll`). It is loaded at run time: machines
//! without an NVIDIA driver simply have no readings.

use nvml_wrapper::Nvml;
use nvml_wrapper::enum_wrappers::device::TemperatureSensor;

use crate::gpu::NvReading;

pub(crate) struct Nvidia {
    nvml: Nvml,
}

impl Nvidia {
    pub(crate) fn load() -> Option<Self> {
        match Nvml::init() {
            Ok(nvml) => Some(Self { nvml }),
            Err(e) => {
                tracing::debug!("no NVML: {e}");
                None
            }
        }
    }

    pub(crate) fn readings(&self) -> Vec<NvReading> {
        let count = self.nvml.device_count().unwrap_or(0);
        (0..count)
            .filter_map(|i| self.nvml.device_by_index(i).ok())
            .map(|d| NvReading {
                pci: d.pci_info().ok().map(|p| p.bus_id),
                celsius: d.temperature(TemperatureSensor::Gpu).ok().map(|c| c as f32),
                // Milliwatts.
                watts: d.power_usage().ok().map(|mw| mw as f32 / 1000.0),
            })
            .collect()
    }
}
