// SPDX-License-Identifier: GPL-3.0-or-later
//! The file manager: a folder's contents on the left, a preview of the
//! selected entry on the right (pictures, markdown, text, folders).
//!
//! Listing, previews and copying run on the manager's own worker thread,
//! so a huge folder or a slow disk never makes the desktop wait. Deleting
//! moves to the system trash (D-050); where there is none, it asks again
//! and then deletes for good.
//!
//! M13c (D-122) adds marks (Space, `*`), archives browsed like folders
//! (`X` extracts, `z` zips), searches by name (`?`) and text (`Ctrl+F`), a
//! git column, bulk rename in the editor (`F2` on marks, `u` undoes), disk
//! usage (`D`) and open with (`O`); [`Files`] puts two of these side by
//! side with bookmarks.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect as TuiRect;
use termos_core::app::{App, AppEffect, AppImage, AppWaker, new_image_key, tui_style};
use termos_core::input::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseKind};
use termos_render::cell::{Attrs, Style};
use termos_render::graphics::Picture;
use termos_render::grid::Rect;
use termos_render::icons::Icon;
use termos_render::theme::Theme;
use unicode_width::UnicodeWidthStr;

use crate::{archive, du, findfiles, gitstatus, markdown, rename};

/// Pictures larger than this are not previewed.
const MAX_PREVIEW_IMAGE: u64 = 32 * 1024 * 1024;
/// How much of a text file the preview reads.
const PREVIEW_TEXT_BYTES: usize = 32 * 1024;
/// How many entries of a folder the preview lists.
const PREVIEW_DIR_ENTRIES: usize = 200;

/// What opening a path means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenKind {
    Dir,
    Image,
    Markdown,
    /// Anything else: an editor.
    Other,
}

const IMAGE_EXTENSIONS: [&str; 6] = ["png", "jpg", "jpeg", "gif", "webp", "bmp"];
const MARKDOWN_EXTENSIONS: [&str; 3] = ["md", "markdown", "mdown"];

fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default()
}

