// SPDX-License-Identifier: GPL-3.0-or-later
//! The image viewer: one image at a time from a folder, fitted to the
//! window (or at most its own size), shown with the best method each
//! client's terminal has (pixels, or blocks; ARCHITECTURE.md §5.8).
//!
//! Decoding runs on its own thread, so a large image never stalls the
//! desktop; the viewer asks for a redraw when it is ready.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::sync::{Arc, Mutex};

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect as TuiRect;
use ratatui_core::text::Line;
use ratatui_core::widgets::Widget;
use termos_core::app::{App, AppEffect, AppImage, AppWaker, new_image_key, tui_style};
use termos_core::input::{KeyCode, KeyEvent};
use termos_render::graphics::Picture;
use termos_render::grid::Rect;
use termos_render::theme::Theme;

/// Files bigger than this are not read.
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Extensions the viewer pages through.
const EXTENSIONS: [&str; 6] = ["png", "jpg", "jpeg", "gif", "webp", "bmp"];

enum State {
    Loading,
    Ready { key: u64, picture: Arc<Picture> },
    Failed(String),
}

type Decoded = (usize, Result<Picture, String>);

pub struct ImageViewer {
    files: Vec<PathBuf>,
    index: usize,
    state: State,
    /// Fit to the window (`true`) or at most the image's own size.
    upscale: bool,
    waker: Arc<Mutex<Option<AppWaker>>>,
    rx: Option<Receiver<Decoded>>,
    area: (u16, u16),
}

impl std::fmt::Debug for ImageViewer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImageViewer")
            .field("file", &self.files.get(self.index))
            .finish_non_exhaustive()
    }
}

