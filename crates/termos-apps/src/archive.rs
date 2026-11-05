// SPDX-License-Identifier: GPL-3.0-or-later
//! Archives in the file manager (M13c, D-122): zip, tar, tar.gz and
//! tar.zst browsed like folders (read-only), extracted, and zips made.
//!
//! Names that would leave the folder they are extracted into (absolute
//! paths, `..`) are refused, as in D-112; links and device files are
//! skipped.

use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};

/// What kind of archive a file is, by its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Zip,
    Tar,
    TarGz,
    TarZst,
}

/// An archive's kind from its file name.
pub fn kind_of(path: &Path) -> Option<Kind> {
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    if name.ends_with(".zip") {
        Some(Kind::Zip)
    } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        Some(Kind::TarGz)
    } else if name.ends_with(".tar.zst") || name.ends_with(".tzst") {
        Some(Kind::TarZst)
    } else if name.ends_with(".tar") {
        Some(Kind::Tar)
    } else {
        None
    }
}

/// The archive's name without its archive extension (`a.tar.gz` → `a`).
pub fn stem(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let lower = name.to_ascii_lowercase();
    for ext in [".tar.gz", ".tar.zst", ".tgz", ".tzst", ".tar", ".zip"] {
        if lower.ends_with(ext) && name.len() > ext.len() {
            return name[..name.len() - ext.len()].to_owned();
        }
    }
    name
}

/// One member of an archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// Its path inside the archive, `/`-separated, without a trailing `/`.
    pub path: String,
    pub dir: bool,
    pub size: u64,
}

/// Why an archive cannot be read or written.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("not an archive TermOS reads: {0}")]
    Format(String),
}

/// A member's name as a path that stays inside the folder: no root, no
/// `..`, no drive.
pub fn safe_relative(name: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in Path::new(&name.replace('\\', "/")).components() {
        match c {
            Component::Normal(p) => out.push(p),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

fn tar_reader(path: &Path, kind: Kind) -> Result<Box<dyn Read>, Error> {
    let file = BufReader::new(File::open(path)?);
    Ok(match kind {
        Kind::Tar => Box::new(file),
        Kind::TarGz => Box::new(flate2::read::GzDecoder::new(file)),
        Kind::TarZst => Box::new(
            ruzstd::decoding::StreamingDecoder::new(file)
                .map_err(|e| Error::Format(e.to_string()))?,
        ),
        Kind::Zip => return Err(Error::Format("a zip file is not a tar".into())),
    })
}

fn clean(name: &str) -> String {
    name.replace('\\', "/").trim_matches('/').to_owned()
}

/// Every member, in the archive's order. Folders the archive only implies
/// (a file `a/b/c` without `a/` and `a/b/`) are listed too.
pub fn list(path: &Path) -> Result<Vec<Member>, Error> {
    let kind = kind_of(path).ok_or_else(|| Error::Format(path.display().to_string()))?;
    let mut out: Vec<Member> = Vec::new();
    match kind {
        Kind::Zip => {
            let mut z = zip::ZipArchive::new(File::open(path)?)
                .map_err(|e| Error::Format(e.to_string()))?;
            for i in 0..z.len() {
                let f = z.by_index(i).map_err(|e| Error::Format(e.to_string()))?;
                let name = f.name().map_err(|e| Error::Format(e.to_string()))?;
                out.push(Member {
                    path: clean(&name),
                    dir: f.is_dir(),
                    size: f.size(),
                });
            }
        }
        _ => {
            let mut a = tar::Archive::new(tar_reader(path, kind)?);
            for e in a.entries()? {
                let e = e?;
                let t = e.header().entry_type();
                let name = clean(&e.path()?.to_string_lossy());
                out.push(Member {
                    path: name,
                    dir: t.is_dir(),
                    size: e.size(),
                });
            }
        }
    }
    out.retain(|m| !m.path.is_empty());
    // The folders that are only implied.
    let mut implied = Vec::new();
    for m in &out {
        let mut prefix = m.path.as_str();
        while let Some((parent, _)) = prefix.rsplit_once('/') {
            if !out.iter().any(|o| o.dir && o.path == parent)
                && !implied.contains(&parent.to_owned())
            {
                implied.push(parent.to_owned());
            }
            prefix = parent;
        }
    }
    out.extend(implied.into_iter().map(|path| Member {
        path,
        dir: true,
        size: 0,
    }));
    Ok(out)
}

/// The members directly inside `inner` (`""` is the top), folders and
/// files, by name.
pub fn children<'a>(members: &'a [Member], inner: &str) -> Vec<&'a Member> {
    let inner = inner.trim_matches('/');
    let mut seen = std::collections::BTreeSet::new();
    members
        .iter()
        .filter(|m| {
            let rest = if inner.is_empty() {
                Some(m.path.as_str())
            } else {
                m.path.strip_prefix(inner).and_then(|r| r.strip_prefix('/'))
            };
            rest.is_some_and(|r| !r.is_empty() && !r.contains('/')) && seen.insert(&m.path)
        })
        .collect()
}

/// What extraction did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Extracted {
    pub files: usize,
    /// Members left out: links, devices, names that would leave the
    /// folder.
    pub skipped: usize,
}

