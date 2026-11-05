// SPDX-License-Identifier: GPL-3.0-or-later
//! The gallery (M13g, D-125, D-126): a signed index of things to install
//! into TermOS. At first only data and sandboxed code: themes, layouts and
//! WASM status-line widgets.
//!
//! - **The index** is one JSON file in the `termos-gallery` repository.
//!   Each entry names a file (an `https` address) and its SHA-256, and
//!   carries an Ed25519 signature by the gallery's key over all its fields.
//!   The maintainers sign entries offline (`termos gallery sign`) after
//!   reviewing them.
//! - **Checking:** every entry's signature must verify with a key built
//!   into TermOS, or the whole index is refused; a downloaded file must
//!   match its SHA-256, and must be what its kind says (a theme, a layout,
//!   a WebAssembly module), before anything is written.
//! - **Installing:** themes go to `themes/NAME.toml` beside the config
//!   (`[theme] name = "NAME"` uses one), layouts to `layouts/NAME.toml`,
//!   widgets to `widgets/NAME.wasm` with a `[widgets.NAME]` table added to
//!   the config. What was installed is recorded in the state folder.
//!
//! Downloads use the system's `curl`, as TermOS's other downloads do.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use base64::Engine as _;
use ed25519_dalek::{Signer as _, Verifier as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

/// Where the index is, unless `[gallery] index` says otherwise.
pub const INDEX_URL: &str = termos_core::config::GALLERY_INDEX;

/// The gallery keys this build trusts: (id, public key in hex). An index
/// names the key that signed it, so a key can be replaced by a new one
/// while both are listed.
// TODO(M13g): the owner's gallery key, once made with `termos gallery keygen` (docs/gallery.md).
pub const KEYS: &[(&str, &str)] = &[];

/// The most an index may weigh, and the most a file may.
pub const MAX_INDEX: u64 = 4 * 1024 * 1024;
pub const MAX_FILE: u64 = 16 * 1024 * 1024;
/// The most entries an index may hold.
pub const MAX_ENTRIES: usize = 4096;

/// The index format this TermOS reads.
pub const FORMAT: u32 = 1;

/// What the signatures sign, before an entry's fields.
const DOMAIN: &str = "termos-gallery-v1";

#[derive(Debug, thiserror::Error)]
pub enum GalleryError {
    #[error("the gallery index is not valid: {0}")]
    Index(String),
    #[error(
        "this TermOS does not trust the gallery's key `{0}`: update TermOS, or check `[gallery] index`"
    )]
    UnknownKey(String),
    #[error("this build of TermOS has no gallery key yet (docs/gallery.md)")]
    NoKeys,
    #[error("the gallery index's signature for `{0}` does not verify: the index is refused")]
    BadSignature(String),
    #[error(
        "`{name}` does not match the gallery's checksum (expected {expected}, got {got}): nothing was installed"
    )]
    Hash {
        name: String,
        expected: String,
        got: String,
    },
    #[error("`{0}` is not what the gallery says it is: {1}")]
    Content(String, String),
    #[error("nothing named `{0}` in the gallery")]
    NotFound(String),
    #[error("`{0}` is not installed")]
    NotInstalled(String),
    #[error("download failed: {0}")]
    Fetch(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// What an entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Theme,
    Layout,
    Widget,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Theme => "theme",
            Kind::Layout => "layout",
            Kind::Widget => "widget",
        }
    }
}

/// One entry as the index file holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub name: String,
    /// `theme`, `layout`, `widget`; other kinds (for later TermOS
    /// versions) are skipped.
    pub kind: String,
    pub version: String,
    pub summary: String,
    pub url: String,
    pub sha256: String,
    pub author: String,
    pub license: String,
    /// Base64 of the Ed25519 signature over [`signed_message`].
    #[serde(default)]
    pub signature: String,
}

impl Entry {
    /// Its kind, if this TermOS installs it.
    pub fn kind(&self) -> Option<Kind> {
        match self.kind.as_str() {
            "theme" => Some(Kind::Theme),
            "layout" => Some(Kind::Layout),
            "widget" => Some(Kind::Widget),
            _ => None,
        }
    }

