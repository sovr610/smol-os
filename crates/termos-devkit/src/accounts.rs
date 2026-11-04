// SPDX-License-Identifier: GPL-3.0-or-later
//! Several accounts of each agent CLI (M11a, D-096, D-097).
//!
//! An account is a profile: a folder of the CLI's own (Claude Code's
//! `CLAUDE_CONFIG_DIR`, Codex's `CODEX_HOME`) holding its login, settings
//! and conversations. `default` is the CLI's usual folder (`~/.claude`,
//! `~/.codex`), so a login made before TermOS keeps working; the others
//! live in `<data>/accounts/<cli>/<name>/`. The user's settings,
//! instructions and skills are shared from the default folder into each
//! new one (linked where the system allows, else copied).
//!
//! The index (`<data>/accounts/accounts.toml`) holds each account's label
//! and budgets and which one is in use; TermOS never reads what is inside
//! an account's folder beyond its session logs (D-096).

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::cli_agents::Tool;

/// The name of the CLI's own folder.
pub const DEFAULT: &str = "default";

#[derive(Debug, thiserror::Error)]
pub enum AccountError {
    #[error("{0}")]
    Invalid(String),
    #[error("no {tool} account named {name} (there are {known})")]
    Unknown {
        tool: &'static str,
        name: String,
        known: String,
    },
    #[error("there is already a {0} account named {1}")]
    Exists(&'static str, String),
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("the account list ({0}) does not parse: {1}")]
    Index(PathBuf, String),
}

/// What TermOS keeps about an account.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Meta {
    /// Shown instead of the name ("Work (Max)").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Tokens the user allows in Claude Code's 5-hour window (a gauge in
    /// the usage app; nothing is enforced).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_5h: Option<u64>,
    /// Tokens the user allows in 7 days.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_week: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct Index {
    /// The account in use, by CLI.
    active: BTreeMap<String, String>,
    /// Accounts by CLI, then name (`default` may be listed for its label).
    accounts: BTreeMap<String, BTreeMap<String, Meta>>,
}

/// One account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub tool: Tool,
    pub name: String,
    pub meta: Meta,
    /// Its folder; `None` for `default`, which is wherever the CLI keeps
    /// its own (see [`Account::home`]).
    pub dir: Option<PathBuf>,
}

impl Account {
    /// "Work (Max)", else the name.
    pub fn title(&self) -> String {
        self.meta.label.clone().unwrap_or_else(|| self.name.clone())
    }

    pub fn is_default(&self) -> bool {
        self.dir.is_none()
    }

    /// The variable that points the CLI at its folder.
    pub fn variable(tool: Tool) -> &'static str {
        match tool {
            Tool::Claude => "CLAUDE_CONFIG_DIR",
            Tool::Codex => "CODEX_HOME",
        }
    }

    /// The environment the CLI runs with for this account (empty for
    /// `default`, which the CLI finds itself).
    pub fn env(&self) -> Vec<(String, String)> {
        self.dir
            .iter()
            .map(|d| {
                (
                    Self::variable(self.tool).to_owned(),
                    d.display().to_string(),
                )
            })
            .collect()
    }

    /// The folder the CLI uses for this account: its own, or for
    /// `default`, `$CLAUDE_CONFIG_DIR` / `$CODEX_HOME` from the
    /// environment, else `~/.claude` / `~/.codex`.
    pub fn home(
        &self,
        get: &dyn Fn(&str) -> Option<String>,
        home: Option<&Path>,
    ) -> Option<PathBuf> {
        if let Some(d) = &self.dir {
            return Some(d.clone());
        }
        default_home(self.tool, get, home)
    }

    /// Claude Code's settings file for this account (`.claude.json`, which
    /// holds its user-scope MCP servers): inside the folder for a
    /// TermOS account; for `default`, `~/.claude.json` unless
    /// `$CLAUDE_CONFIG_DIR` moves it.
    pub fn claude_settings(
        &self,
        get: &dyn Fn(&str) -> Option<String>,
        home: Option<&Path>,
    ) -> Option<PathBuf> {
        match &self.dir {
            Some(d) => Some(d.join(".claude.json")),
            None => crate::cli_agents::claude_settings(get, home),
        }
    }
}

/// The CLI's own folder (the `default` account's).
pub fn default_home(
    tool: Tool,
    get: &dyn Fn(&str) -> Option<String>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    get(Account::variable(tool))
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.map(|h| match tool {
                Tool::Claude => h.join(".claude"),
                Tool::Codex => h.join(".codex"),
            })
        })
}

