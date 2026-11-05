// SPDX-License-Identifier: GPL-3.0-or-later
//! Notifications and shell marks (OSC 9, 99, 133, 633, 777): parsing
//! never panics and what comes out stays within its limits.
#![no_main]

use libfuzzer_sys::fuzz_target;
use termos_term::signals::{self, KittyNotices, Mark, Signal};

fuzz_target!(|data: &[u8]| {
    let Some((&which, body)) = data.split_first() else {
        return;
    };
    let number = [9, 99, 133, 633, 777][usize::from(which) % 5];
    let check = |title: &str, text: &str| {
        assert!(title.chars().count() <= 256 && text.chars().count() <= 1024);
        assert!(!title.chars().chain(text.chars()).any(char::is_control));
    };
    if number == 99 {
        let mut k = KittyNotices::default();
        for part in body.split(|b| *b == 0) {
            if let Some(n) = k.feed(part) {
                check(&n.title, &n.body);
            }
        }
        return;
    }
    match signals::parse(number, body) {
        Some(Signal::Notify(n)) => check(&n.title, &n.body),
        Some(Signal::Mark(Mark::CommandLine(line))) => assert!(line.chars().count() <= 4096),
        _ => {}
    }
});
