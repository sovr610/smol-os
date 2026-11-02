// SPDX-License-Identifier: GPL-3.0-or-later
//! The keys update bundles are signed with (D-078).
//!
//! Release builds use the owner's: `TERMOS_RAUC_KEYRING` (the CA images
//! trust), `TERMOS_RAUC_CERT` and `TERMOS_RAUC_KEY` (the code-signing
//! certificate and its key), paths CI writes from its secrets. Without them
//! a development CA and signing certificate are made with `openssl` in
//! `target/image/keys/`, never committed; images built with them accept
//! only bundles they signed.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Where the keys are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keys {
    /// The CA certificate images put in their keyring.
    pub keyring: PathBuf,
    /// The signing certificate and its key.
    pub cert: PathBuf,
    pub key: PathBuf,
    /// Made here for development, not the owner's.
    pub development: bool,
}

/// The owner's keys from the environment, if all three are set.
pub fn from_env(get: impl Fn(&str) -> Option<String>) -> Result<Option<Keys>, String> {
    let vars = ["TERMOS_RAUC_KEYRING", "TERMOS_RAUC_CERT", "TERMOS_RAUC_KEY"];
    let got: Vec<Option<String>> = vars
        .iter()
        .map(|v| get(v).filter(|s| !s.is_empty()))
        .collect();
    match got.as_slice() {
        [Some(keyring), Some(cert), Some(key)] => Ok(Some(Keys {
            keyring: keyring.into(),
            cert: cert.into(),
            key: key.into(),
            development: false,
        })),
        [None, None, None] => Ok(None),
        _ => Err(format!(
            "set all of {} for release signing, or none for development keys",
            vars.join(", ")
        )),
    }
}

/// The certificate extensions RAUC wants of a signer when it checks the
/// `codesign` purpose (system.conf's `check-purpose`).
pub const SIGNER_EXTENSIONS: &str = "\
basicConstraints = critical, CA:FALSE
keyUsage = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
subjectKeyIdentifier = hash
authorityKeyIdentifier = keyid, issuer
";

/// The keys to sign with: the owner's, or development ones in `dir`, made
/// on first use.
pub fn ensure(dir: &Path) -> Result<Keys, String> {
    if let Some(keys) = from_env(|v| std::env::var(v).ok())? {
        for f in [&keys.keyring, &keys.cert, &keys.key] {
            if !f.is_file() {
                return Err(format!("{}: no such file", f.display()));
            }
        }
        return Ok(keys);
    }
    let keys = Keys {
        keyring: dir.join("ca.pem"),
        cert: dir.join("signing.pem"),
        key: dir.join("signing.key"),
        development: true,
    };
    if keys.keyring.is_file() && keys.cert.is_file() && keys.key.is_file() {
        return Ok(keys);
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let openssl = |args: &[&str]| -> Result<(), String> {
        let out = Command::new("openssl")
            .args(args)
            .current_dir(dir)
            .output()
            .map_err(|e| format!("cannot run openssl: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "openssl {} failed: {}",
                args.first().unwrap_or(&""),
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    };
    println!(
        "image: making development signing keys in {}",
        dir.display()
    );
    openssl(&[
        "req",
        "-x509",
        "-newkey",
        "rsa:3072",
        "-nodes",
        "-days",
        "3650",
        "-subj",
        "/O=TermOS development/CN=TermOS development CA",
        "-addext",
        "basicConstraints=critical,CA:TRUE",
        "-addext",
        "keyUsage=critical,keyCertSign,cRLSign",
        "-keyout",
        "ca.key",
        "-out",
        "ca.pem",
    ])?;
    openssl(&[
        "req",
        "-newkey",
        "rsa:3072",
        "-nodes",
        "-subj",
        "/O=TermOS development/CN=TermOS development signing",
        "-keyout",
        "signing.key",
        "-out",
        "signing.csr",
    ])?;
    std::fs::write(dir.join("signing.ext"), SIGNER_EXTENSIONS)
        .map_err(|e| format!("signing.ext: {e}"))?;
    openssl(&[
        "x509",
        "-req",
        "-in",
        "signing.csr",
        "-CA",
        "ca.pem",
        "-CAkey",
        "ca.key",
        "-CAcreateserial",
        "-days",
        "3650",
        "-extfile",
        "signing.ext",
        "-out",
        "signing.pem",
    ])?;
    #[cfg(unix)]
    for k in ["ca.key", "signing.key"] {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(dir.join(k), std::fs::Permissions::from_mode(0o600));
    }
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_keys_come_all_or_none() {
        let none = from_env(|_| None).expect("ok");
        assert_eq!(none, None);
        let all = from_env(|v| Some(format!("/k/{v}")))
            .expect("ok")
            .expect("keys");
        assert_eq!(all.keyring, PathBuf::from("/k/TERMOS_RAUC_KEYRING"));
        assert!(!all.development);
        let some = from_env(|v| (v == "TERMOS_RAUC_KEY").then(|| "/k".to_owned()));
        assert!(some.expect_err("partial").contains("TERMOS_RAUC_CERT"));
    }

    #[test]
    fn development_keys_sign_for_code() {
        if Command::new("openssl").arg("version").output().is_err() {
            return;
        }
        let dir = tempfile_dir();
        let keys = ensure(&dir).expect("keys");
        assert!(keys.development);
        let text = Command::new("openssl")
            .args(["x509", "-noout", "-text", "-in"])
            .arg(&keys.cert)
            .output()
            .expect("openssl");
        let text = String::from_utf8_lossy(&text.stdout);
        assert!(text.contains("Code Signing"), "{text}");
        let verify = Command::new("openssl")
            .args(["verify", "-purpose", "any", "-CAfile"])
            .arg(&keys.keyring)
            .arg(&keys.cert)
            .output()
            .expect("openssl");
        assert!(
            verify.status.success(),
            "{}",
            String::from_utf8_lossy(&verify.stdout)
        );
        // Made once; the next call finds them.
        let again = ensure(&dir).expect("keys");
        assert_eq!(again, keys);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A fresh folder (the crate has no dependencies, not even tempfile).
    fn tempfile_dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "termos-keys-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        ));
        let d = PathBuf::from(d.to_string_lossy().replace([' ', '{', '}', ':', ','], "_"));
        std::fs::create_dir_all(&d).expect("mkdir");
        d
    }
}
