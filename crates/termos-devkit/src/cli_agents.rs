// SPDX-License-Identifier: GPL-3.0-or-later
//! Claude Code and Codex CLI, installed by TermOS (M10d, D-084, D-094).
//!
//! Each vendor's official native binary, downloaded with the system's
//! `curl` (as mise is, D-053) and checked before it is kept:
//!
//! - **Claude Code** from Anthropic's release site: the channel's version
//!   (`stable`), then that release's `manifest.json`, which gives the
//!   SHA-256 of every platform's binary.
//! - **Codex CLI** from OpenAI's GitHub releases: the latest release's
//!   asset for this platform, with the SHA-256 digest GitHub publishes for
//!   it.
//!
//! They go to `<data>/agents/<name>/<version>/`, with a link in
//! `<data>/bin` (first on every pane's `PATH`), and are registered with
//! TermOS's MCP server (`termos mcp serve`).

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::mise::sha256_file;

/// Anthropic's release site for Claude Code.
pub const CLAUDE_RELEASES: &str = "https://downloads.claude.ai/claude-code-releases";
/// OpenAI's latest Codex release, through GitHub's API.
pub const CODEX_LATEST: &str = "https://api.github.com/repos/openai/codex/releases/latest";
/// A tagged Codex release (`…/tags/rust-v0.46.0`).
pub const CODEX_TAGS: &str = "https://api.github.com/repos/openai/codex/releases/tags";
/// Gemini CLI's releases on GitHub (D-112).
pub const GEMINI_LATEST: &str =
    "https://api.github.com/repos/google-gemini/gemini-cli/releases/latest";
pub const GEMINI_TAGS: &str = "https://api.github.com/repos/google-gemini/gemini-cli/releases/tags";
/// The Node.js major version, from mise, that runs Gemini CLI (which needs
/// 20 or newer).
pub const GEMINI_NODE: &str = "node@22";

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("{0} is not offered for this computer ({1})")]
    Unsupported(&'static str, String),
    #[error("cannot download: {0}")]
    Fetch(String),
    #[error("the release information is not what TermOS expects: {0}")]
    Release(String),
    #[error(
        "the download does not match its checksum (expected {expected}, got {got}); nothing was installed"
    )]
    Checksum { expected: String, got: String },
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Setup(String),
}

/// The CLI agents TermOS installs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tool {
    Claude,
    Codex,
    /// Gemini CLI (M11d, D-112): Google's JavaScript bundle, run by the
    /// Node.js mise installs.
    Gemini,
}

impl Tool {
    pub const ALL: [Tool; 3] = [Tool::Claude, Tool::Codex, Tool::Gemini];

    /// The name in commands (`termos agent install claude`).
    pub fn name(self) -> &'static str {
        match self {
            Tool::Claude => "claude",
            Tool::Codex => "codex",
            Tool::Gemini => "gemini",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Tool::Claude => "Claude Code",
            Tool::Codex => "Codex CLI",
            Tool::Gemini => "Gemini CLI",
        }
    }

    /// The program's file name: Gemini CLI's is TermOS's launcher, a
    /// script (`gemini.cmd` on Windows).
    pub fn program(self) -> String {
        match self {
            Tool::Gemini if cfg!(windows) => "gemini.cmd".to_owned(),
            Tool::Gemini => "gemini".to_owned(),
            _ => exe(self.name()),
        }
    }

    pub fn from_name(name: &str) -> Option<Tool> {
        Tool::ALL.into_iter().find(|t| t.name() == name)
    }
}

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    }
}

/// The computer, as the vendors name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Platform {
    /// `linux`, `macos` or `windows`.
    pub os: &'static str,
    /// `x86_64` or `aarch64`.
    pub arch: &'static str,
    /// A Linux without glibc (Alpine, TermOS's own images).
    pub musl: bool,
}

impl Platform {
    /// This computer.
    pub fn here() -> Platform {
        let musl = cfg!(target_env = "musl")
            || (cfg!(target_os = "linux")
                && std::fs::read_dir("/lib").is_ok_and(|d| {
                    d.flatten()
                        .any(|e| e.file_name().to_string_lossy().starts_with("ld-musl-"))
                }));
        Platform {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            musl,
        }
    }

    fn describe(self) -> String {
        format!(
            "{} {}{}",
            self.os,
            self.arch,
            if self.musl { " (musl)" } else { "" }
        )
    }

    /// Claude Code's name for it (`linux-x64-musl`, `darwin-arm64`, …).
    pub fn claude(self) -> Option<String> {
        let arch = match self.arch {
            "x86_64" => "x64",
            "aarch64" => "arm64",
            _ => return None,
        };
        Some(match self.os {
            "linux" if self.musl => format!("linux-{arch}-musl"),
            "linux" => format!("linux-{arch}"),
            "macos" => format!("darwin-{arch}"),
            "windows" => format!("win32-{arch}"),
            _ => return None,
        })
    }

    /// Codex's target triple. Its Linux builds are static (musl): they
    /// run on glibc systems too.
    pub fn codex(self) -> Option<String> {
        let arch = match self.arch {
            "x86_64" | "aarch64" => self.arch,
            _ => return None,
        };
        Some(match self.os {
            "linux" => format!("{arch}-unknown-linux-musl"),
            "macos" => format!("{arch}-apple-darwin"),
            "windows" => format!("{arch}-pc-windows-msvc"),
            _ => return None,
        })
    }
}

/// How a release's file is packed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Packing {
    /// The program itself.
    Plain,
    TarGz,
    Zip,
}

/// A release to install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tool: Tool,
    pub version: String,
    pub url: String,
    /// Lowercase hex.
    pub sha256: String,
    pub packing: Packing,
    /// For an archive: the program's name in it (an archive can hold
    /// helper programs too).
    pub inner: Option<String>,
}

/// Downloads (the system's `curl`; fixtures in tests).
pub trait Fetch {
    /// The body of `url`.
    fn text(&self, url: &str) -> Result<String, AgentError>;
    /// Saves `url` to `dest`.
    fn file(&self, url: &str, dest: &Path) -> Result<(), AgentError>;
}

