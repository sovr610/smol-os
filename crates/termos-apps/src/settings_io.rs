// SPDX-License-Identifier: GPL-3.0-or-later
//! Settings that move (M13g, D-126): what `termos settings export` puts in
//! its archive, and how `import` reads one back.
//!
//! The archive (`.tar.gz`, everything under `termos-settings/`) holds the
//! config file, the user's layouts and themes, what the gallery installed
//! (its list, not the files), and the names of the AI accounts. Never
//! credentials, keys, the secrets file, histories, logs or snapshots: they
//! are not collected, and the command checks the text it collects for
//! anything shaped like a key before writing.
//!
//! Importing shows what each file would change. The config is merged
//! into the user's with `toml_edit` (the user's comments kept, the
//! archive's values winning), and files that differ are replaced only
//! when the user says so.

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

/// The folder everything is under inside the archive.
pub const ROOT: &str = "termos-settings";
/// The most one file of the archive may weigh, and all of them.
const MAX_FILE: u64 = 8 * 1024 * 1024;
const MAX_TOTAL: u64 = 64 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("not a TermOS settings archive: {0}")]
    Archive(String),
    #[error("{0}: {1}")]
    Config(String, String),
}

/// Where the settings are on this machine.
#[derive(Debug, Clone)]
pub struct Sources {
    pub config_file: PathBuf,
    pub layouts_dir: PathBuf,
    pub themes_dir: PathBuf,
    /// The gallery's record of what it installed.
    pub gallery_record: PathBuf,
    /// The AI accounts' index (`<data>/accounts/accounts.toml`).
    pub accounts_index: PathBuf,
}

impl Sources {
    pub fn from_paths(paths: &termos_core::paths::Paths, config_file: &Path) -> Self {
        let config_dir = config_file
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_owned);
        Self {
            config_file: config_file.to_owned(),
            layouts_dir: paths.layouts_dir(),
            themes_dir: config_dir.join("themes"),
            gallery_record: paths.state_dir.join("gallery").join("installed.json"),
            accounts_index: paths.data_dir.join("accounts").join("accounts.toml"),
        }
    }

    /// Where an archive's file goes on this machine; `None` for a file
    /// this TermOS does not import.
    pub fn destination(&self, name: &str) -> Option<PathBuf> {
        let (dir, file) = match name.split_once('/') {
            Some((d, f)) => (Some(d), f),
            None => (None, name),
        };
        let toml_file = |f: &str| f.strip_suffix(".toml").is_some_and(plain_name);
        match (dir, file) {
            (None, "config.toml") => Some(self.config_file.clone()),
            (Some("layouts"), f) if toml_file(f) => Some(self.layouts_dir.join(f)),
            (Some("themes"), f) if toml_file(f) => Some(self.themes_dir.join(f)),
            _ => None,
        }
    }
}

/// Whether `stem` is a plain file name: letters, digits, `-`, `_` and
/// `.`, not starting with a dot.
fn plain_name(stem: &str) -> bool {
    !stem.is_empty()
        && stem.len() <= 64
        && !stem.starts_with('.')
        && stem
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// The files of an archive: name (below [`ROOT`]) → contents.
pub type Files = BTreeMap<String, Vec<u8>>;

/// The `*.toml` files directly in `dir`, as `prefix/NAME.toml`.
fn toml_files(dir: &Path, prefix: &str, out: &mut Files) -> std::io::Result<()> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".toml") || !e.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        out.insert(format!("{prefix}/{name}"), std::fs::read(e.path())?);
    }
    Ok(())
}

/// The account names in an accounts index, one `CLI NAME` per line.
pub fn account_names(index: &str) -> String {
    let Ok(v) = index.parse::<toml_edit::DocumentMut>() else {
        return String::new();
    };
    let mut out = String::new();
    if let Some(accounts) = v.get("accounts").and_then(|a| a.as_table_like()) {
        for (cli, names) in accounts.iter() {
            if let Some(names) = names.as_table_like() {
                for (name, _) in names.iter() {
                    out.push_str(&format!("{cli} {name}\n"));
                }
            }
        }
    }
    out
}

