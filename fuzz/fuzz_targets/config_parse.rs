// SPDX-License-Identifier: GPL-3.0-or-later
//! Arbitrary text as config.toml: must parse or return an error, never panic
//! (including the line/column computation for error locations).
#![no_main]

use std::path::Path;

use libfuzzer_sys::fuzz_target;
use termos_core::config::Config;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = Config::parse(text, Path::new("fuzz.toml")).map_err(|e| e.to_string());
    }
});
