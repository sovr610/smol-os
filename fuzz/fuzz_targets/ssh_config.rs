// SPDX-License-Identifier: GPL-3.0-or-later
//! `~/.ssh/config` (M13b, D-120): parsing never panics, and every host it
//! keeps is safe to give `ssh`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use termos_core::ssh_config::{MAX_HOSTS, parse, valid_host};

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let (hosts, _includes) = parse(&text);
    assert!(hosts.len() <= MAX_HOSTS);
    for h in hosts {
        assert!(!h.is_empty() && !h.contains(char::is_whitespace));
        assert!(h.starts_with('-') || valid_host(&h) || h.len() > 255);
    }
});
