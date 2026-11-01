// SPDX-License-Identifier: GPL-3.0-or-later
//! Key tables: the tmux-compatible defaults plus config overrides
//! (DECISIONS.md D-023).

use termos_core::config::Config;
use termos_core::input::{KeyCode, KeyEvent, Mods};
use termos_wm::layout::{Direction, Preset};
use termos_wm::workspace::SplitHow;

use crate::commands::Command;

/// A key bound to a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub key: KeyEvent,
    pub command: Command,
    /// Can be pressed again without the prefix within the repeat time
    /// (tmux `bind -r`).
    pub repeat: bool,
}

/// The prefix key and the two binding tables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyTables {
    pub prefix_key: KeyEvent,
    /// Bindings after the prefix, in display order.
    pub prefix: Vec<Binding>,
    /// Bindings without the prefix (tmux `bind -n`).
    pub root: Vec<Binding>,
}

fn key(spec: &str) -> KeyEvent {
    // Only called with the literal specs below, which the tests check.
    spec.parse().unwrap_or(KeyEvent::plain(KeyCode::F(24)))
}

fn b(spec: &str, command: Command) -> Binding {
    Binding {
        key: key(spec),
        command,
        repeat: false,
    }
}

fn r(spec: &str, command: Command) -> Binding {
    Binding {
        key: key(spec),
        command,
        repeat: true,
    }
}

/// The default prefix table (tmux defaults where tmux has an equivalent).
pub fn default_prefix_bindings() -> Vec<Binding> {
    use Command as C;
    use Direction::{Down, Left, Right, Up};
    let mut v = vec![
        b("%", C::Split(SplitHow::Right)),
        b("|", C::Split(SplitHow::Right)),
        b("\"", C::Split(SplitHow::Down)),
        b("-", C::Split(SplitHow::Down)),
        b("Left", C::Focus(Left)),
        b("Right", C::Focus(Right)),
        b("Up", C::Focus(Up)),
        b("Down", C::Focus(Down)),
        r("C-Left", C::Resize(Left, 1)),
        r("C-Right", C::Resize(Right, 1)),
        r("C-Up", C::Resize(Up, 1)),
        r("C-Down", C::Resize(Down, 1)),
        r("M-Left", C::Resize(Left, 5)),
        r("M-Right", C::Resize(Right, 5)),
        r("M-Up", C::Resize(Up, 5)),
        r("M-Down", C::Resize(Down, 5)),
        b("o", C::FocusNext),
        b(";", C::FocusLast),
        b("{", C::SwapPrev),
        b("}", C::SwapNext),
        b("z", C::Zoom),
        b("x", C::KillPane),
        b("Space", C::LayoutNext),
        b("f", C::ToggleFloat),
        b("F", C::NewFloat),
        b("m", C::Minimize),
        b("M", C::RestoreMinimized),
        b("c", C::WorkspaceNew),
        b("n", C::WorkspaceNext),
        b("p", C::WorkspacePrev),
        b("l", C::WorkspaceLast),
        b(",", C::WorkspaceRename),
        b("&", C::WorkspaceKill),
        b("[", C::CopyMode),
        b("PPage", C::CopyModePageUp),
        b("]", C::Paste),
        b(":", C::Palette),
        b("?", C::Keys),
        b("N", C::Notifications),
        b("F1", C::Tutorial),
        b("r", C::ReloadConfig),
        b("d", C::Detach),
        b("(", C::SessionPrev),
        b(")", C::SessionNext),
        b("$", C::SessionRename),
    ];
    for (i, preset) in Preset::ALL.iter().enumerate() {
        v.push(b(&format!("M-{}", i + 1), C::LayoutSelect(*preset)));
    }
    for n in 0..=9 {
        v.push(b(&n.to_string(), C::WorkspaceSelect(n)));
    }
    v
}

/// The default root table.
pub fn default_root_bindings() -> Vec<Binding> {
    vec![b("C-Space", Command::Palette)]
}

fn is_repeatable(c: &Command) -> bool {
    matches!(c, Command::Resize(..))
}

/// Applies `overrides` (key → command or `"none"`) to `table`, collecting
/// errors for unknown commands.
fn apply_overrides(
    table: &mut Vec<Binding>,
    overrides: &[(KeyEvent, String)],
    section: &str,
    errors: &mut Vec<String>,
) {
    for (k, value) in overrides {
        table.retain(|b| b.key != *k);
        if value.trim() == "none" {
            continue;
        }
        match value.parse::<Command>() {
            Ok(command) => {
                let repeat = is_repeatable(&command);
                table.push(Binding {
                    key: *k,
                    command,
                    repeat,
                });
            }
            Err(e) => errors.push(format!("[keys.{section}] \"{k}\": {e}")),
        }
    }
}