/// How `path` opens: by kind on disk, then by extension.
pub fn open_kind(path: &Path) -> OpenKind {
    if path.is_dir() {
        return OpenKind::Dir;
    }
    let ext = extension(path);
    if IMAGE_EXTENSIONS.contains(&ext.as_str()) {
        OpenKind::Image
    } else if MARKDOWN_EXTENSIONS.contains(&ext.as_str()) {
        OpenKind::Markdown
    } else {
        OpenKind::Other
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Dir,
    File,
    /// A symbolic link (to `dir` when it points at a folder).
    Link {
        dir: bool,
    },
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub kind: EntryKind,
    pub size: u64,
    pub modified: Option<SystemTime>,
    /// A file anyone may run (Unix permissions; never on Windows).
    pub executable: bool,
}

impl Entry {
    fn is_dir(&self) -> bool {
        matches!(self.kind, EntryKind::Dir | EntryKind::Link { dir: true })
    }

    /// The icon shown before the name (D-088).
    fn icon(&self) -> Icon {
        Icon::for_file(
            &self.name,
            self.is_dir(),
            matches!(self.kind, EntryKind::Link { .. }),
            self.executable,
        )
    }

    fn hidden(&self) -> bool {
        self.name.starts_with('.')
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sort {
    #[default]
    Name,
    Size,
    Modified,
}

impl Sort {
    fn next(self) -> Self {
        match self {
            Self::Name => Self::Size,
            Self::Size => Self::Modified,
            Self::Modified => Self::Name,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Size => "size",
            Self::Modified => "modified",
        }
    }
}

/// Folders first, then by `sort` (largest and newest first), then by
/// name, ignoring case.
pub fn sort_entries(entries: &mut [Entry], sort: Sort) {
    let by_name = |a: &Entry, b: &Entry| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    };
    entries.sort_by(|a, b| {
        b.is_dir().cmp(&a.is_dir()).then_with(|| match sort {
            Sort::Name => by_name(a, b),
            Sort::Size => b.size.cmp(&a.size).then_with(|| by_name(a, b)),
            Sort::Modified => b.modified.cmp(&a.modified).then_with(|| by_name(a, b)),
        })
    });
}

#[cfg(unix)]
fn runnable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

/// Windows has no execute bit: `.exe` and friends get their icon from the
/// name.
#[cfg(not(unix))]
fn runnable(_meta: &std::fs::Metadata) -> bool {
    false
}

/// Reads a folder.
pub fn list_dir(dir: &Path) -> Result<Vec<Entry>, String> {
    let read = std::fs::read_dir(dir).map_err(|e| format!("cannot open {}: {e}", dir.display()))?;
    let mut out = Vec::new();
    for e in read.flatten() {
        let path = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        let Ok(ft) = e.file_type() else { continue };
        let meta = std::fs::metadata(&path).ok();
        let kind = if ft.is_symlink() {
            EntryKind::Link {
                dir: meta.as_ref().is_some_and(std::fs::Metadata::is_dir),
            }
        } else if ft.is_dir() {
            EntryKind::Dir
        } else if ft.is_file() {
            EntryKind::File
        } else {
            EntryKind::Other
        };
        out.push(Entry {
            name,
            path,
            kind,
            size: meta.as_ref().map_or(0, std::fs::Metadata::len),
            executable: kind == EntryKind::File && meta.as_ref().is_some_and(runnable),
            modified: meta.and_then(|m| m.modified().ok()),
        });
    }
    Ok(out)
}

/// `1.5K`, `23M`, …
pub fn human_size(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "K", "M", "G", "T", "P"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < UNITS.len() {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n}B")
    } else if v < 10.0 {
        format!("{v:.1}{}", UNITS[u])
    } else {
        format!("{v:.0}{}", UNITS[u])
    }
}

/// A free name in `dir` for a copy of `name`: `name`, else
/// `stem (copy).ext`, `stem (copy 2).ext`, …
pub fn free_name(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let p = Path::new(name);
    let (stem, ext) = match (p.file_stem(), p.extension()) {
        (Some(s), Some(e)) if !name.starts_with('.') || name.matches('.').count() > 1 => (
            s.to_string_lossy().into_owned(),
            format!(".{}", e.to_string_lossy()),
        ),
        _ => (name.to_owned(), String::new()),
    };
    for n in 1u32.. {
        let label = if n == 1 {
            "copy".to_owned()
        } else {
            format!("copy {n}")
        };
        let candidate = dir.join(format!("{stem} ({label}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    first
}

/// Copies a file or a folder tree.
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(from)?;
    if meta.file_type().is_symlink() {
        #[cfg(unix)]
        {
            let target = std::fs::read_link(from)?;
            return std::os::unix::fs::symlink(target, to);
        }
        #[cfg(not(unix))]
        {
            std::fs::copy(from, to)?;
            return Ok(());
        }
    }
    if meta.is_dir() {
        std::fs::create_dir(to)?;
        for e in std::fs::read_dir(from)? {
            let e = e?;
            copy_tree(&e.path(), &to.join(e.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(from, to).map(|_| ())
    }
}

/// Copies or moves `sources` into `dest`, each under a free name.
/// Returns how many were done.
pub fn paste(sources: &[PathBuf], dest: &Path, cut: bool) -> Result<usize, String> {
    let mut done = 0;
    for src in sources {
        let Some(name) = src.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        if dest.starts_with(src) {
            return Err(format!("cannot put {name} inside itself"));
        }
        let target = free_name(dest, &name);
        let result = if cut {
            std::fs::rename(src, &target).or_else(|_| {
                // Another file system: copy, then remove the original.
                copy_tree(src, &target)?;
                if src.is_dir() {
                    std::fs::remove_dir_all(src)
                } else {
                    std::fs::remove_file(src)
                }
            })
        } else {
            copy_tree(src, &target)
        };
        result.map_err(|e| format!("{name}: {e}"))?;
        done += 1;
    }
    Ok(done)
}

/// The preview of the selected entry.
#[derive(Debug, Clone)]
pub enum Preview {
    Loading,
    Dir(Result<Vec<Entry>, String>),
    Text(Vec<String>),
    Markdown(Box<markdown::Doc>),
    Image {
        key: u64,
        picture: Arc<Picture>,
    },
    /// Not shown as content: a one-line description.
    Info(String),
}

fn read_preview(path: &Path) -> Preview {
    if path.is_dir() {
        return Preview::Dir(list_dir(path).map(|mut v| {
            sort_entries(&mut v, Sort::Name);
            v.truncate(PREVIEW_DIR_ENTRIES);
            v
        }));
    }
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) => return Preview::Info(format!("cannot read: {e}")),
    };
    let ext = extension(path);
    if IMAGE_EXTENSIONS.contains(&ext.as_str()) {
        if meta.len() > MAX_PREVIEW_IMAGE {
            return Preview::Info(format!("picture, {}", human_size(meta.len())));
        }
        return match std::fs::read(path)
            .map_err(|e| e.to_string())
            .and_then(|b| Picture::decode(&b).map_err(|e| e.to_string()))
        {
            Ok(p) => Preview::Image {
                key: new_image_key(),
                picture: Arc::new(p),
            },
            Err(e) => Preview::Info(format!("not a picture TermOS can read: {e}")),
        };
    }
    let mut buf = vec![0u8; PREVIEW_TEXT_BYTES];
    let n = match std::fs::File::open(path).and_then(|mut f| {
        use std::io::Read;
        f.read(&mut buf)
    }) {
        Ok(n) => n,
        Err(e) => return Preview::Info(format!("cannot read: {e}")),
    };
    buf.truncate(n);
    let text = match std::str::from_utf8(&buf) {
        Ok(t) => t,
        // Cut in the middle of a character at the end of what was read.
        Err(e) if e.error_len().is_none() => {
            std::str::from_utf8(&buf[..e.valid_up_to()]).unwrap_or_default()
        }
        Err(_) => "\0",
    };
    if text.contains('\0') {
        return Preview::Info(format!("binary file, {}", human_size(meta.len())));
    }
    if MARKDOWN_EXTENSIONS.contains(&ext.as_str()) {
        return Preview::Markdown(Box::new(markdown::parse(text)));
    }
    Preview::Text(text.lines().map(|l| l.replace('\t', "    ")).collect())
}

/// Moves to the trash; the error says whether there is no trash.
fn trash(path: &Path) -> Result<(), String> {
    trash::delete(path).map_err(|e| e.to_string())
}

fn delete_for_good(path: &Path) -> Result<(), String> {
    let r = if path.is_dir() && !path.is_symlink() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    r.map_err(|e| e.to_string())
}

enum Job {
    List(PathBuf),
    Preview(u64, PathBuf),
    Paste {
        sources: Vec<PathBuf>,
        dest: PathBuf,
        cut: bool,
    },
    Trash(PathBuf),
    Delete(PathBuf),
    /// Git marks for a folder (D-122).
    Git(PathBuf),
    ListArchive(PathBuf),
    Extract {
        file: PathBuf,
        dest: PathBuf,
        only: Option<Vec<String>>,
        strip: String,
    },
    /// One member, into a temporary folder, to open it.
    ExtractOpen {
        file: PathBuf,
        member: String,
        dest: PathBuf,
    },
    Zip {
        sources: Vec<PathBuf>,
        dest: PathBuf,
    },
    Find {
        dir: PathBuf,
        query: String,
        text: bool,
        hidden: bool,
    },
}

enum Done {
    Listed(PathBuf, Result<Vec<Entry>, String>),
    Preview(u64, Preview),
    Pasted(Result<usize, String>),
    Trashed(PathBuf, Result<(), String>),
    Deleted(PathBuf, Result<(), String>),
    Git(PathBuf, Option<HashMap<String, char>>),
    Archive(PathBuf, Result<Vec<archive::Member>, String>),
    Extracted(PathBuf, Result<archive::Extracted, String>),
    ExtractedOpen(Result<PathBuf, String>),
    Zipped(PathBuf, Result<usize, String>),
    Found(String, bool, Result<Vec<findfiles::Found>, String>),
}

/// The worker: jobs in order, except that only the newest preview
/// request is served (moving quickly through a list asks for many).
fn worker(jobs: Receiver<Job>, done: Sender<Done>, waker: Arc<Mutex<Option<AppWaker>>>) {
    while let Ok(first) = jobs.recv() {
        let mut batch = vec![first];
        while let Ok(more) = jobs.try_recv() {
            batch.push(more);
        }
        let last_preview = batch.iter().rposition(|j| matches!(j, Job::Preview(..)));
        for (i, job) in batch.into_iter().enumerate() {
            let result = match job {
                Job::List(dir) => {
                    let r = list_dir(&dir);
                    Done::Listed(dir, r)
                }
                Job::Preview(generation, path) => {
                    if Some(i) != last_preview {
                        continue;
                    }
                    Done::Preview(generation, read_preview(&path))
                }
                Job::Paste { sources, dest, cut } => Done::Pasted(paste(&sources, &dest, cut)),
                Job::Trash(p) => {
                    let r = trash(&p);
                    Done::Trashed(p, r)
                }
                Job::Delete(p) => {
                    let r = delete_for_good(&p);
                    Done::Deleted(p, r)
                }
                Job::Git(dir) => {
                    let marks = gitstatus::status(&dir);
                    Done::Git(dir, marks)
                }
                Job::ListArchive(file) => {
                    let r = archive::list(&file).map_err(|e| e.to_string());
                    Done::Archive(file, r)
                }
                Job::Extract {
                    file,
                    dest,
                    only,
                    strip,
                } => {
                    let r = archive::extract(&file, &dest, only.as_deref(), &strip)
                        .map_err(|e| e.to_string());
                    Done::Extracted(dest, r)
                }
                Job::ExtractOpen { file, member, dest } => {
                    let parent = member.rsplit_once('/').map_or("", |(p, _)| p);
                    let r = archive::extract(&file, &dest, Some(&[member.clone()][..]), parent)
                        .map_err(|e| e.to_string())
                        .map(|_| dest.join(member.rsplit('/').next().unwrap_or(&member)));
                    Done::ExtractedOpen(r)
                }
                Job::Zip { sources, dest } => {
                    let r = archive::make_zip(&sources, &dest).map_err(|e| e.to_string());
                    Done::Zipped(dest, r)
                }
                Job::Find {
                    dir,
                    query,
                    text,
                    hidden,
                } => {
                    let stop = AtomicBool::new(false);
                    let r = if text {
                        findfiles::text(&dir, &query, hidden, &stop)
                    } else {
                        Ok(findfiles::names(&dir, &query, hidden, &stop))
                    };
                    Done::Found(query, text, r)
                }
            };
            if done.send(result).is_err() {
                return;
            }
            if let Some(w) = waker.lock().ok().and_then(|w| w.clone()) {
                w.wake();
            }
        }
    }
}

/// What the bottom line is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Mode {
    Browse,
    Filter,
    Rename(String),
    NewDir(String),
    /// Move to the trash?
    ConfirmTrash(PathBuf),
    /// No trash here: delete for good?
    ConfirmDelete(PathBuf),
    /// Move these marks to the trash?
    ConfirmTrashMany(Vec<PathBuf>),
    /// Typing a name to search for (`?`), or text (`Ctrl+F`).
    FindName(String),
    FindText(String),
    /// The names are in the editor: Enter reads them back.
    BulkWait,
    /// Do these renames?
    ConfirmBulk(Vec<(String, String)>),
    /// Choose what opens the file.
    OpenWith(Vec<(String, OpenWith)>, usize),
}

/// What can open a file (`O`, D-122).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenWith {
    /// TermOS's picture or document viewer.
    Viewer,
    Editor,
    /// The system's own program for it.
    System,
}

impl OpenWith {
    fn key(self) -> &'static str {
        match self {
            OpenWith::Viewer => "viewer",
            OpenWith::Editor => "editor",
            OpenWith::System => "system",
        }
    }

    fn from_key(k: &str) -> Option<Self> {
        match k {
            "viewer" => Some(OpenWith::Viewer),
            "editor" => Some(OpenWith::Editor),
            "system" => Some(OpenWith::System),
            _ => None,
        }
    }

    /// The command that opens `path` this way.
    fn command(self, path: &Path) -> String {
        let p = path.display();
        match self {
            OpenWith::Viewer => match open_kind(path) {
                OpenKind::Image => format!("image.open {p}"),
                OpenKind::Markdown => format!("markdown.open {p}"),
                _ => format!("file.open {p}"),
            },
            OpenWith::Editor => format!("file.edit {p}"),
            OpenWith::System => format!("file.open-system {p}"),
        }
    }
}

/// Browsing inside an archive.
#[derive(Debug, Clone)]
struct InArchive {
    file: PathBuf,
    /// The folder inside it, `""` at the top.
    inner: String,
    members: Vec<archive::Member>,
}

/// Search results shown in place of a folder.
#[derive(Debug, Clone)]
struct Results {
    title: String,
    /// Each result's name (as listed) → its path and line.
    items: HashMap<String, (PathBuf, Option<u32>)>,
}

/// The disk-usage view (`D`).
#[derive(Debug)]
struct DuView {
    dir: PathBuf,
    items: Option<Result<Vec<du::Usage>, String>>,
    seen: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    rx: Receiver<Result<Vec<du::Usage>, String>>,
    selected: usize,
    top: usize,
    confirm: Option<PathBuf>,
}

/// A bulk rename waiting for the editor.
#[derive(Debug, Clone)]
struct BulkRename {
    dir: PathBuf,
    names: Vec<String>,
    file: PathBuf,
}

pub struct FileManager {
    dir: PathBuf,
    all: Vec<Entry>,
    shown: Vec<Entry>,
    selected: usize,
    top: usize,
    hidden: bool,
    sort: Sort,
    filter: String,
    mode: Mode,
    /// Copied (`false`) or cut (`true`) paths waiting for a paste.
    clip: Option<(Vec<PathBuf>, bool)>,
    preview: Preview,
    preview_for: Option<PathBuf>,
    generation: u64,
    /// Select this name once the listing arrives (after going up).
    reselect: Option<String>,
    message: Option<(String, bool)>,
    loading: bool,
    jobs: Option<Sender<Job>>,
    done: Option<Receiver<Done>>,
    waker: Arc<Mutex<Option<AppWaker>>>,
    rows: u16,
    images: Vec<AppImage>,
    /// Marked entries (Space, `*`), by path (D-122).
    marks: BTreeSet<PathBuf>,
    /// Git marks of the folder's entries, by name.
    git: HashMap<String, char>,
    archive: Option<InArchive>,
    results: Option<Results>,
    du: Option<DuView>,
    renaming: Option<BulkRename>,
    /// The last bulk rename, for `u`.
    undo: Option<(PathBuf, Vec<(String, String)>)>,
    /// A command to run at the next timer (a file extracted to open it).
    pending: Option<String>,
    /// The other pane's folder, where extracting and zipping put their
    /// result when there is one ([`Files`]).
    other: Option<PathBuf>,
    /// Something was put in the other pane's folder: it reloads.
    touched_other: bool,
    /// Where the open-with choices are kept.
    state_dir: Option<PathBuf>,
    open_with: HashMap<String, OpenWith>,
}

impl std::fmt::Debug for FileManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileManager")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

impl FileManager {
    /// Opens `dir` (or the folder of a file).
    pub fn open(dir: PathBuf) -> Self {
        let dir = if dir.is_file() {
            dir.parent().map(Path::to_owned).unwrap_or(dir)
        } else {
            dir
        };
        let waker = Arc::new(Mutex::new(None));
        let (job_tx, job_rx) = channel();
        let (done_tx, done_rx) = channel();
        let w = Arc::clone(&waker);
        let spawned = std::thread::Builder::new()
            .name("file-manager".into())
            .spawn(move || worker(job_rx, done_tx, w));
        let (jobs, done, message) = match spawned {
            Ok(_) => (Some(job_tx), Some(done_rx), None),
            Err(e) => (None, None, Some((format!("cannot start: {e}"), true))),
        };
        let mut fm = Self {
            dir: PathBuf::new(),
            all: Vec::new(),
            shown: Vec::new(),
            selected: 0,
            top: 0,
            hidden: false,
            sort: Sort::Name,
            filter: String::new(),
            mode: Mode::Browse,
            clip: None,
            preview: Preview::Info(String::new()),
            preview_for: None,
            generation: 0,
            reselect: None,
            message,
            loading: false,
            jobs,
            done,
            waker,
            rows: 0,
            images: Vec::new(),
            marks: BTreeSet::new(),
            git: HashMap::new(),
            archive: None,
            results: None,
            du: None,
            renaming: None,
            undo: None,
            pending: None,
            other: None,
            touched_other: false,
            state_dir: None,
            open_with: HashMap::new(),
        };
        fm.go(dir, None);
        fm
    }

    /// The folder shown.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn send(&mut self, job: Job) {
        let failed = self.jobs.as_ref().is_none_or(|j| j.send(job).is_err());
        if failed {
            self.message = Some(("the file manager's worker stopped".into(), true));
        }
    }

    fn go(&mut self, dir: PathBuf, select: Option<String>) {
        if self.dir != dir {
            self.marks.clear();
            self.git.clear();
        }
        self.archive = None;
        self.results = None;
        self.dir = dir.clone();
        self.filter.clear();
        self.reselect = select;
        self.loading = true;
        self.send(Job::List(dir));
    }

    fn reload(&mut self) {
        if self.archive.is_some() || self.results.is_some() {
            return;
        }
        let keep = self.current().map(|e| e.name.clone());
        let dir = self.dir.clone();
        self.reselect = keep;
        self.loading = true;
        self.send(Job::List(dir));
    }

    fn current(&self) -> Option<&Entry> {
        self.shown.get(self.selected)
    }

    /// Rebuilds the visible list from the listing.
    fn refresh(&mut self) {
        let keep = self
            .reselect
            .take()
            .or_else(|| self.current().map(|e| e.name.clone()));
        let filter = self.filter.to_lowercase();
        let mut shown: Vec<Entry> = self
            .all
            .iter()
            .filter(|e| self.hidden || !e.hidden())
            .filter(|e| filter.is_empty() || e.name.to_lowercase().contains(&filter))
            .cloned()
            .collect();
        sort_entries(&mut shown, self.sort);
        self.shown = shown;
        self.selected = keep
            .and_then(|k| self.shown.iter().position(|e| e.name == k))
            .unwrap_or(0)
            .min(self.shown.len().saturating_sub(1));
        self.request_preview();
    }

    fn request_preview(&mut self) {
        let path = self.current().map(|e| e.path.clone());
        if path == self.preview_for {
            return;
        }
        self.preview_for = path.clone();
        self.generation += 1;
        match path {
            Some(p) => {
                self.preview = Preview::Loading;
                let generation = self.generation;
                self.send(Job::Preview(generation, p));
            }
            None => self.preview = Preview::Info(String::new()),
        }
    }

    fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Some(rx) = &self.done {
            let result = match rx.try_recv() {
                Ok(r) => r,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.done = None;
                    break;
                }
            };
            changed = true;
            match result {
                Done::Listed(dir, r) => {
                    if dir != self.dir {
                        continue;
                    }
                    self.loading = false;
                    match r {
                        Ok(entries) => self.all = entries,
                        Err(e) => {
                            self.all.clear();
                            self.message = Some((e, true));
                        }
                    }
                    self.preview_for = None;
                    self.refresh();
                    // Git marks, when it is a repository (D-122).
                    if gitstatus::in_repository(&self.dir) {
                        let d = self.dir.clone();
                        self.send(Job::Git(d));
                    }
                }
                Done::Git(dir, marks) => {
                    if dir == self.dir {
                        self.git = marks.unwrap_or_default();
                    }
                }
                Done::Archive(file, r) => {
                    self.loading = false;
                    match r {
                        Ok(members) => {
                            self.archive = Some(InArchive {
                                file,
                                inner: String::new(),
                                members,
                            });
                            self.marks.clear();
                            self.all = self.archive_entries();
                            self.reselect = None;
                            self.refresh();
                        }
                        Err(e) => self.message = Some((e, true)),
                    }
                }
                Done::Extracted(dest, r) => {
                    self.message = Some(match r {
                        Ok(x) if x.skipped > 0 => (
                            format!(
                                "extracted {} to {} ({} left out: links, devices or names that leave it)",
                                x.files,
                                dest.display(),
                                x.skipped
                            ),
                            false,
                        ),
                        Ok(x) => (
                            format!("extracted {} to {}", x.files, dest.display()),
                            false,
                        ),
                        Err(e) => (format!("cannot extract: {e}"), true),
                    });
                    self.after_writing(&dest);
                }
                Done::ExtractedOpen(r) => match r {
                    Ok(path) => self.pending = Some(format!("file.open {}", path.display())),
                    Err(e) => self.message = Some((format!("cannot open it: {e}"), true)),
                },
                Done::Zipped(dest, r) => {
                    self.message = Some(match r {
                        Ok(n) => (format!("zipped {n} files into {}", name_of(&dest)), false),
                        Err(e) => (format!("cannot make the zip: {e}"), true),
                    });
                    if let Some(parent) = dest.parent().map(Path::to_owned) {
                        self.after_writing(&parent);
                    }
                }
                Done::Found(query, text, r) => {
                    self.loading = false;
                    match r {
                        Ok(found) => self.show_results(&query, text, found),
                        Err(e) => self.message = Some((e, true)),
                    }
                }
                Done::Preview(generation, p) => {
                    if generation == self.generation {
                        self.preview = p;
                    }
                }
                Done::Pasted(r) => {
                    self.message = Some(match r {
                        Ok(n) => (format!("pasted {n}"), false),
                        Err(e) => (e, true),
                    });
                    self.reload();
                }
                Done::Trashed(path, r) => match r {
                    Ok(()) => {
                        self.message =
                            Some((format!("moved {} to the trash", name_of(&path)), false));
                        self.marks.remove(&path);
                        self.reload();
                        if let Some(dir) = self.du.as_ref().map(|d| d.dir.clone()) {
                            self.start_du(dir);
                        }
                    }
                    Err(e) => {
                        tracing::info!("no trash for a deletion: {e}");
                        self.mode = Mode::ConfirmDelete(path);
                    }
                },
                Done::Deleted(path, r) => {
                    self.message = Some(match r {
                        Ok(()) => (format!("deleted {}", name_of(&path)), false),
                        Err(e) => (format!("cannot delete {}: {e}", name_of(&path)), true),
                    });
                    self.reload();
                }
            }
        }
        changed
    }

    fn select(&mut self, i: usize) -> AppEffect {
        let i = i.min(self.shown.len().saturating_sub(1));
        if i == self.selected {
            return AppEffect::None;
        }
        self.selected = i;
        self.request_preview();
        AppEffect::Redraw
    }

    fn up(&mut self) -> AppEffect {
        if let Some(effect) = self.up_special() {
            return effect;
        }
        let Some(parent) = self.dir.parent().map(Path::to_owned) else {
            return AppEffect::None;
        };
        let name = name_of(&self.dir);
        self.go(parent, Some(name));
        AppEffect::Redraw
    }

    /// Enter a folder, or open a file with the right app.
    fn open_selected(&mut self) -> AppEffect {
        let Some(e) = self.current().cloned() else {
            return AppEffect::None;
        };
        if let Some(effect) = self.open_special(&e) {
            return effect;
        }
        if e.is_dir() {
            self.go(e.path, None);
            return AppEffect::Redraw;
        }
        // An archive opens like a folder (D-122).
        if archive::kind_of(&e.path).is_some() {
            self.loading = true;
            self.send(Job::ListArchive(e.path));
            return AppEffect::Redraw;
        }
        let chosen = self.open_with.get(&extension(&e.path)).copied();
        match chosen {
            Some(w) => AppEffect::Command(w.command(&e.path)),
            None => AppEffect::Command(format!("file.open {}", e.path.display())),
        }
    }

    fn browse_key(&mut self, key: KeyEvent) -> AppEffect {
        let page = usize::from(self.rows.max(2) - 1);
        if key == KeyEvent::ctrl('f') {
            self.mode = Mode::FindText(String::new());
            return AppEffect::Redraw;
        }
        if !key.mods.is_empty() {
            return AppEffect::None;
        }
        if let Some(effect) = self.browse_key_more(key) {
            return effect;
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.select(self.selected + 1),
            KeyCode::Char('k') | KeyCode::Up => self.select(self.selected.saturating_sub(1)),
            KeyCode::PageDown => self.select(self.selected + page),
            KeyCode::PageUp => self.select(self.selected.saturating_sub(page)),
            KeyCode::Char('g') | KeyCode::Home => self.select(0),
            KeyCode::Char('G') | KeyCode::End => self.select(usize::MAX),
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => self.open_selected(),
            KeyCode::Char('h') | KeyCode::Left | KeyCode::Backspace => self.up(),
            KeyCode::Char('~') => match home() {
                Some(h) => {
                    self.go(h, None);
                    AppEffect::Redraw
                }
                None => AppEffect::None,
            },
            KeyCode::Char('.') => {
                self.hidden = !self.hidden;
                self.refresh();
                AppEffect::Redraw
            }
            KeyCode::Char('s') => {
                self.sort = self.sort.next();
                self.refresh();
                self.message = Some((format!("sorted by {}", self.sort.label()), false));
                AppEffect::Redraw
            }
            KeyCode::Char('/') => {
                self.mode = Mode::Filter;
                AppEffect::Redraw
            }
            KeyCode::Char('r') | KeyCode::F(2) if self.writable() => match self.current() {
                Some(e) => {
                    self.mode = Mode::Rename(e.name.clone());
                    AppEffect::Redraw
                }
                None => AppEffect::None,
            },
            KeyCode::Char('n') if self.writable() => {
                self.mode = Mode::NewDir(String::new());
                AppEffect::Redraw
            }
            KeyCode::Char('y') | KeyCode::Char('x') => {
                let cut = key.code == KeyCode::Char('x');
                if self.archive.is_some() || (cut && self.results.is_some()) {
                    self.message = Some(("read-only here: X extracts".into(), true));
                    return AppEffect::Redraw;
                }
                let paths = self.selection();
                if paths.is_empty() {
                    return AppEffect::None;
                }
                let what = if cut { "cut" } else { "copied" };
                let label = match paths.as_slice() {
                    [one] => name_of(one),
                    many => format!("{} items", many.len()),
                };
                self.message = Some((format!("{what} {label} · p to paste"), false));
                self.clip = Some((paths, cut));
                AppEffect::Redraw
            }
            KeyCode::Char('p') if self.writable() => {
                let Some((sources, cut)) = self.clip.clone() else {
                    self.message = Some(("nothing to paste: y copies, x cuts".into(), false));
                    return AppEffect::Redraw;
                };
                if cut {
                    self.clip = None;
                }
                let dest = self.dir.clone();
                self.send(Job::Paste { sources, dest, cut });
                AppEffect::Redraw
            }
            KeyCode::Char('d') | KeyCode::Delete if self.writable() => {
                let paths = self.selection();
                match paths.as_slice() {
                    [] => AppEffect::None,
                    [one] => {
                        self.mode = Mode::ConfirmTrash(one.clone());
                        AppEffect::Redraw
                    }
                    _ => {
                        self.mode = Mode::ConfirmTrashMany(paths);
                        AppEffect::Redraw
                    }
                }
            }
            KeyCode::Char('e') => match self.current() {
                Some(e) if !e.is_dir() => {
                    AppEffect::Command(format!("file.edit {}", e.path.display()))
                }
                _ => AppEffect::None,
            },
            KeyCode::Char('R') | KeyCode::F(5) => {
                self.reload();
                AppEffect::Redraw
            }
            KeyCode::Esc if self.results.is_some() || self.archive.is_some() => {
                self.leave_special()
            }
            KeyCode::Char('q') | KeyCode::Esc => AppEffect::Close,
            KeyCode::Char('r' | 'p' | 'd' | 'n') | KeyCode::F(2) | KeyCode::Delete => {
                self.message = Some(("read-only here".into(), true));
                AppEffect::Redraw
            }
            _ => AppEffect::None,
        }
    }

    /// Keys while typing a name or a filter.
    fn line_key(&mut self, key: KeyEvent) -> AppEffect {
        let text = match &mut self.mode {
            Mode::Rename(t) | Mode::NewDir(t) | Mode::FindName(t) | Mode::FindText(t) => t,
            Mode::Filter => &mut self.filter,
            _ => return AppEffect::None,
        };
        match key.code {
            KeyCode::Esc => {
                if self.mode == Mode::Filter {
                    self.filter.clear();
                    self.refresh();
                }
                self.mode = Mode::Browse;
            }
            KeyCode::Backspace => {
                text.pop();
                if self.mode == Mode::Filter {
                    self.refresh();
                }
            }
            KeyCode::Enter => {
                let mode = std::mem::replace(&mut self.mode, Mode::Browse);
                self.finish_line(mode);
            }
            KeyCode::Char(c) if key.mods.is_empty() => {
                text.push(c);
                if self.mode == Mode::Filter {
                    self.refresh();
                }
            }
            _ => return AppEffect::None,
        }
        AppEffect::Redraw
    }

    fn finish_line(&mut self, mode: Mode) {
        match mode {
            Mode::Rename(name) => {
                let Some(e) = self.current().cloned() else {
                    return;
                };
                if name.is_empty() || name == e.name {
                    return;
                }
                if name.contains(['/', '\\']) {
                    self.message = Some(("a name cannot contain / or \\".into(), true));
                    return;
                }
                let to = self.dir.join(&name);
                if to.exists() {
                    self.message = Some((format!("{name} already exists"), true));
                    return;
                }
                match std::fs::rename(&e.path, &to) {
                    Ok(()) => {
                        self.reselect = Some(name.clone());
                        self.message = Some((format!("renamed to {name}"), false));
                        let dir = self.dir.clone();
                        self.loading = true;
                        self.send(Job::List(dir));
                    }
                    Err(err) => self.message = Some((format!("cannot rename: {err}"), true)),
                }
            }
            Mode::FindName(q) | Mode::FindText(q) if q.trim().is_empty() => {}
            Mode::FindName(q) => self.find(q, false),
            Mode::FindText(q) => self.find(q, true),
            Mode::NewDir(name) => {
                if name.is_empty() {
                    return;
                }
                match std::fs::create_dir(self.dir.join(&name)) {
                    Ok(()) => {
                        self.reselect = Some(name);
                        let dir = self.dir.clone();
                        self.loading = true;
                        self.send(Job::List(dir));
                    }
                    Err(err) => self.message = Some((format!("cannot create {name}: {err}"), true)),
                }
            }
            _ => {}
        }
    }

    fn confirm_key(&mut self, key: KeyEvent) -> AppEffect {
        let yes = matches!(key.code, KeyCode::Char('y' | 'Y'));
        let mode = std::mem::replace(&mut self.mode, Mode::Browse);
        if yes {
            match mode {
                Mode::ConfirmTrash(p) => self.send(Job::Trash(p)),
                Mode::ConfirmDelete(p) => self.send(Job::Delete(p)),
                Mode::ConfirmTrashMany(ps) => {
                    for p in ps {
                        self.send(Job::Trash(p));
                    }
                }
                Mode::ConfirmBulk(changes) => self.do_bulk(&changes),
                _ => {}
            }
        } else if let Mode::ConfirmBulk(_) = mode {
            self.renaming = None;
            self.message = Some(("nothing renamed".into(), false));
        }
        AppEffect::Redraw
    }

    fn draw_list(&mut self, area: TuiRect, buf: &mut Buffer, theme: &Theme) {
        let rows = usize::from(area.height);
        if self.selected < self.top {
            self.top = self.selected;
        } else if rows > 0 && self.selected >= self.top + rows {
            self.top = self.selected + 1 - rows;
        }
        self.top = self.top.min(self.shown.len().saturating_sub(rows.max(1)));
        let c = &theme.colors;
        let size_w: u16 = if area.width >= 24 { 6 } else { 0 };
        for (i, e) in self.shown.iter().enumerate().skip(self.top).take(rows) {
            let y = area.y + u16::try_from(i - self.top).unwrap_or(0);
            let selected = i == self.selected;
            let mut style = if e.is_dir() {
                Style::new().fg(c.accent).attrs(Attrs::BOLD)
            } else if e.hidden() {
                Style::new().fg(c.muted)
            } else {
                Style::new().fg(c.fg)
            };
            if selected {
                style = c.selected(style);
                let row = TuiRect::new(area.x, y, area.width, 1);
                buf.set_style(row, tui_style(Style::new().bg(c.selection_bg)));
            }
            let mut name = e.name.clone();
            match e.kind {
                EntryKind::Dir => name.push('/'),
                EntryKind::Link { dir } => name.push_str(if dir { "/@" } else { "@" }),
                _ => {}
            }
            // Git's mark in the first column; a marked entry gets a `+`
            // (D-122).
            if let Some(g) = self.git.get(&e.name) {
                let gc = match g {
                    'M' | 'R' | 'D' => c.warn,
                    'A' => c.accent,
                    'U' => c.error,
                    _ => c.muted,
                };
                let mut st = Style::new().fg(gc);
                if selected {
                    st = c.selected(st);
                }
                buf.set_string(area.x, y, g.to_string(), tui_style(st));
            }
            if self.marks.contains(&e.path) {
                name.insert(0, '+');
                style = style.fg(c.warn).attrs(Attrs::BOLD);
                if selected {
                    style = c.selected(style);
                }
            }
            let glyph = e.icon().glyph(theme.icons);
            let icon_w = if glyph.is_empty() { 0 } else { 2 };
            if icon_w > 0 {
                let mut st = Style::new().fg(if e.is_dir() { c.accent } else { c.muted });
                if selected {
                    st = c.selected(st);
                }
                buf.set_string(area.x + 1, y, glyph, tui_style(st));
            }
            let name_w = area.width.saturating_sub(size_w + 1 + icon_w);
            let shown = fit(&name, usize::from(name_w));
            buf.set_stringn(
                area.x + 1 + icon_w,
                y,
                &shown,
                usize::from(name_w),
                tui_style(style),
            );
            if size_w > 0 && !e.is_dir() {
                let s = human_size(e.size);
                let x = area
                    .right()
                    .saturating_sub(u16::try_from(s.len()).unwrap_or(0) + 1);
                let mut st = Style::new().fg(c.muted);
                if selected {
                    st = c.selected(st);
                }
                buf.set_string(x, y, &s, tui_style(st));
            }
        }
        if self.shown.is_empty() {
            let text = if self.loading {
                "loading…"
            } else if self.filter.is_empty() {
                "(empty)"
            } else {
                "(no match)"
            };
            buf.set_string(
                area.x + 1,
                area.y,
                text,
                tui_style(Style::new().fg(c.muted)),
            );
        }
    }

    fn draw_preview(&mut self, area: TuiRect, buf: &mut Buffer, theme: &Theme) {
        let c = &theme.colors;
        let muted = tui_style(Style::new().fg(c.muted));
        let text = tui_style(Style::new().fg(c.fg));
        match &self.preview {
            Preview::Loading => {}
            Preview::Info(s) => {
                buf.set_stringn(area.x, area.y, s, usize::from(area.width), muted);
            }
            Preview::Dir(Err(e)) => {
                buf.set_stringn(area.x, area.y, e, usize::from(area.width), muted);
            }
            Preview::Dir(Ok(entries)) => {
                if entries.is_empty() {
                    buf.set_string(area.x, area.y, "(empty)", muted);
                }
                for (i, e) in entries.iter().take(usize::from(area.height)).enumerate() {
                    let y = area.y + u16::try_from(i).unwrap_or(0);
                    let (name, st) = if e.is_dir() {
                        (format!("{}/", e.name), tui_style(Style::new().fg(c.accent)))
                    } else {
                        (e.name.clone(), text)
                    };
                    buf.set_stringn(
                        area.x,
                        y,
                        fit(&name, usize::from(area.width)),
                        usize::from(area.width),
                        st,
                    );
                }
            }
            Preview::Text(lines) => {
                for (i, l) in lines.iter().take(usize::from(area.height)).enumerate() {
                    let y = area.y + u16::try_from(i).unwrap_or(0);
                    buf.set_stringn(area.x, y, l, usize::from(area.width), text);
                }
            }
            Preview::Markdown(doc) => {
                let rows = markdown::layout(doc, area.width, &[]);
                for (i, row) in rows.iter().take(usize::from(area.height)).enumerate() {
                    if let markdown::Row::Text(spans) = row {
                        let y = area.y + u16::try_from(i).unwrap_or(0);
                        let mut x = area.x;
                        for s in spans {
                            if x >= area.right() {
                                break;
                            }
                            let st = tui_style(markdown::style(s.look, theme));
                            buf.set_stringn(x, y, &s.text, usize::from(area.right() - x), st);
                            x = x.saturating_add(
                                u16::try_from(UnicodeWidthStr::width(s.text.as_str())).unwrap_or(0),
                            );
                        }
                    }
                }
            }
            Preview::Image { key, picture } => {
                self.images.push(AppImage {
                    key: *key,
                    picture: Arc::clone(picture),
                    rect: Rect::new(area.x, area.y, area.width, area.height),
                    upscale: false,
                    scroll_top: 0,
                    pixels_only: false,
                });
            }
        }
    }

    fn bottom_line(&self, theme: &Theme) -> (String, Style) {
        if let Some(more) = self.bottom_more(theme) {
            return more;
        }
        let c = &theme.colors;
        let plain = Style::new().fg(c.fg);
        match &self.mode {
            Mode::Filter => (format!("/{}▏", self.filter), plain),
            Mode::Rename(t) => (format!("Rename to: {t}▏"), plain),
            Mode::NewDir(t) => (format!("New folder: {t}▏"), plain),
            Mode::ConfirmTrash(p) => (
                format!("Move {} to the trash? y/n", name_of(p)),
                Style::new().fg(c.warn).attrs(Attrs::BOLD),
            ),
            Mode::ConfirmDelete(p) => (
                format!("No trash here. Delete {} for good? y/n", name_of(p)),
                Style::new().fg(c.error).attrs(Attrs::BOLD),
            ),
            Mode::Browse
            | Mode::FindName(_)
            | Mode::FindText(_)
            | Mode::ConfirmTrashMany(_)
            | Mode::BulkWait
            | Mode::ConfirmBulk(_)
            | Mode::OpenWith(..) => match &self.message {
                Some((m, true)) => (m.clone(), Style::new().fg(c.error)),
                Some((m, false)) => (m.clone(), Style::new().fg(c.muted)),
                None => (
                    "enter open · h up · / filter · . hidden · s sort · r rename · n folder · y/x/p copy/cut/paste · d trash · e edit · q close"
                        .into(),
                    Style::new().fg(c.muted),
                ),
            },
        }
    }
}

