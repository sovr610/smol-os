// SPDX-License-Identifier: GPL-3.0-or-later
//! Screenshots from QEMU (D-076): parsing never panics, and whatever
//! parses can be inspected and written out as a PNG.
#![no_main]

use libfuzzer_sys::fuzz_target;
use termos_image::screen;

fuzz_target!(|data: &[u8]| {
    if let Ok(p) = screen::parse_ppm(data) {
        assert_eq!(p.rgb.len(), p.width as usize * p.height as usize);
        let _ = screen::inspect(&p).problems();
        let png = screen::png(&p);
        assert!(png.starts_with(b"\x89PNG"));
    }
});