/// A name for an account: letters, digits, `-` and `_`, up to 32.
pub fn check_name(name: &str) -> Result<(), AccountError> {
    let ok = !name.is_empty()
        && name.len() <= 32
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(AccountError::Invalid(format!(
            "{name:?} is not an account name (letters, digits, - and _, up to 32)"
        )))
    }
}

/// What a new Claude Code account shares with the default one.
const CLAUDE_SHARED: &[&str] = &[
    "settings.json",
    "CLAUDE.md",
    "commands",
    "agents",
    "skills",
    "output-styles",
    "plugins",
];
/// What a new Codex account shares (its `config.toml` is copied, as TermOS
/// edits it per account).
const CODEX_SHARED: &[&str] = &["AGENTS.md", "prompts", "skills"];
const CODEX_COPIED: &[&str] = &["config.toml"];

/// The accounts of every CLI.
#[derive(Debug)]
pub struct Accounts {
    root: PathBuf,
    index: Index,
}

impl Accounts {
    /// The accounts under `data_dir` (`<data>/accounts`).
    pub fn load(data_dir: &Path) -> Result<Self, AccountError> {
        let root = data_dir.join("accounts");
        let file = root.join("accounts.toml");
        let index = match std::fs::read_to_string(&file) {
            Ok(text) => {
                toml::from_str(&text).map_err(|e| AccountError::Index(file, e.to_string()))?
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Index::default(),
            Err(e) => return Err(e.into()),
        };
        Ok(Self { root, index })
    }

    fn save(&self) -> Result<(), AccountError> {
        std::fs::create_dir_all(&self.root)?;
        let text = toml::to_string(&self.index)
            .map_err(|e| AccountError::Index(self.root.join("accounts.toml"), e.to_string()))?;
        let file = self.root.join("accounts.toml");
        let tmp = self
            .root
            .join(format!(".accounts.toml.{}", std::process::id()));
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &file)?;
        Ok(())
    }

    fn dir_of(&self, tool: Tool, name: &str) -> PathBuf {
        self.root.join(tool.name()).join(name)
    }

    fn account(&self, tool: Tool, name: &str) -> Account {
        let meta = self
            .index
            .accounts
            .get(tool.name())
            .and_then(|m| m.get(name))
            .cloned()
            .unwrap_or_default();
        Account {
            tool,
            name: name.to_owned(),
            meta,
            dir: (name != DEFAULT).then(|| self.dir_of(tool, name)),
        }
    }

    /// `default` first, then the others by name.
    pub fn list(&self, tool: Tool) -> Vec<Account> {
        let mut out = vec![self.account(tool, DEFAULT)];
        if let Some(m) = self.index.accounts.get(tool.name()) {
            out.extend(
                m.keys()
                    .filter(|n| n.as_str() != DEFAULT)
                    .map(|n| self.account(tool, n)),
            );
        }
        out
    }

    pub fn get(&self, tool: Tool, name: &str) -> Result<Account, AccountError> {
        let known = self
            .index
            .accounts
            .get(tool.name())
            .is_some_and(|m| m.contains_key(name));
        if name == DEFAULT || known {
            Ok(self.account(tool, name))
        } else {
            Err(AccountError::Unknown {
                tool: tool.title(),
                name: name.to_owned(),
                known: self
                    .list(tool)
                    .iter()
                    .map(|a| a.name.clone())
                    .collect::<Vec<_>>()
                    .join(", "),
            })
        }
    }

    /// The account in use (`default` unless one was chosen).
    pub fn active(&self, tool: Tool) -> Account {
        let name = self
            .index
            .active
            .get(tool.name())
            .filter(|n| {
                n.as_str() == DEFAULT
                    || self
                        .index
                        .accounts
                        .get(tool.name())
                        .is_some_and(|m| m.contains_key(n.as_str()))
            })
            .map_or(DEFAULT, String::as_str);
        self.account(tool, name)
    }

    /// Makes `name` the account new windows use.
    pub fn use_account(&mut self, tool: Tool, name: &str) -> Result<Account, AccountError> {
        let a = self.get(tool, name)?;
        if name == DEFAULT {
            self.index.active.remove(tool.name());
        } else {
            self.index
                .active
                .insert(tool.name().to_owned(), name.to_owned());
        }
        self.save()?;
        Ok(a)
    }

