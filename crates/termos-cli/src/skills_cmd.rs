// SPDX-License-Identifier: GPL-3.0-or-later
//! `termos skills`: TermOS's skills for Claude Code, Codex and the other
//! coding CLIs (M11b, D-098), and keeping them installed.

use std::path::Path;
use std::process::ExitCode;

use clap::Subcommand;
use termos_core::config::Config;
use termos_core::paths::Paths;
use termos_devkit::cli_agents::Tool;
use termos_devkit::skills::{self, Folder, Outcome, SKILLS, State};

use crate::coding_agents::{accounts, env, home, program_of};

#[derive(Debug, Subcommand)]
pub(crate) enum SkillsCmd {
    /// Install or update them in every folder (and keep them so).
    Install {
        /// Also replace copies you changed (a skill of your own with the
        /// same name is still left alone).
        #[arg(long)]
        force: bool,
    },
    /// Remove TermOS's copies, and stop installing them by themselves
    /// (until `termos skills install`).
    #[command(alias = "rm")]
    Remove {
        /// Also remove copies you changed.
        #[arg(long)]
        force: bool,
    },
    /// Each folder, who reads it, and each skill's state there.
    Status,
    /// Print a skill (`termos`, `termos-dev`, `termos-agents`).
    Show { name: String },
}

/// The other CLIs that read `~/.agents/skills` (Gemini CLI's accounts link
/// it into their homes, D-112).
const SHARED_CLIS: &[&str] = &["gemini", "opencode"];

/// Whether a CLI that reads each kind of folder is here: Claude Code's,
/// and the shared one.
fn present(paths: &Paths) -> (bool, bool) {
    let claude = program_of(paths, Tool::Claude).is_some();
    let path = std::env::var_os("PATH");
    let shared = program_of(paths, Tool::Codex).is_some()
        || program_of(paths, Tool::Gemini).is_some()
        || SHARED_CLIS
            .iter()
            .any(|n| termos_devkit::mise::which(n, path.as_ref()).is_some());
    (claude, shared)
}

fn all_folders(paths: &Paths, claude: bool, shared: bool) -> anyhow::Result<Vec<Folder>> {
    let all = accounts(paths)?;
    Ok(skills::folders(
        &all,
        &env,
        home().as_deref(),
        claude,
        shared,
    ))
}

/// Brings TermOS's skills up to date where a CLI will read them, unless
/// the user turned that off. Quiet: for the server's start, installs and
/// new accounts. Returns what it did, one line each.
pub(crate) fn ensure(paths: &Paths, config: &Config) -> Vec<String> {
    if !config.agents.skills.install || skills::off_file(&paths.data_dir).exists() {
        return Vec::new();
    }
    let (claude, shared) = present(paths);
    if !claude && !shared {
        return Vec::new();
    }
    let folders = match all_folders(paths, claude, shared) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!("skills: {e:#}");
            return Vec::new();
        }
    };
    let mut done = Vec::new();
    for f in &folders {
        for s in SKILLS {
            match skills::install(&f.dir, s, false) {
                Ok(Outcome::Installed) => done.push(format!(
                    "installed the {} skill for {}",
                    s.name,
                    f.readers.join(", ")
                )),
                Ok(Outcome::Updated) => done.push(format!(
                    "updated the {} skill for {}",
                    s.name,
                    f.readers.join(", ")
                )),
                Ok(_) => {}
                Err(e) => tracing::warn!("skills: {} in {}: {e}", s.name, f.dir.display()),
            }
        }
    }
    for line in &done {
        tracing::info!("skills: {line}");
    }
    done
}

fn shown(dir: &Path) -> String {
    match home() {
        Some(h) => match dir.strip_prefix(&h) {
            Ok(rest) => format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display()),
            Err(_) => dir.display().to_string(),
        },
        None => dir.display().to_string(),
    }
}

pub(crate) fn skills_command(
    action: &SkillsCmd,
    paths: &Paths,
    config_path: &Path,
) -> anyhow::Result<ExitCode> {
    match action {
        SkillsCmd::Install { force } => {
            let _ = std::fs::remove_file(skills::off_file(&paths.data_dir));
            let mut kept = false;
            for f in all_folders(paths, true, true)? {
                for s in SKILLS {
                    let what = match skills::install(&f.dir, s, *force)? {
                        Outcome::Installed => "installed",
                        Outcome::Updated => "updated",
                        Outcome::Unchanged => "up to date",
                        Outcome::Removed => "removed",
                        Outcome::Kept(st) => {
                            kept = true;
                            st.describe()
                        }
                    };
                    println!("{:<14} {what}  ({})", s.name, shown(&f.dir));
                }
            }
            if kept {
                println!();
                println!(
                    "TermOS left the skills marked kept as they are. `termos skills install --force` replaces the ones you changed; it never replaces a skill of your own."
                );
            }
            let config = Config::load(config_path).unwrap_or_default();
            if !config.agents.skills.install {
                println!();
                println!("[agents.skills] install = false: TermOS will not update them by itself.");
            }
        }
        SkillsCmd::Remove { force } => {
            for f in all_folders(paths, true, true)? {
                for s in SKILLS {
                    match skills::remove(&f.dir, s, *force)? {
                        Outcome::Removed => {
                            println!("{:<14} removed  ({})", s.name, shown(&f.dir));
                        }
                        Outcome::Kept(st) => {
                            println!("{:<14} {}  ({})", s.name, st.describe(), shown(&f.dir));
                        }
                        _ => {}
                    }
                }
            }
            std::fs::create_dir_all(&paths.data_dir)?;
            std::fs::write(skills::off_file(&paths.data_dir), b"")?;
            println!("TermOS will not install its skills again until `termos skills install`.");
        }
        SkillsCmd::Status => {
            let config = Config::load(config_path).unwrap_or_default();
            let auto = config.agents.skills.install && !skills::off_file(&paths.data_dir).exists();
            let (claude, shared) = present(paths);
            println!(
                "Installed and updated by TermOS: {}",
                if auto {
                    "yes"
                } else if config.agents.skills.install {
                    "no (`termos skills remove` turned it off; `termos skills install` turns it on)"
                } else {
                    "no ([agents.skills] install = false)"
                }
            );
            for f in all_folders(paths, true, true)? {
                let found = if f.tool.is_some() { claude } else { shared };
                println!();
                println!(
                    "{}  ({}{})",
                    shown(&f.dir),
                    f.readers.join(", "),
                    if found { "" } else { "; none found here" }
                );
                for s in SKILLS {
                    let st = skills::state(&f.dir, s);
                    let hint = match st {
                        State::Missing | State::Outdated => "  (`termos skills install`)",
                        State::Edited => "  (`termos skills install --force` replaces it)",
                        _ => "",
                    };
                    println!("  {:<14} {}{hint}", s.name, st.describe());
                }
            }
        }
        SkillsCmd::Show { name } => {
            let Some(s) = skills::skill(name) else {
                let names: Vec<&str> = SKILLS.iter().map(|s| s.name).collect();
                anyhow::bail!("no skill named {name}: {}", names.join(", "));
            };
            print!("{}", s.text());
        }
    }
    Ok(ExitCode::SUCCESS)
}
