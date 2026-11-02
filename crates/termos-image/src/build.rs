// SPDX-License-Identifier: GPL-3.0-or-later
//! `cargo xtask image build <target>` (D-074):
//!
//! 1. Buildroot, the pinned release, downloaded once and checked.
//! 2. The target's configuration: Buildroot's board defconfig plus
//!    TermOS's fragments, each setting checked after `olddefconfig`.
//! 3. `make termos-depends`: the toolchain and the libraries `termos`
//!    links (libinput, libxkbcommon, libseat, libudev) in the sysroot.
//! 4. `termos` with the `kms` feature, by cargo with our pinned Rust,
//!    linked by Buildroot's compiler against that sysroot (Buildroot's own
//!    Rust is older than the workspace needs).
//! 5. `make`: the image, with that binary in it.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::fragments;
use crate::targets::Target;

/// The Buildroot release images are built with (an LTS line, D-074).
pub const BUILDROOT_VERSION: &str = "2026.02.1";

/// SHA-256 of `buildroot-<BUILDROOT_VERSION>.tar.xz`. `None` until it is
/// pinned from a first download checked against buildroot.org's signed
/// list (the build stops and prints both).
pub const BUILDROOT_SHA256: Option<&str> = None;

/// Where everything goes.
#[derive(Debug, Clone)]
pub struct Paths {
    /// The repository.
    pub repo: PathBuf,
    /// `target/image`.
    pub work: PathBuf,
    /// Buildroot's download cache (`BR2_DL_DIR`), kept between builds.
    pub dl: PathBuf,
}

impl Paths {
    pub fn new(repo: &Path) -> Self {
        let work = std::env::var_os("TERMOS_IMAGE_DIR")
            .map_or_else(|| repo.join("target/image"), PathBuf::from);
        let dl = std::env::var_os("BR2_DL_DIR").map_or_else(|| work.join("dl"), PathBuf::from);
        Self {
            repo: repo.to_owned(),
            work,
            dl,
        }
    }

    pub fn buildroot(&self) -> PathBuf {
        self.work.join(format!("buildroot-{BUILDROOT_VERSION}"))
    }

    pub fn external(&self) -> PathBuf {
        self.repo.join("crates/termos-image/br2-external")
    }

    /// Buildroot's output folder for a target.
    pub fn out(&self, t: &Target) -> PathBuf {
        self.work.join(t.name)
    }

    pub fn images(&self, t: &Target) -> PathBuf {
        self.out(t).join("images")
    }
}

fn run(cmd: &mut Command) -> Result<(), String> {
    let shown = format!("{cmd:?}");
    println!("image: {shown}");
    let status = cmd
        .status()
        .map_err(|e| format!("cannot run {shown}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{shown} failed ({status})"))
    }
}