    /// Whether `query`'s words are all in its name, kind or summary
    /// (ignoring case).
    pub fn matches(&self, query: &str) -> bool {
        let hay = format!("{} {} {}", self.name, self.kind, self.summary).to_lowercase();
        query
            .split_whitespace()
            .all(|w| hay.contains(&w.to_lowercase()))
    }
}

/// The index file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexFile {
    pub format: u32,
    /// The id of the key that signed the entries ([`key_id`]).
    pub key: String,
    pub entries: Vec<Entry>,
}

/// A checked index: every entry signed, each of a kind this TermOS
/// installs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Index {
    pub entries: Vec<Entry>,
    /// Entries of kinds this TermOS does not install yet.
    pub skipped: usize,
}

impl Index {
    pub fn get(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.name == name)
    }
}

/// Whether `name` can be an entry's name: lowercase letters, digits and
/// `-`, starting with a letter or digit, at most 40.
pub fn valid_name(name: &str) -> bool {
    (1..=40).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// A file's SHA-256, in lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// A key's id: the first 16 hex digits of its public key's SHA-256.
pub fn key_id(public: &[u8; 32]) -> String {
    sha256_hex(public)[..16].to_owned()
}

/// What an entry's signature signs: a fixed prefix, then each field but
/// the signature on a line of its own (fields hold no line breaks).
pub fn signed_message(e: &Entry) -> Vec<u8> {
    [
        DOMAIN, &e.kind, &e.name, &e.version, &e.url, &e.sha256, &e.author, &e.license, &e.summary,
    ]
    .join("\n")
    .into_bytes()
}

/// The checks an entry passes before its signature is looked at.
fn check_fields(e: &Entry) -> Result<(), String> {
    let n = &e.name;
    if !valid_name(n) {
        return Err(format!("`{n}` is not a valid name"));
    }
    let fields = [
        &e.kind, &e.version, &e.url, &e.sha256, &e.author, &e.license, &e.summary,
    ];
    if fields.iter().any(|f| f.contains(['\n', '\r'])) {
        return Err(format!("`{n}`: a field holds a line break"));
    }
    if e.version.is_empty()
        || e.version.len() > 32
        || !e
            .version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
    {
        return Err(format!("`{n}`: `{}` is not a version", e.version));
    }
    if !e.url.starts_with("https://") || e.url.len() > 2048 {
        return Err(format!("`{n}`: its address is not https"));
    }
    if unhex::<32>(&e.sha256).is_none() || e.sha256.chars().any(|c| c.is_ascii_uppercase()) {
        return Err(format!("`{n}`: its sha256 is not 64 lowercase hex digits"));
    }
    if e.summary.chars().count() > 200 || e.author.chars().count() > 100 || e.license.len() > 64 {
        return Err(format!("`{n}`: a field is too long"));
    }
    Ok(())
}

/// Parses an index file (no signature checks).
pub fn parse_index(bytes: &[u8]) -> Result<IndexFile, GalleryError> {
    if bytes.len() as u64 > MAX_INDEX {
        return Err(GalleryError::Index("larger than 4 MiB".into()));
    }
    let file: IndexFile =
        serde_json::from_slice(bytes).map_err(|e| GalleryError::Index(e.to_string()))?;
    if file.format != FORMAT {
        return Err(GalleryError::Index(format!(
            "format {} (this TermOS reads format {FORMAT}: update TermOS)",
            file.format
        )));
    }
    if file.entries.len() > MAX_ENTRIES {
        return Err(GalleryError::Index("too many entries".into()));
    }
    let mut seen = std::collections::BTreeSet::new();
    for e in &file.entries {
        check_fields(e).map_err(GalleryError::Index)?;
        if !seen.insert(e.name.as_str()) {
            return Err(GalleryError::Index(format!("`{}` is listed twice", e.name)));
        }
    }
    Ok(file)
}

/// The built-in keys, decoded.
pub fn builtin_keys() -> Vec<(String, [u8; 32])> {
    KEYS.iter()
        .filter_map(|(id, k)| Some(((*id).to_owned(), unhex::<32>(k)?)))
        .collect()
}

/// Checks every entry's signature with the key the index names, among
/// `keys`; keeps the kinds this TermOS installs.
pub fn verify(file: &IndexFile, keys: &[(String, [u8; 32])]) -> Result<Index, GalleryError> {
    if keys.is_empty() {
        return Err(GalleryError::NoKeys);
    }
    let Some((_, public)) = keys.iter().find(|(id, _)| *id == file.key) else {
        return Err(GalleryError::UnknownKey(file.key.clone()));
    };
    let key = ed25519_dalek::VerifyingKey::from_bytes(public)
        .map_err(|_| GalleryError::UnknownKey(file.key.clone()))?;
    let mut entries = Vec::new();
    let mut skipped = 0;
    for e in &file.entries {
        let sig = base64::engine::general_purpose::STANDARD
            .decode(&e.signature)
            .ok()
            .and_then(|b| <[u8; 64]>::try_from(b).ok())
            .ok_or_else(|| GalleryError::BadSignature(e.name.clone()))?;
        let sig = ed25519_dalek::Signature::from_bytes(&sig);
        key.verify(&signed_message(e), &sig)
            .map_err(|_| GalleryError::BadSignature(e.name.clone()))?;
        if e.kind().is_some() {
            entries.push(e.clone());
        } else {
            skipped += 1;
        }
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Index { entries, skipped })
}

/// Signs every entry with `secret` and names its key in the index: the
/// maintainers' step, offline (`termos gallery sign`).
pub fn sign(file: &mut IndexFile, secret: &[u8; 32]) -> Result<(), GalleryError> {
    let key = ed25519_dalek::SigningKey::from_bytes(secret);
    for e in &mut file.entries {
        check_fields(e).map_err(GalleryError::Index)?;
        let sig = key.sign(&signed_message(e));
        e.signature = base64::engine::general_purpose::STANDARD.encode(sig.to_bytes());
    }
    file.key = key_id(&key.verifying_key().to_bytes());
    Ok(())
}

/// A new signing key: (secret, public), the secret to keep offline.
pub fn keygen() -> Result<([u8; 32], [u8; 32]), GalleryError> {
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret)
        .map_err(|e| GalleryError::Io(std::io::Error::other(e.to_string())))?;
    let public = ed25519_dalek::SigningKey::from_bytes(&secret)
        .verifying_key()
        .to_bytes();
    Ok((secret, public))
}

