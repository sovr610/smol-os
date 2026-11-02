// SPDX-License-Identifier: GPL-3.0-or-later
//! `termos update` and `termos system` (D-077, D-078): A/B updates on TermOS
//! images.
//!
//! `termos update` runs as the desktop's user. It reads what the image is
//! (`/etc/termos/board`, `/etc/termos/version`), asks GitHub Releases for a
//! newer bundle only when told to, downloads it into the user's data folder
//! and hands it to `termos system`, which sudo runs as root with one rule
//! (`termos ALL=(root) NOPASSWD: /usr/bin/termos system *`). `termos system`
//! does a fixed set of things with checked arguments: RAUC installs the
//! bundle into the other root slot, after checking its signature against the
//! image's keyring and its `compatible` against this board.

use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context as _, bail};
use clap::Subcommand;
use futures_util::StreamExt as _;
use termos_core::paths::Paths;

macro_rules! out {
    ($($arg:tt)*) => {
        writeln!(std::io::stdout(), $($arg)*)?
    };
}

/// Where releases come from (D-077).
const REPOSITORY: &str = "sovr610/smol-os";
/// The binary sudo's rule names; `termos update` asks for it by this path.
const SYSTEM_TERMOS: &str = "/usr/bin/termos";

#[derive(Debug, Subcommand)]
pub(crate) enum UpdateCmd {
    /// What runs now: the version, the board, the slot, and RAUC's view of
    /// both slots.
    Status,
    /// Ask GitHub Releases whether a newer TermOS exists for this board.
    Check,
    /// Download an update and install it into the other slot.
    Install {
        /// A bundle (`.raucb`): a file or a URL. By default, the newest
        /// release's bundle for this board.
        bundle: Option<String>,
        /// Install without asking first.
        #[arg(long)]
        yes: bool,
        /// Restart into the update once it is installed.
        #[arg(long)]
        restart: bool,
    },
    /// Restart the machine (into a newly installed update, if any).
    Restart,
}

#[derive(Debug, Subcommand)]
pub(crate) enum SystemCmd {
    /// Install a downloaded bundle with RAUC.
    InstallBundle { bundle: PathBuf },
    /// RAUC's view of the slots.
    Status,
    /// Restart the machine.
    Restart,
    /// Turn the machine off.
    PowerOff,
}

/// What this TermOS image is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Image {
    pub(crate) board: String,
    pub(crate) version: String,
    /// The root slot it booted from (`A` or `B`); none when it runs from
    /// the live ISO.
    pub(crate) slot: Option<String>,
}

impl Image {
    /// Reads `/etc/termos/*` and the kernel's command line under `root`.
    pub(crate) fn read(root: &Path) -> anyhow::Result<Self> {
        let read = |p: &str| std::fs::read_to_string(root.join(p)).map(|s| s.trim().to_owned());
        let board = read("etc/termos/board").map_err(|_| {
            anyhow::anyhow!(
                "this is not a TermOS image, so `termos update` has nothing to update; \
                 a TermOS installed in another system updates with a new release"
            )
        })?;
        let version = read("etc/termos/version").context("etc/termos/version")?;
        let slot = read("proc/cmdline").ok().and_then(|c| booted_slot(&c));
        Ok(Self {
            board,
            version,
            slot,
        })
    }

    /// The image this process runs in (`TERMOS_IMAGE_ROOT` moves `/`, for
    /// tests).
    fn detect() -> anyhow::Result<Self> {
        let root =
            std::env::var_os("TERMOS_IMAGE_ROOT").map_or_else(|| PathBuf::from("/"), PathBuf::from);
        Self::read(&root)
    }

    /// The slot an update goes into.
    fn other_slot(&self) -> Option<&'static str> {
        match self.slot.as_deref() {
            Some("A") => Some("B"),
            Some("B") => Some("A"),
            _ => None,
        }
    }
}

