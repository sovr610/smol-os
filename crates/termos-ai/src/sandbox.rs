// SPDX-License-Identifier: GPL-3.0-or-later
//! Where agent commands run (ARCHITECTURE.md §11, D-061).
//!
//! On Linux a command runs under **bubblewrap**: new mount, PID, IPC, UTS
//! and network namespaces; the whole system bound read-only; the
//! workspace (and a profile's `write-paths`) bound read-write; a private
//! `/tmp`; keys and credentials (`~/.ssh`, keyrings, the runtime folder
//! with its D-Bus and TermOS sockets…) hidden behind empty folders; and
//! no network unless the call asked for it and the user agreed.
//!
//! Inside it, TermOS's own helper (`termos sandbox-exec`) adds a
//! **Landlock** ruleset before running the command: writes only beneath
//! the writable folders, no TCP without the network, no signals or
//! abstract sockets beyond the sandbox. It also drops environment
//! variables that look like secrets. The helper is the same `termos`
//! binary, so no code runs between `fork` and `exec` (D-061).
//!
//! Without bubblewrap the helper's Landlock layer still confines writes,
//! but reads are not hidden and the network is only partly closed: every
//! command then asks. Without either (macOS, Windows, old kernels),
//! commands run as they are and every one asks, with a warning.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use termos_core::config::agents::SandboxMode;

use crate::policy::Confinement;

/// What this system offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    /// A working bubblewrap.
    pub bwrap: Option<PathBuf>,
    pub landlock: Landlock,
}

/// How much Landlock the kernel enforces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Landlock {
    /// Every restriction TermOS asks for (writes, TCP, scopes).
    Full,
    /// Some (an older kernel: writes, perhaps not the network).
    Partial,
    None,
}

impl Capabilities {
    pub fn none() -> Self {
        Self {
            bwrap: None,
            landlock: Landlock::None,
        }
    }

    /// How confined commands are under `mode`.
    pub fn confinement(&self, mode: SandboxMode) -> Confinement {
        match (mode, &self.bwrap, self.landlock) {
            (SandboxMode::Off, _, _) => Confinement::None,
            (_, Some(_), _) => Confinement::Full,
            (_, None, Landlock::Full | Landlock::Partial) => Confinement::Partial,
            (_, None, Landlock::None) => Confinement::None,
        }
    }

    /// One line for the agent window.
    pub fn describe(&self, mode: SandboxMode) -> String {
        if mode == SandboxMode::Off {
            return "sandbox off (profile): every command asks".into();
        }
        match (&self.bwrap, self.landlock) {
            (Some(_), Landlock::Full) => "sandbox: bubblewrap + Landlock".into(),
            (Some(_), Landlock::Partial) => "sandbox: bubblewrap + partial Landlock (older kernel)".into(),
            (Some(_), Landlock::None) => "sandbox: bubblewrap (no Landlock in this kernel)".into(),
            (None, Landlock::Full | Landlock::Partial) => {
                "sandbox: Landlock only (no bubblewrap): writes confined, reads not hidden; every command asks".into()
            }
            (None, Landlock::None) if cfg!(target_os = "linux") => {
                "no sandbox (install bubblewrap): every command asks".into()
            }
            (None, Landlock::None) => "no sandbox on this system: every command asks".into(),
        }
    }
}

/// Probes once per process: is there a bubblewrap that can make
/// namespaces, and how much Landlock does `helper` (the `termos` binary)
/// manage to enforce?
pub fn probe(helper: Option<&Path>) -> &'static Capabilities {
    static CAPS: OnceLock<Capabilities> = OnceLock::new();
    CAPS.get_or_init(|| probe_now(helper))
}

