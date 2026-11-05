// SPDX-License-Identifier: GPL-3.0-or-later
//! Icons for apps, files and menu items (D-087, D-088), in three sets:
//! Unicode symbols (the default: in every font), Nerd Font glyphs (with a
//! patched font) and plain text. Every icon is one cell wide in every set,
//! so layouts are the same whichever is chosen.

use std::fmt;
use std::str::FromStr;

use serde::Deserialize;

/// Which glyphs icons use (`[ui] icons`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IconSet {
    /// Symbols every font has.
    #[default]
    Unicode,
    /// Nerd Font glyphs (needs a patched font).
    Nerd,
    /// Plain ASCII.
    Ascii,
    /// No icons.
    None,
}

impl IconSet {
    pub const ALL: [IconSet; 4] = [Self::Unicode, Self::Nerd, Self::Ascii, Self::None];
}

impl fmt::Display for IconSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unicode => "unicode",
            Self::Nerd => "nerd",
            Self::Ascii => "ascii",
            Self::None => "none",
        })
    }
}

impl FromStr for IconSet {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|i| i.to_string() == s)
            .ok_or_else(|| format!("unknown icon set `{s}` (unicode, nerd, ascii or none)"))
    }
}

/// Something with an icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Icon {
    // The desktop.
    Menu,
    Terminal,
    FloatingTerminal,
    Apps,
    Agent,
    Project,
    Layout,
    Theme,
    Workspace,
    Session,
    Help,
    Detach,
    Quit,
    // Apps.
    Files,
    Monitor,
    Clipboard,
    Network,
    Updates,
    Settings,
    Audit,
    Notifications,
    Tutorial,
    Keys,
    Palette,
    About,
    /// The command center (M10c).
    Center,
    ImageViewer,
    Markdown,
    Git,
    /// Sound and Displays (M13e).
    Sound,
    Display,
    // Actions.
    Build,
    Run,
    Test,
    Paste,
    Copy,
    Split,
    SplitDown,
    Float,
    Zoom,
    Rename,
    Close,
    Minimize,
    New,
    // Files.
    Folder,
    File,
    Image,
    Code,
    Archive,
    Executable,
    Link,
    Text,
}

