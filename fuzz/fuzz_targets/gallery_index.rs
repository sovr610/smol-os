// SPDX-License-Identifier: GPL-3.0-or-later
//! The gallery's index (M13g, D-126): parsing never panics, every entry
//! it accepts has checked fields, and checking signatures with a fixture
//! key never panics either.
#![no_main]

use libfuzzer_sys::fuzz_target;
use termos_apps::gallery::{key_id, parse_index, valid_name, verify};

fuzz_target!(|data: &[u8]| {
    let Ok(file) = parse_index(data) else {
        return;
    };
    for e in &file.entries {
        assert!(valid_name(&e.name));
        assert!(e.url.starts_with("https://"));
        assert_eq!(e.sha256.len(), 64);
    }
    let public = [3u8; 32];
    let _ = verify(&file, &[(key_id(&public), public)]);
});
