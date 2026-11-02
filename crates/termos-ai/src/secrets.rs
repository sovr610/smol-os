// SPDX-License-Identifier: GPL-3.0-or-later
//! API keys: the OS keyring, or an age file locked with a passphrase
//! (D-017, D-058).
//!
//! Values are [`SecretString`]s from the moment they are read: their
//! `Debug` output is redacted, and they are wiped when dropped. They are
//! never logged, written to the config, to snapshots or to audit records.
//!
//! - **Keyring:** service `termos`, one entry per name (Secret Service on
//!   Linux, Keychain on macOS, Credential Manager on Windows). Keyring
//!   calls may block (D-Bus), so callers run them off the event loop.
//! - **File:** `<data>/secrets.age`, an age v1 file encrypted to a
//!   passphrase (scrypt), holding a JSON map of names to values. It is
//!   unlocked once and kept in memory for the life of the process.

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use termos_core::config::agents::{SecretStoreKind, valid_secret_name};

/// The keyring service name entries are kept under.
pub const KEYRING_SERVICE: &str = "termos";

/// The secrets file in TermOS's data folder.
pub fn file_path(data_dir: &Path) -> PathBuf {
    data_dir.join("secrets.age")
}

/// Why a secret could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("no secret named `{0}`: store it with `termos secret set {0}`")]
    NotFound(String),
    #[error("the secrets file is locked: enter its passphrase")]
    Locked,
    #[error("there is no secrets file yet: `termos secret set <name>` creates one")]
    NoFile,
    #[error("wrong passphrase for the secrets file")]
    BadPassphrase,
    #[error("invalid secret name `{0}`: use letters, digits, `.`, `-` and `_`")]
    BadName(String),
    #[error("the OS keyring is not available: {0}")]
    NoKeyring(String),
    #[error("keyring: {0}")]
    Keyring(String),
    #[error("{}: {message}", path.display())]
    File { path: PathBuf, message: String },
}

/// Which store a [`SecretStore`] uses after `auto` is resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Keyring,
    File,
}

impl std::fmt::Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Keyring => "the OS keyring",
            Self::File => "the secrets file",
        })
    }
}

/// The decrypted secrets file.
struct Unlocked {
    passphrase: SecretString,
    values: BTreeMap<String, SecretString>,
}

#[derive(Serialize, Deserialize)]
struct FileBody {
    version: u32,
    secrets: BTreeMap<String, String>,
}

/// Where secrets are read and written.
pub struct SecretStore {
    backend: Backend,
    path: PathBuf,
    unlocked: Mutex<Option<Unlocked>>,
    /// scrypt work factor (log2 N) for writing the file; `None`: age's
    /// default (about a second of work).
    work_factor: Option<u8>,
}

impl std::fmt::Debug for SecretStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretStore")
            .field("backend", &self.backend)
            .field("path", &self.path)
            .field("locked", &self.is_locked())
            .finish_non_exhaustive()
    }
}

impl SecretStore {
    /// The store `kind` names. `auto` is the secrets file if it exists,
    /// else the keyring when it answers, else the file. Resolving `auto`
    /// may ask the keyring service (a D-Bus call), so call this off the
    /// event loop.
    pub fn open(kind: SecretStoreKind, data_dir: &Path) -> Self {
        let path = file_path(data_dir);
        let backend = match kind {
            SecretStoreKind::Keyring => Backend::Keyring,
            SecretStoreKind::File => Backend::File,
            SecretStoreKind::Auto if path.exists() => Backend::File,
            SecretStoreKind::Auto => match keyring_works() {
                Ok(()) => Backend::Keyring,
                Err(e) => {
                    tracing::info!("using the secrets file: the keyring is not available: {e}");
                    Backend::File
                }
            },
        };
        Self::with_backend(backend, path)
    }

    /// A store using `backend`, with the secrets file at `path`.
    pub fn with_backend(backend: Backend, path: PathBuf) -> Self {
        Self {
            backend,
            path,
            unlocked: Mutex::new(None),
            work_factor: None,
        }
    }

