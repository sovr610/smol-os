// SPDX-License-Identifier: GPL-3.0-or-later
//! Capability-probe replies: parsing never panics, never invents bytes,
//! and hands back everything it did not understand.
#![no_main]

use libfuzzer_sys::fuzz_target;
use termos_render::probe::parse_replies;

fuzz_target!(|data: &[u8]| {
    let r = parse_replies(data);
    assert!(r.other.len() + r.partial.len() <= data.len());
    if !data.contains(&0x1b) {
        assert_eq!(r.other, data, "plain bytes are input");
    }
    // Parsing is a function of its input: the same bytes, the same result.
    assert_eq!(parse_replies(data), r);
    // Splitting the input never makes a complete probe incomplete once the
    // rest arrives.
    if let Some(cut) = data.len().checked_div(2) {
        let head = parse_replies(&data[..cut]);
        if head.complete() {
            assert!(r.complete());
        }
    }
});
