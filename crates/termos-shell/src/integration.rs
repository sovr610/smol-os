// SPDX-License-Identifier: GPL-3.0-or-later
//! Shell integration (D-115, D-116): panes start bash, zsh, fish and
//! PowerShell with a script that loads the user's own configuration first
//! and then marks prompts, command lines, output and exit status (OSC
//! 133), so TermOS can tell commands apart (command blocks). The user's
//! dotfiles are not changed; `[shell] integration = false` turns it off.
//!
//! - **bash**: `bash --rcfile <dir>/termos.bash`, which sources
//!   `~/.bashrc`.
//! - **zsh**: `ZDOTDIR=<dir>/zsh`, whose `.zshenv` puts the user's
//!   `ZDOTDIR` back (kept in `TERMOS_ZDOTDIR`) and sources theirs.
//! - **fish**: `<dir>/fish-data` first in `XDG_DATA_DIRS`, so fish loads
//!   `fish/vendor_conf.d/termos.fish` (which puts `XDG_DATA_DIRS` back).
//! - **PowerShell**: `-NoExit -Command ". '<dir>/termos.ps1'"`, after the
//!   profile.
//!
//! The scripts are written to TermOS's data folder when they differ from
//! the ones this build carries.

use std::io;
use std::path::{Path, PathBuf};

use crate::snapshot::program_name;

const BASH: &str = include_str!("../shell-integration/termos.bash");
const ZSH: &str = include_str!("../shell-integration/zsh/.zshenv");
const FISH: &str = include_str!("../shell-integration/termos.fish");
const PWSH: &str = include_str!("../shell-integration/termos.ps1");

/// A shell TermOS integrates with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    PowerShell,
}

impl Shell {
    /// The shell `program` (a path or a name) is, if one of these.
    pub fn of(program: &str) -> Option<Self> {
        match program_name(program).to_ascii_lowercase().as_str() {
            "bash" => Some(Self::Bash),
            "zsh" => Some(Self::Zsh),
            "fish" => Some(Self::Fish),
            "pwsh" | "powershell" => Some(Self::PowerShell),
            _ => None,
        }
    }
}

/// What a pane's shell starts with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Injection {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// The scripts' folder in TermOS's data folder.
pub fn dir(data_dir: &Path) -> PathBuf {
    data_dir.join("shell-integration")
}

fn files(dir: &Path) -> [(PathBuf, &'static str); 4] {
    [
        (dir.join("termos.bash"), BASH),
        (dir.join("zsh").join(".zshenv"), ZSH),
        (
            dir.join("fish-data")
                .join("fish")
                .join("vendor_conf.d")
                .join("termos.fish"),
            FISH,
        ),
        (dir.join("termos.ps1"), PWSH),
    ]
}

/// Writes the scripts into `dir` where they are missing or differ.
pub fn install(dir: &Path) -> io::Result<()> {
    for (path, text) in files(dir) {
        if std::fs::read_to_string(&path).is_ok_and(|t| t == text) {
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension(format!("termos-{}", std::process::id()));
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &path)?;
    }
    Ok(())
}

/// How to start `shell` with the scripts in `dir`; `get` reads the
/// server's environment.
pub fn inject(shell: Shell, dir: &Path, get: &dyn Fn(&str) -> Option<String>) -> Injection {
    let path = |p: PathBuf| p.display().to_string();
    match shell {
        Shell::Bash => Injection {
            args: vec!["--rcfile".into(), path(dir.join("termos.bash"))],
            env: Vec::new(),
        },
        Shell::Zsh => {
            let mut env = vec![("ZDOTDIR".to_owned(), path(dir.join("zsh")))];
            if let Some(user) = get("ZDOTDIR") {
                env.push(("TERMOS_ZDOTDIR".into(), user));
            }
            Injection {
                args: Vec::new(),
                env,
            }
        }
        Shell::Fish => {
            let user = get("XDG_DATA_DIRS").filter(|d| !d.is_empty());
            let sep = if cfg!(windows) { ';' } else { ':' };
            let rest = user
                .clone()
                .unwrap_or_else(|| "/usr/local/share:/usr/share".to_owned());
            let mut env = vec![(
                "XDG_DATA_DIRS".to_owned(),
                format!("{}{sep}{rest}", path(dir.join("fish-data"))),
            )];
            if let Some(u) = user {
                env.push(("TERMOS_XDG_DATA_DIRS".into(), u));
            }
            Injection {
                args: Vec::new(),
                env,
            }
        }
        Shell::PowerShell => Injection {
            args: vec![
                "-NoExit".into(),
                "-Command".into(),
                format!(". '{}'", path(dir.join("termos.ps1")).replace('\'', "''")),
            ],
            env: Vec::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shells_by_name() {
        assert_eq!(Shell::of("/usr/bin/bash"), Some(Shell::Bash));
        assert_eq!(Shell::of("/opt/homebrew/bin/zsh"), Some(Shell::Zsh));
        assert_eq!(Shell::of("fish"), Some(Shell::Fish));
        assert_eq!(
            Shell::of(r"C:\Program Files\PowerShell\7\pwsh.exe"),
            Some(Shell::PowerShell)
        );
        assert_eq!(Shell::of("powershell.exe"), Some(Shell::PowerShell));
        assert_eq!(Shell::of("/bin/sh"), None);
        assert_eq!(Shell::of("cmd.exe"), None);
    }

    #[test]
    fn scripts_are_written_once_and_named_in_the_injection() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let d = dir(tmp.path());
        install(&d).expect("writes");
        let bash = d.join("termos.bash");
        assert_eq!(std::fs::read_to_string(&bash).expect("read"), BASH);
        let fish = d.join("fish-data/fish/vendor_conf.d/termos.fish");
        assert!(fish.is_file());
        // A changed copy is put back.
        std::fs::write(&bash, "old").expect("write");
        install(&d).expect("writes");
        assert_eq!(std::fs::read_to_string(&bash).expect("read"), BASH);

        let none = |_: &str| None;
        let b = inject(Shell::Bash, &d, &none);
        assert_eq!(
            b.args,
            vec!["--rcfile".to_owned(), bash.display().to_string()]
        );
        let user = |k: &str| (k == "ZDOTDIR").then(|| "/home/u/.config/zsh".to_owned());
        let z = inject(Shell::Zsh, &d, &user);
        assert_eq!(
            z.env[0],
            ("ZDOTDIR".into(), d.join("zsh").display().to_string())
        );
        assert_eq!(
            z.env[1],
            ("TERMOS_ZDOTDIR".into(), "/home/u/.config/zsh".into())
        );
        assert_eq!(inject(Shell::Zsh, &d, &none).env.len(), 1);
        let f = inject(Shell::Fish, &d, &none);
        assert!(f.env[0].1.ends_with("/usr/local/share:/usr/share") || cfg!(windows));
        let p = inject(Shell::PowerShell, Path::new("/o'k"), &none);
        assert_eq!(
            p.args[2],
            format!(
                ". '{}'",
                Path::new("/o'k")
                    .join("termos.ps1")
                    .display()
                    .to_string()
                    .replace('\'', "''")
            )
        );
    }
}
