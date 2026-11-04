// SPDX-License-Identifier: GPL-3.0-or-later
//! Packages for the package manager app and `termos pkg` (M5b, D-110):
//! what is installed and what can be, from four sources:
//! - **mise**: the toolchains and tools TermOS installs (`mise ls`, its
//!   registry);
//! - **the dev rootfs**: apt in Debian 13 ([`crate::devroot`]);
//! - **the host**: the system's own package manager (apt, dnf, pacman,
//!   zypper, apk, Homebrew, winget, Scoop), where there is one;
//! - **Podman**: its images and containers, where it is installed.
//!
//! Listing and searching run the sources' own commands and read their
//! output ([`Backend`]); installing, removing and upgrading run in a
//! window (`termos pkg …`), where a password prompt or a question can be
//! answered.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::devroot;
use crate::mise::which;

/// A package, installed or found.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Package {
    pub name: String,
    pub version: String,
    /// One line about it.
    pub summary: String,
    /// What it is (Podman lists images and containers).
    pub kind: Kind,
}

/// What a [`Package`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Kind {
    #[default]
    Package,
    /// A Podman image.
    Image,
    /// A Podman container.
    Container,
}

impl Package {
    fn new(name: &str, version: &str, summary: &str) -> Self {
        Package {
            name: name.trim().to_owned(),
            version: version.trim().to_owned(),
            summary: summary.trim().to_owned(),
            kind: Kind::Package,
        }
    }
}

/// Where packages come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    Mise,
    Devroot,
    Host,
    Podman,
}

impl Source {
    /// Every source, in the app's order.
    pub const ALL: [Source; 4] = [Source::Mise, Source::Devroot, Source::Host, Source::Podman];

    /// Its name on the command line.
    pub fn id(self) -> &'static str {
        match self {
            Source::Mise => "mise",
            Source::Devroot => "devroot",
            Source::Host => "host",
            Source::Podman => "podman",
        }
    }

    pub fn from_id(id: &str) -> Option<Source> {
        Source::ALL.into_iter().find(|s| s.id() == id)
    }
}

/// A source as this machine has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceInfo {
    pub source: Source,
    /// Its tab's title: `mise`, `Dev rootfs (apt)`, `Host (dnf)`, `Podman`.
    pub title: String,
    /// Why it cannot be used, and what would make it usable.
    pub unavailable: Option<Unavailable>,
}

/// Why a source cannot be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unavailable {
    pub reason: String,
    /// A key, what it does, and the desktop command it runs.
    pub action: Option<(char, String, String)>,
}

/// Whether `name` can be handed to a package manager as one argument: no
/// options, spaces or shell characters.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name.len() <= 200
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"@._+:/=-~".contains(&b))
}

