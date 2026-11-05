// SPDX-License-Identifier: GPL-3.0-or-later
//! Which displays the console drives, how, and where each one is (M13e,
//! D-127, D-128).
//!
//! The console plans its outputs from the connectors the card reports and
//! `[[kms.display]]`: every connected display that is not turned off, in
//! the configured mode (else its preferred one), the primary first, then
//! the configured ones in their order, then the rest. They sit side by side,
//! left to right, and the pointer moves across them. This module is that
//! planning and arithmetic; `display.rs` talks to the card.

use termos_core::config::{DisplayConfig, kms::parse_mode};

/// One mode a connector offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ModeInfo {
    pub(super) width: u16,
    pub(super) height: u16,
    pub(super) refresh: u32,
    pub(super) preferred: bool,
}

/// A connector as the card reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Connector {
    /// `HDMI-A-1`, as the kernel names it.
    pub(super) name: String,
    pub(super) connected: bool,
    pub(super) modes: Vec<ModeInfo>,
}

/// A display to drive.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Want {
    pub(super) name: String,
    /// Which of the connector's modes.
    pub(super) mode: usize,
    /// Text size relative to the density (none: as measured).
    pub(super) scale: Option<f32>,
}

/// The mode `spec` names among `modes` (`preferred`, `WxH` at the highest
/// refresh, or `WxH@HZ`), else the preferred one, else the first.
pub(super) fn pick_mode(modes: &[ModeInfo], spec: &str) -> Option<usize> {
    if modes.is_empty() {
        return None;
    }
    let preferred = || modes.iter().position(|m| m.preferred).unwrap_or(0);
    let Some((w, h, hz)) = parse_mode(spec) else {
        return Some(preferred());
    };
    let fits = modes
        .iter()
        .enumerate()
        .filter(|(_, m)| m.width == w && m.height == h)
        .filter(|(_, m)| hz.is_none_or(|hz| m.refresh == hz));
    match fits.max_by_key(|(_, m)| m.refresh) {
        Some((i, _)) => Some(i),
        None => {
            tracing::warn!("no mode {spec}: using the display's preferred one");
            Some(preferred())
        }
    }
}

/// The displays to drive, in order (the primary first).
pub(super) fn plan(connectors: &[Connector], configs: &[DisplayConfig]) -> Vec<Want> {
    let usable: Vec<&Connector> = connectors
        .iter()
        .filter(|c| c.connected && !c.modes.is_empty())
        .collect();
    let config = |name: &str| configs.iter().find(|d| d.name == name);
    let rank = |c: &Connector| -> (u8, usize) {
        match configs.iter().position(|d| d.name == c.name) {
            Some(i) if configs[i].primary => (0, i),
            Some(i) => (1, i),
            None => (2, 0),
        }
    };
    let mut on: Vec<&Connector> = usable
        .iter()
        .copied()
        .filter(|c| config(&c.name).is_none_or(|d| d.enabled))
        .collect();
    // Every display turned off: the first still shows, or nothing would.
    if on.is_empty()
        && let Some(first) = usable.first()
    {
        tracing::warn!(
            "every display is turned off in [[kms.display]]: {} stays on",
            first.name
        );
        on.push(first);
    }
    // A stable sort keeps the card's order among equals.
    on.sort_by_key(|c| rank(c));
    on.into_iter()
        .filter_map(|c| {
            let d = config(&c.name);
            let mode = pick_mode(&c.modes, d.map_or("preferred", |d| d.mode.as_str()))?;
            Some(Want {
                name: c.name.clone(),
                mode,
                scale: d.and_then(|d| d.scale).filter(|s| *s > 0.0),
            })
        })
        .collect()
}

/// The text size for a display: the configured size (pixels at 96 dpi)
/// scaled for its density, in quarter steps from 1× to 3×, times its own
/// scale.
pub(super) fn text_size(
    px: f32,
    width: u32,
    size_mm: Option<(u32, u32)>,
    scale: Option<f32>,
) -> f32 {
    let density = match size_mm {
        Some((mm, _)) if mm > 0 => {
            let dpi = width as f32 / (mm as f32 / 25.4);
            (((dpi / 96.0) * 4.0).round() / 4.0).clamp(1.0, 3.0)
        }
        _ => 1.0,
    };
    (px * density * scale.unwrap_or(1.0)).clamp(6.0, 96.0)
}

/// Where each display starts, left to right, for the widths given.
pub(super) fn offsets(widths: &[u32]) -> Vec<u32> {
    let mut x = 0u32;
    widths
        .iter()
        .map(|w| {
            let at = x;
            x = x.saturating_add(*w);
            at
        })
        .collect()
}