/// How often the disk-usage view redraws its count while it runs.
const DU_TICK: Duration = Duration::from_millis(500);

impl FileManager {
    /// Where extracting and zipping put their result: the other pane's
    /// folder ([`Files`]), else this one.
    pub fn set_other(&mut self, other: Option<PathBuf>) {
        self.other = other;
    }

    /// Whether something was put in the other pane's folder since the last
    /// call.
    pub fn take_touched_other(&mut self) -> bool {
        std::mem::take(&mut self.touched_other)
    }

    /// Where the open-with choices are kept (`<state>/open-with`).
    pub fn set_state_dir(&mut self, dir: PathBuf) {
        let text = std::fs::read_to_string(dir.join("open-with")).unwrap_or_default();
        self.open_with = text
            .lines()
            .filter_map(|l| {
                let (ext, k) = l.split_once('=')?;
                Some((ext.trim().to_owned(), OpenWith::from_key(k.trim())?))
            })
            .collect();
        self.state_dir = Some(dir);
    }

    /// Browsing, not typing or answering: the keys [`Files`] takes are
    /// free.
    pub fn is_browsing(&self) -> bool {
        self.mode == Mode::Browse && self.du.is_none()
    }

    /// Showing a real folder (not an archive or search results).
    fn writable(&self) -> bool {
        self.archive.is_none() && self.results.is_none()
    }

