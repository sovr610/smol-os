// SPDX-License-Identifier: GPL-3.0-or-later
//! Keeps secrets out of what TermOS writes down (D-017).
//!
//! Audit records, conversation files and the agent window show tool
//! arguments and results. Before any of them is stored, the [`Redactor`]
//! replaces the values of the keys this session resolved, and text shaped
//! like a credential (API keys, bearer tokens, private key blocks,
//! `password=…` assignments), with `[redacted]`.

use std::sync::LazyLock;

use regex::Regex;
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;

pub const MARK: &str = "[redacted]";

static PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        // Provider keys and tokens with a recognisable prefix.
        r"\bsk-(?:ant-)?[A-Za-z0-9_\-]{16,}",
        r"\b(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{20,}",
        r"\bgithub_pat_[A-Za-z0-9_]{20,}",
        r"\bxox[abprs]-[A-Za-z0-9\-]{10,}",
        r"\bAKIA[0-9A-Z]{16}\b",
        r"\bAIza[0-9A-Za-z_\-]{30,}",
        r"\bhf_[A-Za-z0-9]{20,}",
        r"(?i)\bbearer\s+[A-Za-z0-9._~+/=\-]{16,}",
        r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?(?:-----END [A-Z ]*PRIVATE KEY-----|$)",
    ]
    .iter()
    .map(|p| Regex::new(p).expect("a valid built-in pattern"))
    .collect()
});

/// `name = value` where the name says it is secret: the value goes.
static ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)\b([A-Za-z0-9_\-]*(?:api[_\-]?key|secret|token|password|passwd)[A-Za-z0-9_\-]*)(["']?\s*[:=]\s*["']?)([^\s"',;]{6,})"#,
    )
    .expect("a valid built-in pattern")
});

/// Redacts known secret values and credential-shaped text.
#[derive(Default, Clone)]
pub struct Redactor {
    secrets: Vec<SecretString>,
}

impl std::fmt::Debug for Redactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Redactor({} secrets)", self.secrets.len())
    }
}

impl Redactor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Also redact this exact value (a key the session uses). Values
    /// shorter than 6 characters are not secrets worth hiding (and would
    /// redact half of every text).
    pub fn add(&mut self, secret: &SecretString) {
        if secret.expose_secret().len() >= 6
            && !self
                .secrets
                .iter()
                .any(|s| s.expose_secret() == secret.expose_secret())
        {
            self.secrets
                .push(SecretString::from(secret.expose_secret().to_owned()));
        }
    }

    /// Only the session's own keys replaced (for what the user typed:
    /// their text is theirs, but the keys TermOS holds stay out of it).
    pub fn text_known(&self, s: &str) -> String {
        let mut out = s.to_owned();
        for secret in &self.secrets {
            let v = secret.expose_secret();
            if out.contains(v) {
                out = out.replace(v, MARK);
            }
        }
        out
    }

    pub fn text(&self, s: &str) -> String {
        let mut out = self.text_known(s);
        for re in PATTERNS.iter() {
            if re.is_match(&out) {
                out = re.replace_all(&out, MARK).into_owned();
            }
        }
        if ASSIGNMENT.is_match(&out) {
            out = ASSIGNMENT
                .replace_all(&out, |c: &regex::Captures<'_>| {
                    if c[3].starts_with('$') || c[3] == *MARK {
                        // `$TOKEN` names a variable; nothing to hide.
                        c[0].to_owned()
                    } else {
                        format!("{}{}{MARK}", &c[1], &c[2])
                    }
                })
                .into_owned();
        }
        out
    }

    /// Every string inside `v`, redacted.
    pub fn value(&self, v: &Value) -> Value {
        match v {
            Value::String(s) => Value::String(self.text(s)),
            Value::Array(a) => Value::Array(a.iter().map(|x| self.value(x)).collect()),
            Value::Object(o) => {
                Value::Object(o.iter().map(|(k, x)| (k.clone(), self.value(x))).collect())
            }
            other => other.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_values_and_shapes() {
        let mut r = Redactor::new();
        r.add(&SecretString::from("plain-looking-canary".to_owned()));
        assert_eq!(r.text("key=plain-looking-canary!"), "key=[redacted]!");
        assert_eq!(
            r.text("curl -H 'Authorization: Bearer abcdefghijklmnopqrstuvwxyz0123'"),
            "curl -H 'Authorization: [redacted]'"
        );
        assert_eq!(
            r.text("export OPENAI_API_KEY=sk-proj-abcdefghijklmnop1234"),
            "export OPENAI_API_KEY=[redacted]"
        );
        assert_eq!(r.text("password: hunter22x"), "password: [redacted]");
        assert_eq!(r.text("token=$GITHUB_TOKEN"), "token=$GITHUB_TOKEN");
        assert_eq!(
            r.text("ordinary text, nothing secret"),
            "ordinary text, nothing secret"
        );
        let pem =
            "-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----\nrest";
        assert_eq!(r.text(pem), "[redacted]\nrest");
        let v = r.value(&serde_json::json!({"command": "echo plain-looking-canary", "n": 3}));
        assert_eq!(v, serde_json::json!({"command": "echo [redacted]", "n": 3}));
        assert!(!format!("{r:?}").contains("canary"));
    }
}