/// Probes without caching.
pub fn probe_now(helper: Option<&Path>) -> Capabilities {
    // macOS runs agent commands confirm-only by design (ARCHITECTURE.md §11).
    // TODO(M9): a Windows sandbox for agent commands (an AppContainer or a restricted token); until then every command asks.
    if !cfg!(target_os = "linux") {
        return Capabilities::none();
    }
    let bwrap = find_program("bwrap").filter(|bwrap| {
        Command::new(bwrap)
            .args([
                "--ro-bind",
                "/",
                "/",
                "--unshare-all",
                "--dev",
                "/dev",
                "--proc",
                "/proc",
                "--",
                "true",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    });
    let landlock = helper
        .and_then(|h| {
            Command::new(h)
                .args(["sandbox-exec", "--probe"])
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output()
                .ok()
        })
        .map_or(Landlock::None, |out| {
            match String::from_utf8_lossy(&out.stdout).trim() {
                "landlock: full" => Landlock::Full,
                "landlock: partial" => Landlock::Partial,
                _ => Landlock::None,
            }
        });
    tracing::info!("sandbox probe: bwrap {:?}, landlock {landlock:?}", bwrap);
    Capabilities { bwrap, landlock }
}

fn find_program(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// What a sandboxed command may touch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    pub workspace: PathBuf,
    /// Writable besides the workspace.
    pub writable: Vec<PathBuf>,
    /// Made empty inside the sandbox (folders) or `/dev/null` (files).
    pub hidden: Vec<PathBuf>,
    pub network: bool,
    /// Where the command starts.
    pub cwd: PathBuf,
    /// Pass the environment on unchanged (external agents need their own
    /// keys); otherwise secret-looking variables are dropped.
    pub keep_env: bool,
}

/// The sandbox cannot be used as the profile requires.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct SandboxError(pub String);

/// The command line that runs `argv` under `spec`: bubblewrap around
/// the helper around `argv`, or less where less is available.
pub fn wrap(
    spec: &Spec,
    caps: &Capabilities,
    mode: SandboxMode,
    helper: Option<&Path>,
    argv: &[String],
) -> Result<Vec<String>, SandboxError> {
    if argv.is_empty() {
        return Err(SandboxError("nothing to run".into()));
    }
    if mode == SandboxMode::Required && caps.bwrap.is_none() {
        return Err(SandboxError(
            "the profile requires the sandbox, and bubblewrap is not available here".into(),
        ));
    }
    if mode == SandboxMode::Off {
        return Ok(argv.to_vec());
    }
    let mut writable = vec![spec.workspace.clone()];
    writable.extend(spec.writable.iter().cloned());
    let inner: Vec<String> = match (helper, cfg!(target_os = "linux")) {
        (Some(helper), true) => {
            let mut v = vec![helper.display().to_string(), "sandbox-exec".into()];
            for w in &writable {
                v.push("--write".into());
                v.push(w.display().to_string());
            }
            if caps.bwrap.is_some() {
                // bubblewrap gives the command a /tmp of its own.
                v.extend(["--write".into(), "/tmp".into()]);
            } else {
                // The system's /tmp is shared: a private folder instead.
                let tmp =
                    private_tmp().map_err(|e| SandboxError(format!("no temporary folder: {e}")))?;
                v.extend(["--tmp".into(), tmp.display().to_string()]);
            }
            if spec.network {
                v.push("--net".into());
            }
            if spec.keep_env {
                v.push("--keep-env".into());
            }
            v.push("--".into());
            v.extend(argv.iter().cloned());
            v
        }
        _ => argv.to_vec(),
    };
    let Some(bwrap) = &caps.bwrap else {
        return Ok(inner);
    };
    let mut v = vec![
        bwrap.display().to_string(),
        "--die-with-parent".into(),
        "--unshare-all".into(),
    ];
    if spec.network {
        v.push("--share-net".into());
    }
    for a in [
        "--ro-bind",
        "/",
        "/",
        "--dev",
        "/dev",
        "--proc",
        "/proc",
        "--tmpfs",
        "/tmp",
    ] {
        v.push(a.into());
    }
    for h in &spec.hidden {
        // Never hide the workspace itself, or a folder it lives in.
        if writable.iter().any(|w| w.starts_with(h)) {
            continue;
        }
        match std::fs::symlink_metadata(h) {
            Ok(m) if m.is_dir() => v.extend(["--tmpfs".into(), h.display().to_string()]),
            Ok(_) => v.extend([
                "--ro-bind".into(),
                "/dev/null".into(),
                h.display().to_string(),
            ]),
            Err(_) => {}
        }
    }
    for w in &writable {
        if w.exists() {
            v.extend([
                "--bind".into(),
                w.display().to_string(),
                w.display().to_string(),
            ]);
        }
    }
    // Where the command starts must exist inside, even under the private
    // /tmp or a hidden folder: read-only unless it is writable anyway.
    if !writable.iter().any(|w| spec.cwd.starts_with(w)) && spec.cwd.exists() {
        v.extend([
            "--ro-bind".into(),
            spec.cwd.display().to_string(),
            spec.cwd.display().to_string(),
        ]);
    }
    v.extend([
        "--chdir".into(),
        spec.cwd.display().to_string(),
        "--".into(),
    ]);
    v.extend(inner);
    Ok(v)
}

/// A fresh folder, readable only by us, for a sandboxed command's
/// temporary files when the system's `/tmp` is shared.
fn private_tmp() -> std::io::Result<PathBuf> {
    let mut rnd = [0u8; 6];
    getrandom::fill(&mut rnd).map_err(|e| std::io::Error::other(e.to_string()))?;
    let name: String = rnd.iter().map(|b| format!("{b:02x}")).collect();
    let dir = std::env::temp_dir().join(format!("termos-sandbox-{name}"));
    crate::secrets::create_private_dir(&dir)?;
    Ok(dir)
}

/// Environment variables an agent's commands never inherit: anything
/// named like a credential, agent and bus sockets, and TermOS's own.
pub fn is_secret_env(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    const PARTS: &[&str] = &[
        "KEY",
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "CREDENTIAL",
        "_AUTH",
        "AUTH_",
        "COOKIE",
        "SESSION",
    ];
    const NAMES: &[&str] = &[
        "SSH_AUTH_SOCK",
        "SSH_AGENT_PID",
        "GPG_AGENT_INFO",
        "DBUS_SESSION_BUS_ADDRESS",
        "TERMOS",
        "KRB5CCNAME",
        "GOOGLE_APPLICATION_CREDENTIALS",
    ];
    NAMES.contains(&upper.as_str())
        || upper.starts_with("TERMOS_PLUGIN")
        || upper.starts_with("AWS_")
        || upper.starts_with("AZURE_")
        || PARTS.iter().any(|p| upper.contains(p))
}

/// The inherited variables to drop, from `vars`.
pub fn secret_env_names(vars: impl Iterator<Item = (String, String)>) -> Vec<String> {
    vars.filter(|(k, _)| is_secret_env(k))
        .map(|(k, _)| k)
        .collect()
}

/// `termos sandbox-exec [--probe] [--write PATH]… [--net] [--keep-env] -- PROGRAM ARGS…`:
/// restricts itself with Landlock, drops secret-looking variables and
/// runs the program in its place. Returns only on failure, with the exit
/// code to use (`--probe` returns 0 after printing what is enforced).
pub fn helper_main(args: &[String]) -> i32 {
    let mut writable = Vec::new();
    let mut net = false;
    let mut probe = false;
    let mut keep_env = false;
    let mut tmp: Option<PathBuf> = None;
    let mut rest: &[String] = &[];
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--probe" => probe = true,
            "--net" => net = true,
            "--keep-env" => keep_env = true,
            "--write" => {
                i += 1;
                if let Some(p) = args.get(i) {
                    writable.push(PathBuf::from(p));
                }
            }
            "--tmp" => {
                i += 1;
                if let Some(p) = args.get(i) {
                    writable.push(PathBuf::from(p));
                    tmp = Some(PathBuf::from(p));
                }
            }
            "--" => {
                rest = &args[i + 1..];
                break;
            }
            other => {
                eprintln!("termos sandbox-exec: unknown option {other}");
                return 2;
            }
        }
        i += 1;
    }
    if !probe && rest.is_empty() {
        eprintln!("termos sandbox-exec: nothing to run");
        return 2;
    }
    let status = landlock::restrict(&writable, net);
    if probe {
        println!(
            "landlock: {}",
            match status {
                Ok(landlock::Enforced::Full) => "full",
                Ok(landlock::Enforced::Partial) => "partial",
                Ok(landlock::Enforced::None) | Err(_) => "none",
            }
        );
        return 0;
    }
    if let Err(e) = &status {
        eprintln!("termos sandbox-exec: Landlock failed: {e}");
        return 126;
    }
    let drop: Vec<String> = std::env::vars_os()
        .filter_map(|(k, _)| k.into_string().ok())
        .filter(|k| !keep_env && is_secret_env(k))
        .collect();
    exec(rest, &drop, tmp.as_deref())
}

