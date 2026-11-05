// SPDX-License-Identifier: GPL-3.0-or-later
//! Layout files (M13b, D-120): a project's workspaces in one file, their
//! splits, folders and commands.
//!
//! A layout lives in `~/.config/termos/layouts/NAME.toml` (its tables at
//! the top), or as `[layout.NAME]` in a project's `termos.toml`:
//!
//! ```toml
//! [layout.dev]
//! workspace = "api"             # its tab's name
//! cwd = "."                     # relative to the project
//! [[layout.dev.pane]]           # the first pane, then each split from the one before
//! command = "nvim ."
//! [[layout.dev.pane]]
//! split = "right"               # right | down | float
//! size = "35%"
//! command = "cargo watch -x test"
//! [[layout.dev.pane]]
//! split = "down"
//! of = 2                        # split pane 2 (default: the one before)
//! command = "npm run dev"
//! focus = true
//! ```
//!
//! `[[layout.NAME.workspace]]` (each with `name`, `cwd` and `pane`s) gives
//! a layout several workspaces. Errors name the file and line. Parsing is
//! fuzzed by `fuzz/fuzz_targets/layout_parse.rs`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::line_col;

/// Where a pane goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Split {
    Right,
    Down,
    Float,
}

/// One pane of a layout.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct PaneSpec {
    /// How it is split from pane `of` (none for the first pane).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub split: Option<Split>,
    /// Its share of the room it splits, `"35%"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
    /// The pane it splits, counting from 1 (default: the one before).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub of: Option<usize>,
    /// Typed into the pane's shell (so it can be run again), or with
    /// `exec`, run as the pane's program.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub exec: bool,
    /// Its folder, relative to the workspace's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Its name, as `pane.rename` gives.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The pane that has focus when the workspace opens.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub focus: bool,
}

/// One workspace of a layout.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct WorkspaceSpec {
    /// Its tab's name (default: the layout's).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Its folder, relative to the project (or the folder it opens in).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub pane: Vec<PaneSpec>,
}

/// `workspace` in a layout: a tab name, or tables of workspaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
enum WorkspaceField {
    Name(String),
    Many(Vec<WorkspaceSpec>),
}

/// A layout as written.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
struct LayoutFile {
    #[serde(skip_serializing_if = "Option::is_none")]
    workspace: Option<WorkspaceField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pane: Vec<PaneSpec>,
}

/// A layout, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub name: String,
    pub workspaces: Vec<WorkspaceSpec>,
}

/// Why a layout cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LayoutError {
    #[error("{}: cannot read: {message}", path.display())]
    Io { path: PathBuf, message: String },
    #[error("{}:{line}:{col}: {message}", path.display())]
    Parse {
        path: PathBuf,
        line: usize,
        col: usize,
        message: String,
    },
    #[error("{}: layout {layout}: {message}", path.display())]
    Invalid {
        path: PathBuf,
        layout: String,
        message: String,
    },
    #[error("no layout named {0} (in {1} or a project's termos.toml)")]
    NotFound(String, String),
}

/// The most panes one workspace of a layout may have.
pub const MAX_PANES: usize = 32;

/// The most workspaces one layout may have.
pub const MAX_WORKSPACES: usize = 16;

/// `"35%"` as a fraction.
pub fn parse_size(text: &str) -> Option<f64> {
    let n: f64 = text.trim().strip_suffix('%')?.trim().parse().ok()?;
    (n > 0.0 && n < 100.0).then_some(n / 100.0)
}

fn toml_error(src: &str, path: &Path, e: &toml::de::Error) -> LayoutError {
    let (line, col) = e.span().map_or((1, 1), |span| line_col(src, span.start));
    LayoutError::Parse {
        path: path.to_owned(),
        line,
        col,
        message: e.message().trim().to_owned(),
    }
}

