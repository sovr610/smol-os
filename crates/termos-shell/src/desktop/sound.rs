// SPDX-License-Identifier: GPL-3.0-or-later
//! Sound on the desktop (M13e, D-128): the volume in the status line and
//! the commands that change it, on a TermOS image (PipeWire through
//! `wpctl`). In a terminal the host system has its own volume: the
//! commands say so.
//!
//! `wpctl` takes some milliseconds, so it runs on a thread; its answer
//! wakes the desktop through a pane id of its own, as the wallpaper's
//! decoding does. The status item is read at start and after each change
//! TermOS makes, never on a timer (another program's change shows at the
//! next one).

use std::sync::mpsc::{Receiver, Sender, channel};

use termos_core::ids::PaneId;
use termos_hw::sound::Volume;
use termos_term::pty::PtyEvent;

use super::Desktop;
use super::power::on_image;
use crate::notify::Level;

/// The volume item and the thread's answers.
pub(super) struct Sound {
    /// The id the thread wakes the desktop with.
    id: Option<PaneId>,
    tx: Sender<Answer>,
    rx: Receiver<Answer>,
    /// The default output's volume, once read (an image with sound).
    pub(super) volume: Option<Volume>,
}

/// What a sound job found, and whether to say it.
struct Answer {
    result: Result<Volume, String>,
    say: bool,
}

impl Default for Sound {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            id: None,
            tx,
            rx,
            volume: None,
        }
    }
}

impl std::fmt::Debug for Sound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sound")
            .field("volume", &self.volume)
            .finish_non_exhaustive()
    }
}

impl Desktop {
    pub(super) fn is_sound_wake(&self, id: PaneId) -> bool {
        self.sound.id == Some(id)
    }

    /// Runs `job` on a thread; its volume becomes the status item, and
    /// `say` posts it (or why it failed) as a message.
    fn sound_job(
        &mut self,
        say: bool,
        job: impl FnOnce() -> std::io::Result<Volume> + Send + 'static,
    ) {
        let id = match self.sound.id {
            Some(id) => id,
            None => {
                let id = self.alloc_id();
                self.sound.id = Some(id);
                id
            }
        };
        let tx = self.sound.tx.clone();
        let wake = self.pty_tx.clone();
        let spawned = std::thread::Builder::new()
            .name("termos-sound".into())
            .spawn(move || {
                let result = job().map_err(|e| e.to_string());
                let _ = tx.send(Answer { result, say });
                // A full queue already has a wakeup in it.
                let _ = wake.try_send(PtyEvent::Wake(id));
            });
        if let Err(e) = spawned {
            tracing::warn!("cannot start a thread for sound: {e}");
        }
    }

    /// At start, on an image: the output `[sound] device` names, and the
    /// volume for the status line.
    pub(super) fn start_sound(&mut self) {
        let device = self.config.sound.device.clone();
        self.sound_job(false, move || termos_hw::sound::start(&device));
    }

    /// A sound job finished.
    pub(super) fn sound_woke(&mut self) {
        while let Ok(a) = self.sound.rx.try_recv() {
            match a.result {
                Ok(v) => {
                    if a.say {
                        self.post(Level::Info, v.message());
                    }
                    if self.sound.volume != Some(v) {
                        self.sound.volume = Some(v);
                        self.dirty = true;
                    }
                }
                Err(e) if a.say => self.post(Level::Warn, format!("sound: {e}")),
                Err(e) => tracing::info!("no volume in the status line: {e}"),
            }
        }
    }

    /// `sound.volume ±N`, `sound.mute`, `sound.refresh`.
    pub(super) fn sound_command(&mut self, change: SoundChange) {
        if !on_image() {
            if change != SoundChange::Refresh {
                self.post(Level::Info, "the volume is the host system's");
            }
            return;
        }
        match change {
            SoundChange::Volume(d) => {
                self.sound_job(true, move || termos_hw::sound::change_volume(d));
            }
            SoundChange::Mute => self.sound_job(true, termos_hw::sound::toggle_mute),
            SoundChange::Refresh => self.sound_job(false, termos_hw::sound::volume),
        }
    }

    /// The status item: `◁ 45%`.
    pub(super) fn volume_status(&self) -> Option<String> {
        self.sound.volume.map(|v| v.status_text())
    }
}

/// What a sound command changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SoundChange {
    Volume(i8),
    Mute,
    Refresh,
}
