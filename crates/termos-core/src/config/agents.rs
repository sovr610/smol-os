// SPDX-License-Identifier: GPL-3.0-or-later
//! `[agents]` and `[secrets]`: providers, agent profiles, MCP servers and
//! external CLI agents (M6, ARCHITECTURE.md §11, D-059).
//!
//! Only the shape lives here; `termos-ai` gives it meaning. Secrets are
//! never written in the config: a provider names one as
//! `keyring:<name>` (D-017), stored with `termos secret set <name>`.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Deserializer};

/// `[agents]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct AgentsConfig {
    /// The profile `agent.open` uses when it names none.
    pub default_profile: String,
    /// Keep conversations on disk, so they can be resumed.
    pub history: bool,
    /// LLM providers by name (`[agents.providers.<name>]`).
    pub providers: BTreeMap<String, ProviderConfig>,
    /// Agent profiles by name: provider, model and permissions.
    pub profiles: BTreeMap<String, ProfileConfig>,
    /// MCP servers by name; a profile lists the ones it may use.
    pub mcp: BTreeMap<String, McpServerConfig>,
    /// External CLI agents (Claude Code, Codex CLI, …) by name, added to
    /// or replacing the built-in presets.
    pub external: BTreeMap<String, ExternalAgentConfig>,
}

impl Default for AgentsConfig {
    fn default() -> Self {
        Self {
            default_profile: "default".into(),
            history: true,
            providers: BTreeMap::new(),
            profiles: BTreeMap::new(),
            mcp: BTreeMap::new(),
            external: BTreeMap::new(),
        }
    }
}

/// Which API a provider speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    /// Anthropic's Messages API.
    Anthropic,
    /// OpenAI's Chat Completions API.
    Openai,
    /// Moonshot's Kimi (OpenAI-compatible, its own address).
    Kimi,
    /// Any OpenAI-compatible server: Ollama, vLLM, LM Studio, OpenRouter…
    OpenaiCompatible,
    /// TermOS's own model router (`termos models serve`, M7).
    Local,
}

impl ProviderKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::Openai => "openai",
            Self::Kimi => "kimi",
            Self::OpenaiCompatible => "openai-compatible",
            Self::Local => "local",
        }
    }
}

impl fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// `[agents.providers.<name>]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct ProviderConfig {
    pub kind: ProviderKind,
    /// The API's address; each kind but `openai-compatible` has a default.
    pub base_url: Option<String>,
    /// The API key, as `keyring:<name>`.
    pub key: Option<SecretRef>,
    /// Most tokens one reply may have (default: what the provider lists
    /// for the model, else 32000).
    pub max_tokens: Option<u32>,
    /// Prices in US dollars per million tokens, by model id. Costs are
    /// shown only for models priced here or by the provider's own list.
    #[serde(default)]
    pub prices: BTreeMap<String, Price>,
}

/// Prices in US dollars per million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Price {
    pub input: f64,
    pub output: f64,
    /// Input read from the provider's prompt cache (default: `input`).
    pub cache_read: Option<f64>,
    /// Input written to the prompt cache (default: `input`).
    pub cache_write: Option<f64>,
}

/// A secret named in the config: `keyring:<name>` (D-017).
#[derive(Clone, PartialEq, Eq)]
pub struct SecretRef(String);

impl SecretRef {
    /// Parses `keyring:<name>`.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.strip_prefix("keyring:") {
            Some(name) if valid_secret_name(name) => Ok(Self(name.to_owned())),
            Some(_) => Err(format!(
                "invalid secret name in `{s}`: use letters, digits, `.`, `-` and `_`"
            )),
            None => Err(format!(
                "secrets are named, never written here: use `keyring:<name>` and store the value \
                 with `termos secret set <name>` (got `{}`)",
                shown_ref(s)
            )),
        }
    }

    /// The secret's name.
    pub fn name(&self) -> &str {
        &self.0
    }
}

/// What a wrong `key` value shows in an error: a value that is not a
/// reference may be the key itself, so only its start is repeated.
fn shown_ref(s: &str) -> String {
    let start: String = s.chars().take(8).collect();
    if s.chars().count() > 8 {
        format!("{start}…")
    } else {
        start
    }
}

/// Secret names: what `termos secret set` accepts.
pub fn valid_secret_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

impl fmt::Debug for SecretRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "keyring:{}", self.0)
    }
}

impl fmt::Display for SecretRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "keyring:{}", self.0)
    }
}