/// Everything an export holds, read from this machine.
pub fn collect(src: &Sources, version: &str) -> std::io::Result<Files> {
    let mut files = Files::new();
    if let Ok(c) = std::fs::read(&src.config_file) {
        files.insert("config.toml".into(), c);
    }
    toml_files(&src.layouts_dir, "layouts", &mut files)?;
    toml_files(&src.themes_dir, "themes", &mut files)?;
    if let Ok(g) = std::fs::read(&src.gallery_record) {
        files.insert("gallery.json".into(), g);
    }
    if let Ok(a) = std::fs::read_to_string(&src.accounts_index) {
        let names = account_names(&a);
        if !names.is_empty() {
            files.insert("accounts.txt".into(), names.into_bytes());
        }
    }
    let list: Vec<&str> = files.keys().map(String::as_str).collect();
    files.insert(
        "MANIFEST.txt".into(),
        format!(
            "TermOS {version} settings.\nNo keys, credentials, histories or snapshots are in it.\n\n{}\n",
            list.join("\n")
        )
        .into_bytes(),
    );
    Ok(files)
}

/// Writes `files` as a `.tar.gz` at `out` (made beside it, then renamed).
pub fn write_archive(files: &Files, out: &Path) -> std::io::Result<()> {
    let tmp = out.with_extension("termos-new");
    {
        let f = std::fs::File::create(&tmp)?;
        let gz = flate2::write::GzEncoder::new(f, flate2::Compression::default());
        let mut tar = tar::Builder::new(gz);
        for (name, data) in files {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o600);
            h.set_mtime(0);
            h.set_entry_type(tar::EntryType::Regular);
            h.set_cksum();
            tar.append_data(&mut h, format!("{ROOT}/{name}"), data.as_slice())?;
        }
        let mut gz = tar.into_inner()?;
        gz.flush()?;
        gz.finish()?.sync_all()?;
    }
    std::fs::rename(&tmp, out)
}

/// Reads an archive: only regular files below [`ROOT`] with plain names
/// (no `..`, no absolute paths), within the size limits.
pub fn read_archive(path: &Path) -> Result<Files, SettingsError> {
    let f = std::fs::File::open(path)?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(f));
    let mut files = Files::new();
    let mut total = 0u64;
    for entry in tar
        .entries()
        .map_err(|e| SettingsError::Archive(e.to_string()))?
    {
        let mut entry = entry.map_err(|e| SettingsError::Archive(e.to_string()))?;
        if entry.header().entry_type() != tar::EntryType::Regular {
            continue;
        }
        let name = entry
            .path()
            .map_err(|e| SettingsError::Archive(e.to_string()))?
            .to_string_lossy()
            .replace('\\', "/");
        let Some(rel) = name.strip_prefix(&format!("{ROOT}/")) else {
            continue;
        };
        if rel
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
        {
            continue;
        }
        let size = entry.size();
        if size > MAX_FILE || total + size > MAX_TOTAL {
            return Err(SettingsError::Archive("a file is too large".into()));
        }
        total += size;
        let mut data = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
        entry.read_to_end(&mut data)?;
        files.insert(rel.to_owned(), data);
    }
    if !files.contains_key("MANIFEST.txt") {
        return Err(SettingsError::Archive(format!(
            "{} has no MANIFEST.txt",
            path.display()
        )));
    }
    Ok(files)
}

/// What importing a file would do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// Nothing there yet.
    New,
    /// The same already.
    Same,
    /// Something else is there: the user says whether to replace it.
    Differs,
}

/// The change for a file going to `dest`.
pub fn change(dest: &Path, data: &[u8]) -> Change {
    match std::fs::read(dest) {
        Ok(now) if now == data => Change::Same,
        Ok(_) => Change::Differs,
        Err(_) => Change::New,
    }
}

/// Merges `imported` into `mine` (both config files' text): every value
/// the archive sets wins, every other line of `mine` stays, comments
/// included. Returns the new text and the keys it changes
/// (`theme.base`).
pub fn merge_config(mine: &str, imported: &str) -> Result<(String, Vec<String>), SettingsError> {
    let mut doc: toml_edit::DocumentMut = mine
        .parse()
        .map_err(|e| SettingsError::Config("your config.toml".into(), format!("{e}")))?;
    let theirs: toml_edit::DocumentMut = imported
        .parse()
        .map_err(|e| SettingsError::Config("the archive's config.toml".into(), format!("{e}")))?;
    let mut changed = Vec::new();
    merge_table(doc.as_table_mut(), theirs.as_table(), "", &mut changed);
    Ok((doc.to_string(), changed))
}

