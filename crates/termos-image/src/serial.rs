// SPDX-License-Identifier: GPL-3.0-or-later
//! What the image prints on its serial console for the boot tests (D-076):
//! `TERMOS-READY` when the console's first frame is on the screen, and
//! `TERMOS-MEM used_kb=… total_kb=…` once the desktop has settled.

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
    let field = |key: &str| {
        line.split_whitespace()
            .find_map(|w| w.strip_prefix(key))
            .and_then(|v| v.parse::<u64>().ok())
    };
    Some(Memory {
        used_kb: field("used_kb=")?,
        total_kb: field("total_kb=")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
