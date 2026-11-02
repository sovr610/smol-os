// SPDX-License-Identifier: GPL-3.0-or-later
//! The model cache, in `huggingface_hub`'s layout so other tools share
//! the downloads (D-021, D-066):
//!
//! ```text
//! <cache>/models--<org>--<name>/
//!   blobs/<sha256 or git blob id>          the content, once
//!   blobs/<…>.incomplete                   a download in progress
//!   snapshots/<commit>/<path>              links to blobs
//!   refs/<revision>                        the commit a branch pointed to
//! ```
//!
//! Downloads resume with HTTP range requests from what an interrupted one
//! left, and a file is kept only if its SHA-256 (LFS files) or git blob id
//! (small files) and size match what the Hub lists.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use futures_util::StreamExt as _;
use sha1::Digest as _;

use crate::hub::{Hub, HubError, RepoFile, net, safe_path};

/// The cache folder: `[models] cache`, else `$HF_HUB_CACHE`, else
/// `$HF_HOME/hub`, else `~/.cache/huggingface/hub`.
pub fn default_dir(config: Option<&Path>, get: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    if let Some(c) = config {
        return c.to_owned();
    }
    if let Some(c) = get("HF_HUB_CACHE").filter(|v| !v.is_empty()) {
        return PathBuf::from(c);
    }
    if let Some(h) = get("HF_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(h).join("hub");
    }
    let home = get(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).unwrap_or_default();
    PathBuf::from(home)
        .join(".cache")
        .join("huggingface")
        .join("hub")
}

/// Why a download failed.
#[derive(Debug, thiserror::Error)]
pub enum PullError {
    #[error(transparent)]
    Hub(#[from] HubError),
    #[error(
        "{path}: the download does not match the Hub's checksum (expected {expected}, got {got}); it was deleted"
    )]
    Checksum {
        path: String,
        expected: String,
        got: String,
    },
    #[error("{path}: expected {expected} bytes, got {got}")]
    Size {
        path: String,
        expected: u64,
        got: u64,
    },
    #[error("{0}")]
    Io(String),
}

fn io(e: impl std::fmt::Display) -> PullError {
    PullError::Io(e.to_string())
}

/// A cache.
#[derive(Debug, Clone)]
pub struct Cache {
    pub dir: PathBuf,
}

/// Progress of one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub done: u64,
    pub total: u64,
    /// Bytes an earlier, interrupted download had already fetched.
    pub resumed: u64,
}

impl Cache {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// A repository's folder.
    pub fn repo_dir(&self, repo: &str) -> PathBuf {
        self.dir
            .join(format!("models--{}", repo.replace('/', "--")))
    }

    pub fn snapshot_dir(&self, repo: &str, commit: &str) -> PathBuf {
        self.repo_dir(repo).join("snapshots").join(commit)
    }

    /// Downloads `file` of `repo` at `commit` into the cache (resuming an
    /// interrupted download) and links it into the snapshot. Returns the
    /// file's path in the snapshot.
    pub async fn pull(
        &self,
        hub: &Hub,
        repo: &str,
        commit: &str,
        file: &RepoFile,
        progress: &mut (dyn FnMut(Progress) + Send),
    ) -> Result<PathBuf, PullError> {
        if !safe_path(&file.path) {
            return Err(PullError::Io(format!("an unsafe file path: {}", file.path)));
        }
        let blobs = self.repo_dir(repo).join("blobs");
        std::fs::create_dir_all(&blobs).map_err(io)?;
        let blob = blobs.join(file.etag());
        let have = std::fs::metadata(&blob).map(|m| m.len()).ok();
        if have != Some(file.size) {
            self.download(hub, repo, commit, file, &blob, progress)
                .await?;
        } else {
            progress(Progress {
                done: file.size,
                total: file.size,
                resumed: file.size,
            });
        }
        let link = self.snapshot_dir(repo, commit).join(&file.path);
        link_blob(&blob, &link).map_err(io)?;
        Ok(link)
    }