/// Whether member `name` is wanted: everything, or under one of `only`.
fn wanted(name: &str, only: Option<&[String]>) -> bool {
    only.is_none_or(|o| {
        o.iter().any(|w| {
            let w = w.trim_matches('/');
            name == w || name.starts_with(&format!("{w}/"))
        })
    })
}

/// Extracts the archive (or only the members under `only`) into `dest`,
/// which is made. Members keep their paths below `strip`, a folder inside
/// the archive (`""`: none).
pub fn extract(
    path: &Path,
    dest: &Path,
    only: Option<&[String]>,
    strip: &str,
) -> Result<Extracted, Error> {
    let kind = kind_of(path).ok_or_else(|| Error::Format(path.display().to_string()))?;
    std::fs::create_dir_all(dest)?;
    let strip = strip.trim_matches('/');
    let target = |name: &str| -> Option<PathBuf> {
        let rel = if strip.is_empty() {
            name
        } else {
            name.strip_prefix(strip)?.strip_prefix('/')?
        };
        safe_relative(rel).map(|r| dest.join(r))
    };
    let mut done = Extracted::default();
    match kind {
        Kind::Zip => {
            let mut z = zip::ZipArchive::new(File::open(path)?)
                .map_err(|e| Error::Format(e.to_string()))?;
            for i in 0..z.len() {
                let mut f = z.by_index(i).map_err(|e| Error::Format(e.to_string()))?;
                let name = clean(&f.name().map_err(|e| Error::Format(e.to_string()))?);
                if !wanted(&name, only) {
                    continue;
                }
                let Some(to) = target(&name).filter(|_| f.enclosed_name().is_some()) else {
                    done.skipped += 1;
                    continue;
                };
                if f.is_dir() {
                    std::fs::create_dir_all(&to)?;
                    continue;
                }
                if f.is_symlink() {
                    done.skipped += 1;
                    continue;
                }
                if let Some(parent) = to.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let mut out = File::create(&to)?;
                io::copy(&mut f, &mut out)?;
                done.files += 1;
            }
        }
        _ => {
            let mut a = tar::Archive::new(tar_reader(path, kind)?);
            for e in a.entries()? {
                let mut e = e?;
                let t = e.header().entry_type();
                let name = clean(&e.path()?.to_string_lossy());
                if !wanted(&name, only) {
                    continue;
                }
                let Some(to) = target(&name) else {
                    done.skipped += 1;
                    continue;
                };
                if t.is_dir() {
                    std::fs::create_dir_all(&to)?;
                } else if t.is_file() {
                    if let Some(parent) = to.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    let mut out = File::create(&to)?;
                    io::copy(&mut e, &mut out)?;
                    done.files += 1;
                } else {
                    done.skipped += 1;
                }
            }
        }
    }
    Ok(done)
}

