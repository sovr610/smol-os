// SPDX-License-Identifier: GPL-3.0-or-later
//! `claude` and `codex` in TermOS's windows (M11a, D-097).
//!
//! `<data>/bin/claude` and `<data>/bin/codex` (first on every pane's
//! `PATH`) are links to `termos`, which answers to the name it was
//! started by: it picks the account (one pinned for the window with
//! `TERMOS_ACCOUNT_CLAUDE`, else the one in use), points the CLI at that
//! account's folder, and runs the real program (TermOS's install, else
//! the next on `PATH`) with the same arguments. A folder the user set
//! themselves (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`) is left alone.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use termos_core::paths::Paths;
use termos_devkit::accounts::Account;
use termos_devkit::cli_agents::{self as ca, Tool};

/// The CLI this process was started as, by its link's name.
pub(crate) fn invoked_as() -> Option<Tool> {
    let first = std::env::args_os().next()?;
    let stem = Path::new(&first)
        .file_stem()?
        .to_string_lossy()
        .to_ascii_lowercase();
    Tool::from_name(&stem)
}

/// The variable that pins a window to an account (`TERMOS_ACCOUNT_CLAUDE`).
pub(crate) fn pin_variable(tool: Tool) -> String {
    format!("TERMOS_ACCOUNT_{}", tool.name().to_ascii_uppercase())
}

/// How to run a CLI.
#[derive(Debug, Clone)]
pub(crate) struct Launch {
    pub program: PathBuf,
    pub env: Vec<(String, String)>,
    /// The account, unless the user's own folder variable chose.
    pub account: Option<Account>,
}

/// The real program: TermOS's install, else the first on `path` that is
/// not TermOS itself (a link like this one).
fn real_program(paths: &Paths, tool: Tool, path: Option<&OsString>) -> Option<(PathBuf, bool)> {
    if let Some(i) = ca::installed(&paths.data_dir, tool) {
        return Some((i.path, true));
    }
    let me = std::env::current_exe()
        .ok()
        .and_then(|e| e.canonicalize().ok());
    let ours = ca::bin_dir(&paths.data_dir);
    std::env::split_paths(path?)
        .filter(|d| *d != ours)
        .map(|d| d.join(crate::tos::file_name(tool.name())))
        .filter(|p| p.is_file())
        .find(|p| me.is_none() || p.canonicalize().ok() != me)
        .map(|p| (p, false))
}

/// How to run `tool` as `account` (`None`: the pinned one, else the one
/// in use).
pub(crate) fn plan(paths: &Paths, tool: Tool, account: Option<&str>) -> anyhow::Result<Launch> {
    plan_with(paths, tool, account, &|k| std::env::var_os(k))
}

fn plan_with(
    paths: &Paths,
    tool: Tool,
    account: Option<&str>,
    get: &dyn Fn(&str) -> Option<OsString>,
) -> anyhow::Result<Launch> {
    let Some((program, ours)) = real_program(paths, tool, get("PATH").as_ref()) else {
        anyhow::bail!(
            "{} is not installed: `termos agent install {}`",
            tool.title(),
            tool.name()
        );
    };
    let mut env = Vec::new();
    if ours && tool == Tool::Claude {
        // TermOS updates its copy (`termos agent update`), not Claude Code.
        env.push(("DISABLE_AUTOUPDATER".to_owned(), "1".to_owned()));
    }
    let set = |k: &str| get(k).filter(|v| !v.is_empty());
    let chosen = account
        .map(str::to_owned)
        .or_else(|| set(&pin_variable(tool)).map(|v| v.to_string_lossy().into_owned()));
    let account = if chosen.is_none() && set(Account::variable(tool)).is_some() {
        // The user pointed it at a folder themselves.
        None
    } else {
        let all = crate::coding_agents::accounts(paths)?;
        Some(match chosen {
            Some(name) => all.get(tool, &name).map_err(|e| anyhow::anyhow!("{e}"))?,
            None => all.active(tool),
        })
    };
    if let Some(a) = &account {
        env.extend(a.env());
        // Children (a `claude` run from this one) stay on the account.
        env.push((pin_variable(tool), a.name.clone()));
    }
    Ok(Launch {
        program,
        env,
        account,
    })
}

/// Runs as the `claude` or `codex` shim.
pub(crate) fn main(tool: Tool) -> ExitCode {
    let paths = Paths::detect();
    let launch = match plan(&paths, tool, None) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("termos: {e:#}");
            return ExitCode::from(127);
        }
    };
    let mut cmd = std::process::Command::new(&launch.program);
    cmd.args(std::env::args_os().skip(1)).envs(launch.env);
    run(cmd, &launch.program)
}

#[cfg(unix)]
fn run(mut cmd: std::process::Command, program: &Path) -> ExitCode {
    use std::os::unix::process::CommandExt as _;
    // Becomes the program: its signals, terminal and exit status.
    let err = cmd.exec();
    eprintln!("termos: cannot start {}: {err}", program.display());
    ExitCode::from(126)
}

