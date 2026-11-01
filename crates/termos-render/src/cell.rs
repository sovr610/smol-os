// SPDX-License-Identifier: GPL-3.0-or-later
//! Cells, attributes and styles.

use bitflags::bitflags;
use compact_str::CompactString;
use serde::{Deserialize, Serialize};

use crate::color::Color;

bitflags! {
    /// SGR text attributes.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
    pub struct Attrs: u16 {
        const BOLD = 1;
        const DIM = 1 << 1;
        const ITALIC = 1 << 2;
        const UNDERLINE = 1 << 3;
        const DOUBLE_UNDERLINE = 1 << 4;
        const CURLY_UNDERLINE = 1 << 5;
        const DOTTED_UNDERLINE = 1 << 6;
        const DASHED_UNDERLINE = 1 << 7;
        const BLINK = 1 << 8;
        const REVERSE = 1 << 9;
        const HIDDEN = 1 << 10;
        const STRIKE = 1 << 11;
    }
}

/// Colours plus attributes, without content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    /// Underline colour (`Color::Default` = same as the text).
    pub ul: Color,
    pub attrs: Attrs,
}

impl Style {
    pub const fn new() -> Self {
        Self {
            fg: Color::Default,
            bg: Color::Default,
            ul: Color::Default,
            attrs: Attrs::empty(),
        }
    }

    pub const fn fg(mut self, fg: Color) -> Self {
        self.fg = fg;
        self
    }

    pub const fn bg(mut self, bg: Color) -> Self {
        self.bg = bg;
        self
    }

    pub fn attrs(mut self, attrs: Attrs) -> Self {
        self.attrs |= attrs;
        self
    }

    pub fn bold(self) -> Self {
        self.attrs(Attrs::BOLD)
    }

    pub fn reversed(self) -> Self {
        self.attrs(Attrs::REVERSE)
    }
}

/// One terminal cell.
///
/// A double-width character occupies two cells: the leading cell holds the
/// symbol with `width == 2`, and the following cell is a continuation with
/// `width == 0` and an empty symbol.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Cell {
    /// A grapheme cluster; never contains control characters.
    pub symbol: CompactString,
    pub width: u8,
    pub style: Style,
}

impl Default for Cell {
    fn default() -> Self {
        Self::blank(Style::new())
    }
}

impl Cell {
    /// A space in `style`.
    pub fn blank(style: Style) -> Self {
        Self {
            symbol: CompactString::const_new(" "),
            width: 1,
            style,
        }
    }

    /// A single-width character.
    pub fn new(ch: char, style: Style) -> Self {
        let mut symbol = CompactString::const_new("");
        symbol.push(sanitize(ch));
        Self {
            symbol,
            width: 1,
            style,
        }
    }

    /// The trailing half of a double-width character.
    pub fn continuation(style: Style) -> Self {
        Self {
            symbol: CompactString::const_new(""),
            width: 0,
            style,
        }
    }

    pub fn is_continuation(&self) -> bool {
        self.width == 0
    }

    /// The symbol, or `" "` for a cell with no visible content.
    pub fn display_symbol(&self) -> &str {
        if self.symbol.is_empty() {
            " "
        } else {
            &self.symbol
        }
    }
}

/// Replaces control characters, which must never reach the output stream.
pub(crate) fn sanitize(ch: char) -> char {
    if ch.is_control() { ' ' } else { ch }
}