impl<'de> Deserialize<'de> for SecretRef {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// What a profile does with one class of tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Rule {
    /// Ask in the agent window each time (or once per session).
    #[default]
    Ask,
    /// Run it without asking (destructive and network actions still ask).
    Allow,
    /// Refuse it; the model is told.
    Deny,
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Ask => "ask",
            Self::Allow => "allow",
            Self::Deny => "deny",
        })
    }
}

/// `[agents.profiles.<name>.tools]`: a rule per class of tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct ToolRules {
    /// `read_file`, `list_dir`, `search_code` inside the readable paths.
    pub read: Rule,
    /// `write_file`, `edit_file` inside the writable paths.
    pub write: Rule,
    /// `run_command`.
    pub command: Rule,
    /// `fetch_url`, and commands that ask for the network.
    pub network: Rule,
    /// Tools of MCP servers.
    pub mcp: Rule,
}

impl Default for ToolRules {
    fn default() -> Self {
        Self {
            read: Rule::Allow,
            write: Rule::Ask,
            command: Rule::Ask,
            network: Rule::Ask,
            mcp: Rule::Ask,
        }
    }
}

/// Where commands run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxMode {
    /// bubblewrap and Landlock where they work; with less, say so and ask
    /// before every command.
    #[default]
    Auto,
    /// Refuse to run commands without bubblewrap and Landlock.
    Required,
    /// Run commands without a sandbox (they still follow the rules).
    Off,
}

impl fmt::Display for SandboxMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Auto => "auto",
            Self::Required => "required",
            Self::Off => "off",
        })
    }
}

/// `[agents.profiles.<name>]`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct ProfileConfig {
    /// The provider (default: the only one configured, else ask).
    pub provider: Option<String>,
    /// The model id (default: pick from the provider's list).
    pub model: Option<String>,
    /// Extra instructions for the model, after TermOS's own.
    pub instructions: Option<String>,
    pub tools: ToolRules,
    /// Readable outside the workspace (the folder the agent works in).
    pub read_paths: Vec<PathBuf>,
    /// Writable besides the workspace.
    pub write_paths: Vec<PathBuf>,
    /// Commands that run without asking (patterns, `*` matches anything),
    /// unless they are destructive or need the network.
    pub allow_commands: Vec<String>,
    /// Commands that are always refused (patterns).
    pub deny_commands: Vec<String>,
    /// Domains `fetch_url` may reach (`*.example.com` covers subdomains);
    /// empty: any, after asking.
    pub domains: Vec<String>,
    /// MCP servers (names under `[agents.mcp]`) this profile may use.
    pub mcp_servers: Vec<String>,
    pub sandbox: SandboxMode,
}

/// `[agents.mcp.<name>]`: a server started with `command` (stdio) or
/// reached at `url` (streamable HTTP).
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct McpServerConfig {
    pub command: Vec<String>,
    pub url: Option<String>,
    /// A bearer token for `url`, as `keyring:<name>`.
    pub key: Option<SecretRef>,
    /// Extra environment for `command`.
    pub env: BTreeMap<String, String>,
    /// A local server's sandbox may reach the network.
    pub network: bool,
    /// Folders a local server may write besides the workspace.
    pub write_paths: Vec<PathBuf>,
}

/// `[agents.external.<name>]`: a CLI agent opened in a terminal window.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct ExternalAgentConfig {
    /// The program and its arguments.
    pub command: Vec<String>,
    /// Shown in the palette.
    pub description: Option<String>,
    /// Run it in the sandbox: the workspace and `write-paths` writable.
    pub sandbox: bool,
    /// Its sandbox may reach the network (it needs it to reach its API).
    pub network: Option<bool>,
    /// Writable besides the workspace (its own settings, e.g. `~/.claude`).
    pub write_paths: Vec<PathBuf>,
}

/// Where API keys are kept (`[secrets]`, D-058).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SecretStoreKind {
    /// The secrets file if there is one, else the OS keyring when it
    /// works, else the secrets file.
    #[default]
    Auto,
    /// The OS keyring (Secret Service, Keychain, Credential Manager).
    Keyring,
    /// An age file encrypted to a passphrase.
    File,
}

/// `[secrets]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct SecretsConfig {
    pub store: SecretStoreKind,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_refs_are_names_only() {
        assert_eq!(
            SecretRef::parse("keyring:anthropic").map(|r| r.name().to_owned()),
            Ok("anthropic".into())
        );
        let err = SecretRef::parse("sk-ant-abcdefghijklmnop").expect_err("a value");
        assert!(err.contains("keyring:<name>"), "{err}");
        // A key pasted by mistake is not repeated in full.
        assert!(!err.contains("abcdefghijklmnop"), "{err}");
        assert!(SecretRef::parse("keyring:").is_err());
        assert!(SecretRef::parse("keyring:a/b").is_err());
    }
}
