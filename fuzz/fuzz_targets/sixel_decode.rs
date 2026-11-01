// SPDX-License-Identifier: GPL-3.0-or-later
//! Sixel data from programs: decoding never panics, never exceeds its
//! limits, and an image it accepts re-encodes and decodes to itself up to
//! Sixel's colour precision (same size, same transparent pixels).
#![no_main]

use libfuzzer_sys::fuzz_target;
use termos_render::graphics::sixel;

const MAX_SIDE: u32 = 512;
const MAX_PIXELS: u64 = 64 * 1024;

fuzz_target!(|data: &[u8]| {
    let Ok(pic) = sixel::decode(data, MAX_SIDE, MAX_PIXELS) else {
        return;
    };
    assert!(pic.width() <= MAX_SIDE && pic.height() <= MAX_SIDE);
    assert!(u64::from(pic.width()) * u64::from(pic.height()) <= MAX_PIXELS);
    let seq = sixel::encode(&pic);
    let start = seq.iter().position(|b| *b == b'q').expect("q") + 1;
    let again = sixel::decode(&seq[start..seq.len() - 2], MAX_SIDE, MAX_PIXELS)
        .expect("our own output decodes");
    assert_eq!((again.width(), again.height()), (pic.width(), pic.height()));
    for (a, b) in pic.rgba().chunks_exact(4).zip(again.rgba().chunks_exact(4)) {
        assert_eq!(a[3], b[3], "transparency survives");
    }
});
