// SPDX-License-Identifier: GPL-3.0-or-later
//! The dev rootfs (M5b; D-109, D-110): Debian 13 from the official
//! `debian` image, pinned by digest, where compilers and system packages
//! are installed with apt, without the host's root.
//!
//! Two ways to run it:
//! - **bubblewrap** (Linux): the image's layer is downloaded with the
//!   system's `curl`, checked against its SHA-256 and unpacked into
//!   `<data>/devroot/rootfs` inside a user namespace that maps the user to
//!   root and the user's subordinate IDs to the others (`unshare` from
//!   util-linux 2.38 or newer, with shadow's `newuidmap`), so files keep
//!   their owners and apt works. Entering it is bubblewrap in the same
//!   kind of namespace, with the user's home bound at its own path.
//! - **Podman** (any host that has it): the same image, by digest, in a
//!   container named [`CONTAINER`].

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::mise::which;

/// The image a dev rootfs is made from.
#[derive(Debug)]
pub struct Image {
    /// What it is called.
    pub name: &'static str,
    /// When the official image was built.
    pub built: &'static str,
    /// The official image's index digest: what Podman pulls.
    pub index: &'static str,
    /// Its one layer, per architecture.
    pub layers: &'static [Layer],
}

/// One architecture's layer of the official image.
#[derive(Debug)]
pub struct Layer {
    /// Debian's name for the architecture.
    pub arch: &'static str,
    /// The commit of the repository the official image is built from
    /// (debuerreotype/docker-debian-artifacts) that holds the layer.
    pub commit: &'static str,
    /// The layer's digest: the SHA-256 of its `.tar.gz`.
    pub sha256: &'static str,
    /// Its size in bytes.
    pub size: u64,
}

impl Layer {
    /// Where it is downloaded from: the same bytes as the registry's blob,
    /// without the registry's limits on anonymous pulls (D-109).
    pub fn url(&self) -> String {
        format!(
            "https://raw.githubusercontent.com/debuerreotype/docker-debian-artifacts/{}/trixie/oci/blobs/rootfs.tar.gz",
            self.commit
        )
    }
}

/// The pinned image: `debian:trixie` as built on 2026-10-27. A newer one
/// comes with a TermOS release; `termos devroot update` upgrades the
/// packages in between.
pub const DEBIAN: Image = Image {
    name: "Debian 13 (trixie)",
    built: "2026-10-27",
    index: "sha256:2dc419c6d7fe3dad0a2f9fd469c34cc90b0f9ba642d8fde837fe3d7d34a0e7f1",
    layers: &[
        Layer {
            arch: "amd64",
            commit: "69faaf492e77aff83e4c7fc401017c9d25ecde24",
            sha256: "a1d156b0aab5fb07d822a7f4681f6d3ed1689073531c34f5668818c233633f11",
            size: 49_407_895,
        },
        Layer {
            arch: "arm64",
            commit: "e36653f9967231373aeba7cff2102fd004ac4f4a",
            sha256: "dfb68c024d5cdf4872bb7f0be8d63874aa6d0b7a0050f1b06169573e34ea543a",
            size: 49_776_179,
        },
    ],
};

/// The layer for Rust's name of an architecture (`std::env::consts::ARCH`).
pub fn layer(arch: &str) -> Option<&'static Layer> {
    let debian = match arch {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        _ => return None,
    };
    DEBIAN.layers.iter().find(|l| l.arch == debian)
}

/// What Podman pulls: the official image by its index digest.
pub fn podman_image() -> String {
    format!("docker.io/library/debian@{}", DEBIAN.index)
}

/// Podman's container.
pub const CONTAINER: &str = "termos-devroot";

/// What `termos devroot update` runs inside: apt's upgrade, keeping
/// changed configuration files without asking.
pub const UPDATE: &str = "apt-get update && DEBIAN_FRONTEND=noninteractive apt-get -y \
     -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold upgrade";

/// What `termos devroot create` runs inside once, so packages can be
/// searched and installed at once.
pub const LISTS: &str = "apt-get update";

/// How a dev rootfs runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    /// Unpacked in TermOS's data folder, entered with bubblewrap.
    Bubblewrap,
    /// Podman's container [`CONTAINER`].
    Podman,
}

impl Backend {
    /// Its name, as `termos devroot status` shows it.
    pub fn name(self) -> &'static str {
        match self {
            Backend::Bubblewrap => "bubblewrap",
            Backend::Podman => "Podman",
        }
    }
}

