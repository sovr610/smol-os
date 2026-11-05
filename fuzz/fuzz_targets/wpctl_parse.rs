// SPDX-License-Identifier: GPL-3.0-or-later
//! WirePlumber's `wpctl` output (M13e, D-128): reading volumes and the
//! outputs never panics, and what is read is sane.
#![no_main]

use libfuzzer_sys::fuzz_target;
use termos_hw::sound::{parse_sinks, parse_volume};

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    if let Some(v) = parse_volume(&text) {
        assert!(v.level.is_finite() && v.level >= 0.0);
        let _ = v.status_text();
    }
    for s in parse_sinks(&text) {
        assert!(!s.name.is_empty());
        assert!(s.volume.is_none_or(|v| v.level.is_finite() && v.level >= 0.0));
    }
});