    async fn download(
        &self,
        hub: &Hub,
        repo: &str,
        commit: &str,
        file: &RepoFile,
        blob: &Path,
        progress: &mut (dyn FnMut(Progress) + Send),
    ) -> Result<(), PullError> {
        let partial = blob.with_extension("incomplete");
        let mut offset = std::fs::metadata(&partial).map(|m| m.len()).unwrap_or(0);
        if offset > file.size {
            offset = 0;
        }
        let mut hasher = Hasher::new(file);
        if offset > 0 {
            // Hash what an earlier download already fetched.
            let mut f = std::fs::File::open(&partial).map_err(io)?;
            let mut buf = vec![0u8; 1 << 20];
            let mut left = offset;
            while left > 0 {
                let n = f.read(&mut buf).map_err(io)?;
                if n == 0 {
                    break;
                }
                let n = n.min(usize::try_from(left).unwrap_or(n));
                hasher.update(&buf[..n]);
                left -= n as u64;
            }
        }
        let resumed = offset;
        let mut req = hub.get(&hub.file_url(repo, commit, &file.path))?;
        if offset > 0 {
            req = req.header(reqwest::header::RANGE, format!("bytes={offset}-"));
        }
        let resp = req.send().await.map_err(|e| PullError::Hub(net(&e)))?;
        if resp.status().as_u16() == 416 && offset == file.size {
            // Nothing left to fetch: the earlier download was complete.
        } else {
            let resp = hub.check(resp, repo).await?;
            let mut out = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&partial)
                .map_err(io)?;
            if resp.status().as_u16() != 206 && offset > 0 {
                // The server ignored the range: start over.
                out.set_len(0).map_err(io)?;
                offset = 0;
                hasher = Hasher::new(file);
            }
            let mut done = offset;
            progress(Progress {
                done,
                total: file.size,
                resumed,
            });
            let mut stream = resp.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = out.sync_all();
                        return Err(PullError::Hub(HubError::Network(format!(
                            "{}: the download was interrupted at {done} of {} bytes ({}); run the same command to resume",
                            file.path,
                            file.size,
                            net(&e)
                        ))));
                    }
                };
                if done + chunk.len() as u64 > file.size {
                    return Err(PullError::Size {
                        path: file.path.clone(),
                        expected: file.size,
                        got: done + chunk.len() as u64,
                    });
                }
                out.write_all(&chunk).map_err(io)?;
                hasher.update(&chunk);
                done += chunk.len() as u64;
                progress(Progress {
                    done,
                    total: file.size,
                    resumed,
                });
            }
            out.sync_all().map_err(io)?;
            if done != file.size {
                // Cut off: keep the part for the next try.
                return Err(PullError::Hub(HubError::Network(format!(
                    "{}: the download stopped at {done} of {} bytes; run the same command to resume",
                    file.path, file.size
                ))));
            }
        }
        let got = hasher.finish();
        let expected = file.etag().to_ascii_lowercase();
        if got != expected {
            let _ = std::fs::remove_file(&partial);
            return Err(PullError::Checksum {
                path: file.path.clone(),
                expected,
                got,
            });
        }
        std::fs::rename(&partial, blob).map_err(io)?;
        Ok(())
    }

    /// Records the commit a branch or tag pointed to.
    pub fn set_ref(&self, repo: &str, revision: &str, commit: &str) -> Result<(), PullError> {
        if revision == commit || !safe_path(revision) || revision.contains('/') {
            return Ok(());
        }
        let refs = self.repo_dir(repo).join("refs");
        std::fs::create_dir_all(&refs).map_err(io)?;
        std::fs::write(refs.join(revision), commit).map_err(io)
    }

    /// Every cached repository.
    pub fn repos(&self) -> Vec<CachedRepo> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(&self.dir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(rest) = name.strip_prefix("models--") else {
                continue;
            };
            let repo = rest.replacen("--", "/", 1);
            let mut snapshots = Vec::new();
            for s in std::fs::read_dir(e.path().join("snapshots"))
                .into_iter()
                .flatten()
                .flatten()
            {
                let commit = s.file_name().to_string_lossy().into_owned();
                let mut files = Vec::new();
                collect_files(&s.path(), &s.path(), &mut files);
                files.sort();
                let used = std::fs::metadata(s.path().join(USED))
                    .and_then(|m| m.modified())
                    .or_else(|_| std::fs::metadata(s.path()).and_then(|m| m.modified()))
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                snapshots.push(Snapshot {
                    commit,
                    dir: s.path(),
                    files,
                    used,
                });
            }
            if snapshots.is_empty() {
                continue;
            }
            snapshots.sort_by_key(|s| std::cmp::Reverse(s.used));
            out.push(CachedRepo {
                repo,
                dir: e.path(),
                snapshots,
            });
        }
        out.sort_by(|a, b| a.repo.cmp(&b.repo));
        out
    }

    /// Removes `repo` (every snapshot, then blobs nothing links to).
    pub fn remove(&self, repo: &str) -> Result<u64, PullError> {
        let dir = self.repo_dir(repo);
        if !dir.exists() {
            return Ok(0);
        }
        let freed = dir_size(&dir);
        std::fs::remove_dir_all(&dir).map_err(io)?;
        Ok(freed)
    }

    /// Removes interrupted downloads older than a day and blobs no
    /// snapshot links to; then, over `limit`, the least recently used
    /// repositories. Returns the bytes freed and what was removed.
    pub fn gc(&self, limit: Option<u64>) -> (u64, Vec<String>) {
        let mut freed = 0;
        let mut removed = Vec::new();
        let day = std::time::Duration::from_secs(86_400);
        for r in self.repos() {
            let linked: Vec<PathBuf> = r
                .snapshots
                .iter()
                .flat_map(|s| {
                    s.files
                        .iter()
                        .map(|(_, p)| std::fs::canonicalize(s.dir.join(p)).unwrap_or_default())
                })
                .collect();
            for b in std::fs::read_dir(r.dir.join("blobs"))
                .into_iter()
                .flatten()
                .flatten()
            {
                let p = b.path();
                let meta = b.metadata().ok();
                let size = meta.as_ref().map_or(0, std::fs::Metadata::len);
                let stale = p.extension().is_some_and(|x| x == "incomplete")
                    && meta
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age > day);
                let orphan = p.extension().is_none()
                    && !linked.contains(&std::fs::canonicalize(&p).unwrap_or_default());
                if (stale || orphan) && std::fs::remove_file(&p).is_ok() {
                    freed += size;
                    removed.push(format!(
                        "{}: {}",
                        r.repo,
                        p.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    ));
                }
            }
        }
        if let Some(limit) = limit {
            let mut repos = self.repos();
            // Oldest use first.
            repos.sort_by_key(|r| r.snapshots.iter().map(|s| s.used).max());
            let mut total: u64 = repos.iter().map(|r| dir_size(&r.dir)).sum();
            for r in repos {
                if total <= limit {
                    break;
                }
                let size = dir_size(&r.dir);
                if std::fs::remove_dir_all(&r.dir).is_ok() {
                    total -= size.min(total);
                    freed += size;
                    removed.push(r.repo);
                }
            }
        }
        (freed, removed)
    }

    /// Total bytes in the cache.
    pub fn size(&self) -> u64 {
        dir_size(&self.dir)
    }
}