/// What TermOS records about the dev rootfs (`<data>/devroot/devroot.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// How it runs.
    pub backend: Backend,
    /// What it was made from: the image's name, or the file it came from.
    pub image: String,
    /// The layer's SHA-256 (bubblewrap) or the image's reference (Podman).
    pub source: String,
    /// When it was made, in seconds since 1970.
    pub created: u64,
    /// When `termos devroot update` last succeeded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<u64>,
}

/// TermOS's folder for the dev rootfs.
pub fn dir(data_dir: &Path) -> PathBuf {
    data_dir.join("devroot")
}

/// The unpacked root file system (bubblewrap).
pub fn rootfs(data_dir: &Path) -> PathBuf {
    dir(data_dir).join("rootfs")
}

fn state_path(data_dir: &Path) -> PathBuf {
    dir(data_dir).join("devroot.json")
}

/// The recorded state, if a dev rootfs was made.
pub fn load(data_dir: &Path) -> Option<State> {
    let text = std::fs::read_to_string(state_path(data_dir)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Records `state`.
pub fn save(data_dir: &Path, state: &State) -> io::Result<()> {
    let path = state_path(data_dir);
    std::fs::create_dir_all(dir(data_dir))?;
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(state).map_err(io::Error::other)?;
    std::fs::write(&tmp, text + "\n")?;
    std::fs::rename(tmp, path)
}

/// Forgets the recorded state.
pub fn forget(data_dir: &Path) -> io::Result<()> {
    match std::fs::remove_file(state_path(data_dir)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Seconds since 1970.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Why the dev rootfs cannot be made or entered.
#[derive(Debug, thiserror::Error)]
pub enum DevrootError {
    #[error("{0}")]
    Unsupported(String),
    #[error("`{program}` is missing: install {package}")]
    Missing {
        program: &'static str,
        package: &'static str,
    },
    #[error(
        "unshare is {found}; the dev rootfs needs util-linux 2.38 or newer (`unshare --map-users`), or Podman (`termos devroot create --podman`)"
    )]
    OldUnshare { found: String },
    #[error(
        "{file} gives {user} no range of {ID_COUNT} IDs; add one, e.g. `sudo usermod --add-subuids 100000-165535 --add-subgids 100000-165535 {user}`"
    )]
    NoIds { file: &'static str, user: String },
    #[error("the download is {got} bytes, not {expected}")]
    Size { expected: u64, got: u64 },
    #[error("checksum mismatch: expected SHA-256 {expected}, got {got}; nothing was unpacked")]
    Checksum { expected: String, got: String },
    #[error("no dev rootfs yet: `termos devroot create` makes one")]
    NotCreated,
    #[error("there is a dev rootfs already ({0}); `termos devroot remove` deletes it")]
    Exists(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// How many IDs the namespace maps besides root: every ID a Debian
/// system uses, `nobody` (65534) included.
pub const ID_COUNT: u32 = 65_536;

/// A block of subordinate IDs (`/etc/subuid`, `/etc/subgid`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdRange {
    pub start: u32,
    pub count: u32,
}

/// The first block of at least [`ID_COUNT`] IDs in `/etc/subuid` or
/// `/etc/subgid`'s `text` for the user called `name` or numbered `uid`
/// (both files name users, as `newuidmap` and `newgidmap` read them).
pub fn sub_range(text: &str, name: &str, uid: u32) -> Option<IdRange> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let mut f = line.split(':');
            let who = f.next()?;
            let start = f.next()?.trim().parse().ok()?;
            let count = f.next()?.trim().parse().ok()?;
            (who == name || who.parse::<u32>().ok() == Some(uid))
                .then_some(IdRange { start, count })
        })
        .find(|r| r.count >= ID_COUNT)
}

/// The user's name for `uid` in `/etc/passwd`'s `text`.
pub fn user_name(passwd: &str, uid: u32) -> Option<String> {
    passwd.lines().find_map(|line| {
        let mut f = line.split(':');
        let name = f.next()?;
        let id: u32 = f.nth(1)?.parse().ok()?;
        (id == uid && !name.is_empty()).then(|| name.to_owned())
    })
}

/// The user and the IDs the namespace maps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ids {
    pub uid: u32,
    pub gid: u32,
    pub users: IdRange,
    pub groups: IdRange,
}