fn command(argv: &[String], drop_env: &[String], tmp: Option<&Path>) -> Command {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    for k in drop_env {
        cmd.env_remove(k);
    }
    if let Some(t) = tmp {
        cmd.env("TMPDIR", t);
    }
    cmd
}

#[cfg(unix)]
fn exec(argv: &[String], drop_env: &[String], tmp: Option<&Path>) -> i32 {
    use std::os::unix::process::CommandExt as _;
    let err = command(argv, drop_env, tmp).exec();
    eprintln!("termos sandbox-exec: cannot run {}: {err}", argv[0]);
    if err.kind() == std::io::ErrorKind::NotFound {
        127
    } else {
        126
    }
}

#[cfg(not(unix))]
fn exec(argv: &[String], drop_env: &[String], tmp: Option<&Path>) -> i32 {
    match command(argv, drop_env, tmp).status() {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!("termos sandbox-exec: cannot run {}: {e}", argv[0]);
            127
        }
    }
}

#[cfg(target_os = "linux")]
mod landlock {
    use std::path::PathBuf;

    use landlock::{
        ABI, Access, AccessFs, AccessNet, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus,
        Scope, path_beneath_rules,
    };

    pub(super) enum Enforced {
        Full,
        Partial,
        None,
    }

    /// Device files every program may write.
    const DEVICES: &[&str] = &[
        "/dev/null",
        "/dev/zero",
        "/dev/full",
        "/dev/random",
        "/dev/urandom",
        "/dev/tty",
        "/dev/ptmx",
        "/dev/pts",
        "/dev/shm",
    ];

