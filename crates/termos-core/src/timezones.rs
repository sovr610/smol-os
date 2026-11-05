// SPDX-License-Identifier: GPL-3.0-or-later
//! Time zones on a TermOS image (M13d, D-124): the names in the zoneinfo
//! database, checked before `termos system set-time-zone` links
//! `/etc/localtime` to one, and listed for the Clock page in Settings.

use std::path::{Component, Path, PathBuf};

/// The zoneinfo database (the image's `tzdata`).
pub const ZONEINFO: &str = "/usr/share/zoneinfo";

/// Whether `zone` is a zone's name: `Area/City` words of letters, digits,
/// `_`, `-` and `+`, no `..`, not absolute.
pub fn valid_zone(zone: &str) -> bool {
    !zone.is_empty()
        && zone.len() <= 64
        && !zone.starts_with('/')
        && Path::new(zone)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
        && zone
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '+'))
}

/// Whether `path` is a compiled zone (it starts with `TZif`).
pub fn is_tzif(path: &Path) -> bool {
    let mut magic = [0u8; 4];
    std::fs::File::open(path)
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut magic))
        .is_ok()
        && &magic == b"TZif"
}

/// Every zone under `zoneinfo`, sorted (`Europe/Paris`, `UTC`…): the
/// compiled zone files, without the `posix/` and `right/` copies.
pub fn zones(zoneinfo: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(zoneinfo.join(&rel)) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if rel.as_os_str().is_empty() && matches!(name.as_str(), "posix" | "right") {
                continue;
            }
            let path = rel.join(&name);
            let full = zoneinfo.join(&path);
            if full.is_dir() {
                stack.push(path);
                continue;
            }
            let z = path.to_string_lossy().replace('\\', "/");
            if valid_zone(&z) && is_tzif(&full) {
                out.push(z);
            }
        }
    }
    out.sort();
    out
}

/// The zone `localtime` (`/etc/localtime`) names: the part after
/// `zoneinfo/` of where it links.
pub fn current_zone(localtime: &Path) -> Option<String> {
    let target = std::fs::canonicalize(localtime).ok()?;
    let s = target.to_string_lossy().replace('\\', "/");
    s.split_once("zoneinfo/").map(|(_, z)| z.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zone_names_are_checked() {
        for ok in [
            "UTC",
            "Europe/Paris",
            "America/Argentina/Buenos_Aires",
            "Etc/GMT+5",
        ] {
            assert!(valid_zone(ok), "{ok}");
        }
        for bad in [
            "",
            "/etc/passwd",
            "../../etc/shadow",
            "Europe/../../x",
            "a b",
            "Zone;rm",
        ] {
            assert!(!valid_zone(bad), "{bad}");
        }
    }

    #[test]
    fn zones_are_the_compiled_files() {
        let dir = tempfile::tempdir().expect("dir");
        let zi = dir.path();
        for (p, text) in [
            ("Europe/Paris", "TZif2..."),
            ("posix/Europe/Paris", "TZif2..."),
            ("Etc/UTC", "TZif2..."),
            ("zone.tab", "# not a zone"),
        ] {
            let f = zi.join(p);
            std::fs::create_dir_all(f.parent().expect("parent")).expect("mkdir");
            std::fs::write(f, text).expect("write");
        }
        assert_eq!(zones(zi), ["Etc/UTC", "Europe/Paris"]);
        assert!(zones(&zi.join("missing")).is_empty());
    }
}