    /// Writes the file with a cheaper scrypt work factor (tests).
    #[must_use]
    pub fn with_work_factor(mut self, log_n: u8) -> Self {
        self.work_factor = Some(log_n);
        self
    }

    pub fn backend(&self) -> Backend {
        self.backend
    }

    pub fn file(&self) -> &Path {
        &self.path
    }

    /// Whether the secrets file exists.
    pub fn file_exists(&self) -> bool {
        self.path.exists()
    }

    /// Whether reading needs [`SecretStore::unlock`] first.
    pub fn is_locked(&self) -> bool {
        self.backend == Backend::File && self.lock().is_none()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Unlocked>> {
        // A panic while holding the lock leaves plain data behind.
        self.unlocked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Unlocks the secrets file with its passphrase.
    pub fn unlock(&self, passphrase: SecretString) -> Result<(), SecretError> {
        let bytes = match std::fs::read(&self.path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(SecretError::NoFile),
            Err(e) => return Err(self.file_err(e)),
        };
        let values = decrypt(&bytes, &passphrase).map_err(|e| match e {
            DecryptFailure::Passphrase => SecretError::BadPassphrase,
            DecryptFailure::Other(message) => SecretError::File {
                path: self.path.clone(),
                message,
            },
        })?;
        *self.lock() = Some(Unlocked { passphrase, values });
        Ok(())
    }

    /// Creates an empty secrets file locked with `passphrase` (and
    /// unlocks it). Refuses to replace an existing file.
    pub fn create(&self, passphrase: SecretString) -> Result<(), SecretError> {
        if self.path.exists() {
            return Err(self.file_err("already exists"));
        }
        let unlocked = Unlocked {
            passphrase,
            values: BTreeMap::new(),
        };
        self.write_file(&unlocked)?;
        *self.lock() = Some(unlocked);
        Ok(())
    }

    /// Forgets the decrypted file (the next read needs the passphrase).
    pub fn relock(&self) {
        *self.lock() = None;
    }

    /// The secret called `name`.
    pub fn get(&self, name: &str) -> Result<SecretString, SecretError> {
        check_name(name)?;
        match self.backend {
            Backend::Keyring => {
                let entry = keyring_entry(name)?;
                match entry.get_password() {
                    Ok(v) => Ok(SecretString::from(v)),
                    Err(keyring::Error::NoEntry) => Err(SecretError::NotFound(name.to_owned())),
                    Err(e) => Err(keyring_err(&e)),
                }
            }
            Backend::File => {
                let guard = self.lock();
                let unlocked = match guard.as_ref() {
                    Some(u) => u,
                    None if self.path.exists() => return Err(SecretError::Locked),
                    None => return Err(SecretError::NotFound(name.to_owned())),
                };
                unlocked
                    .values
                    .get(name)
                    .map(|v| SecretString::from(v.expose_secret().to_owned()))
                    .ok_or_else(|| SecretError::NotFound(name.to_owned()))
            }
        }
    }

    /// Stores `value` as `name` (replacing any old value). The file store
    /// must be unlocked (or created) first.
    pub fn set(&self, name: &str, value: SecretString) -> Result<(), SecretError> {
        check_name(name)?;
        match self.backend {
            Backend::Keyring => keyring_entry(name)?
                .set_password(value.expose_secret())
                .map_err(|e| keyring_err(&e)),
            Backend::File => {
                let mut guard = self.lock();
                let Some(unlocked) = guard.as_mut() else {
                    return Err(if self.path.exists() {
                        SecretError::Locked
                    } else {
                        SecretError::NoFile
                    });
                };
                let old = unlocked.values.insert(name.to_owned(), value);
                if let Err(e) = self.write_file(unlocked) {
                    // Keep memory and disk the same.
                    match old {
                        Some(v) => unlocked.values.insert(name.to_owned(), v),
                        None => unlocked.values.remove(name),
                    };
                    return Err(e);
                }
                Ok(())
            }
        }
    }

    /// Removes `name`; whether it existed.
    pub fn remove(&self, name: &str) -> Result<bool, SecretError> {
        check_name(name)?;
        match self.backend {
            Backend::Keyring => match keyring_entry(name)?.delete_credential() {
                Ok(()) => Ok(true),
                Err(keyring::Error::NoEntry) => Ok(false),
                Err(e) => Err(keyring_err(&e)),
            },
            Backend::File => {
                let mut guard = self.lock();
                let Some(unlocked) = guard.as_mut() else {
                    return if self.path.exists() {
                        Err(SecretError::Locked)
                    } else {
                        Ok(false)
                    };
                };
                let Some(old) = unlocked.values.remove(name) else {
                    return Ok(false);
                };
                if let Err(e) = self.write_file(unlocked) {
                    unlocked.values.insert(name.to_owned(), old);
                    return Err(e);
                }
                Ok(true)
            }
        }
    }

    /// The names in the secrets file (it must be unlocked). The keyring
    /// cannot list what it holds: `None`.
    pub fn names(&self) -> Result<Option<Vec<String>>, SecretError> {
        match self.backend {
            Backend::Keyring => Ok(None),
            Backend::File => match self.lock().as_ref() {
                Some(u) => Ok(Some(u.values.keys().cloned().collect())),
                None if self.path.exists() => Err(SecretError::Locked),
                None => Ok(Some(Vec::new())),
            },
        }
    }

    fn write_file(&self, unlocked: &Unlocked) -> Result<(), SecretError> {
        let body = FileBody {
            version: 1,
            secrets: unlocked
                .values
                .iter()
                .map(|(k, v)| (k.clone(), v.expose_secret().to_owned()))
                .collect(),
        };
        let json = SecretString::from(
            serde_json::to_string(&body).map_err(|e| self.file_err(e.to_string()))?,
        );
        let bytes = encrypt(
            json.expose_secret().as_bytes(),
            &unlocked.passphrase,
            self.work_factor,
        )
        .map_err(|e| self.file_err(e))?;
        write_private(&self.path, &bytes).map_err(|e| self.file_err(e))
    }

    fn file_err(&self, e: impl std::fmt::Display) -> SecretError {
        SecretError::File {
            path: self.path.clone(),
            message: e.to_string(),
        }
    }
}

fn check_name(name: &str) -> Result<(), SecretError> {
    if valid_secret_name(name) {
        Ok(())
    } else {
        Err(SecretError::BadName(name.to_owned()))
    }
}

fn keyring_entry(name: &str) -> Result<keyring::Entry, SecretError> {
    keyring::Entry::new(KEYRING_SERVICE, name).map_err(|e| match e {
        keyring::Error::NoDefaultStore => SecretError::NoKeyring(store_problem()),
        e => keyring_err(&e),
    })
}

fn store_problem() -> String {
    match keyring::Entry::store_status() {
        Ok(()) => "no store".into(),
        Err(e) => e.to_string(),
    }
}

fn keyring_err(e: &keyring::Error) -> SecretError {
    match e {
        keyring::Error::NoStorageAccess(_) | keyring::Error::NoDefaultStore => {
            SecretError::NoKeyring(e.to_string())
        }
        e => SecretError::Keyring(e.to_string()),
    }
}

/// Whether the keyring answers (a missing probe entry is an answer).
fn keyring_works() -> Result<(), SecretError> {
    match keyring_entry(".termos-probe")?.get_password() {
        Ok(_) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(keyring_err(&e)),
    }
}

enum DecryptFailure {
    Passphrase,
    Other(String),
}

fn decrypt(
    bytes: &[u8],
    passphrase: &SecretString,
) -> Result<BTreeMap<String, SecretString>, DecryptFailure> {
    let other = |e: &dyn std::fmt::Display| DecryptFailure::Other(e.to_string());
    let decryptor = age::Decryptor::new_buffered(bytes).map_err(|e| other(&e))?;
    if !decryptor.is_scrypt() {
        return Err(DecryptFailure::Other(
            "not a passphrase-encrypted age file".into(),
        ));
    }
    let identity =
        age::scrypt::Identity::new(SecretString::from(passphrase.expose_secret().to_owned()));
    let mut reader = decryptor
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .map_err(|e| match e {
            age::DecryptError::DecryptionFailed | age::DecryptError::NoMatchingKeys => {
                DecryptFailure::Passphrase
            }
            e => other(&e),
        })?;
    let mut plain = Vec::new();
    reader.read_to_end(&mut plain).map_err(|e| other(&e))?;
    let plain = SecretString::from(
        String::from_utf8(plain).map_err(|_| DecryptFailure::Other("not UTF-8".into()))?,
    );
    let body: FileBody = serde_json::from_str(plain.expose_secret())
        .map_err(|_| DecryptFailure::Other("not a TermOS secrets file".into()))?;
    if body.version != 1 {
        return Err(DecryptFailure::Other(format!(
            "secrets file version {} is newer than this TermOS",
            body.version
        )));
    }
    Ok(body
        .secrets
        .into_iter()
        .map(|(k, v)| (k, SecretString::from(v)))
        .collect())
}

fn encrypt(
    plain: &[u8],
    passphrase: &SecretString,
    work_factor: Option<u8>,
) -> Result<Vec<u8>, String> {
    let mut recipient =
        age::scrypt::Recipient::new(SecretString::from(passphrase.expose_secret().to_owned()));
    if let Some(log_n) = work_factor {
        recipient.set_work_factor(log_n);
    }
    let encryptor =
        age::Encryptor::with_recipients(std::iter::once(&recipient as &dyn age::Recipient))
            .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let mut writer = encryptor.wrap_output(&mut out).map_err(|e| e.to_string())?;
    writer.write_all(plain).map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())?;
    Ok(out)
}

/// Writes `bytes` to `path` atomically, readable only by us (0600 in a
/// 0700 folder on Unix).
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("no parent folder"))?;
    create_private_dir(dir)?;
    let tmp = dir.join(format!(
        ".{}.tmp{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
        std::process::id()
    ));
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Creates `dir` (and its parents) with mode 0700 on Unix.
pub(crate) fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(dir: &Path) -> SecretStore {
        SecretStore::with_backend(Backend::File, dir.join("secrets.age")).with_work_factor(2)
    }

    fn s(v: &str) -> SecretString {
        SecretString::from(v.to_owned())
    }

    #[test]
    fn file_store_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let st = store(dir.path());
        assert!(matches!(st.set("a", s("1")), Err(SecretError::NoFile)));
        st.create(s("pass phrase")).expect("create");
        st.set("anthropic", s("sk-ant-canary")).expect("set");
        st.set("openai", s("sk-other")).expect("set");
        assert_eq!(
            st.get("anthropic").expect("get").expose_secret(),
            "sk-ant-canary"
        );
        assert_eq!(
            st.names().expect("names"),
            Some(vec!["anthropic".into(), "openai".into()])
        );

        // A fresh store sees the file, locked; a wrong passphrase fails.
        let again = store(dir.path());
        assert!(again.is_locked());
        assert!(matches!(again.get("anthropic"), Err(SecretError::Locked)));
        assert!(matches!(
            again.unlock(s("nope")),
            Err(SecretError::BadPassphrase)
        ));
        again.unlock(s("pass phrase")).expect("unlock");
        assert_eq!(
            again.get("openai").expect("get").expose_secret(),
            "sk-other"
        );
        assert!(again.remove("openai").expect("remove"));
        assert!(!again.remove("openai").expect("gone"));
        assert!(matches!(again.get("openai"), Err(SecretError::NotFound(_))));
        assert!(matches!(
            again.get("bad/name"),
            Err(SecretError::BadName(_))
        ));

        // On disk it is an age file: no value in the clear.
        let raw = std::fs::read(dir.path().join("secrets.age")).expect("file");
        assert!(raw.starts_with(b"age-encryption.org/v1"));
        assert!(!raw.windows(6).any(|w| w == b"canary"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("secrets.age"))
                .expect("meta")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(st.create(s("x")).is_err(), "never replaces a file");
    }

    #[test]
    fn debug_never_shows_values() {
        let dir = tempfile::tempdir().expect("tempdir");
        let st = store(dir.path());
        st.create(s("pp")).expect("create");
        st.set("k", s("sk-ant-canary")).expect("set");
        let shown = format!("{st:?} {:?}", st.get("k").expect("get"));
        assert!(!shown.contains("canary"), "{shown}");
    }
}
