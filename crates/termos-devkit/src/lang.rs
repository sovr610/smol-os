// SPDX-License-Identifier: GPL-3.0-or-later
//! `termos lang install|use|list|remove`: languages through mise.
//!
//! - **install** makes the runtime the global default (`mise use
//!   --global`, so TermOS panes find it through mise's shims), installs
//!   its helpers the same way, and installs its language servers and
//!   debugger (`mise install`; editors start them by absolute path).
//!   A server that fails to install is reported; the language still is.
//! - **use** pins a version for the project in a folder (`mise use`,
//!   which writes `mise.toml`).
//! - **remove** uninstalls one version, or every version together with
//!   the servers and the debugger.
//! - **list** shows each language and what mise has of it.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::langs::{Language, Tool};
use crate::mise::Mise;

/// Runs mise. The real one runs the binary; tests record the calls.
pub trait Runner {
    /// Runs `mise <args>` in `cwd`, its output going to the user;
    /// whether it succeeded.
    fn run(&mut self, args: &[String], cwd: Option<&Path>) -> io::Result<bool>;
    /// Runs `mise <args>` and returns its standard output when it
    /// succeeded.
    fn output(&mut self, args: &[String]) -> io::Result<Option<String>>;
    /// Runs `mise <args>` in `cwd`, its errors going to the user; whether
    /// it succeeded, and its standard output.
    fn capture(&mut self, args: &[String], cwd: &Path) -> io::Result<(bool, String)>;
}

/// The real mise.
#[derive(Debug, Clone)]
pub struct MiseRunner {
    pub mise: Mise,
}

impl Runner for MiseRunner {
    fn run(&mut self, args: &[String], cwd: Option<&Path>) -> io::Result<bool> {
        let mut c = Command::new(&self.mise.path);
        c.args(args).stdin(Stdio::null());
        if let Some(d) = cwd {
            c.current_dir(d);
        }
        Ok(c.status()?.success())
    }

    fn output(&mut self, args: &[String]) -> io::Result<Option<String>> {
        let out = Command::new(&self.mise.path)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()?;
        Ok(out
            .status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned()))
    }

    fn capture(&mut self, args: &[String], cwd: &Path) -> io::Result<(bool, String)> {
        let out = Command::new(&self.mise.path)
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stderr(Stdio::inherit())
            .output()?;
        Ok((
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        ))
    }
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| (*x).to_owned()).collect()
}

fn spec_at(t: &Tool, version: Option<&str>) -> String {
    format!("{}@{}", t.spec, version.unwrap_or(t.version))
}

