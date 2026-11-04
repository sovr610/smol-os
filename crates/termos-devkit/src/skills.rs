// SPDX-License-Identifier: GPL-3.0-or-later
//! TermOS's skills for the coding CLIs (M11b, D-098).
//!
//! Three skills (`termos`, `termos-dev`, `termos-agents`) teach Claude
//! Code, Codex and the other CLIs that read Agent Skills what TermOS
//! offers. They are built into the binary and written into each Claude
//! Code account's `skills` folder and into `~/.agents/skills`, which
//! Codex, Gemini CLI and opencode read.
//!
//! TermOS owns only what it wrote: each copy holds a `.termos-skill` file
//! with the SHA-256 of every file in it. A copy whose files still match is
//! TermOS's to update or remove; one the user changed is left alone (unless
//! forced); a folder of the same name without that file is the user's own
//! skill and is never touched.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::accounts::Accounts;
use crate::cli_agents::Tool;

/// One skill: a folder of files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Skill {
    pub name: &'static str,
    /// File names (relative to the skill's folder) and contents.
    pub files: &'static [(&'static str, &'static str)],
}

impl Skill {
    /// The `description` in `SKILL.md`'s front matter.
    pub fn description(&self) -> &'static str {
        self.files
            .iter()
            .find(|(n, _)| *n == "SKILL.md")
            .and_then(|(_, text)| front_matter(text, "description"))
            .unwrap_or("")
    }

    /// The `SKILL.md` text.
    pub fn text(&self) -> &'static str {
        self.files
            .iter()
            .find(|(n, _)| *n == "SKILL.md")
            .map_or("", |(_, t)| t)
    }
}

/// A field of a `---` front matter block (one line, unquoted).
fn front_matter<'a>(text: &'a str, field: &str) -> Option<&'a str> {
    let body = text.strip_prefix("---\n")?;
    let end = body.find("\n---")?;
    body[..end].lines().find_map(|l| {
        l.strip_prefix(field)
            .and_then(|r| r.strip_prefix(':'))
            .map(str::trim)
    })
}

/// TermOS's skills.
pub const SKILLS: &[Skill] = &[
    Skill {
        name: "termos",
        files: &[("SKILL.md", include_str!("../skills/termos/SKILL.md"))],
    },
    Skill {
        name: "termos-dev",
        files: &[("SKILL.md", include_str!("../skills/termos-dev/SKILL.md"))],
    },
    Skill {
        name: "termos-agents",
        files: &[("SKILL.md", include_str!("../skills/termos-agents/SKILL.md"))],
    },
];

/// The skill named `name`.
pub fn skill(name: &str) -> Option<&'static Skill> {
    SKILLS.iter().find(|s| s.name == name)
}

/// The file in each copy that says TermOS wrote it.
pub const MARKER: &str = ".termos-skill";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Marker {
    /// The TermOS version that wrote it.
    termos: String,
    /// SHA-256 of each file it wrote.
    files: BTreeMap<String, String>,
}

fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn sums(skill: &Skill) -> BTreeMap<String, String> {
    skill
        .files
        .iter()
        .map(|(n, c)| ((*n).to_owned(), hash(c.as_bytes())))
        .collect()
}

fn read_marker(dir: &Path) -> Option<Marker> {
    let text = std::fs::read_to_string(dir.join(MARKER)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Where a skill stands in a skills folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Not there.
    Missing,
    /// TermOS's, as this version writes it.
    Current,
    /// TermOS's, unchanged, from another version.
    Outdated,
    /// TermOS's, changed by the user since.
    Edited,
    /// The user's own skill of the same name.
    Theirs,
}

impl State {
    pub fn describe(self) -> &'static str {
        match self {
            Self::Missing => "not installed",
            Self::Current => "installed",
            Self::Outdated => "out of date",
            Self::Edited => "changed by you (kept)",
            Self::Theirs => "a skill of yours has this name (kept)",
        }
    }
}