    /// The marked paths, else the current one.
    pub fn selection(&self) -> Vec<PathBuf> {
        if self.marks.is_empty() {
            self.current()
                .map(|e| vec![e.path.clone()])
                .unwrap_or_default()
        } else {
            self.marks.iter().cloned().collect()
        }
    }

    /// Copies (or moves) `sources` into this pane's folder (`F5`, `F6`
    /// from the other pane).
    pub fn paste_here(&mut self, sources: Vec<PathBuf>, cut: bool) {
        if !self.writable() {
            self.message = Some(("cannot put files into an archive or results".into(), true));
            return;
        }
        let dest = self.dir.clone();
        self.send(Job::Paste { sources, dest, cut });
    }

    /// Reloads (after the other pane put something here).
    pub fn refresh_listing(&mut self) {
        self.reload();
    }

    /// After writing into `dest`: this pane or the other reloads.
    fn after_writing(&mut self, dest: &Path) {
        if dest.starts_with(&self.dir) || dest == self.dir {
            self.reload();
        }
        if self.other.as_deref().is_some_and(|o| dest.starts_with(o)) {
            self.touched_other = true;
        }
    }

    /// The folder extraction and zips write into.
    fn target_dir(&self) -> PathBuf {
        self.other.clone().unwrap_or_else(|| match &self.archive {
            Some(a) => a
                .file
                .parent()
                .map_or_else(|| self.dir.clone(), Path::to_owned),
            None => self.dir.clone(),
        })
    }

    /// The entries of the archive folder shown.
    fn archive_entries(&self) -> Vec<Entry> {
        let Some(a) = &self.archive else {
            return Vec::new();
        };
        archive::children(&a.members, &a.inner)
            .into_iter()
            .map(|m| Entry {
                name: m.path.rsplit('/').next().unwrap_or(&m.path).to_owned(),
                path: a.file.join(&m.path),
                kind: if m.dir {
                    EntryKind::Dir
                } else {
                    EntryKind::File
                },
                size: m.size,
                modified: None,
                executable: false,
            })
            .collect()
    }

    /// The member path of an entry of the archive folder shown.
    fn member_of(&self, e: &Entry) -> Option<String> {
        let a = self.archive.as_ref()?;
        Some(if a.inner.is_empty() {
            e.name.clone()
        } else {
            format!("{}/{}", a.inner, e.name)
        })
    }

    /// `h` in an archive (a folder up, or out) or in results (back).
    fn up_special(&mut self) -> Option<AppEffect> {
        if self.results.is_some() {
            return Some(self.leave_special());
        }
        let a = self.archive.as_mut()?;
        if a.inner.is_empty() {
            return Some(self.leave_special());
        }
        let name = a.inner.rsplit('/').next().unwrap_or(&a.inner).to_owned();
        a.inner = a
            .inner
            .rsplit_once('/')
            .map_or_else(String::new, |(p, _)| p.to_owned());
        self.all = self.archive_entries();
        self.reselect = Some(name);
        self.marks.clear();
        self.refresh();
        Some(AppEffect::Redraw)
    }

    /// Back to the folder from an archive or results.
    fn leave_special(&mut self) -> AppEffect {
        let select = self
            .archive
            .take()
            .and_then(|a| a.file.file_name().map(|n| n.to_string_lossy().into_owned()));
        self.results = None;
        let dir = self.dir.clone();
        self.go(dir, select);
        AppEffect::Redraw
    }

