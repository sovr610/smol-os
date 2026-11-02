// SPDX-License-Identifier: GPL-3.0-or-later
//! `termos system wifi-add|wifi-forget` (D-081): iwd's files for the Wi-Fi
//! networks the Network app saves. iwd watches its folder and joins a saved
//! network when it is in range; the passphrase arrives on standard input,
//! never in arguments, and the file is readable by root only.
// Only `termos system` on Linux uses it; the tests run everywhere.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::path::Path;

use anyhow::{Context as _, bail};
use secrecy::{ExposeSecret as _, SecretString};

/// Where iwd keeps networks (bound from the data partition, D-075).
pub(crate) const IWD_DIR: &str = "/var/lib/iwd";

const KINDS: [&str; 3] = ["psk", "open", "8021x"];

/// An SSID: 1 to 32 bytes, no control characters.
pub(crate) fn check_ssid(ssid: &str) -> anyhow::Result<()> {
    if ssid.is_empty() || ssid.len() > 32 || ssid.chars().any(char::is_control) {
        bail!("`{}` is not a Wi-Fi network name", ssid.escape_debug());
    }
    Ok(())
}

/// A WPA passphrase: 8 to 63 printable ASCII characters.
pub(crate) fn check_passphrase(p: &str) -> anyhow::Result<()> {
    if !(8..=63).contains(&p.len()) || !p.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
        bail!("a passphrase has 8 to 63 printable characters");
    }
    Ok(())
}

/// iwd's file name for a network, without its extension: the SSID itself
/// when it has only letters, digits, spaces, `_` and `-`, else `=` and its
/// bytes in hex.
pub(crate) fn file_stem(ssid: &str) -> String {
    if ssid
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '_' | '-'))
    {
        ssid.to_owned()
    } else {
        let hex: String = ssid.bytes().map(|b| format!("{b:02x}")).collect();
        format!("={hex}")
    }
}

/// A value as iwd's settings files read it back: backslashes doubled, a
/// leading space as `\s`.
fn escape(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\");
    match escaped.strip_prefix(' ') {
        Some(rest) => format!("\\s{rest}"),
        None => escaped,
    }
}

/// Saves a network in `dir`: a passphrase makes a `.psk` file, none an
/// `.open` one; another kind saved for it goes.
pub(crate) fn add(dir: &Path, ssid: &str, passphrase: Option<&SecretString>) -> anyhow::Result<()> {
    check_ssid(ssid)?;
    let stem = file_stem(ssid);
    let (kind, text) = match passphrase {
        Some(p) => {
            check_passphrase(p.expose_secret())?;
            (
                "psk",
                SecretString::from(format!(
                    "[Security]\nPassphrase={}\n",
                    escape(p.expose_secret())
                )),
            )
        }
        None => (
            "open",
            SecretString::from("[Settings]\nAutoConnect=true\n".to_owned()),
        ),
    };
    std::fs::create_dir_all(dir).with_context(|| dir.display().to_string())?;
    let file = dir.join(format!("{stem}.{kind}"));
    let tmp = dir.join(format!(".{stem}.{kind}.new"));
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let mut f = opts.open(&tmp).with_context(|| tmp.display().to_string())?;
        std::io::Write::write_all(&mut f, text.expose_secret().as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &file).with_context(|| file.display().to_string())?;
    for other in KINDS.iter().filter(|k| **k != kind) {
        let _ = std::fs::remove_file(dir.join(format!("{stem}.{other}")));
    }
    Ok(())
}

/// Forgets a network; false if it was not saved.
pub(crate) fn forget(dir: &Path, ssid: &str) -> anyhow::Result<bool> {
    check_ssid(ssid)?;
    let stem = file_stem(ssid);
    let mut any = false;
    for kind in KINDS {
        any |= std::fs::remove_file(dir.join(format!("{stem}.{kind}"))).is_ok();
    }
    Ok(any)
}

/// The passphrase from standard input (one line; empty for an open
/// network).
pub(crate) fn read_passphrase() -> anyhow::Result<Option<SecretString>> {
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let p = line.trim_end_matches(['\r', '\n']).to_owned();
    Ok((!p.is_empty()).then(|| SecretString::from(p)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_iwd() {
        assert_eq!(file_stem("Home Net_5-G"), "Home Net_5-G");
        assert_eq!(file_stem("Café"), "=436166c3a9");
        assert_eq!(file_stem("a/b"), "=612f62");
        assert!(check_ssid("").is_err());
        assert!(check_ssid(&"x".repeat(33)).is_err());
        assert!(check_ssid("tab\there").is_err());
        assert!(check_passphrase("short").is_err());
        assert!(check_passphrase("tab\tinside!").is_err());
        assert!(check_passphrase("correct horse battery").is_ok());
        assert_eq!(escape(" lead\\back"), "\\slead\\\\back");
    }

    #[test]
    fn networks_are_saved_and_forgotten() {
        let dir = tempfile::tempdir().expect("dir");
        let p = SecretString::from("correct horse".to_owned());
        add(dir.path(), "Home", Some(&p)).expect("add");
        let file = dir.path().join("Home.psk");
        assert_eq!(
            std::fs::read_to_string(&file).expect("read"),
            "[Security]\nPassphrase=correct horse\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&file).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // Saved again as open: the passphrase file goes.
        add(dir.path(), "Home", None).expect("add");
        assert!(!file.exists());
        assert!(dir.path().join("Home.open").is_file());
        assert!(
            add(
                dir.path(),
                "Home",
                Some(&SecretString::from("short".to_owned()))
            )
            .is_err()
        );
        assert!(forget(dir.path(), "Home").expect("forget"));
        assert!(!forget(dir.path(), "Home").expect("forget"));
        assert_eq!(std::fs::read_dir(dir.path()).expect("ls").count(), 0);
    }
}
