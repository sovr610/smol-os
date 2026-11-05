// SPDX-License-Identifier: GPL-3.0-or-later
//! Secrets handed to a program as environment variables (an account's API
//! key for Claude Code or Gemini CLI, D-112), kept in `secrecy` types until
//! the program starts (D-017). `Debug` shows the names only.

use secrecy::{ExposeSecret as _, SecretString};

/// Variables and their secret values.
#[derive(Clone, Default)]
pub struct SecretEnv(Vec<(String, SecretString)>);

impl SecretEnv {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `name`, replacing an earlier value.
    pub fn set(&mut self, name: &str, value: SecretString) {
        self.0.retain(|(n, _)| n != name);
        self.0.push((name.to_owned(), value));
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The variables' names.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(|(n, _)| n.as_str())
    }

    /// The variables with their values, for starting the program.
    pub fn exposed(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(n, v)| (n.as_str(), v.expose_secret()))
    }

    /// Sets the variables on `cmd`.
    pub fn apply(&self, cmd: &mut std::process::Command) {
        cmd.envs(self.exposed());
    }
}

impl std::fmt::Debug for SecretEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.names().map(|n| format!("{n}=[REDACTED]")))
            .finish()
    }
}

/// Equal when the same variables are set (values are not compared).
impl PartialEq for SecretEnv {
    fn eq(&self, other: &Self) -> bool {
        self.names().eq(other.names())
    }
}

impl Eq for SecretEnv {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_never_show_and_replace() {
        let mut e = SecretEnv::new();
        e.set("ANTHROPIC_API_KEY", SecretString::from("sk-fixture-1"));
        e.set("ANTHROPIC_API_KEY", SecretString::from("sk-fixture-2"));
        let shown = format!("{e:?}");
        assert!(!shown.contains("sk-fixture"), "{shown}");
        assert!(shown.contains("ANTHROPIC_API_KEY"));
        assert_eq!(
            e.exposed().collect::<Vec<_>>(),
            [("ANTHROPIC_API_KEY", "sk-fixture-2")]
        );
        assert_eq!(e, e.clone());
        assert_ne!(e, SecretEnv::new());
    }
}
