// SPDX-License-Identifier: GPL-3.0-or-later
//! Program output through the image interceptor: never panics, and where
//! the input is split between reads makes no difference.
#![no_main]

use libfuzzer_sys::fuzz_target;
use termos_term::intercept::{Interceptor, Piece};

/// Pieces with adjacent byte runs merged (how reads were split may cut a
/// run of ordinary bytes in two, nothing more).
fn merged(chunks: &[&[u8]]) -> Vec<Piece> {
    let mut i = Interceptor::default();
    let mut out: Vec<Piece> = Vec::new();
    for chunk in chunks {
        let mut pieces = Vec::new();
        i.feed(chunk, &mut pieces);
        for p in pieces {
            match (out.last_mut(), p) {
                (Some(Piece::Bytes(prev)), Piece::Bytes(more)) => prev.extend(more),
                (_, p) => out.push(p),
            }
        }
    }
    out
}

fuzz_target!(|data: &[u8]| {
    let whole = merged(&[data]);
    let passed: usize = whole
        .iter()
        .map(|p| match p {
            Piece::Bytes(b) => b.len(),
            _ => 0,
        })
        .sum();
    assert!(passed <= data.len(), "no bytes are invented");
    if !data.is_empty() {
        let cut = usize::from(data[0]) % data.len();
        let (a, b) = data.split_at(cut);
        assert_eq!(merged(&[a, b]), whole, "split at {cut}");
    }
});