/// The installed packages in a dpkg status file (`/var/lib/dpkg/status`),
/// by name.
pub fn dpkg_installed(status: &str) -> Vec<Package> {
    let mut out: Vec<Package> = status
        .split("\n\n")
        .filter_map(|para| {
            let mut p = Package::default();
            let mut installed = false;
            for line in para.lines() {
                if line.starts_with([' ', '\t']) {
                    continue;
                }
                let Some((key, value)) = line.split_once(':') else {
                    continue;
                };
                let value = value.trim();
                match key {
                    "Package" => p.name = value.to_owned(),
                    "Version" => p.version = value.to_owned(),
                    "Description" => p.summary = value.to_owned(),
                    // `install ok installed`, `hold ok installed`.
                    "Status" => installed = value.split_whitespace().nth(2) == Some("installed"),
                    _ => {}
                }
            }
            (installed && !p.name.is_empty()).then_some(p)
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// `apt-cache search`'s lines: `name - summary`.
pub fn apt_search(out: &str) -> Vec<Package> {
    out.lines()
        .filter_map(|l| l.split_once(" - "))
        .map(|(name, summary)| Package::new(name, "", summary))
        .collect()
}

/// `mise ls --json`: tool → versions, with whether each is installed.
pub fn mise_installed(json: &str) -> Vec<Package> {
    let Ok(serde_json::Value::Object(tools)) = serde_json::from_str(json) else {
        return Vec::new();
    };
    let mut out: Vec<Package> = tools
        .iter()
        .filter_map(|(name, versions)| {
            let versions = versions.as_array()?;
            let installed: Vec<&str> = versions
                .iter()
                .filter(|v| v["installed"].as_bool().unwrap_or(true))
                .filter_map(|v| v["version"].as_str())
                .collect();
            let missing = versions.len() - installed.len();
            let active = versions.iter().any(|v| v["active"].as_bool() == Some(true));
            let summary = match (active, missing) {
                (_, m) if m > 0 && installed.is_empty() => "in a config, not installed",
                (true, _) => "active",
                (false, _) => "",
            };
            Some(Package::new(name, &installed.join(", "), summary))
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// `mise registry`'s lines (`name  backend…`) whose name has `term`.
pub fn mise_registry(out: &str, term: &str) -> Vec<Package> {
    let term = term.to_lowercase();
    out.lines()
        .filter_map(|l| {
            let mut words = l.split_whitespace();
            let name = words.next()?;
            let backends: Vec<&str> = words.collect();
            (name != "NAME" && name.to_lowercase().contains(&term))
                .then(|| Package::new(name, "", &backends.join(" ")))
        })
        .collect()
}

/// `podman images --format json`.
pub fn podman_images(json: &str) -> Vec<Package> {
    let Ok(serde_json::Value::Array(images)) = serde_json::from_str(json) else {
        return Vec::new();
    };
    images
        .iter()
        .map(|i| {
            let first = |k: &str| {
                i[k].as_array()
                    .and_then(|a| a.first())
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
            };
            let id = i["Id"].as_str().unwrap_or("");
            let name = first("Names")
                .or_else(|| first("RepoDigests"))
                .unwrap_or_else(|| id.chars().take(12).collect());
            let size = i["Size"].as_u64().unwrap_or(0);
            let (name, version) = split_reference(&name);
            Package {
                name,
                version,
                summary: format!("image · {} MB", size / 1_000_000),
                kind: Kind::Image,
            }
        })
        .collect()
}

/// `debian:trixie` → (`debian`, `trixie`); `a@sha256:…` → (`a`, `sha256:…`
/// shortened).
fn split_reference(r: &str) -> (String, String) {
    if let Some((name, digest)) = r.split_once('@') {
        let short: String = digest.chars().take(19).collect();
        return (name.to_owned(), short);
    }
    match r.rsplit_once(':') {
        Some((name, tag)) if !tag.contains('/') => (name.to_owned(), tag.to_owned()),
        _ => (r.to_owned(), String::new()),
    }
}

/// `podman ps --all --format json`.
pub fn podman_containers(json: &str) -> Vec<Package> {
    let Ok(serde_json::Value::Array(list)) = serde_json::from_str(json) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|c| {
            let name = c["Names"].as_array()?.first()?.as_str()?.to_owned();
            let state = c["State"].as_str().unwrap_or("?");
            let image = c["Image"].as_str().unwrap_or("");
            Some(Package {
                name,
                version: String::new(),
                summary: format!("container · {state} · {image}"),
                kind: Kind::Container,
            })
        })
        .collect()
}

/// `podman search --format '{{.Name}}\t{{.Description}}'`.
pub fn podman_search(out: &str) -> Vec<Package> {
    out.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let (name, summary) = l.split_once('\t').unwrap_or((l, ""));
            Package {
                kind: Kind::Image,
                ..Package::new(name, "", summary)
            }
        })
        .collect()
}

/// A host package manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Manager {
    Apt,
    Dnf,
    Pacman,
    Zypper,
    Apk,
    Brew,
    Winget,
    Scoop,
}

impl Manager {
    pub fn name(self) -> &'static str {
        match self {
            Manager::Apt => "apt",
            Manager::Dnf => "dnf",
            Manager::Pacman => "pacman",
            Manager::Zypper => "zypper",
            Manager::Apk => "apk",
            Manager::Brew => "Homebrew",
            Manager::Winget => "winget",
            Manager::Scoop => "Scoop",
        }
    }

    /// The programs that tell it is there.
    fn programs(self) -> &'static [&'static str] {
        match self {
            Manager::Apt => &["apt-get", "dpkg-query"],
            Manager::Dnf => &["dnf", "rpm"],
            Manager::Pacman => &["pacman"],
            Manager::Zypper => &["zypper", "rpm"],
            Manager::Apk => &["apk"],
            Manager::Brew => &["brew"],
            Manager::Winget => &["winget"],
            Manager::Scoop => &["scoop"],
        }
    }

    /// Whether installing needs root (`sudo`).
    pub fn needs_root(self) -> bool {
        !matches!(self, Manager::Brew | Manager::Winget | Manager::Scoop)
    }

    /// How it lists what is installed.
    pub fn list_argv(self) -> Vec<String> {
        let v: &[&str] = match self {
            Manager::Apt => &[
                "dpkg-query",
                "-W",
                "-f=${db:Status-Abbrev}\\t${Package}\\t${Version}\\t${binary:Summary}\\n",
            ],
            Manager::Dnf | Manager::Zypper => &[
                "rpm",
                "-qa",
                "--qf",
                "%{NAME}\\t%{VERSION}-%{RELEASE}\\t%{SUMMARY}\\n",
            ],
            Manager::Pacman => &["pacman", "-Q"],
            Manager::Apk => &["apk", "list", "--installed"],
            Manager::Brew => &["brew", "list", "--versions"],
            Manager::Winget => &[
                "winget",
                "list",
                "--disable-interactivity",
                "--accept-source-agreements",
            ],
            Manager::Scoop => &["cmd", "/c", "scoop", "list"],
        };
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    /// How it searches.
    pub fn search_argv(self, term: &str) -> Vec<String> {
        let v: &[&str] = match self {
            Manager::Apt => &["apt-cache", "search", "--names-only"],
            Manager::Dnf => &["dnf", "search", "--quiet"],
            Manager::Pacman => &["pacman", "-Ss"],
            Manager::Zypper => &["zypper", "--quiet", "--non-interactive", "search"],
            Manager::Apk => &["apk", "search", "-v"],
            Manager::Brew => &["brew", "search"],
            Manager::Winget => &[
                "winget",
                "search",
                "--disable-interactivity",
                "--accept-source-agreements",
            ],
            Manager::Scoop => &["cmd", "/c", "scoop", "search"],
        };
        let mut a: Vec<String> = v.iter().map(|s| (*s).to_owned()).collect();
        a.push(term.to_owned());
        a
    }

    /// How it installs `name` (with `elevate`, `sudo` or `doas`, in front
    /// when it needs root).
    pub fn install_argv(self, name: &str, elevate: &[String]) -> Vec<String> {
        let v: &[&str] = match self {
            Manager::Apt => &["apt-get", "install"],
            Manager::Dnf => &["dnf", "install"],
            Manager::Pacman => &["pacman", "-S"],
            Manager::Zypper => &["zypper", "install"],
            Manager::Apk => &["apk", "add"],
            Manager::Brew => &["brew", "install"],
            Manager::Winget => &["winget", "install", "--exact", "--id"],
            Manager::Scoop => &["cmd", "/c", "scoop", "install"],
        };
        self.elevated(v, Some(name), elevate)
    }

    /// How it removes `name`.
    pub fn remove_argv(self, name: &str, elevate: &[String]) -> Vec<String> {
        let v: &[&str] = match self {
            Manager::Apt => &["apt-get", "remove"],
            Manager::Dnf => &["dnf", "remove"],
            Manager::Pacman => &["pacman", "-R"],
            Manager::Zypper => &["zypper", "remove"],
            Manager::Apk => &["apk", "del"],
            Manager::Brew => &["brew", "uninstall"],
            Manager::Winget => &["winget", "uninstall", "--exact", "--id"],
            Manager::Scoop => &["cmd", "/c", "scoop", "uninstall"],
        };
        self.elevated(v, Some(name), elevate)
    }

    /// How it upgrades everything.
    pub fn upgrade_argv(self, elevate: &[String]) -> Vec<String> {
        let v: &[&str] = match self {
            Manager::Apt => &["sh", "-c", "apt-get update && apt-get upgrade"],
            Manager::Dnf => &["dnf", "upgrade"],
            Manager::Pacman => &["pacman", "-Syu"],
            Manager::Zypper => &["zypper", "update"],
            Manager::Apk => &["apk", "upgrade", "--update-cache"],
            Manager::Brew => &["brew", "upgrade"],
            Manager::Winget => &["winget", "upgrade", "--all"],
            Manager::Scoop => &["cmd", "/c", "scoop", "update", "*"],
        };
        self.elevated(v, None, elevate)
    }

    fn elevated(self, v: &[&str], name: Option<&str>, elevate: &[String]) -> Vec<String> {
        let mut a: Vec<String> = if self.needs_root() {
            elevate.to_vec()
        } else {
            Vec::new()
        };
        a.extend(v.iter().map(|s| (*s).to_owned()));
        a.extend(name.map(str::to_owned));
        a
    }

    /// What `list_argv` printed.
    pub fn parse_list(self, out: &str) -> Vec<Package> {
        let mut list = match self {
            Manager::Apt => out
                .lines()
                .filter_map(|l| {
                    let f: Vec<&str> = l.split('\t').collect();
                    // `ii `: wanted and installed.
                    (f.len() >= 4 && f[0].starts_with("ii")).then(|| Package::new(f[1], f[2], f[3]))
                })
                .collect(),
            Manager::Dnf | Manager::Zypper => tab_separated(out),
            Manager::Pacman => out
                .lines()
                .filter_map(|l| l.split_once(' '))
                .map(|(n, v)| Package::new(n, v, ""))
                .collect(),
            Manager::Apk => out
                .lines()
                .filter_map(|l| l.split_whitespace().next())
                .filter_map(apk_split)
                .map(|(n, v)| Package::new(&n, &v, ""))
                .collect(),
            Manager::Brew => out
                .lines()
                .filter_map(|l| {
                    let mut w = l.split_whitespace();
                    let name = w.next()?;
                    Some(Package::new(name, &w.collect::<Vec<_>>().join(", "), ""))
                })
                .collect(),
            Manager::Winget => winget_table(out),
            Manager::Scoop => scoop_table(out),
        };
        list.sort_by_key(|p: &Package| p.name.to_lowercase());
        list
    }

    /// What `search_argv` printed.
    pub fn parse_search(self, out: &str) -> Vec<Package> {
        match self {
            Manager::Apt => apt_search(out),
            Manager::Dnf => out
                .lines()
                .filter_map(|l| {
                    // dnf 4: `htop.x86_64 : Interactive process viewer`;
                    // dnf 5: ` htop.x86_64\tInteractive process viewer`.
                    let (name, summary) = l
                        .split_once(" : ")
                        .or_else(|| l.starts_with(' ').then(|| l.trim().split_once('\t'))?)?;
                    let name = name.trim();
                    let name = name.rsplit_once('.').map_or(name, |(n, _)| n);
                    Some(Package::new(name, "", summary))
                })
                .collect(),
            Manager::Pacman => {
                let mut out_list = Vec::new();
                let mut lines = out.lines().peekable();
                while let Some(l) = lines.next() {
                    if l.starts_with(' ') {
                        continue;
                    }
                    let mut w = l.split_whitespace();
                    let (Some(repo_name), Some(version)) = (w.next(), w.next()) else {
                        continue;
                    };
                    let name = repo_name.rsplit_once('/').map_or(repo_name, |(_, n)| n);
                    let summary = lines.next_if(|n| n.starts_with(' ')).unwrap_or("");
                    out_list.push(Package::new(name, version, summary));
                }
                out_list
            }
            Manager::Zypper => out
                .lines()
                .filter_map(|l| {
                    let f: Vec<&str> = l.split('|').map(str::trim).collect();
                    (f.len() >= 3 && !f[1].is_empty() && f[1] != "Name" && !f[1].starts_with('-'))
                        .then(|| Package::new(f[1], "", f[2]))
                })
                .collect(),
            Manager::Apk => out
                .lines()
                .filter_map(|l| {
                    let (nv, summary) = l.split_once(" - ").unwrap_or((l, ""));
                    let (n, v) = apk_split(nv.trim())?;
                    Some(Package::new(&n, &v, summary))
                })
                .collect(),
            Manager::Brew => out
                .lines()
                .filter(|l| !l.trim().is_empty() && !l.starts_with("==>"))
                .map(|l| Package::new(l, "", ""))
                .collect(),
            Manager::Winget => winget_table(out),
            Manager::Scoop => scoop_table(out),
        }
    }
}

fn tab_separated(out: &str) -> Vec<Package> {
    out.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            (f.len() >= 2 && !f[0].is_empty())
                .then(|| Package::new(f[0], f[1], f.get(2).copied().unwrap_or("")))
        })
        .collect()
}