    pub(super) fn restrict(writable: &[PathBuf], net: bool) -> Result<Enforced, String> {
        let abi = ABI::V6;
        let write = AccessFs::from_write(abi);
        let s = |e: landlock::RulesetError| e.to_string();
        let mut rs = Ruleset::default().handle_access(write).map_err(s)?;
        if !net {
            rs = rs.handle_access(AccessNet::from_all(abi)).map_err(s)?;
        }
        rs = rs.scope(Scope::from_all(abi)).map_err(s)?;
        let status = rs
            .create()
            .map_err(s)?
            .add_rules(path_beneath_rules(writable, write))
            .map_err(s)?
            .add_rules(path_beneath_rules(DEVICES, write))
            .map_err(s)?
            .restrict_self()
            .map_err(s)?;
        Ok(match status.ruleset {
            RulesetStatus::FullyEnforced => Enforced::Full,
            RulesetStatus::PartiallyEnforced => Enforced::Partial,
            RulesetStatus::NotEnforced => Enforced::None,
        })
    }
}

#[cfg(not(target_os = "linux"))]
mod landlock {
    use std::path::PathBuf;

    pub(super) enum Enforced {
        #[allow(dead_code)]
        Full,
        #[allow(dead_code)]
        Partial,
        None,
    }

    pub(super) fn restrict(_writable: &[PathBuf], _net: bool) -> Result<Enforced, String> {
        Ok(Enforced::None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(dir: &Path) -> Spec {
        Spec {
            workspace: dir.join("ws"),
            writable: vec![dir.join("cache")],
            hidden: vec![
                dir.join("home/.ssh"),
                dir.join("home/.netrc"),
                dir.join("missing"),
            ],
            network: false,
            cwd: dir.join("ws"),
            keep_env: false,
        }
    }

    #[test]
    fn the_command_lines() {
        let dir = tempfile::tempdir().expect("tempdir");
        for d in ["ws", "cache", "home/.ssh"] {
            std::fs::create_dir_all(dir.path().join(d)).expect("dir");
        }
        std::fs::write(dir.path().join("home/.netrc"), "x").expect("file");
        let argv = vec!["sh".to_owned(), "-c".into(), "make".into()];
        let helper = PathBuf::from("/opt/termos");
        let full = Capabilities {
            bwrap: Some(PathBuf::from("/usr/bin/bwrap")),
            landlock: Landlock::Full,
        };
        let line = wrap(
            &spec(dir.path()),
            &full,
            SandboxMode::Auto,
            Some(&helper),
            &argv,
        )
        .expect("wrap");
        let text = line.join(" ");
        let d = dir.path().display();
        if cfg!(target_os = "linux") {
            assert!(text.starts_with("/usr/bin/bwrap --die-with-parent --unshare-all --ro-bind / / --dev /dev --proc /proc --tmpfs /tmp"), "{text}");
            assert!(text.contains(&format!("--tmpfs {d}/home/.ssh --ro-bind /dev/null {d}/home/.netrc --bind {d}/ws {d}/ws --bind {d}/cache {d}/cache --chdir {d}/ws -- /opt/termos sandbox-exec --write {d}/ws --write {d}/cache --write /tmp -- sh -c make")), "{text}");
            assert!(!text.contains("--share-net"));
        }
        let mut with_net = spec(dir.path());
        with_net.network = true;
        let line = wrap(&with_net, &full, SandboxMode::Auto, Some(&helper), &argv).expect("wrap");
        assert!(line.contains(&"--share-net".to_owned()));

        let none = Capabilities::none();
        assert!(
            wrap(
                &spec(dir.path()),
                &none,
                SandboxMode::Required,
                Some(&helper),
                &argv
            )
            .is_err()
        );
        assert_eq!(
            wrap(
                &spec(dir.path()),
                &full,
                SandboxMode::Off,
                Some(&helper),
                &argv
            )
            .expect("off"),
            argv
        );
        assert_eq!(none.confinement(SandboxMode::Auto), Confinement::None);
        assert_eq!(full.confinement(SandboxMode::Auto), Confinement::Full);
        let ll = Capabilities {
            bwrap: None,
            landlock: Landlock::Full,
        };
        assert_eq!(ll.confinement(SandboxMode::Auto), Confinement::Partial);
    }

    #[test]
    fn secret_looking_variables() {
        for name in [
            "ANTHROPIC_API_KEY",
            "GITHUB_TOKEN",
            "AWS_PROFILE",
            "SSH_AUTH_SOCK",
            "TERMOS",
            "TERMOS_PLUGIN_TOKEN",
            "DB_PASSWORD",
            "DBUS_SESSION_BUS_ADDRESS",
        ] {
            assert!(is_secret_env(name), "{name}");
        }
        for name in [
            "PATH",
            "HOME",
            "TERM",
            "LANG",
            "CARGO_HOME",
            "TERMOS_PANE",
            "USER",
        ] {
            assert!(!is_secret_env(name), "{name}");
        }
    }
}