/// `rauc.slot=` from a kernel command line.
pub(crate) fn booted_slot(cmdline: &str) -> Option<String> {
    cmdline
        .split_whitespace()
        .find_map(|w| w.strip_prefix("rauc.slot="))
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// The numeric parts of a version (`v0.10.2+test` → `[0, 10, 2]`).
fn version_parts(v: &str) -> Vec<u64> {
    let v = v.trim().trim_start_matches('v');
    let core = v.split(['+', '-']).next().unwrap_or("");
    core.split('.').map_while(|p| p.parse().ok()).collect()
}

/// Whether `candidate` is newer than `current`.
pub(crate) fn newer(candidate: &str, current: &str) -> bool {
    version_parts(candidate) > version_parts(current)
}

/// A release's bundle for one board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Offer {
    pub(crate) version: String,
    pub(crate) name: String,
    pub(crate) url: String,
}

/// The bundle for `board` in a GitHub release (`releases/latest`'s JSON):
/// the asset named `termos-<board>-<version>.raucb`.
pub(crate) fn offer(release: &serde_json::Value, board: &str) -> Option<Offer> {
    let version = release["tag_name"]
        .as_str()?
        .trim_start_matches('v')
        .to_owned();
    let want = format!("termos-{board}-{version}.raucb");
    release["assets"].as_array()?.iter().find_map(|a| {
        let name = a["name"].as_str()?;
        if name != want {
            return None;
        }
        Some(Offer {
            version: version.clone(),
            name: name.to_owned(),
            url: a["browser_download_url"].as_str()?.to_owned(),
        })
    })
}

/// A bundle path `termos system` accepts: absolute, plain (no `..`), a
/// `.raucb` file.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn check_bundle_path(p: &Path) -> anyhow::Result<()> {
    if !p.is_absolute() || p.components().any(|c| matches!(c, Component::ParentDir)) {
        bail!("{}: give the bundle's full path", p.display());
    }
    if p.extension().is_none_or(|e| e != "raucb") {
        bail!("{}: not a RAUC bundle (.raucb)", p.display());
    }
    let meta = std::fs::metadata(p).with_context(|| p.display().to_string())?;
    if !meta.is_file() {
        bail!("{}: not a file", p.display());
    }
    Ok(())
}

fn rt() -> anyhow::Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?)
}

/// The newest release's bundle for `board`.
async fn latest(board: &str) -> anyhow::Result<Option<Offer>> {
    let api =
        std::env::var("TERMOS_UPDATE_API").unwrap_or_else(|_| "https://api.github.com".into());
    let url = format!(
        "{}/repos/{REPOSITORY}/releases/latest",
        api.trim_end_matches('/')
    );
    let resp = termos_models::hub::client()?
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .with_context(|| format!("asking {url}"))?;
    if resp.status().as_u16() == 404 {
        return Ok(None);
    }
    if !resp.status().is_success() {
        bail!("{url}: {}", resp.status());
    }
    let release: serde_json::Value = resp.json().await.context("reading the release")?;
    Ok(offer(&release, board))
}

/// Downloads `url` into `dir`, showing progress; returns the file.
async fn download(url: &str, dir: &Path) -> anyhow::Result<PathBuf> {
    let name = url
        .rsplit('/')
        .next()
        .filter(|n| n.ends_with(".raucb") && !n.contains(['?', '#']))
        .unwrap_or("update.raucb");
    std::fs::create_dir_all(dir).with_context(|| dir.display().to_string())?;
    let file = dir.join(name);
    let part = dir.join(format!("{name}.part"));
    let resp = termos_models::hub::client()?
        .get(url)
        .send()
        .await
        .with_context(|| format!("downloading {url}"))?;
    if !resp.status().is_success() {
        bail!("{url}: {}", resp.status());
    }
    let total = resp.content_length();
    let mut out = std::fs::File::create(&part).with_context(|| part.display().to_string())?;
    let mut got = 0u64;
    let mut shown = 0u64;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.with_context(|| format!("downloading {url}"))?;
        out.write_all(&chunk)?;
        got += chunk.len() as u64;
        if let Some(total) = total.filter(|t| *t > 0) {
            let pct = got * 100 / total;
            if pct >= shown + 10 {
                shown = pct - pct % 10;
                out!("  {shown}% of {} MB", total / 1_000_000);
            }
        }
    }
    out.sync_all()?;
    std::fs::rename(&part, &file)?;
    Ok(file)
}