/// Makes a zip at `dest` of `sources` (files and folder trees), each under
/// its own name. Links are left out. The number of files written.
pub fn make_zip(sources: &[PathBuf], dest: &Path) -> Result<usize, Error> {
    let file = File::create(dest)?;
    let mut z = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();
    let mut files = 0;
    for src in sources {
        let base = src.parent().unwrap_or(Path::new(""));
        let mut stack = vec![src.clone()];
        while let Some(p) = stack.pop() {
            let meta = std::fs::symlink_metadata(&p)?;
            let rel = p
                .strip_prefix(base)
                .unwrap_or(&p)
                .to_string_lossy()
                .replace('\\', "/");
            if meta.is_dir() {
                z.add_directory(format!("{rel}/"), options)
                    .map_err(|e| Error::Format(e.to_string()))?;
                let mut kids: Vec<PathBuf> =
                    std::fs::read_dir(&p)?.flatten().map(|e| e.path()).collect();
                kids.sort();
                stack.extend(kids.into_iter().rev());
            } else if meta.is_file() {
                z.start_file(rel, options)
                    .map_err(|e| Error::Format(e.to_string()))?;
                let mut f = File::open(&p)?;
                io::copy(&mut f, &mut z)?;
                files += 1;
            }
        }
    }
    let mut out = z.finish().map_err(|e| Error::Format(e.to_string()))?;
    out.flush()?;
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(dir: &Path) {
        std::fs::create_dir_all(dir.join("proj/src")).expect("mkdir");
        std::fs::write(dir.join("proj/README.md"), "# hi\n").expect("write");
        std::fs::write(dir.join("proj/src/main.rs"), "fn main() {}\n").expect("write");
    }

    #[test]
    fn kinds_and_stems() {
        assert_eq!(kind_of(Path::new("a.ZIP")), Some(Kind::Zip));
        assert_eq!(kind_of(Path::new("a.tar.gz")), Some(Kind::TarGz));
        assert_eq!(kind_of(Path::new("a.tgz")), Some(Kind::TarGz));
        assert_eq!(kind_of(Path::new("a.tar.zst")), Some(Kind::TarZst));
        assert_eq!(kind_of(Path::new("a.tar")), Some(Kind::Tar));
        assert_eq!(kind_of(Path::new("a.7z")), None);
        assert_eq!(stem(Path::new("/x/release-1.2.tar.gz")), "release-1.2");
        assert_eq!(stem(Path::new("notes.zip")), "notes");
    }

    #[test]
    fn names_that_leave_are_refused() {
        assert_eq!(safe_relative("a/b.txt"), Some(PathBuf::from("a/b.txt")));
        assert_eq!(safe_relative("./a"), Some(PathBuf::from("a")));
        for bad in [
            "../evil",
            "a/../../evil",
            "/etc/passwd",
            "",
            ".",
            "..\\evil",
        ] {
            assert_eq!(safe_relative(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_zip_is_made_listed_and_extracted() {
        let dir = tempfile::tempdir().expect("tempdir");
        tree(dir.path());
        let zip = dir.path().join("proj.zip");
        let n = make_zip(&[dir.path().join("proj")], &zip).expect("made");
        assert_eq!(n, 2);
        let members = list(&zip).expect("listed");
        let top: Vec<&str> = children(&members, "")
            .iter()
            .map(|m| m.path.as_str())
            .collect();
        assert_eq!(top, vec!["proj"]);
        let mut inside: Vec<&str> = children(&members, "proj")
            .iter()
            .map(|m| m.path.as_str())
            .collect();
        inside.sort_unstable();
        assert_eq!(inside, vec!["proj/README.md", "proj/src"]);
        let out = dir.path().join("out");
        let done = extract(&zip, &out, None, "").expect("extracted");
        assert_eq!(done.files, 2);
        assert_eq!(
            std::fs::read_to_string(out.join("proj/src/main.rs")).expect("read"),
            "fn main() {}\n"
        );
        // One member, without the folders above it.
        let one = dir.path().join("one");
        extract(&zip, &one, Some(&["proj/src".to_owned()][..]), "proj").expect("extracted");
        assert!(one.join("src/main.rs").is_file());
        assert!(!one.join("README.md").exists());
    }

    #[test]
    fn tars_with_an_escaping_name_extract_the_rest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("t.tar.gz");
        {
            let f = File::create(&path).expect("create");
            let gz = flate2::write::GzEncoder::new(f, flate2::Compression::fast());
            let mut b = tar::Builder::new(gz);
            let mut add = |name: &[u8], body: &[u8]| {
                let mut h = tar::Header::new_gnu();
                // Written raw: `set_path` refuses `..` itself.
                h.as_old_mut().name[..name.len()].copy_from_slice(name);
                h.set_entry_type(tar::EntryType::Regular);
                h.set_mode(0o644);
                h.set_size(body.len() as u64);
                h.set_cksum();
                b.append(&h, body).expect("append");
            };
            add(b"ok/file.txt", b"fine");
            add(b"../evil.txt", b"nope");
            b.into_inner()
                .expect("finish")
                .finish()
                .expect("gzip")
                .flush()
                .expect("flush");
        }
        let members = list(&path).expect("listed");
        assert!(members.iter().any(|m| m.path == "ok/file.txt"));
        assert!(
            members.iter().any(|m| m.path == "ok" && m.dir),
            "implied folder"
        );
        let out = dir.path().join("out");
        let done = extract(&path, &out, None, "").expect("extracted");
        assert_eq!(
            done,
            Extracted {
                files: 1,
                skipped: 1
            }
        );
        assert!(out.join("ok/file.txt").is_file());
        assert!(!dir.path().join("evil.txt").exists());
    }
}
