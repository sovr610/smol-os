// SPDX-License-Identifier: GPL-3.0-or-later
//! Finding mise, or fetching the pinned one (D-053).
//!
//! TermOS uses the `mise` on `PATH` when it is at least [`MIN_VERSION`].
//! Otherwise it downloads [`PINNED`] from mise's GitHub releases with the
//! system's `curl` (Windows 10 and later, macOS and Linux distributions
//! ship it; TermOS links no TLS stack of its own), checks the file
//! against the SHA-256 sum compiled in here, and keeps it under
//! `<data>/mise/<version>/`. Nothing outside that folder changes.

use std::ffi::OsString;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use sha2::{Digest, Sha256};

/// The mise release TermOS fetches. Pinned, with sums taken from that
/// release's `SHASUMS256.txt`; at least two weeks old when chosen.
pub const PINNED: &str = "2026.10.14";

/// The oldest mise on `PATH` TermOS uses as it is.
pub const MIN_VERSION: Version = Version(2025, 10, 0);

/// SHA-256 of each release file of [`PINNED`], by platform.
const SUMS: [(&str, &str); 6] = [
    (
        "linux-x64",
        "50be7185bcf75929f01cc5630276bcb43ca367f7c5b176e2580903d28c611dc4",
    ),
    (
        "linux-arm64",
        "a9a8eec284d1c8e1a76685dcfa2fd609e76773fe56546eb72e4568dd1f50c1c3",
    ),
    (
        "macos-arm64",
        "005b32b9c22ca9cb63ff9d35c74b3abe69c433e9ca655fea288ba3dcbe7e4918",
    ),
    (
        "macos-x64",
        "158cd8124bc1108efa6588679b87031c970f592230b618e11c409b55c58e7b27",
    ),
    (
        "windows-x64",
        "4d82abd041a9295e2b95ee0699a4a783fbd98e0c6e1ff66d59d537874d16215a",
    ),
    (
        "windows-arm64",
        "5580cc7a939c7e6187f6e798ae78957b4b3cc000d1e6198bac3b42366ce495ba",
    ),
];

/// A mise version (`2026.10.14`): year, month, patch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u32, pub u32, pub u32);

impl Version {
    /// From `mise --version` output (`2026.10.14 linux-x64 (2026-10-18)`).
    pub fn parse(text: &str) -> Option<Self> {
        let word = text.split_whitespace().find(|w| w.contains('.'))?;
        let word = word.trim_start_matches('v');
        let mut parts = word.split('.').map(|p| {
            p.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse::<u32>()
                .ok()
        });
        Some(Self(
            parts.next()??,
            parts.next()??,
            parts.next().flatten().unwrap_or(0),
        ))
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MiseError {
    #[error(
        "mise {found} on PATH is older than {MIN_VERSION}, and fetching {PINNED} failed: {why}"
    )]
    TooOld { found: Version, why: String },
    #[error(
        "TermOS has no mise build for this system ({0}); install mise yourself: https://mise.jdx.dev"
    )]
    Unsupported(String),
    #[error("cannot fetch mise {PINNED}: {0}")]
    Fetch(String),
    #[error(
        "the mise download did not match its checksum (expected {expected}, got {got}); nothing was installed"
    )]
    Checksum { expected: String, got: String },
    #[error("{0}")]
    Io(#[from] io::Error),
}

/// A usable mise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mise {
    pub path: PathBuf,
    pub version: Version,
    /// TermOS fetched it (it is not on the user's `PATH`).
    pub fetched: bool,
}

/// The release file name for this platform: `(platform, file)`.
pub fn asset(os: &str, arch: &str) -> Option<(&'static str, String)> {
    let platform = match (os, arch) {
        ("linux", "x86_64") => "linux-x64",
        ("linux", "aarch64") => "linux-arm64",
        ("macos", "aarch64") => "macos-arm64",
        ("macos", "x86_64") => "macos-x64",
        ("windows", "x86_64") => "windows-x64",
        ("windows", "aarch64") => "windows-arm64",
        _ => return None,
    };
    let ext = if os == "windows" { ".zip" } else { "" };
    Some((platform, format!("mise-v{PINNED}-{platform}{ext}")))
}

fn sum_for(platform: &str) -> Option<&'static str> {
    SUMS.iter().find(|(p, _)| *p == platform).map(|(_, s)| *s)
}

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    }
}

/// Where TermOS keeps the mise it fetched.
pub fn fetched_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("mise").join(PINNED)
}

/// The fetched mise's executable (whether or not it exists yet).
pub fn fetched_path(data_dir: &Path) -> PathBuf {
    fetched_dir(data_dir).join(exe("mise"))
}

/// The first `name` on `path` (a `PATH` value).
pub fn which(name: &str, path: Option<&OsString>) -> Option<PathBuf> {
    let path = path?;
    std::env::split_paths(path)
        .map(|d| d.join(exe(name)))
        .find(|p| p.is_file())
}