/// Hex helpers for the key files.
pub fn to_hex(bytes: &[u8]) -> String {
    hex(bytes)
}

pub fn from_hex32(s: &str) -> Option<[u8; 32]> {
    unhex::<32>(s.trim())
}

/// Downloads `url` (https only) with the system's curl, at most `max`
/// bytes. A local path is read instead (a maintainer's index).
pub fn fetch(url: &str, max: u64) -> Result<Vec<u8>, GalleryError> {
    if !url.starts_with("https://") {
        let p = Path::new(url);
        if p.is_absolute() || url.starts_with('.') {
            let bytes = std::fs::read(p)?;
            if bytes.len() as u64 > max {
                return Err(GalleryError::Fetch(format!("{url} is larger than allowed")));
            }
            return Ok(bytes);
        }
        return Err(GalleryError::Fetch(format!("{url}: only https addresses")));
    }
    let out = Command::new(if cfg!(windows) { "curl.exe" } else { "curl" })
        .args(["--fail", "--location", "--silent", "--show-error"])
        .args(["--proto", "=https", "--tlsv1.2", "--retry", "3"])
        .arg("--max-filesize")
        .arg(max.to_string())
        .arg(url)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| GalleryError::Fetch(format!("cannot run curl: {e}")))?;
    if !out.status.success() {
        return Err(GalleryError::Fetch(format!(
            "{url}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    if out.stdout.len() as u64 > max {
        return Err(GalleryError::Fetch(format!("{url} is larger than allowed")));
    }
    Ok(out.stdout)
}

/// Fetches and checks the index at `url` with the built-in keys.
pub fn load(url: &str) -> Result<Index, GalleryError> {
    let keys = builtin_keys();
    if keys.is_empty() {
        return Err(GalleryError::NoKeys);
    }
    let bytes = fetch(url, MAX_INDEX)?;
    let file = parse_index(&bytes)?;
    verify(&file, &keys)
}

/// Where things are installed.
#[derive(Debug, Clone)]
pub struct Places {
    pub config_file: PathBuf,
    pub layouts_dir: PathBuf,
    /// TermOS's state folder (`gallery/installed.json` is kept there).
    pub state_dir: PathBuf,
}

impl Places {
    pub fn from_paths(paths: &termos_core::paths::Paths, config_file: &Path) -> Self {
        Self {
            config_file: config_file.to_owned(),
            layouts_dir: paths.layouts_dir(),
            state_dir: paths.state_dir.clone(),
        }
    }

    fn config_dir(&self) -> PathBuf {
        self.config_file
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_owned)
    }

    pub fn themes_dir(&self) -> PathBuf {
        self.config_dir().join("themes")
    }

    pub fn widgets_dir(&self) -> PathBuf {
        self.config_dir().join("widgets")
    }

    fn record(&self) -> PathBuf {
        self.state_dir.join("gallery").join("installed.json")
    }
}

/// An installed entry, as recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    pub kind: Kind,
    pub version: String,
    /// The files written.
    pub files: Vec<PathBuf>,
}

