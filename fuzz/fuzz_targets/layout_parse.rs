// SPDX-License-Identifier: GPL-3.0-or-later
//! Layout files (M13b, D-120): parsing never panics, a parsed layout
//! keeps its limits, and writing one out reads back the same.
#![no_main]

use std::path::Path;

use libfuzzer_sys::fuzz_target;
use termos_core::layouts::{Layout, MAX_PANES, MAX_WORKSPACES, parse_project};

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    if let Ok(l) = Layout::parse("fuzz", text, Path::new("fuzz.toml")) {
        assert!(l.workspaces.len() <= MAX_WORKSPACES);
        assert!(l.workspaces.iter().all(|w| w.pane.len() <= MAX_PANES));
        let again = Layout::parse("fuzz", &l.to_toml(), Path::new("fuzz.toml"))
            .expect("a written layout reads back");
        assert_eq!(again, l);
    }
    let _ = parse_project(text, Path::new("termos.toml"));
});
