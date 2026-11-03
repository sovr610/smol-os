// SPDX-License-Identifier: GPL-3.0-or-later
//! The desktop's wallpaper (D-087, D-089): drawn where the active
//! workspace has no tiled panes. A picture wallpaper is decoded once, on a
//! thread of its own, and then shown like any other image (in pixels where
//! the terminal can, in blocks elsewhere), under the floats.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use termos_core::app::new_image_key;
use termos_core::config::Wallpaper;
use termos_core::ids::PaneId;
use termos_render::caps::{ImageProtocol, PxSize};
use termos_render::color::Rgb;
use termos_render::graphics::Picture;
use termos_render::grid::{Grid, Rect};
use termos_term::pty::PtyEvent;

use super::{Desktop, Hover};
use crate::commands::Command;
use crate::images::{self, Candidate};
use crate::notify::Level;
use crate::wallpaper::{self, Look};

/// The wallpaper picture, when `[ui] wallpaper` names one.
#[derive(Debug, Default)]
pub(super) struct Wall {
    /// The id its decode wakes the desktop with (taken when first needed,
    /// so a pattern wallpaper uses no id).
    id: Option<PaneId>,
    picture: WallPicture,
}

#[derive(Debug, Default)]
enum WallPicture {
    #[default]
    None,
    Loading {
        path: PathBuf,
        rx: Receiver<Result<Picture, String>>,
    },
    Ready {
        path: PathBuf,
        key: u64,
        picture: Arc<Picture>,
    },
    Failed {
        path: PathBuf,
    },
}

impl WallPicture {
    fn path(&self) -> Option<&PathBuf> {
        match self {
            WallPicture::None => None,
            WallPicture::Loading { path, .. }
            | WallPicture::Ready { path, .. }
            | WallPicture::Failed { path } => Some(path),
        }
    }
}

/// The hint under the logo.
const HINT: &str = "Click a shortcut, or right-click anywhere for the menu";

impl Desktop {
    /// Follows `[ui] wallpaper`: starts decoding a newly named picture.
    pub(super) fn sync_wallpaper(&mut self) {
        let Wallpaper::Picture(path) = self.config.ui.wallpaper.clone() else {
            self.wall.picture = WallPicture::None;
            return;
        };
        let path = self.config_relative(&path);
        if self.wall.picture.path() == Some(&path) {
            return;
        }
        let id = match self.wall.id {
            Some(id) => id,
            None => {
                let id = self.alloc_id();
                self.wall.id = Some(id);
                id
            }
        };
        let (tx, rx) = channel();
        let wake = self.pty_tx.clone();
        let file = path.clone();
        let spawned = std::thread::Builder::new()
            .name("wallpaper-decode".into())
            .spawn(move || {
                let _ = tx.send(termos_apps::image_viewer::load(&file));
                // A full queue already has a wakeup in it.
                let _ = wake.try_send(PtyEvent::Wake(id));
            });
        self.wall.picture = match spawned {
            Ok(_) => WallPicture::Loading { path, rx },
            Err(e) => {
                self.notify(Level::Warn, format!("cannot load the wallpaper: {e}"));
                WallPicture::Failed { path }
            }
        };
    }

    /// `~` and paths relative to the config file's folder.
    fn config_relative(&self, path: &std::path::Path) -> PathBuf {
        let text = path.to_string_lossy();
        if text == "~" || text.starts_with("~/") || text.starts_with("~\\") {
            return self.resolve_path(&text);
        }
        if path.is_absolute() {
            return path.to_path_buf();
        }
        self.opts
            .config_path
            .parent()
            .map_or_else(|| path.to_path_buf(), |dir| dir.join(path))
    }

    /// Whether `id` is the wallpaper's (its decode finished).
    pub(super) fn is_wallpaper(&self, id: PaneId) -> bool {
        self.wall.id == Some(id)
    }

    /// Takes a finished decode.
    pub(super) fn wallpaper_woke(&mut self) {
        let WallPicture::Loading { path, rx } = &self.wall.picture else {
            return;
        };
        let next = match rx.try_recv() {
            Ok(Ok(picture)) => WallPicture::Ready {
                path: path.clone(),
                key: new_image_key(),
                picture: Arc::new(picture),
            },
            Ok(Err(e)) => {
                let shown = format!("wallpaper: {e}; showing the pattern");
                let path = path.clone();
                self.notify(Level::Warn, shown);
                WallPicture::Failed { path }
            }
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => WallPicture::Failed { path: path.clone() },
        };
        self.wall.picture = next;
        self.dirty = true;
    }

    /// Draws the wallpaper into `area` for the current client, and adds a
    /// picture to the images to place. Returns where its shortcuts are.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn draw_wallpaper(
        &mut self,
        grid: &mut Grid,
        area: Rect,
        candidates: &mut Vec<Candidate>,
        protocol: ImageProtocol,
        cell: PxSize,
        image_bg: Rgb,
    ) -> Vec<(Rect, Command)> {
        if self.config.ui.wallpaper == Wallpaper::None {
            return Vec::new();
        }
        let pointer = self
            .ui
            .pointer
            .filter(|_| matches!(self.ui.hover, Hover::Wallpaper(_)));
        let theme = &self.ui.theme;
        let (WallPicture::Ready { key, picture, .. }, Some(id)) =
            (&self.wall.picture, self.wall.id)
        else {
            return wallpaper::draw(grid, area, theme, Look::Pattern { hint: HINT }, pointer);
        };
        let regions = wallpaper::draw(grid, area, theme, Look::Picture, pointer);
        let room = wallpaper::picture_area(area, theme);
        let place = images::fit_place(
            i32::from(room.x),
            i32::from(room.y),
            room.w,
            room.h,
            picture,
            cell,
            true,
        );
        if protocol == ImageProtocol::Blocks {
            let cells = self.block_cache.get(*key, picture, place, image_bg);
            images::draw_blocks(grid, &cells, place, &[room]);
        } else {
            candidates.push(Candidate {
                pane: id,
                key: *key,
                picture: Arc::clone(picture),
                place,
                clip: room,
                pixels_only: false,
            });
        }
        regions
    }

    /// The wallpaper shortcut at (`x`, `y`), if any.
    pub(super) fn wallpaper_at(&self, x: u16, y: u16) -> Option<(usize, Command)> {
        self.ui
            .wallpaper_regions
            .iter()
            .enumerate()
            .find(|(_, (r, _))| r.contains(x, y))
            .map(|(i, (_, c))| (i, c.clone()))
    }
}