impl Icon {
    /// The glyph in `set` (empty for [`IconSet::None`]).
    pub fn glyph(self, set: IconSet) -> &'static str {
        match set {
            IconSet::Unicode => self.unicode(),
            IconSet::Nerd => self.nerd(),
            IconSet::Ascii => self.ascii(),
            IconSet::None => "",
        }
    }

    /// The glyph and a space, ready to go before a label (empty for
    /// [`IconSet::None`]).
    pub fn prefix(self, set: IconSet) -> String {
        match set {
            IconSet::None => String::new(),
            _ => format!("{} ", self.glyph(set)),
        }
    }

    /// Geometric shapes, arrows and a few dingbats that every terminal font
    /// has and none draws as a wide colour emoji.
    fn unicode(self) -> &'static str {
        match self {
            Icon::Menu => "◆",
            Icon::Terminal => "❯",
            Icon::FloatingTerminal => "◰",
            Icon::Apps => "▦",
            Icon::Agent => "✦",
            Icon::Project => "▣",
            Icon::Layout => "▥",
            Icon::Theme => "◑",
            Icon::Workspace => "▢",
            Icon::Session => "◫",
            Icon::Help => "?",
            Icon::Detach => "⇠",
            Icon::Quit => "✗",
            Icon::Files | Icon::Folder => "■",
            Icon::Monitor => "◔",
            Icon::Clipboard | Icon::Paste => "▤",
            Icon::Network => "◎",
            Icon::Updates => "↻",
            Icon::Settings => "✱",
            Icon::Audit => "≣",
            Icon::Notifications => "◉",
            Icon::Tutorial => "◇",
            Icon::Keys => "⌥",
            Icon::Palette => "›",
            Icon::About => "◈",
            Icon::Center => "◎",
            Icon::ImageViewer | Icon::Image => "▧",
            Icon::Markdown | Icon::Text => "≡",
            Icon::Git => "±",
            Icon::Sound => "◁",
            Icon::Display => "▯",
            Icon::Build => "▲",
            Icon::Run => "▷",
            Icon::Test => "✓",
            Icon::Copy => "▭",
            Icon::Split => "▥",
            Icon::SplitDown => "▤",
            Icon::Float => "◰",
            Icon::Zoom => "□",
            Icon::Rename => "✎",
            Icon::Close => "×",
            Icon::Minimize => "–",
            Icon::New => "+",
            Icon::File => "□",
            Icon::Code => "λ",
            Icon::Archive => "▤",
            Icon::Executable => "▶",
            Icon::Link => "⇢",
        }
    }

    /// Nerd Font glyphs (Font Awesome 4 code points, in every Nerd Font).
    fn nerd(self) -> &'static str {
        match self {
            Icon::Menu => "\u{f0c9}",
            Icon::Terminal | Icon::FloatingTerminal => "\u{f120}",
            Icon::Apps => "\u{f00a}",
            Icon::Agent => "\u{f0eb}",
            Icon::Project => "\u{f0b1}",
            Icon::Layout | Icon::Split => "\u{f0db}",
            Icon::Theme => "\u{f1fc}",
            Icon::Workspace => "\u{f108}",
            Icon::Session => "\u{f0c0}",
            Icon::Help | Icon::Tutorial => "\u{f059}",
            Icon::Detach => "\u{f08b}",
            Icon::Quit => "\u{f011}",
            Icon::Files => "\u{f07c}",
            Icon::Folder => "\u{f07b}",
            Icon::Monitor => "\u{f0e4}",
            Icon::Clipboard | Icon::Paste => "\u{f0ea}",
            Icon::Network => "\u{f1eb}",
            Icon::Updates => "\u{f021}",
            Icon::Settings => "\u{f013}",
            Icon::Audit => "\u{f0ae}",
            Icon::Notifications => "\u{f0f3}",
            Icon::Keys => "\u{f11c}",
            Icon::Palette => "\u{f002}",
            Icon::About => "\u{f05a}",
            Icon::Center => "\u{f0e4}",
            Icon::ImageViewer | Icon::Image => "\u{f03e}",
            Icon::Markdown | Icon::Text => "\u{f15c}",
            Icon::Git => "\u{f1d3}",
            Icon::Sound => "\u{f028}",
            Icon::Display => "\u{f26c}",
            Icon::Build => "\u{f0ad}",
            Icon::Run => "\u{f04b}",
            Icon::Test => "\u{f00c}",
            Icon::Copy => "\u{f0c5}",
            Icon::SplitDown => "\u{f0ce}",
            Icon::Float => "\u{f2d0}",
            Icon::Zoom => "\u{f065}",
            Icon::Rename => "\u{f044}",
            Icon::Close => "\u{f00d}",
            Icon::Minimize => "\u{f068}",
            Icon::New => "\u{f067}",
            Icon::File => "\u{f15b}",
            Icon::Code => "\u{f121}",
            Icon::Archive => "\u{f1c6}",
            Icon::Executable => "\u{f085}",
            Icon::Link => "\u{f0c1}",
        }
    }

    fn ascii(self) -> &'static str {
        match self {
            Icon::Menu | Icon::Apps => "*",
            Icon::Terminal | Icon::FloatingTerminal | Icon::Palette => ">",
            Icon::Agent => "@",
            Icon::Project | Icon::Build => "#",
            Icon::Layout | Icon::Split | Icon::SplitDown | Icon::Float | Icon::Zoom => "+",
            Icon::Theme => "%",
            Icon::Workspace | Icon::Session => "=",
            Icon::Help | Icon::Tutorial | Icon::About => "?",
            Icon::Center => "@",
            Icon::Detach => "<",
            Icon::Quit | Icon::Close => "x",
            Icon::Files | Icon::Folder => "/",
            Icon::Monitor => "%",
            Icon::Clipboard | Icon::Paste | Icon::Copy => "&",
            Icon::Network => "~",
            Icon::Updates => "^",
            Icon::Settings => "*",
            Icon::Audit | Icon::Markdown | Icon::Text => "#",
            Icon::Notifications => "!",
            Icon::Keys => "k",
            Icon::ImageViewer | Icon::Image => "~",
            Icon::Git => "+",
            Icon::Sound => ")",
            Icon::Display => "[",
            Icon::Run => ">",
            Icon::Test => "v",
            Icon::Rename => "e",
            Icon::Minimize => "-",
            Icon::New => "+",
            Icon::File => "-",
            Icon::Code => "<",
            Icon::Archive => "%",
            Icon::Executable => "*",
            Icon::Link => "@",
        }
    }

    /// The icon for a file named `name` (by its extension).
    pub fn for_file(name: &str, is_dir: bool, is_link: bool, executable: bool) -> Icon {
        if is_link {
            return Icon::Link;
        }
        if is_dir {
            return Icon::Folder;
        }
        let ext = name
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" | "ico" => Icon::Image,
            "md" | "markdown" => Icon::Markdown,
            "txt" | "log" | "csv" | "toml" | "yaml" | "yml" | "json" | "ini" | "conf" => Icon::Text,
            "rs" | "c" | "h" | "cpp" | "hpp" | "cc" | "go" | "py" | "js" | "ts" | "tsx" | "jsx"
            | "java" | "kt" | "cs" | "zig" | "lua" | "sh" | "bash" | "zsh" | "fish" | "ps1"
            | "rb" | "swift" | "html" | "css" | "scss" => Icon::Code,
            "zip" | "tar" | "gz" | "tgz" | "xz" | "zst" | "bz2" | "7z" | "rar" => Icon::Archive,
            "exe" | "msi" | "bat" | "cmd" | "appimage" => Icon::Executable,
            _ if executable => Icon::Executable,
            _ => Icon::File,
        }
    }

    /// Every icon (for tests and previews).
    pub const ALL: [Icon; 52] = [
        Icon::Menu,
        Icon::Terminal,
        Icon::FloatingTerminal,
        Icon::Apps,
        Icon::Agent,
        Icon::Project,
        Icon::Layout,
        Icon::Theme,
        Icon::Workspace,
        Icon::Session,
        Icon::Help,
        Icon::Detach,
        Icon::Quit,
        Icon::Files,
        Icon::Monitor,
        Icon::Clipboard,
        Icon::Network,
        Icon::Updates,
        Icon::Settings,
        Icon::Audit,
        Icon::Notifications,
        Icon::Tutorial,
        Icon::Keys,
        Icon::Palette,
        Icon::About,
        Icon::Center,
        Icon::ImageViewer,
        Icon::Markdown,
        Icon::Git,
        Icon::Sound,
        Icon::Display,
        Icon::Build,
        Icon::Run,
        Icon::Test,
        Icon::Paste,
        Icon::Copy,
        Icon::Split,
        Icon::SplitDown,
        Icon::Float,
        Icon::Zoom,
        Icon::Rename,
        Icon::Close,
        Icon::Minimize,
        Icon::New,
        Icon::Folder,
        Icon::File,
        Icon::Image,
        Icon::Code,
        Icon::Archive,
        Icon::Executable,
        Icon::Link,
        Icon::Text,
    ];
}

