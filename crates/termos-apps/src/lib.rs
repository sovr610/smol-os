// SPDX-License-Identifier: GPL-3.0-or-later
//! # termos-apps
//!
//! Built-in TermOS apps. Each implements the in-process `App` trait and draws
//! with ratatui widgets into its window.
//!
//! - [`image_viewer`]: images from a folder, in pixels where the terminal
//!   can show them (M4a).
//! - [`markdown`]: markdown documents, with their local images.
//! - [`files`]: the file manager, with previews.
//! - [`monitor`]: CPU, memory, GPUs, temperatures, disks, network and
//!   processes.
//! - [`clipboard`]: what was copied, to paste again.
//! - [`settings`]: the common options of `config.toml`.
//!
//! Planned (see `ARCHITECTURE.md`, "Apps and plugins"): the package
//! manager UI (M5).

pub mod clipboard;
pub mod files;
pub mod image_viewer;
pub mod markdown;
pub mod monitor;
pub mod network;
pub mod settings;

pub use clipboard::ClipboardManager;
pub use files::FileManager;
pub use image_viewer::ImageViewer;
pub use markdown::MarkdownViewer;
pub use monitor::SystemMonitor;
pub use network::NetworkPage;
pub use settings::Settings;

/// Drawing apps in tests: the text of an app rendered at a size.
#[cfg(test)]
pub(crate) mod testing {
    use ratatui_core::buffer::Buffer;
    use ratatui_core::layout::Rect;
    use termos_core::app::App;
    use termos_render::theme::Theme;

    pub(crate) fn render_text(app: &mut dyn App, w: u16, h: u16) -> String {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        app.render(area, &mut buf, &Theme::default());
        let mut out = String::new();
        for y in 0..h {
            let mut line = String::new();
            let mut skip = 0;
            for x in 0..w {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                let sym = buf[(x, y)].symbol();
                skip = unicode_width::UnicodeWidthStr::width(sym).saturating_sub(1);
                line.push_str(sym);
            }
            out.push_str(line.trim_end());
            out.push('\n');
        }
        out
    }
}