/// Runs `termos system <args>` as root, through sudo's rule.
fn as_root(args: &[&str]) -> anyhow::Result<()> {
    let status = Command::new("sudo")
        .arg("-n")
        .arg(SYSTEM_TERMOS)
        .arg("system")
        .args(args)
        .status()
        .context("running sudo")?;
    if !status.success() {
        bail!("`termos system {}` failed ({status})", args.join(" "));
    }
    Ok(())
}

fn ask(question: &str) -> bool {
    print!("{question} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    let _ = std::io::stdin().read_line(&mut answer);
    matches!(answer.trim(), "y" | "Y" | "yes")
}

pub(crate) fn update_command(cmd: &UpdateCmd, paths: &Paths) -> anyhow::Result<ExitCode> {
    let image = Image::detect()?;
    match cmd {
        UpdateCmd::Status => {
            out!(
                "TermOS {} on {}, running from {}",
                image.version,
                image.board,
                image
                    .slot
                    .as_deref()
                    .map_or_else(|| "the live ISO".to_owned(), |s| format!("slot {s}"))
            );
            if image.slot.is_some() {
                as_root(&["status"])?;
            }
        }
        UpdateCmd::Check => match rt()?.block_on(latest(&image.board))? {
            Some(o) if newer(&o.version, &image.version) => {
                out!(
                    "TermOS {} is available (this is {}): termos update install",
                    o.version,
                    image.version
                );
            }
            Some(o) => out!(
                "TermOS {} is up to date (the newest release is {}).",
                image.version,
                o.version
            ),
            None => out!("No release has an update for {} yet.", image.board),
        },
        UpdateCmd::Install {
            bundle,
            yes,
            restart,
        } => {
            let Some(other) = image.other_slot() else {
                bail!("this system runs from the live ISO: install it with `termos install` first");
            };
            let rt = rt()?;
            let (source, what) = match bundle {
                Some(b) => (b.clone(), format!("the update in {b}")),
                None => match rt.block_on(latest(&image.board))? {
                    Some(o) if newer(&o.version, &image.version) => (
                        o.url,
                        format!("TermOS {} (this is {})", o.version, image.version),
                    ),
                    Some(_) => {
                        out!("TermOS {} is up to date.", image.version);
                        return Ok(ExitCode::SUCCESS);
                    }
                    None => bail!("no release has an update for {} yet", image.board),
                },
            };
            if !yes && !ask(&format!("Install {what} into slot {other}?")) {
                return Ok(ExitCode::from(1));
            }
            let file = if source.starts_with("https://") || source.starts_with("http://") {
                out!("Downloading {source}");
                rt.block_on(download(&source, &paths.data_dir.join("updates")))?
            } else {
                std::path::absolute(&source).with_context(|| source.clone())?
            };
            out!("Installing into slot {other}");
            as_root(&["install-bundle", &file.to_string_lossy()])?;
            if source.starts_with("http") {
                let _ = std::fs::remove_file(&file);
            }
            out!("TERMOS-UPDATE installed into slot {other}");
            if *restart {
                as_root(&["restart"])?;
            } else {
                out!("Restart to use it: termos update restart");
            }
        }
        UpdateCmd::Restart => as_root(&["restart"])?,
    }
    Ok(ExitCode::SUCCESS)
}

/// `termos system`: the root side, run through sudo.
pub(crate) fn system_command(cmd: &SystemCmd) -> anyhow::Result<ExitCode> {
    #[cfg(unix)]
    if !rustix::process::geteuid().is_root() {
        bail!("`termos system` runs as root, through sudo; use `termos update`");
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = cmd;
        bail!("`termos system` exists on TermOS images (Linux)");
    }
    #[cfg(target_os = "linux")]
    {
        let run = |program: &str, args: &[&std::ffi::OsStr]| -> anyhow::Result<ExitCode> {
            let status = Command::new(program)
                .args(args)
                .status()
                .with_context(|| format!("running {program}"))?;
            Ok(if status.success() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            })
        };
        match cmd {
            SystemCmd::InstallBundle { bundle } => {
                check_bundle_path(bundle)?;
                run("rauc", &["install".as_ref(), bundle.as_os_str()])
            }
            SystemCmd::Status => run("rauc", &["status".as_ref(), "--detailed".as_ref()]),
            SystemCmd::Restart => run("reboot", &[]),
            SystemCmd::PowerOff => run("poweroff", &[]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_and_versions() {
        assert_eq!(
            booted_slot("BOOT_IMAGE=/boot/bzImage root=PARTUUID=7e57c0de-03 rauc.slot=B quiet")
                .as_deref(),
            Some("B")
        );
        assert_eq!(booted_slot("quiet termos.live"), None);
        assert!(newer("0.2.0", "0.1.9"));
        assert!(newer("v0.10.0", "0.9.3"));
        assert!(newer("1.0", "0.99.99"));
        assert!(!newer("0.1.0", "0.1.0+test-update"));
        assert!(!newer("0.1.0", "0.2.0"));
    }

    #[test]
    fn the_image_is_read_from_its_files() {
        let dir = tempfile::tempdir().expect("dir");
        let err = Image::read(dir.path()).expect_err("not an image");
        assert!(err.to_string().contains("not a TermOS image"), "{err}");
        std::fs::create_dir_all(dir.path().join("etc/termos")).expect("mkdir");
        std::fs::create_dir_all(dir.path().join("proc")).expect("mkdir");
        std::fs::write(dir.path().join("etc/termos/board"), "x86_64\n").expect("write");
        std::fs::write(dir.path().join("etc/termos/version"), "0.1.0\n").expect("write");
        std::fs::write(dir.path().join("proc/cmdline"), "root=x rauc.slot=A\n").expect("write");
        let image = Image::read(dir.path()).expect("image");
        assert_eq!(
            image,
            Image {
                board: "x86_64".into(),
                version: "0.1.0".into(),
                slot: Some("A".into())
            }
        );
        assert_eq!(image.other_slot(), Some("B"));
    }

    #[test]
    fn a_release_offers_this_boards_bundle() {
        let release = serde_json::json!({
            "tag_name": "v0.2.0",
            "assets": [
                {"name": "termos-rpi4-0.2.0.raucb", "browser_download_url": "https://x/rpi4"},
                {"name": "termos-x86_64-0.2.0.raucb", "browser_download_url": "https://x/x86"},
                {"name": "termos-x86_64-0.2.0.iso", "browser_download_url": "https://x/iso"}
            ]
        });
        assert_eq!(
            offer(&release, "x86_64"),
            Some(Offer {
                version: "0.2.0".into(),
                name: "termos-x86_64-0.2.0.raucb".into(),
                url: "https://x/x86".into()
            })
        );
        assert_eq!(offer(&release, "aarch64-virt"), None);
        assert_eq!(
            offer(&serde_json::json!({"message": "Not Found"}), "x86_64"),
            None
        );
    }

    #[test]
    fn system_takes_only_full_paths_to_bundles() {
        let dir = tempfile::tempdir().expect("dir");
        let bundle = dir.path().join("u.raucb");
        std::fs::write(&bundle, b"x").expect("write");
        assert!(check_bundle_path(&bundle).is_ok());
        assert!(check_bundle_path(Path::new("u.raucb")).is_err());
        assert!(check_bundle_path(&dir.path().join("../x/u.raucb")).is_err());
        assert!(check_bundle_path(&dir.path().join("u.tar")).is_err());
        assert!(check_bundle_path(dir.path()).is_err());
        assert!(check_bundle_path(&dir.path().join("missing.raucb")).is_err());
    }
}