/// How often a spinner moves on (D-090).
pub const SPINNER_FRAME: std::time::Duration = std::time::Duration::from_millis(100);

/// Frame `frame` of a spinner, one cell wide: braille dots, or a turning
/// bar for the ASCII set and with no icons.
pub fn spinner(set: IconSet, frame: usize) -> &'static str {
    const DOTS: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    const BAR: [&str; 4] = ["|", "/", "-", "\\"];
    match set {
        IconSet::Unicode | IconSet::Nerd => DOTS[frame % DOTS.len()],
        IconSet::Ascii | IconSet::None => BAR[frame % BAR.len()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    /// Code points that some terminals draw as a wide colour emoji even
    /// without the emoji selector.
    fn emoji_prone(c: char) -> bool {
        matches!(u32::from(c), 0x1F000..=0x1FAFF | 0x2600..=0x26FF)
    }

    #[test]
    fn every_icon_is_one_cell_in_every_set() {
        for icon in Icon::ALL {
            for set in [IconSet::Unicode, IconSet::Nerd, IconSet::Ascii] {
                let g = icon.glyph(set);
                assert_eq!(g.chars().count(), 1, "{icon:?} in {set}: {g:?}");
                assert_eq!(g.width(), 1, "{icon:?} in {set}: {g:?} is not one cell");
            }
            let u = icon
                .glyph(IconSet::Unicode)
                .chars()
                .next()
                .expect("a glyph");
            assert!(!emoji_prone(u), "{icon:?}: {u} may draw as an emoji");
            assert!(icon.glyph(IconSet::Ascii).is_ascii());
            assert_eq!(icon.prefix(IconSet::None), "");
        }
        assert_eq!(Icon::Folder.prefix(IconSet::Unicode), "■ ");
    }

    #[test]
    fn file_icons_by_kind_and_sets_by_name() {
        assert_eq!(Icon::for_file("src", true, false, false), Icon::Folder);
        assert_eq!(Icon::for_file("a.PNG", false, false, false), Icon::Image);
        assert_eq!(
            Icon::for_file("README.md", false, false, false),
            Icon::Markdown
        );
        assert_eq!(Icon::for_file("main.rs", false, false, false), Icon::Code);
        assert_eq!(
            Icon::for_file("x.tar.gz", false, false, false),
            Icon::Archive
        );
        assert_eq!(Icon::for_file("run", false, false, true), Icon::Executable);
        assert_eq!(Icon::for_file("notes", false, false, false), Icon::File);
        assert_eq!(Icon::for_file("l", false, true, false), Icon::Link);
        for set in IconSet::ALL {
            assert_eq!(set.to_string().parse::<IconSet>(), Ok(set));
        }
        assert!("emoji".parse::<IconSet>().is_err());
    }

    #[test]
    fn spinners_turn_in_one_cell() {
        for set in IconSet::ALL {
            let frames: std::collections::BTreeSet<&str> =
                (0..12).map(|f| spinner(set, f)).collect();
            assert!(frames.len() >= 4, "{set}: it moves");
            for f in frames {
                assert_eq!(UnicodeWidthStr::width(f), 1, "{set}: {f:?}");
            }
        }
    }
}