/// The system's `curl`, HTTPS only. A `GITHUB_TOKEN` in the environment is
/// sent to GitHub's API (more requests an hour), through curl's standard
/// input so other users cannot see it.
pub struct Curl;

impl Curl {
    fn run(url: &str, out: Option<&Path>) -> Result<Vec<u8>, AgentError> {
        let token = url
            .starts_with("https://api.github.com/")
            .then(|| std::env::var("GITHUB_TOKEN").or_else(|_| std::env::var("GH_TOKEN")))
            .and_then(Result::ok)
            .filter(|t| !t.is_empty());
        let mut cmd = Command::new(exe("curl"));
        cmd.args(["--fail", "--location", "--silent", "--show-error"])
            .args(["--proto", "=https", "--tlsv1.2", "--retry", "3"])
            .args(["--user-agent", "termos"]);
        if token.is_some() {
            cmd.args(["--header", "@-"]);
        }
        if let Some(dest) = out {
            cmd.arg("--output").arg(dest);
        }
        cmd.arg(url)
            .stdin(if token.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // The images ship curl with mbedTLS (D-109); hosts have their own.
        let mut child = cmd.spawn().map_err(|e| {
            AgentError::Fetch(format!("cannot run curl ({e}); installing needs curl"))
        })?;
        if let (Some(t), Some(mut stdin)) = (token, child.stdin.take()) {
            use std::io::Write as _;
            let _ = writeln!(stdin, "Authorization: Bearer {t}");
        }
        let output = child
            .wait_with_output()
            .map_err(|e| AgentError::Fetch(format!("curl: {e}")))?;
        if !output.status.success() {
            let why = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            return Err(AgentError::Fetch(format!("{url}: {why}")));
        }
        Ok(output.stdout)
    }
}

impl Fetch for Curl {
    fn text(&self, url: &str) -> Result<String, AgentError> {
        Curl::run(url, None).map(|b| String::from_utf8_lossy(&b).into_owned())
    }
    fn file(&self, url: &str, dest: &Path) -> Result<(), AgentError> {
        Curl::run(url, Some(dest)).map(|_| ())
    }
}

/// Claude Code's release for `platform`: `version`, or the `stable`
/// channel's.
pub fn claude_release(
    fetch: &dyn Fetch,
    platform: Platform,
    version: Option<&str>,
) -> Result<Release, AgentError> {
    let plat = platform
        .claude()
        .ok_or_else(|| AgentError::Unsupported("Claude Code", platform.describe()))?;
    let version = match version {
        Some(v) => v.to_owned(),
        None => fetch
            .text(&format!("{CLAUDE_RELEASES}/stable"))?
            .trim()
            .to_owned(),
    };
    check_version(&version)?;
    let manifest: Value =
        serde_json::from_str(&fetch.text(&format!("{CLAUDE_RELEASES}/{version}/manifest.json"))?)
            .map_err(|e| AgentError::Release(format!("manifest.json: {e}")))?;
    let sha256 = manifest
        .pointer(&format!("/platforms/{plat}/checksum"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            AgentError::Unsupported("Claude Code", format!("{plat}, not in the manifest"))
        })?
        .to_ascii_lowercase();
    check_hex(&sha256)?;
    Ok(Release {
        tool: Tool::Claude,
        url: format!(
            "{CLAUDE_RELEASES}/{version}/{plat}/{}",
            if platform.os == "windows" {
                "claude.exe"
            } else {
                "claude"
            }
        ),
        version,
        sha256,
        packing: Packing::Plain,
        inner: None,
    })
}

/// Codex CLI's release for `platform`: `version`, or the latest.
pub fn codex_release(
    fetch: &dyn Fetch,
    platform: Platform,
    version: Option<&str>,
) -> Result<Release, AgentError> {
    let triple = platform
        .codex()
        .ok_or_else(|| AgentError::Unsupported("Codex CLI", platform.describe()))?;
    let url = match version {
        Some(v) => {
            check_version(v)?;
            format!("{CODEX_TAGS}/rust-v{v}")
        }
        None => CODEX_LATEST.to_owned(),
    };
    let release: Value = serde_json::from_str(&fetch.text(&url)?)
        .map_err(|e| AgentError::Release(format!("the release: {e}")))?;
    let tag = release
        .get("tag_name")
        .and_then(Value::as_str)
        .ok_or_else(|| AgentError::Release("no tag_name".into()))?;
    let version = tag
        .trim_start_matches("rust-v")
        .trim_start_matches('v')
        .to_owned();
    check_version(&version)?;
    // The CLI itself: `codex-<triple>.tar.gz`, or `.zip` (Windows: the
    // program inside is `codex-<triple>.exe`); other programs in the
    // release (`codex-responses-api-proxy-…`) are not it.
    let wanted: Vec<(String, Packing)> = if platform.os == "windows" {
        vec![
            (format!("codex-{triple}.exe.zip"), Packing::Zip),
            (format!("codex-{triple}.zip"), Packing::Zip),
            (format!("codex-{triple}.exe.tar.gz"), Packing::TarGz),
        ]
    } else {
        vec![(format!("codex-{triple}.tar.gz"), Packing::TarGz)]
    };
    let assets = release
        .get("assets")
        .and_then(Value::as_array)
        .ok_or_else(|| AgentError::Release("no assets".into()))?;
    let (asset, packing) = wanted
        .iter()
        .find_map(|(name, packing)| {
            assets
                .iter()
                .find(|a| a.get("name").and_then(Value::as_str) == Some(name))
                .map(|a| (a, *packing))
        })
        .ok_or_else(|| {
            AgentError::Unsupported("Codex CLI", format!("{triple}, not in release {tag}"))
        })?;
    let (url, sha256) = github_download(asset)?;
    Ok(Release {
        tool: Tool::Codex,
        version,
        url,
        sha256,
        packing,
        inner: Some(format!(
            "codex-{triple}{}",
            if platform.os == "windows" { ".exe" } else { "" }
        )),
    })
}

/// A GitHub release asset's HTTPS download and the SHA-256 GitHub gives
/// for it (an asset without one is refused).
fn github_download(asset: &Value) -> Result<(String, String), AgentError> {
    let url = asset
        .get("browser_download_url")
        .and_then(Value::as_str)
        .filter(|u| u.starts_with("https://"))
        .ok_or_else(|| AgentError::Release("no download URL".into()))?
        .to_owned();
    let sha256 = asset
        .get("digest")
        .and_then(Value::as_str)
        .and_then(|d| d.strip_prefix("sha256:"))
        .ok_or_else(|| {
            AgentError::Release(format!(
                "GitHub gives no SHA-256 digest for {}; refusing an unchecked download",
                asset
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("the asset")
            ))
        })?
        .to_ascii_lowercase();
    check_hex(&sha256)?;
    Ok((url, sha256))
}

/// Gemini CLI's release: its JavaScript bundle (`gemini-cli-bundle.zip`),
/// the same on every computer, which Node.js runs (D-112).
pub fn gemini_release(
    fetch: &dyn Fetch,
    platform: Platform,
    version: Option<&str>,
) -> Result<Release, AgentError> {
    if !matches!(platform.os, "linux" | "macos" | "windows")
        || !matches!(platform.arch, "x86_64" | "aarch64")
    {
        return Err(AgentError::Unsupported("Gemini CLI", platform.describe()));
    }
    let url = match version {
        Some(v) => {
            check_version(v)?;
            format!("{GEMINI_TAGS}/v{v}")
        }
        None => GEMINI_LATEST.to_owned(),
    };
    let release: Value = serde_json::from_str(&fetch.text(&url)?)
        .map_err(|e| AgentError::Release(format!("the release: {e}")))?;
    let tag = release
        .get("tag_name")
        .and_then(Value::as_str)
        .ok_or_else(|| AgentError::Release("no tag_name".into()))?;
    let version = tag.trim_start_matches('v').to_owned();
    check_version(&version)?;
    let asset = release
        .get("assets")
        .and_then(Value::as_array)
        .and_then(|a| {
            a.iter()
                .find(|a| a.get("name").and_then(Value::as_str) == Some("gemini-cli-bundle.zip"))
        })
        .ok_or_else(|| {
            AgentError::Release(format!("release {tag} has no gemini-cli-bundle.zip"))
        })?;
    let (url, sha256) = github_download(asset)?;
    Ok(Release {
        tool: Tool::Gemini,
        version,
        url,
        sha256,
        packing: Packing::Zip,
        inner: Some(GEMINI_SCRIPT.to_owned()),
    })
}

/// The bundle's entry point.
pub const GEMINI_SCRIPT: &str = "gemini.js";

/// TermOS's launcher for Gemini CLI: the Node.js mise installed, running
/// the bundle (paths quoted for the shell, or for `cmd`).
pub fn gemini_launcher(node: &Path, script: &Path) -> String {
    if cfg!(windows) {
        // Inside quotes only `%` is special to cmd (a user name may hold one).
        let q = |p: &Path| format!("\"{}\"", p.display().to_string().replace('%', "%%"));
        format!(
            "@echo off\r\nrem Gemini CLI, installed by TermOS: Node.js from mise runs its bundle.\r\n{} {} %*\r\n",
            q(node),
            q(script)
        )
    } else {
        let q = |p: &Path| format!("'{}'", p.display().to_string().replace('\'', "'\\''"));
        format!(
            "#!/bin/sh\n# Gemini CLI, installed by TermOS: Node.js from mise runs its bundle.\nexec {} {} \"$@\"\n",
            q(node),
            q(script)
        )
    }
}

/// A version is a plain name: it becomes part of URLs and a folder.
fn check_version(v: &str) -> Result<(), AgentError> {
    let plain = !v.is_empty()
        && v.len() <= 64
        && v.starts_with(|c: char| c.is_ascii_digit())
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || ".-+".contains(c));
    if plain {
        Ok(())
    } else {
        Err(AgentError::Release(format!("version {v:?}")))
    }
}