/// `htop-3.3.0-r0` → (`htop`, `3.3.0-r0`).
fn apk_split(nv: &str) -> Option<(String, String)> {
    let mut parts = nv.rsplitn(3, '-');
    let release = parts.next()?;
    let version = parts.next()?;
    let name = parts.next()?;
    (release.starts_with('r') && version.starts_with(|c: char| c.is_ascii_digit()))
        .then(|| (name.to_owned(), format!("{version}-{release}")))
}

/// winget's tables: columns where the header's words start, above a line
/// of dashes. The package is its Id; the name is its summary.
fn winget_table(out: &str) -> Vec<Package> {
    let lines: Vec<&str> = out
        .lines()
        // Its spinner is written over with carriage returns.
        .map(|l| l.rsplit('\r').next().unwrap_or(l))
        .collect();
    let Some(dashes) = lines
        .iter()
        .position(|l| l.len() > 10 && l.trim().chars().all(|c| c == '-'))
    else {
        return Vec::new();
    };
    let Some(header) = dashes.checked_sub(1).map(|i| lines[i]) else {
        return Vec::new();
    };
    let head: Vec<char> = header.chars().collect();
    let starts: Vec<usize> = (0..head.len())
        .filter(|&i| head[i] != ' ' && (i == 0 || head[i - 1] == ' '))
        .collect();
    if starts.len() < 3 {
        return Vec::new();
    }
    lines[dashes + 1..]
        .iter()
        .filter_map(|l| {
            let c: Vec<char> = l.chars().collect();
            let col = |i: usize| -> String {
                let from = starts[i].min(c.len());
                let to = starts.get(i + 1).copied().unwrap_or(c.len()).min(c.len());
                c[from..to].iter().collect::<String>().trim().to_owned()
            };
            let id = col(1);
            (!id.is_empty()).then(|| Package::new(&id, &col(2), &col(0)))
        })
        .collect()
}