fn is_image(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Reads and decodes one file, within the size limits (also the
/// desktop's wallpaper).
pub fn load(path: &Path) -> Result<Picture, String> {
    let meta =
        std::fs::metadata(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!(
            "{} is too large ({} MiB; at most {} MiB)",
            path.display(),
            meta.len() / (1024 * 1024),
            MAX_FILE_BYTES / (1024 * 1024)
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Picture::decode(&bytes).map_err(|e| e.to_string())
}

impl ImageViewer {
    /// Opens `path`; the other images in its folder are a key away.
    pub fn open(path: PathBuf) -> Self {
        let mut files: Vec<PathBuf> = path
            .parent()
            .and_then(|dir| std::fs::read_dir(dir).ok())
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_file() && is_image(p))
                    .collect()
            })
            .unwrap_or_default();
        files.sort();
        let index = match files.iter().position(|f| *f == path) {
            Some(i) => i,
            None => {
                files.insert(0, path);
                0
            }
        };
        let mut viewer = Self {
            files,
            index,
            state: State::Loading,
            upscale: true,
            waker: Arc::new(Mutex::new(None)),
            rx: None,
            area: (0, 0),
        };
        viewer.start_loading();
        viewer
    }

    fn start_loading(&mut self) {
        let Some(path) = self.files.get(self.index).cloned() else {
            return;
        };
        self.state = State::Loading;
        let (tx, rx) = channel();
        self.rx = Some(rx);
        let index = self.index;
        let waker = Arc::clone(&self.waker);
        let spawned = std::thread::Builder::new()
            .name("image-decode".into())
            .spawn(move || {
                let _ = tx.send((index, load(&path)));
                if let Some(w) = waker.lock().ok().and_then(|w| w.clone()) {
                    w.wake();
                }
            });
        if let Err(e) = spawned {
            self.state = State::Failed(format!("cannot start decoding: {e}"));
            self.rx = None;
        }
    }

    /// Takes a finished decode, if any.
    fn poll(&mut self) {
        let Some(rx) = &self.rx else {
            return;
        };
        match rx.try_recv() {
            // A result for an image paged away from is dropped.
            Ok((index, result)) if index == self.index => {
                self.state = match result {
                    Ok(p) => State::Ready {
                        key: new_image_key(),
                        picture: Arc::new(p),
                    },
                    Err(e) => State::Failed(e),
                };
                self.rx = None;
            }
            Ok(_) | Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.rx = None,
        }
    }

    /// Waits for the current image (tests).
    pub fn wait_loaded(&mut self) {
        if let Some(rx) = self.rx.take()
            && let Ok((index, result)) = rx.recv()
            && index == self.index
        {
            self.state = match result {
                Ok(p) => State::Ready {
                    key: new_image_key(),
                    picture: Arc::new(p),
                },
                Err(e) => State::Failed(e),
            };
        }
    }

    fn step(&mut self, forward: bool) -> AppEffect {
        if self.files.len() < 2 {
            return AppEffect::None;
        }
        let n = self.files.len();
        self.index = if forward {
            (self.index + 1) % n
        } else {
            (self.index + n - 1) % n
        };
        self.start_loading();
        AppEffect::Redraw
    }

    fn name(&self) -> String {
        self.files
            .get(self.index)
            .and_then(|p| p.file_name())
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
    }
}

impl App for ImageViewer {
    fn icon(&self) -> termos_render::icons::Icon {
        termos_render::icons::Icon::ImageViewer
    }

    fn busy(&self) -> bool {
        matches!(self.state, State::Loading)
    }

    fn catch_up(&mut self) {
        self.poll();
    }

    fn title(&self) -> String {
        format!("Image · {}", self.name())
    }

    fn handle_key(&mut self, key: KeyEvent) -> AppEffect {
        if !key.mods.is_empty() {
            return AppEffect::None;
        }
        match key.code {
            KeyCode::Char('n' | 'j' | ' ') | KeyCode::Right | KeyCode::PageDown => self.step(true),
            KeyCode::Char('p' | 'k') | KeyCode::Left | KeyCode::PageUp | KeyCode::Backspace => {
                self.step(false)
            }
            KeyCode::Char('z') => {
                self.upscale = !self.upscale;
                AppEffect::Redraw
            }
            KeyCode::Char('q') | KeyCode::Esc => AppEffect::Close,
            _ => AppEffect::None,
        }
    }

    fn render(&mut self, area: TuiRect, buf: &mut Buffer, theme: &Theme) {
        self.poll();
        self.area = (area.width, area.height);
        if area.height == 0 {
            return;
        }
        let muted = tui_style(theme.overlay_muted);
        let info_y = area.y + area.height - 1;
        let mut info = format!(" {}", self.name());
        if let State::Ready { picture, .. } = &self.state {
            info.push_str(&format!("  {}×{}", picture.width(), picture.height()));
        }
        info.push_str(if self.upscale { "  fit" } else { "  1:1" });
        if self.files.len() > 1 {
            info.push_str(&format!("  {}/{}", self.index + 1, self.files.len()));
        }
        info.push_str("   n/p next/prev · z zoom · q close");
        Line::styled(info, muted).render(TuiRect::new(area.x, info_y, area.width, 1), buf);
        let message = match &self.state {
            State::Loading => Some(format!("Loading {}…", self.name())),
            State::Failed(e) => Some(e.clone()),
            State::Ready { .. } => None,
        };
        if let Some(m) = message {
            let y = area.y + area.height.saturating_sub(1) / 2;
            Line::styled(m, tui_style(theme.overlay))
                .centered()
                .render(TuiRect::new(area.x, y, area.width, 1), buf);
        }
    }

    fn images(&self) -> Vec<AppImage> {
        let State::Ready { key, picture } = &self.state else {
            return Vec::new();
        };
        let (w, h) = self.area;
        if w == 0 || h < 2 {
            return Vec::new();
        }
        vec![AppImage {
            key: *key,
            picture: Arc::clone(picture),
            rect: Rect::new(0, 0, w, h - 1),
            upscale: self.upscale,
            scroll_top: 0,
            pixels_only: false,
        }]
    }

    fn set_waker(&mut self, waker: AppWaker) {
        if let Ok(mut w) = self.waker.lock() {
            *w = Some(waker.clone());
        }
        // Work started before the waker existed may already be done:
        // look once now.
        if self.rx.is_some() {
            waker.wake();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_png(dir: &Path, name: &str, w: u32, h: u32) -> PathBuf {
        let p = dir.join(name);
        let pic = Picture::from_rgba(w, h, vec![200; (w * h * 4) as usize]).expect("valid");
        std::fs::write(&p, pic.to_png()).expect("write");
        p
    }

    #[test]
    fn opens_pages_and_zooms() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = write_png(dir.path(), "a.png", 40, 20);
        write_png(dir.path(), "b.png", 10, 10);
        std::fs::write(dir.path().join("notes.txt"), "not an image").expect("write");
        let mut v = ImageViewer::open(a);
        assert_eq!(v.files.len(), 2, "only images: {:?}", v.files);
        v.wait_loaded();
        let theme = Theme::default();
        let area = TuiRect::new(0, 0, 30, 10);
        let mut buf = Buffer::empty(area);
        v.render(area, &mut buf, &theme);
        let imgs = v.images();
        assert_eq!(imgs.len(), 1);
        assert_eq!(
            imgs[0].rect,
            Rect::new(0, 0, 30, 9),
            "all but the info line"
        );
        assert_eq!(imgs[0].picture.width(), 40);
        assert!(imgs[0].upscale);
        assert_eq!(v.title(), "Image · a.png");

        assert_eq!(v.handle_key(KeyEvent::ch('z')), AppEffect::Redraw);
        assert!(!v.images()[0].upscale);
        assert_eq!(v.handle_key(KeyEvent::ch('n')), AppEffect::Redraw);
        assert!(v.images().is_empty(), "loading the next one");
        v.wait_loaded();
        v.render(area, &mut buf, &theme);
        assert_eq!(v.images()[0].picture.width(), 10);
        assert_eq!(v.title(), "Image · b.png");
        assert_eq!(v.handle_key(KeyEvent::ch('q')), AppEffect::Close);
    }

    #[test]
    fn errors_are_shown_not_fatal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bad = dir.path().join("broken.png");
        std::fs::write(&bad, b"not a png").expect("write");
        let mut v = ImageViewer::open(bad);
        v.wait_loaded();
        let area = TuiRect::new(0, 0, 120, 5);
        let mut buf = Buffer::empty(area);
        v.render(area, &mut buf, &Theme::default());
        assert!(v.images().is_empty());
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("cannot decode"), "{text}");
        let mut missing = ImageViewer::open(dir.path().join("nope.png"));
        missing.wait_loaded();
        missing.render(area, &mut buf, &Theme::default());
        assert!(missing.images().is_empty());
    }
}
