// SPDX-License-Identifier: GPL-3.0-or-later
//! What the image prints on its serial console for the boot tests (D-076,
//! D-078, D-080): `TERMOS-READY` when the console's first frame is on the
//! screen; `TERMOS-MEM used_kb=… total_kb=…` and `TERMOS-DATA size_kb=…`
//! once the desktop has settled; `TERMOS-SLOT slot=… version=… good|bad`
//! when the booted slot is marked; `TERMOS-UPDATE installed into slot …`
//! after `termos update install`.

/// The console's first frame is up.
pub const READY: &str = "TERMOS-READY";
/// The memory report's prefix.
pub const MEM: &str = "TERMOS-MEM";

/// `MemTotal − MemAvailable` and `MemTotal`, in KiB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Memory {
    pub used_kb: u64,
    pub total_kb: u64,
}

/// Where `READY` is in the log (a byte offset), if it is there.
pub fn ready(log: &str) -> Option<usize> {
    log.find(READY)
}

/// The memory report after `READY`, if both are there.
pub fn memory(log: &str) -> Option<Memory> {
    let after = &log[ready(log)?..];
    let line = after.lines().find(|l| l.contains(MEM))?;
    Some(Memory {
        used_kb: field(line, "used_kb=")?,
        total_kb: field(line, "total_kb=")?,
    })
}

/// A `key=` field's number on a line.
fn field(line: &str, key: &str) -> Option<u64> {
    line.split_whitespace()
        .find_map(|w| w.strip_prefix(key))
        .and_then(|v| v.parse::<u64>().ok())
}

/// The data file system's size after `READY`, in KiB.
pub fn data_kb(log: &str) -> Option<u64> {
    let after = &log[ready(log)?..];
    after
        .lines()
        .find(|l| l.contains("TERMOS-DATA"))
        .and_then(|l| field(l, "size_kb="))
}

/// The booted slot's marking: its name, the system's version, and whether
/// it was marked good.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    pub slot: String,
    pub version: String,
    pub good: bool,
}

/// The first `TERMOS-SLOT` line.
pub fn slot(log: &str) -> Option<Slot> {
    let line = log.lines().find(|l| l.contains("TERMOS-SLOT "))?;
    let word = |key: &str| {
        line.split_whitespace()
            .find_map(|w| w.strip_prefix(key))
            .map(str::to_owned)
    };
    Some(Slot {
        slot: word("slot=")?,
        version: word("version=")?,
        good: line.split_whitespace().any(|w| w == "good"),
    })
}

/// The slot `termos update install` put an update into.
pub fn update_installed(log: &str) -> Option<String> {
    log.lines()
        .find_map(|l| l.split("TERMOS-UPDATE installed into slot ").nth(1))
        .map(|s| s.trim().to_owned())
}

/// Whether `termos install` finished (`Some(true)`) or failed.
pub fn install_done(log: &str) -> Option<bool> {
    if log.contains("TERMOS-INSTALL done") {
        Some(true)
    } else if log.contains("TERMOS-INSTALL failed") {
        Some(false)
    } else {
        None
    }
}

/// The last `n` lines of a log, for a report: firmware and boot-loader
/// escape sequences and other control characters left out.
pub fn tail(log: &str, n: usize) -> String {
    let mut plain = String::with_capacity(log.len());
    let mut chars = log.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => {
                // CSI: parameters, then a final byte. Others: intermediate
                // bytes, then a final byte.
                if chars.next_if_eq(&'[').is_some() {
                    while chars
                        .next()
                        .is_some_and(|c| !('\x40'..='\x7e').contains(&c))
                    {}
                } else {
                    while chars.next_if(|c| (' '..='/').contains(c)).is_some() {}
                    chars.next();
                }
            }
            '\n' | '\t' => plain.push(c),
            c if c.is_control() => {}
            c => plain.push(c),
        }
    }
    let lines: Vec<&str> = plain.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_slots_and_updates() {
        let log = "TERMOS-SLOT slot=B version=0.0.0-update-test good\r\nTERMOS-READY\r\n\
                   TERMOS-MEM used_kb=1 total_kb=2\r\nTERMOS-DATA size_kb=1534000\r\n\
                   Installing into slot A\r\nTERMOS-UPDATE installed into slot A\r\n";
        assert_eq!(data_kb(log), Some(1_534_000));
        assert_eq!(
            slot(log),
            Some(Slot {
                slot: "B".into(),
                version: "0.0.0-update-test".into(),
                good: true
            })
        );
        assert_eq!(update_installed(log).as_deref(), Some("A"));
        let bad = "TERMOS-SLOT slot=A version=0.0.0 bad: the console did not start; restarting\n";
        assert_eq!(slot(bad).map(|s| s.good), Some(false));
        assert_eq!(data_kb("TERMOS-DATA size_kb=5\n"), None, "only after READY");
        assert_eq!(update_installed("TERMOS-UPDATE failed\n"), None);
        assert_eq!(
            install_done("x\nTERMOS-INSTALL done /dev/vda\n"),
            Some(true)
        );
        assert_eq!(install_done("TERMOS-INSTALL failed\n"), Some(false));
        assert_eq!(install_done("TERMOS-READY\n"), None);
    }

    #[test]
    fn tails_are_plain_text() {
        let log = "\x1b[2J\x1b[01;01HBdsDxe: loading\r\n\r\n\x1b(Bone\r\ntwo\x07\r\nthree\r\n";
        assert_eq!(tail(log, 2), "two\nthree");
        assert_eq!(tail(log, 10), "BdsDxe: loading\none\ntwo\nthree");
        assert_eq!(tail("", 3), "");
    }

    #[test]
    fn markers() {
        let log = "Starting seatd: OK\r\nTERMOS-MEM used_kb=1 total_kb=2\r\n[    3.1] x\r\nTERMOS-READY\r\n\
                   TERMOS-MEM used_kb=98765 total_kb=1000000\r\n";
        assert_eq!(ready(log), Some(log.find("TERMOS-READY").expect("here")));
        // Only a report after the console is up counts.
        assert_eq!(
            memory(log),
            Some(Memory {
                used_kb: 98765,
                total_kb: 1_000_000
            })
        );
        assert_eq!(memory("TERMOS-MEM used_kb=1 total_kb=2\n"), None);
        assert_eq!(
            memory("TERMOS-READY\nTERMOS-MEM used_kb=x total_kb=2\n"),
            None
        );
    }
}
