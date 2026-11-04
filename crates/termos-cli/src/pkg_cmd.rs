// SPDX-License-Identifier: GPL-3.0-or-later
//! `termos pkg`: packages from mise, the dev rootfs, the host's package
//! manager and Podman (M5b, D-110; `termos_devkit::packages`). The
//! package manager app lists and searches through the same code, and runs
//! installs, removals and upgrades in windows with `termos pkg … --wait`.

use std::process::{Command, ExitCode};

use anyhow::{Context as _, anyhow, bail};
use clap::Subcommand;
use termos_core::paths::Paths;
use termos_devkit::devroot::{self, DevrootError};
use termos_devkit::packages::{self, Backend as _, Package, Source, System};

#[derive(Debug, Subcommand)]
pub(crate) enum PkgCmd {
    /// The sources here (mise, devroot, host, podman) and whether each
    /// can be used.
    Sources,
    /// What is installed: every source, or one.
    #[command(alias = "ls")]
    List { source: Option<String> },
    /// Search a source.
    Search { source: String, term: String },
    /// Install a package: `termos pkg install devroot gcc`.
    Install {
        source: String,
        name: String,
        /// Wait for Enter when it ends (TermOS's windows).
        #[arg(long)]
        wait: bool,
    },
    /// Remove a package (a Podman image, or a container with
    /// `--container`).
    #[command(alias = "rm")]
    Remove {
        source: String,
        name: String,
        #[arg(long)]
        container: bool,
        /// Wait for Enter when it ends (TermOS's windows).
        #[arg(long)]
        wait: bool,
    },
    /// Upgrade everything a source installed.
    Upgrade {
        source: String,
        /// Wait for Enter when it ends (TermOS's windows).
        #[arg(long)]
        wait: bool,
    },
    /// Set a source up: `termos pkg setup mise` fetches mise (pinned,
    /// checked); `termos pkg setup devroot` makes the dev rootfs.
    Setup {
        source: String,
        /// Wait for Enter when it ends (TermOS's windows).
        #[arg(long)]
        wait: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    Install,
    Remove,
    Upgrade,
}

fn source(id: &str) -> anyhow::Result<Source> {
    Source::from_id(id)
        .ok_or_else(|| anyhow!("unknown source {id:?}: mise, devroot, host or podman"))
}

fn checked_name(name: &str) -> anyhow::Result<&str> {
    if !packages::valid_name(name) {
        bail!("{name:?} is not a package name");
    }
    Ok(name)
}

fn system(paths: &Paths) -> anyhow::Result<System> {
    Ok(System {
        data_dir: paths.data_dir.clone(),
        termos: std::env::current_exe().context("cannot find the termos program")?,
    })
}

/// `termos pkg …`.
pub(crate) fn pkg_command(action: &PkgCmd, paths: &Paths) -> anyhow::Result<ExitCode> {
    let sys = system(paths)?;
    match action {
        PkgCmd::Sources => {
            for info in sys.sources() {
                let state = info
                    .unavailable
                    .as_ref()
                    .map_or_else(|| "ready".to_owned(), |u| u.reason.clone());
                println!("{:<8} {:<18} {state}", info.source.id(), info.title);
            }
            Ok(ExitCode::SUCCESS)
        }
        PkgCmd::List { source: one } => {
            let wanted = one.as_deref().map(source).transpose()?;
            let mut failed = false;
            for info in sys.sources() {
                if wanted.is_some_and(|w| w != info.source) {
                    continue;
                }
                if let Some(u) = &info.unavailable {
                    if wanted.is_some() {
                        bail!("{}: {}", info.title, u.reason);
                    }
                    continue;
                }
                match sys.installed(info.source) {
                    Ok(list) => print_list(&info.title, &list),
                    Err(e) => {
                        eprintln!("termos: {}: {e}", info.title);
                        failed = true;
                    }
                }
            }
            Ok(if failed {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            })
        }
        PkgCmd::Search { source: s, term } => {
            let s = source(s)?;
            let term = checked_name(term)?;
            let found = sys.search(s, term).map_err(|e| anyhow!(e))?;
            print_list(&format!("{} for {term:?}", s.id()), &found);
            Ok(ExitCode::SUCCESS)
        }
        PkgCmd::Install {
            source: s,
            name,
            wait,
        } => crate::agents::pause(
            *wait,
            act(paths, &sys, source(s)?, Verb::Install, Some(name), false),
        ),
        PkgCmd::Remove {
            source: s,
            name,
            container,
            wait,
        } => crate::agents::pause(
            *wait,
            act(
                paths,
                &sys,
                source(s)?,
                Verb::Remove,
                Some(name),
                *container,
            ),
        ),
        PkgCmd::Upgrade { source: s, wait } => crate::agents::pause(
            *wait,
            act(paths, &sys, source(s)?, Verb::Upgrade, None, false),
        ),
        PkgCmd::Setup { source: s, wait } => crate::agents::pause(*wait, setup(paths, source(s)?)),
    }
}

fn print_list(title: &str, list: &[Package]) {
    println!("{title}: {}", list.len());
    let name_w = list.iter().map(|p| p.name.len()).max().unwrap_or(0).min(40);
    let ver_w = list
        .iter()
        .map(|p| p.version.len())
        .max()
        .unwrap_or(0)
        .min(30);
    for p in list {
        let line = format!(
            "  {:<name_w$}  {:<ver_w$}  {}",
            p.name, p.version, p.summary
        );
        println!("{}", line.trim_end());
    }
}

fn setup(paths: &Paths, s: Source) -> anyhow::Result<ExitCode> {
    match s {
        Source::Mise => {
            let mise = termos_devkit::mise::ensure(&paths.data_dir, &mut |m| println!("{m}"))?;
            println!("mise {}: {}", mise.version, mise.path.display());
            Ok(ExitCode::SUCCESS)
        }
        Source::Devroot => crate::devroot_cmd::devroot_command(
            &crate::devroot_cmd::DevrootCmd::Create {
                podman: false,
                from: None,
                sha256: None,
                wait: false,
            },
            paths,
        ),
        Source::Host | Source::Podman => {
            bail!("TermOS does not set up the {} source", s.id())
        }
    }
}

fn is_root() -> bool {
    #[cfg(unix)]
    {
        termos_core::paths::current_uid() == 0
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn on_path(p: &str) -> bool {
    termos_devkit::mise::which(p, std::env::var_os("PATH").as_ref()).is_some()
}

fn argv_command(argv: &[String]) -> anyhow::Result<Command> {
    let (program, args) = argv.split_first().ok_or_else(|| anyhow!("no command"))?;
    let mut cmd = Command::new(program);
    cmd.args(args);
    Ok(cmd)
}

/// Installs, removes or upgrades, in this terminal.
fn act(
    paths: &Paths,
    sys: &System,
    s: Source,
    verb: Verb,
    name: Option<&str>,
    container: bool,
) -> anyhow::Result<ExitCode> {
    let name = name.map(checked_name).transpose()?;
    let data = &paths.data_dir;
    // The command, and how it is shown.
    let (cmd, shown): (Command, String) = match s {
        Source::Mise => {
            let mise = termos_devkit::mise::ensure(data, &mut |m| println!("{m}"))?;
            let args: Vec<String> = match (verb, name) {
                (Verb::Install, Some(n)) if n.contains('@') => {
                    vec!["use".into(), "--global".into(), n.into()]
                }
                (Verb::Install, Some(n)) => {
                    vec!["use".into(), "--global".into(), format!("{n}@latest")]
                }
                (Verb::Remove, Some(n)) => vec!["unuse".into(), "--global".into(), n.into()],
                _ => vec!["upgrade".into()],
            };
            let mut cmd = Command::new(&mise.path);
            cmd.args(&args);
            (cmd, format!("mise {}", args.join(" ")))
        }
        Source::Devroot => {
            let state = devroot::load(data).ok_or(DevrootError::NotCreated)?;
            let argv: Vec<String> = match (verb, name) {
                (Verb::Install, Some(n)) => vec!["apt-get".into(), "install".into(), n.into()],
                (Verb::Remove, Some(n)) => vec!["apt-get".into(), "remove".into(), n.into()],
                _ => return crate::devroot_cmd::update(data),
            };
            let shown = format!("{} (in the dev rootfs)", argv.join(" "));
            (crate::devroot_cmd::inside(data, &state, &argv)?, shown)
        }
        Source::Host => {
            let m = sys
                .host()
                .ok_or_else(|| anyhow!("this system has no package manager TermOS knows"))?;
            let elevate = packages::elevate(is_root(), &on_path);
            let argv = match (verb, name) {
                (Verb::Install, Some(n)) => m.install_argv(n, &elevate),
                (Verb::Remove, Some(n)) => m.remove_argv(n, &elevate),
                _ => m.upgrade_argv(&elevate),
            };
            (argv_command(&argv)?, argv.join(" "))
        }
        Source::Podman => {
            let podman = termos_devkit::mise::which("podman", std::env::var_os("PATH").as_ref())
                .ok_or_else(|| anyhow!("Podman is not installed"))?;
            let args: [&str; 2] = match (verb, name) {
                (Verb::Install, Some(n)) => ["pull", n],
                (Verb::Remove, Some(n)) if container => ["rm", n],
                (Verb::Remove, Some(n)) => ["rmi", n],
                _ => bail!(
                    "Podman images update when pulled again: `termos pkg install podman IMAGE`"
                ),
            };
            let mut cmd = Command::new(podman);
            cmd.args(args);
            (cmd, format!("podman {}", args.join(" ")))
        }
    };
    run_shown(cmd, &shown)
}

/// Runs `cmd` after showing it, in this terminal.
fn run_shown(mut cmd: Command, shown: &str) -> anyhow::Result<ExitCode> {
    println!("$ {shown}");
    let status = cmd.status().context("cannot start it")?;
    if !status.success() {
        bail!("it failed ({status})");
    }
    println!("Done.");
    Ok(ExitCode::SUCCESS)
}
