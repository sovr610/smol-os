// SPDX-License-Identifier: GPL-3.0-or-later
//! Saved commands (M13b, D-120): `[[snippets]]` in the config and in a
//! project's `termos.toml`.
//!
//! ```toml
//! [[snippets]]
//! name = "Tail a service's log"
//! command = "journalctl -fu {service:termos}"
//! run = true                 # press Enter after typing it (default false)
//! where = "pane"             # pane (the focused one) | new-pane | float
//! ```
//!
//! `{param}` and `{param:default}` are asked for, in order, before the
//! line is pasted; `{{` and `}}` are braces. Parameters are plain text:
//! the docs say not to put secrets in them.

use std::path::Path;

use serde::Deserialize;

/// Where a snippet is typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Where {
    /// The focused pane.
    #[default]
    Pane,
    /// A new pane split from it.
    NewPane,
    /// A new floating window.
    Float,
}

/// A saved command.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Snippet {
    pub name: String,
    pub command: String,
    /// Press Enter after typing it.
    #[serde(default)]
    pub run: bool,
    #[serde(default, rename = "where")]
    pub place: Where,
}

/// A parameter: its name and default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    pub name: String,
    pub default: Option<String>,
}

/// A piece of a command: text, or a parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    Text(String),
    Param(Param),
}

fn pieces(command: &str) -> Vec<Piece> {
    let mut out = Vec::new();
    let mut text = String::new();
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                text.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                text.push('}');
            }
            '{' => {
                let mut inner = String::new();
                let mut closed = false;
                for c in chars.by_ref() {
                    if c == '}' {
                        closed = true;
                        break;
                    }
                    inner.push(c);
                }
                let (name, default) = match inner.split_once(':') {
                    Some((n, d)) => (n.trim(), Some(d.to_owned())),
                    None => (inner.trim(), None),
                };
                if closed
                    && !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | ' '))
                {
                    if !text.is_empty() {
                        out.push(Piece::Text(std::mem::take(&mut text)));
                    }
                    out.push(Piece::Param(Param {
                        name: name.to_owned(),
                        default,
                    }));
                } else {
                    // Not a parameter: the text as written.
                    text.push('{');
                    text.push_str(&inner);
                    if closed {
                        text.push('}');
                    }
                }
            }
            c => text.push(c),
        }
    }
    if !text.is_empty() {
        out.push(Piece::Text(text));
    }
    out
}

/// The parameters `command` asks for, in order, each once.
pub fn params(command: &str) -> Vec<Param> {
    let mut out: Vec<Param> = Vec::new();
    for p in pieces(command) {
        if let Piece::Param(p) = p
            && !out.iter().any(|o| o.name == p.name)
        {
            out.push(p);
        }
    }
    out
}

/// `command` with each parameter replaced by its value (by name), else its
/// default (a default given at one use counts for every use), else nothing.
pub fn fill(command: &str, values: &[(String, String)]) -> String {
    let known = params(command);
    pieces(command)
        .into_iter()
        .map(|p| match p {
            Piece::Text(t) => t,
            Piece::Param(p) => values
                .iter()
                .find(|(n, _)| *n == p.name)
                .map(|(_, v)| v.clone())
                .or(p.default)
                .or_else(|| {
                    known
                        .iter()
                        .find(|k| k.name == p.name)
                        .and_then(|k| k.default.clone())
                })
                .unwrap_or_default(),
        })
        .collect()
}

/// `[[snippets]]` in a project's `termos.toml` (other tables are someone
/// else's).
#[derive(Debug, Default, Deserialize)]
struct ProjectFile {
    #[serde(default)]
    snippets: Vec<Snippet>,
}

/// The snippets of the project `dir` is in, if its `termos.toml` has any.
pub fn project_snippets(dir: &Path) -> Vec<Snippet> {
    let Some(file) = crate::layouts::project_file(dir) else {
        return Vec::new();
    };
    let Ok(src) = std::fs::read_to_string(&file) else {
        return Vec::new();
    };
    match toml::from_str::<ProjectFile>(&src) {
        Ok(f) => f.snippets,
        Err(e) => {
            tracing::warn!("{}: snippets: {}", file.display(), e.message().trim());
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameters_in_order_with_defaults() {
        let c = "journalctl -fu {service:termos} --since {since} {service}";
        assert_eq!(
            params(c),
            vec![
                Param {
                    name: "service".into(),
                    default: Some("termos".into())
                },
                Param {
                    name: "since".into(),
                    default: None
                },
            ]
        );
        assert_eq!(
            fill(c, &[("since".into(), "1h ago".into())]),
            "journalctl -fu termos --since 1h ago termos"
        );
    }

    #[test]
    fn braces_that_are_not_parameters_stay() {
        assert_eq!(params("awk '{print $1}' {{x}} {"), Vec::new());
        assert_eq!(fill("awk '{print $1}' {{x}}", &[]), "awk '{print $1}' {x}");
        assert_eq!(fill("echo {", &[]), "echo {");
    }

    #[test]
    fn project_snippets_are_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("termos.toml"),
            "[tasks]\nbuild = \"make\"\n[[snippets]]\nname = \"deploy\"\ncommand = \"make deploy ENV={env:staging}\"\nrun = true\nwhere = \"new-pane\"\n",
        )
        .expect("write");
        let s = project_snippets(dir.path());
        assert_eq!(s.len(), 1);
        assert!(s[0].run);
        assert_eq!(s[0].place, Where::NewPane);
    }
}