    /// Enter in an archive or results.
    fn open_special(&mut self, e: &Entry) -> Option<AppEffect> {
        if let Some(r) = &self.results {
            let (path, line) = r.items.get(&e.name).cloned()?;
            return Some(match line {
                Some(l) => AppEffect::Command(format!("file.edit {}:{l}", path.display())),
                None if path.is_dir() => {
                    self.go(path, None);
                    AppEffect::Redraw
                }
                None => AppEffect::Command(format!("file.open {}", path.display())),
            });
        }
        let member = self.member_of(e)?;
        let a = self.archive.as_mut()?;
        if e.is_dir() {
            a.inner = member;
            self.all = self.archive_entries();
            self.marks.clear();
            self.refresh();
            return Some(AppEffect::Redraw);
        }
        let file = a.file.clone();
        let dest = std::env::temp_dir()
            .join(format!("termos-archive-{}", std::process::id()))
            .join(archive::stem(&file));
        self.message = Some((format!("extracting {}…", e.name), false));
        self.send(Job::ExtractOpen { file, member, dest });
        Some(AppEffect::Redraw)
    }

    /// Search results in place of the folder.
    fn show_results(&mut self, query: &str, text: bool, found: Vec<findfiles::Found>) {
        let n = found.len();
        let mut items = HashMap::new();
        self.all = found
            .into_iter()
            .map(|f| {
                let dir = f.line.is_none() && f.path.is_dir();
                items.insert(f.shown.clone(), (f.path.clone(), f.line));
                Entry {
                    name: f.shown,
                    path: f.path,
                    kind: if dir { EntryKind::Dir } else { EntryKind::File },
                    size: 0,
                    modified: None,
                    executable: false,
                }
            })
            .collect();
        let what = if text { "text" } else { "names" };
        let more = if n >= findfiles::MAX_RESULTS {
            format!(" (the first {n})")
        } else {
            String::new()
        };
        self.results = Some(Results {
            title: format!("{n} {what} matching “{query}”{more} · Esc back"),
            items,
        });
        self.marks.clear();
        self.reselect = None;
        self.refresh();
    }

    fn find(&mut self, query: String, text: bool) {
        if self.archive.is_some() {
            self.message = Some(("search the folder, not the archive".into(), true));
            return;
        }
        self.loading = true;
        let dir = self.dir.clone();
        let hidden = self.hidden;
        self.send(Job::Find {
            dir,
            query,
            text,
            hidden,
        });
    }

    /// Keys the M13c features add (D-122); `None` for the others.
    fn browse_key_more(&mut self, key: KeyEvent) -> Option<AppEffect> {
        Some(match key.code {
            KeyCode::Char(' ') => {
                let e = self.current()?.clone();
                if !self.marks.remove(&e.path) {
                    self.marks.insert(e.path);
                }
                let _ = self.select(self.selected + 1);
                AppEffect::Redraw
            }
            KeyCode::Char('*') => {
                for e in &self.shown {
                    if !self.marks.remove(&e.path) {
                        self.marks.insert(e.path.clone());
                    }
                }
                AppEffect::Redraw
            }
            KeyCode::Char('?') => {
                self.mode = Mode::FindName(String::new());
                AppEffect::Redraw
            }
            KeyCode::F(2) if self.marks.len() > 1 && self.writable() => self.start_bulk(),
            KeyCode::Char('u') if self.writable() => {
                let (dir, done) = self.undo.take()?;
                self.message = Some(match rename::apply(&dir, &rename::undo_of(&done)) {
                    Ok(d) => (format!("undid {} renames", d.len()), false),
                    Err(e) => (e, true),
                });
                self.reload();
                AppEffect::Redraw
            }
            KeyCode::Char('X') => self.extract(),
            KeyCode::Char('z') if self.writable() => self.zip(),
            KeyCode::Char('D') if self.writable() => {
                let dir = self.dir.clone();
                self.start_du(dir);
                AppEffect::Redraw
            }
            KeyCode::Char('O') if self.writable() => {
                let e = self.current()?.clone();
                if e.is_dir() {
                    return None;
                }
                let mut choices = Vec::new();
                if matches!(open_kind(&e.path), OpenKind::Image | OpenKind::Markdown) {
                    choices.push(("TermOS's viewer".to_owned(), OpenWith::Viewer));
                }
                choices.push(("The editor".to_owned(), OpenWith::Editor));
                choices.push(("The system's program".to_owned(), OpenWith::System));
                self.mode = Mode::OpenWith(choices, 0);
                AppEffect::Redraw
            }
            _ => return None,
        })
    }

    /// `X`: an archive extracted, or the marked members of the one shown.
    fn extract(&mut self) -> AppEffect {
        let dest_base = self.target_dir();
        if let Some(a) = &self.archive {
            let file = a.file.clone();
            let strip = a.inner.clone();
            let only: Vec<String> = if self.marks.is_empty() {
                self.current()
                    .and_then(|e| self.member_of(e))
                    .into_iter()
                    .collect()
            } else {
                self.marks
                    .iter()
                    .filter_map(|p| p.strip_prefix(&file).ok())
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .collect()
            };
            if only.is_empty() {
                return AppEffect::None;
            }
            self.send(Job::Extract {
                file,
                dest: dest_base,
                only: Some(only),
                strip,
            });
            return AppEffect::Redraw;
        }
        let Some(e) = self.current().cloned() else {
            return AppEffect::None;
        };
        if archive::kind_of(&e.path).is_none() {
            self.message = Some((
                "X extracts an archive: zip, tar, tar.gz, tar.zst".into(),
                true,
            ));
            return AppEffect::Redraw;
        }
        let dest = free_name(&dest_base, &archive::stem(&e.path));
        self.send(Job::Extract {
            file: e.path,
            dest,
            only: None,
            strip: String::new(),
        });
        AppEffect::Redraw
    }

    /// `z`: a zip of the marks (or the current entry).
    fn zip(&mut self) -> AppEffect {
        let sources = self.selection();
        let name = match sources.as_slice() {
            [] => return AppEffect::None,
            [one] => format!("{}.zip", name_of(one)),
            _ => "archive.zip".to_owned(),
        };
        let dest = free_name(&self.target_dir(), &name);
        self.message = Some((format!("zipping into {}…", name_of(&dest)), false));
        self.send(Job::Zip { sources, dest });
        AppEffect::Redraw
    }

    // ----- bulk rename ----------------------------------------------------

    /// `F2` on several marks: their names to the editor (D-122).
    fn start_bulk(&mut self) -> AppEffect {
        let names: Vec<String> = self
            .marks
            .iter()
            .filter(|p| p.parent() == Some(self.dir.as_path()))
            .map(|p| name_of(p))
            .collect();
        if names.len() < 2 {
            self.message = Some(("mark two or more entries of this folder".into(), true));
            return AppEffect::Redraw;
        }
        let file = std::env::temp_dir().join(format!(
            "termos-rename-{}-{}.txt",
            std::process::id(),
            self.generation
        ));
        if let Err(e) = std::fs::write(&file, rename::names_file(&names)) {
            self.message = Some((format!("cannot write {}: {e}", file.display()), true));
            return AppEffect::Redraw;
        }
        self.renaming = Some(BulkRename {
            dir: self.dir.clone(),
            names,
            file: file.clone(),
        });
        self.mode = Mode::BulkWait;
        AppEffect::Command(format!("file.edit {}", file.display()))
    }

    /// Enter after editing: the names read back and checked.
    fn bulk_read(&mut self) -> AppEffect {
        let Some(r) = self.renaming.clone() else {
            self.mode = Mode::Browse;
            return AppEffect::Redraw;
        };
        let text = match std::fs::read_to_string(&r.file) {
            Ok(t) => t,
            Err(e) => {
                self.message = Some((format!("cannot read {}: {e}", r.file.display()), true));
                return AppEffect::Redraw;
            }
        };
        let dir = r.dir.clone();
        match rename::plan(&r.names, &text, &|n| dir.join(n).exists()) {
            Ok(changes) if changes.is_empty() => {
                self.mode = Mode::Browse;
                self.renaming = None;
                let _ = std::fs::remove_file(&r.file);
                self.message = Some(("no name changed".into(), false));
            }
            Ok(changes) => self.mode = Mode::ConfirmBulk(changes),
            Err(e) => self.message = Some((format!("{e}; fix it and press Enter again"), true)),
        }
        AppEffect::Redraw
    }

    fn do_bulk(&mut self, changes: &[(String, String)]) {
        let Some(r) = self.renaming.take() else {
            return;
        };
        let _ = std::fs::remove_file(&r.file);
        self.message = Some(match rename::apply(&r.dir, changes) {
            Ok(done) => {
                let n = done.len();
                self.undo = Some((r.dir.clone(), done));
                self.marks.clear();
                (format!("renamed {n} · u undoes"), false)
            }
            Err(e) => (format!("{e}; nothing was renamed"), true),
        });
        self.reload();
    }

    // ----- open with --------------------------------------------------------