impl KeyTables {
    /// Builds the tables from the config. Errors (unknown commands) are
    /// returned alongside the usable tables; bad entries are skipped.
    pub fn from_config(config: &Config) -> (Self, Vec<String>) {
        let mut errors = Vec::new();
        let mut prefix = default_prefix_bindings();
        // Pressing the prefix twice sends it to the program (tmux).
        prefix.push(Binding {
            key: config.prefix,
            command: Command::SendPrefix,
            repeat: false,
        });
        let mut root = default_root_bindings();
        apply_overrides(&mut prefix, &config.keys.prefix, "prefix", &mut errors);
        apply_overrides(&mut root, &config.keys.root, "root", &mut errors);
        if root.iter().any(|b| b.key == config.prefix) {
            errors.push(format!(
                "[keys.root] binds the prefix key {}; that binding is ignored",
                config.prefix
            ));
            root.retain(|b| b.key != config.prefix);
        }
        (
            Self {
                prefix_key: config.prefix,
                prefix,
                root,
            },
            errors,
        )
    }

    pub fn lookup_prefix(&self, key: KeyEvent) -> Option<Binding> {
        self.prefix.iter().rev().find(|b| b.key == key).cloned()
    }

    pub fn lookup_root(&self, key: KeyEvent) -> Option<Binding> {
        self.root.iter().rev().find(|b| b.key == key).cloned()
    }

    /// The first key bound to `command` (prefix table first), for hints.
    pub fn key_for(&self, command: &Command) -> Option<String> {
        if let Some(b) = self.root.iter().find(|b| b.command == *command) {
            return Some(b.key.to_string());
        }
        self.prefix
            .iter()
            .find(|b| b.command == *command)
            .map(|b| format!("{} {}", self.prefix_key, b.key))
    }
}

/// True for keys that mean "cancel" in overlays.
pub fn is_cancel(k: KeyEvent) -> bool {
    k == KeyEvent::plain(KeyCode::Esc) || k == KeyEvent::ctrl('c') || k == KeyEvent::ctrl('g')
}

/// True for plain printable characters (text input).
pub fn printable(k: KeyEvent) -> Option<char> {
    match k.code {
        KeyCode::Char(c) if !k.mods.intersects(Mods::CTRL | Mods::ALT | Mods::SUPER) => Some(c),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn tables(src: &str) -> (KeyTables, Vec<String>) {
        let cfg = Config::parse(src, Path::new("t.toml")).expect("valid config");
        KeyTables::from_config(&cfg)
    }

    #[test]
    fn defaults_are_tmux_like() {
        let (t, errors) = tables("");
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(t.prefix_key, KeyEvent::ctrl('b'));
        let get = |s: &str| t.lookup_prefix(s.parse().expect("key")).map(|b| b.command);
        assert_eq!(get("%"), Some(Command::Split(SplitHow::Right)));
        assert_eq!(get("\""), Some(Command::Split(SplitHow::Down)));
        assert_eq!(get("c"), Some(Command::WorkspaceNew));
        assert_eq!(get("z"), Some(Command::Zoom));
        assert_eq!(get("3"), Some(Command::WorkspaceSelect(3)));
        assert_eq!(get("C-b"), Some(Command::SendPrefix));
        assert_eq!(get("M-5"), Some(Command::LayoutSelect(Preset::Tiled)));
        assert!(t.lookup_prefix(KeyEvent::ch('C')).is_none());
        assert!(
            t.lookup_prefix("C-Left".parse().expect("key"))
                .is_some_and(|b| b.repeat)
        );
        assert_eq!(
            t.lookup_root(KeyEvent::ctrl(' ')).map(|b| b.command),
            Some(Command::Palette)
        );
        assert_eq!(t.key_for(&Command::Palette).as_deref(), Some("C-Space"));
        assert_eq!(t.key_for(&Command::Zoom).as_deref(), Some("C-b z"));
        // No duplicate keys in the default prefix table.
        let mut keys: Vec<String> = t.prefix.iter().map(|b| b.key.to_string()).collect();
        let n = keys.len();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), n, "duplicate default bindings");
    }

    #[test]
    fn overrides_and_unbinding() {
        let (t, errors) = tables(
            "prefix = \"C-a\"\n[keys.prefix]\n\"v\" = \"pane.split-right\"\n\"%\" = \"none\"\n\"H\" = \"pane.resize-left 5\"\n[keys.root]\n\"M-Left\" = \"pane.focus-left\"\n",
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(t.prefix_key, KeyEvent::ctrl('a'));
        assert!(t.lookup_prefix(KeyEvent::ch('%')).is_none());
        assert_eq!(
            t.lookup_prefix(KeyEvent::ch('v')).map(|b| b.command),
            Some(Command::Split(SplitHow::Right))
        );
        assert!(t.lookup_prefix(KeyEvent::ch('H')).is_some_and(|b| b.repeat));
        assert_eq!(
            t.lookup_prefix(KeyEvent::ctrl('a')).map(|b| b.command),
            Some(Command::SendPrefix)
        );
        assert!(t.lookup_prefix(KeyEvent::ctrl('b')).is_none());
        assert!(t.lookup_root("M-Left".parse().expect("key")).is_some());
    }

    #[test]
    fn bad_commands_are_reported() {
        let (t, errors) =
            tables("[keys.prefix]\n\"v\" = \"pane.explode\"\n[keys.root]\n\"C-b\" = \"palette\"\n");
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(errors[0].contains("pane.explode"));
        assert!(t.lookup_prefix(KeyEvent::ch('v')).is_none());
        assert!(t.lookup_root(KeyEvent::ctrl('b')).is_none());
    }
}