/// Scoop's tables: `Name Version Source …` above dashes.
fn scoop_table(out: &str) -> Vec<Package> {
    let mut after_dashes = false;
    out.lines()
        .filter_map(|l| {
            if l.trim_start().starts_with("----") {
                after_dashes = true;
                return None;
            }
            if !after_dashes {
                return None;
            }
            let mut w = l.split_whitespace();
            let name = w.next()?;
            let version = w.next().unwrap_or("");
            let source = w.next().unwrap_or("");
            Some(Package::new(name, version, source))
        })
        .collect()
}

/// The host's package manager: the first whose programs `has` finds, of
/// those `os` has.
pub fn detect_host(os: &str, has: &dyn Fn(&str) -> bool) -> Option<Manager> {
    let order: &[Manager] = match os {
        "linux" => &[
            Manager::Apt,
            Manager::Dnf,
            Manager::Pacman,
            Manager::Zypper,
            Manager::Apk,
        ],
        "macos" => &[Manager::Brew],
        "windows" => &[Manager::Winget, Manager::Scoop],
        _ => &[],
    };
    order
        .iter()
        .copied()
        .find(|m| m.programs().iter().all(|p| has(p)))
}

/// What puts root in front of the host's package manager: nothing for
/// root, else `sudo`, else `doas`.
pub fn elevate(is_root: bool, has: &dyn Fn(&str) -> bool) -> Vec<String> {
    if is_root {
        Vec::new()
    } else if has("sudo") {
        vec!["sudo".to_owned()]
    } else if has("doas") {
        vec!["doas".to_owned()]
    } else {
        Vec::new()
    }
}