impl Layout {
    fn from_file(name: &str, f: LayoutFile, path: &Path) -> Result<Self, LayoutError> {
        let invalid = |message: String| LayoutError::Invalid {
            path: path.to_owned(),
            layout: name.to_owned(),
            message,
        };
        let workspaces = match f.workspace {
            Some(WorkspaceField::Many(many)) => {
                if !f.pane.is_empty() {
                    return Err(invalid(
                        "give panes either in each [[workspace]] or at the top, not both".into(),
                    ));
                }
                many.into_iter()
                    .map(|mut w| {
                        if w.cwd.is_none() {
                            w.cwd.clone_from(&f.cwd);
                        }
                        w
                    })
                    .collect()
            }
            Some(WorkspaceField::Name(n)) => vec![WorkspaceSpec {
                name: Some(n),
                cwd: f.cwd,
                pane: f.pane,
            }],
            None => vec![WorkspaceSpec {
                name: None,
                cwd: f.cwd,
                pane: f.pane,
            }],
        };
        if workspaces.is_empty() {
            return Err(invalid("it has no workspace".into()));
        }
        if workspaces.len() > MAX_WORKSPACES {
            return Err(invalid(format!(
                "at most {MAX_WORKSPACES} workspaces in one layout"
            )));
        }
        for (wi, w) in workspaces.iter().enumerate() {
            let at = |i: usize| {
                if workspaces.len() > 1 {
                    format!("workspace {}, pane {}", wi + 1, i + 1)
                } else {
                    format!("pane {}", i + 1)
                }
            };
            if w.pane.len() > MAX_PANES {
                return Err(invalid(format!(
                    "at most {MAX_PANES} panes in one workspace"
                )));
            }
            for (i, p) in w.pane.iter().enumerate() {
                if i == 0 && (p.split.is_some() || p.of.is_some()) {
                    return Err(invalid(format!(
                        "{}: the first pane is not split from another",
                        at(i)
                    )));
                }
                if let Some(of) = p.of
                    && (of == 0 || of > i)
                {
                    return Err(invalid(format!(
                        "{}: `of = {of}` names no pane before it",
                        at(i)
                    )));
                }
                if let Some(size) = &p.size
                    && parse_size(size).is_none()
                {
                    return Err(invalid(format!(
                        "{}: size {size:?} is not a share like \"35%\"",
                        at(i)
                    )));
                }
                if p.exec && p.command.as_deref().is_none_or(|c| c.trim().is_empty()) {
                    return Err(invalid(format!("{}: `exec` needs a command", at(i))));
                }
            }
            if w.pane.iter().filter(|p| p.focus).count() > 1 {
                return Err(invalid("only one pane may have the focus".into()));
            }
        }
        Ok(Self {
            name: name.to_owned(),
            workspaces,
        })
    }

    /// A layout file's text (`layouts/NAME.toml`): its tables at the top.
    pub fn parse(name: &str, src: &str, path: &Path) -> Result<Self, LayoutError> {
        let f: LayoutFile = toml::from_str(src).map_err(|e| toml_error(src, path, &e))?;
        Self::from_file(name, f, path)
    }

    /// The text of a layout file that opens this layout again.
    pub fn to_toml(&self) -> String {
        let file = match self.workspaces.as_slice() {
            [one] => LayoutFile {
                workspace: one.name.clone().map(WorkspaceField::Name),
                cwd: one.cwd.clone(),
                pane: one.pane.clone(),
            },
            many => LayoutFile {
                workspace: Some(WorkspaceField::Many(many.to_vec())),
                cwd: None,
                pane: Vec::new(),
            },
        };
        toml::to_string_pretty(&file).unwrap_or_default()
    }
}

/// `[layout.NAME]` tables in a project's `termos.toml` (other tables are
/// someone else's).
#[derive(Debug, Default, Deserialize)]
struct ProjectFile {
    #[serde(default)]
    layout: BTreeMap<String, LayoutFile>,
}

/// The layouts in a project's `termos.toml` text.
pub fn parse_project(src: &str, path: &Path) -> Result<Vec<Layout>, LayoutError> {
    let f: ProjectFile = toml::from_str(src).map_err(|e| toml_error(src, path, &e))?;
    f.layout
        .into_iter()
        .map(|(name, l)| Layout::from_file(&name, l, path))
        .collect()
}

/// The nearest `termos.toml` in `dir` or a folder above it.
pub fn project_file(dir: &Path) -> Option<PathBuf> {
    dir.ancestors()
        .map(|d| d.join("termos.toml"))
        .find(|p| p.is_file())
}