/// The state of `skill` in `skills_dir`.
pub fn state(skills_dir: &Path, skill: &Skill) -> State {
    let dir = skills_dir.join(skill.name);
    if std::fs::symlink_metadata(&dir).is_err() {
        return State::Missing;
    }
    let Some(marker) = read_marker(&dir) else {
        return State::Theirs;
    };
    for (name, sum) in &marker.files {
        match std::fs::read(dir.join(name)) {
            Ok(b) if hash(&b) == *sum => {}
            _ => return State::Edited,
        }
    }
    if marker.files == sums(skill) {
        State::Current
    } else {
        State::Outdated
    }
}

/// What installing or removing did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Installed,
    Updated,
    Removed,
    /// Nothing to do.
    Unchanged,
    /// Left alone: the user's own, or changed by them.
    Kept(State),
}

/// Writes `skill` into `skills_dir` (creating it), or updates TermOS's
/// copy there. `force` replaces a copy the user changed; the user's own
/// skill of the same name is never replaced.
pub fn install(skills_dir: &Path, skill: &Skill, force: bool) -> io::Result<Outcome> {
    let st = state(skills_dir, skill);
    let outcome = match st {
        State::Current => return Ok(Outcome::Unchanged),
        State::Theirs => return Ok(Outcome::Kept(st)),
        State::Edited if !force => return Ok(Outcome::Kept(st)),
        State::Missing => Outcome::Installed,
        State::Outdated | State::Edited => Outcome::Updated,
    };
    let dir = skills_dir.join(skill.name);
    // Files an older version wrote that this one does not.
    if let Some(old) = read_marker(&dir) {
        for name in old.files.keys() {
            if !skill.files.iter().any(|(n, _)| n == name) {
                let _ = std::fs::remove_file(dir.join(name));
            }
        }
    }
    for (name, content) in skill.files {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_new(&path, content.as_bytes())?;
    }
    let marker = Marker {
        termos: env!("CARGO_PKG_VERSION").to_owned(),
        files: sums(skill),
    };
    let text = serde_json::to_string_pretty(&marker).map_err(io::Error::other)?;
    write_new(&dir.join(MARKER), text.as_bytes())?;
    Ok(outcome)
}

/// Writes a file through a temporary one beside it, so a CLI reading the
/// folder never sees half of it.
fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".termos-new");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Removes TermOS's copy of `skill` from `skills_dir` (`force`: also one
/// the user changed). Files the user added to the folder stay, and so does
/// the folder then.
pub fn remove(skills_dir: &Path, skill: &Skill, force: bool) -> io::Result<Outcome> {
    let st = state(skills_dir, skill);
    match st {
        State::Missing => return Ok(Outcome::Unchanged),
        State::Theirs => return Ok(Outcome::Kept(st)),
        State::Edited if !force => return Ok(Outcome::Kept(st)),
        State::Current | State::Outdated | State::Edited => {}
    }
    let dir = skills_dir.join(skill.name);
    if let Some(marker) = read_marker(&dir) {
        for name in marker.files.keys() {
            let _ = std::fs::remove_file(dir.join(name));
        }
    }
    std::fs::remove_file(dir.join(MARKER))?;
    // Empty folders the skill's files were in, then its own.
    let _ = std::fs::remove_dir(&dir);
    Ok(Outcome::Removed)
}

/// A skills folder and who reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub dir: PathBuf,
    /// "Claude Code (default)", "Claude Code · Work", or the CLIs of the
    /// shared folder.
    pub readers: Vec<String>,
    /// Claude Code's (`Some`), or the shared one.
    pub tool: Option<Tool>,
}

/// The CLIs that read the shared folder.
pub const SHARED_READERS: &str = "Codex CLI, Gemini CLI, opencode";

/// The shared Agent Skills folder: `~/.agents/skills`.
pub fn shared_dir(home: &Path) -> PathBuf {
    home.join(".agents").join("skills")
}

