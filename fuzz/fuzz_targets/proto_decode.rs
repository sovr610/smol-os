// SPDX-License-Identifier: GPL-3.0-or-later
//! The session socket's decoders: arbitrary bytes into the frame decoder
//! (as both directions' message types) and into the JSON-RPC parser. Bad
//! input must produce errors, never panics or unbounded allocation; any
//! message that decodes must re-encode to an equal message.
#![no_main]

use libfuzzer_sys::fuzz_target;
use termos_core::proto::{ClientMsg, ServerMsg};
use termos_core::wire::{self, FrameDecoder};

fuzz_target!(|data: &[u8]| {
    for split in [1usize, 7, 4096] {
        let mut client = FrameDecoder::new();
        let mut server = FrameDecoder::new();
        for chunk in data.chunks(split) {
            client.push(chunk);
            server.push(chunk);
            while let Ok(Some(msg)) = client.next_frame::<ClientMsg>() {
                let bytes = wire::encode_frame(&msg).expect("decoded messages re-encode");
                let mut d = FrameDecoder::new();
                d.push(&bytes);
                assert_eq!(d.next_frame::<ClientMsg>().ok().flatten(), Some(msg));
            }
            while let Ok(Some(_msg)) = server.next_frame::<ServerMsg>() {}
        }
    }
    if let Ok(text) = std::str::from_utf8(data) {
        for line in text.lines() {
            if let Ok(wire::Message::Request(req)) = wire::parse_message(line) {
                let _ = wire::check_hello(&req);
            }
        }
    }
});
