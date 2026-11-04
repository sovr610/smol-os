// SPDX-License-Identifier: GPL-3.0-or-later
//! Terminal input decoding: never panics, gives the same events however
//! the bytes are split, and leaves nothing pending once flushed.
#![no_main]

use libfuzzer_sys::fuzz_target;
use termos_core::input::decode::{Decoder, Input};

fn decode(chunks: &[&[u8]]) -> Vec<Input> {
    let mut d = Decoder::new();
    let mut out = Vec::new();
    for chunk in chunks {
        d.feed(chunk, &mut out);
    }
    d.flush(&mut out);
    // A paste cut off by the flush ends at the next one.
    if d.timeout().is_some() {
        d.flush(&mut out);
    }
    assert_eq!(d.timeout(), None, "something still pending after flushing");
    out
}

fuzz_target!(|data: &[u8]| {
    let Some((&step, bytes)) = data.split_first() else {
        return;
    };
    let whole = decode(&[bytes]);
    let step = usize::from(step).max(1);
    let chunks: Vec<&[u8]> = bytes.chunks(step).collect();
    assert_eq!(decode(&chunks), whole, "splitting every {step} bytes");
});