/// The path a folder really is, for telling two names of one folder
/// apart: itself when it exists, else its nearest existing parent's.
fn real(path: &Path) -> PathBuf {
    if let Ok(p) = path.canonicalize() {
        return p;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => real(parent).join(name),
        _ => path.to_owned(),
    }
}

/// The skills folders: each Claude Code account's (`claude`; one entry
/// for accounts that share a folder) and the shared one (`shared`).
pub fn folders(
    accounts: &Accounts,
    get: &dyn Fn(&str) -> Option<String>,
    home: Option<&Path>,
    claude: bool,
    shared: bool,
) -> Vec<Folder> {
    let mut out: Vec<(PathBuf, Folder)> = Vec::new();
    if claude {
        for a in accounts.list(Tool::Claude) {
            let Some(dir) = a.home(get, home).map(|h| h.join("skills")) else {
                continue;
            };
            let reader = if a.is_default() {
                format!("{} (default)", Tool::Claude.title())
            } else {
                format!("{} · {}", Tool::Claude.title(), a.title())
            };
            let key = real(&dir);
            match out.iter_mut().find(|(k, _)| *k == key) {
                Some((_, f)) => f.readers.push(reader),
                None => out.push((
                    key,
                    Folder {
                        dir,
                        readers: vec![reader],
                        tool: Some(Tool::Claude),
                    },
                )),
            }
        }
    }
    if shared && let Some(h) = home {
        let dir = shared_dir(h);
        out.push((
            real(&dir),
            Folder {
                dir,
                readers: vec![SHARED_READERS.to_owned()],
                tool: None,
            },
        ));
    }
    out.into_iter().map(|(_, f)| f).collect()
}

