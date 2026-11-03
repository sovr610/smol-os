// SPDX-License-Identifier: GPL-3.0-or-later
//! The which-key overlay: after a pause following the prefix (or with
//! prefix `?`), a panel lists every key and what it does.

use termos_render::grid::{Grid, Rect};
use termos_render::theme::Theme;

use crate::commands::Command;
use crate::keys::KeyTables;
use crate::ui::{Chrome, panel, truncate, width};

/// `(key, description)` rows for the prefix table, with the digit and
/// layout-preset runs collapsed.
pub fn entries(keys: &KeyTables) -> Vec<(String, String)> {
    commands(keys).into_iter().map(|(k, d, _)| (k, d)).collect()
}

/// The entries with the command each runs (`None` for collapsed groups).
fn commands(keys: &KeyTables) -> Vec<(String, String, Option<Command>)> {
    let mut out = Vec::new();
    let mut saw_digits = false;
    let mut saw_presets = false;
    for b in &keys.prefix {
        match &b.command {
            Command::WorkspaceSelect(n) if *n <= 9 && b.key.to_string() == n.to_string() => {
                if !saw_digits {
                    out.push(("0-9".into(), "Go to workspace N".into(), None));
                    saw_digits = true;
                }
            }
            Command::LayoutSelect(_) if b.key.to_string().starts_with("M-") => {
                if !saw_presets {
                    out.push(("M-1..5".into(), "Layout presets".into(), None));
                    saw_presets = true;
                }
            }
            Command::SendPrefix => out.push((
                b.key.to_string(),
                "Send the prefix to the pane".into(),
                Some(Command::SendPrefix),
            )),
            c => out.push((b.key.to_string(), c.description(), Some(c.clone()))),
        }
    }
    out
}

/// Draws the overlay at the bottom of `area`. `full` adds the root
/// bindings and mouse help (prefix `?`).
/// Draws the key hints; returns where, if anything was drawn.
pub fn draw(
    grid: &mut Grid,
    area: Rect,
    theme: &Theme,
    keys: &KeyTables,
    full: bool,
    chrome: Chrome,
) -> (Option<Rect>, Vec<(Rect, Command)>) {
    let mut items = commands(keys);
    if full {
        for b in &keys.root {
            items.push((
                format!("{} (no prefix)", b.key),
                b.command.description(),
                Some(b.command.clone()),
            ));
        }
        items.push((
            "mouse".into(),
            "click focus · drag border resize".into(),
            None,
        ));
        items.push((
            "mouse".into(),
            "drag title move · wheel scroll".into(),
            None,
        ));
        items.push((
            "mouse".into(),
            "right-click: menus · ◆ Menu: everything".into(),
            None,
        ));
    }
    let mut regions = Vec::new();
    if items.is_empty() || area.w < 20 || area.h < 4 {
        return (None, regions);
    }
    let key_w = items
        .iter()
        .map(|(k, _, _)| width(k))
        .max()
        .unwrap_or(1)
        .min(14);
    let col_w: u16 = (key_w + 2 + 28).min(area.w.saturating_sub(4));
    let inner_w = area.w.saturating_sub(4);
    let ncols = (inner_w / (col_w + 2)).max(1);
    let n = u16::try_from(items.len()).unwrap_or(u16::MAX);
    let nrows = n.div_ceil(ncols).min(area.h.saturating_sub(3));
    let h = nrows + 2;
    let rect = Rect::new(
        area.x + 1,
        area.bottom().saturating_sub(h),
        area.w.saturating_sub(2),
        h,
    );
    let title = if full {
        format!("Key bindings — prefix {}", keys.prefix_key)
    } else {
        format!("{} …", keys.prefix_key)
    };
    let inner = panel(grid, rect, area, theme, &title, chrome);
    for (i, (k, d, cmd)) in items.iter().enumerate() {
        let Ok(i) = u16::try_from(i) else { break };
        let (col, row) = (i / nrows.max(1), i % nrows.max(1));
        if col >= ncols {
            break;
        }
        let x = inner.x + 1 + col * (col_w + 2);
        let y = inner.y + row;
        let end = x + col_w;
        grid.put_str(x, y, &truncate(k, key_w), theme.overlay_key, end);
        grid.put_str(
            x + key_w + 2,
            y,
            &truncate(d, col_w.saturating_sub(key_w + 2)),
            theme.overlay,
            end,
        );
        if let Some(c) = cmd {
            regions.push((Rect::new(x, y, col_w, 1), c.clone()));
        }
    }
    (Some(rect), regions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use termos_core::config::Config;

    #[test]
    fn collapses_digits_and_presets() {
        let (keys, _) = KeyTables::from_config(&Config::parse("", Path::new("c")).expect("valid"));
        let e = entries(&keys);
        assert_eq!(e.iter().filter(|(k, _)| k == "0-9").count(), 1);
        assert_eq!(e.iter().filter(|(k, _)| k == "M-1..5").count(), 1);
        assert!(e.iter().any(|(k, d)| k == "%" && d.contains("right")));
        assert!(!e.iter().any(|(k, _)| k == "5"));
    }

    #[test]
    fn draws_inside_the_area() {
        let (keys, _) = KeyTables::from_config(&Config::default());
        let mut g = Grid::new(100, 30);
        let (_, regions) = draw(
            &mut g,
            Rect::new(0, 0, 100, 29),
            &Theme::default(),
            &keys,
            false,
            Chrome::default(),
        );
        // Each command can be clicked where it is drawn.
        let split = regions
            .iter()
            .find(|(_, c)| *c == Command::Split(termos_wm::workspace::SplitHow::Right))
            .expect("split right is listed");
        assert!(
            g.row_text(split.0.y)
                .contains("Split: new pane to the right")
        );
        let text = g.to_text();
        assert!(text.contains("C-b …"));
        assert!(text.contains("Split: new pane to the right"));
        assert_eq!(g.row_text(29).trim(), "", "status row untouched");
    }
}
