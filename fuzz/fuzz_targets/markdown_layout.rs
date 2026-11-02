// SPDX-License-Identifier: GPL-3.0-or-later
//! Markdown files: parsing and laying out never panic, at any width, with
//! pictures decoded or not.
#![no_main]

use libfuzzer_sys::fuzz_target;
use termos_apps::markdown::{layout, parse};

fuzz_target!(|data: &[u8]| {
    let Some((&w, text)) = data.split_first() else {
        return;
    };
    let text = String::from_utf8_lossy(text);
    let doc = parse(&text);
    let sizes: Vec<Option<(u32, u32)>> = (0..doc.images.len())
        .map(|i| (i % 2 == 0).then_some((u32::from(w) + 1, 7)))
        .collect();
    let _ = layout(&doc, u16::from(w % 160), &sizes);
});
