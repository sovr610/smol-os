// SPDX-License-Identifier: GPL-3.0-or-later
//! The boards TermOS images are built for (D-074).
//!
//! Each starts from one of Buildroot's own board configurations, which
//! Buildroot keeps building, and adds TermOS's fragments from
//! `br2-external/configs/`: the toolchain, the packages, the read-only
//! root with A/B slots and a data partition, the boot loader setup.

/// A firmware QEMU boots an image with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Firmware {
    /// SeaBIOS (x86 only), QEMU's default.
    Bios,
    /// OVMF (x86_64) or AAVMF (aarch64).
    Uefi,
}

impl Firmware {
    pub fn name(self) -> &'static str {
        match self {
            Self::Bios => "bios",
            Self::Uefi => "uefi",
        }
    }
}

/// How QEMU runs a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Qemu {
    pub system: &'static str,
    /// `-M` and `-cpu`.
    pub machine: &'static str,
    pub cpu: &'static str,
    /// Firmwares the boot test tries, in order.
    pub firmwares: &'static [Firmware],
    /// The serial console the kernel writes to (for the logs).
    pub serial: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub name: &'static str,
    /// Buildroot's defconfig this starts from.
    pub base: &'static str,
    /// TermOS fragments, in `br2-external/configs/`, applied in order.
    pub fragments: &'static [&'static str],
    /// Rust's name for the CPU and system.
    pub rust: &'static str,
    /// `None`: no QEMU machine (real boards).
    pub qemu: Option<Qemu>,
}

pub const TARGETS: &[Target] = &[
    Target {
        name: "x86_64",
        base: "pc_x86_64_efi_defconfig",
        fragments: &["common.fragment", "x86_64.fragment"],
        rust: "x86_64-unknown-linux-gnu",
        qemu: Some(Qemu {
            system: "qemu-system-x86_64",
            machine: "q35",
            cpu: "max",
            firmwares: &[Firmware::Uefi, Firmware::Bios],
            serial: "ttyS0",
        }),
    },
    Target {
        name: "aarch64-virt",
        base: "aarch64_efi_defconfig",
        fragments: &["common.fragment", "aarch64-virt.fragment"],
        rust: "aarch64-unknown-linux-gnu",
        qemu: Some(Qemu {
            system: "qemu-system-aarch64",
            machine: "virt",
            cpu: "cortex-a72",
            firmwares: &[Firmware::Uefi],
            serial: "ttyAMA0",
        }),
    },
    Target {
        name: "rpi5",
        base: "raspberrypi5_defconfig",
        fragments: &["common.fragment", "rpi.fragment", "rpi5.fragment"],
        rust: "aarch64-unknown-linux-gnu",
        qemu: None,
    },
    Target {
        name: "rpi4",
        base: "raspberrypi4_64_defconfig",
        fragments: &["common.fragment", "rpi.fragment", "rpi4.fragment"],
        rust: "aarch64-unknown-linux-gnu",
        qemu: None,
    },
    Target {
        name: "rpi3",
        base: "raspberrypi3_64_defconfig",
        fragments: &["common.fragment", "rpi.fragment", "rpi3.fragment"],
        rust: "aarch64-unknown-linux-gnu",
        qemu: None,
    },
    Target {
        name: "rpi-zero2w",
        base: "raspberrypizero2w_64_defconfig",
        fragments: &["common.fragment", "rpi.fragment", "rpi-zero2w.fragment"],
        rust: "aarch64-unknown-linux-gnu",
        qemu: None,
    },
];

/// The target called `name`.
pub fn find(name: &str) -> Result<&'static Target, String> {
    TARGETS.iter().find(|t| t.name == name).ok_or_else(|| {
        let names: Vec<&str> = TARGETS.iter().map(|t| t.name).collect();
        format!("no image target `{name}`: {}", names.join(", "))
    })
}

impl Target {
    /// The suffix the `cc` crate looks for, e.g. `aarch64_unknown_linux_gnu`.
    pub fn cc_suffix(&self) -> String {
        self.rust.replace('-', "_")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_and_names() {
        let t = find("aarch64-virt").expect("target");
        assert_eq!(t.cc_suffix(), "aarch64_unknown_linux_gnu");
        assert!(
            find("x86_64")
                .expect("x86")
                .qemu
                .is_some_and(|q| q.firmwares.len() == 2)
        );
        assert!(find("vax").expect_err("none").contains("rpi4"));
        // Every fragment a target names exists.
        let configs = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("br2-external/configs");
        for t in TARGETS {
            for f in t.fragments {
                assert!(configs.join(f).is_file(), "{}: {f}", t.name);
            }
        }
    }
}
