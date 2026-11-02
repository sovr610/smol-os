// SPDX-License-Identifier: GPL-3.0-or-later
//! Conversations kept on disk, to resume them (D-011: agents restore
//! their history, not in-flight tool calls).
//!
//! One JSON file per session in `$XDG_STATE_HOME/termos/agents/`, mode
//! 0600 in a 0700 folder, rewritten atomically after every message. Tool
//! results are stored redacted, as the model saw them. `[agents] history
//! = false` keeps nothing.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::provider::{Message, Role, Usage};

/// The folder of saved conversations.
pub fn dir(state_dir: &Path) -> PathBuf {
    state_dir.join("agents")
}

/// A saved conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    pub version: u32,
    pub id: String,
    pub profile: String,
    pub provider: String,
    pub model: String,
    pub workspace: PathBuf,
    /// RFC 3339.
    pub created: String,
    pub updated: String,
    pub messages: Vec<Message>,
    pub usage: Usage,
    /// The audit log's last hash, so a log cut short shows (D-062).
    pub audit_head: Option<String>,
}

impl Saved {
    /// The first thing the user asked, shortened, for lists.
    pub fn title(&self) -> String {
        let first = self
            .messages
            .iter()
            .find(|m| m.role == Role::User && !m.text().trim().is_empty())
            .map(Message::text)
            .unwrap_or_default();
        let one_line = first.split_whitespace().collect::<Vec<_>>().join(" ");
        if one_line.chars().count() > 60 {
            format!("{}…", one_line.chars().take(59).collect::<String>())
        } else {
            one_line
        }
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        crate::secrets::write_private(&dir.join(format!("{}.json", self.id)), &json)
    }

    pub fn load(dir: &Path, id: &str) -> std::io::Result<Self> {
        if !valid_id(id) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "not a session id",
            ));
        }
        let bytes = std::fs::read(dir.join(format!("{id}.json")))?;
        let saved: Self = serde_json::from_slice(&bytes).map_err(std::io::Error::other)?;
        if saved.version != 1 {
            return Err(std::io::Error::other(format!(
                "conversation format {} is newer than this TermOS",
                saved.version
            )));
        }
        Ok(saved)
    }
}

/// Session ids: `YYYYMMDD-HHMMSS-xxxxxx`.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 40 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// A fresh session id: the time and six random hex digits.
pub fn new_id() -> String {
    let now = jiff::Zoned::now();
    let mut rnd = [0u8; 3];
    let _ = getrandom::fill(&mut rnd);
    format!(
        "{}-{:02x}{:02x}{:02x}",
        now.strftime("%Y%m%d-%H%M%S"),
        rnd[0],
        rnd[1],
        rnd[2]
    )
}

/// Saved conversations, newest first.
pub fn list(dir: &Path) -> Vec<Saved> {
    let mut all: Vec<Saved> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let id = name.strip_suffix(".json")?;
            Saved::load(dir, id).ok()
        })
        .collect();
    all.sort_by(|a, b| b.updated.cmp(&a.updated));
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_load_list() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = Saved {
            version: 1,
            id: new_id(),
            profile: "default".into(),
            provider: "p".into(),
            model: "fixture-model".into(),
            workspace: dir.path().to_owned(),
            created: "2026-11-02T10:00:00Z".into(),
            updated: "2026-11-02T10:00:00Z".into(),
            messages: vec![Message::user_text(
                "  fix   the failing test in src/lib.rs, please, it has been broken since yesterday afternoon",
            )],
            usage: Usage::default(),
            audit_head: None,
        };
        assert!(valid_id(&s.id), "{}", s.id);
        s.save(dir.path()).expect("save");
        assert_eq!(Saved::load(dir.path(), &s.id).expect("load"), s);
        assert_eq!(
            s.title(),
            "fix the failing test in src/lib.rs, please, it has been bro…"
        );
        let mut older = s.clone();
        older.id = "20200101-000000-000000".into();
        older.updated = "2020-01-01T00:00:00Z".into();
        older.save(dir.path()).expect("save");
        let ids: Vec<String> = list(dir.path()).into_iter().map(|x| x.id).collect();
        assert_eq!(ids, [s.id.clone(), older.id]);
        assert!(Saved::load(dir.path(), "../etc/passwd").is_err());
    }
}