/// What an install did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub done: Vec<String>,
    /// Tools that did not install, and what they were for.
    pub failed: Vec<(String, &'static str)>,
    /// Tools with no build for this platform, and what to do instead.
    pub skipped: Vec<(String, &'static str)>,
}

#[derive(Debug, thiserror::Error)]
pub enum LangError {
    #[error("{0} has no runtime for mise to install; {1}")]
    NoRuntime(&'static str, &'static str),
    #[error("mise could not {0}")]
    Mise(String),
    #[error("{0}")]
    Io(#[from] io::Error),
}

/// `termos lang install <lang>[@version]` on `platform` (`os-arch`).
pub fn install(
    lang: &Language,
    version: Option<&str>,
    platform: &str,
    mise: &mut dyn Runner,
) -> Result<Report, LangError> {
    let mut report = Report::default();
    for limit in lang.limits_on(platform) {
        report.skipped.push((limit.tool.to_owned(), limit.why));
    }
    // A helper becomes a default only where there is none yet: installing
    // Kotlin must not replace the Java someone already uses. Servers get
    // the helper version they need either way (they start with exact
    // versions).
    let active: Vec<String> = if lang.helpers.is_empty() {
        Vec::new()
    } else {
        installed(mise)?
            .into_iter()
            .filter(|(_, v)| v.iter().any(|i| i.active))
            .map(|(k, _)| k)
            .collect()
    };
    for h in lang.helpers {
        let at = spec_at(h, None);
        let args = if active.iter().any(|k| k == h.spec) {
            s(&["install", &at])
        } else {
            s(&["use", "--global", &at])
        };
        if mise.run(&args, None)? {
            report.done.push(at);
        } else {
            report
                .failed
                .push((at, "a helper the language servers need"));
        }
    }
    match &lang.runtime {
        Some(rt) => {
            let at = spec_at(rt, version);
            if !mise.run(&s(&["use", "--global", &at]), None)? {
                return Err(LangError::Mise(format!("install {at}")));
            }
            report.done.push(at);
        }
        None if version.is_some() => {
            return Err(LangError::NoRuntime(
                lang.title,
                "use your system's compiler (TermOS installs its language server and tools)",
            ));
        }
        None => {}
    }
    let extra: Vec<(Tool, &'static str)> = lang
        .servers
        .iter()
        .filter_map(|srv| srv.tool.map(|t| (t, "the language server")))
        .chain(lang.debugger.map(|d| (d.tool, "the debugger")))
        .collect();
    let mut seen = Vec::new();
    for (t, what) in extra {
        if seen.contains(&t.spec) || !lang.available(t.spec, platform) {
            continue;
        }
        seen.push(t.spec);
        let at = spec_at(&t, None);
        if mise.run(&s(&["install", &at]), None)? {
            report.done.push(at);
        } else {
            report.failed.push((at, what));
        }
    }
    Ok(report)
}

/// `termos lang use <lang>@<version>` in `dir`: pins it for the project.
pub fn use_in(
    lang: &Language,
    version: Option<&str>,
    dir: &Path,
    mise: &mut dyn Runner,
) -> Result<String, LangError> {
    let rt = lang.runtime.as_ref().ok_or(LangError::NoRuntime(
        lang.title,
        "there is no version to pin",
    ))?;
    let at = spec_at(rt, version);
    if !mise.run(&s(&["use", &at]), Some(dir))? {
        return Err(LangError::Mise(format!("pin {at}")));
    }
    Ok(at)
}

/// `termos lang remove <lang>[@version]`.
pub fn remove(
    lang: &Language,
    version: Option<&str>,
    mise: &mut dyn Runner,
) -> Result<Vec<String>, LangError> {
    let mut removed = Vec::new();
    if let Some(v) = version {
        let rt = lang.runtime.as_ref().ok_or(LangError::NoRuntime(
            lang.title,
            "there is no version to remove",
        ))?;
        let at = format!("{}@{v}", rt.spec);
        if !mise.run(&s(&["uninstall", &at]), None)? {
            return Err(LangError::Mise(format!("uninstall {at}")));
        }
        removed.push(at);
        return Ok(removed);
    }
    if let Some(rt) = &lang.runtime {
        // Drop it from the global config first (a missing entry is fine).
        let _ = mise.run(&s(&["unuse", "--global", rt.spec]), None)?;
    }
    let mut tools: Vec<&'static str> = lang.runtime.iter().map(|t| t.spec).collect();
    tools.extend(lang.servers.iter().filter_map(|s| s.tool).map(|t| t.spec));
    tools.extend(lang.debugger.map(|d| d.tool.spec));
    tools.dedup();
    for spec in tools {
        if mise.run(&s(&["uninstall", "--all", spec]), None)? {
            removed.push(spec.to_owned());
        }
    }
    Ok(removed)
}

/// One installed version of a tool, from `mise ls --json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub version: String,
    pub path: PathBuf,
    pub active: bool,
}

/// What mise has installed, by tool spec.
pub fn installed(mise: &mut dyn Runner) -> Result<BTreeMap<String, Vec<Installed>>, LangError> {
    let text = mise
        .output(&s(&["ls", "--installed", "--json"]))?
        .ok_or_else(|| LangError::Mise("list the installed tools".into()))?;
    Ok(parse_ls(&text))
}

/// Parses `mise ls --json`: `{ "tool": [ {version, install_path, active}, … ] }`.
pub fn parse_ls(text: &str) -> BTreeMap<String, Vec<Installed>> {
    let mut out = BTreeMap::new();
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(text) else {
        return out;
    };
    for (tool, versions) in map {
        let list: Vec<Installed> = versions
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| {
                        Some(Installed {
                            version: v.get("version")?.as_str()?.to_owned(),
                            path: PathBuf::from(v.get("install_path")?.as_str()?),
                            active: v
                                .get("active")
                                .and_then(serde_json::Value::as_bool)
                                .unwrap_or(false),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.insert(tool, list);
    }
    out
}

/// A row of `termos lang list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub name: &'static str,
    pub title: &'static str,
    /// Installed runtime versions (`*` marks the active one).
    pub versions: Vec<String>,
    /// Servers and debugger: installed or not.
    pub tools: Vec<(&'static str, bool)>,
    /// What is missing on this platform.
    pub limit: Option<&'static str>,
}

/// The rows of `termos lang list`, from what mise has installed.
pub fn rows(installed: &BTreeMap<String, Vec<Installed>>, platform: &str) -> Vec<Row> {
    let has = |spec: &str| {
        // mise lists registry tools by their short name and others by
        // their full spec (without options in brackets).
        let key = spec.split('[').next().unwrap_or(spec);
        installed.get(key).is_some_and(|v| !v.is_empty())
    };
    crate::langs::LANGUAGES
        .iter()
        .map(|l| Row {
            name: l.name,
            title: l.title,
            versions: l
                .runtime
                .and_then(|rt| installed.get(rt.spec))
                .map(|v| {
                    v.iter()
                        .map(|i| {
                            if i.active {
                                format!("{}*", i.version)
                            } else {
                                i.version.clone()
                            }
                        })
                        .collect()
                })
                .unwrap_or_default(),
            tools: l
                .servers
                .iter()
                .filter_map(|srv| srv.tool.map(|t| (srv.name, has(t.spec))))
                .chain(l.debugger.map(|d| (d.name, has(d.tool.spec))))
                .collect(),
            limit: l.limits_on(platform).next().map(|x| x.why),
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::langs::find;

    /// Records calls; fails the specs listed in `fail`.
    #[derive(Default)]
    pub(crate) struct Fake {
        pub calls: Vec<String>,
        pub fail: Vec<&'static str>,
        pub ls: String,
        /// What `capture` returns as the program's output.
        pub printed: String,
    }

    impl Runner for Fake {
        fn run(&mut self, args: &[String], cwd: Option<&Path>) -> io::Result<bool> {
            let mut line = args.join(" ");
            if let Some(d) = cwd {
                line.push_str(&format!(" (in {})", d.display()));
            }
            let ok = !self.fail.iter().any(|f| line.contains(f));
            self.calls.push(line);
            Ok(ok)
        }

        fn output(&mut self, args: &[String]) -> io::Result<Option<String>> {
            self.calls.push(args.join(" "));
            Ok(Some(self.ls.clone()))
        }

        fn capture(&mut self, args: &[String], cwd: &Path) -> io::Result<(bool, String)> {
            let line = format!("{} (in {})", args.join(" "), cwd.display());
            let ok = !self.fail.iter().any(|f| line.contains(f));
            self.calls.push(line);
            Ok((ok, self.printed.clone()))
        }
    }

    #[test]
    fn install_sets_the_runtime_globally_and_installs_servers() {
        let mut m = Fake::default();
        let r = install(
            find("go").expect("go"),
            Some("1.23"),
            "linux-x86_64",
            &mut m,
        )
        .expect("installs");
        assert_eq!(
            m.calls,
            [
                "use --global go@1.23",
                "install go:golang.org/x/tools/gopls@latest",
                "install go:github.com/go-delve/delve/cmd/dlv@latest",
            ]
        );
        assert!(r.failed.is_empty());
        // Python: the helper first; a failing server is reported, the
        // language still installs.
        let mut m = Fake {
            fail: vec!["basedpyright"],
            ..Fake::default()
        };
        let r = install(
            find("python").expect("python"),
            None,
            "linux-x86_64",
            &mut m,
        )
        .expect("installs");
        assert_eq!(m.calls[0], "ls --installed --json");
        assert_eq!(m.calls[1], "use --global uv@latest");
        assert_eq!(m.calls[2], "use --global python@latest");
        assert_eq!(
            r.failed,
            [("pipx:basedpyright@latest".to_owned(), "the language server")]
        );
        // The runtime failing fails the whole install.
        let mut m = Fake {
            fail: vec!["use --global go@"],
            ..Fake::default()
        };
        assert!(install(find("go").expect("go"), None, "linux-x86_64", &mut m).is_err());
        // C has no runtime: no version can be asked for.
        let mut m = Fake::default();
        assert!(matches!(
            install(find("c").expect("c"), Some("17"), "linux-x86_64", &mut m),
            Err(LangError::NoRuntime(..))
        ));
        let r = install(find("c").expect("c"), None, "linux-x86_64", &mut m).expect("tools only");
        assert!(
            r.done.contains(&"github:clangd/clangd@latest".to_owned()),
            "{r:?}"
        );
        // A helper someone already has a default for is only installed:
        // Kotlin's Java does not replace the user's.
        let mut m = Fake {
            ls:
                r#"{"java": [{"version": "temurin-25.0.1", "install_path": "/j", "active": true}]}"#
                    .to_owned(),
            ..Fake::default()
        };
        install(
            find("kotlin").expect("kotlin"),
            None,
            "linux-x86_64",
            &mut m,
        )
        .expect("installs");
        let java = m.calls.iter().find(|c| c.contains("java@")).expect("java");
        assert!(java.starts_with("install java@"), "{:?}", m.calls);
        assert!(m.calls.contains(&"use --global kotlin@latest".to_owned()));
        // Where clangd has no build it is not tried, and the report says
        // why.
        let mut m = Fake::default();
        let r = install(find("c").expect("c"), None, "linux-aarch64", &mut m).expect("tools");
        assert!(
            !m.calls.iter().any(|c| c.contains("clangd")),
            "{:?}",
            m.calls
        );
        assert_eq!(r.skipped.len(), 1);
        assert!(r.skipped[0].1.contains("system's packages"));
    }

    #[test]
    fn use_and_remove() {
        let mut m = Fake::default();
        let at = use_in(
            find("node").expect("node"),
            Some("22"),
            Path::new("/p"),
            &mut m,
        )
        .expect("pins");
        assert_eq!(at, "node@22");
        assert_eq!(
            m.calls,
            [format!("use node@22 (in {})", Path::new("/p").display())]
        );
        let mut m = Fake::default();
        assert_eq!(
            remove(find("go").expect("go"), Some("1.22"), &mut m).expect("removes"),
            ["go@1.22"]
        );
        let mut m = Fake::default();
        let all = remove(find("go").expect("go"), None, &mut m).expect("removes");
        assert_eq!(m.calls[0], "unuse --global go");
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn listing_from_mise_json() {
        let json = r#"{
            "go": [{"version": "1.23.2", "install_path": "/m/go/1.23.2", "active": true},
                   {"version": "1.22.0", "install_path": "/m/go/1.22.0", "active": false}],
            "go:golang.org/x/tools/gopls": [{"version": "0.16.0", "install_path": "/m/gopls", "active": false}],
            "http:jdtls": [{"version": "1.63.0", "install_path": "/m/jdtls", "active": false}],
            "junk": 3
        }"#;
        let ls = parse_ls(json);
        assert_eq!(ls["go"].len(), 2);
        assert!(ls["junk"].is_empty());
        let table = rows(&ls, "linux-x86_64");
        let go = table.iter().find(|r| r.name == "go").expect("go row");
        assert_eq!(go.versions, ["1.23.2*", "1.22.0"]);
        assert_eq!(go.tools, [("gopls", true), ("delve", false)]);
        let java = table.iter().find(|r| r.name == "java").expect("java row");
        assert_eq!(java.tools, [("jdtls", true)]);
        let c = rows(&BTreeMap::new(), "linux-aarch64")
            .into_iter()
            .find(|r| r.name == "c")
            .expect("c row");
        assert!(c.limit.is_some_and(|l| l.contains("clangd")));
        assert_eq!(parse_ls("not json"), BTreeMap::new());
    }
}