#[cfg(not(unix))]
fn run(mut cmd: std::process::Command, program: &Path) -> ExitCode {
    // The console sends Ctrl-C to both; only the program should act on it.
    ignore_ctrl_c();
    match cmd.status() {
        Ok(s) => ExitCode::from(u8::try_from(s.code().unwrap_or(1) & 0xff).unwrap_or(1)),
        Err(e) => {
            eprintln!("termos: cannot start {}: {e}", program.display());
            ExitCode::from(126)
        }
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn ignore_ctrl_c() {
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    // SAFETY: a null handler with TRUE makes this process ignore Ctrl-C;
    // no pointer is passed or kept.
    unsafe {
        SetConsoleCtrlHandler(None, 1);
    }
}

#[cfg(not(any(unix, windows)))]
fn ignore_ctrl_c() {}

/// Keeps `<data>/bin/claude` and `codex` pointing at TermOS while the CLI
/// is there to run (installed by TermOS or on `PATH`), and removes them
/// otherwise, so a missing CLI is not shadowed by TermOS.
pub(crate) fn ensure(paths: &Paths) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let dir = ca::bin_dir(&paths.data_dir);
    for tool in Tool::ALL {
        let available = real_program(paths, tool, std::env::var_os("PATH").as_ref()).is_some();
        let link = dir.join(crate::tos::file_name(tool.name()));
        if available {
            if let Err(e) = crate::tos::make_link(&exe, &dir, tool.name()) {
                tracing::warn!("cannot make {}: {e}", link.display());
            }
        } else if link.exists() {
            let _ = std::fs::remove_file(&link);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths_in(dir: &Path) -> Paths {
        let d = dir.to_owned();
        Paths::from_env(move |k| match k {
            "XDG_DATA_HOME" => Some(d.join("data").display().to_string()),
            "XDG_STATE_HOME" => Some(d.join("state").display().to_string()),
            "XDG_CONFIG_HOME" => Some(d.join("config").display().to_string()),
            "XDG_RUNTIME_DIR" => Some(d.join("run").display().to_string()),
            "HOME" | "USERPROFILE" => Some(d.join("home").display().to_string()),
            _ => None,
        })
    }

    #[test]
    fn the_account_comes_from_the_pin_then_the_one_in_use() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = paths_in(dir.path());
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).expect("mkdir");
        std::fs::write(elsewhere.join(crate::tos::file_name("claude")), b"x").expect("write");
        let path = OsString::from(elsewhere.as_os_str());
        let mut all = crate::coding_agents::accounts(&paths).expect("accounts");
        all.add(Tool::Claude, "work", None, None).expect("add");
        all.add(Tool::Claude, "play", None, None).expect("add");

        let env = |vars: Vec<(&'static str, String)>| {
            let path = path.clone();
            move |k: &str| {
                if k == "PATH" {
                    return Some(path.clone());
                }
                vars.iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| OsString::from(v))
            }
        };
        let var = |l: &Launch, k: &str| l.env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());

        // The default: nothing to point at; the program on PATH.
        let l = plan_with(&paths, Tool::Claude, None, &env(vec![])).expect("plan");
        assert_eq!(l.program, elsewhere.join(crate::tos::file_name("claude")));
        assert_eq!(var(&l, "CLAUDE_CONFIG_DIR"), None);
        assert_eq!(var(&l, "DISABLE_AUTOUPDATER"), None, "not TermOS's copy");

        // The one in use.
        all.use_account(Tool::Claude, "work").expect("use");
        let l = plan_with(&paths, Tool::Claude, None, &env(vec![])).expect("plan");
        assert!(
            var(&l, "CLAUDE_CONFIG_DIR")
                .expect("set")
                .ends_with("claude/work")
        );
        assert_eq!(var(&l, "TERMOS_ACCOUNT_CLAUDE").as_deref(), Some("work"));

        // Pinned for this window.
        let l = plan_with(
            &paths,
            Tool::Claude,
            None,
            &env(vec![("TERMOS_ACCOUNT_CLAUDE", "play".into())]),
        )
        .expect("plan");
        assert!(
            var(&l, "CLAUDE_CONFIG_DIR")
                .expect("set")
                .ends_with("claude/play")
        );

        // The user's own folder wins over the one in use.
        let l = plan_with(
            &paths,
            Tool::Claude,
            None,
            &env(vec![("CLAUDE_CONFIG_DIR", "/mine".into())]),
        )
        .expect("plan");
        assert!(l.account.is_none() && var(&l, "CLAUDE_CONFIG_DIR").is_none());

        // An unknown account, a missing program: errors that say so.
        let err = plan_with(&paths, Tool::Claude, Some("nope"), &env(vec![])).expect_err("unknown");
        assert!(
            err.to_string()
                .contains("no Claude Code account named nope"),
            "{err}"
        );
        let err = plan_with(&paths, Tool::Codex, None, &env(vec![])).expect_err("missing");
        assert!(
            err.to_string().contains("termos agent install codex"),
            "{err}"
        );
    }
}