fn check_hex(sha: &str) -> Result<(), AgentError> {
    if sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(AgentError::Release(format!("checksum {sha:?}")))
    }
}

/// `tool`'s release for this computer: `version`, or the newest.
pub fn release(
    fetch: &dyn Fetch,
    tool: Tool,
    platform: Platform,
    version: Option<&str>,
) -> Result<Release, AgentError> {
    match tool {
        Tool::Claude => claude_release(fetch, platform, version),
        Tool::Codex => codex_release(fetch, platform, version),
        Tool::Gemini => gemini_release(fetch, platform, version),
    }
}

/// The newest release of `tool` for this computer.
pub fn latest(fetch: &dyn Fetch, tool: Tool, platform: Platform) -> Result<Release, AgentError> {
    release(fetch, tool, platform, None)
}

/// Where TermOS keeps `tool`.
pub fn dir(data_dir: &Path, tool: Tool) -> PathBuf {
    data_dir.join("agents").join(tool.name())
}

/// The folder TermOS puts first on every pane's `PATH`, where its `tos`,
/// `claude` and `codex` are (links to `termos`, D-085, D-097).
pub fn bin_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("bin")
}

/// An installed tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub tool: Tool,
    pub version: String,
    /// The program.
    pub path: PathBuf,
}

/// What TermOS has installed of `tool`.
pub fn installed(data_dir: &Path, tool: Tool) -> Option<Installed> {
    let d = dir(data_dir, tool);
    let version = std::fs::read_to_string(d.join("current")).ok()?;
    let version = version.trim().to_owned();
    let path = d.join(&version).join(tool.program());
    path.is_file().then_some(Installed {
        tool,
        version,
        path,
    })
}

/// Installs `release` (downloaded, checked, unpacked and made current).
/// Older versions are removed. TermOS's `claude` and `codex` in
/// `<data>/bin` are its own shims, which run the current version (D-097).
pub fn install(
    fetch: &dyn Fetch,
    data_dir: &Path,
    release: &Release,
    say: &mut dyn FnMut(&str),
) -> Result<Installed, AgentError> {
    install_with(fetch, data_dir, release, None, say)
}

