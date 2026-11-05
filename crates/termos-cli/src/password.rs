// SPDX-License-Identifier: GPL-3.0-or-later
//! The lock screen's password (M13d, D-123, D-124): the system user's, in
//! `/etc/shadow`.
//!
//! The image's root is read-only, so `/etc/shadow` links to
//! `/data/etc/shadow` and `termos system set-password` writes that file
//! itself (a new file renamed over the old): `chpasswd` would write next to
//! the link, on the read-only root. Hashes are sha512crypt (`$6$`), the
//! method the image's shadow suite uses (`ENCRYPT_METHOD SHA512`).
//!
//! `termos password` is the user's side: it reads the new password twice
//! without echo and hands it to `sudo termos system set-password`.

// Only TermOS images (Linux) run the root side; the tests run everywhere.
#![cfg_attr(not(target_os = "linux"), allow(dead_code, unused_imports))]

use std::io::{BufRead as _, Write as _};
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context as _, bail};
use secrecy::{ExposeSecret as _, SecretString};

/// Where the system keeps its users' password hashes.
pub(crate) const SHADOW: &str = "/etc/shadow";

/// What `/etc/shadow` holds for one user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Stored {
    /// No password: an empty field, or `!` / `*` alone (the image's user
    /// before one is set).
    None,
    /// A hash (a `!` before it, as `usermod -L` puts, is dropped: the lock
    /// still asks for that password).
    Hash(String),
}

/// The entry for `user` in `shadow`'s text; `None` when there is none.
pub(crate) fn stored(shadow: &str, user: &str) -> Option<Stored> {
    shadow.lines().find_map(|l| {
        let mut f = l.split(':');
        if f.next()? != user {
            return None;
        }
        let field = f.next().unwrap_or("");
        let hash = field.trim_start_matches('!');
        Some(if hash.starts_with('$') {
            Stored::Hash(hash.to_owned())
        } else {
            Stored::None
        })
    })
}

