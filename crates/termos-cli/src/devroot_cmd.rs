// SPDX-License-Identifier: GPL-3.0-or-later
//! `termos devroot`: the dev rootfs, Debian 13 where compilers and system
//! packages are installed with apt, without the host's root (M5b, D-110;
//! `termos_devkit::devroot`).

use std::ffi::OsStr;
use std::io::{IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use anyhow::{Context as _, anyhow, bail};
use clap::Subcommand;
use termos_core::paths::Paths;
use termos_devkit::cli_agents::{Curl, Fetch as _};
use termos_devkit::devroot::{self, Backend, DEBIAN, DevrootError, Ids, State, Tools};

#[derive(Debug, Subcommand)]
pub(crate) enum DevrootCmd {
    /// Download Debian 13 and set it up: unpacked in TermOS's data folder
    /// and entered with bubblewrap (Linux), or Podman's container.
    Create {
        /// Use Podman instead of bubblewrap (the only way off Linux).
        #[arg(long)]
        podman: bool,
        /// A rootfs `.tar.gz` already here: the pinned image's layer, or
        /// another one whose SHA-256 is given with `--sha256`.
        #[arg(long, value_name = "FILE")]
        from: Option<PathBuf>,
        /// The SHA-256 of `--from` when it is not the pinned layer.
        #[arg(long, value_name = "HEX", requires = "from")]
        sha256: Option<String>,
        /// Wait for Enter when it ends (TermOS's windows).
        #[arg(long)]
        wait: bool,
    },
    /// A shell inside, or a command: `termos devroot enter -- make`.
    #[command(alias = "shell")]
    Enter {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Upgrade its packages (apt).
    Update {
        /// Wait for Enter when it ends (TermOS's windows).
        #[arg(long)]
        wait: bool,
    },
    /// What there is, and what this machine can run.
    Status,
    /// Stop Podman's container (bubblewrap leaves nothing running).
    Stop,
    /// Delete it and everything installed in it.
    #[command(alias = "rm")]
    Remove {
        /// Without asking.
        #[arg(long)]
        yes: bool,
    },
    /// Unpacks a layer, inside the user namespace.
    #[command(hide = true)]
    Unpack { archive: PathBuf, dest: PathBuf },
    /// Deletes a rootfs, inside the user namespace.
    #[command(hide = true)]
    Wipe { path: PathBuf },
}

/// `termos devroot …`.
pub(crate) fn devroot_command(action: &DevrootCmd, paths: &Paths) -> anyhow::Result<ExitCode> {
    let data = &paths.data_dir;
    match action {
        DevrootCmd::Create {
            podman,
            from,
            sha256,
            wait,
        } => crate::agents::pause(
            *wait,
            create(data, *podman, from.as_deref(), sha256.as_deref()),
        ),
        DevrootCmd::Enter { command } => enter(data, command),
        DevrootCmd::Update { wait } => crate::agents::pause(*wait, update(data)),
        DevrootCmd::Status => status(data),
        DevrootCmd::Stop => stop(data),
        DevrootCmd::Remove { yes } => remove(data, *yes),
        DevrootCmd::Unpack { archive, dest } => {
            devroot::unpack(archive, dest)
                .with_context(|| format!("cannot unpack {}", archive.display()))?;
            Ok(ExitCode::SUCCESS)
        }
        DevrootCmd::Wipe { path } => {
            devroot::wipe(path).with_context(|| format!("cannot delete {}", path.display()))?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn home() -> anyhow::Result<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("no home folder (HOME is not set)"))
}

/// The bubblewrap way's programs and the IDs it maps.
fn bubblewrap() -> Result<(Tools, Ids), DevrootError> {
    let tools = devroot::tools(std::env::var_os("PATH").as_ref())?;
    Ok((tools, devroot::ids()?))
}

/// `program ARGS` as root of the user namespace.
fn in_namespace(tools: &Tools, ids: &Ids, program: &Path, args: &[&OsStr]) -> Command {
    let mut cmd = Command::new(&tools.unshare);
    cmd.args(devroot::unshare_args(ids)).arg(program).args(args);
    cmd
}

/// Makes a user namespace once, to say why it fails before downloading.
fn try_namespace(tools: &Tools, ids: &Ids) -> anyhow::Result<()> {
    let out = in_namespace(tools, ids, Path::new("true"), &[])
        .stdin(Stdio::null())
        .output()
        .context("cannot run unshare")?;
    if out.status.success() {
        return Ok(());
    }
    let why = String::from_utf8_lossy(&out.stderr).trim().to_owned();
    let hint = devroot::namespace_hint(&|p| std::fs::read_to_string(p).ok())
        .map(|h| format!("\n{h}"))
        .unwrap_or_default();
    bail!("cannot make a user namespace with your subordinate IDs: {why}{hint}")
}

/// Runs `program ARGS` in the namespace and says what failed.
fn namespace_step(tools: &Tools, ids: &Ids, args: &[&OsStr], what: &str) -> anyhow::Result<()> {
    let exe = std::env::current_exe().context("cannot find the termos program")?;
    let status = in_namespace(tools, ids, &exe, args)
        .stdin(Stdio::null())
        .status()
        .context("cannot run unshare")?;
    if !status.success() {
        bail!("{what} failed ({status})");
    }
    Ok(())
}

/// Deletes `path` (a rootfs) as root of the namespace, which owns it.
fn wipe(tools: &Tools, ids: &Ids, path: &Path) -> anyhow::Result<()> {
    if path.symlink_metadata().is_err() {
        return Ok(());
    }
    namespace_step(
        tools,
        ids,
        &[OsStr::new("devroot"), OsStr::new("wipe"), path.as_os_str()],
        "deleting the rootfs",
    )
}

/// `/bin/sh -c SCRIPT`.
fn sh(script: &str) -> Vec<String> {
    vec!["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()]
}

fn podman() -> anyhow::Result<PathBuf> {
    termos_devkit::mise::which("podman", std::env::var_os("PATH").as_ref())
        .ok_or_else(|| anyhow!("Podman is not installed (https://podman.io)"))
}

/// Runs `cmd`, which says what it does on the terminal.
fn checked(mut cmd: Command, what: &str) -> anyhow::Result<()> {
    let status = cmd
        .status()
        .with_context(|| format!("cannot start {what}"))?;
    if !status.success() {
        bail!("{what} failed ({status})");
    }
    Ok(())
}

/// Starts Podman's container when it is not running.
fn start_container(podman: &Path) -> anyhow::Result<()> {
    let mut cmd = Command::new(podman);
    cmd.args(["start", devroot::CONTAINER])
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    checked(cmd, "podman start")
}

/// The command that runs `command` (a shell when empty) inside.
pub(crate) fn inside(data: &Path, state: &State, command: &[String]) -> anyhow::Result<Command> {
    let home = home()?;
    let cwd = std::env::current_dir().unwrap_or_else(|_| home.clone());
    let env = devroot::inside_env(&|k| std::env::var(k).ok());
    match state.backend {
        Backend::Bubblewrap => {
            let (tools, ids) = bubblewrap()?;
            let rootfs = devroot::rootfs(data);
            if !rootfs.is_dir() {
                bail!(
                    "the dev rootfs's folder is missing ({}); `termos devroot remove`, then `termos devroot create`",
                    rootfs.display()
                );
            }
            let command = if command.is_empty() {
                devroot::shell(Some(&rootfs))
            } else {
                command.to_vec()
            };
            let args = devroot::bwrap_args(&devroot::Inside {
                rootfs: &rootfs,
                home: &home,
                cwd: &cwd,
                env: &env,
                command: &command,
            });
            let mut cmd = Command::new(&tools.unshare);
            cmd.args(devroot::unshare_args(&ids))
                .arg(&tools.bwrap)
                .args(args);
            Ok(cmd)
        }
        Backend::Podman => {
            let podman = podman()?;
            start_container(&podman)?;
            let command = if command.is_empty() {
                devroot::shell(None)
            } else {
                command.to_vec()
            };
            // Only the home is bound into the container.
            let dir = devroot::podman_path(if cwd.starts_with(&home) { &cwd } else { &home });
            let tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
            let mut cmd = Command::new(&podman);
            cmd.args(devroot::podman_exec_args(tty, &dir, &env, &command));
            Ok(cmd)
        }
    }
}

fn create(
    data: &Path,
    podman: bool,
    from: Option<&Path>,
    sha256: Option<&str>,
) -> anyhow::Result<ExitCode> {
    if let Some(s) = devroot::load(data) {
        return Err(DevrootError::Exists(format!("{}, {}", s.image, s.backend.name())).into());
    }
    if podman || !cfg!(target_os = "linux") {
        if from.is_some() {
            bail!("`--from` is for the bubblewrap way: Podman pulls the image itself");
        }
        return create_podman(data);
    }
    create_bubblewrap(data, from, sha256)
}

fn create_bubblewrap(
    data: &Path,
    from: Option<&Path>,
    sha256: Option<&str>,
) -> anyhow::Result<ExitCode> {
    let (tools, ids) = bubblewrap()?;
    try_namespace(&tools, &ids)?;
    let dir = devroot::dir(data);
    let rootfs = devroot::rootfs(data);
    let partial = dir.join("rootfs.partial");
    if rootfs.exists() {
        return Err(DevrootError::Exists(rootfs.display().to_string()).into());
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let pinned = || {
        devroot::layer(std::env::consts::ARCH).ok_or_else(|| {
            DevrootError::Unsupported(format!(
                "Debian's image has no build for {}",
                std::env::consts::ARCH
            ))
        })
    };
    let custom = sha256.map(str::to_ascii_lowercase);
    if let Some(sum) = &custom
        && (sum.len() != 64 || !sum.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        bail!("`--sha256` takes 64 hexadecimal digits");
    }
    let (archive, expected, image, downloaded) = match (from, custom) {
        (Some(file), Some(sum)) => (file.to_owned(), sum, file.display().to_string(), false),
        (Some(file), None) => {
            let layer = pinned()?;
            check_size(file, layer.size)?;
            (
                file.to_owned(),
                layer.sha256.to_owned(),
                DEBIAN.name.to_owned(),
                false,
            )
        }
        (None, _) => {
            let layer = pinned()?;
            let part = dir.join("rootfs.tar.gz.part");
            println!(
                "Downloading {} for {} ({} MB)…",
                DEBIAN.name,
                layer.arch,
                layer.size / 1_000_000
            );
            Curl.file(&layer.url(), &part)
                .map_err(|e| anyhow!("cannot download the image: {e}"))?;
            if let Err(e) = check_size(&part, layer.size) {
                let _ = std::fs::remove_file(&part);
                return Err(e);
            }
            (part, layer.sha256.to_owned(), DEBIAN.name.to_owned(), true)
        }
    };
    let got = termos_devkit::mise::sha256_file(&archive)
        .with_context(|| format!("cannot read {}", archive.display()))?;
    if got != expected {
        if downloaded {
            let _ = std::fs::remove_file(&archive);
        }
        return Err(DevrootError::Checksum { expected, got }.into());
    }
    println!("Unpacking (SHA-256 checked)…");
    wipe(&tools, &ids, &partial)?;
    let unpacked = namespace_step(
        &tools,
        &ids,
        &[
            OsStr::new("devroot"),
            OsStr::new("unpack"),
            archive.as_os_str(),
            partial.as_os_str(),
        ],
        "unpacking",
    );
    if let Err(e) = unpacked {
        let _ = wipe(&tools, &ids, &partial);
        return Err(e);
    }
    std::fs::rename(&partial, &rootfs)
        .with_context(|| format!("cannot move the rootfs to {}", rootfs.display()))?;
    if downloaded {
        let _ = std::fs::remove_file(&archive);
    }
    let state = State {
        backend: Backend::Bubblewrap,
        image,
        source: expected,
        created: devroot::now(),
        updated: None,
    };
    devroot::save(data, &state).context("cannot record the dev rootfs")?;
    fetch_lists(data, &state);
    Ok(ExitCode::SUCCESS)
}

fn check_size(file: &Path, expected: u64) -> anyhow::Result<()> {
    let got = std::fs::metadata(file)
        .with_context(|| format!("cannot read {}", file.display()))?
        .len();
    if got != expected {
        return Err(DevrootError::Size { expected, got }.into());
    }
    Ok(())
}

/// apt's package lists, so packages can be searched at once.
fn fetch_lists(data: &Path, state: &State) {
    println!("Fetching the package lists…");
    let ok = inside(data, state, &sh(devroot::LISTS))
        .and_then(|mut c| c.status().context("cannot start apt"))
        .is_ok_and(|s| s.success());
    if !ok {
        eprintln!(
            "termos: apt could not fetch the package lists; `termos devroot update` tries again"
        );
    }
    println!(
        "Ready: `termos devroot enter` opens a shell inside ({}), where apt installs packages.",
        state.backend.name()
    );
}

fn create_podman(data: &Path) -> anyhow::Result<ExitCode> {
    let podman = podman()?;
    let exists = Command::new(&podman)
        .args(["container", "exists", devroot::CONTAINER])
        .stdin(Stdio::null())
        .status()
        .context("cannot run podman")?;
    if exists.success() {
        return Err(
            DevrootError::Exists(format!("Podman's container {}", devroot::CONTAINER)).into(),
        );
    }
    let image = devroot::podman_image();
    println!("Pulling {} ({image})…", DEBIAN.name);
    let mut pull = Command::new(&podman);
    pull.args(["pull", &image]);
    checked(pull, "podman pull")?;
    let mut make = Command::new(&podman);
    make.args(devroot::podman_create_args(&home()?))
        .stdout(Stdio::null());
    checked(make, "podman create")?;
    let state = State {
        backend: Backend::Podman,
        image: DEBIAN.name.to_owned(),
        source: image,
        created: devroot::now(),
        updated: None,
    };
    devroot::save(data, &state).context("cannot record the dev rootfs")?;
    fetch_lists(data, &state);
    Ok(ExitCode::SUCCESS)
}

fn enter(data: &Path, command: &[String]) -> anyhow::Result<ExitCode> {
    let state = devroot::load(data).ok_or(DevrootError::NotCreated)?;
    let cmd = inside(data, &state, command)?;
    crate::run_as(cmd, "the dev rootfs")
}

pub(crate) fn update(data: &Path) -> anyhow::Result<ExitCode> {
    let mut state = devroot::load(data).ok_or(DevrootError::NotCreated)?;
    println!("Upgrading the dev rootfs's packages (apt)…");
    let status = inside(data, &state, &sh(devroot::UPDATE))?
        .status()
        .context("cannot start apt")?;
    if !status.success() {
        bail!("apt failed ({status})");
    }
    state.updated = Some(devroot::now());
    devroot::save(data, &state).context("cannot record the update")?;
    println!("Up to date.");
    Ok(ExitCode::SUCCESS)
}

fn date(secs: u64) -> String {
    i64::try_from(secs)
        .ok()
        .and_then(|s| jiff::Timestamp::from_second(s).ok())
        .map_or_else(
            || secs.to_string(),
            |t| t.strftime("%Y-%m-%d %H:%M UTC").to_string(),
        )
}

fn status(data: &Path) -> anyhow::Result<ExitCode> {
    let Some(state) = devroot::load(data) else {
        let size = devroot::layer(std::env::consts::ARCH)
            .map(|l| format!(", {} MB to download", l.size / 1_000_000))
            .unwrap_or_default();
        println!(
            "No dev rootfs. `termos devroot create` makes one: {}{size}.",
            DEBIAN.name
        );
        if cfg!(target_os = "linux") {
            match bubblewrap() {
                Ok((tools, ids)) => match try_namespace(&tools, &ids) {
                    Ok(()) => println!("  bubblewrap: ready"),
                    Err(e) => println!("  bubblewrap: not ready: {e:#}"),
                },
                Err(e) => println!("  bubblewrap: not ready: {e}"),
            }
        }
        match podman() {
            Ok(p) => println!("  Podman: {} (`--podman`)", p.display()),
            Err(_) => println!("  Podman: not installed"),
        }
        return Ok(ExitCode::SUCCESS);
    };
    println!("Dev rootfs: {} ({})", state.image, state.backend.name());
    match state.backend {
        Backend::Bubblewrap => {
            let rootfs = devroot::rootfs(data);
            println!("  folder:   {}", rootfs.display());
            let installed = termos_devkit::packages::dpkg_installed(
                &std::fs::read_to_string(rootfs.join("var/lib/dpkg/status")).unwrap_or_default(),
            )
            .len();
            println!("  packages: {installed} installed");
        }
        Backend::Podman => {
            let out = podman().ok().and_then(|p| {
                Command::new(p)
                    .args([
                        "container",
                        "inspect",
                        "--format",
                        "{{.State.Status}}",
                        devroot::CONTAINER,
                    ])
                    .stdin(Stdio::null())
                    .stderr(Stdio::null())
                    .output()
                    .ok()
            });
            let running = out
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
            println!(
                "  container: {} ({})",
                devroot::CONTAINER,
                running.as_deref().unwrap_or("missing")
            );
        }
    }
    println!("  source:   {}", state.source);
    println!("  made:     {}", date(state.created));
    println!(
        "  updated:  {}",
        state.updated.map_or_else(|| "never".to_owned(), date)
    );
    Ok(ExitCode::SUCCESS)
}

fn stop(data: &Path) -> anyhow::Result<ExitCode> {
    match devroot::load(data).map(|s| s.backend) {
        Some(Backend::Podman) => {
            let mut cmd = Command::new(podman()?);
            cmd.args(["stop", devroot::CONTAINER]).stdout(Stdio::null());
            checked(cmd, "podman stop")?;
            println!("Stopped.");
        }
        Some(Backend::Bubblewrap) => {
            println!("Nothing runs in the background: the dev rootfs stops with its last shell.");
        }
        None => println!("There is no dev rootfs."),
    }
    Ok(ExitCode::SUCCESS)
}

fn remove(data: &Path, yes: bool) -> anyhow::Result<ExitCode> {
    let state = devroot::load(data);
    let dir = devroot::dir(data);
    let rootfs = devroot::rootfs(data);
    let partial = dir.join("rootfs.partial");
    let leftovers = rootfs.exists() || partial.exists();
    if state.is_none() && !leftovers {
        println!("There is no dev rootfs.");
        return Ok(ExitCode::SUCCESS);
    }
    if !yes {
        if !std::io::stdin().is_terminal() {
            bail!("`termos devroot remove --yes` deletes it without asking");
        }
        print!("Delete the dev rootfs and everything installed in it? [y/N] ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        if !matches!(line.trim(), "y" | "Y" | "yes") {
            println!("Kept.");
            return Ok(ExitCode::SUCCESS);
        }
    }
    if state.as_ref().is_some_and(|s| s.backend == Backend::Podman) {
        let mut cmd = Command::new(podman()?);
        cmd.args(["rm", "--force", devroot::CONTAINER])
            .stdout(Stdio::null());
        checked(cmd, "podman rm")?;
        println!(
            "Podman keeps the image; `podman rmi {}` deletes it.",
            devroot::podman_image()
        );
    }
    if leftovers {
        let (tools, ids) = bubblewrap()?;
        wipe(&tools, &ids, &partial)?;
        wipe(&tools, &ids, &rootfs)?;
    }
    let _ = std::fs::remove_file(dir.join("rootfs.tar.gz.part"));
    devroot::forget(data).context("cannot forget the dev rootfs")?;
    let _ = std::fs::remove_dir(&dir);
    println!("Deleted.");
    Ok(ExitCode::SUCCESS)
}
