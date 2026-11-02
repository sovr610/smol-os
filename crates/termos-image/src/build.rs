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

/// SHA-256 of `buildroot-<BUILDROOT_VERSION>.tar.xz`, as buildroot.org's
/// signed list gives it (and the first download in CI had). `None` would
/// stop the build and print both, to pin a new release.
pub const BUILDROOT_SHA256: Option<&str> =
    Some("a2216fdc46b5e81e529acb9077324f7b4a9403366922f350bf7be67f46231b66");

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
        .env("BR2_DL_DIR", &p.dl)
        // `cargo xtask` runs with cargo's library path, which Buildroot's
        // sanity checks refuse.
        .env_remove("LD_LIBRARY_PATH");
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

/// The prefix of the cross tools Buildroot made, e.g.
/// `<out>/host/bin/x86_64-linux-`, as Buildroot itself names them
/// (`TARGET_CROSS`): Bootlin's toolchains and Buildroot's own differ.
pub fn cross_prefix(p: &Paths, t: &Target) -> Result<String, String> {
    let out = output(make(p, t).args([
        "-s",
        "--no-print-directory",
        "printvars",
        "VARS=TARGET_CROSS",
    ]))?;
    let prefix =
        parse_cross(&out).ok_or_else(|| format!("Buildroot named no TARGET_CROSS: {out}"))?;
    let gcc = PathBuf::from(format!("{prefix}gcc"));
    if gcc.is_file() {
        Ok(prefix)
    } else {
        Err(format!("no cross compiler at {}", gcc.display()))
    }
}

/// `TARGET_CROSS` from `make printvars`.
fn parse_cross(printvars: &str) -> Option<String> {
    printvars
        .lines()
        .find_map(|l| l.trim().strip_prefix("TARGET_CROSS="))
        .map(|v| v.trim().trim_matches('\'').to_owned())
        .filter(|v| !v.is_empty())
}

/// The environment cargo builds `termos` for the image with.
///
/// The linker is given in `RUSTFLAGS` rather than `CARGO_TARGET_<T>_LINKER`:
/// with `--target`, `RUSTFLAGS` reach only what is built for the image,
/// while the linker variable would also link the build scripts with
/// Buildroot's compiler when the image's CPU is the build machine's
/// (x86_64), and they could not run here. `inherited` is the caller's
/// `RUSTFLAGS` (CI's `-D warnings`).
pub fn cargo_env(
    p: &Paths,
    t: &Target,
    cross: &str,
    inherited: Option<&str>,
) -> Vec<(String, String)> {
    let tool = |name: &str| format!("{cross}{name}");
    let cc = t.cc_suffix();
    let linker = format!("-C linker={}", tool("gcc"));
    let rustflags = match inherited.map(str::trim).filter(|f| !f.is_empty()) {
        Some(f) => format!("{f} {linker}"),
        None => linker,
    };
    vec![
        ("RUSTFLAGS".into(), rustflags),
        (format!("CC_{cc}"), tool("gcc")),
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
    let cross = cross_prefix(p, t)?;
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
        .envs(cargo_env(
            p,
            t,
            &cross,
            std::env::var("RUSTFLAGS").ok().as_deref(),
        ));
    run(&mut cargo)?;
    let dir = prebuilt(p, t);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let bin = p.repo.join("target").join(t.rust).join("release/termos");
    std::fs::copy(&bin, dir.join("termos")).map_err(|e| format!("{}: {e}", bin.display()))?;
    std::fs::copy(p.repo.join("LICENSE"), dir.join("LICENSE"))
        .map_err(|e| format!("LICENSE: {e}"))?;
    // A new binary each time: let the package pick it up again.
    run(make(p, t).arg("termos-dirclean"))?;
    // The post-build script writes the version and the keyring into the
    // image (D-078).
    let keys = crate::keys::ensure(&p.work.join("keys"))?;
    let version = version()?;
    run(make(p, t)
        .env("TERMOS_IMAGE_VERSION", &version)
        .env("TERMOS_RAUC_KEYRING", &keys.keyring))?;
    if legal_info {
        run(make(p, t).arg("legal-info"))?;
    }
    // Its update bundle, as releases carry it.
    let bundle = crate::bundle::make(p, t, &keys, &crate::bundle::Variant::default())?;
    println!("image: the update bundle is {}", bundle.display());
    Ok(p.images(t))
}

/// The version images are built as: `TERMOS_IMAGE_VERSION` (releases set
/// it from their tag), else the workspace's.
pub fn version() -> Result<String, String> {
    let v = std::env::var("TERMOS_IMAGE_VERSION")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned());
    check_version(&v)?;
    Ok(v)
}

/// A version goes into file names, manifests and boot scripts: digits,
/// letters, `.`, `+` and `-` only, starting with a digit.
pub fn check_version(v: &str) -> Result<(), String> {
    let ok = v.starts_with(|c: char| c.is_ascii_digit())
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-'));
    if ok {
        Ok(())
    } else {
        Err(format!("`{v}` is not a version (like 0.2.0 or 0.2.0-rc.1)"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::targets::find;

    #[test]
    fn buildroots_tool_prefix_is_read_from_printvars() {
        assert_eq!(
            parse_cross("TARGET_CROSS=/o/host/bin/x86_64-linux-\n").as_deref(),
            Some("/o/host/bin/x86_64-linux-")
        );
        assert_eq!(
            parse_cross("noise\nTARGET_CROSS='/o/host/bin/aarch64-linux-'\n").as_deref(),
            Some("/o/host/bin/aarch64-linux-")
        );
        assert_eq!(parse_cross("TARGET_CROSS=\n"), None);
        assert_eq!(parse_cross(""), None);
    }

    #[test]
    fn versions_are_plain() {
        for good in ["0.2.0", "0.2.0-rc.1", "1.0.0+update-test"] {
            assert!(check_version(good).is_ok(), "{good}");
        }
        for bad in ["", "v0.2.0", "0.2 0", "0.2;rm", "0.2/../x"] {
            assert!(check_version(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn cargo_uses_buildroots_compiler_and_sysroot() {
        let p = Paths {
            repo: PathBuf::from("/r"),
            work: PathBuf::from("/r/target/image"),
            dl: PathBuf::from("/dl"),
        };
        let t = find("aarch64-virt").expect("target");
        let cross = "/r/target/image/aarch64-virt/host/bin/aarch64-linux-";
        let env = cargo_env(&p, t, cross, Some("-D warnings"));
        let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(
            get("RUSTFLAGS"),
            Some("-D warnings -C linker=/r/target/image/aarch64-virt/host/bin/aarch64-linux-gcc")
        );
        assert!(
            !env.iter().any(|(k, _)| k.ends_with("_LINKER")),
            "the build scripts keep the build machine's linker"
        );
        let plain = cargo_env(&p, t, cross, None);
        assert!(
            plain
                .iter()
                .any(|(k, v)| k == "RUSTFLAGS" && v.starts_with("-C linker="))
        );
        assert_eq!(
            get("CXX_aarch64_unknown_linux_gnu"),
            Some("/r/target/image/aarch64-virt/host/bin/aarch64-linux-g++")
        );
        assert_eq!(
            get("AR_aarch64_unknown_linux_gnu"),
            Some("/r/target/image/aarch64-virt/host/bin/aarch64-linux-ar")
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