    fn open_with_key(&mut self, key: KeyEvent) -> AppEffect {
        let Mode::OpenWith(choices, sel) = &mut self.mode else {
            return AppEffect::None;
        };
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                *sel = (*sel + 1).min(choices.len().saturating_sub(1));
            }
            KeyCode::Char('k') | KeyCode::Up => *sel = sel.saturating_sub(1),
            KeyCode::Enter => {
                let chosen = choices.get(*sel).map(|(_, w)| *w);
                self.mode = Mode::Browse;
                let (Some(w), Some(e)) = (chosen, self.current().cloned()) else {
                    return AppEffect::Redraw;
                };
                self.remember_open_with(&extension(&e.path), w);
                return AppEffect::Command(w.command(&e.path));
            }
            _ => self.mode = Mode::Browse,
        }
        AppEffect::Redraw
    }

    fn remember_open_with(&mut self, ext: &str, w: OpenWith) {
        if ext.is_empty() {
            return;
        }
        self.open_with.insert(ext.to_owned(), w);
        let Some(dir) = &self.state_dir else {
            return;
        };
        let mut lines: Vec<String> = self
            .open_with
            .iter()
            .map(|(e, w)| format!("{e}={}", w.key()))
            .collect();
        lines.sort();
        let written = std::fs::create_dir_all(dir)
            .and_then(|()| std::fs::write(dir.join("open-with"), lines.join("\n") + "\n"));
        if let Err(e) = written {
            tracing::warn!("cannot keep the open-with choice: {e}");
        }
    }

    // ----- disk usage -------------------------------------------------------

    fn start_du(&mut self, dir: PathBuf) {
        if let Some(old) = &self.du {
            old.stop.store(true, Ordering::Relaxed);
        }
        let (tx, rx) = channel();
        let seen = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (s2, st2, d2, w) = (
            Arc::clone(&seen),
            Arc::clone(&stop),
            dir.clone(),
            Arc::clone(&self.waker),
        );
        let spawned = std::thread::Builder::new()
            .name("disk-usage".into())
            .spawn(move || {
                let _ = tx.send(du::usage(&d2, &s2, &st2));
                if let Some(w) = w.lock().ok().and_then(|w| w.clone()) {
                    w.wake();
                }
            });
        if let Err(e) = spawned {
            self.message = Some((format!("cannot size the folder: {e}"), true));
            return;
        }
        self.du = Some(DuView {
            dir,
            items: None,
            seen,
            stop,
            rx,
            selected: 0,
            top: 0,
            confirm: None,
        });
    }

    fn du_poll(&mut self) {
        if let Some(d) = &mut self.du
            && d.items.is_none()
            && let Ok(r) = d.rx.try_recv()
        {
            d.items = Some(r);
        }
    }

    fn du_key(&mut self, key: KeyEvent) -> AppEffect {
        let Some(d) = &mut self.du else {
            return AppEffect::None;
        };
        if let Some(path) = d.confirm.take() {
            if matches!(key.code, KeyCode::Char('y' | 'Y')) {
                self.send(Job::Trash(path));
            }
            return AppEffect::Redraw;
        }
        let n = d
            .items
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .map_or(0, Vec::len);
        let current = d
            .items
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .and_then(|v| v.get(d.selected))
            .cloned();
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                d.selected = (d.selected + 1).min(n.saturating_sub(1))
            }
            KeyCode::Char('k') | KeyCode::Up => d.selected = d.selected.saturating_sub(1),
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => {
                if let Some(u) = current.filter(|u| u.dir) {
                    self.start_du(u.path);
                }
            }
            KeyCode::Char('h') | KeyCode::Left | KeyCode::Backspace => {
                if let Some(parent) = d.dir.parent().map(Path::to_owned) {
                    self.start_du(parent);
                }
            }
            KeyCode::Char('d') | KeyCode::Delete => d.confirm = current.map(|u| u.path),
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('D') => {
                d.stop.store(true, Ordering::Relaxed);
                let dir = d.dir.clone();
                self.du = None;
                if dir != self.dir {
                    self.go(dir, None);
                }
            }
            _ => return AppEffect::None,
        }
        AppEffect::Redraw
    }

    fn draw_du(&mut self, area: TuiRect, buf: &mut Buffer, theme: &Theme) {
        let c = &theme.colors;
        let Some(d) = &mut self.du else {
            return;
        };
        let muted = tui_style(Style::new().fg(c.muted));
        let head = format!(
            "Disk usage · {}",
            fit_path(&d.dir, usize::from(area.width.saturating_sub(14)))
        );
        buf.set_stringn(
            area.x + 1,
            area.y,
            &head,
            usize::from(area.width),
            tui_style(Style::new().fg(c.muted).attrs(Attrs::BOLD)),
        );
        let body_h = usize::from(area.height.saturating_sub(2));
        match &d.items {
            None => {
                let n = d.seen.load(Ordering::Relaxed);
                buf.set_string(
                    area.x + 1,
                    area.y + 1,
                    format!("sizing… {n} entries"),
                    muted,
                );
            }
            Some(Err(e)) => {
                buf.set_stringn(
                    area.x + 1,
                    area.y + 1,
                    e,
                    usize::from(area.width),
                    tui_style(Style::new().fg(c.error)),
                );
            }
            Some(Ok(items)) => {
                let most = items.first().map_or(0, |u| u.bytes);
                let total: u64 = items.iter().map(|u| u.bytes).sum();
                if d.selected < d.top {
                    d.top = d.selected;
                } else if body_h > 0 && d.selected >= d.top + body_h {
                    d.top = d.selected + 1 - body_h;
                }
                let bar_w = 20usize.min(usize::from(area.width) / 4);
                for (i, u) in items.iter().enumerate().skip(d.top).take(body_h) {
                    let y = area.y + 1 + u16::try_from(i - d.top).unwrap_or(0);
                    let name = if u.dir {
                        format!("{}/", u.name)
                    } else {
                        u.name.clone()
                    };
                    let line = format!(
                        "{:>7} {} {name}",
                        human_size(u.bytes),
                        du::bar(u.bytes, most, bar_w)
                    );
                    let mut st = Style::new().fg(if u.dir { c.accent } else { c.fg });
                    if i == d.selected {
                        st = c.selected(st);
                        buf.set_style(
                            TuiRect::new(area.x, y, area.width, 1),
                            tui_style(Style::new().bg(c.selection_bg)),
                        );
                    }
                    buf.set_stringn(
                        area.x + 1,
                        y,
                        &line,
                        usize::from(area.width.saturating_sub(1)),
                        tui_style(st),
                    );
                }
                let y = area.bottom().saturating_sub(1);
                let line = match &d.confirm {
                    Some(p) => format!("Move {} to the trash? y/n", name_of(p)),
                    None => format!(
                        "{} in all · enter in · h up · d trash · Esc back",
                        human_size(total)
                    ),
                };
                let st = if d.confirm.is_some() {
                    tui_style(Style::new().fg(c.warn).attrs(Attrs::BOLD))
                } else {
                    muted
                };
                buf.set_stringn(
                    area.x + 1,
                    y,
                    &line,
                    usize::from(area.width.saturating_sub(1)),
                    st,
                );
            }
        }
    }

    /// The bottom line's text for the M13c modes.
    fn bottom_more(&self, theme: &Theme) -> Option<(String, Style)> {
        let c = &theme.colors;
        let plain = Style::new().fg(c.fg);
        let ask = Style::new().fg(c.warn).attrs(Attrs::BOLD);
        Some(match &self.mode {
            Mode::FindName(t) => (format!("Find names: {t}▏"), plain),
            Mode::FindText(t) => (format!("Find text: {t}▏"), plain),
            Mode::ConfirmTrashMany(ps) => {
                (format!("Move {} items to the trash? y/n", ps.len()), ask)
            }
            Mode::BulkWait => (
                "Edit the names, save and close the editor, then press Enter here (Esc cancels)"
                    .into(),
                ask,
            ),
            Mode::ConfirmBulk(changes) => {
                let shown: Vec<String> = changes
                    .iter()
                    .take(3)
                    .map(|(a, b)| format!("{a} → {b}"))
                    .collect();
                let more = if changes.len() > 3 {
                    format!(" and {} more", changes.len() - 3)
                } else {
                    String::new()
                };
                (format!("Rename {}{more}? y/n", shown.join(", ")), ask)
            }
            Mode::OpenWith(choices, sel) => {
                let items: Vec<String> = choices
                    .iter()
                    .enumerate()
                    .map(|(i, (l, _))| {
                        if i == *sel {
                            format!("[{l}]")
                        } else {
                            l.clone()
                        }
                    })
                    .collect();
                (
                    format!("Open with: {} · j/k, Enter", items.join("  ")),
                    plain,
                )
            }
            _ => return None,
        })
    }
}

fn name_of(p: &Path) -> String {
    p.file_name().map_or_else(
        || p.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// `text` in at most `max` cells, ending in `…` when cut.
fn fit(text: &str, max: usize) -> String {
    if UnicodeWidthStr::width(text) <= max {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in text.chars() {
        let cw = UnicodeWidthStr::width(ch.encode_utf8(&mut [0; 4]) as &str);
        if w + cw + 1 > max {
            break;
        }
        out.push(ch);
        w += cw;
    }
    if max > 0 {
        out.push('…');
    }
    out
}

/// A path that fits: its last components, `…/` in front.
fn fit_path(p: &Path, max: usize) -> String {
    let full = p.display().to_string();
    if UnicodeWidthStr::width(full.as_str()) <= max {
        return full;
    }
    let parts: Vec<String> = p
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let mut out = String::new();
    for part in parts.iter().rev() {
        let next = if out.is_empty() {
            part.clone()
        } else {
            format!("{part}/{out}")
        };
        if UnicodeWidthStr::width(next.as_str()) + 2 > max {
            break;
        }
        out = next;
    }
    format!("…/{out}")
}

impl App for FileManager {
    fn reopen(&self) -> Option<String> {
        // A path that is not Unicode would not come back the same.
        Some(self.dir.as_path())
            .filter(|d| !d.as_os_str().is_empty())
            .and_then(|p| p.to_str())
            .map(|p| format!("files.open {p}"))
    }

    fn icon(&self) -> Icon {
        Icon::Files
    }

    fn busy(&self) -> bool {
        self.loading
    }

    fn catch_up(&mut self) {
        self.poll();
    }

    fn title(&self) -> String {
        format!("Files · {}", name_of(&self.dir))
    }

    fn handle_key(&mut self, key: KeyEvent) -> AppEffect {
        self.message = None;
        if self.du.is_some() {
            return self.du_key(key);
        }
        match self.mode {
            Mode::Browse => self.browse_key(key),
            Mode::Filter
            | Mode::Rename(_)
            | Mode::NewDir(_)
            | Mode::FindName(_)
            | Mode::FindText(_) => self.line_key(key),
            Mode::ConfirmTrash(_)
            | Mode::ConfirmDelete(_)
            | Mode::ConfirmTrashMany(_)
            | Mode::ConfirmBulk(_) => self.confirm_key(key),
            Mode::BulkWait => match key.code {
                KeyCode::Enter => self.bulk_read(),
                KeyCode::Esc => {
                    self.mode = Mode::Browse;
                    if let Some(r) = self.renaming.take() {
                        let _ = std::fs::remove_file(r.file);
                    }
                    self.message = Some(("nothing renamed".into(), false));
                    AppEffect::Redraw
                }
                _ => AppEffect::None,
            },
            Mode::OpenWith(..) => self.open_with_key(key),
        }
    }

    fn deadline(&self) -> Option<Instant> {
        if self.pending.is_some() {
            return Some(Instant::now());
        }
        // The disk-usage count moves while it sizes.
        self.du
            .as_ref()
            .filter(|d| d.items.is_none())
            .map(|_| Instant::now() + DU_TICK)
    }

    fn on_timer(&mut self, _now: Instant) -> AppEffect {
        if let Some(cmd) = self.pending.take() {
            return AppEffect::Command(cmd);
        }
        self.du_poll();
        AppEffect::Redraw
    }

    fn handle_mouse(&mut self, event: MouseEvent) -> AppEffect {
        if self.mode != Mode::Browse {
            return AppEffect::None;
        }
        match event.kind {
            MouseKind::ScrollDown => self.select(self.selected + 3),
            MouseKind::ScrollUp => self.select(self.selected.saturating_sub(3)),
            MouseKind::Down(MouseButton::Left) => {
                // Row 0 is the path; the list starts on row 1.
                let Some(row) = usize::from(event.y).checked_sub(1) else {
                    return AppEffect::None;
                };
                let i = self.top + row;
                if i >= self.shown.len() {
                    return AppEffect::None;
                }
                if i == self.selected {
                    // A click on the selected entry opens it.
                    self.open_selected()
                } else {
                    self.select(i)
                }
            }
            _ => AppEffect::None,
        }
    }

    fn render(&mut self, area: TuiRect, buf: &mut Buffer, theme: &Theme) {
        self.poll();
        self.du_poll();
        self.images.clear();
        if area.height < 3 || area.width < 8 {
            return;
        }
        if self.du.is_some() {
            self.draw_du(area, buf, theme);
            return;
        }
        let c = &theme.colors;
        // The folder on top: in an archive, the archive and the folder in
        // it; search results say what they are.
        let top_path = match (&self.archive, &self.results) {
            (_, Some(r)) => PathBuf::from(&r.title),
            (Some(a), None) if a.inner.is_empty() => a.file.clone(),
            (Some(a), None) => a.file.join(&a.inner),
            (None, None) => self.dir.clone(),
        };
        let path = fit_path(&top_path, usize::from(area.width.saturating_sub(1)));
        buf.set_string(
            area.x + 1,
            area.y,
            &path,
            tui_style(Style::new().fg(c.muted).attrs(Attrs::BOLD)),
        );
        let body_h = area.height - 2;
        self.rows = body_h;
        let body = TuiRect::new(area.x, area.y + 1, area.width, body_h);
        if area.width >= 60 {
            let list_w = (area.width * 2 / 5).max(24);
            let list = TuiRect::new(body.x, body.y, list_w, body.height);
            self.draw_list(list, buf, theme);
            let sep_x = body.x + list_w;
            for y in body.top()..body.bottom() {
                buf.set_string(sep_x, y, "│", tui_style(Style::new().fg(c.border)));
            }
            let preview = TuiRect::new(
                sep_x + 2,
                body.y,
                area.right().saturating_sub(sep_x + 3),
                body.height,
            );
            self.draw_preview(preview, buf, theme);
        } else {
            self.draw_list(body, buf, theme);
        }
        let (line, style) = self.bottom_line(theme);
        let y = area.bottom() - 1;
        buf.set_stringn(
            area.x + 1,
            y,
            fit(&line, usize::from(area.width - 1)),
            usize::from(area.width - 1),
            tui_style(style),
        );
    }

    fn images(&self) -> Vec<AppImage> {
        self.images.clone()
    }

    fn set_waker(&mut self, waker: AppWaker) {
        if let Ok(mut w) = self.waker.lock() {
            *w = Some(waker.clone());
        }
        // The first listing may be done already.
        waker.wake();
    }
}

/// What [`Files`] starts with (`[files]`, D-122).
#[derive(Debug, Clone, Default)]
pub struct FilesOptions {
    pub bookmarks: Vec<PathBuf>,
    pub two_pane: bool,
    /// The config file `b` writes bookmarks into.
    pub config_path: Option<PathBuf>,
    /// Where open-with choices are kept.
    pub state_dir: Option<PathBuf>,
}

/// The trash folder, where the system has one TermOS can show.
fn trash_dir() -> Option<PathBuf> {
    let home = home()?;
    let d = if cfg!(target_os = "macos") {
        home.join(".Trash")
    } else if cfg!(windows) {
        return None;
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .filter(|v| !v.is_empty())
            .map_or_else(|| home.join(".local/share"), PathBuf::from)
            .join("Trash/files")
    };
    d.is_dir().then_some(d)
}

/// The file manager's window (D-122): one pane, or two side by side
/// (`Tab` opens and switches, `F5` copies and `F6` moves to the other),
/// with a sidebar of places when there is room (Home, bookmarks, Trash);
/// `b` bookmarks the folder, `'` lists the bookmarks.
pub struct Files {
    panes: Vec<FileManager>,
    active: usize,
    opts: FilesOptions,
    /// The bookmark list (`'`), and the selected place.
    places: Option<usize>,
    message: Option<(String, bool)>,
    waker: Option<AppWaker>,
    /// Where the right pane starts, from the window's left edge (mouse).
    split_at: u16,
}

impl std::fmt::Debug for Files {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Files")
            .field("panes", &self.panes.len())
            .finish_non_exhaustive()
    }
}

/// The sidebar is shown from this width.
const SIDEBAR_FROM: u16 = 100;
const SIDEBAR_W: u16 = 22;

impl Files {
    pub fn open(dir: PathBuf, opts: FilesOptions) -> Self {
        let mut first = FileManager::open(dir.clone());
        if let Some(s) = &opts.state_dir {
            first.set_state_dir(s.clone());
        }
        let mut panes = vec![first];
        if opts.two_pane {
            panes.push(Self::pane(dir, &opts));
        }
        Self {
            panes,
            active: 0,
            opts,
            places: None,
            message: None,
            waker: None,
            split_at: u16::MAX,
        }
    }

    fn pane(dir: PathBuf, opts: &FilesOptions) -> FileManager {
        let mut p = FileManager::open(dir);
        if let Some(s) = &opts.state_dir {
            p.set_state_dir(s.clone());
        }
        p
    }

    /// The pane with the focus.
    pub fn active(&self) -> &FileManager {
        &self.panes[self.active]
    }

    /// The places of the sidebar and the `'` list: Home, the bookmarks,
    /// the trash.
    fn places(&self) -> Vec<(String, PathBuf)> {
        let mut v = Vec::new();
        if let Some(h) = home() {
            v.push(("Home".to_owned(), h));
        }
        for b in &self.opts.bookmarks {
            v.push((name_of(b), b.clone()));
        }
        if let Some(t) = trash_dir() {
            v.push(("Trash".to_owned(), t));
        }
        v
    }

    fn other_dir(&self) -> Option<PathBuf> {
        (self.panes.len() == 2).then(|| self.panes[1 - self.active].dir().to_owned())
    }

    /// `b`: bookmarks the folder shown, or takes its bookmark away, in
    /// the config file too.
    fn toggle_bookmark(&mut self) {
        let dir = self.active().dir().to_owned();
        let removed = if let Some(i) = self.opts.bookmarks.iter().position(|b| *b == dir) {
            self.opts.bookmarks.remove(i);
            true
        } else {
            self.opts.bookmarks.push(dir.clone());
            false
        };
        let saved = self
            .opts
            .config_path
            .as_deref()
            .map(|p| write_bookmarks(p, &self.opts.bookmarks));
        let what = if removed {
            "removed the bookmark"
        } else {
            "bookmarked"
        };
        self.message = Some(match saved {
            Some(Err(e)) => (format!("{what} for now; cannot save it: {e}"), true),
            _ => (format!("{what} {}", name_of(&dir)), false),
        });
    }

    fn places_key(&mut self, key: KeyEvent) -> AppEffect {
        let places = self.places();
        let Some(sel) = self.places.as_mut() else {
            return AppEffect::None;
        };
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                *sel = (*sel + 1).min(places.len().saturating_sub(1))
            }
            KeyCode::Char('k') | KeyCode::Up => *sel = sel.saturating_sub(1),
            KeyCode::Enter => {
                let chosen = places.get(*sel).map(|(_, p)| p.clone());
                self.places = None;
                if let Some(p) = chosen {
                    let a = self.active;
                    self.panes[a].go(p, None);
                }
            }
            _ => self.places = None,
        }
        AppEffect::Redraw
    }

    fn draw_places(&self, area: TuiRect, buf: &mut Buffer, theme: &Theme) {
        let c = &theme.colors;
        buf.set_string(
            area.x + 1,
            area.y,
            "Places",
            tui_style(Style::new().fg(c.muted).attrs(Attrs::BOLD)),
        );
        let here = self.active().dir();
        for (i, (name, path)) in self.places().iter().enumerate() {
            let y = area.y + 1 + u16::try_from(i).unwrap_or(u16::MAX);
            if y >= area.bottom() {
                break;
            }
            let mut st = Style::new().fg(if path == here { c.accent } else { c.fg });
            if self.places == Some(i) {
                st = c.selected(st);
                buf.set_style(
                    TuiRect::new(area.x, y, area.width, 1),
                    tui_style(Style::new().bg(c.selection_bg)),
                );
            }
            buf.set_stringn(
                area.x + 1,
                y,
                fit(name, usize::from(area.width.saturating_sub(2))),
                usize::from(area.width.saturating_sub(2)),
                tui_style(st),
            );
        }
    }
}