/// Whether `password` is the one `hash` was made from. Only sha512crypt
/// (`$6$`) and sha256crypt (`$5$`) hashes are known; others never match.
pub(crate) fn matches(password: &str, hash: &str) -> bool {
    use sha_crypt::password_hash::PasswordVerifier as _;
    if !(hash.starts_with("$6$") || hash.starts_with("$5$")) {
        return false;
    }
    let Ok(parsed) = sha_crypt::PasswordHash::new(hash) else {
        return false;
    };
    sha_crypt::ShaCrypt::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

/// A new sha512crypt hash of `password`, with a random salt.
pub(crate) fn hash(password: &str) -> anyhow::Result<String> {
    use sha_crypt::password_hash::PasswordHasher as _;
    let h = sha_crypt::ShaCrypt::default()
        .hash_password(password.as_bytes())
        .map_err(|e| anyhow::anyhow!("cannot hash the password: {e}"))?;
    Ok(h.as_str().to_owned())
}

/// `shadow`'s text with `user`'s hash replaced by `hash` (`!` for no
/// password) and its last change set to `days` since 1970.
pub(crate) fn with_hash(shadow: &str, user: &str, hash: &str, days: u64) -> Option<String> {
    let days = days.to_string();
    let mut found = false;
    let mut out = String::with_capacity(shadow.len() + hash.len());
    for line in shadow.lines() {
        let mut f: Vec<&str> = line.split(':').collect();
        if f.first() == Some(&user) && f.len() >= 2 {
            found = true;
            f[1] = hash;
            if let Some(changed) = f.get_mut(2) {
                *changed = &days;
            }
            out.push_str(&f.join(":"));
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    found.then_some(out)
}

/// The user `termos system` acts for: the one sudo ran it for.
fn sudo_user() -> anyhow::Result<String> {
    match std::env::var("SUDO_USER") {
        Ok(u) if !u.is_empty() && u != "root" => Ok(u),
        _ => bail!("run through sudo, as the desktop's user"),
    }
}

/// Reads one line of standard input as a secret (the trailing newline
/// dropped).
fn read_secret_line() -> anyhow::Result<SecretString> {
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .context("reading the password")?;
    let trimmed = line.trim_end_matches(['\r', '\n']).to_owned();
    // The buffer held the password too: clear it before it is freed.
    line.clear();
    line.shrink_to_fit();
    Ok(SecretString::from(trimmed))
}

/// `termos system has-password`: success when the user has a password.
pub(crate) fn has_password(shadow: &Path) -> anyhow::Result<ExitCode> {
    let user = sudo_user()?;
    let text = std::fs::read_to_string(shadow).with_context(|| shadow.display().to_string())?;
    Ok(match stored(&text, &user) {
        Some(Stored::Hash(_)) => ExitCode::SUCCESS,
        _ => ExitCode::from(1),
    })
}

/// `termos system check-password`: the password on standard input against
/// the user's. Success when it matches (or when no password is set and
/// the input is empty); a wrong one waits a second first, so guessing
/// through this verb is slow too.
pub(crate) fn check_password(shadow: &Path) -> anyhow::Result<ExitCode> {
    let user = sudo_user()?;
    let given = read_secret_line()?;
    let text = std::fs::read_to_string(shadow).with_context(|| shadow.display().to_string())?;
    let ok = match stored(&text, &user) {
        Some(Stored::Hash(h)) => matches(given.expose_secret(), &h),
        Some(Stored::None) => given.expose_secret().is_empty(),
        None => false,
    };
    if ok {
        return Ok(ExitCode::SUCCESS);
    }
    std::thread::sleep(std::time::Duration::from_secs(1));
    Ok(ExitCode::from(1))
}

/// `termos system set-password`: the password on standard input becomes
/// the user's (an empty one removes it). `shadow` is followed to the file
/// it links to, and that file is replaced.
#[cfg(unix)]
pub(crate) fn set_password(shadow: &Path) -> anyhow::Result<ExitCode> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let user = sudo_user()?;
    let given = read_secret_line()?;
    let real = std::fs::canonicalize(shadow).with_context(|| shadow.display().to_string())?;
    let text = std::fs::read_to_string(&real).with_context(|| real.display().to_string())?;
    let new_hash = if given.expose_secret().is_empty() {
        "!".to_owned()
    } else {
        hash(given.expose_secret())?
    };
    let days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() / 86_400);
    let Some(new) = with_hash(&text, &user, &new_hash, days) else {
        bail!("{user} is not in {}", real.display());
    };
    let tmp = real.with_extension("termos-new");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .with_context(|| tmp.display().to_string())?;
    f.write_all(new.as_bytes())?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, &real).with_context(|| real.display().to_string())?;
    Ok(ExitCode::SUCCESS)
}

/// Reads a password from the terminal without echoing it.
#[cfg(unix)]
fn read_hidden(prompt: &str) -> anyhow::Result<SecretString> {
    use rustix::termios::{LocalModes, OptionalActions, tcgetattr, tcsetattr};
    print!("{prompt}");
    std::io::stdout().flush()?;
    let stdin = std::io::stdin();
    let saved = tcgetattr(&stdin).ok();
    if let Some(t) = &saved {
        let mut quiet = t.clone();
        quiet.local_modes.remove(LocalModes::ECHO);
        let _ = tcsetattr(&stdin, OptionalActions::Now, &quiet);
    }
    let result = read_secret_line();
    if let Some(t) = &saved {
        let _ = tcsetattr(&stdin, OptionalActions::Now, t);
    }
    println!();
    result
}