/// Whether `name` can be a layout's file name.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !name.starts_with('.')
}

/// A layout found by name: the project's (from `dir` up) first, then the
/// user's file. The folder its relative paths start from comes with it:
/// the project's, or `dir` for the user's.
pub fn find(name: &str, layouts_dir: &Path, dir: &Path) -> Result<(Layout, PathBuf), LayoutError> {
    if let Some(file) = project_file(dir) {
        let src = std::fs::read_to_string(&file).map_err(|e| LayoutError::Io {
            path: file.clone(),
            message: e.to_string(),
        })?;
        let root = file.parent().map_or_else(|| dir.to_owned(), Path::to_owned);
        if let Some(l) = parse_project(&src, &file)?
            .into_iter()
            .find(|l| l.name == name)
        {
            return Ok((l, root));
        }
    }
    if valid_name(name) {
        let file = layouts_dir.join(format!("{name}.toml"));
        match std::fs::read_to_string(&file) {
            Ok(src) => return Ok((Layout::parse(name, &src, &file)?, dir.to_owned())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(LayoutError::Io {
                    path: file,
                    message: e.to_string(),
                });
            }
        }
    }
    Err(LayoutError::NotFound(
        name.to_owned(),
        layouts_dir.display().to_string(),
    ))
}

/// Every layout's name: the project's (from `dir` up), then the user's.
pub fn names(layouts_dir: &Path, dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = project_file(dir)
        .and_then(|f| {
            let src = std::fs::read_to_string(&f).ok()?;
            parse_project(&src, &f).ok()
        })
        .unwrap_or_default()
        .into_iter()
        .map(|l| l.name)
        .collect();
    if let Ok(entries) = std::fs::read_dir(layouts_dir) {
        let mut mine: Vec<String> = entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.strip_suffix(".toml").map(str::to_owned)
            })
            .filter(|n| valid_name(n) && !out.contains(n))
            .collect();
        mine.sort();
        out.extend(mine);
    }
    out
}