/// This user's IDs, from the system's files.
#[cfg(unix)]
pub fn ids() -> Result<Ids, DevrootError> {
    let read = |f: &str| std::fs::read_to_string(f).unwrap_or_default();
    let uid = rustix::process::getuid().as_raw();
    let gid = rustix::process::getgid().as_raw();
    let name = user_name(&read("/etc/passwd"), uid)
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| uid.to_string());
    let users = sub_range(&read("/etc/subuid"), &name, uid).ok_or_else(|| DevrootError::NoIds {
        file: "/etc/subuid",
        user: name.clone(),
    })?;
    let groups =
        sub_range(&read("/etc/subgid"), &name, uid).ok_or_else(|| DevrootError::NoIds {
            file: "/etc/subgid",
            user: name.clone(),
        })?;
    Ok(Ids {
        uid,
        gid,
        users,
        groups,
    })
}

/// This user's IDs: there are none to map off Unix.
#[cfg(not(unix))]
pub fn ids() -> Result<Ids, DevrootError> {
    Err(DevrootError::Unsupported(
        "the bubblewrap dev rootfs is Linux's; use Podman (`termos devroot create --podman`)"
            .to_owned(),
    ))
}

/// `unshare`'s arguments for a user namespace where the user is root and
/// the subordinate IDs are 1 to [`ID_COUNT`], up to and including `--`.
pub fn unshare_args(ids: &Ids) -> Vec<String> {
    vec![
        "--user".to_owned(),
        "--map-user=0".to_owned(),
        "--map-group=0".to_owned(),
        format!("--map-users=1:{}:{ID_COUNT}", ids.users.start),
        format!("--map-groups=1:{}:{ID_COUNT}", ids.groups.start),
        "--".to_owned(),
    ]
}

/// util-linux's version in `unshare --version`'s output.
pub fn util_linux_version(text: &str) -> Option<(u32, u32)> {
    let version = text.split("util-linux").nth(1)?.split_whitespace().next()?;
    let mut parts = version.split('.');
    let number = |s: &str| -> Option<u32> {
        let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    };
    Some((number(parts.next()?)?, number(parts.next()?)?))
}

/// The programs the bubblewrap way runs.
#[derive(Debug, Clone)]
pub struct Tools {
    pub unshare: PathBuf,
    pub bwrap: PathBuf,
}

/// Finds them on `path` (a `PATH` value), with `newuidmap` and `newgidmap`,
/// which `unshare` runs.
pub fn tools(path: Option<&OsString>) -> Result<Tools, DevrootError> {
    if !cfg!(target_os = "linux") {
        return Err(DevrootError::Unsupported(
            "the bubblewrap dev rootfs is Linux's; use Podman (`termos devroot create --podman`)"
                .to_owned(),
        ));
    }
    let need = |program: &'static str, package: &'static str| {
        which(program, path).ok_or(DevrootError::Missing { program, package })
    };
    let bwrap = need("bwrap", "bubblewrap")?;
    let unshare = need("unshare", "util-linux")?;
    need("newuidmap", "uidmap (Debian, Ubuntu) or shadow-utils")?;
    need("newgidmap", "uidmap (Debian, Ubuntu) or shadow-utils")?;
    let out = Command::new(&unshare).arg("--version").output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    match util_linux_version(&text) {
        Some(v) if v >= (2, 38) => Ok(Tools { unshare, bwrap }),
        _ => Err(DevrootError::OldUnshare {
            found: text.trim().to_owned(),
        }),
    }
}

/// Why user namespaces may be refused here, from the kernel's settings.
pub fn namespace_hint(read: &dyn Fn(&str) -> Option<String>) -> Option<&'static str> {
    let set = |path: &str, value: &str| read(path).is_some_and(|v| v.trim() == value);
    if set(
        "/proc/sys/kernel/apparmor_restrict_unprivileged_userns",
        "1",
    ) {
        Some(
            "this system restricts user namespaces through AppArmor (Ubuntu 23.10 and later): \
             `sudo sysctl kernel.apparmor_restrict_unprivileged_userns=0` lifts it, or use Podman \
             (`termos devroot create --podman`)",
        )
    } else if set("/proc/sys/kernel/unprivileged_userns_clone", "0") {
        Some("user namespaces are off: `sudo sysctl kernel.unprivileged_userns_clone=1`")
    } else if set("/proc/sys/user/max_user_namespaces", "0") {
        Some("user namespaces are off: `sudo sysctl user.max_user_namespaces=15000`")
    } else {
        None
    }
}