fn output(cmd: &mut Command) -> Result<String, String> {
    let shown = format!("{cmd:?}");
    let out = cmd
        .output()
        .map_err(|e| format!("cannot run {shown}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{shown} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn sha256(file: &Path) -> Result<String, String> {
    let out = output(Command::new("sha256sum").arg(file))?;
    out.split_whitespace()
        .next()
        .map(str::to_owned)
        .ok_or_else(|| "sha256sum printed nothing".into())
}

/// Downloads and unpacks Buildroot, once.
pub fn fetch(p: &Paths) -> Result<(), String> {
    if p.buildroot().join("Makefile").is_file() {
        return Ok(());
    }
    std::fs::create_dir_all(&p.dl).map_err(|e| e.to_string())?;
    let name = format!("buildroot-{BUILDROOT_VERSION}.tar.xz");
    let tarball = p.dl.join(&name);
    let url = format!("https://buildroot.org/downloads/{name}");
    if !tarball.is_file() {
        let part = p.dl.join(format!("{name}.part"));
        run(Command::new("curl")
            .args(["--proto", "=https", "--tlsv1.2", "-fsSL", "-o"])
            .arg(&part)
            .arg(&url))?;
        std::fs::rename(&part, &tarball).map_err(|e| e.to_string())?;
    }
    let got = sha256(&tarball)?;
    match BUILDROOT_SHA256 {
        Some(want) if want == got => {}
        pinned => {
            // Say what buildroot.org's signed list has, to pin it from.
            let listed = output(Command::new("curl").args(["-fsSL", &format!("{url}.sign")]))
                .unwrap_or_else(|e| format!("(cannot read {url}.sign: {e})"));
            let listed: Vec<&str> = listed
                .lines()
                .filter(|l| l.contains("SHA256") || l.contains(&name))
                .collect();
            let _ = std::fs::remove_file(&tarball);
            return Err(match pinned {
                Some(want) => format!(
                    "{name} has SHA-256 {got}, not the pinned {want}; buildroot.org lists:\n{}",
                    listed.join("\n")
                ),
                None => format!(
                    "no SHA-256 is pinned for {name} yet: the download has {got}; buildroot.org's signed list has:\n{}\n\
                     Pin it in termos-image's build.rs once they match.",
                    listed.join("\n")
                ),
            });
        }
    }
    run(Command::new("tar")
        .arg("-xJf")
        .arg(&tarball)
        .arg("-C")
        .arg(&p.work))
}

fn make(p: &Paths, t: &Target) -> Command {
    let mut c = Command::new("make");
    c.arg("-C")
        .arg(p.buildroot())
        .arg(format!("O={}", p.out(t).display()))
        .arg(format!("BR2_EXTERNAL={}", p.external().display()))
        .env("BR2_DL_DIR", &p.dl);
    c
}

/// Where `termos` is put for the Buildroot package.
pub fn prebuilt(p: &Paths, t: &Target) -> PathBuf {
    p.out(t).join("termos-prebuilt")
}

/// Step 2: the configuration, checked.
pub fn configure(p: &Paths, t: &Target) -> Result<(), String> {
    let out = p.out(t);
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    run(make(p, t).arg(t.base))?;
    // TermOS's settings on top, and where the binary will be.
    let extra = out.join("termos-local.fragment");
    std::fs::write(
        &extra,
        format!(
            "BR2_PACKAGE_TERMOS_PREBUILT_DIR=\"{}\"\n",
            prebuilt(p, t).display()
        ),
    )
    .map_err(|e| e.to_string())?;
    let configs = p.external().join("configs");
    let frags: Vec<PathBuf> = t
        .fragments
        .iter()
        .map(|f| configs.join(f))
        .chain([extra])
        .collect();
    run(
        Command::new(p.buildroot().join("support/kconfig/merge_config.sh"))
            .arg("-m")
            .arg("-O")
            .arg(&out)
            .arg(out.join(".config"))
            .args(&frags)
            .env("BR2_EXTERNAL", p.external()),
    )?;
    run(make(p, t).arg("olddefconfig"))?;
    let config =
        fragments::parse(&std::fs::read_to_string(out.join(".config")).map_err(|e| e.to_string())?);
    let mut problems = Vec::new();
    for f in &frags {
        let text = std::fs::read_to_string(f).map_err(|e| format!("{}: {e}", f.display()))?;
        for m in fragments::missing(&fragments::parse(&text), &config) {
            problems.push(format!(
                "{}: {m}",
                f.file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
            ));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "the configuration lost settings:\n  {}",
            problems.join("\n  ")
        ))
    }
}

/// The cross compiler Buildroot made: `<out>/host/bin/<tuple>-gcc`.
pub fn cross_gcc(p: &Paths, t: &Target) -> Result<PathBuf, String> {
    let bin = p.out(t).join("host/bin");
    let arch = t.rust.split('-').next().unwrap_or("");
    let mut found: Vec<PathBuf> = std::fs::read_dir(&bin)
        .map_err(|e| format!("{}: {e}", bin.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|f| {
            f.file_name().is_some_and(|n| {
                let n = n.to_string_lossy();
                n.starts_with(arch) && n.ends_with("-linux-gnu-gcc")
            })
        })
        .collect();
    found.sort();
    found
        .into_iter()
        .next()
        .ok_or_else(|| format!("no {arch} cross compiler in {}", bin.display()))
}

/// The environment cargo builds `termos` for the image with.
pub fn cargo_env(p: &Paths, t: &Target, gcc: &Path) -> Vec<(String, String)> {
    let tool = |suffix: &str| gcc.to_string_lossy().trim_end_matches("gcc").to_owned() + suffix;
    let cc = t.cc_suffix();
    vec![
        (t.cargo_linker_var(), gcc.display().to_string()),
        (format!("CC_{cc}"), gcc.display().to_string()),
        (format!("CXX_{cc}"), tool("g++")),
        (format!("AR_{cc}"), tool("ar")),
        (
            "PKG_CONFIG".into(),
            p.out(t).join("host/bin/pkg-config").display().to_string(),
        ),
        ("PKG_CONFIG_ALLOW_CROSS".into(), "1".into()),
    ]
}

/// The whole build. `legal_info`: also Buildroot's license report.
pub fn build(p: &Paths, t: &Target, legal_info: bool) -> Result<PathBuf, String> {
    fetch(p)?;
    configure(p, t)?;
    run(make(p, t).arg("termos-depends"))?;
    let gcc = cross_gcc(p, t)?;
    let mut cargo = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    cargo
        .current_dir(&p.repo)
        .args([
            "build",
            "--release",
            "--locked",
            "-p",
            "termos-cli",
            "--features",
            "kms",
            "--target",
            t.rust,
        ])
        .envs(cargo_env(p, t, &gcc));
    run(&mut cargo)?;
    let dir = prebuilt(p, t);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let bin = p.repo.join("target").join(t.rust).join("release/termos");
    std::fs::copy(&bin, dir.join("termos")).map_err(|e| format!("{}: {e}", bin.display()))?;
    std::fs::copy(p.repo.join("LICENSE"), dir.join("LICENSE"))
        .map_err(|e| format!("LICENSE: {e}"))?;
    // A new binary each time: let the package pick it up again.
    run(make(p, t).arg("termos-dirclean"))?;
    run(&mut make(p, t))?;
    if legal_info {
        run(make(p, t).arg("legal-info"))?;
    }
    Ok(p.images(t))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::targets::find;

    #[test]
    fn cargo_uses_buildroots_compiler_and_sysroot() {
        let p = Paths {
            repo: PathBuf::from("/r"),
            work: PathBuf::from("/r/target/image"),
            dl: PathBuf::from("/dl"),
        };
        let t = find("aarch64-virt").expect("target");
        let gcc =
            PathBuf::from("/r/target/image/aarch64-virt/host/bin/aarch64-buildroot-linux-gnu-gcc");
        let env = cargo_env(&p, t, &gcc);
        let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(
            get("CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER"),
            Some(gcc.to_str().expect("utf8"))
        );
        assert_eq!(
            get("CXX_aarch64_unknown_linux_gnu"),
            Some("/r/target/image/aarch64-virt/host/bin/aarch64-buildroot-linux-gnu-g++")
        );
        assert_eq!(
            get("PKG_CONFIG"),
            Some("/r/target/image/aarch64-virt/host/bin/pkg-config")
        );
        assert_eq!(
            p.buildroot(),
            PathBuf::from(format!("/r/target/image/buildroot-{BUILDROOT_VERSION}"))
        );
        assert_eq!(
            prebuilt(&p, t),
            PathBuf::from("/r/target/image/aarch64-virt/termos-prebuilt")
        );
    }
}