/// What is installed, by name.
pub fn installed(places: &Places) -> BTreeMap<String, Installed> {
    std::fs::read(places.record())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_record(places: &Places, all: &BTreeMap<String, Installed>) -> Result<(), GalleryError> {
    let path = places.record();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_vec_pretty(all).map_err(|e| GalleryError::Index(e.to_string()))?;
    write_atomically(&path, &text)?;
    Ok(())
}

fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("termos-new");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// The config file's text with `edit` applied (a missing file is empty),
/// checked as a config before it is written; the user's comments stay.
fn edit_config(
    config: &Path,
    edit: impl FnOnce(&mut toml_edit::DocumentMut),
) -> Result<(), GalleryError> {
    let text = match std::fs::read_to_string(config) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e| GalleryError::Content("config.toml".into(), format!("{e}")))?;
    edit(&mut doc);
    let new = doc.to_string();
    termos_core::config::Config::parse(&new, config)
        .map_err(|e| GalleryError::Content("config.toml".into(), e.to_string()))?;
    write_atomically(config, new.as_bytes())?;
    Ok(())
}

/// Checks that `bytes` are what `kind` says.
pub fn check_content(name: &str, kind: Kind, bytes: &[u8]) -> Result<(), GalleryError> {
    let bad = |why: String| GalleryError::Content(name.to_owned(), why);
    match kind {
        Kind::Theme => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct ThemeFile {
                theme: termos_render::theme::ThemeConfig,
            }
            let text = std::str::from_utf8(bytes).map_err(|_| bad("not text".into()))?;
            let file: ThemeFile = toml::from_str(text)
                .map_err(|e| bad(format!("not a theme: {}", e.message().trim())))?;
            if file.theme.name.is_some() {
                return Err(bad("a theme cannot name another theme".into()));
            }
        }
        Kind::Layout => {
            let text = std::str::from_utf8(bytes).map_err(|_| bad("not text".into()))?;
            termos_core::layouts::Layout::parse(name, text, Path::new(name))
                .map_err(|e| bad(format!("not a layout: {e}")))?;
        }
        Kind::Widget => {
            if !bytes.starts_with(b"\0asm") {
                return Err(bad("not a WebAssembly module".into()));
            }
        }
    }
    Ok(())
}