    /// A new account, its folder made and the default account's settings,
    /// instructions and skills shared into it from `default_home`.
    pub fn add(
        &mut self,
        tool: Tool,
        name: &str,
        label: Option<&str>,
        default_home: Option<&Path>,
    ) -> Result<Account, AccountError> {
        check_name(name)?;
        if name == DEFAULT || self.get(tool, name).is_ok() {
            return Err(AccountError::Exists(tool.title(), name.to_owned()));
        }
        let dir = self.dir_of(tool, name);
        create_private_dir(&dir)?;
        if let Some(from) = default_home.filter(|d| d.is_dir()) {
            share(tool, from, &dir)?;
        }
        self.index
            .accounts
            .entry(tool.name().to_owned())
            .or_default()
            .insert(
                name.to_owned(),
                Meta {
                    label: label.map(str::to_owned).filter(|l| !l.is_empty()),
                    ..Meta::default()
                },
            );
        self.save()?;
        Ok(self.account(tool, name))
    }

    /// Removes an account and its folder (its login and conversations).
    /// `default` is the CLI's own and stays.
    pub fn remove(&mut self, tool: Tool, name: &str) -> Result<Account, AccountError> {
        if name == DEFAULT {
            return Err(AccountError::Invalid(format!(
                "`default` is {}'s own folder; TermOS does not remove it",
                tool.title()
            )));
        }
        let a = self.get(tool, name)?;
        if let Some(m) = self.index.accounts.get_mut(tool.name()) {
            m.remove(name);
        }
        if self
            .index
            .active
            .get(tool.name())
            .is_some_and(|n| n == name)
        {
            self.index.active.remove(tool.name());
        }
        self.save()?;
        if let Some(d) = &a.dir
            && d.exists()
        {
            std::fs::remove_dir_all(d)?;
        }
        Ok(a)
    }

    /// Changes an account's label and budgets (its name and folder stay:
    /// the CLI may tie its login to the folder's path).
    pub fn set_meta(
        &mut self,
        tool: Tool,
        name: &str,
        meta: Meta,
    ) -> Result<Account, AccountError> {
        self.get(tool, name)?;
        self.index
            .accounts
            .entry(tool.name().to_owned())
            .or_default()
            .insert(name.to_owned(), meta);
        self.save()?;
        Ok(self.account(tool, name))
    }
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Shares `tool`'s settings from `from` (the default folder) into `to`.
fn share(tool: Tool, from: &Path, to: &Path) -> io::Result<()> {
    let (linked, copied): (&[&str], &[&str]) = match tool {
        Tool::Claude => (CLAUDE_SHARED, &[]),
        Tool::Codex => (CODEX_SHARED, CODEX_COPIED),
    };
    for name in linked {
        let src = from.join(name);
        if src.exists() && !to.join(name).exists() {
            link_or_copy(&src, &to.join(name))?;
        }
    }
    for name in copied {
        let src = from.join(name);
        if src.is_file() && !to.join(name).exists() {
            std::fs::copy(&src, to.join(name))?;
        }
    }
    Ok(())
}

/// A symbolic link where the system allows one (Unix; Windows with
/// Developer Mode), else a copy.
fn link_or_copy(src: &Path, dst: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(src, dst)
    }
    #[cfg(windows)]
    {
        let linked = if src.is_dir() {
            std::os::windows::fs::symlink_dir(src, dst)
        } else {
            std::os::windows::fs::symlink_file(src, dst)
        };
        if linked.is_ok() {
            return Ok(());
        }
        copy_tree(src, dst)
    }
    #[cfg(not(any(unix, windows)))]
    {
        copy_tree(src, dst)
    }
}

