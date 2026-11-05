// SPDX-License-Identifier: GPL-3.0-or-later
//! `termos layout`, `termos snippet` and `termos play` (M13b, D-120).

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context as _, bail};
use clap::Subcommand;
use serde_json::{Value, json};
use termos_core::config::Config;
use termos_core::paths::Paths;

use crate::Target;

#[derive(Debug, Subcommand)]
pub(crate) enum LayoutCmd {
    /// The layouts found from this folder: the project's, then yours.
    List {
        /// Look from this folder instead.
        dir: Option<PathBuf>,
    },
    /// Open a layout's workspaces in the session.
    Open {
        name: String,
        /// The folder its relative paths start from (default: here, or
        /// its project's).
        dir: Option<PathBuf>,
    },
    /// Save the active workspace as `layouts/NAME.toml`.
    Save { name: String },
    /// Edit `layouts/NAME.toml` (started from an example when new).
    Edit { name: String },
}

#[derive(Debug, Subcommand)]
pub(crate) enum SnippetCmd {
    /// The saved commands: the config's, then this folder's project's.
    List,
    /// Type a saved command into this pane (or the focused one),
    /// parameters given as NAME=VALUE.
    Run {
        name: String,
        params: Vec<String>,
        /// Do not press Enter, whatever the snippet says.
        #[arg(long)]
        no_run: bool,
    },
}

/// A new layout file's text.
const EXAMPLE: &str = r#"# A TermOS layout: open it with `termos layout open NAME` or the palette.
# Each pane after the first is split from the one before it (or `of`).
workspace = "dev"
cwd = "."

[[pane]]
command = "nvim ."

[[pane]]
split = "right"     # right | down | float
size = "35%"
command = "git status"
"#;

fn here() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn absolute(dir: &Path) -> PathBuf {
    if dir.is_absolute() {
        dir.to_owned()
    } else {
        here().join(dir)
    }
}

/// `termos layout …`; `open` and `save` act in the session this runs in
/// (or the server's newest).
pub(crate) fn layout_command(
    action: &LayoutCmd,
    paths: &Paths,
    config_path: &Path,
    t: &Target,
) -> anyhow::Result<ExitCode> {
    match action {
        LayoutCmd::List { dir } => {
            let dir = dir.as_deref().map_or_else(here, absolute);
            let names = termos_core::layouts::names(&paths.layouts_dir(), &dir);
            if names.is_empty() {
                println!(
                    "no layouts: save one with `termos layout save NAME`, or see `termos layout edit NAME`"
                );
            }
            for n in names {
                println!("{n}");
            }
            Ok(ExitCode::SUCCESS)
        }
        LayoutCmd::Open { name, dir } => {
            let dir = dir.as_deref().map_or_else(here, absolute);
            // Checked here first, so errors show where it was asked.
            termos_core::layouts::find(name, &paths.layouts_dir(), &dir)?;
            let arg = format!("{name} {}", dir.display());
            crate::call(
                t,
                "layout.open",
                Some(json!({ "arg": arg, "session": t.session })),
            )?;
            Ok(ExitCode::SUCCESS)
        }
        LayoutCmd::Save { name } => {
            if !termos_core::layouts::valid_name(name) {
                bail!("`{name}` is not a layout name (letters, digits, - _ .)");
            }
            crate::call(
                t,
                "layout.save",
                Some(json!({ "arg": name, "session": t.session })),
            )?;
            println!(
                "{}",
                paths.layouts_dir().join(format!("{name}.toml")).display()
            );
            Ok(ExitCode::SUCCESS)
        }
        LayoutCmd::Edit { name } => {
            if !termos_core::layouts::valid_name(name) {
                bail!("`{name}` is not a layout name (letters, digits, - _ .)");
            }
            let dir = paths.layouts_dir();
            let file = dir.join(format!("{name}.toml"));
            if !file.exists() {
                std::fs::create_dir_all(&dir)
                    .with_context(|| format!("cannot make {}", dir.display()))?;
                std::fs::write(&file, EXAMPLE)
                    .with_context(|| format!("cannot write {}", file.display()))?;
            }
            crate::edit_command(&[file.display().to_string()], paths, config_path)
        }
    }
}

/// `termos snippet …`.
pub(crate) fn snippet_command(
    action: &SnippetCmd,
    config_path: &Path,
    t: &Target,
) -> anyhow::Result<ExitCode> {
    let config = Config::load(config_path)?;
    let mut all = config.snippets;
    for s in termos_core::snippets::project_snippets(&here()) {
        if !all.iter().any(|a| a.name == s.name) {
            all.push(s);
        }
    }
    match action {
        SnippetCmd::List => {
            if all.is_empty() {
                println!("no saved commands: add [[snippets]] to your config or a termos.toml");
            }
            for s in &all {
                println!("{}\t{}", s.name, s.command);
            }
            Ok(ExitCode::SUCCESS)
        }
        SnippetCmd::Run {
            name,
            params,
            no_run,
        } => {
            let s = all.iter().find(|s| s.name == *name).with_context(|| {
                format!("no saved command named {name} (`termos snippet list`)")
            })?;
            let mut values = Vec::new();
            for p in params {
                let (k, v) = p
                    .split_once('=')
                    .with_context(|| format!("`{p}`: parameters are NAME=VALUE"))?;
                values.push((k.to_owned(), v.to_owned()));
            }
            let missing: Vec<String> = termos_core::snippets::params(&s.command)
                .into_iter()
                .filter(|p| p.default.is_none() && !values.iter().any(|(k, _)| *k == p.name))
                .map(|p| p.name)
                .collect();
            if !missing.is_empty() {
                bail!("give {} as NAME=VALUE", missing.join(", "));
            }
            let mut text = termos_core::snippets::fill(&s.command, &values);
            if s.run && !no_run {
                text.push('\r');
            }
            let mut req = json!({ "text": text, "session": t.session });
            if let Some(p) = crate::tos::pane_id() {
                req["pane"] = Value::from(p);
            }
            crate::call(t, "pane.send-text", Some(req))?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// `termos play FILE`: a recording played back to this terminal at its
/// recorded pace (`speed` times faster), pauses cut to `idle_limit`.
pub(crate) fn play_command(
    file: &Path,
    speed: f64,
    idle_limit: Option<f64>,
) -> anyhow::Result<ExitCode> {
    let f = std::fs::File::open(file).with_context(|| format!("cannot open {}", file.display()))?;
    let (header, events) = termos_core::cast::read(std::io::BufReader::new(f))
        .with_context(|| file.display().to_string())?;
    let times = termos_core::cast::schedule(&events, speed, idle_limit);
    let mut out = std::io::stdout().lock();
    let (cols, rows) = crossterm::terminal::size().unwrap_or((header.width, header.height));
    if (cols, rows) != (header.width, header.height) {
        eprintln!(
            "termos: recorded at {}x{}, this terminal is {cols}x{rows}",
            header.width, header.height
        );
    }
    let start = Instant::now();
    for (e, at) in events.iter().zip(times) {
        let wait = at.saturating_sub(start.elapsed());
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
        if let termos_core::cast::Event::Output(_, text) = e {
            out.write_all(text.as_bytes())?;
            out.flush()?;
        }
    }
    Ok(ExitCode::SUCCESS)
}
