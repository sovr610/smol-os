// SPDX-License-Identifier: GPL-3.0-or-later
//! The harness's planner answers (D-100): any text is a plan or an error,
//! never a panic, and a plan it accepts passes its own checks.
#![no_main]

use libfuzzer_sys::fuzz_target;
use termos_harness::plan;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    if let Ok(p) = plan::parse("goal", &text, 8) {
        assert!(p.check(8).is_ok());
    }
    let _ = plan::first_object(&text);
});