/// The display under the pointer at `x` across all of them (sizes in
/// order), and the pointer's place on it; the pointer stays within the
/// displays' span and each display's height.
pub(super) fn locate(sizes: &[(u32, u32)], x: f64, y: f64) -> Option<(usize, f64, f64)> {
    if sizes.is_empty() {
        return None;
    }
    let total: u32 = sizes.iter().map(|(w, _)| *w).sum();
    let x = x.clamp(0.0, f64::from(total.saturating_sub(1)));
    let starts = offsets(&sizes.iter().map(|(w, _)| *w).collect::<Vec<_>>());
    let i = starts.iter().rposition(|s| f64::from(*s) <= x).unwrap_or(0);
    let (_, h) = sizes[i];
    let y = y.clamp(0.0, f64::from(h.saturating_sub(1)));
    Some((i, x - f64::from(starts[i]), y))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(width: u16, height: u16, refresh: u32, preferred: bool) -> ModeInfo {
        ModeInfo {
            width,
            height,
            refresh,
            preferred,
        }
    }

    fn connector(name: &str, connected: bool) -> Connector {
        Connector {
            name: name.into(),
            connected,
            modes: vec![
                mode(1920, 1080, 60, true),
                mode(1920, 1080, 144, false),
                mode(1280, 720, 60, false),
            ],
        }
    }

    fn display(name: &str) -> DisplayConfig {
        DisplayConfig {
            name: name.into(),
            mode: "preferred".into(),
            scale: None,
            enabled: true,
            primary: false,
        }
    }

    #[test]
    fn modes_are_picked_by_name() {
        let m = connector("x", true).modes;
        assert_eq!(pick_mode(&m, "preferred"), Some(0));
        assert_eq!(
            pick_mode(&m, "1920x1080"),
            Some(1),
            "the fastest of that size"
        );
        assert_eq!(pick_mode(&m, "1920x1080@60"), Some(0));
        assert_eq!(pick_mode(&m, "1280x720"), Some(2));
        assert_eq!(pick_mode(&m, "640x480"), Some(0), "unknown: preferred");
        assert_eq!(pick_mode(&[], "preferred"), None);
    }

    #[test]
    fn displays_are_planned_from_the_connectors_and_config() {
        let cards = [
            connector("eDP-1", true),
            connector("HDMI-A-1", true),
            connector("DP-1", false),
            connector("DP-2", true),
        ];
        // No config: every connected one, in the card's order.
        let names = |w: Vec<Want>| w.into_iter().map(|w| w.name).collect::<Vec<_>>();
        assert_eq!(names(plan(&cards, &[])), ["eDP-1", "HDMI-A-1", "DP-2"]);
        // The primary first, then the configured ones, then the rest;
        // one turned off.
        let mut hdmi = display("HDMI-A-1");
        hdmi.primary = true;
        hdmi.mode = "1280x720".into();
        let mut edp = display("eDP-1");
        edp.enabled = false;
        let mut dp2 = display("DP-2");
        dp2.scale = Some(1.5);
        let wants = plan(&cards, &[edp.clone(), dp2, hdmi]);
        assert_eq!(names(wants.clone()), ["HDMI-A-1", "DP-2"]);
        assert_eq!(wants[0].mode, 2);
        assert_eq!(wants[1].scale, Some(1.5));
        // All turned off: the first stays on.
        let mut off = display("HDMI-A-1");
        off.enabled = false;
        let mut off2 = display("DP-2");
        off2.enabled = false;
        assert_eq!(names(plan(&cards, &[edp, off, off2])), ["eDP-1"]);
        assert!(plan(&[connector("DP-1", false)], &[]).is_empty());
    }

    #[test]
    fn text_grows_with_density_and_scale() {
        assert_eq!(text_size(16.0, 1920, Some((527, 296)), None), 16.0);
        assert_eq!(text_size(16.0, 2560, Some((286, 179)), None), 36.0);
        assert_eq!(text_size(16.0, 1920, None, Some(1.5)), 24.0);
        assert_eq!(
            text_size(80.0, 3840, Some((300, 170)), Some(2.0)),
            96.0,
            "clamped"
        );
    }

    #[test]
    fn the_pointer_moves_across_displays() {
        let sizes = [(1920, 1080), (1280, 1024)];
        assert_eq!(offsets(&[1920, 1280]), [0, 1920]);
        assert_eq!(locate(&sizes, 100.0, 50.0), Some((0, 100.0, 50.0)));
        assert_eq!(locate(&sizes, 1920.0, 50.0), Some((1, 0.0, 50.0)));
        assert_eq!(
            locate(&sizes, 2500.0, 1060.0),
            Some((1, 580.0, 1023.0)),
            "its own height"
        );
        assert_eq!(locate(&sizes, -10.0, -5.0), Some((0, 0.0, 0.0)));
        assert_eq!(locate(&sizes, 9999.0, 0.0), Some((1, 1279.0, 0.0)));
        assert_eq!(locate(&[], 0.0, 0.0), None);
    }
}