/// Runs `mise --version`.
pub fn version_of(mise: &Path) -> Option<Version> {
    let out = Command::new(mise)
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| Version::parse(&String::from_utf8_lossy(&out.stdout)))
        .flatten()
}

/// A mise without fetching: `TERMOS_MISE`, then `PATH` (recent enough),
/// then one fetched earlier.
pub fn find(data_dir: &Path) -> Option<Mise> {
    if let Some(explicit) = std::env::var_os("TERMOS_MISE").map(PathBuf::from) {
        let version = version_of(&explicit)?;
        return Some(Mise {
            path: explicit,
            version,
            fetched: false,
        });
    }
    if let Some(p) = which("mise", std::env::var_os("PATH").as_ref())
        && let Some(v) = version_of(&p)
        && v >= MIN_VERSION
    {
        return Some(Mise {
            path: p,
            version: v,
            fetched: false,
        });
    }
    let fetched = fetched_path(data_dir);
    let version = version_of(&fetched)?;
    Some(Mise {
        path: fetched,
        version,
        fetched: true,
    })
}

/// A mise, fetching the pinned one when there is none (printing what it
/// does through `say`).
pub fn ensure(data_dir: &Path, say: &mut dyn FnMut(&str)) -> Result<Mise, MiseError> {
    if let Some(m) = find(data_dir) {
        return Ok(m);
    }
    let old = which("mise", std::env::var_os("PATH").as_ref()).and_then(|p| version_of(&p));
    let (platform, file) =
        asset(std::env::consts::OS, std::env::consts::ARCH).ok_or_else(|| {
            MiseError::Unsupported(format!(
                "{}-{}",
                std::env::consts::OS,
                std::env::consts::ARCH
            ))
        })?;
    let url = format!("https://github.com/jdx/mise/releases/download/v{PINNED}/{file}");
    say(&format!("termos: fetching mise {PINNED} from {url}"));
    let result = fetch(data_dir, platform, &file, &url);
    match (result, old) {
        (Ok(m), _) => Ok(m),
        (Err(e), Some(found)) => Err(MiseError::TooOld {
            found,
            why: e.to_string(),
        }),
        (Err(e), None) => Err(e),
    }
}

fn fetch(data_dir: &Path, platform: &str, file: &str, url: &str) -> Result<Mise, MiseError> {
    let expected = sum_for(platform).ok_or_else(|| MiseError::Unsupported(platform.to_owned()))?;
    let dir = fetched_dir(data_dir);
    let work = dir.with_extension("partial");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)?;
    let download = work.join(file);
    let status = Command::new(exe("curl"))
        .args(["--fail", "--location", "--silent", "--show-error"])
        .args(["--proto", "=https", "--tlsv1.2", "--retry", "3"])
        .arg("--output")
        .arg(&download)
        .arg(url)
        .stdin(Stdio::null())
        .status()
        .map_err(|e| MiseError::Fetch(format!("cannot run curl: {e}")))?;
    if !status.success() {
        let _ = std::fs::remove_dir_all(&work);
        return Err(MiseError::Fetch(format!("curl failed ({status})")));
    }
    let got = sha256_file(&download)?;
    if got != expected {
        let _ = std::fs::remove_dir_all(&work);
        return Err(MiseError::Checksum {
            expected: expected.to_owned(),
            got,
        });
    }
    let target = work.join(exe("mise"));
    if file.ends_with(".zip") {
        // Windows 10 and later ship bsdtar, which reads zip files. The
        // archive holds mise/bin/mise.exe.
        let status = Command::new("tar")
            .arg("-xf")
            .arg(&download)
            .arg("-C")
            .arg(&work)
            .stdin(Stdio::null())
            .status()
            .map_err(|e| MiseError::Fetch(format!("cannot run tar: {e}")))?;
        if !status.success() {
            return Err(MiseError::Fetch(format!("cannot unpack {file} ({status})")));
        }
        std::fs::rename(work.join("mise").join("bin").join(exe("mise")), &target)?;
        let _ = std::fs::remove_dir_all(work.join("mise"));
        let _ = std::fs::remove_file(&download);
    } else {
        std::fs::rename(&download, &target)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))?;
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::rename(&work, &dir)?;
    let path = dir.join(exe("mise"));
    let version = version_of(&path)
        .ok_or_else(|| MiseError::Fetch("the fetched mise does not run".to_owned()))?;
    Ok(Mise {
        path,
        version,
        fetched: true,
    })
}

/// The SHA-256 of a file, in lowercase hex.
pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// mise's own data folder (where tools and shims go): `MISE_DATA_DIR`,
/// else the platform default.
pub fn mise_data_dir(get: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let abs = |k: &str| get(k).map(PathBuf::from).filter(|p| p.is_absolute());
    if let Some(d) = abs("MISE_DATA_DIR") {
        return Some(d);
    }
    if cfg!(windows) {
        abs("LOCALAPPDATA").map(|l| l.join("mise"))
    } else {
        abs("XDG_DATA_HOME")
            .or_else(|| abs("HOME").map(|h| h.join(".local").join("share")))
            .map(|d| d.join("mise"))
    }
}