/// Installs `entry` from its downloaded `bytes`: checked against its
/// SHA-256 and its kind, then written where TermOS reads it.
pub fn install(entry: &Entry, bytes: &[u8], places: &Places) -> Result<Installed, GalleryError> {
    let Some(kind) = entry.kind() else {
        return Err(GalleryError::NotFound(entry.name.clone()));
    };
    let got = sha256_hex(bytes);
    if got != entry.sha256 {
        return Err(GalleryError::Hash {
            name: entry.name.clone(),
            expected: entry.sha256.clone(),
            got,
        });
    }
    check_content(&entry.name, kind, bytes)?;
    let name = &entry.name;
    let file = match kind {
        Kind::Theme => places.themes_dir().join(format!("{name}.toml")),
        Kind::Layout => places.layouts_dir.join(format!("{name}.toml")),
        Kind::Widget => places.widgets_dir().join(format!("{name}.wasm")),
    };
    write_atomically(&file, bytes)?;
    if kind == Kind::Widget {
        let rel = format!("widgets/{name}.wasm");
        let n = name.clone();
        edit_config(&places.config_file, move |doc| {
            if !doc.contains_table("widgets") {
                let mut t = toml_edit::Table::new();
                t.set_implicit(true);
                doc.insert("widgets", toml_edit::Item::Table(t));
            }
            if doc["widgets"].get(&n).is_none() {
                let mut t = toml_edit::Table::new();
                t.insert("path", toml_edit::value(rel));
                doc["widgets"][n.as_str()] = toml_edit::Item::Table(t);
            }
        })?;
    }
    let done = Installed {
        kind,
        version: entry.version.clone(),
        files: vec![file],
    };
    let mut all = installed(places);
    all.insert(name.clone(), done.clone());
    save_record(places, &all)?;
    Ok(done)
}

/// Uses an installed theme: `[theme] name = "NAME"` in the config.
pub fn use_theme(name: &str, places: &Places) -> Result<(), GalleryError> {
    if !places.themes_dir().join(format!("{name}.toml")).is_file() {
        return Err(GalleryError::NotInstalled(name.to_owned()));
    }
    let n = name.to_owned();
    edit_config(&places.config_file, move |doc| {
        if !doc.contains_table("theme") {
            doc.insert("theme", toml_edit::Item::Table(toml_edit::Table::new()));
        }
        doc["theme"]["name"] = toml_edit::value(n);
    })
}