fn merge_table(
    mine: &mut toml_edit::Table,
    theirs: &toml_edit::Table,
    prefix: &str,
    changed: &mut Vec<String>,
) {
    for (key, item) in theirs.iter() {
        let path = if prefix.is_empty() {
            key.to_owned()
        } else {
            format!("{prefix}.{key}")
        };
        match (mine.get_mut(key), item) {
            (Some(toml_edit::Item::Table(m)), toml_edit::Item::Table(t)) => {
                merge_table(m, t, &path, changed);
            }
            (Some(existing), _) => {
                if existing.to_string().trim() != item.to_string().trim() {
                    *existing = item.clone();
                    changed.push(path);
                }
            }
            (None, _) => {
                mine.insert(key, item.clone());
                changed.push(path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sources(dir: &Path) -> Sources {
        Sources {
            config_file: dir.join("config/config.toml"),
            layouts_dir: dir.join("config/layouts"),
            themes_dir: dir.join("config/themes"),
            gallery_record: dir.join("state/gallery/installed.json"),
            accounts_index: dir.join("data/accounts/accounts.toml"),
        }
    }

    fn put(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    }

    #[test]
    fn a_round_trip_through_an_archive() {
        let old = tempfile::tempdir().expect("dir");
        let s = sources(old.path());
        put(&s.config_file, "# mine\n[theme]\nbase = \"dark\"\n");
        put(
            &s.layouts_dir.join("dev.toml"),
            "[[pane]]\ncommand = \"nvim\"\n",
        );
        put(&s.layouts_dir.join("notes.txt"), "not a layout");
        put(
            &s.themes_dir.join("nord.toml"),
            "[theme]\naccent = \"#88c0d0\"\n",
        );
        put(&s.gallery_record, "{}");
        put(
            &s.accounts_index,
            "[active]\nclaude = \"work\"\n[accounts.claude.work]\nlabel = \"Work\"\n[accounts.codex.default]\n",
        );
        // Not collected: the secrets file and histories live elsewhere.
        put(
            &old.path().join("data/secrets.age"),
            "age-encryption.org/v1",
        );
        let files = collect(&s, "0.0.0").expect("collected");
        let names: Vec<&str> = files.keys().map(String::as_str).collect();
        assert_eq!(
            names,
            [
                "MANIFEST.txt",
                "accounts.txt",
                "config.toml",
                "gallery.json",
                "layouts/dev.toml",
                "themes/nord.toml"
            ]
        );
        assert_eq!(files["accounts.txt"], b"claude work\ncodex default\n");
        let archive = old.path().join("settings.tar.gz");
        write_archive(&files, &archive).expect("written");
        let back = read_archive(&archive).expect("read");
        assert_eq!(back, files);

        let new = tempfile::tempdir().expect("dir");
        let t = sources(new.path());
        assert_eq!(
            t.destination("layouts/dev.toml"),
            Some(t.layouts_dir.join("dev.toml"))
        );
        assert_eq!(t.destination("config.toml"), Some(t.config_file.clone()));
        assert_eq!(
            t.destination("gallery.json"),
            None,
            "reinstalled, not copied"
        );
        assert_eq!(t.destination("layouts/../x.toml"), None);
        assert_eq!(t.destination("themes/a/b.toml"), None);
        let dest = t.destination("themes/nord.toml").expect("a theme");
        assert_eq!(change(&dest, &back["themes/nord.toml"]), Change::New);
        put(&dest, "[theme]\naccent = \"#88c0d0\"\n");
        assert_eq!(change(&dest, &back["themes/nord.toml"]), Change::Same);
        put(&dest, "[theme]\n");
        assert_eq!(change(&dest, &back["themes/nord.toml"]), Change::Differs);
    }

    #[test]
    fn the_config_is_merged_keeping_comments() {
        let mine =
            "# my comment\n[theme]\nbase = \"light\"   # light at work\n[ui]\nicons = \"ascii\"\n";
        let theirs = "[theme]\nbase = \"dark\"\naccent = \"#ff0000\"\n[files]\ntwo-pane = true\n";
        let (text, changed) = merge_config(mine, theirs).expect("merged");
        assert!(text.starts_with("# my comment"), "{text}");
        assert!(text.contains("icons = \"ascii\""), "{text}");
        assert_eq!(changed, ["theme.base", "theme.accent", "files"]);
        let cfg = termos_core::config::Config::parse(&text, Path::new("c.toml")).expect("valid");
        assert!(cfg.files.two_pane);
        // The same values again change nothing.
        let (_, again) = merge_config(&text, theirs).expect("merged");
        assert!(again.is_empty(), "{again:?}");
        assert!(merge_config("x = [", theirs).is_err());
    }

    #[test]
    fn archives_are_checked() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("x.tar.gz");
        std::fs::write(&path, b"not gzip").expect("write");
        assert!(read_archive(&path).is_err());
        let mut files = Files::new();
        files.insert("config.toml".into(), b"".to_vec());
        write_archive(&files, &path).expect("written");
        assert!(
            matches!(read_archive(&path), Err(SettingsError::Archive(_))),
            "no manifest"
        );
    }
}