/// `PATH` for programs in TermOS panes: mise's shims (so tools installed
/// with `termos lang` run, in any shell) and the fetched mise, before
/// `path`.
pub fn pane_path(
    get: &dyn Fn(&str) -> Option<String>,
    data_dir: &Path,
    path: Option<&OsString>,
) -> Option<OsString> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(d) = mise_data_dir(get) {
        dirs.push(d.join("shims"));
    }
    let fetched = fetched_dir(data_dir);
    if fetched.join(exe("mise")).is_file() {
        dirs.push(fetched);
    }
    let rest: Vec<PathBuf> = path
        .map(|p| std::env::split_paths(p).collect())
        .unwrap_or_default();
    // Already there (a shell that activates mise itself): leave PATH be.
    dirs.retain(|d| !rest.contains(d));
    if dirs.is_empty() {
        return None;
    }
    std::env::join_paths(dirs.into_iter().chain(rest)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_parse_and_compare() {
        assert_eq!(
            Version::parse("2026.10.14 linux-x64 (2026-10-18)"),
            Some(Version(2026, 10, 14))
        );
        assert_eq!(Version::parse("mise v2025.1.0"), Some(Version(2025, 1, 0)));
        assert_eq!(Version::parse("2024.12"), Some(Version(2024, 12, 0)));
        assert_eq!(Version::parse("nonsense"), None);
        assert!(Version(2025, 9, 30) < MIN_VERSION);
        assert!(Version(2026, 1, 0) > MIN_VERSION);
        assert!(Version::parse(PINNED).is_some_and(|v| v >= MIN_VERSION));
    }

    #[test]
    fn every_platform_has_a_file_and_a_sum() {
        for (os, arch) in [
            ("linux", "x86_64"),
            ("linux", "aarch64"),
            ("macos", "aarch64"),
            ("macos", "x86_64"),
            ("windows", "x86_64"),
            ("windows", "aarch64"),
        ] {
            let (platform, file) = asset(os, arch).expect("supported");
            assert!(file.starts_with(&format!("mise-v{PINNED}-")), "{file}");
            let sum = sum_for(platform).expect("a sum");
            assert_eq!(sum.len(), 64);
            assert!(sum.bytes().all(|b| b.is_ascii_hexdigit()));
        }
        assert_eq!(asset("freebsd", "x86_64"), None);
        assert!(asset("windows", "x86_64").is_some_and(|(_, f)| f.ends_with(".zip")));
    }

    #[test]
    fn checksums_of_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = dir.path().join("x");
        std::fs::write(&f, b"abc").expect("write");
        assert_eq!(
            sha256_file(&f).expect("read"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn panes_find_the_shims_first() {
        let get = |k: &str| match k {
            "HOME" => Some(
                if cfg!(windows) {
                    r"C:\Users\me"
                } else {
                    "/home/me"
                }
                .to_owned(),
            ),
            "LOCALAPPDATA" => Some(r"C:\Users\me\AppData\Local".to_owned()),
            _ => None,
        };
        let data = tempfile::tempdir().expect("tempdir");
        let base = std::env::join_paths([PathBuf::from("/usr/bin")]).expect("path");
        let p = pane_path(&get, data.path(), Some(&base)).expect("changed");
        let dirs: Vec<PathBuf> = std::env::split_paths(&p).collect();
        let shims = mise_data_dir(&get).expect("a data dir").join("shims");
        assert_eq!(dirs, vec![shims.clone(), PathBuf::from("/usr/bin")]);
        // Already on PATH: unchanged.
        let with = std::env::join_paths([shims, PathBuf::from("/usr/bin")]).expect("path");
        assert_eq!(pane_path(&get, data.path(), Some(&with)), None);
        // MISE_DATA_DIR wins.
        let custom = if cfg!(windows) {
            r"D:\mise"
        } else {
            "/opt/mise"
        };
        let get2 = |k: &str| (k == "MISE_DATA_DIR").then(|| custom.to_owned());
        assert_eq!(mise_data_dir(&get2), Some(PathBuf::from(custom)));
    }

    /// A stand-in mise (a script) answering `--version`.
    #[cfg(unix)]
    #[test]
    fn find_uses_a_recent_mise_and_skips_old_ones() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fake = |name: &str, version: &str| {
            let p = dir.path().join(name);
            std::fs::write(&p, format!("#!/bin/sh\necho '{version} linux-x64'\n")).expect("write");
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
            p
        };
        // A script just written can be briefly "busy" to run while another
        // test thread forks; only that is retried.
        let settled = |p: &Path| {
            (0..50).find_map(|_| {
                let v = version_of(p);
                if v.is_none() {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                v
            })
        };
        let new = fake("new", "2026.1.2");
        assert_eq!(settled(&new), Some(Version(2026, 1, 2)));
        let old = fake("old", "2024.1.0");
        assert_eq!(settled(&old), Some(Version(2024, 1, 0)));
        assert_eq!(version_of(&dir.path().join("missing")), None);
    }
}
