// SPDX-License-Identifier: GPL-3.0-or-later
//! Network links for the Network app (D-081): each interface but loopback,
//! whether it is Wi-Fi, whether it is up, its hardware address and its IP
//! addresses. Addresses come from `sysinfo` everywhere; the kind and the
//! state come from Linux's sysfs (elsewhere they are unknown).

use std::path::Path;

/// One interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub name: String,
    pub wifi: bool,
    pub state: LinkState,
    /// `aa:bb:cc:dd:ee:ff`; empty when there is none.
    pub mac: String,
    /// `192.168.1.20/24`, `fe80::1/64`…
    pub addresses: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkState {
    Up,
    /// Up, but no cable or no network joined.
    NoCarrier,
    Down,
    Unknown,
}

impl LinkState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Up => "connected",
            Self::NoCarrier => "not connected",
            Self::Down => "off",
            Self::Unknown => "",
        }
    }
}

/// Kind and state from `/sys/class/net/<name>` (`dir`).
pub fn from_sysfs(dir: &Path) -> (bool, LinkState) {
    let read = |f: &str| {
        std::fs::read_to_string(dir.join(f))
            .map(|s| s.trim().to_owned())
            .unwrap_or_default()
    };
    let wifi = dir.join("wireless").exists() || dir.join("phy80211").exists();
    let state = match read("operstate").as_str() {
        "up" => LinkState::Up,
        "dormant" | "lowerlayerdown" => LinkState::NoCarrier,
        "down" => {
            // IFF_UP is bit 0 of `flags`: up with no carrier.
            let flags =
                u32::from_str_radix(read("flags").trim_start_matches("0x"), 16).unwrap_or(0);
            if flags & 1 == 1 {
                LinkState::NoCarrier
            } else {
                LinkState::Down
            }
        }
        _ => LinkState::Unknown,
    };
    (wifi, state)
}

/// The machine's links, sorted by name (loopback left out).
pub fn links() -> Vec<Link> {
    let Ok(networks) = sysinfo::Networks::new_with_refreshed_list() else {
        return Vec::new();
    };
    let mut out: Vec<Link> = networks
        .iter()
        .map(|(name, data)| (name.to_string_lossy().into_owned(), data))
        .filter(|(name, _)| !name.starts_with("lo"))
        .map(|(name, data)| {
            let (wifi, state) = if cfg!(target_os = "linux") {
                from_sysfs(&Path::new("/sys/class/net").join(&name))
            } else {
                (false, LinkState::Unknown)
            };
            let mac = data.mac_address().to_string();
            Link {
                name,
                wifi,
                state,
                mac: if mac == "00:00:00:00:00:00" {
                    String::new()
                } else {
                    mac
                },
                addresses: data.ip_networks().iter().map(ToString::to_string).collect(),
            }
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iface(dir: &Path, operstate: &str, flags: &str, carrier: &str, wifi: bool) {
        std::fs::create_dir_all(dir).expect("mkdir");
        std::fs::write(dir.join("operstate"), format!("{operstate}\n")).expect("w");
        std::fs::write(dir.join("flags"), format!("{flags}\n")).expect("w");
        std::fs::write(dir.join("carrier"), format!("{carrier}\n")).expect("w");
        if wifi {
            std::fs::create_dir_all(dir.join("wireless")).expect("mkdir");
        }
    }

    #[test]
    fn kinds_and_states_from_sysfs() {
        let dir = std::env::temp_dir().join(format!("termos-net-{}", std::process::id()));
        iface(&dir.join("eth0"), "up", "0x1003", "1", false);
        iface(&dir.join("wlan0"), "dormant", "0x1003", "0", true);
        iface(&dir.join("eth1"), "down", "0x1003", "0", false);
        iface(&dir.join("eth2"), "down", "0x1002", "0", false);
        assert_eq!(from_sysfs(&dir.join("eth0")), (false, LinkState::Up));
        assert_eq!(from_sysfs(&dir.join("wlan0")), (true, LinkState::NoCarrier));
        assert_eq!(from_sysfs(&dir.join("eth1")), (false, LinkState::NoCarrier));
        assert_eq!(from_sysfs(&dir.join("eth2")), (false, LinkState::Down));
        assert_eq!(
            from_sysfs(&dir.join("missing")),
            (false, LinkState::Unknown)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn this_machines_links_have_names() {
        for l in links() {
            assert!(!l.name.is_empty());
            assert_ne!(l.name, "lo");
        }
    }
}
