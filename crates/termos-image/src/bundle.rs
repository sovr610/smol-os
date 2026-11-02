// SPDX-License-Identifier: GPL-3.0-or-later
//! `cargo xtask image bundle <target>` (D-078): an update bundle for a
//! built image, made with Buildroot's host RAUC and signed with
//! [`crate::keys`].
//!
//! A bundle holds the root file system (kernel included) and a manifest
//! naming its target (`compatible`) and version, in RAUC's `verity` format.
//! The boot tests also make variants of the image's own root file system:
//! one with another version, to see an update arrive, and one with no init
//! at all, to see a failed update fall back.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::build::Paths;
use crate::keys::Keys;
use crate::targets::Target;

/// What goes into a bundle besides the image's root file system as built.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Variant {
    /// Another version in `/etc/termos/version` and the manifest.
    pub version: Option<String>,
    /// Without any init the kernel would try (`/sbin/init`, `/etc/init`,
    /// `/bin/init`, `/bin/sh`): it panics, and the slot is never good.
    pub broken: bool,
}

/// The manifest RAUC reads.
pub fn manifest(t: &Target, version: &str) -> String {
    format!(
        "[update]\ncompatible=termos-{}\nversion={version}\n\n[bundle]\nformat=verity\n\n[image.rootfs]\nfilename=rootfs.squashfs\n",
        t.name
    )
}

/// The bundle's file name, as releases name it (`termos update` looks for
/// `termos-<target>-<version>.raucb`).
pub fn file_name(t: &Target, version: &str) -> String {
    format!("termos-{}-{version}.raucb", t.name)
}

fn run(cmd: &mut Command) -> Result<(), String> {
    let shown = format!("{cmd:?}");
    println!("image: {shown}");
    let out = cmd
        .output()
        .map_err(|e| format!("cannot run {shown}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{shown} failed: {}{}",
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// The image's version, as its root file system says.
pub fn image_version(p: &Paths, t: &Target) -> Result<String, String> {
    let f = p.out(t).join("target/etc/termos/version");
    std::fs::read_to_string(&f)
        .map(|v| v.trim().to_owned())
        .map_err(|e| {
            format!(
                "{}: {e} (`cargo xtask image build {}`)",
                f.display(),
                t.name
            )
        })
}

/// Makes a bundle for `t`'s built image; returns its path in the images
/// folder.
pub fn make(p: &Paths, t: &Target, keys: &Keys, variant: &Variant) -> Result<PathBuf, String> {
    let host = p.out(t).join("host/bin");
    let images = p.images(t);
    let rootfs = images.join("rootfs.squashfs");
    if !rootfs.is_file() {
        return Err(format!("{}: build the image first", rootfs.display()));
    }
    let version = match &variant.version {
        Some(v) => v.clone(),
        None => image_version(p, t)?,
    };
    let stem = if variant.broken {
        format!("{version}-broken")
    } else {
        version.clone()
    };
    let work = p.out(t).join(format!("bundle-{stem}"));
    let _ = std::fs::remove_dir_all(&work);
    let content = work.join("content");
    std::fs::create_dir_all(&content).map_err(|e| format!("{}: {e}", content.display()))?;
    let target_rootfs = content.join("rootfs.squashfs");
    if *variant == Variant::default() {
        std::fs::copy(&rootfs, &target_rootfs).map_err(|e| format!("rootfs: {e}"))?;
    } else {
        // Unpack, change and pack again in one fakeroot session (Buildroot
        // builds it), so owners, modes and device nodes come back as they
        // were.
        let script = "set -e
            unsquashfs -quiet -no-xattrs -d \"$1\" \"$2\"
            if [ -n \"$4\" ]; then echo \"$4\" >\"$1/etc/termos/version\"; fi
            if [ \"$5\" = broken ]; then for f in sbin/init etc/init bin/init bin/sh init linuxrc; do rm -f \"$1/$f\"; done; fi
            mksquashfs \"$1\" \"$3\" -noappend -no-xattrs -quiet";
        let tree = work.join("tree");
        let mut path = std::ffi::OsString::from(&host);
        if let Some(rest) = std::env::var_os("PATH") {
            path.push(":");
            path.push(rest);
        }
        run(Command::new(host.join("fakeroot"))
            .env("PATH", path)
            .args(["--", "sh", "-c", script, "sh"])
            .arg(&tree)
            .arg(&rootfs)
            .arg(&target_rootfs)
            .arg(variant.version.as_deref().unwrap_or(""))
            .arg(if variant.broken { "broken" } else { "" }))?;
    }
    write(&content.join("manifest.raucm"), &manifest(t, &version))?;
    let out = images.join(if variant.broken {
        format!("termos-{}-{stem}.raucb", t.name)
    } else {
        file_name(t, &version)
    });
    let _ = std::fs::remove_file(&out);
    // No --keyring: RAUC would check the new signature with OpenSSL's
    // default purpose (S/MIME), which a code-signing certificate does not
    // have; the images check it with `check-purpose=codesign`.
    run(Command::new(host.join("rauc"))
        .arg("bundle")
        .arg(format!("--cert={}", keys.cert.display()))
        .arg(format!("--key={}", keys.key.display()))
        .arg(&content)
        .arg(&out))?;
    let _ = std::fs::remove_dir_all(&work);
    Ok(out)
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::targets::find;

    #[test]
    fn manifests_and_names() {
        let t = find("x86_64").expect("target");
        let m = manifest(t, "0.2.0");
        assert!(
            m.contains("compatible=termos-x86_64\nversion=0.2.0\n"),
            "{m}"
        );
        assert!(m.contains("[bundle]\nformat=verity\n"), "{m}");
        assert!(
            m.contains("[image.rootfs]\nfilename=rootfs.squashfs\n"),
            "{m}"
        );
        assert_eq!(file_name(t, "0.2.0"), "termos-x86_64-0.2.0.raucb");
    }
}
