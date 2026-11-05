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
    /// The account signs in with an API key kept in TermOS's secret store
    /// under this name, given to the CLI when it starts (Claude Code; D-112).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct Index {
    /// The account in use, by CLI.
    active: BTreeMap<String, String>,
    /// Accounts by CLI, then name (`default` may be listed for its label).
    accounts: BTreeMap<String, BTreeMap<String, Meta>>,
}

/// Variables that would override an account's own sign-in when inherited
/// from the environment; the shims drop them for TermOS's accounts and for
/// an account that uses an API key (D-112). Only the ones each CLI signs in
/// with: a project's own `OPENAI_API_KEY`, say, is left alone.
pub fn credential_variables(tool: Tool) -> &'static [&'static str] {
    match tool {
        Tool::Claude => &[
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CLAUDE_CODE_OAUTH_REFRESH_TOKEN",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
        ],
        Tool::Codex => &["CODEX_API_KEY", "CODEX_ACCESS_TOKEN"],
        Tool::Gemini => &[
            "GEMINI_API_KEY",
            "GOOGLE_API_KEY",
            "GOOGLE_GENAI_USE_VERTEXAI",
            "GOOGLE_GENAI_USE_GCA",
            "GOOGLE_GEMINI_BASE_URL",
        ],
    }
}

/// The variable an account's API key is given to the CLI in; `None` for
/// Codex, which keeps the key itself (`codex login --with-api-key`).
pub fn key_variable(tool: Tool) -> Option<&'static str> {
    match tool {
        Tool::Claude => Some("ANTHROPIC_API_KEY"),
        Tool::Codex => None,
        Tool::Gemini => Some("GEMINI_API_KEY"),
    }
}

/// The secret an account's API key is kept under: `account.claude.work`.
pub fn key_secret(tool: Tool, name: &str) -> String {
    format!("account.{}.{name}", tool.name())
}

/// Whether a sign-in here can open a browser: not over SSH, and on Linux
/// not without a graphical session (the bare-metal console, a server).
/// Where it cannot, the CLIs' code and device sign-ins are used.
pub fn browser_here(get: &dyn Fn(&str) -> Option<String>) -> bool {
    let set = |k: &str| get(k).is_some_and(|v| !v.is_empty());
    if set("SSH_CONNECTION") || set("SSH_TTY") {
        return false;
    }
    if cfg!(target_os = "linux") {
        return set("DISPLAY") || set("WAYLAND_DISPLAY");
    }
    true
}

/// What a CLI says about an account's sign-in, from its own status command
/// (`claude auth status`, `codex login status`), never from its login files
/// (D-112).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AuthStatus {
    pub signed_in: bool,
    /// How: "Claude Max", "Anthropic Console", "API key", "ChatGPT".
    pub method: String,
    /// The email or organisation it is signed in as.
    pub who: Option<String>,
}

impl AuthStatus {
    /// [`AuthStatus::line`], or nothing when not signed in (where a
    /// missing file cannot tell).
    pub fn line_or_empty(&self) -> String {
        if self.signed_in {
            self.line()
        } else {
            String::new()
        }
    }

    /// "signed in: Claude Max · you@example.com", "not signed in".
    pub fn line(&self) -> String {
        if !self.signed_in {
            return "not signed in".to_owned();
        }
        let mut parts = vec![format!("signed in: {}", self.method)];
        parts.extend(self.who.clone());
        parts.join(" · ")
    }
}

/// `claude auth status`'s JSON (and whether it exited with 0).
pub fn claude_status(json: &str, ok: bool) -> AuthStatus {
    let v: serde_json::Value = serde_json::from_str(json.trim()).unwrap_or_default();
    let text = |k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let signed_in = v
        .get("loggedIn")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(ok);
    let method = match text("authMethod").as_deref() {
        Some("claude.ai") => match text("subscriptionType") {
            Some(plan) => {
                let mut c = plan.chars();
                let plan: String = c
                    .next()
                    .map(|f| f.to_uppercase().chain(c).collect())
                    .unwrap_or_default();
                format!("Claude {plan}")
            }
            None => "Claude subscription".to_owned(),
        },
        Some("api_key" | "api_key_helper") => "API key".to_owned(),
        Some("oauth_token") => "OAuth token".to_owned(),
        Some("third_party") => text("apiProvider").unwrap_or_else(|| "a cloud provider".into()),
        Some(other) if other != "none" => other.to_owned(),
        _ => String::new(),
    };
    AuthStatus {
        signed_in,
        method,
        who: text("email").or_else(|| text("orgName")),
    }
}