/// `[files] bookmarks` written into the config file, keeping the rest
/// of it as it is.
fn write_bookmarks(config: &Path, bookmarks: &[PathBuf]) -> Result<(), String> {
    let text = std::fs::read_to_string(config).unwrap_or_default();
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e| format!("{e}"))?;
    if !doc.contains_table("files") {
        doc["files"] = toml_edit::table();
    }
    let mut list = toml_edit::Array::new();
    for b in bookmarks {
        list.push(b.display().to_string());
    }
    doc["files"]["bookmarks"] = toml_edit::value(list);
    std::fs::write(config, doc.to_string()).map_err(|e| e.to_string())
}

impl App for Files {
    fn reopen(&self) -> Option<String> {
        self.active().reopen()
    }

    fn icon(&self) -> Icon {
        Icon::Files
    }

    fn busy(&self) -> bool {
        self.panes.iter().any(App::busy)
    }

    fn catch_up(&mut self) {
        for p in &mut self.panes {
            p.catch_up();
        }
        // Something was put in the other pane's folder.
        if self.panes.len() == 2 {
            for i in 0..2 {
                if self.panes[i].take_touched_other() {
                    self.panes[1 - i].refresh_listing();
                }
            }
        }
    }

    fn title(&self) -> String {
        self.active().title()
    }

    fn handle_key(&mut self, key: KeyEvent) -> AppEffect {
        self.message = None;
        if self.places.is_some() {
            return self.places_key(key);
        }
        let browsing = self.active().is_browsing();
        if browsing && key.mods.is_empty() {
            match key.code {
                KeyCode::Tab => {
                    if self.panes.len() == 1 {
                        let dir = self.active().dir().to_owned();
                        self.panes.push(Self::pane(dir, &self.opts));
                        if let Some(w) = &self.waker {
                            self.panes[1].set_waker(w.clone());
                        }
                    }
                    self.active = (self.active + 1) % self.panes.len();
                    return AppEffect::Redraw;
                }
                KeyCode::F(5) | KeyCode::F(6) if self.panes.len() == 2 => {
                    let cut = key.code == KeyCode::F(6);
                    let sources = self.panes[self.active].selection();
                    if sources.is_empty() {
                        return AppEffect::None;
                    }
                    let other = 1 - self.active;
                    self.panes[other].paste_here(sources, cut);
                    if cut {
                        self.panes[self.active].refresh_listing();
                    }
                    let n = self.panes[self.active].selection().len();
                    self.message = Some((
                        format!(
                            "{} {n} to the other pane",
                            if cut { "moving" } else { "copying" }
                        ),
                        false,
                    ));
                    return AppEffect::Redraw;
                }
                KeyCode::Char('b') => {
                    self.toggle_bookmark();
                    return AppEffect::Redraw;
                }
                KeyCode::Char('\'') => {
                    self.places = Some(0);
                    return AppEffect::Redraw;
                }
                _ => {}
            }
        }
        let other = self.other_dir();
        let a = self.active;
        self.panes[a].set_other(other);
        self.panes[a].handle_key(key)
    }

    fn handle_mouse(&mut self, event: MouseEvent) -> AppEffect {
        // Which pane: the left or right half.
        if self.panes.len() == 2 && event.x >= self.split_at {
            self.active = 1;
        } else if self.panes.len() == 2 {
            self.active = 0;
        }
        let a = self.active;
        self.panes[a].handle_mouse(event)
    }

    fn render(&mut self, area: TuiRect, buf: &mut Buffer, theme: &Theme) {
        let c = theme.colors.clone();
        let mut body = area;
        let sidebar = self.panes.len() == 1 && area.width >= SIDEBAR_FROM;
        if sidebar || self.places.is_some() {
            let w = SIDEBAR_W.min(area.width / 3);
            self.draw_places(TuiRect::new(area.x, area.y, w, area.height), buf, theme);
            for y in area.top()..area.bottom() {
                buf.set_string(area.x + w, y, "│", tui_style(Style::new().fg(c.border)));
            }
            body = TuiRect::new(
                area.x + w + 1,
                area.y,
                area.width.saturating_sub(w + 1),
                area.height,
            );
        }
        if self.panes.len() == 2 {
            let half = body.width / 2;
            self.split_at = (body.x - area.x) + half;
            let left = TuiRect::new(body.x, body.y, half, body.height);
            let right = TuiRect::new(
                body.x + half + 1,
                body.y,
                body.width.saturating_sub(half + 1),
                body.height,
            );
            self.panes[0].render(left, buf, theme);
            self.panes[1].render(right, buf, theme);
            for y in body.top()..body.bottom() {
                buf.set_string(body.x + half, y, "│", tui_style(Style::new().fg(c.border)));
            }
            // The active pane's path stands out.
            let x = if self.active == 0 { left.x } else { right.x };
            let w = if self.active == 0 {
                left.width
            } else {
                right.width
            };
            buf.set_style(
                TuiRect::new(x, body.y, w, 1),
                tui_style(Style::new().fg(c.accent)),
            );
        } else {
            self.panes[0].render(body, buf, theme);
        }
        if let Some((m, err)) = &self.message {
            let st = Style::new().fg(if *err { c.error } else { c.muted });
            buf.set_stringn(
                body.x + 1,
                area.bottom().saturating_sub(1),
                fit(m, usize::from(body.width.saturating_sub(1))),
                usize::from(body.width.saturating_sub(1)),
                tui_style(st),
            );
        }
    }

    fn images(&self) -> Vec<AppImage> {
        self.panes.iter().flat_map(App::images).collect()
    }

    fn set_waker(&mut self, waker: AppWaker) {
        for p in &mut self.panes {
            p.set_waker(waker.clone());
        }
        self.waker = Some(waker);
    }

    fn deadline(&self) -> Option<Instant> {
        self.panes.iter().filter_map(App::deadline).min()
    }