/// Removes an installed entry: its files, its `[widgets.NAME]` table, and
/// `[theme] name` if it named this theme.
pub fn remove(name: &str, places: &Places) -> Result<Installed, GalleryError> {
    let mut all = installed(places);
    let Some(done) = all.remove(name) else {
        return Err(GalleryError::NotInstalled(name.to_owned()));
    };
    for f in &done.files {
        match std::fs::remove_file(f) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    let n = name.to_owned();
    let kind = done.kind;
    edit_config(&places.config_file, move |doc| match kind {
        Kind::Widget => {
            if let Some(t) = doc.get_mut("widgets").and_then(|w| w.as_table_like_mut()) {
                t.remove(&n);
            }
        }
        Kind::Theme => {
            if let Some(t) = doc.get_mut("theme").and_then(|t| t.as_table_like_mut())
                && t.get("name").and_then(|v| v.as_str()) == Some(n.as_str())
            {
                t.remove("name");
            }
        }
        Kind::Layout => {}
    })?;
    save_record(places, &all)?;
    Ok(done)
}

/// Installed entries the index has a different version of.
pub fn updates<'a>(index: &'a Index, places: &Places) -> Vec<(&'a Entry, String)> {
    let all = installed(places);
    index
        .entries
        .iter()
        .filter_map(|e| {
            let i = all.get(&e.name)?;
            (i.version != e.version).then(|| (e, i.version.clone()))
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A fixture key (never a real gallery's).
    pub(crate) const FIXTURE_SECRET: [u8; 32] = [7u8; 32];

    pub(crate) fn fixture_keys() -> Vec<(String, [u8; 32])> {
        let public = ed25519_dalek::SigningKey::from_bytes(&FIXTURE_SECRET)
            .verifying_key()
            .to_bytes();
        vec![(key_id(&public), public)]
    }

    pub(crate) const THEME: &str = "[theme]\nbase = \"dark\"\naccent = \"#88c0d0\"\n";
    pub(crate) const LAYOUT: &str = "workspace = \"dev\"\n[[pane]]\ncommand = \"nvim .\"\n";
    pub(crate) const WIDGET: &[u8] = b"\0asm\x01\0\0\0";

    pub(crate) fn entry(name: &str, kind: &str, bytes: &[u8]) -> Entry {
        Entry {
            name: name.into(),
            kind: kind.into(),
            version: "1.0.0".into(),
            summary: format!("a {kind} for tests"),
            url: format!("https://example.org/{name}"),
            sha256: sha256_hex(bytes),
            author: "Test Author".into(),
            license: "MIT".into(),
            signature: String::new(),
        }
    }

    pub(crate) fn signed_index() -> IndexFile {
        let mut file = IndexFile {
            format: FORMAT,
            key: String::new(),
            entries: vec![
                entry("nord", "theme", THEME.as_bytes()),
                entry("dev", "layout", LAYOUT.as_bytes()),
                entry("clock", "widget", WIDGET),
                entry("later", "plugin", b"x"),
            ],
        };
        sign(&mut file, &FIXTURE_SECRET).expect("signed");
        file
    }

    fn places(dir: &Path) -> Places {
        Places {
            config_file: dir.join("config/config.toml"),
            layouts_dir: dir.join("config/layouts"),
            state_dir: dir.join("state"),
        }
    }

    #[test]
    fn a_signed_index_verifies_and_round_trips() {
        let file = signed_index();
        let bytes = serde_json::to_vec(&file).expect("json");
        let parsed = parse_index(&bytes).expect("parses");
        let index = verify(&parsed, &fixture_keys()).expect("verifies");
        let names: Vec<&str> = index.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["clock", "dev", "nord"]);
        assert_eq!(index.skipped, 1, "a plugin is for a later TermOS");
        assert!(index.get("nord").expect("nord").matches("THEME tests"));
        assert!(!index.get("nord").expect("nord").matches("widget"));
    }

    #[test]
    fn tampering_and_unknown_keys_are_refused() {
        let mut file = signed_index();
        file.entries[0].url = "https://evil.example/nord".into();
        assert!(matches!(
            verify(&file, &fixture_keys()),
            Err(GalleryError::BadSignature(n)) if n == "nord"
        ));
        let mut file = signed_index();
        file.entries[1].signature = "AAAA".into();
        assert!(matches!(
            verify(&file, &fixture_keys()),
            Err(GalleryError::BadSignature(_))
        ));
        let mut file = signed_index();
        file.key = "0000000000000000".into();
        assert!(matches!(
            verify(&file, &fixture_keys()),
            Err(GalleryError::UnknownKey(_))
        ));
        assert!(matches!(
            verify(&signed_index(), &[]),
            Err(GalleryError::NoKeys)
        ));
        // A key that is not the one the index names.
        let other = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32])
            .verifying_key()
            .to_bytes();
        let wrong = vec![(signed_index().key, other)];
        assert!(matches!(
            verify(&signed_index(), &wrong),
            Err(GalleryError::BadSignature(_))
        ));
    }

    #[test]
    fn bad_indexes_are_refused() {
        let ok = serde_json::to_value(signed_index()).expect("json");
        let with = |f: &dyn Fn(&mut serde_json::Value)| {
            let mut v = ok.clone();
            f(&mut v);
            parse_index(&serde_json::to_vec(&v).expect("json"))
        };
        assert!(with(&|_| {}).is_ok());
        assert!(with(&|v| v["format"] = 2.into()).is_err());
        assert!(with(&|v| v["entries"][0]["name"] = "Nord".into()).is_err());
        assert!(with(&|v| v["entries"][0]["url"] = "http://x.org/a".into()).is_err());
        assert!(with(&|v| v["entries"][0]["sha256"] = "abc".into()).is_err());
        assert!(with(&|v| v["entries"][0]["summary"] = "a\nb".into()).is_err());
        assert!(
            with(&|v| v["entries"][1]["name"] = "nord".into()).is_err(),
            "twice"
        );
        assert!(with(&|v| v["extra"] = 1.into()).is_err());
        assert!(parse_index(b"not json").is_err());
    }

    #[test]
    fn installing_puts_each_kind_where_termos_reads_it() {
        let dir = tempfile::tempdir().expect("dir");
        let p = places(dir.path());
        std::fs::create_dir_all(dir.path().join("config")).expect("mkdir");
        std::fs::write(&p.config_file, "# mine\n[ui]\nicons = \"ascii\"\n").expect("write");
        let index = verify(&signed_index(), &fixture_keys()).expect("verifies");

        // A wrong download is refused before anything is written.
        let nord = index.get("nord").expect("nord");
        assert!(matches!(
            install(nord, b"[theme]\nbase = \"light\"\n", &p),
            Err(GalleryError::Hash { .. })
        ));
        assert!(!p.themes_dir().exists());

        install(nord, THEME.as_bytes(), &p).expect("theme");
        assert!(p.themes_dir().join("nord.toml").is_file());
        use_theme("nord", &p).expect("used");
        install(index.get("dev").expect("dev"), LAYOUT.as_bytes(), &p).expect("layout");
        assert!(p.layouts_dir.join("dev.toml").is_file());
        install(index.get("clock").expect("clock"), WIDGET, &p).expect("widget");
        assert!(p.widgets_dir().join("clock.wasm").is_file());

        let text = std::fs::read_to_string(&p.config_file).expect("read");
        assert!(text.starts_with("# mine"), "{text}");
        let cfg = termos_core::config::Config::parse(&text, &p.config_file).expect("valid");
        assert_eq!(cfg.theme.name.as_deref(), Some("nord"));
        assert_eq!(
            cfg.widgets.get("clock").map(|w| w.path.clone()),
            Some(PathBuf::from("widgets/clock.wasm"))
        );
        // The theme's colours are used.
        assert_eq!(
            cfg.theme.accent,
            Some(termos_render::color::Color::rgb(0x88, 0xc0, 0xd0))
        );
        assert_eq!(installed(&p).len(), 3);

        // Removing takes the files and the config's tables away again.
        remove("clock", &p).expect("removed");
        remove("nord", &p).expect("removed");
        assert!(!p.widgets_dir().join("clock.wasm").exists());
        let text = std::fs::read_to_string(&p.config_file).expect("read");
        let cfg = termos_core::config::Config::parse(&text, &p.config_file).expect("valid");
        assert!(cfg.widgets.is_empty() && cfg.theme.name.is_none(), "{text}");
        assert!(matches!(
            remove("clock", &p),
            Err(GalleryError::NotInstalled(_))
        ));
        assert_eq!(installed(&p).len(), 1);

        // A different version in the index is an update.
        let mut newer = signed_index();
        newer.entries[1].version = "1.1.0".into();
        sign(&mut newer, &FIXTURE_SECRET).expect("signed");
        let newer = verify(&newer, &fixture_keys()).expect("verifies");
        let up = updates(&newer, &p);
        assert_eq!(up.len(), 1);
        assert_eq!((up[0].0.name.as_str(), up[0].1.as_str()), ("dev", "1.0.0"));
    }

    #[test]
    fn contents_must_be_what_they_say() {
        assert!(check_content("x", Kind::Theme, b"[theme]\nbase = \"pink\"\n").is_err());
        assert!(check_content("x", Kind::Theme, b"[theme]\nname = \"y\"\n").is_err());
        assert!(check_content("x", Kind::Theme, b"[ui]\nicons = \"ascii\"\n").is_err());
        assert!(check_content("x", Kind::Layout, b"not = [a layout").is_err());
        assert!(check_content("x", Kind::Widget, b"#!/bin/sh\n").is_err());
        assert!(check_content("x", Kind::Widget, WIDGET).is_ok());
        let (secret, public) = keygen().expect("a key");
        assert_eq!(from_hex32(&to_hex(&secret)), Some(secret));
        assert_eq!(key_id(&public).len(), 16);
        assert!(valid_name("nord-dark") && !valid_name("-x") && !valid_name("Nord"));
    }
}
