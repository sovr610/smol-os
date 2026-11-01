// SPDX-License-Identifier: GPL-3.0-or-later
//! iTerm2 inline images (`OSC 1337 ; File=…`), also understood by WezTerm
//! and others: the whole image at the cursor, sized in cells. There are no
//! placements, so an image that is partly covered cannot be shown this way
//! (the compositor falls back to blocks).

use base64::Engine as _;

/// Shows PNG data filling `cols`×`rows` cells with its top-left at the
/// 0-based cell (`x`, `y`). The cursor is saved and restored around it.
pub fn inline(png: &[u8], x: u16, y: u16, cols: u16, rows: u16) -> Vec<u8> {
    let b64 = base64::engine::general_purpose::STANDARD.encode(png);
    let mut out = format!(
        "\x1b7\x1b[{};{}H\x1b]1337;File=inline=1;size={};width={cols};height={rows};preserveAspectRatio=0:",
        u32::from(y) + 1,
        u32::from(x) + 1,
        png.len()
    )
    .into_bytes();
    out.extend_from_slice(b64.as_bytes());
    out.extend_from_slice(b"\x07\x1b8");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_bytes() {
        assert_eq!(
            inline(b"PNG", 0, 2, 10, 4),
            b"\x1b7\x1b[3;1H\x1b]1337;File=inline=1;size=3;width=10;height=4;preserveAspectRatio=0:UE5H\x07\x1b8"
                .to_vec()
        );
    }
}