/// The file that says the user removed the skills, so TermOS does not put
/// them back by itself (until `termos skills install`).
pub fn off_file(data_dir: &Path) -> PathBuf {
    data_dir.join("skills-off")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_skill_names_itself_and_says_when_to_use_it() {
        for s in SKILLS {
            let text = s.text();
            assert_eq!(front_matter(text, "name"), Some(s.name), "{}", s.name);
            let d = s.description();
            // Agent Skills' limits: a name of lowercase words, a
            // description of at most 1024 characters.
            assert!(
                s.name.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
                "{}",
                s.name
            );
            assert!(!d.is_empty() && d.len() <= 1024, "{}: {d}", s.name);
            assert!(d.contains("TERMOS"), "{} says when it applies", s.name);
            assert!(!text.contains('\r'), "{}: LF only", s.name);
        }
    }

    #[test]
    fn copies_are_installed_updated_kept_and_removed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let skills = dir.path().join("skills");
        let s = &SKILLS[0];
        assert_eq!(state(&skills, s), State::Missing);
        assert_eq!(
            install(&skills, s, false).expect("install"),
            Outcome::Installed
        );
        assert_eq!(state(&skills, s), State::Current);
        assert_eq!(
            std::fs::read_to_string(skills.join(s.name).join("SKILL.md")).expect("read"),
            s.text()
        );
        assert_eq!(
            install(&skills, s, false).expect("again"),
            Outcome::Unchanged
        );

        // An older version's copy, unchanged: updated.
        let older = Skill {
            name: s.name,
            files: &[
                ("SKILL.md", "---\nname: termos\n---\nold\n"),
                ("extra.md", "x"),
            ],
        };
        let other = dir.path().join("other");
        install(&other, &older, false).expect("older");
        assert_eq!(state(&other, s), State::Outdated);
        assert_eq!(install(&other, s, false).expect("update"), Outcome::Updated);
        assert_eq!(state(&other, s), State::Current);
        assert!(!other.join(s.name).join("extra.md").exists(), "dropped");

        // Changed by the user: kept, unless forced.
        let skill_md = skills.join(s.name).join("SKILL.md");
        std::fs::write(&skill_md, "mine now").expect("edit");
        assert_eq!(state(&skills, s), State::Edited);
        assert_eq!(
            install(&skills, s, false).expect("kept"),
            Outcome::Kept(State::Edited)
        );
        assert_eq!(
            std::fs::read_to_string(&skill_md).expect("read"),
            "mine now"
        );
        assert_eq!(
            remove(&skills, s, false).expect("kept"),
            Outcome::Kept(State::Edited)
        );
        assert_eq!(install(&skills, s, true).expect("forced"), Outcome::Updated);
        assert_eq!(state(&skills, s), State::Current);

        // Removed, with a file of the user's left in its folder.
        std::fs::write(skills.join(s.name).join("notes.txt"), "mine").expect("note");
        assert_eq!(remove(&skills, s, false).expect("remove"), Outcome::Removed);
        assert!(!skill_md.exists() && !skills.join(s.name).join(MARKER).exists());
        assert!(skills.join(s.name).join("notes.txt").exists());
        // That folder is now the user's.
        assert_eq!(state(&skills, s), State::Theirs);
        std::fs::remove_file(skills.join(s.name).join("notes.txt")).expect("rm");
        std::fs::remove_dir(skills.join(s.name)).expect("rmdir");
        assert_eq!(state(&skills, s), State::Missing);
    }

    #[test]
    fn a_skill_of_the_users_is_never_touched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = &SKILLS[1];
        let theirs = dir.path().join(s.name);
        std::fs::create_dir_all(&theirs).expect("mkdir");
        std::fs::write(theirs.join("SKILL.md"), "their own").expect("write");
        assert_eq!(state(dir.path(), s), State::Theirs);
        for force in [false, true] {
            assert_eq!(
                install(dir.path(), s, force).expect("install"),
                Outcome::Kept(State::Theirs)
            );
            assert_eq!(
                remove(dir.path(), s, force).expect("remove"),
                Outcome::Kept(State::Theirs)
            );
        }
        assert_eq!(
            std::fs::read_to_string(theirs.join("SKILL.md")).expect("read"),
            "their own"
        );
    }

    #[test]
    fn accounts_that_share_a_folder_are_one_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        let data = dir.path().join("data");
        std::fs::create_dir_all(home.join(".claude").join("skills")).expect("skills");
        let mut accounts = Accounts::load(&data).expect("load");
        // `work` links the default's skills (where links work); `play` has
        // its own folder.
        let default_home = home.join(".claude");
        let work = accounts
            .add(Tool::Claude, "work", None, Some(&default_home))
            .expect("work");
        let play = accounts
            .add(Tool::Claude, "play", None, Some(&default_home))
            .expect("play");
        let play_skills = play.dir.clone().expect("dir").join("skills");
        if let Ok(meta) = std::fs::symlink_metadata(&play_skills) {
            if meta.file_type().is_symlink() {
                std::fs::remove_file(&play_skills).expect("unlink");
            } else {
                std::fs::remove_dir_all(&play_skills).expect("rm");
            }
        }
        let get = |_: &str| None;
        let all = folders(&accounts, &get, Some(&home), true, true);
        let claude: Vec<&Folder> = all.iter().filter(|f| f.tool.is_some()).collect();
        // Windows copies where it may not link.
        let linked = std::fs::symlink_metadata(work.dir.clone().expect("dir").join("skills"))
            .is_ok_and(|m| m.file_type().is_symlink());
        assert_eq!(claude.len(), if linked { 2 } else { 3 }, "{all:?}");
        let default = &claude[0];
        assert_eq!(default.dir, home.join(".claude").join("skills"));
        if linked {
            assert_eq!(
                default.readers,
                ["Claude Code (default)", "Claude Code · work"]
            );
        }
        assert!(claude.iter().any(|f| f.dir == play_skills));
        let shared = all.last().expect("shared");
        assert_eq!(shared.tool, None);
        assert_eq!(shared.dir, home.join(".agents").join("skills"));
        // Only the kinds asked for.
        assert!(folders(&accounts, &get, Some(&home), false, true).len() == 1);
        assert!(folders(&accounts, &get, None, false, true).is_empty());
    }
}
