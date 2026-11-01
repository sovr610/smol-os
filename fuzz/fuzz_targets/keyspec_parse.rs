// SPDX-License-Identifier: GPL-3.0-or-later
//! tmux key notation: parsing must never panic, and every accepted key must
//! print back to notation that parses to the same key.
#![no_main]

use libfuzzer_sys::fuzz_target;
use termos_core::input::KeyEvent;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data)
        && let Ok(key) = text.parse::<KeyEvent>()
    {
        let printed = key.to_string();
        let reparsed: KeyEvent = printed.parse().expect("printed key notation must parse");
        assert_eq!(reparsed, key, "round trip of {text:?} via {printed:?}");
    }
});
