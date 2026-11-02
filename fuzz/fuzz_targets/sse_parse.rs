// SPDX-License-Identifier: GPL-3.0-or-later
//! Provider event streams (D-058): parsing never panics, and however the
//! bytes are split into reads, the same events come out.
#![no_main]

use libfuzzer_sys::fuzz_target;
use termos_ai::sse::SseParser;

fuzz_target!(|data: &[u8]| {
    let Some((&split, data)) = data.split_first() else {
        return;
    };
    let whole = SseParser::new().feed(data);
    let cut = usize::from(split) % (data.len() + 1);
    let mut p = SseParser::new();
    let parts = p.feed(&data[..cut]).and_then(|mut first| {
        first.extend(p.feed(&data[cut..])?);
        Ok(first)
    });
    // A line too long for one feed can fit in two (it is checked per
    // line), so only compare when both readings succeeded.
    if let (Ok(a), Ok(b)) = (&whole, &parts) {
        assert_eq!(a, b, "split at {cut}");
        for ev in a {
            assert!(!ev.event.is_empty());
        }
    }
});