    fn on_timer(&mut self, now: Instant) -> AppEffect {
        let mut out = AppEffect::None;
        for p in &mut self.panes {
            if p.deadline().is_some_and(|d| d <= now) {
                let e = p.on_timer(now);
                if e != AppEffect::None {
                    out = e;
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, kind: EntryKind, size: u64, secs: u64) -> Entry {
        Entry {
            name: name.into(),
            path: PathBuf::from(name),
            kind,
            size,
            modified: Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs)),
            executable: false,
        }
    }

    #[test]
    fn folders_first_then_by_sort() {
        let mut v = vec![
            entry("b.txt", EntryKind::File, 10, 3),
            entry("A.txt", EntryKind::File, 30, 1),
            entry("zdir", EntryKind::Dir, 0, 0),
            entry("c.txt", EntryKind::File, 20, 2),
        ];
        let names = |v: &[Entry]| v.iter().map(|e| e.name.clone()).collect::<Vec<_>>();
        sort_entries(&mut v, Sort::Name);
        assert_eq!(names(&v), ["zdir", "A.txt", "b.txt", "c.txt"]);
        sort_entries(&mut v, Sort::Size);
        assert_eq!(names(&v), ["zdir", "A.txt", "c.txt", "b.txt"]);
        sort_entries(&mut v, Sort::Modified);
        assert_eq!(names(&v), ["zdir", "b.txt", "c.txt", "A.txt"]);
    }

    #[test]
    fn sizes_names_and_kinds() {
        assert_eq!(human_size(512), "512B");
        assert_eq!(human_size(1536), "1.5K");
        assert_eq!(human_size(25 * 1024 * 1024), "25M");
        let dir = tempfile::tempdir().expect("tempdir");
        let d = dir.path();
        std::fs::write(d.join("a.txt"), "x").expect("write");
        std::fs::write(d.join(".env"), "x").expect("write");
        assert_eq!(free_name(d, "new.txt"), d.join("new.txt"));
        assert_eq!(free_name(d, "a.txt"), d.join("a (copy).txt"));
        std::fs::write(d.join("a (copy).txt"), "x").expect("write");
        assert_eq!(free_name(d, "a.txt"), d.join("a (copy 2).txt"));
        assert_eq!(free_name(d, ".env"), d.join(".env (copy)"));
        assert_eq!(open_kind(d), OpenKind::Dir);
        assert_eq!(open_kind(Path::new("x/Photo.JPG")), OpenKind::Image);
        assert_eq!(open_kind(Path::new("README.md")), OpenKind::Markdown);
        assert_eq!(open_kind(Path::new("main.rs")), OpenKind::Other);
    }

    #[test]
    fn copy_and_move_trees() {
        let dir = tempfile::tempdir().expect("tempdir");
        let d = dir.path();
        std::fs::create_dir_all(d.join("src/sub")).expect("mkdir");
        std::fs::write(d.join("src/sub/f.txt"), "hello").expect("write");
        std::fs::create_dir(d.join("dest")).expect("mkdir");
        assert_eq!(paste(&[d.join("src")], &d.join("dest"), false), Ok(1));
        assert_eq!(
            std::fs::read_to_string(d.join("dest/src/sub/f.txt")).expect("copied"),
            "hello"
        );
        // Again: a second copy beside the first.
        assert_eq!(paste(&[d.join("src")], &d.join("dest"), false), Ok(1));
        assert!(d.join("dest/src (copy)/sub/f.txt").exists());
        // Moving.
        assert_eq!(paste(&[d.join("src")], &d.join("dest/src"), true), Ok(1));
        assert!(!d.join("src").exists());
        assert!(d.join("dest/src/src/sub/f.txt").exists());
        // Into itself: refused.
        assert!(paste(&[d.join("dest")], &d.join("dest/src"), false).is_err());
    }

    #[test]
    fn previews_by_kind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let d = dir.path();
        std::fs::write(d.join("t.txt"), "one\n\ttwo\n").expect("write");
        std::fs::write(d.join("b.bin"), [0u8, 1, 2, 3]).expect("write");
        std::fs::write(d.join("n.md"), "# Title\n\ntext\n").expect("write");
        let pic = Picture::from_rgba(2, 2, vec![255; 16]).expect("valid");
        std::fs::write(d.join("p.png"), pic.to_png()).expect("write");
        assert!(
            matches!(read_preview(&d.join("t.txt")), Preview::Text(l) if l == ["one", "    two"])
        );
        assert!(
            matches!(read_preview(&d.join("b.bin")), Preview::Info(s) if s.starts_with("binary"))
        );
        assert!(matches!(
            read_preview(&d.join("n.md")),
            Preview::Markdown(_)
        ));
        assert!(matches!(
            read_preview(&d.join("p.png")),
            Preview::Image { .. }
        ));
        assert!(matches!(read_preview(d), Preview::Dir(Ok(v)) if v.len() == 4));
    }

    #[test]
    fn the_window() {
        let dir = tempfile::tempdir().expect("tempdir");
        let d = dir.path();
        std::fs::create_dir(d.join("photos")).expect("mkdir");
        std::fs::write(d.join("todo.txt"), "buy milk\ncall home\n").expect("write");
        std::fs::write(d.join("big.log"), "x".repeat(5000)).expect("write");
        std::fs::write(d.join(".hidden"), "").expect("write");
        let mut fm = FileManager::open(d.to_owned());
        // Wait for the listing and the first preview.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            crate::testing::render_text(&mut fm, 70, 8);
            if !fm.loading && !matches!(fm.preview, Preview::Loading) {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "the worker answers");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let _ = fm.handle_key(KeyEvent::plain(KeyCode::End));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while fm.poll() || matches!(fm.preview, Preview::Loading) {
            assert!(std::time::Instant::now() < deadline, "the preview arrives");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let text = crate::testing::render_text(&mut fm, 70, 8);
        // The first row is the temporary folder's path.
        let text: String = text.lines().skip(1).map(|l| format!("{l}\n")).collect();
        insta::assert_snapshot!("files-window", text);
    }

    /// Renders until `done` holds (the worker answers).
    fn settle(fm: &mut FileManager, done: impl Fn(&FileManager) -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            crate::testing::render_text(fm, 80, 12);
            if !fm.loading && done(fm) {
                return;
            }
            assert!(std::time::Instant::now() < deadline, "the worker answers");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn key(fm: &mut FileManager, k: KeyCode) -> AppEffect {
        fm.handle_key(KeyEvent::plain(k))
    }

    fn select_name(fm: &mut FileManager, name: &str) {
        let i = fm
            .shown
            .iter()
            .position(|e| e.name == name)
            .expect("listed");
        let _ = fm.select(i);
    }

    #[test]
    fn marks_bulk_rename_and_undo() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.txt"), "A").expect("write");
        std::fs::write(dir.path().join("b.txt"), "B").expect("write");
        let mut fm = FileManager::open(dir.path().to_owned());
        settle(&mut fm, |f| f.shown.len() == 2);
        key(&mut fm, KeyCode::Char('*'));
        assert_eq!(fm.marks.len(), 2);
        let AppEffect::Command(cmd) = key(&mut fm, KeyCode::F(2)) else {
            panic!("the editor opens");
        };
        let file = PathBuf::from(cmd.strip_prefix("file.edit ").expect("file.edit"));
        let text = std::fs::read_to_string(&file).expect("the names");
        // Swapped in the editor.
        let edited = text
            .replace("\ta.txt", "\tX")
            .replace("\tb.txt", "\ta.txt")
            .replace("\tX", "\tb.txt");
        std::fs::write(&file, edited).expect("write");
        key(&mut fm, KeyCode::Enter);
        assert!(
            matches!(fm.mode, Mode::ConfirmBulk(ref c) if c.len() == 2),
            "{:?}",
            fm.mode
        );
        key(&mut fm, KeyCode::Char('y'));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).expect("a"),
            "B"
        );
        settle(&mut fm, |f| f.shown.len() == 2);
        key(&mut fm, KeyCode::Char('u'));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).expect("a"),
            "A"
        );
    }

    #[test]
    fn archives_browse_like_folders_and_extract() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("src");
        std::fs::create_dir_all(src.join("docs")).expect("mkdir");
        std::fs::write(src.join("docs/guide.md"), "# guide\n").expect("write");
        archive::make_zip(&[src.join("docs")], &dir.path().join("pack.zip")).expect("zip");
        let mut fm = FileManager::open(dir.path().to_owned());
        settle(&mut fm, |f| f.shown.iter().any(|e| e.name == "pack.zip"));
        select_name(&mut fm, "pack.zip");
        key(&mut fm, KeyCode::Enter);
        settle(&mut fm, |f| f.archive.is_some());
        assert_eq!(
            fm.shown.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            vec!["docs"]
        );
        key(&mut fm, KeyCode::Enter);
        assert_eq!(
            fm.shown.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            vec!["guide.md"]
        );
        let text = crate::testing::render_text(&mut fm, 80, 6);
        assert!(text.contains("pack.zip"), "{text}");
        // Copying out of an archive is extracting.
        key(&mut fm, KeyCode::Char('X'));
        settle(&mut fm, |f| {
            f.message
                .as_ref()
                .is_some_and(|(m, _)| m.starts_with("extracted"))
        });
        assert!(dir.path().join("guide.md").is_file());
        // Up and out.
        key(&mut fm, KeyCode::Char('h'));
        key(&mut fm, KeyCode::Char('h'));
        settle(&mut fm, |f| {
            f.archive.is_none() && f.current().is_some_and(|e| e.name == "pack.zip")
        });
    }

    #[test]
    fn two_panes_copy_to_each_other() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        std::fs::create_dir_all(&a).expect("mkdir");
        std::fs::create_dir_all(&b).expect("mkdir");
        std::fs::write(a.join("note.txt"), "n").expect("write");
        let mut files = Files::open(
            a.clone(),
            FilesOptions {
                two_pane: true,
                ..FilesOptions::default()
            },
        );
        files.panes[1].go(b.clone(), None);
        settle(&mut files.panes[0], |f| f.shown.len() == 1);
        settle(&mut files.panes[1], |f| !f.loading);
        assert_eq!(
            files.handle_key(KeyEvent::plain(KeyCode::F(5))),
            AppEffect::Redraw
        );
        settle(&mut files.panes[1], |f| {
            f.shown.iter().any(|e| e.name == "note.txt")
        });
        assert!(a.join("note.txt").is_file(), "copied, not moved");
        // Tab gives the other pane the keys.
        files.handle_key(KeyEvent::plain(KeyCode::Tab));
        assert_eq!(files.active().dir(), b.as_path());
        let text = crate::testing::render_text(&mut files, 100, 8);
        assert!(text.contains('│'), "{text}");
    }

    #[test]
    fn bookmarks_are_written_into_the_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = dir.path().join("config.toml");
        std::fs::write(&config, "# mine\n[ui]\nicons = \"ascii\"\n").expect("write");
        let mut files = Files::open(
            dir.path().to_owned(),
            FilesOptions {
                config_path: Some(config.clone()),
                ..FilesOptions::default()
            },
        );
        settle(&mut files.panes[0], |f| !f.loading);
        files.handle_key(KeyEvent::ch('b'));
        let text = std::fs::read_to_string(&config).expect("read");
        assert!(text.starts_with("# mine"), "{text}");
        assert!(
            text.contains("[files]") && text.contains("bookmarks"),
            "{text}"
        );
        assert_eq!(files.opts.bookmarks, vec![dir.path().to_owned()]);
        files.handle_key(KeyEvent::ch('b'));
        assert!(files.opts.bookmarks.is_empty());
    }

    #[test]
    fn disk_usage_sizes_the_folder() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("big")).expect("mkdir");
        std::fs::write(dir.path().join("big/x"), vec![0u8; 5000]).expect("write");
        std::fs::write(dir.path().join("small"), "s").expect("write");
        let mut fm = FileManager::open(dir.path().to_owned());
        settle(&mut fm, |f| f.shown.len() == 2);
        key(&mut fm, KeyCode::Char('D'));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let text = crate::testing::render_text(&mut fm, 80, 8);
            if text.contains("big/") {
                assert!(text.find("big/") < text.find("small"), "{text}");
                break;
            }
            assert!(std::time::Instant::now() < deadline, "sized: {text}");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        key(&mut fm, KeyCode::Esc);
        assert!(fm.du.is_none());
    }

    #[test]
    fn paths_fit_from_the_end() {
        let p = Path::new("/home/someone/projects/termos/crates");
        assert_eq!(fit_path(p, 80), "/home/someone/projects/termos/crates");
        assert_eq!(fit_path(p, 20), "…/termos/crates");
    }
}