/// How a command runs inside.
#[derive(Debug, Clone)]
pub struct Inside<'a> {
    /// The unpacked root file system.
    pub rootfs: &'a Path,
    /// The user's home on the host, bound at the same path.
    pub home: &'a Path,
    /// The host's current folder.
    pub cwd: &'a Path,
    /// The whole environment inside ([`inside_env`]).
    pub env: &'a [(String, String)],
    /// The program and its arguments.
    pub command: &'a [String],
}

/// Folders of the host that are never bound over the dev rootfs's own.
const SYSTEM: &[&str] = &[
    "/bin", "/boot", "/dev", "/etc", "/lib", "/lib32", "/lib64", "/libx32", "/proc", "/root",
    "/run", "/sbin", "/sys", "/usr", "/var",
];

/// Whether the host's `cwd` gets its own bind: a folder outside the home
/// (a project in `/data` or `/tmp`) that is not one of the system's.
pub fn binds_cwd(home: &Path, cwd: &Path) -> bool {
    cwd.is_absolute()
        && cwd.parent().is_some()
        && !cwd.starts_with(home)
        && !SYSTEM.iter().any(|s| cwd.starts_with(s))
}

/// Where the command starts inside: the host's folder when it is there,
/// else the home.
pub fn start_dir<'a>(home: &'a Path, cwd: &'a Path) -> &'a Path {
    if cwd.starts_with(home) || binds_cwd(home, cwd) {
        cwd
    } else {
        home
    }
}

/// bubblewrap's arguments: the rootfs as `/`, its own `/dev`, `/proc`,
/// `/tmp` and `/run`, the host's `/sys`, DNS and hosts files read-only,
/// the home (and the current folder) read-write, the host's network, a
/// clean environment.
pub fn bwrap_args(i: &Inside<'_>) -> Vec<OsString> {
    let mut a: Vec<OsString> = Vec::new();
    let mut add = |items: &[&dyn AsRef<std::ffi::OsStr>]| {
        a.extend(items.iter().map(|s| s.as_ref().to_owned()));
    };
    add(&[&"--bind", &i.rootfs, &"/"]);
    add(&[&"--dev", &"/dev", &"--proc", &"/proc"]);
    add(&[&"--ro-bind", &"/sys", &"/sys"]);
    // `/tmp` for everyone, as on Debian: apt checks signatures as its
    // `_apt` user there (bubblewrap's tmpfs is 0755 otherwise).
    add(&[
        &"--perms", &"1777", &"--tmpfs", &"/tmp", &"--tmpfs", &"/run",
    ]);
    add(&[&"--ro-bind-try", &"/etc/resolv.conf", &"/etc/resolv.conf"]);
    add(&[&"--ro-bind-try", &"/etc/hosts", &"/etc/hosts"]);
    add(&[&"--bind", &i.home, &i.home]);
    if binds_cwd(i.home, i.cwd) {
        add(&[&"--bind", &i.cwd, &i.cwd]);
    }
    add(&[&"--chdir", &start_dir(i.home, i.cwd)]);
    add(&[&"--unshare-pid", &"--die-with-parent", &"--clearenv"]);
    for (k, v) in i.env {
        add(&[&"--setenv", k, v]);
    }
    a.push("--".into());
    a.extend(i.command.iter().map(OsString::from));
    a
}

/// The environment inside: root's, Debian's `PATH`, the terminal's type
/// and the proxies; nothing else of the host's.
pub fn inside_env(get: &dyn Fn(&str) -> Option<String>) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = [
        ("HOME", "/root"),
        ("USER", "root"),
        ("LOGNAME", "root"),
        ("SHELL", "/bin/bash"),
        (
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        ),
        ("LANG", "C.UTF-8"),
        ("TERMOS_DEVROOT", "1"),
        // Debian's prompt shows it: `(devroot)root@host:~#`.
        ("debian_chroot", "devroot"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    env.push((
        "TERM".to_owned(),
        get("TERM")
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "xterm-256color".to_owned()),
    ));
    for k in [
        "COLORTERM",
        "http_proxy",
        "https_proxy",
        "no_proxy",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
    ] {
        if let Some(v) = get(k).filter(|v| !v.is_empty()) {
            env.push((k.to_owned(), v));
        }
    }
    env
}