/// [`install`], with the Node.js that runs Gemini CLI.
pub fn install_with(
    fetch: &dyn Fetch,
    data_dir: &Path,
    release: &Release,
    node: Option<&Path>,
    say: &mut dyn FnMut(&str),
) -> Result<Installed, AgentError> {
    let tool = release.tool;
    if tool == Tool::Gemini && node.is_none() {
        return Err(AgentError::Setup(
            "Gemini CLI needs Node.js (from mise)".into(),
        ));
    }
    let root = dir(data_dir, tool);
    std::fs::create_dir_all(&root)?;
    let work = root.join(format!(".{}.partial", release.version));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)?;
    let download = work.join("download");
    say(&format!(
        "downloading {} {}…",
        tool.title(),
        release.version
    ));
    if let Err(e) = fetch.file(&release.url, &download) {
        let _ = std::fs::remove_dir_all(&work);
        return Err(e);
    }
    let got = sha256_file(&download)?;
    if got != release.sha256 {
        let _ = std::fs::remove_dir_all(&work);
        return Err(AgentError::Checksum {
            expected: release.sha256.clone(),
            got,
        });
    }
    say("checksum verified");
    let program = work.join(tool.program());
    let target = root.join(&release.version);
    match release.packing {
        Packing::Zip if tool == Tool::Gemini => {
            unpack_zip(&download, &work)?;
            let _ = std::fs::remove_file(&download);
            let Some(script) = find_file(&work, GEMINI_SCRIPT, 3) else {
                let _ = std::fs::remove_dir_all(&work);
                return Err(AgentError::Release(format!(
                    "the bundle has no {GEMINI_SCRIPT}; nothing was installed"
                )));
            };
            // Where it will be once the folder is in place.
            let relative = script.strip_prefix(&work).unwrap_or(&script).to_owned();
            let node = node.unwrap_or(Path::new("node"));
            std::fs::write(&program, gemini_launcher(node, &target.join(relative)))?;
        }
        Packing::Plain => std::fs::rename(&download, &program)?,
        Packing::TarGz | Packing::Zip => {
            if release.packing == Packing::Zip {
                unpack_zip(&download, &work)?;
            } else {
                unpack(&download, &work)?;
            }
            let _ = std::fs::remove_file(&download);
            // The program by its exact name; helpers beside it stay.
            let inner = release.inner.as_deref().unwrap_or(tool.name());
            let Some(found) = find_file(&work, inner, 3) else {
                let _ = std::fs::remove_dir_all(&work);
                return Err(AgentError::Release(format!(
                    "the archive has no {inner}; nothing was installed"
                )));
            };
            if found != program {
                std::fs::rename(&found, &program)?;
            }
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))?;
    }
    let _ = std::fs::remove_dir_all(&target);
    std::fs::rename(&work, &target)?;
    std::fs::write(root.join("current"), format!("{}\n", release.version))?;
    // Older versions go (one still running stays until next time).
    if let Ok(entries) = std::fs::read_dir(&root) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if e.path().is_dir() && name != release.version && !name.starts_with('.') {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    let path = target.join(tool.program());
    Ok(Installed {
        tool,
        version: release.version.clone(),
        path,
    })
}

/// The file named `name` under `dir`, at most `depth` folders down.
fn find_file(dir: &Path, name: &str, depth: u32) -> Option<PathBuf> {
    let entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .collect();
    entries
        .iter()
        .find(|p| p.is_file() && p.file_name().is_some_and(|n| n == name))
        .cloned()
        .or_else(|| {
            (depth > 0)
                .then(|| {
                    entries
                        .iter()
                        .filter(|p| p.is_dir())
                        .find_map(|d| find_file(d, name, depth - 1))
                })
                .flatten()
        })
}

/// Unpacks a zip file (names that would leave `into` are refused by the
/// `zip` crate).
fn unpack_zip(archive: &Path, into: &Path) -> Result<(), AgentError> {
    let file = std::fs::File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|e| AgentError::Setup(format!("cannot read the download: {e}")))?;
    zip.extract(into)
        .map_err(|e| AgentError::Setup(format!("cannot unpack the download: {e}")))
}

fn unpack(archive: &Path, into: &Path) -> Result<(), AgentError> {
    // `tar` reads both: GNU or BSD tar on Unix, bsdtar (which also reads
    // zip files) on Windows 10 and later.
    let status = Command::new(exe("tar"))
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(into)
        .stdin(Stdio::null())
        .status()
        .map_err(|e| AgentError::Setup(format!("cannot run tar: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(AgentError::Setup(format!(
            "cannot unpack the download ({status})"
        )))
    }
}

/// Removes `tool`: its versions.
pub fn remove(data_dir: &Path, tool: Tool) -> io::Result<bool> {
    let root = dir(data_dir, tool);
    let had = root.exists();
    if had {
        std::fs::remove_dir_all(&root)?;
    }
    Ok(had)
}

/// The command lines that register `termos mcp serve` with `tool`, run
/// with the installed program (`claude mcp add`); `None` for tools set up
/// by editing their config ([`codex_config`]).
pub fn mcp_register(tool: Tool, program: &Path, termos: &Path) -> Option<Vec<Vec<String>>> {
    let p = program.display().to_string();
    let t = termos.display().to_string();
    match tool {
        Tool::Claude => Some(vec![
            // Replaced, so a moved termos is picked up.
            vec![
                p.clone(),
                "mcp".into(),
                "remove".into(),
                "--scope".into(),
                "user".into(),
                "termos".into(),
            ],
            vec![
                p,
                "mcp".into(),
                "add".into(),
                "--scope".into(),
                "user".into(),
                "termos".into(),
                "--".into(),
                t,
                "mcp".into(),
                "serve".into(),
            ],
        ]),
        Tool::Codex | Tool::Gemini => None,
    }
}

