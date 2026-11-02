// SPDX-License-Identifier: GPL-3.0-or-later
//! The file manager: a folder's contents on the left, a preview of the
//! selected entry on the right (pictures, markdown, text, folders).
//!
//! Listing, previews and copying run on the manager's own worker thread,
//! so a huge folder or a slow disk never makes the desktop wait. Deleting
//! moves to the system trash (D-050); where there is none, it asks again
//! and then deletes for good.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect as TuiRect;
use termos_core::app::{App, AppEffect, AppImage, AppWaker, new_image_key, tui_style};
use termos_core::input::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseKind};
use termos_render::cell::{Attrs, Style};
use termos_render::graphics::Picture;
use termos_render::grid::Rect;
use termos_render::theme::Theme;
use unicode_width::UnicodeWidthStr;

use crate::markdown;

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
}

impl Entry {
    fn is_dir(&self) -> bool {
        matches!(self.kind, EntryKind::Dir | EntryKind::Link { dir: true })
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
}

enum Done {
    Listed(PathBuf, Result<Vec<Entry>, String>),
    Preview(u64, Preview),
    Pasted(Result<usize, String>),
    Trashed(PathBuf, Result<(), String>),
    Deleted(PathBuf, Result<(), String>),
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
        self.dir = dir.clone();
        self.filter.clear();
        self.reselect = select;
        self.loading = true;
        self.send(Job::List(dir));
    }

    fn reload(&mut self) {
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
                        self.reload();
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
        if e.is_dir() {
            self.go(e.path, None);
            return AppEffect::Redraw;
        }
        AppEffect::Command(format!("file.open {}", e.path.display()))
    }

    fn browse_key(&mut self, key: KeyEvent) -> AppEffect {
        let page = usize::from(self.rows.max(2) - 1);
        if !key.mods.is_empty() {
            return AppEffect::None;
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
            KeyCode::Char('r') | KeyCode::F(2) => match self.current() {
                Some(e) => {
                    self.mode = Mode::Rename(e.name.clone());
                    AppEffect::Redraw
                }
                None => AppEffect::None,
            },
            KeyCode::Char('n') => {
                self.mode = Mode::NewDir(String::new());
                AppEffect::Redraw
            }
            KeyCode::Char('y') | KeyCode::Char('x') => {
                let Some(e) = self.current().cloned() else {
                    return AppEffect::None;
                };
                let cut = key.code == KeyCode::Char('x');
                let what = if cut { "cut" } else { "copied" };
                self.message = Some((format!("{what} {} · p to paste", e.name), false));
                self.clip = Some((vec![e.path], cut));
                AppEffect::Redraw
            }
            KeyCode::Char('p') => {
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
            KeyCode::Char('d') | KeyCode::Delete => match self.current() {
                Some(e) => {
                    self.mode = Mode::ConfirmTrash(e.path.clone());
                    AppEffect::Redraw
                }
                None => AppEffect::None,
            },
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
            KeyCode::Char('q') | KeyCode::Esc => AppEffect::Close,
            _ => AppEffect::None,
        }
    }

    /// Keys while typing a name or a filter.
    fn line_key(&mut self, key: KeyEvent) -> AppEffect {
        let text = match &mut self.mode {
            Mode::Rename(t) | Mode::NewDir(t) => t,
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
                _ => {}
            }
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
            let name_w = area.width.saturating_sub(size_w + 1);
            let shown = fit(&name, usize::from(name_w));
            buf.set_stringn(area.x + 1, y, &shown, usize::from(name_w), tui_style(style));
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
                });
            }
        }
    }

    fn bottom_line(&self, theme: &Theme) -> (String, Style) {
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
            Mode::Browse => match &self.message {
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
    fn title(&self) -> String {
        format!("Files · {}", name_of(&self.dir))
    }

    fn handle_key(&mut self, key: KeyEvent) -> AppEffect {
        self.message = None;
        match self.mode {
            Mode::Browse => self.browse_key(key),
            Mode::Filter | Mode::Rename(_) | Mode::NewDir(_) => self.line_key(key),
            Mode::ConfirmTrash(_) | Mode::ConfirmDelete(_) => self.confirm_key(key),
        }
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
        self.images.clear();
        if area.height < 3 || area.width < 8 {
            return;
        }
        let c = &theme.colors;
        // The folder on top.
        let path = fit_path(&self.dir, usize::from(area.width.saturating_sub(1)));
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

    #[test]
    fn paths_fit_from_the_end() {
        let p = Path::new("/home/someone/projects/termos/crates");
        assert_eq!(fit_path(p, 80), "/home/someone/projects/termos/crates");
        assert_eq!(fit_path(p, 20), "…/termos/crates");
    }
}