/// `termos password`: sets the lock screen's password on a TermOS image
/// (the user's system password).
#[cfg(unix)]
pub(crate) fn password_command(remove: bool) -> anyhow::Result<ExitCode> {
    if !Path::new("/etc/termos/board").exists() {
        bail!(
            "`termos password` sets the lock screen's password on a TermOS image; \
             in a terminal, the host system's lock and password apply"
        );
    }
    let new = if remove {
        SecretString::from(String::new())
    } else {
        let a = read_hidden("New password: ")?;
        let b = read_hidden("Again: ")?;
        if a.expose_secret() != b.expose_secret() {
            bail!("the two passwords differ; nothing changed");
        }
        if a.expose_secret().is_empty() {
            bail!("an empty password: use `termos password --remove` to have none");
        }
        a
    };
    let mut child = std::process::Command::new("sudo")
        .args(["-n", "/usr/bin/termos", "system", "set-password"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .context("running sudo")?;
    {
        let mut stdin = child.stdin.take().context("sudo's input")?;
        stdin.write_all(new.expose_secret().as_bytes())?;
        stdin.write_all(b"\n")?;
    }
    if !child.wait()?.success() {
        bail!("the password was not changed");
    }
    println!(
        "{}",
        if remove {
            "The password is removed: anyone at the keyboard can unlock the screen."
        } else {
            "The password is set; the lock screen asks for it."
        }
    );
    Ok(ExitCode::SUCCESS)
}

/// Windows: TermOS images are Linux.
#[cfg(not(unix))]
pub(crate) fn password_command(_remove: bool) -> anyhow::Result<ExitCode> {
    bail!("`termos password` sets the lock screen's password on a TermOS image")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// glibc's own sha512crypt test vectors (`openssl passwd -6` gives the
    /// same).
    const HELLO: &str = "$6$saltstring$svn8UoSVapNtMuq1ukKS4tPQd8iKwSMHWjl/O817G3uBnIFNjnQJuesI68u4OTLiBFdcbYEdFCoEOfaS35inz1";
    const HELLO_ROUNDS: &str = "$6$rounds=10000$saltstringsaltst$OW1/O6BYHV6BcXZu8QVeXbDWra3Oeqh0sbHbbMCVNSnCM/UrjmM0Dp8vOuZeHBy/YTBmSK6H9qs/y3RnOaw5v.";

    fn shadow(field: &str) -> String {
        format!("root:*:19000:0:99999:7:::\ntermos:{field}:19000:0:99999:7:::\n")
    }

    #[test]
    fn what_shadow_holds() {
        assert_eq!(stored(&shadow("!"), "termos"), Some(Stored::None));
        assert_eq!(stored(&shadow(""), "termos"), Some(Stored::None));
        assert_eq!(stored(&shadow("*"), "termos"), Some(Stored::None));
        assert_eq!(
            stored(&shadow(HELLO), "termos"),
            Some(Stored::Hash(HELLO.into()))
        );
        assert_eq!(
            stored(&shadow(&format!("!{HELLO}")), "termos"),
            Some(Stored::Hash(HELLO.into())),
            "usermod -L's lock keeps the password"
        );
        assert_eq!(stored(&shadow("!"), "nobody"), None);
    }

    #[test]
    fn hashes_written_by_the_system_are_checked() {
        assert!(matches("Hello world!", HELLO));
        assert!(matches("Hello world!", HELLO_ROUNDS));
        assert!(!matches("Hello world", HELLO));
        assert!(!matches("", HELLO));
        // MD5 and DES hashes are not trusted.
        assert!(!matches("x", "$1$abc$def"));
        assert!(!matches("x", "abJnggxhB/yWI"));
        assert!(!matches("x", "$6$broken"));
    }

    #[test]
    fn a_new_hash_checks_and_replaces_the_old() {
        let h = hash("correct horse").expect("hashed");
        assert!(h.starts_with("$6$"), "{h}");
        assert!(matches("correct horse", &h));
        assert!(!matches("correct horse ", &h));
        let next = with_hash(&shadow("!"), "termos", &h, 20_400).expect("found");
        assert!(next.starts_with("root:*:19000:"), "others are kept");
        assert!(next.contains(&format!("termos:{h}:20400:0:99999:7:::\n")));
        assert_eq!(stored(&next, "termos"), Some(Stored::Hash(h)));
        assert_eq!(with_hash(&shadow("!"), "nobody", "!", 1), None);
        let removed = with_hash(&next, "termos", "!", 20_401).expect("found");
        assert_eq!(stored(&removed, "termos"), Some(Stored::None));
    }
}