/// What `termos mcp serve` needs from the agent's environment: the
/// server, session and pane (`$TERMOS`, `$TERMOS_PANE`) and TermOS's
/// folders.
pub const MCP_FORWARDED: &[&str] = &[
    "TERMOS",
    "TERMOS_PANE",
    "XDG_RUNTIME_DIR",
    "XDG_STATE_HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
];

/// Codex's `config.toml` with `[mcp_servers.termos]` running
/// `termos mcp serve` (the rest of the file kept as it is).
pub fn codex_config(existing: &str, termos: &Path) -> Result<String, AgentError> {
    let mut doc: toml_edit::DocumentMut = existing
        .parse()
        .map_err(|e| AgentError::Setup(format!("~/.codex/config.toml does not parse: {e}")))?;
    let servers = doc
        .entry("mcp_servers")
        .or_insert_with(|| {
            let mut t = toml_edit::Table::new();
            t.set_implicit(true);
            toml_edit::Item::Table(t)
        })
        .as_table_mut()
        .ok_or_else(|| {
            AgentError::Setup("`mcp_servers` in ~/.codex/config.toml is not a table".into())
        })?;
    let mut termos_table = toml_edit::Table::new();
    termos_table["command"] = toml_edit::value(termos.display().to_string());
    let mut args = toml_edit::Array::new();
    args.push("mcp");
    args.push("serve");
    termos_table["args"] = toml_edit::value(args);
    // Codex starts MCP servers with an emptied environment; these tell
    // `termos mcp serve` which TermOS, session and pane it serves.
    let mut vars = toml_edit::Array::new();
    for v in MCP_FORWARDED {
        vars.push(*v);
    }
    termos_table["env_vars"] = toml_edit::value(vars);
    servers.insert("termos", toml_edit::Item::Table(termos_table));
    Ok(doc.to_string())
}

/// Codex's settings folder: `$CODEX_HOME`, else `~/.codex`.
pub fn codex_home(get: &dyn Fn(&str) -> Option<String>, home: Option<&Path>) -> Option<PathBuf> {
    get("CODEX_HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.map(|h| h.join(".codex")))
}

/// Claude Code's settings file, which holds its user-scope MCP servers:
/// `$CLAUDE_CONFIG_DIR/.claude.json`, else `~/.claude.json`.
pub fn claude_settings(
    get: &dyn Fn(&str) -> Option<String>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    get("CLAUDE_CONFIG_DIR")
        .filter(|h| !h.is_empty())
        .map(|d| PathBuf::from(d).join(".claude.json"))
        .or_else(|| home.map(|h| h.join(".claude.json")))
}

/// Whether TermOS's MCP server is registered in `tool`'s settings (the
/// file's text); only that one entry is looked at.
pub fn registered(tool: Tool, settings: &str) -> bool {
    match tool {
        Tool::Claude => serde_json::from_str::<Value>(settings)
            .ok()
            .is_some_and(|v| v.pointer("/mcpServers/termos").is_some()),
        Tool::Codex => settings
            .parse::<toml_edit::DocumentMut>()
            .ok()
            .is_some_and(|d| d.get("mcp_servers").and_then(|s| s.get("termos")).is_some()),
        Tool::Gemini => serde_json::from_str::<Value>(settings)
            .ok()
            .is_some_and(|v| v.pointer("/mcpServers/termos").is_some()),
    }
}

/// Gemini CLI's `settings.json` as an object (empty when there is none).
fn gemini_object(existing: &str) -> Result<serde_json::Map<String, Value>, AgentError> {
    if existing.trim().is_empty() {
        return Ok(serde_json::Map::new());
    }
    match serde_json::from_str::<Value>(existing) {
        Ok(Value::Object(m)) => Ok(m),
        _ => Err(AgentError::Setup(
            "Gemini CLI's settings.json does not parse as plain JSON (comments?); not changed"
                .into(),
        )),
    }
}

fn object_at<'a>(
    m: &'a mut serde_json::Map<String, Value>,
    key: &str,
) -> &'a mut serde_json::Map<String, Value> {
    let v = m
        .entry(key.to_owned())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !v.is_object() {
        *v = Value::Object(serde_json::Map::new());
    }
    v.as_object_mut().expect("made an object above")
}

/// Gemini CLI's `settings.json` with `mcpServers.termos` running `termos
/// mcp serve` (the rest kept). With `managed` (TermOS's install), its own
/// updates and update notices are turned off: `termos agent update`
/// updates it.
pub fn gemini_settings(existing: &str, termos: &Path, managed: bool) -> Result<String, AgentError> {
    let mut m = gemini_object(existing)?;
    object_at(&mut m, "mcpServers").insert(
        "termos".into(),
        serde_json::json!({
            "command": termos.display().to_string(),
            "args": ["mcp", "serve"],
        }),
    );
    if managed {
        let general = object_at(&mut m, "general");
        general.insert("enableAutoUpdate".into(), Value::Bool(false));
        general.insert("enableAutoUpdateNotification".into(), Value::Bool(false));
    }
    serde_json::to_string_pretty(&Value::Object(m))
        .map(|t| t + "\n")
        .map_err(|e| AgentError::Setup(e.to_string()))
}

/// Gemini CLI's `settings.json` without `mcpServers.termos`.
pub fn gemini_settings_without(existing: &str) -> Result<String, AgentError> {
    let mut m = gemini_object(existing)?;
    if let Some(Value::Object(servers)) = m.get_mut("mcpServers") {
        servers.remove("termos");
    }
    serde_json::to_string_pretty(&Value::Object(m))
        .map(|t| t + "\n")
        .map_err(|e| AgentError::Setup(e.to_string()))
}

/// Gemini CLI's `settings.json` signing in with `kind` (`oauth-personal`:
/// a Google account; `gemini-api-key`: `GEMINI_API_KEY`), so it does not
/// ask which.
pub fn gemini_auth(existing: &str, kind: &str) -> Result<String, AgentError> {
    let mut m = gemini_object(existing)?;
    let security = object_at(&mut m, "security");
    object_at(security, "auth").insert("selectedType".into(), Value::String(kind.into()));
    serde_json::to_string_pretty(&Value::Object(m))
        .map(|t| t + "\n")
        .map_err(|e| AgentError::Setup(e.to_string()))
}

