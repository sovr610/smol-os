// SPDX-License-Identifier: GPL-3.0-or-later
//! What headless Claude Code and Codex CLI print (D-100): any line is
//! read without panicking.
#![no_main]

use libfuzzer_sys::fuzz_target;
use termos_harness::events;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    for line in text.lines() {
        let _ = events::claude_line(line);
        let _ = events::codex_line(line);
        let _ = events::is_limit(line);
    }
});
