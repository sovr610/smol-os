// SPDX-License-Identifier: GPL-3.0-or-later
//! udev's and the kernel's netlink messages (M13d, D-124): parsing never
//! panics, and a device name never carries `/dev/`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use termos_hw::drives::{parse_uevent, unescape_udev};

fuzz_target!(|data: &[u8]| {
    if let Some(ev) = parse_uevent(data) {
        assert!(!ev.devname.is_some_and(|d| d.starts_with("/dev/")));
    }
    let _ = unescape_udev(&String::from_utf8_lossy(data));
});