/// The shell `termos devroot enter` starts: a login bash, or `sh` in a
/// rootfs without bash.
pub fn shell(rootfs: Option<&Path>) -> Vec<String> {
    let bash = rootfs.is_none_or(|r| r.join("bin/bash").exists());
    if bash {
        vec!["/bin/bash".to_owned(), "-l".to_owned()]
    } else {
        vec!["/bin/sh".to_owned(), "-l".to_owned()]
    }
}

/// A host path as Podman's Linux sees it: the same on Linux and macOS,
/// `/mnt/c/…` for `C:\…` on Windows.
pub fn podman_path(host: &Path) -> String {
    let s = host.to_string_lossy().replace('\\', "/");
    let b = s.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        format!("/mnt/{}{}", char::from(b[0]).to_ascii_lowercase(), &s[2..])
    } else {
        s
    }
}

/// `podman create`'s arguments: the pinned image, idle until entered, the
/// home bound as in the bubblewrap way.
pub fn podman_create_args(home: &Path) -> Vec<String> {
    [
        "create",
        "--name",
        CONTAINER,
        "--label",
        "io.termos.devroot=1",
        // SELinux would keep the container out of the home.
        "--security-opt",
        "label=disable",
        // `sleep` ignores SIGTERM as the container's first process.
        "--stop-signal",
        "SIGKILL",
        "--volume",
        &format!("{}:{}", home.display(), podman_path(home)),
        &podman_image(),
        "sleep",
        "infinity",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

/// `podman exec`'s arguments for `command`, starting in `dir` (a path
/// inside).
pub fn podman_exec_args(
    tty: bool,
    dir: &str,
    env: &[(String, String)],
    command: &[String],
) -> Vec<String> {
    let mut a = vec!["exec".to_owned(), "--interactive".to_owned()];
    if tty {
        a.push("--tty".to_owned());
    }
    a.extend(["--workdir".to_owned(), dir.to_owned()]);
    for (k, v) in env {
        a.extend(["--env".to_owned(), format!("{k}={v}")]);
    }
    a.push(CONTAINER.to_owned());
    a.extend(command.iter().cloned());
    a
}

/// What [`unpack`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Unpacked {
    /// Entries written.
    pub entries: u64,
    /// Device nodes and pipes left out.
    pub skipped: u64,
}

/// Unpacks a rootfs `.tar.gz` (or `.tar`) into `dest`, keeping owners,
/// modes and times; run as root of the user namespace, so the owners are
/// the namespace's. Device nodes and pipes are left out: a user namespace
/// cannot make devices, and bubblewrap gives the rootfs its own `/dev`.
pub fn unpack(archive: &Path, dest: &Path) -> io::Result<Unpacked> {
    let mut file = File::open(archive)?;
    let mut magic = [0u8; 2];
    let gzip = file.read_exact(&mut magic).is_ok() && magic == [0x1f, 0x8b];
    let file = File::open(archive)?;
    let reader: Box<dyn Read> = if gzip {
        Box::new(flate2::read::GzDecoder::new(BufReader::new(file)))
    } else {
        Box::new(BufReader::new(file))
    };
    let mut archive = tar::Archive::new(reader);
    archive.set_preserve_permissions(true);
    archive.set_preserve_ownerships(true);
    archive.set_preserve_mtime(true);
    archive.set_overwrite(true);
    archive.set_unpack_xattrs(false);
    std::fs::create_dir_all(dest)?;
    let mut done = Unpacked::default();
    // Directories last, deepest first, as tar's own `unpack`: their modes
    // could otherwise keep their contents from being written.
    let mut dirs = Vec::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let kind = entry.header().entry_type();
        if kind.is_character_special() || kind.is_block_special() || kind.is_fifo() {
            done.skipped += 1;
        } else if kind.is_dir() {
            dirs.push(entry);
        } else {
            entry.unpack_in(dest)?;
            done.entries += 1;
        }
    }
    dirs.sort_by(|a, b| b.path_bytes().cmp(&a.path_bytes()));
    for mut d in dirs {
        d.unpack_in(dest)?;
        done.entries += 1;
    }
    Ok(done)
}