/// A folder of a layout (`cwd`), relative to `base`; `~` is the home
/// folder.
pub fn resolve_dir(base: &Path, rel: Option<&str>) -> PathBuf {
    let Some(rel) = rel.map(str::trim).filter(|r| !r.is_empty()) else {
        return base.to_owned();
    };
    if let Some(rest) = rel.strip_prefix('~')
        && (rest.is_empty() || rest.starts_with(['/', '\\']))
        && let Some(home) = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .filter(|h| !h.is_empty())
    {
        return PathBuf::from(home).join(rest.trim_start_matches(['/', '\\']));
    }
    let p = Path::new(rel);
    if p.is_absolute() {
        p.to_owned()
    } else {
        base.join(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEV: &str = r#"
workspace = "api"
cwd = "."
[[pane]]
command = "nvim ."
[[pane]]
split = "right"
size = "35%"
command = "cargo watch -x test"
[[pane]]
split = "down"
of = 2
command = "npm run dev"
focus = true
"#;

    #[test]
    fn a_layout_file_parses() {
        let l = Layout::parse("dev", DEV, Path::new("dev.toml")).expect("valid");
        assert_eq!(l.workspaces.len(), 1);
        let w = &l.workspaces[0];
        assert_eq!(w.name.as_deref(), Some("api"));
        assert_eq!(w.pane.len(), 3);
        assert_eq!(w.pane[1].split, Some(Split::Right));
        assert_eq!(w.pane[1].size.as_deref().and_then(parse_size), Some(0.35));
        assert_eq!(w.pane[2].of, Some(2));
        assert!(w.pane[2].focus);
        // Written out and read again: the same layout.
        let again = Layout::parse("dev", &l.to_toml(), Path::new("dev.toml")).expect("valid");
        assert_eq!(again, l);
    }

    #[test]
    fn errors_name_the_line_or_the_pane() {
        let e = Layout::parse("x", "[[pane]]\nsplit = \"sideways\"\n", Path::new("x.toml"))
            .expect_err("bad split");
        assert!(e.to_string().starts_with("x.toml:2:"), "{e}");
        let e = Layout::parse("x", "[[pane]]\ncolour = 1\n", Path::new("x.toml"))
            .expect_err("unknown key");
        assert!(e.to_string().starts_with("x.toml:2:"), "{e}");
        for (src, says) in [
            ("[[pane]]\nsplit = \"right\"\n", "the first pane"),
            ("[[pane]]\n[[pane]]\nof = 3\n", "pane 2: `of = 3`"),
            ("[[pane]]\n[[pane]]\nsize = \"big\"\n", "pane 2: size"),
            ("[[pane]]\nexec = true\n", "`exec` needs a command"),
            (
                "[[pane]]\nfocus = true\n[[pane]]\nfocus = true\n",
                "only one pane",
            ),
        ] {
            let e = Layout::parse("x", src, Path::new("x.toml")).expect_err(src);
            assert!(e.to_string().contains(says), "{src}: {e}");
        }
    }

    #[test]
    fn several_workspaces_and_project_files() {
        let src = r#"
[tasks]
build = "make"
[layout.full]
cwd = "srv"
[[layout.full.workspace]]
name = "code"
[[layout.full.workspace.pane]]
command = "nvim"
[[layout.full.workspace]]
name = "logs"
cwd = "/var/log"
[[layout.full.workspace.pane]]
command = "tail -f syslog"
[layout.small]
[[layout.small.pane]]
"#;
        let ls = parse_project(src, Path::new("termos.toml")).expect("valid");
        assert_eq!(ls.len(), 2);
        let full = ls.iter().find(|l| l.name == "full").expect("full");
        assert_eq!(full.workspaces.len(), 2);
        assert_eq!(full.workspaces[0].cwd.as_deref(), Some("srv"));
        assert_eq!(full.workspaces[1].cwd.as_deref(), Some("/var/log"));
        let again = Layout::parse("full", &full.to_toml(), Path::new("full.toml")).expect("valid");
        assert_eq!(&again, full);
    }

    #[test]
    fn layouts_are_found_project_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        let project = dir.path().join("proj");
        let deep = project.join("src/deep");
        std::fs::create_dir_all(&deep).expect("mkdir");
        std::fs::write(
            project.join("termos.toml"),
            "[layout.dev]\nworkspace = \"proj\"\n[[layout.dev.pane]]\n",
        )
        .expect("write");
        let mine = dir.path().join("layouts");
        std::fs::create_dir_all(&mine).expect("mkdir");
        std::fs::write(mine.join("dev.toml"), "workspace = \"mine\"\n[[pane]]\n").expect("write");
        std::fs::write(mine.join("ops.toml"), "[[pane]]\n").expect("write");
        let (l, root) = find("dev", &mine, &deep).expect("found");
        assert_eq!(l.workspaces[0].name.as_deref(), Some("proj"));
        assert_eq!(root, project);
        let (l, root) = find("ops", &mine, &deep).expect("found");
        assert_eq!(l.name, "ops");
        assert_eq!(root, deep);
        assert!(matches!(
            find("none", &mine, &deep),
            Err(LayoutError::NotFound(..))
        ));
        assert!(matches!(
            find("../x", &mine, &deep),
            Err(LayoutError::NotFound(..))
        ));
        assert_eq!(names(&mine, &deep), vec!["dev", "ops"]);
    }

    #[test]
    fn sizes_and_folders() {
        assert_eq!(parse_size("35%"), Some(0.35));
        assert_eq!(parse_size(" 50 % "), Some(0.5));
        assert_eq!(parse_size("0%"), None);
        assert_eq!(parse_size("100%"), None);
        assert_eq!(parse_size("35"), None);
        let base = Path::new("/p");
        assert_eq!(resolve_dir(base, None), PathBuf::from("/p"));
        assert_eq!(resolve_dir(base, Some("srv")), PathBuf::from("/p/srv"));
        if cfg!(unix) {
            assert_eq!(resolve_dir(base, Some("/etc")), PathBuf::from("/etc"));
        }
        assert!(valid_name("my-layout_2"));
        assert!(!valid_name("../evil"));
        assert!(!valid_name(".hidden"));
    }
}