#[cfg_attr(unix, allow(dead_code))]
fn copy_tree(src: &Path, dst: &Path) -> io::Result<()> {
    if src.is_dir() {
        std::fs::create_dir_all(dst)?;
        for e in std::fs::read_dir(src)?.flatten() {
            copy_tree(&e.path(), &dst.join(e.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(src, dst).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn accounts_are_added_used_and_removed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        let claude_home = home.join(".claude");
        std::fs::create_dir_all(claude_home.join("skills/mine")).expect("mkdir");
        std::fs::write(claude_home.join("settings.json"), "{}").expect("write");
        std::fs::write(claude_home.join(".credentials.json"), "LOGIN").expect("write");
        let data = dir.path().join("data");

        let mut a = Accounts::load(&data).expect("load");
        assert_eq!(a.active(Tool::Claude).name, DEFAULT);
        assert_eq!(a.list(Tool::Claude).len(), 1);
        let work = a
            .add(Tool::Claude, "work", Some("Work (Max)"), Some(&claude_home))
            .expect("added");
        let wdir = work.dir.clone().expect("its own folder");
        assert!(wdir.starts_with(data.join("accounts/claude")));
        assert_eq!(
            work.env(),
            [("CLAUDE_CONFIG_DIR".to_owned(), wdir.display().to_string())]
        );
        assert_eq!(work.title(), "Work (Max)");
        // Settings and skills are shared; the login is not.
        assert!(wdir.join("settings.json").exists());
        assert!(wdir.join("skills/mine").is_dir());
        assert!(!wdir.join(".credentials.json").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&wdir).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        assert!(matches!(
            a.add(Tool::Claude, "work", None, None),
            Err(AccountError::Exists(..))
        ));
        assert!(a.add(Tool::Claude, "../x", None, None).is_err());
        assert!(a.add(Tool::Claude, DEFAULT, None, None).is_err());

        a.use_account(Tool::Claude, "work").expect("used");
        // It is kept: another process sees the same.
        let again = Accounts::load(&data).expect("load");
        assert_eq!(again.active(Tool::Claude).name, "work");
        assert_eq!(again.active(Tool::Codex).name, DEFAULT, "per CLI");
        assert!(matches!(
            again.get(Tool::Claude, "play"),
            Err(AccountError::Unknown { .. })
        ));

        let mut a = again;
        a.set_meta(
            Tool::Claude,
            "work",
            Meta {
                label: Some("Work".into()),
                budget_5h: Some(2_000_000),
                budget_week: None,
            },
        )
        .expect("meta");
        assert_eq!(
            a.get(Tool::Claude, "work").expect("get").meta.budget_5h,
            Some(2_000_000)
        );
        a.remove(Tool::Claude, "work").expect("removed");
        assert!(!wdir.exists());
        assert_eq!(a.active(Tool::Claude).name, DEFAULT, "back to default");
        assert!(a.remove(Tool::Claude, DEFAULT).is_err());
        // The default folder is untouched.
        assert!(claude_home.join(".credentials.json").exists());
    }

    #[test]
    fn codex_accounts_copy_the_config_and_link_the_rest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let codex_home = dir.path().join(".codex");
        std::fs::create_dir_all(codex_home.join("prompts")).expect("mkdir");
        std::fs::write(
            codex_home.join("config.toml"),
            "model = \"fixture-model\"\n",
        )
        .expect("write");
        std::fs::write(codex_home.join("auth.json"), "LOGIN").expect("write");
        let mut a = Accounts::load(&dir.path().join("data")).expect("load");
        let second = a
            .add(Tool::Codex, "second", None, Some(&codex_home))
            .expect("added");
        let d = second.dir.clone().expect("dir");
        assert_eq!(second.env()[0].0, "CODEX_HOME");
        let cfg = d.join("config.toml");
        assert!(cfg.is_file());
        assert!(
            !std::fs::symlink_metadata(&cfg)
                .expect("meta")
                .file_type()
                .is_symlink(),
            "copied, so TermOS can register its server per account"
        );
        assert!(d.join("prompts").exists());
        assert!(
            !d.join("auth.json").exists(),
            "the login is the account's own"
        );
    }

    #[test]
    fn the_default_folder_follows_the_environment() {
        let home = Path::new("/h");
        assert_eq!(
            default_home(Tool::Claude, &no_env, Some(home)),
            Some(PathBuf::from("/h/.claude"))
        );
        let env = |k: &str| (k == "CODEX_HOME").then(|| "/c".to_owned());
        assert_eq!(
            default_home(Tool::Codex, &env, Some(home)),
            Some(PathBuf::from("/c"))
        );
        let a = Accounts::load(Path::new("/nonexistent/data")).expect("empty");
        let d = a.active(Tool::Claude);
        assert!(d.is_default() && d.env().is_empty());
        assert_eq!(
            d.claude_settings(&no_env, Some(home)),
            Some(PathBuf::from("/h/.claude.json"))
        );
    }

    #[test]
    fn a_broken_index_is_an_error_not_a_reset() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("accounts")).expect("mkdir");
        std::fs::write(dir.path().join("accounts/accounts.toml"), "active = [").expect("write");
        assert!(matches!(
            Accounts::load(dir.path()),
            Err(AccountError::Index(..))
        ));
    }
}
