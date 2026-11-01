// SPDX-License-Identifier: GPL-3.0-or-later
//! Config hot reload: a debounced file watcher that pokes the server.
//!
//! The parent directory is watched (not the file) so that editors that save
//! by writing a temporary file and renaming it are still noticed. The
//! watcher thread blocks in the OS (inotify/FSEvents), so it costs nothing
//! while idle.

use std::path::{Path, PathBuf};
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_mini::{DebounceEventResult, Debouncer, new_debouncer};
use tokio::sync::mpsc;

/// Keeps the watcher alive; dropping it stops watching.
pub struct ConfigWatcher {
    _debouncer: Debouncer<notify::RecommendedWatcher>,
    path: PathBuf,
}

impl std::fmt::Debug for ConfigWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigWatcher")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl ConfigWatcher {
    /// Watches `file`; sends `()` on `tx` (debounced) whenever it changes.
    /// Creates the parent directory if needed.
    pub fn spawn(file: &Path, tx: mpsc::UnboundedSender<()>) -> Result<Self, notify::Error> {
        let dir = file
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        std::fs::create_dir_all(&dir).map_err(notify::Error::io)?;
        let name = file.file_name().map(ToOwned::to_owned);
        let mut debouncer = new_debouncer(
            Duration::from_millis(150),
            move |res: DebounceEventResult| {
                if let Ok(events) = res
                    && events.iter().any(|e| e.path.file_name() == name.as_deref())
                {
                    // The server may have shut down; nothing to do then.
                    let _ = tx.send(());
                }
            },
        )?;
        debouncer
            .watcher()
            .watch(&dir, RecursiveMode::NonRecursive)?;
        Ok(Self {
            _debouncer: debouncer,
            path: file.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn notices_writes_and_renames() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("config.toml");
        std::fs::write(&file, "mouse = true\n").expect("write");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let _w = ConfigWatcher::spawn(&file, tx).expect("watch");

        std::fs::write(&file, "mouse = false\n").expect("write");
        let got = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await;
        assert!(matches!(got, Ok(Some(()))), "no event after write");

        // Atomic save: write elsewhere, rename over the file.
        let tmp = dir.path().join(".config.toml.swp");
        std::fs::write(&tmp, "mouse = true\n").expect("write");
        std::fs::rename(&tmp, &file).expect("rename");
        let got = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await;
        assert!(matches!(got, Ok(Some(()))), "no event after rename");
    }
}
