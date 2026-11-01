// SPDX-License-Identifier: GPL-3.0-or-later
//! The Kitty graphics protocol
//! (<https://sw.kovidgoyal.net/kitty/graphics-protocol/>), as far as
//! TermOS uses it: upload an image once, then show any part of it in any
//! cell rectangle as a placement, which can be moved or removed without
//! sending the image again. That is what makes partial occlusion work:
//! each visible piece of an image is its own placement of a source crop.
//!
//! Every command carries `q=2`, so the terminal never answers (answers
//! would arrive as keystrokes).

use base64::Engine as _;

/// Base64 bytes per chunk (the protocol's limit).
const CHUNK: usize = 4096;

/// A part of the image, in source pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Uploads PNG data as image `id`, in chunks.
pub fn upload(id: u32, png: &[u8]) -> Vec<u8> {
    let b64 = base64::engine::general_purpose::STANDARD.encode(png);
    let chunks: Vec<&[u8]> = b64.as_bytes().chunks(CHUNK).collect();
    let mut out = Vec::with_capacity(b64.len() + chunks.len() * 40);
    for (i, chunk) in chunks.iter().enumerate() {
        let more = u8::from(i + 1 < chunks.len());
        if i == 0 {
            out.extend_from_slice(format!("\x1b_Ga=t,f=100,i={id},q=2,m={more};").as_bytes());
        } else {
            out.extend_from_slice(format!("\x1b_Gm={more};").as_bytes());
        }
        out.extend_from_slice(chunk);
        out.extend_from_slice(b"\x1b\\");
    }
    if chunks.is_empty() {
        out.extend_from_slice(format!("\x1b_Ga=t,f=100,i={id},q=2;\x1b\\").as_bytes());
    }
    out
}

/// Shows `src` of image `id` filling `cols`×`rows` cells with its top-left
/// at the 0-based cell (`x`, `y`), as placement `placement`. The cursor is
/// moved there first and does not move afterwards (`C=1`).
pub fn place(
    id: u32,
    placement: u32,
    x: u16,
    y: u16,
    src: SourceRect,
    cols: u16,
    rows: u16,
) -> Vec<u8> {
    format!(
        "\x1b[{};{}H\x1b_Ga=p,i={id},p={placement},x={},y={},w={},h={},c={cols},r={rows},C=1,q=2\x1b\\",
        u32::from(y) + 1,
        u32::from(x) + 1,
        src.x,
        src.y,
        src.w,
        src.h
    )
    .into_bytes()
}

/// Removes one placement; the image stays uploaded.
pub fn remove_placement(id: u32, placement: u32) -> Vec<u8> {
    format!("\x1b_Ga=d,d=i,i={id},p={placement},q=2\x1b\\").into_bytes()
}

/// Removes every placement of image `id` and frees its data.
pub fn free(id: u32) -> Vec<u8> {
    format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_bytes() {
        assert_eq!(
            upload(7, b"PNG"),
            b"\x1b_Ga=t,f=100,i=7,q=2,m=0;UE5H\x1b\\".to_vec()
        );
        assert_eq!(
            place(
                7,
                2,
                3,
                1,
                SourceRect {
                    x: 10,
                    y: 0,
                    w: 40,
                    h: 20
                },
                4,
                1
            ),
            b"\x1b[2;4H\x1b_Ga=p,i=7,p=2,x=10,y=0,w=40,h=20,c=4,r=1,C=1,q=2\x1b\\".to_vec()
        );
        assert_eq!(
            remove_placement(7, 2),
            b"\x1b_Ga=d,d=i,i=7,p=2,q=2\x1b\\".to_vec()
        );
        assert_eq!(free(7), b"\x1b_Ga=d,d=I,i=7,q=2\x1b\\".to_vec());
    }

    #[test]
    fn large_uploads_are_chunked() {
        let data = vec![0xabu8; 10_000];
        let out = upload(1, &data);
        let text = String::from_utf8(out).expect("ascii");
        let parts: Vec<&str> = text.split("\x1b\\").filter(|p| !p.is_empty()).collect();
        let b64_len = base64::engine::general_purpose::STANDARD
            .encode(&data)
            .len();
        assert_eq!(parts.len(), b64_len.div_ceil(CHUNK));
        assert!(parts[0].starts_with("\x1b_Ga=t,f=100,i=1,q=2,m=1;"));
        assert!(parts[1].starts_with("\x1b_Gm=1;"));
        assert!(parts.last().is_some_and(|p| p.starts_with("\x1b_Gm=0;")));
        for p in &parts {
            let payload = p.split_once(';').map_or("", |(_, b)| b);
            assert!(payload.len() <= CHUNK);
        }
    }
}