/// Touched when the router loads a model (for `gc`'s least recently used).
pub const USED: &str = ".termos-used";

/// Marks a snapshot as just used.
pub fn touch(snapshot: &Path) {
    let _ = std::fs::write(snapshot.join(USED), b"");
}

/// A cached repository.
#[derive(Debug, Clone)]
pub struct CachedRepo {
    pub repo: String,
    pub dir: PathBuf,
    /// Most recently used first.
    pub snapshots: Vec<Snapshot>,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub commit: String,
    pub dir: PathBuf,
    /// Relative path and size.
    pub files: Vec<(u64, String)>,
    pub used: SystemTime,
}

impl Snapshot {
    pub fn size(&self) -> u64 {
        self.files.iter().map(|(s, _)| s).sum()
    }
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<(u64, String)>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        let Ok(meta) = std::fs::metadata(&p) else {
            continue;
        };
        if meta.is_dir() {
            collect_files(root, &p, out);
        } else if e.file_name() != USED {
            let rel = p
                .strip_prefix(root)
                .unwrap_or(&p)
                .to_string_lossy()
                .replace('\\', "/");
            out.push((meta.len(), rel));
        }
    }
}

fn dir_size(dir: &Path) -> u64 {
    let mut total = 0;
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let Ok(meta) = std::fs::symlink_metadata(e.path()) else {
            continue;
        };
        if meta.is_dir() {
            total += dir_size(&e.path());
        } else if meta.is_file() {
            total += meta.len();
        }
    }
    total
}