/// `codex login status`'s report (it writes to standard error) and whether
/// it exited with 0. A configured key is only named, never echoed.
pub fn codex_status(text: &str, ok: bool) -> AuthStatus {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if !ok || line.starts_with("Not logged in") || line.starts_with("Error") {
        return AuthStatus::default();
    }
    let method = if line.starts_with("API key") {
        "API key".to_owned()
    } else if let Some(how) = line.strip_prefix("Logged in using ") {
        how.trim_start_matches("an ")
            .trim_start_matches("a ")
            .to_owned()
    } else {
        "signed in".to_owned()
    };
    AuthStatus {
        signed_in: true,
        method,
        who: None,
    }
}

/// Gemini CLI's folder in its home.
pub const GEMINI_DIR: &str = ".gemini";

/// Gemini CLI's sign-in, from its folder (`.gemini`): it has no status
/// command. `google_accounts.json` names the Google account and holds no
/// token; the login files are only checked for, never read.
pub fn gemini_status(dir: Option<&Path>) -> AuthStatus {
    let Some(dir) = dir else {
        return AuthStatus::default();
    };
    let who = std::fs::read_to_string(dir.join("google_accounts.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.get("active").and_then(|a| a.as_str()).map(str::to_owned))
        .filter(|e| !e.is_empty());
    let login = ["oauth_creds.json", "gemini-credentials.json"]
        .iter()
        .any(|f| dir.join(f).exists());
    AuthStatus {
        signed_in: login || who.is_some(),
        method: "Google account".into(),
        who,
    }
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
            // A home folder: Gemini CLI keeps its files in `.gemini` in it.
            Tool::Gemini => "GEMINI_CLI_HOME",
        }
    }

    /// The environment the CLI runs with for this account (empty for
    /// `default`, which the CLI finds itself).
    pub fn env(&self) -> Vec<(String, String)> {
        let Some(d) = &self.dir else {
            return Vec::new();
        };
        let mut env = vec![(
            Self::variable(self.tool).to_owned(),
            d.display().to_string(),
        )];
        if self.tool == Tool::Gemini {
            // Its keychain entries are the same for every home: logins in
            // the account's folder instead (D-112).
            env.push(("GEMINI_FORCE_FILE_STORAGE".to_owned(), "true".to_owned()));
        }
        env
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
            return Some(match self.tool {
                Tool::Gemini => d.join(GEMINI_DIR),
                _ => d.clone(),
            });
        }
        default_home(self.tool, get, home)
    }

    /// Who the account is signed in as, as far as can be told without
    /// reading its login: Claude Code keeps the account's email beside its
    /// settings; a login file's presence says the rest. Empty when it
    /// cannot be told (Claude Code on macOS keeps its login in the
    /// Keychain).
    pub fn signed_in(&self, get: &dyn Fn(&str) -> Option<String>, home: Option<&Path>) -> String {
        match self.tool {
            Tool::Claude => {
                let email = self
                    .claude_settings(get, home)
                    .and_then(|f| std::fs::read_to_string(f).ok())
                    .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                    .and_then(|v| {
                        v.pointer("/oauthAccount/emailAddress")
                            .and_then(|e| e.as_str())
                            .map(str::to_owned)
                    });
                match email {
                    Some(e) => format!("signed in as {e}"),
                    None if self
                        .home(get, home)
                        .is_some_and(|h| h.join(".credentials.json").exists()) =>
                    {
                        "signed in".into()
                    }
                    None if cfg!(target_os = "macos") => String::new(),
                    None => "not signed in".into(),
                }
            }
            Tool::Codex => {
                let h = self.home(get, home);
                // A file login, or (Windows) its encrypted one.
                if h.as_ref().is_some_and(|h| {
                    h.join("auth.json").exists()
                        || h.join("secrets").join("codex_auth.age").exists()
                }) {
                    "signed in".into()
                } else {
                    String::new()
                }
            }
            Tool::Gemini => gemini_status(self.home(get, home).as_deref()).line_or_empty(),
        }
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
        .map(|d| match tool {
            Tool::Gemini => d.join(GEMINI_DIR),
            _ => d,
        })
        .or_else(|| {
            home.map(|h| match tool {
                Tool::Claude => h.join(".claude"),
                Tool::Codex => h.join(".codex"),
                Tool::Gemini => h.join(GEMINI_DIR),
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
/// Gemini CLI's: its instructions, commands, skills and extensions are
/// shared; its settings are the account's own (TermOS's server, the
/// sign-in method).
const GEMINI_SHARED: &[&str] = &["GEMINI.md", "commands", "skills", "extensions"];
const GEMINI_COPIED: &[&str] = &["settings.json"];

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
        // Gemini CLI's files go in `.gemini` in the account's folder.
        let into = match tool {
            Tool::Gemini => {
                let g = dir.join(GEMINI_DIR);
                create_private_dir(&g)?;
                g
            }
            _ => dir.clone(),
        };
        if let Some(from) = default_home.filter(|d| d.is_dir()) {
            share(tool, from, &into)?;
            // Agent Skills (D-098) are read from `~/.agents`, which for
            // Gemini CLI is in its home: the user's, linked.
            if tool == Tool::Gemini
                && let Some(user_home) = from.parent()
            {
                let agents = user_home.join(".agents");
                std::fs::create_dir_all(&agents)?;
                if !dir.join(".agents").exists() {
                    link_or_copy(&agents, &dir.join(".agents"))?;
                }
            }
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
        Tool::Gemini => (GEMINI_SHARED, GEMINI_COPIED),
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

/// A plan's limits as the CLI reported them (D-097): Codex writes them
/// into its logs; Claude Code gives them to its status line, and TermOS's
/// status line command keeps them in `<state>/accounts/limits/`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Limits {
    /// When they were reported (Unix seconds).
    pub time: i64,
    /// The plan, when the CLI names it ("plus", "pro").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    pub windows: Vec<LimitWindow>,
}

/// One limit window: how much of it is used, and when it resets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LimitWindow {
    /// Its length (300: 5 hours; 10080: a week).
    pub minutes: u64,
    /// 0 to 100.
    pub used_percent: f32,
    /// Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<i64>,
}

/// Where an account's reported limits are kept.
pub fn limits_file(state_dir: &Path, tool: Tool, account: &str) -> PathBuf {
    state_dir
        .join("accounts")
        .join("limits")
        .join(format!("{}-{account}.json", tool.name()))
}

/// Keeps limits an account's CLI reported.
pub fn save_limits(state_dir: &Path, tool: Tool, account: &str, l: &Limits) -> io::Result<()> {
    let file = limits_file(state_dir, tool, account);
    if let Some(dir) = file.parent() {
        create_private_dir(dir)?;
    }
    let tmp = file.with_extension(format!("json.{}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec(l).map_err(io::Error::other)?)?;
    std::fs::rename(&tmp, &file)
}

/// The limits an account's CLI last reported.
pub fn load_limits(state_dir: &Path, tool: Tool, account: &str) -> Option<Limits> {
    let text = std::fs::read_to_string(limits_file(state_dir, tool, account)).ok()?;
    serde_json::from_str(&text).ok()
}

/// The limits in Claude Code's status line input (`rate_limits.five_hour`
/// and `seven_day`, each `used_percentage` and `resets_at`); only
/// subscribers get them.
pub fn claude_status_limits(input: &serde_json::Value, now: i64) -> Option<Limits> {
    let r = input.get("rate_limits")?;
    let windows: Vec<LimitWindow> = [("five_hour", 300), ("seven_day", 10_080)]
        .iter()
        .filter_map(|(key, minutes)| {
            let w = r.get(*key)?;
            Some(LimitWindow {
                minutes: *minutes,
                used_percent: w.get("used_percentage")?.as_f64()? as f32,
                resets_at: w.get("resets_at").and_then(serde_json::Value::as_i64),
            })
        })
        .collect();
    (!windows.is_empty()).then_some(Limits {
        time: now,
        plan: None,
        windows,
    })
}

/// The command TermOS puts in Claude Code's `statusLine`.
pub const STATUSLINE_ARGS: &str = "agent statusline";

/// Claude Code's `settings.json` with TermOS's status line: `termos`
/// (quoted as needed) `agent statusline`. The status line it had is
/// returned, for TermOS's command to run after its own (`None`: none, or
/// already TermOS's).
pub fn with_statusline(
    settings: &mut serde_json::Value,
    termos: &str,
) -> Result<Option<String>, AccountError> {
    let obj = settings.as_object_mut().ok_or_else(|| {
        AccountError::Invalid("Claude Code's settings are not a JSON object".into())
    })?;
    let ours = format!("{termos} {STATUSLINE_ARGS}");
    let old = obj
        .get("statusLine")
        .and_then(|s| s.get("command"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .filter(|c| !c.contains(STATUSLINE_ARGS));
    let mut line = serde_json::Map::new();
    line.insert("type".into(), "command".into());
    line.insert("command".into(), ours.into());
    // Keep its other settings (padding).
    if let Some(serde_json::Value::Object(prev)) = obj.get("statusLine") {
        for (k, v) in prev {
            if k != "type" && k != "command" {
                line.insert(k.clone(), v.clone());
            }
        }
    }
    obj.insert("statusLine".into(), serde_json::Value::Object(line));
    Ok(old)
}

/// Claude Code's `settings.json` without TermOS's status line: the one it
/// had before (`previous`) is put back, else none.
pub fn without_statusline(settings: &mut serde_json::Value, previous: Option<&str>) -> bool {
    let Some(obj) = settings.as_object_mut() else {
        return false;
    };
    let ours = obj
        .get("statusLine")
        .and_then(|s| s.get("command"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|c| c.contains(STATUSLINE_ARGS));
    if !ours {
        return false;
    }
    match previous {
        Some(cmd) => {
            obj.insert(
                "statusLine".into(),
                serde_json::json!({ "type": "command", "command": cmd }),
            );
        }
        None => {
            obj.remove("statusLine");
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_in_status_from_the_clis_own_commands() {
        let max = r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","email":"me@example.com","orgName":"Me","subscriptionType":"max","configDirectory":"/a"}"#;
        assert_eq!(
            claude_status(max, true).line(),
            "signed in: Claude Max · me@example.com"
        );
        let out = r#"{"loggedIn":false,"authMethod":"none"}"#;
        assert_eq!(claude_status(out, false).line(), "not signed in");
        assert_eq!(claude_status("not json", false).line(), "not signed in");
        let key = r#"{"loggedIn":true,"authMethod":"api_key"}"#;
        assert_eq!(claude_status(key, true).line(), "signed in: API key");
        assert_eq!(
            codex_status("Logged in using ChatGPT\n", true).line(),
            "signed in: ChatGPT"
        );
        let masked = "API key configured - sk-proj-***abcde\nAuthentication is checked when you send a request.\n";
        let s = codex_status(masked, true);
        assert_eq!(s.line(), "signed in: API key");
        assert!(!s.line().contains("sk-"));
        assert_eq!(
            codex_status("Not logged in\n", false).line(),
            "not signed in"
        );
        assert_eq!(
            codex_status("Logged in using personal access token", true).method,
            "personal access token"
        );
        let get = |k: &str| match k {
            "SSH_CONNECTION" => Some("1.2.3.4 5 6.7.8.9 22".to_owned()),
            _ => Some(":0".to_owned()),
        };
        assert!(!browser_here(&get));
        let key = key_secret(Tool::Claude, "work");
        assert_eq!(key, "account.claude.work");
        assert!(termos_core::config::agents::valid_secret_name(&key));
        assert!(credential_variables(Tool::Claude).contains(&"ANTHROPIC_API_KEY"));
        assert!(!credential_variables(Tool::Codex).contains(&"OPENAI_API_KEY"));
    }

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
                api_key: None,
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
    fn limits_are_kept_and_the_status_line_chains_the_users_own() {
        let dir = tempfile::tempdir().expect("tempdir");
        let input = serde_json::json!({
            "model": { "display_name": "Fixture" },
            "rate_limits": {
                "five_hour": { "used_percentage": 42.5, "resets_at": 1_800_000_000 },
                "seven_day": { "used_percentage": 10 }
            }
        });
        let l = claude_status_limits(&input, 7).expect("limits");
        assert_eq!(
            l.windows[0],
            LimitWindow {
                minutes: 300,
                used_percent: 42.5,
                resets_at: Some(1_800_000_000)
            }
        );
        assert_eq!(l.windows[1].resets_at, None);
        assert!(
            claude_status_limits(&serde_json::json!({ "model": {} }), 7).is_none(),
            "API users have none"
        );
        save_limits(dir.path(), Tool::Claude, "work", &l).expect("saved");
        assert_eq!(load_limits(dir.path(), Tool::Claude, "work"), Some(l));
        assert_eq!(load_limits(dir.path(), Tool::Claude, "play"), None);

        // Their own status line is kept, to run after TermOS's.
        let mut s = serde_json::json!({
            "model": "fixture-model",
            "statusLine": { "type": "command", "command": "~/bin/mine.sh", "padding": 0 }
        });
        let old = with_statusline(&mut s, "/opt/termos").expect("set");
        assert_eq!(old.as_deref(), Some("~/bin/mine.sh"));
        assert_eq!(s["statusLine"]["command"], "/opt/termos agent statusline");
        assert_eq!(s["statusLine"]["padding"], 0);
        assert_eq!(s["model"], "fixture-model");
        // Again: no second wrapping.
        assert_eq!(with_statusline(&mut s, "/opt/termos").expect("set"), None);
        assert!(without_statusline(&mut s, old.as_deref()));
        assert_eq!(s["statusLine"]["command"], "~/bin/mine.sh");
        assert!(!without_statusline(&mut s, None), "not TermOS's");
        let mut fresh = serde_json::json!({});
        with_statusline(&mut fresh, "termos").expect("set");
        assert!(without_statusline(&mut fresh, None));
        assert!(fresh.get("statusLine").is_none());
        assert!(with_statusline(&mut serde_json::json!([]), "t").is_err());
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