/// Whether `path` is one [`wipe`] may delete: the rootfs, or one being
/// unpacked, in a `devroot` folder.
pub fn wipeable(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str());
    let parent = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|n| n.to_str());
    matches!(name, Some("rootfs" | "rootfs.partial")) && parent == Some("devroot")
}

/// Deletes a rootfs; run as root of the user namespace, which owns its
/// files.
pub fn wipe(path: &Path) -> io::Result<()> {
    if !wipeable(path) {
        return Err(io::Error::other(format!(
            "{} is not a dev rootfs",
            path.display()
        )));
    }
    match std::fs::remove_dir_all(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layers_are_pinned_per_architecture() {
        let amd = layer("x86_64").expect("x86_64");
        assert_eq!(amd.arch, "amd64");
        assert_eq!(amd.sha256.len(), 64);
        assert!(amd.url().contains(amd.commit));
        assert!(amd.url().starts_with("https://"));
        assert_eq!(layer("aarch64").map(|l| l.arch), Some("arm64"));
        assert!(layer("riscv64").is_none());
        assert!(podman_image().starts_with("docker.io/library/debian@sha256:"));
    }

    #[test]
    fn subordinate_ids_by_name_or_number() {
        let text =
            "# comment\nalice:100000:65536\n1001:200000:65536\nbob:300000:1000\nbob:400000:70000\n";
        assert_eq!(
            sub_range(text, "alice", 1000),
            Some(IdRange {
                start: 100_000,
                count: 65_536
            })
        );
        assert_eq!(
            sub_range(text, "carol", 1001).map(|r| r.start),
            Some(200_000)
        );
        // Too small a block is passed over for the next.
        assert_eq!(sub_range(text, "bob", 1002).map(|r| r.start), Some(400_000));
        assert_eq!(sub_range(text, "dave", 1003), None);
        assert_eq!(sub_range("alice:x:y\n", "alice", 1), None);
        let passwd = "root:x:0:0:root:/root:/bin/bash\nalice:x:1000:1000::/home/alice:/bin/sh\n";
        assert_eq!(user_name(passwd, 1000).as_deref(), Some("alice"));
        assert_eq!(user_name(passwd, 7), None);
    }

    #[test]
    fn the_namespace_maps_root_and_the_subordinate_ids() {
        let ids = Ids {
            uid: 1000,
            gid: 1000,
            users: IdRange {
                start: 100_000,
                count: 65_536,
            },
            groups: IdRange {
                start: 200_000,
                count: 65_536,
            },
        };
        assert_eq!(
            unshare_args(&ids),
            [
                "--user",
                "--map-user=0",
                "--map-group=0",
                "--map-users=1:100000:65536",
                "--map-groups=1:200000:65536",
                "--"
            ]
        );
    }

    #[test]
    fn util_linux_versions() {
        assert_eq!(
            util_linux_version("unshare from util-linux 2.39.3\n"),
            Some((2, 39))
        );
        assert_eq!(
            util_linux_version("unshare from util-linux 2.40-rc1"),
            Some((2, 40))
        );
        assert_eq!(
            util_linux_version("unshare from util-linux 2.37.2"),
            Some((2, 37))
        );
        assert_eq!(util_linux_version("BusyBox v1.36.1"), None);
    }

    #[test]
    fn hints_name_the_setting_that_refuses() {
        let apparmor = |p: &str| {
            (p == "/proc/sys/kernel/apparmor_restrict_unprivileged_userns")
                .then(|| "1\n".to_owned())
        };
        assert!(namespace_hint(&apparmor).is_some_and(|h| h.contains("AppArmor")));
        let off = |p: &str| (p == "/proc/sys/user/max_user_namespaces").then(|| "0".to_owned());
        assert!(namespace_hint(&off).is_some_and(|h| h.contains("max_user_namespaces")));
        assert_eq!(
            namespace_hint(&|_| Some("1".to_owned())).map(|_| ()),
            Some(())
        );
        assert_eq!(namespace_hint(&|_| None), None);
    }

    #[cfg(unix)]
    fn strings(a: &[OsString]) -> Vec<String> {
        a.iter().map(|s| s.to_string_lossy().into_owned()).collect()
    }

    // bubblewrap is Linux's; Windows paths without a drive are not absolute.
    #[cfg(unix)]
    #[test]
    fn bubblewrap_binds_the_home_and_a_project_outside_it() {
        let env = vec![("HOME".to_owned(), "/root".to_owned())];
        let command = vec!["/bin/bash".to_owned(), "-l".to_owned()];
        let inside = Inside {
            rootfs: Path::new("/home/u/.local/share/termos/devroot/rootfs"),
            home: Path::new("/home/u"),
            cwd: Path::new("/home/u/src/app"),
            env: &env,
            command: &command,
        };
        let a = strings(&bwrap_args(&inside)).join(" ");
        assert!(a.starts_with(
            "--bind /home/u/.local/share/termos/devroot/rootfs / --dev /dev --proc /proc"
        ));
        assert!(a.contains("--bind /home/u /home/u --chdir /home/u/src/app"));
        assert!(
            a.contains("--perms 1777 --tmpfs /tmp"),
            "apt's `_apt` writes there"
        );
        assert!(a.contains("--clearenv --setenv HOME /root -- /bin/bash -l"));
        assert!(a.ends_with("-- /bin/bash -l"));
        let outside = Inside {
            cwd: Path::new("/data/projects/x"),
            ..inside.clone()
        };
        let a = strings(&bwrap_args(&outside)).join(" ");
        assert!(a.contains("--bind /data/projects/x /data/projects/x --chdir /data/projects/x"));
        // The host's system folders never cover the rootfs's own.
        let system = Inside {
            cwd: Path::new("/usr/share"),
            ..inside.clone()
        };
        let a = strings(&bwrap_args(&system)).join(" ");
        assert!(!a.contains("--bind /usr/share"));
        assert!(a.contains("--chdir /home/u "));
        assert!(!binds_cwd(Path::new("/home/u"), Path::new("/")));
        assert!(binds_cwd(Path::new("/home/u"), Path::new("/tmp/x")));
    }

    #[test]
    fn the_environment_inside_is_clean() {
        let get = |k: &str| match k {
            "TERM" => Some("xterm-kitty".to_owned()),
            "https_proxy" => Some("http://proxy:3128".to_owned()),
            "ANTHROPIC_API_KEY" => Some("sk-fixture".to_owned()),
            _ => None,
        };
        let env = inside_env(&get);
        let has = |k: &str, v: &str| env.iter().any(|(a, b)| a == k && b == v);
        assert!(has("HOME", "/root"));
        assert!(has("TERM", "xterm-kitty"));
        assert!(has("https_proxy", "http://proxy:3128"));
        assert!(has("debian_chroot", "devroot"));
        assert!(!env.iter().any(|(k, _)| k == "ANTHROPIC_API_KEY"));
        assert!(
            inside_env(&|_| None)
                .iter()
                .any(|(k, v)| k == "TERM" && v == "xterm-256color")
        );
    }

    #[test]
    fn podman_runs_the_same_image_by_digest() {
        let a = podman_create_args(Path::new("/home/u")).join(" ");
        assert!(a.starts_with("create --name termos-devroot"));
        assert!(a.contains("--volume /home/u:/home/u"));
        assert!(a.contains(&podman_image()));
        assert!(a.ends_with("sleep infinity"));
        assert_eq!(
            podman_path(Path::new(r"C:\Users\u\src")),
            "/mnt/c/Users/u/src"
        );
        assert_eq!(podman_path(Path::new("/Users/u")), "/Users/u");
        let env = vec![("TERM".to_owned(), "xterm".to_owned())];
        let e = podman_exec_args(
            true,
            "/home/u",
            &env,
            &["apt".to_owned(), "list".to_owned()],
        );
        assert_eq!(
            e.join(" "),
            "exec --interactive --tty --workdir /home/u --env TERM=xterm termos-devroot apt list"
        );
        assert!(!podman_exec_args(false, "/", &[], &[]).contains(&"--tty".to_owned()));
    }

    #[test]
    fn state_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(load(dir.path()), None);
        let state = State {
            backend: Backend::Bubblewrap,
            image: DEBIAN.name.to_owned(),
            source: DEBIAN.layers[0].sha256.to_owned(),
            created: 1_700_000_000,
            updated: None,
        };
        save(dir.path(), &state).expect("save");
        assert_eq!(load(dir.path()), Some(state));
        forget(dir.path()).expect("forget");
        forget(dir.path()).expect("forget twice");
        assert_eq!(load(dir.path()), None);
    }

    #[test]
    fn only_rootfs_folders_are_wiped() {
        assert!(wipeable(Path::new("/d/termos/devroot/rootfs")));
        assert!(wipeable(Path::new("/d/termos/devroot/rootfs.partial")));
        assert!(!wipeable(Path::new("/d/termos/devroot")));
        assert!(!wipeable(Path::new("/home/u")));
        assert!(!wipeable(Path::new("/d/termos/other/rootfs")));
        assert!(wipe(Path::new("/home")).is_err());
    }

    /// An archive with a restrictive folder, a link, a hard link and a
    /// device node, owned by the user running the test.
    #[cfg(unix)]
    fn sample_archive(path: &Path, gzip: bool) {
        use std::io::Write as _;
        let uid = rustix::process::getuid().as_raw();
        let gid = rustix::process::getgid().as_raw();
        let file = File::create(path).expect("create");
        let out: Box<dyn io::Write> = if gzip {
            Box::new(flate2::write::GzEncoder::new(
                file,
                flate2::Compression::fast(),
            ))
        } else {
            Box::new(file)
        };
        let mut b = tar::Builder::new(out);
        let header = |path: &str, kind: tar::EntryType, mode: u32, size: u64| {
            let mut h = tar::Header::new_gnu();
            h.set_path(path).expect("path");
            h.set_entry_type(kind);
            h.set_mode(mode);
            h.set_uid(u64::from(uid));
            h.set_gid(u64::from(gid));
            h.set_size(size);
            h.set_mtime(1_700_000_000);
            h
        };
        let mut h = header("etc/", tar::EntryType::Directory, 0o555, 0);
        h.set_cksum();
        b.append(&h, io::empty()).expect("dir");
        let body = b"PRETTY_NAME=\"Fixture\"\n";
        let mut h = header(
            "etc/os-release",
            tar::EntryType::Regular,
            0o644,
            body.len() as u64,
        );
        h.set_cksum();
        b.append(&h, &body[..]).expect("file");
        let mut h = header("usr/bin/tool", tar::EntryType::Regular, 0o4755, 3);
        h.set_cksum();
        b.append(&h, &b"#!\n"[..]).expect("file");
        let mut h = header("bin", tar::EntryType::Symlink, 0o777, 0);
        h.set_link_name("usr/bin").expect("link");
        h.set_cksum();
        b.append(&h, io::empty()).expect("symlink");
        let mut h = header("usr/bin/tool2", tar::EntryType::Link, 0o755, 0);
        h.set_link_name("usr/bin/tool").expect("link");
        h.set_cksum();
        b.append(&h, io::empty()).expect("hard link");
        let mut h = header("dev/null", tar::EntryType::Char, 0o666, 0);
        h.set_device_major(1).expect("major");
        h.set_device_minor(3).expect("minor");
        h.set_cksum();
        b.append(&h, io::empty()).expect("device");
        b.into_inner().expect("finish").flush().expect("flush");
    }

    #[cfg(unix)]
    #[test]
    fn unpacks_keeping_modes_links_and_restrictive_folders() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        for gzip in [true, false] {
            let dir = tempfile::tempdir().expect("tempdir");
            let archive = dir.path().join("rootfs.tar.gz");
            sample_archive(&archive, gzip);
            let dest = dir.path().join("devroot").join("rootfs.partial");
            let done = unpack(&archive, &dest).expect("unpack");
            assert_eq!(
                done,
                Unpacked {
                    entries: 5,
                    skipped: 1
                }
            );
            let os = std::fs::read_to_string(dest.join("etc/os-release")).expect("read");
            assert!(os.contains("Fixture"));
            let etc = std::fs::metadata(dest.join("etc")).expect("etc");
            assert_eq!(etc.permissions().mode() & 0o777, 0o555);
            let tool = std::fs::metadata(dest.join("usr/bin/tool")).expect("tool");
            assert_eq!(tool.permissions().mode() & 0o7777, 0o4755);
            assert_eq!(tool.mtime(), 1_700_000_000);
            assert!(dest.join("bin/tool").exists(), "through the link");
            let tool2 = std::fs::metadata(dest.join("usr/bin/tool2")).expect("tool2");
            assert_eq!(tool2.ino(), tool.ino(), "a hard link");
            assert!(!dest.join("dev/null").exists());
            // The test's user can write its own folder back.
            std::fs::set_permissions(dest.join("etc"), std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
            wipe(&dest).expect("wipe");
            assert!(!dest.exists());
        }
    }
}