/// Codex's `config.toml` without `[mcp_servers.termos]`.
pub fn codex_config_without(existing: &str) -> Result<String, AgentError> {
    let mut doc: toml_edit::DocumentMut = existing
        .parse()
        .map_err(|e| AgentError::Setup(format!("~/.codex/config.toml does not parse: {e}")))?;
    if let Some(servers) = doc
        .get_mut("mcp_servers")
        .and_then(|s| s.as_table_like_mut())
    {
        servers.remove("termos");
    }
    Ok(doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    /// Recorded release data and downloads.
    struct Fixture {
        texts: BTreeMap<String, String>,
        files: BTreeMap<String, Vec<u8>>,
        asked: Mutex<Vec<String>>,
    }

    impl Fetch for Fixture {
        fn text(&self, url: &str) -> Result<String, AgentError> {
            self.asked.lock().expect("asked").push(url.to_owned());
            self.texts
                .get(url)
                .cloned()
                .ok_or_else(|| AgentError::Fetch(format!("{url}: 404")))
        }
        fn file(&self, url: &str, dest: &Path) -> Result<(), AgentError> {
            self.asked.lock().expect("asked").push(url.to_owned());
            let body = self
                .files
                .get(url)
                .ok_or_else(|| AgentError::Fetch(format!("{url}: 404")))?;
            std::fs::write(dest, body).map_err(AgentError::Io)
        }
    }

    fn sha(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    const LINUX: Platform = Platform {
        os: "linux",
        arch: "x86_64",
        musl: false,
    };

    #[test]
    fn platforms_are_named_as_each_vendor_names_them() {
        let p = |os, arch, musl| Platform { os, arch, musl };
        assert_eq!(
            p("linux", "x86_64", false).claude().as_deref(),
            Some("linux-x64")
        );
        assert_eq!(
            p("linux", "aarch64", true).claude().as_deref(),
            Some("linux-arm64-musl")
        );
        assert_eq!(
            p("macos", "aarch64", false).claude().as_deref(),
            Some("darwin-arm64")
        );
        assert_eq!(
            p("windows", "x86_64", false).claude().as_deref(),
            Some("win32-x64")
        );
        assert_eq!(p("linux", "riscv64", false).claude(), None);
        assert_eq!(
            p("linux", "x86_64", false).codex().as_deref(),
            Some("x86_64-unknown-linux-musl"),
            "static builds run on glibc too"
        );
        assert_eq!(
            p("macos", "x86_64", false).codex().as_deref(),
            Some("x86_64-apple-darwin")
        );
        assert_eq!(
            p("windows", "aarch64", false).codex().as_deref(),
            Some("aarch64-pc-windows-msvc")
        );
        assert_eq!(Tool::from_name("codex"), Some(Tool::Codex));
        assert_eq!(Tool::from_name("gemini"), Some(Tool::Gemini));
        assert_eq!(Tool::from_name("aider"), None);
    }

    /// A zip of `files` (name, contents).
    #[cfg(unix)]
    fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write as _;
        let mut out = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut out);
            for (name, body) in files {
                z.start_file(*name, zip::write::SimpleFileOptions::default())
                    .expect("start");
                z.write_all(body).expect("write");
            }
            z.finish().expect("finish");
        }
        out.into_inner()
    }

    #[cfg(unix)]
    #[test]
    fn gemini_is_its_bundle_run_by_node() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bytes = zip_of(&[
            ("bundle/gemini.js", b"console.log('fixture')\n"),
            ("bundle/policies/read-only.toml", b"x\n"),
        ]);
        let url = "https://github.com/google-gemini/gemini-cli/releases/download/v0.67.0/gemini-cli-bundle.zip";
        let release_json = |digest: Option<String>| {
            let mut asset = serde_json::json!({
                "name": "gemini-cli-bundle.zip",
                "browser_download_url": url,
            });
            if let Some(d) = digest {
                asset["digest"] = serde_json::Value::String(d);
            }
            serde_json::json!({"tag_name": "v0.67.0", "assets": [asset]}).to_string()
        };
        let f = Fixture {
            texts: BTreeMap::from([(
                GEMINI_LATEST.to_owned(),
                release_json(Some(format!("sha256:{}", sha(&bytes)))),
            )]),
            files: BTreeMap::from([(url.to_owned(), bytes.clone())]),
            asked: Mutex::default(),
        };
        let r = latest(&f, Tool::Gemini, LINUX).expect("release");
        assert_eq!((r.version.as_str(), r.packing), ("0.67.0", Packing::Zip));
        let data = dir.path().join("data");
        // No Node.js: nothing is installed.
        assert!(install(&f, &data, &r, &mut |_| {}).is_err());
        // A stand-in Node.js that says what it was asked to run.
        let node = dir.path().join("node it");
        std::fs::write(&node, b"#!/bin/sh\necho \"node $*\"\n").expect("write");
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&node, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let got = install_with(&f, &data, &r, Some(&node), &mut |_| {}).expect("installed");
        assert!(got.path.ends_with("0.67.0/gemini"));
        assert_eq!(
            installed(&data, Tool::Gemini).map(|i| i.version).as_deref(),
            Some("0.67.0")
        );
        let script = dir
            .path()
            .join("data/agents/gemini/0.67.0/bundle/gemini.js");
        assert!(
            script.is_file()
                && dir
                    .path()
                    .join("data/agents/gemini/0.67.0/bundle/policies/read-only.toml")
                    .is_file()
        );
        let out = (0..50)
            .find_map(
                |_| match Command::new(&got.path).arg("--version").output() {
                    Err(e) if e.kind() == io::ErrorKind::ExecutableFileBusy => {
                        std::thread::sleep(std::time::Duration::from_millis(20));
                        None
                    }
                    other => Some(other),
                },
            )
            .expect("settled")
            .expect("runs");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            format!("node {} --version", script.display())
        );
        // A bundle without its digest is refused.
        let f = Fixture {
            texts: BTreeMap::from([(GEMINI_LATEST.to_owned(), release_json(None))]),
            files: BTreeMap::new(),
            asked: Mutex::default(),
        };
        let err = latest(&f, Tool::Gemini, LINUX).expect_err("no digest");
        assert!(err.to_string().contains("no SHA-256"), "{err}");
    }

    #[test]
    fn gemini_settings_get_termos_and_their_sign_in_method() {
        let before = r#"{"theme":"Dracula","mcpServers":{"mine":{"command":"x"}}}"#;
        let after = gemini_settings(before, Path::new("/t/termos"), true).expect("edit");
        let v: Value = serde_json::from_str(&after).expect("json");
        assert_eq!(v["theme"], "Dracula");
        assert_eq!(v["mcpServers"]["mine"]["command"], "x");
        assert_eq!(v["mcpServers"]["termos"]["command"], "/t/termos");
        assert_eq!(
            v["mcpServers"]["termos"]["args"],
            serde_json::json!(["mcp", "serve"])
        );
        assert_eq!(v["general"]["enableAutoUpdate"], false);
        assert!(registered(Tool::Gemini, &after));
        let removed = gemini_settings_without(&after).expect("remove");
        assert!(!registered(Tool::Gemini, &removed));
        assert!(removed.contains("mine"));
        let keyed = gemini_auth("", "gemini-api-key").expect("auth");
        assert!(
            keyed.contains("\"selectedType\": \"gemini-api-key\""),
            "{keyed}"
        );
        assert!(gemini_settings("// a comment\n{}", Path::new("/t"), false).is_err());
        let launcher = gemini_launcher(Path::new("/n/o'de%x"), Path::new("/b/gemini.js"));
        if cfg!(windows) {
            assert!(
                launcher.contains("\"/n/o'de%%x\" \"/b/gemini.js\" %*"),
                "{launcher}"
            );
        } else {
            assert!(
                launcher.contains("exec '/n/o'\\''de%x' '/b/gemini.js' \"$@\""),
                "{launcher}"
            );
        }
    }

    fn claude_fixture(binary: &[u8], checksum: &str) -> Fixture {
        let manifest = serde_json::json!({
            "version": "9.9.9",
            "platforms": {
                "linux-x64": { "checksum": checksum, "size": binary.len() },
                "darwin-arm64": { "checksum": "0".repeat(64), "size": 1 },
            }
        });
        Fixture {
            texts: BTreeMap::from([
                (format!("{CLAUDE_RELEASES}/stable"), "9.9.9\n".to_owned()),
                (
                    format!("{CLAUDE_RELEASES}/9.9.9/manifest.json"),
                    manifest.to_string(),
                ),
            ]),
            files: BTreeMap::from([(
                format!("{CLAUDE_RELEASES}/9.9.9/linux-x64/claude"),
                binary.to_vec(),
            )]),
            asked: Mutex::default(),
        }
    }

    #[test]
    fn claude_installs_from_the_manifest_and_is_removed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let binary = b"#!/bin/sh\necho 9.9.9 (Claude Code)\n";
        let f = claude_fixture(binary, &sha(binary));
        let release = latest(&f, Tool::Claude, LINUX).expect("release");
        assert_eq!(release.version, "9.9.9");
        assert_eq!(
            release.url,
            format!("{CLAUDE_RELEASES}/9.9.9/linux-x64/claude")
        );
        let mut said = Vec::new();
        let got =
            install(&f, dir.path(), &release, &mut |s| said.push(s.to_owned())).expect("installed");
        assert_eq!(std::fs::read(&got.path).expect("read"), binary);
        assert_eq!(installed(dir.path(), Tool::Claude), Some(got.clone()));
        assert!(said.iter().any(|s| s == "checksum verified"));
        // Another platform is not in this manifest's fixture download.
        let mac = Platform {
            os: "macos",
            arch: "aarch64",
            musl: false,
        };
        assert!(latest(&f, Tool::Claude, mac).is_ok());
        let musl = Platform {
            musl: true,
            ..LINUX
        };
        assert!(matches!(
            latest(&f, Tool::Claude, musl),
            Err(AgentError::Unsupported(..))
        ));
        assert!(remove(dir.path(), Tool::Claude).expect("removed"));
        assert_eq!(installed(dir.path(), Tool::Claude), None);
        assert!(!dir.path().join("agents/claude").exists());
    }

    #[test]
    fn a_bad_checksum_installs_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = claude_fixture(b"tampered", &sha(b"the real one"));
        let release = latest(&f, Tool::Claude, LINUX).expect("release");
        let err = install(&f, dir.path(), &release, &mut |_| {}).expect_err("refused");
        assert!(matches!(err, AgentError::Checksum { .. }), "{err}");
        assert_eq!(installed(dir.path(), Tool::Claude), None);
        assert!(!dir.path().join("agents/claude/9.9.9").exists());
        // A manifest that is not JSON, a version that is not one.
        let mut bad = claude_fixture(b"x", &sha(b"x"));
        bad.texts
            .insert(format!("{CLAUDE_RELEASES}/stable"), "<html>".to_owned());
        assert!(matches!(
            latest(&bad, Tool::Claude, LINUX),
            Err(AgentError::Release(_))
        ));
        // A version asked for: no channel, and only plain names.
        let f = claude_fixture(b"x", &sha(b"x"));
        let r = super::release(&f, Tool::Claude, LINUX, Some("9.9.9")).expect("pinned");
        assert_eq!(r.version, "9.9.9");
        assert!(
            !f.asked
                .lock()
                .expect("asked")
                .iter()
                .any(|u| u.ends_with("/stable"))
        );
        for v in ["../../etc", "latest", "", "9.9.9/linux-x64"] {
            assert!(
                super::release(&f, Tool::Claude, LINUX, Some(v)).is_err(),
                "{v}"
            );
        }
    }

    fn codex_release_json(name: &str, digest: Option<&str>) -> String {
        let mut asset = serde_json::json!({
            "name": name,
            "browser_download_url": format!("https://github.com/openai/codex/releases/download/rust-v0.166.0/{name}"),
        });
        if let Some(d) = digest {
            asset["digest"] = Value::from(d);
        }
        serde_json::json!({
            "tag_name": "rust-v0.166.0",
            "assets": [
                {
                    "name": "codex-responses-api-proxy-x86_64-unknown-linux-musl.tar.gz",
                    "browser_download_url": "https://github.com/x",
                    "digest": format!("sha256:{}", "1".repeat(64)),
                },
                asset,
            ]
        })
        .to_string()
    }

    #[test]
    fn codex_comes_from_the_github_release_with_its_digest() {
        let f = Fixture {
            texts: BTreeMap::from([(
                CODEX_LATEST.to_owned(),
                codex_release_json(
                    "codex-x86_64-unknown-linux-musl.tar.gz",
                    Some(&format!("sha256:{}", "A".repeat(64))),
                ),
            )]),
            files: BTreeMap::new(),
            asked: Mutex::default(),
        };
        let r = latest(&f, Tool::Codex, LINUX).expect("release");
        assert_eq!(r.version, "0.166.0");
        assert_eq!(r.packing, Packing::TarGz);
        assert_eq!(r.sha256, "a".repeat(64));
        assert!(r.url.ends_with("/codex-x86_64-unknown-linux-musl.tar.gz"));
        // A version asked for is looked up by its tag.
        let err = super::release(&f, Tool::Codex, LINUX, Some("0.1.0")).expect_err("not recorded");
        assert!(err.to_string().contains("tags/rust-v0.1.0"), "{err}");
        // No digest: no unchecked download.
        let f = Fixture {
            texts: BTreeMap::from([(
                CODEX_LATEST.to_owned(),
                codex_release_json("codex-x86_64-unknown-linux-musl.tar.gz", None),
            )]),
            files: BTreeMap::new(),
            asked: Mutex::default(),
        };
        let err = latest(&f, Tool::Codex, LINUX).expect_err("refused");
        assert!(err.to_string().contains("no SHA-256"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn codex_is_unpacked_from_its_archive() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).expect("mkdir");
        // A helper program that sorts first must not be taken for it.
        std::fs::write(src.join("codex-a-helper"), b"#!/bin/sh\necho helper\n").expect("write");
        std::fs::write(
            src.join("codex-x86_64-unknown-linux-musl"),
            b"#!/bin/sh\necho codex-cli 0.166.0\n",
        )
        .expect("write");
        let archive = dir.path().join("codex.tar.gz");
        let ok = Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&src)
            .args(["codex-a-helper", "codex-x86_64-unknown-linux-musl"])
            .status()
            .expect("tar");
        assert!(ok.success());
        let bytes = std::fs::read(&archive).expect("read");
        let name = "codex-x86_64-unknown-linux-musl.tar.gz";
        let release_json = codex_release_json(name, Some(&format!("sha256:{}", sha(&bytes))));
        let url = format!("https://github.com/openai/codex/releases/download/rust-v0.166.0/{name}");
        let f = Fixture {
            texts: BTreeMap::from([(CODEX_LATEST.to_owned(), release_json)]),
            files: BTreeMap::from([(url, bytes)]),
            asked: Mutex::default(),
        };
        let data = dir.path().join("data");
        let r = latest(&f, Tool::Codex, LINUX).expect("release");
        let got = install(&f, &data, &r, &mut |_| {}).expect("installed");
        assert!(got.path.ends_with("0.166.0/codex"));
        // Just written: briefly "busy" to run while another test thread
        // forks; only that is retried.
        let out = (0..50)
            .find_map(|_| match Command::new(&got.path).output() {
                Err(e) if e.kind() == io::ErrorKind::ExecutableFileBusy => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    None
                }
                other => Some(other),
            })
            .expect("settled")
            .expect("runs");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "codex-cli 0.166.0\n");
    }

    #[test]
    fn mcp_registration() {
        let termos = Path::new("/opt/termos/termos");
        let lines = mcp_register(Tool::Claude, Path::new("/d/claude"), termos).expect("claude");
        assert_eq!(
            lines[1].join(" "),
            "/d/claude mcp add --scope user termos -- /opt/termos/termos mcp serve"
        );
        assert!(mcp_register(Tool::Codex, Path::new("/d/codex"), termos).is_none());
        let before = "# my settings\nmodel = \"fixture-model\"\n\n[mcp_servers.other]\ncommand = \"other\"\n";
        let after = codex_config(before, termos).expect("edited");
        assert!(
            after.starts_with("# my settings\nmodel = \"fixture-model\""),
            "{after}"
        );
        assert!(after.contains("[mcp_servers.other]"));
        assert!(after.contains("[mcp_servers.termos]\ncommand = \"/opt/termos/termos\"\nargs = [\"mcp\", \"serve\"]\nenv_vars = [\"TERMOS\", \"TERMOS_PANE\""), "{after}");
        // Again: the same, not twice.
        let again = codex_config(&after, termos).expect("edited");
        assert_eq!(again.matches("[mcp_servers.termos]").count(), 1);
        assert!(codex_config("not = [toml", termos).is_err());
        assert!(registered(Tool::Codex, &after));
        let removed = codex_config_without(&after).expect("removed");
        assert!(!registered(Tool::Codex, &removed));
        assert!(removed.contains("[mcp_servers.other]"), "{removed}");
        assert!(registered(
            Tool::Claude,
            r#"{"numStartups": 3, "mcpServers": {"termos": {"type": "stdio", "command": "/t"}}}"#
        ));
        assert!(!registered(Tool::Claude, r#"{"mcpServers": {}}"#));
        assert!(!registered(Tool::Claude, "not json"));
        let env = |k: &str| (k == "CODEX_HOME").then(|| "/c".to_owned());
        assert_eq!(
            codex_home(&env, Some(Path::new("/h"))),
            Some(PathBuf::from("/c"))
        );
        assert_eq!(
            claude_settings(&env, Some(Path::new("/h"))),
            Some(PathBuf::from("/h/.claude.json"))
        );
    }
}