/// Links `link` to `blob`: a relative symbolic link (as `huggingface_hub`
/// makes), else a hard link, else a copy (Windows without the right to
/// make symbolic links).
fn link_blob(blob: &Path, link: &Path) -> std::io::Result<()> {
    let dir = link
        .parent()
        .ok_or_else(|| std::io::Error::other("no parent folder"))?;
    std::fs::create_dir_all(dir)?;
    if std::fs::symlink_metadata(link).is_ok() {
        std::fs::remove_file(link)?;
    }
    #[cfg(unix)]
    {
        // snapshots/<commit>/<a/b/file> → ../../(../..)blobs/<etag>
        let depth = link
            .strip_prefix(
                blob.parent()
                    .and_then(Path::parent)
                    .unwrap_or(Path::new("/")),
            )
            .map(|p| p.components().count().saturating_sub(1))
            .unwrap_or(2);
        let mut rel = PathBuf::new();
        for _ in 0..depth {
            rel.push("..");
        }
        rel.push("blobs");
        rel.push(blob.file_name().unwrap_or_default());
        if std::os::unix::fs::symlink(&rel, link).is_ok() {
            return Ok(());
        }
    }
    #[cfg(windows)]
    if std::os::windows::fs::symlink_file(blob, link).is_ok() {
        return Ok(());
    }
    if std::fs::hard_link(blob, link).is_ok() {
        return Ok(());
    }
    std::fs::copy(blob, link).map(|_| ())
}

/// SHA-256 for LFS files, git's blob SHA-1 otherwise.
enum Hasher {
    Sha256(sha2::Sha256),
    Git(sha1::Sha1),
}

impl Hasher {
    fn new(file: &RepoFile) -> Self {
        if file.sha256.is_some() {
            Self::Sha256(sha2::Sha256::new())
        } else {
            let mut h = sha1::Sha1::new();
            h.update(format!("blob {}\0", file.size).as_bytes());
            Self::Git(h)
        }
    }

    fn update(&mut self, b: &[u8]) {
        match self {
            Self::Sha256(h) => sha2::Digest::update(h, b),
            Self::Git(h) => h.update(b),
        }
    }

    fn finish(self) -> String {
        let bytes: Vec<u8> = match self {
            Self::Sha256(h) => sha2::Digest::finalize(h).to_vec(),
            Self::Git(h) => h.finalize().to_vec(),
        };
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cache_folder() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        assert_eq!(
            default_dir(Some(Path::new("/c")), &env(&[("HF_HOME", "/h")])),
            PathBuf::from("/c")
        );
        assert_eq!(
            default_dir(None, &env(&[("HF_HUB_CACHE", "/x"), ("HF_HOME", "/h")])),
            PathBuf::from("/x")
        );
        assert_eq!(
            default_dir(None, &env(&[("HF_HOME", "/h")])),
            PathBuf::from("/h/hub")
        );
        let home = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        let got = default_dir(None, &move |k: &str| {
            (k == home).then(|| "/home/u".to_owned())
        });
        assert_eq!(got, PathBuf::from("/home/u/.cache/huggingface/hub"));
    }

    #[test]
    fn git_blob_ids() {
        // `git hash-object` of "hello\n".
        let f = RepoFile {
            path: "x".into(),
            size: 6,
            sha256: None,
            git_oid: String::new(),
        };
        let mut h = Hasher::new(&f);
        h.update(b"hello\n");
        assert_eq!(h.finish(), "ce013625030ba8dba906f756967f9e9ca394464a");
    }
}
