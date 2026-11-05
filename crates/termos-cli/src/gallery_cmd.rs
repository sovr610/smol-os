// SPDX-License-Identifier: GPL-3.0-or-later
//! `termos gallery` and `termos settings` (M13g, D-126): themes, layouts
//! and widgets from the signed gallery index; settings exported to an
//! archive and imported on another machine.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, bail};
use clap::Subcommand;
use termos_apps::gallery::{self, Entry, Kind, Places};
use termos_apps::settings_io::{self, Change, Sources};
use termos_core::config::Config;
use termos_core::paths::Paths;

macro_rules! out {
    ($($arg:tt)*) => {
        writeln!(std::io::stdout(), $($arg)*)?
    };
}

#[derive(Debug, Subcommand)]
pub(crate) enum GalleryCmd {
    /// Everything in the gallery (installed ones marked).
    List {
        /// Only themes, layouts or widgets.
        #[arg(long, value_parser = ["theme", "layout", "widget"])]
        kind: Option<String>,
    },
    /// Entries whose name or summary has all these words.
    Search {
        #[arg(required = true)]
        words: Vec<String>,
    },
    /// One entry: what it is, who made it, its licence and address.
    Show { name: String },
    /// Download, check and install an entry.
    Install {
        name: String,
        /// For a theme: use it too (`[theme] name`).
        #[arg(long = "use")]
        use_it: bool,
    },
    /// Install newer versions of what is installed (or of one entry).
    Update { name: Option<String> },
    /// Remove an installed entry.
    Remove { name: String },
    /// Use an installed theme.
    Use { name: String },
    /// For the gallery's maintainers: a new signing key. The secret half
    /// goes to FILE (keep it offline); the public half is printed.
    #[command(hide = true)]
    Keygen { file: PathBuf },
    /// For the gallery's maintainers: sign every entry of an index with
    /// the key in KEY, in place.
    #[command(hide = true)]
    Sign {
        index: PathBuf,
        #[arg(long)]
        key: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum SettingsCmd {
    /// Write the config, layouts, themes, the gallery's list and the AI
    /// accounts' names to a `.tar.gz` (never keys or histories).
    Export {
        file: PathBuf,
        /// Wait for Enter at the end (the desktop's window).
        #[arg(long)]
        wait: bool,
    },
    /// Show what an export would change here, then apply it, asking
    /// before replacing anything.
    Import {
        /// The archive (asked for when left out).
        file: Option<PathBuf>,
        /// Replace without asking.
        #[arg(long)]
        yes: bool,
        /// Wait for Enter at the end (the desktop's window).
        #[arg(long)]
        wait: bool,
    },
}

fn index_url(config: &Path) -> String {
    Config::load(config)
        .map(|c| c.gallery.index)
        .unwrap_or_else(|_| gallery::INDEX_URL.to_owned())
}

fn ask(question: &str) -> bool {
    print!("{question} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    let _ = std::io::stdin().read_line(&mut answer);
    matches!(answer.trim(), "y" | "Y" | "yes")
}

fn line(e: &Entry, installed: Option<&str>) -> String {
    let mark = match installed {
        Some(v) if v == e.version => "✓".to_owned(),
        Some(v) => format!("↑ (you have {v})"),
        None => String::new(),
    };
    format!(
        "{:<20} {:<7} {:<9} {} {mark}",
        e.name, e.kind, e.version, e.summary
    )
}

/// Downloads and installs one entry.
fn install_one(e: &Entry, places: &Places) -> anyhow::Result<()> {
    let bytes = gallery::fetch(&e.url, gallery::MAX_FILE)?;
    let done = gallery::install(e, &bytes, places)?;
    for f in &done.files {
        writeln!(
            std::io::stdout(),
            "installed {} {} → {}",
            e.kind,
            e.name,
            f.display()
        )?;
    }
    Ok(())
}

pub(crate) fn gallery_command(
    cmd: &GalleryCmd,
    paths: &Paths,
    config: &Path,
) -> anyhow::Result<ExitCode> {
    let places = Places::from_paths(paths, config);
    match cmd {
        GalleryCmd::Keygen { file } => {
            if file.exists() {
                bail!("{} exists: a key is never overwritten", file.display());
            }
            let (secret, public) = gallery::keygen()?;
            write_secret(file, &gallery::to_hex(&secret))?;
            out!("The secret key is in {} (keep it offline).", file.display());
            out!("Its public half, for termos-apps/src/gallery.rs's KEYS:");
            out!(
                "    (\"{}\", \"{}\"),",
                gallery::key_id(&public),
                gallery::to_hex(&public)
            );
            return Ok(ExitCode::SUCCESS);
        }
        GalleryCmd::Sign { index, key } => {
            let secret = std::fs::read_to_string(key)
                .ok()
                .and_then(|k| gallery::from_hex32(&k))
                .with_context(|| format!("{}: not a gallery key", key.display()))?;
            let bytes = std::fs::read(index).with_context(|| index.display().to_string())?;
            let mut file = gallery::parse_index(&bytes)?;
            gallery::sign(&mut file, &secret)?;
            let text = serde_json::to_string_pretty(&file)?;
            std::fs::write(index, format!("{text}\n"))?;
            out!(
                "signed {} entries with key {}",
                file.entries.len(),
                file.key
            );
            return Ok(ExitCode::SUCCESS);
        }
        GalleryCmd::Use { name } => {
            gallery::use_theme(name, &places)?;
            out!("theme {name} in use");
            return Ok(ExitCode::SUCCESS);
        }
        GalleryCmd::Remove { name } => {
            let done = gallery::remove(name, &places)?;
            out!("removed {} {name}", done.kind.name());
            return Ok(ExitCode::SUCCESS);
        }
        _ => {}
    }
    let url = index_url(config);
    let index = gallery::load(&url)?;
    let installed = gallery::installed(&places);
    let have = |e: &Entry| installed.get(&e.name).map(|i| i.version.as_str());
    match cmd {
        GalleryCmd::List { kind } => {
            for e in index
                .entries
                .iter()
                .filter(|e| kind.as_deref().is_none_or(|k| e.kind == k))
            {
                out!("{}", line(e, have(e)));
            }
            if index.skipped > 0 {
                out!(
                    "({} entries are for a newer TermOS: update it to see them)",
                    index.skipped
                );
            }
        }
        GalleryCmd::Search { words } => {
            let q = words.join(" ");
            let found: Vec<&Entry> = index.entries.iter().filter(|e| e.matches(&q)).collect();
            if found.is_empty() {
                out!("nothing in the gallery matches `{q}`");
                return Ok(ExitCode::from(1));
            }
            for e in found {
                out!("{}", line(e, have(e)));
            }
        }
        GalleryCmd::Show { name } => {
            let e = index
                .get(name)
                .ok_or_else(|| gallery::GalleryError::NotFound(name.clone()))?;
            out!("{} ({} {})", e.name, e.kind, e.version);
            out!("  {}", e.summary);
            out!("  by {}, licence {}", e.author, e.license);
            out!("  {}", e.url);
            out!("  sha256 {}", e.sha256);
            match have(e) {
                Some(v) => out!("  installed: {v}"),
                None => out!("  not installed: termos gallery install {}", e.name),
            }
        }
        GalleryCmd::Install { name, use_it } => {
            let e = index
                .get(name)
                .ok_or_else(|| gallery::GalleryError::NotFound(name.clone()))?;
            install_one(e, &places)?;
            match e.kind() {
                Some(Kind::Theme) if *use_it => {
                    gallery::use_theme(name, &places)?;
                    out!("theme {name} in use");
                }
                Some(Kind::Theme) => out!("use it: termos gallery use {name}"),
                Some(Kind::Widget) => {
                    out!("it shows in the status line once the config reloads (prefix r)")
                }
                Some(Kind::Layout) => out!("open it: termos layout open {name}"),
                None => {}
            }
        }
        GalleryCmd::Update { name } => {
            let ups = gallery::updates(&index, &places);
            let ups: Vec<_> = ups
                .into_iter()
                .filter(|(e, _)| name.as_deref().is_none_or(|n| e.name == n))
                .collect();
            if ups.is_empty() {
                out!("everything installed is up to date");
            }
            for (e, old) in ups {
                out!("{} {old} → {}", e.name, e.version);
                install_one(e, &places)?;
            }
        }
        GalleryCmd::Keygen { .. }
        | GalleryCmd::Sign { .. }
        | GalleryCmd::Use { .. }
        | GalleryCmd::Remove { .. } => {}
    }
    Ok(ExitCode::SUCCESS)
}

/// Writes a secret key file only its owner can read.
fn write_secret(path: &Path, text: &str) -> anyhow::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(path)
        .with_context(|| path.display().to_string())?;
    f.write_all(text.as_bytes())?;
    f.write_all(b"\n")?;
    Ok(())
}

/// Lines of `text` holding something shaped like a key or credential
/// (`termos_ai`'s redaction patterns), by number.
pub(crate) fn secret_lines(text: &str) -> Vec<usize> {
    let r = termos_ai::redact::Redactor::new();
    text.lines()
        .enumerate()
        .filter(|(_, l)| r.text(l) != *l)
        .map(|(i, _)| i + 1)
        .collect()
}

pub(crate) fn settings_command(
    cmd: &SettingsCmd,
    paths: &Paths,
    config: &Path,
) -> anyhow::Result<ExitCode> {
    let result = run_settings(cmd, paths, config);
    if let SettingsCmd::Export { wait: true, .. } | SettingsCmd::Import { wait: true, .. } = cmd {
        // The window closes when this ends: show what happened first.
        if let Err(e) = &result {
            out!("termos: {e:#}");
        }
        print!("\nPress Enter to close.");
        std::io::stdout().flush()?;
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
    }
    result
}

fn run_settings(cmd: &SettingsCmd, paths: &Paths, config: &Path) -> anyhow::Result<ExitCode> {
    let src = Sources::from_paths(paths, config);
    match cmd {
        SettingsCmd::Export { file, .. } => {
            let files = settings_io::collect(&src, env!("CARGO_PKG_VERSION"))?;
            // A key pasted into a file must not leave in the archive.
            let mut problems = Vec::new();
            for (name, data) in &files {
                if let Ok(text) = std::str::from_utf8(data) {
                    for n in secret_lines(text) {
                        problems.push(format!("{name}:{n}"));
                    }
                }
            }
            if !problems.is_empty() {
                bail!(
                    "these lines look like keys or credentials, so nothing was exported: {}. \
                     Keep keys in the keyring (`termos secret set`, then `keyring:NAME` in the config)",
                    problems.join(", ")
                );
            }
            settings_io::write_archive(&files, file)?;
            for name in files.keys() {
                out!("  {name}");
            }
            out!("TermOS's settings are in {}", file.display());
        }
        SettingsCmd::Import { file, yes, .. } => {
            let file = match file {
                Some(f) => f.clone(),
                None => {
                    print!("The settings archive to import: ");
                    std::io::stdout().flush()?;
                    let mut answer = String::new();
                    std::io::stdin().read_line(&mut answer)?;
                    let answer = answer.trim();
                    if answer.is_empty() {
                        return Ok(ExitCode::from(1));
                    }
                    PathBuf::from(answer)
                }
            };
            import(&file, &src, paths, config, *yes)?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn import(
    file: &Path,
    src: &Sources,
    paths: &Paths,
    config: &Path,
    yes: bool,
) -> anyhow::Result<()> {
    let files = settings_io::read_archive(file)?;
    if let Some(m) = files.get("MANIFEST.txt") {
        out!(
            "{}",
            String::from_utf8_lossy(m).lines().next().unwrap_or("")
        );
    }
    for (name, data) in &files {
        if name == "config.toml" {
            let mine = std::fs::read_to_string(config).unwrap_or_default();
            let theirs = String::from_utf8_lossy(data);
            let (merged, changed) = settings_io::merge_config(&mine, &theirs)?;
            if changed.is_empty() {
                out!("config.toml: nothing to change");
                continue;
            }
            out!("config.toml would change: {}", changed.join(", "));
            Config::parse(&merged, config)
                .map_err(|e| anyhow::anyhow!("the merged config would not be valid: {e}"))?;
            if yes || ask("Merge them into your config.toml (your comments are kept)?") {
                if let Some(dir) = config.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(config, merged)?;
                out!("config.toml merged");
            }
            continue;
        }
        let Some(dest) = src.destination(name) else {
            continue;
        };
        match settings_io::change(&dest, data) {
            Change::Same => {}
            Change::New => {
                if let Some(dir) = dest.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(&dest, data)?;
                out!("{name}: added");
            }
            Change::Differs => {
                if yes || ask(&format!("{name}: yours differs. Replace it?")) {
                    std::fs::write(&dest, data)?;
                    out!("{name}: replaced");
                } else {
                    out!("{name}: kept yours");
                }
            }
        }
    }
    if let Some(g) = files.get("gallery.json") {
        let list: std::collections::BTreeMap<String, gallery::Installed> =
            serde_json::from_slice(g).unwrap_or_default();
        if !list.is_empty() {
            let names: Vec<&str> = list.keys().map(String::as_str).collect();
            out!("From the gallery: {}", names.join(", "));
            if yes || ask("Install them again from the gallery?") {
                let places = Places::from_paths(paths, config);
                match gallery::load(&index_url(config)) {
                    Ok(index) => {
                        for n in names {
                            match index.get(n) {
                                Some(e) => install_one(e, &places)?,
                                None => out!("{n}: no longer in the gallery"),
                            }
                        }
                    }
                    Err(e) => out!("the gallery is not reachable ({e}); install them later"),
                }
            }
        }
    }
    if let Some(a) = files.get("accounts.txt") {
        out!("AI accounts on the other machine (sign in to each again here):");
        for l in String::from_utf8_lossy(a).lines() {
            match l.split_once(' ') {
                Some((cli, "default")) => out!("  {cli}: sign in as usual (`{cli}`)"),
                Some(_) => out!("  {l}   (termos account add {l})"),
                None => {}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_in_settings_are_found() {
        let text = "[agents]\nkey = \"keyring:openai\"\napi-key = \"sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\"\n";
        assert_eq!(secret_lines(text), [3]);
        assert!(secret_lines("[theme]\nbase = \"dark\"\n").is_empty());
    }
}