/// Lists and searches the sources (the package manager app's view; the
/// system's in [`System`], fixtures in tests).
pub trait Backend: Send + Sync {
    /// Every source, and whether it can be used here.
    fn sources(&self) -> Vec<SourceInfo>;
    /// What `source` has installed.
    fn installed(&self, source: Source) -> Result<Vec<Package>, String>;
    /// What `source` has for `term`.
    fn search(&self, source: Source, term: &str) -> Result<Vec<Package>, String>;
    /// Files that change when `source`'s packages change, so the app
    /// lists them again (after an install in its window, say).
    fn watch(&self, source: Source) -> Vec<PathBuf> {
        let _ = source;
        Vec::new()
    }
}

/// The sources of this machine.
#[derive(Debug, Clone)]
pub struct System {
    /// TermOS's data folder (the dev rootfs, a fetched mise).
    pub data_dir: PathBuf,
    /// The `termos` program, which enters the dev rootfs.
    pub termos: PathBuf,
}

fn run(argv: &[String]) -> Result<String, String> {
    let (program, args) = argv.split_first().ok_or("no command")?;
    let out = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        let why = why
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        // Searches that find nothing exit with 1 (pacman, zypper's 104…)
        // and say nothing.
        if why.is_empty() {
            return Ok(String::new());
        }
        return Err(format!("{program}: {}", why.trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn path_has(p: &str) -> bool {
    which(p, std::env::var_os("PATH").as_ref()).is_some()
}

impl System {
    /// The host's package manager here.
    pub fn host(&self) -> Option<Manager> {
        detect_host(std::env::consts::OS, &path_has)
    }

    fn mise(&self) -> Result<PathBuf, String> {
        crate::mise::find(&self.data_dir)
            .map(|m| m.path)
            .ok_or_else(|| "mise is not installed".to_owned())
    }

    fn podman(&self) -> Result<PathBuf, String> {
        which("podman", std::env::var_os("PATH").as_ref())
            .ok_or_else(|| "Podman is not installed".to_owned())
    }

    fn termos(&self, args: &[&str]) -> Vec<String> {
        std::iter::once(self.termos.display().to_string())
            .chain(args.iter().map(|a| (*a).to_owned()))
            .collect()
    }
}

impl Backend for System {
    fn sources(&self) -> Vec<SourceInfo> {
        let mise = crate::mise::find(&self.data_dir);
        let devroot = devroot::load(&self.data_dir);
        let host = self.host();
        let podman = self.podman().is_ok();
        vec![
            SourceInfo {
                source: Source::Mise,
                title: "mise".to_owned(),
                unavailable: mise.is_none().then(|| Unavailable {
                    reason: "mise is not installed yet. TermOS fetches a pinned, checked copy.".to_owned(),
                    action: Some(('f', "fetch mise".to_owned(), "pkg.run setup mise".to_owned())),
                }),
            },
            SourceInfo {
                source: Source::Devroot,
                title: "Dev rootfs (apt)".to_owned(),
                unavailable: devroot.is_none().then(|| Unavailable {
                    reason: format!(
                        "No dev rootfs yet: {}, where apt installs compilers and libraries without the host's root.",
                        devroot::DEBIAN.name
                    ),
                    action: Some(('c', "create it".to_owned(), "devroot.create".to_owned())),
                }),
            },
            SourceInfo {
                source: Source::Host,
                title: host.map_or_else(|| "Host".to_owned(), |m| format!("Host ({})", m.name())),
                unavailable: host.is_none().then(|| Unavailable {
                    reason: "This system has no package manager TermOS knows: install packages in the dev rootfs.".to_owned(),
                    action: None,
                }),
            },
            SourceInfo {
                source: Source::Podman,
                title: "Podman".to_owned(),
                unavailable: (!podman).then(|| Unavailable {
                    reason: "Podman is not installed (https://podman.io).".to_owned(),
                    action: None,
                }),
            },
        ]
    }

    fn installed(&self, source: Source) -> Result<Vec<Package>, String> {
        match source {
            Source::Mise => {
                let mise = self.mise()?.display().to_string();
                Ok(mise_installed(&run(&[mise, "ls".into(), "--json".into()])?))
            }
            Source::Devroot => {
                let state = devroot::load(&self.data_dir).ok_or("no dev rootfs yet")?;
                let status = match state.backend {
                    devroot::Backend::Bubblewrap => std::fs::read_to_string(
                        devroot::rootfs(&self.data_dir).join("var/lib/dpkg/status"),
                    )
                    .map_err(|e| format!("cannot read the dev rootfs's packages: {e}"))?,
                    devroot::Backend::Podman => run(&self.termos(&[
                        "devroot",
                        "enter",
                        "--",
                        "cat",
                        "/var/lib/dpkg/status",
                    ]))?,
                };
                Ok(dpkg_installed(&status))
            }
            Source::Host => {
                let m = self.host().ok_or("no package manager")?;
                Ok(m.parse_list(&run(&m.list_argv())?))
            }
            Source::Podman => {
                let podman = self.podman()?.display().to_string();
                let mut list = podman_containers(&run(&[
                    podman.clone(),
                    "ps".into(),
                    "--all".into(),
                    "--format".into(),
                    "json".into(),
                ])?);
                list.extend(podman_images(&run(&[
                    podman,
                    "images".into(),
                    "--format".into(),
                    "json".into(),
                ])?));
                Ok(list)
            }
        }
    }

    fn search(&self, source: Source, term: &str) -> Result<Vec<Package>, String> {
        if !valid_name(term) {
            return Err(format!("cannot search for {term:?}"));
        }
        match source {
            Source::Mise => {
                let mise = self.mise()?.display().to_string();
                Ok(mise_registry(&run(&[mise, "registry".into()])?, term))
            }
            Source::Devroot => Ok(apt_search(&run(&self.termos(&[
                "devroot",
                "enter",
                "--",
                "apt-cache",
                "search",
                "--names-only",
                term,
            ]))?)),
            Source::Host => {
                let m = self.host().ok_or("no package manager")?;
                Ok(m.parse_search(&run(&m.search_argv(term))?))
            }
            Source::Podman => {
                let podman = self.podman()?.display().to_string();
                Ok(podman_search(&run(&[
                    podman,
                    "search".into(),
                    "--limit".into(),
                    "30".into(),
                    "--format".into(),
                    "{{.Name}}\t{{.Description}}".into(),
                    term.into(),
                ])?))
            }
        }
    }

    fn watch(&self, source: Source) -> Vec<PathBuf> {
        match source {
            Source::Mise => crate::mise::mise_data_dir(&|k| std::env::var(k).ok())
                .map(|d| d.join("installs"))
                .into_iter()
                .collect(),
            Source::Devroot => match devroot::load(&self.data_dir).map(|s| s.backend) {
                Some(devroot::Backend::Bubblewrap) => {
                    vec![devroot::rootfs(&self.data_dir).join("var/lib/dpkg/status")]
                }
                _ => Vec::new(),
            },
            Source::Host => match self.host() {
                Some(Manager::Apt) => vec![PathBuf::from("/var/lib/dpkg/status")],
                Some(Manager::Dnf | Manager::Zypper) => vec![PathBuf::from("/var/lib/rpm")],
                Some(Manager::Pacman) => vec![PathBuf::from("/var/lib/pacman/local")],
                Some(Manager::Apk) => vec![PathBuf::from("/lib/apk/db/installed")],
                _ => Vec::new(),
            },
            Source::Podman => Vec::new(),
        }
    }
}

/// When `path` last changed.
pub fn changed_at(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[Package]) -> Vec<&str> {
        list.iter().map(|p| p.name.as_str()).collect()
    }

    #[test]
    fn dpkg_status_lists_what_is_installed() {
        let status = "Package: zlib1g\nStatus: install ok installed\nVersion: 1:1.3.dfsg+really1.3.1-1+b1\nDescription: compression library - runtime\n zlib is a library implementing the deflate compression method\n\nPackage: adduser\nStatus: install ok installed\nVersion: 3.152\nDescription: add and remove users and groups\n\nPackage: gone\nStatus: deinstall ok config-files\nVersion: 1\n\nPackage: held\nStatus: hold ok installed\nVersion: 2\nDescription: kept back\n";
        let got = dpkg_installed(status);
        assert_eq!(names(&got), ["adduser", "held", "zlib1g"]);
        assert_eq!(got[2].version, "1:1.3.dfsg+really1.3.1-1+b1");
        assert_eq!(got[2].summary, "compression library - runtime");
        assert!(dpkg_installed("").is_empty());
        let found = apt_search(
            "hello - example package based on GNU hello\nhello-traditional - the classic\n",
        );
        assert_eq!(names(&found), ["hello", "hello-traditional"]);
        assert_eq!(found[1].summary, "the classic");
    }

    #[test]
    fn mise_tools_and_its_registry() {
        let json = r#"{"node":[{"version":"22.11.0","requested_version":"22","installed":true,"active":true}],
            "go":[{"version":"1.25.3","installed":true,"active":false},{"version":"1.24.0","installed":true}],
            "zig":[{"version":"0.15.2","installed":false}]}"#;
        let got = mise_installed(json);
        assert_eq!(names(&got), ["go", "node", "zig"]);
        assert_eq!(got[0].version, "1.25.3, 1.24.0");
        assert_eq!(got[1].summary, "active");
        assert_eq!(got[2].summary, "in a config, not installed");
        assert!(mise_installed("not json").is_empty());
        let registry = "NAME BACKENDS\njq                 aqua:jqlang/jq ubi:jqlang/jq\njqp     aqua:noahgorstein/jqp\nripgrep  aqua:BurntSushi/ripgrep\n";
        let found = mise_registry(registry, "JQ");
        assert_eq!(names(&found), ["jq", "jqp"]);
        assert_eq!(found[0].summary, "aqua:jqlang/jq ubi:jqlang/jq");
    }

    #[test]
    fn podman_images_and_containers() {
        let images = r#"[{"Id":"0123456789abcdef","Names":["docker.io/library/debian:trixie"],"Size":124000000},
            {"Id":"fedcba9876543210","Names":null,"RepoDigests":["docker.io/library/debian@sha256:2dc419c6d7fe3dad0a2f"],"Size":5},
            {"Id":"aaaaaaaaaaaaaaaaaaaa","Size":0}]"#;
        let got = podman_images(images);
        assert_eq!(
            names(&got),
            [
                "docker.io/library/debian",
                "docker.io/library/debian",
                "aaaaaaaaaaaa"
            ]
        );
        assert_eq!(got[0].version, "trixie");
        assert_eq!(got[0].summary, "image · 124 MB");
        assert_eq!(got[1].version, "sha256:2dc419c6d7fe");
        assert!(got.iter().all(|p| p.kind == Kind::Image));
        let ps = r#"[{"Names":["termos-devroot"],"State":"running","Image":"docker.io/library/debian@sha256:2dc4"}]"#;
        let c = podman_containers(ps);
        assert_eq!(c[0].name, "termos-devroot");
        assert_eq!(c[0].kind, Kind::Container);
        assert!(c[0].summary.contains("running"));
        let found =
            podman_search("docker.io/library/alpine\tA minimal Docker image\nquay.io/x/y\t\n");
        assert_eq!(names(&found), ["docker.io/library/alpine", "quay.io/x/y"]);
        assert_eq!(
            split_reference("localhost:5000/app"),
            ("localhost:5000/app".to_owned(), String::new())
        );
    }

    #[test]
    fn host_managers_are_found_in_order() {
        let has = |p: &str| ["dnf", "rpm", "pacman", "sudo"].contains(&p);
        assert_eq!(detect_host("linux", &has), Some(Manager::Dnf));
        assert_eq!(
            detect_host("linux", &|p| p == "apt-get"),
            None,
            "dpkg-query too"
        );
        assert_eq!(detect_host("macos", &|p| p == "brew"), Some(Manager::Brew));
        assert_eq!(
            detect_host("windows", &|p| p == "scoop"),
            Some(Manager::Scoop)
        );
        assert_eq!(detect_host("freebsd", &has), None);
        assert_eq!(elevate(false, &has), ["sudo"]);
        assert!(elevate(true, &has).is_empty());
        assert_eq!(elevate(false, &|p| p == "doas"), ["doas"]);
    }

    #[test]
    fn host_commands() {
        let sudo = vec!["sudo".to_owned()];
        assert_eq!(
            Manager::Apt.install_argv("htop", &sudo).join(" "),
            "sudo apt-get install htop"
        );
        assert_eq!(
            Manager::Brew.install_argv("htop", &sudo).join(" "),
            "brew install htop"
        );
        assert_eq!(
            Manager::Winget.remove_argv("Git.Git", &sudo).join(" "),
            "winget uninstall --exact --id Git.Git"
        );
        assert_eq!(
            Manager::Apt.upgrade_argv(&sudo).join(" "),
            "sudo sh -c apt-get update && apt-get upgrade"
        );
        assert_eq!(
            Manager::Pacman.search_argv("htop").join(" "),
            "pacman -Ss htop"
        );
        assert!(Manager::Apt.list_argv()[2].contains("${binary:Summary}"));
    }

    #[test]
    fn host_lists() {
        let apt = "ii \thtop\t3.3.0-4\tinteractive processes viewer\nrc \told\t1\tremoved\n";
        assert_eq!(names(&Manager::Apt.parse_list(apt)), ["htop"]);
        let rpm = "bash\t5.2.26-3.fc40\tThe GNU Bourne Again shell\nhtop\t3.3.0-1.fc40\tInteractive process viewer\n";
        let r = Manager::Dnf.parse_list(rpm);
        assert_eq!(names(&r), ["bash", "htop"]);
        assert_eq!(r[1].version, "3.3.0-1.fc40");
        assert_eq!(
            names(&Manager::Pacman.parse_list("bash 5.2.037-1\nhtop 3.3.0-3\n")),
            ["bash", "htop"]
        );
        let apk = "htop-3.3.0-r0 x86_64 {htop} (GPL-2.0-or-later) [installed]\npy3-foo-bar-1.2-r1 noarch {x} (MIT) [installed]\n";
        let a = Manager::Apk.parse_list(apk);
        assert_eq!(names(&a), ["htop", "py3-foo-bar"]);
        assert_eq!(a[1].version, "1.2-r1");
        let brew = Manager::Brew.parse_list("git 2.47.0\nnode 22.1.0 20.11.1\n");
        assert_eq!(brew[1].version, "22.1.0, 20.11.1");
        let winget = "\r-\r\\\rName                 Id                Version   Source\n----------------------------------------------------\nGit                  Git.Git           2.47.0    winget\nMicrosoft Visual C…  Microsoft.VCRedi… 14.40     winget\n";
        let w = Manager::Winget.parse_list(winget);
        assert_eq!(names(&w), ["Git.Git", "Microsoft.VCRedi…"]);
        assert_eq!(w[0].version, "2.47.0");
        assert_eq!(w[0].summary, "Git");
        let scoop = "Installed apps:\n\nName Version Source Updated\n---- ------- ------ -------\n7zip 24.08   main   2024-10-01\ngit  2.47.0  main   2024-10-02\n";
        assert_eq!(names(&Manager::Scoop.parse_list(scoop)), ["7zip", "git"]);
    }

    #[test]
    fn host_searches() {
        let dnf4 = "======== Name Exactly Matched: htop ========\nhtop.x86_64 : Interactive process viewer\n";
        assert_eq!(names(&Manager::Dnf.parse_search(dnf4)), ["htop"]);
        let dnf5 = "Matched fields: name (exact)\n htop.x86_64\tInteractive process viewer\n";
        let d = Manager::Dnf.parse_search(dnf5);
        assert_eq!(names(&d), ["htop"]);
        assert_eq!(d[0].summary, "Interactive process viewer");
        let pacman = "extra/htop 3.3.0-3 [installed]\n    Interactive process viewer\nextra/btop 1.4.0-1\n    A monitor of resources\n";
        let p = Manager::Pacman.parse_search(pacman);
        assert_eq!(names(&p), ["htop", "btop"]);
        assert_eq!(p[1].summary, "A monitor of resources");
        let zypper = "S | Name | Summary                    | Type\n--+------+----------------------------+--------\n  | htop | An interactive process viewer | package\n";
        assert_eq!(names(&Manager::Zypper.parse_search(zypper)), ["htop"]);
        let apk = "htop-3.3.0-r0 - Interactive process viewer\n";
        assert_eq!(
            Manager::Apk.parse_search(apk)[0].summary,
            "Interactive process viewer"
        );
        let brew = "==> Formulae\nhtop\nhtop-osx\n\n==> Casks\nhtop-app\n";
        assert_eq!(
            names(&Manager::Brew.parse_search(brew)),
            ["htop", "htop-osx", "htop-app"]
        );
    }

    #[test]
    fn names_are_one_plain_argument() {
        for ok in [
            "htop",
            "npm:prettier",
            "aqua:jqlang/jq",
            "python@3.12",
            "docker.io/library/debian:trixie",
            "g++",
        ] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in ["", "-y", "a b", "x;rm", "$(x)", "a\nb"] {
            assert!(!valid_name(bad), "{bad:?}");
        }
        assert_eq!(Source::from_id("devroot"), Some(Source::Devroot));
        assert_eq!(Source::from_id("apt"), None);
    }
}
