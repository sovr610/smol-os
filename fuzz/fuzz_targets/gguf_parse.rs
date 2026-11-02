// SPDX-License-Identifier: GPL-3.0-or-later
//! GGUF headers (D-066), from files and partial downloads: parsing never
//! panics, asks for more bytes only when it needs them, reads a prefix
//! the same way as the whole, and the estimate from any header is sound.
#![no_main]

use libfuzzer_sys::fuzz_target;
use termos_hw::accel::Accelerators;
use termos_models::{fit, gguf};

fuzz_target!(|data: &[u8]| {
    match gguf::parse(data) {
        Err(gguf::GgufError::Incomplete(need)) => {
            assert!(
                need > data.len() as u64,
                "asked for {need} with {} given",
                data.len()
            );
        }
        Err(_) => {}
        Ok(h) => {
            // Trailing bytes (the tensors) change nothing.
            let mut longer = data.to_vec();
            longer.extend_from_slice(&[0xAB; 64]);
            let again = gguf::parse(&longer).expect("a longer buffer parses too");
            assert_eq!(again.meta.len(), h.meta.len());
            let _ = (h.architecture(), h.name(), h.has_chat_template());
            let shape = h.shape();
            for context in [0, 4096, u64::MAX] {
                let e = fit::Estimate::new(u64::MAX / 3, shape.as_ref(), context);
                let _ = e.total();
                for gpu in [false, true] {
                    let p = fit::place(&e, &Accelerators::default(), gpu);
                    let _ = p.describe();
                }
            }
        }
    }
});
