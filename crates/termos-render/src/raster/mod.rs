// SPDX-License-Identifier: GPL-3.0-or-later
//! The software rasterizer the KMS client draws with (ARCHITECTURE.md §5.9,
//! D-073): cells of a [`Grid`] into a 32-bit pixel buffer, on the CPU.
//!
//! - Glyphs come from the configured fonts through swash (hinted, with
//!   bold from a bold face or by emboldening, italic by slanting); box
//!   drawing, block elements and braille are drawn from geometry
//!   ([`boxdraw`]) so borders join exactly.
//! - Each glyph is rasterized once per size and style into a cell-sized
//!   coverage mask (the glyph cache) and blended with the cell's colours.
//! - Only the cells the caller names are painted (the damage).

pub mod boxdraw;

use std::collections::HashMap;
use std::sync::Arc;

use compact_str::CompactString;
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::zeno::{Angle, Format, Transform};
use swash::{CacheKey, FontRef};

use crate::cell::{Attrs, Cell};
use crate::color::{Palette, Rgb};
use crate::encode::{CursorShape, CursorState};
use crate::grid::Grid;

pub use boxdraw::Mask;

/// DejaVu Sans Mono, built in so the console always has a font
/// (`fonts/LICENSE`).
pub const BUNDLED_REGULAR: &[u8] = include_bytes!("../../fonts/DejaVuSansMono.ttf");
/// DejaVu Sans Mono Bold.
pub const BUNDLED_BOLD: &[u8] = include_bytes!("../../fonts/DejaVuSansMono-Bold.ttf");

/// The smallest and largest font sizes, in pixels.
pub const MIN_SIZE: f32 = 6.0;
pub const MAX_SIZE: f32 = 96.0;

/// Glyphs kept before the cache starts over (a few screens of distinct
/// characters; a cleared cache refills from what is on screen).
const CACHE_LIMIT: usize = 8192;

/// Why a font could not be used.
#[derive(Debug, thiserror::Error)]
pub enum RasterError {
    #[error("not a font file (TrueType or OpenType)")]
    NotAFont,
    #[error("the font has no outline for `M`, so it cannot set the cell size")]
    NoMetrics,
}

/// One font file's data.
#[derive(Clone)]
pub struct FontData {
    data: Arc<[u8]>,
    offset: u32,
    key: CacheKey,
}

impl std::fmt::Debug for FontData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FontData({} bytes)", self.data.len())
    }
}

impl FontData {
    /// The first face of a TrueType/OpenType file or collection.
    pub fn new(data: impl Into<Arc<[u8]>>) -> Result<Self, RasterError> {
        let data: Arc<[u8]> = data.into();
        let f = FontRef::from_index(&data, 0).ok_or(RasterError::NotAFont)?;
        let (offset, key) = (f.offset, f.key);
        Ok(Self { data, offset, key })
    }

    fn font(&self) -> FontRef<'_> {
        FontRef {
            data: &self.data,
            offset: self.offset,
            key: self.key,
        }
    }

    fn has(&self, c: char) -> bool {
        self.font().charmap().map(c) != 0
    }
}

/// The faces text is drawn with.
#[derive(Debug, Clone)]
pub struct FontSet {
    pub regular: FontData,
    /// `None`: bold is the regular face, emboldened.
    pub bold: Option<FontData>,
    /// Tried in order for characters the main face lacks.
    pub fallbacks: Vec<FontData>,
}

impl FontSet {
    /// The built-in DejaVu Sans Mono.
    pub fn bundled() -> Self {
        Self {
            regular: FontData::new(BUNDLED_REGULAR)
                .expect("the bundled font is a valid TrueType file"),
            bold: Some(
                FontData::new(BUNDLED_BOLD)
                    .expect("the bundled bold font is a valid TrueType file"),
            ),
            fallbacks: Vec::new(),
        }
    }
}

/// The size of a cell and where lines go in it, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellMetrics {
    pub width: u32,
    pub height: u32,
    /// From the top of the cell.
    pub baseline: u32,
    /// The top of the underline, from the top of the cell.
    pub underline: u32,
    /// The top of the strikethrough line.
    pub strike: u32,
    /// Line thickness (underline, strikethrough, light box lines).
    pub stroke: u32,
}

impl CellMetrics {
    /// From a font at `px` pixels per em.
    pub fn of(font: &FontData, px: f32) -> Result<Self, RasterError> {
        let f = font.font();
        let m = f.metrics(&[]).scale(px);
        let gid = f.charmap().map('M');
        if gid == 0 {
            return Err(RasterError::NoMetrics);
        }
        let advance = f.glyph_metrics(&[]).scale(px).advance_width(gid);
        let width = advance.round().max(1.0) as u32;
        let ascent = m.ascent.round().max(1.0);
        let height = (m.ascent + m.descent + m.leading).ceil().max(ascent + 1.0) as u32;
        let baseline = ascent as u32;
        let stroke = m.stroke_size.round().max(1.0) as u32;
        let below = (-m.underline_offset).round().max(1.0) as u32;
        let underline = (baseline + below).min(height.saturating_sub(stroke));
        let x_height = if m.x_height > 0.0 {
            m.x_height
        } else {
            m.ascent / 2.0
        };
        let strike = baseline.saturating_sub((x_height / 2.0).round() as u32);
        Ok(Self {
            width,
            height,
            baseline,
            underline,
            strike,
            stroke,
        })
    }
}

/// A 32-bit `0x00RRGGBB` pixel buffer (DRM's XRGB8888).
pub struct Canvas<'a> {
    pub pixels: &'a mut [u32],
    /// Pixels per row (at least `width`).
    pub stride: usize,
    pub width: u32,
    pub height: u32,
}

impl Canvas<'_> {
    /// Fills a rectangle, clipped to the canvas.
    fn fill(&mut self, x: u32, y: u32, w: u32, h: u32, c: u32) {
        if x >= self.width || y >= self.height {
            return;
        }
        let x1 = (x + w).min(self.width);
        let y1 = (y + h).min(self.height);
        for row in y..y1 {
            let start = row as usize * self.stride;
            for px in &mut self.pixels[start + x as usize..start + x1 as usize] {
                *px = c;
            }
        }
    }

    /// The pixel at `(x, y)` (tests and the screenshot check).
    pub fn at(&self, x: u32, y: u32) -> u32 {
        self.pixels[y as usize * self.stride + x as usize]
    }
}

/// `0x00RRGGBB`.
pub fn pack(c: Rgb) -> u32 {
    (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b)
}

/// Blends `fg` over `bg` by `a` (0–255).
fn mix(bg: Rgb, fg: Rgb, a: u8) -> u32 {
    let m = |b: u8, f: u8| -> u32 {
        let (b, f, a) = (u32::from(b), u32::from(f), u32::from(a));
        (b * (255 - a) + f * a + 127) / 255
    };
    (m(bg.r, fg.r) << 16) | (m(bg.g, fg.g) << 8) | m(bg.b, fg.b)
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct GlyphKey {
    symbol: CompactString,
    bold: bool,
    italic: bool,
    /// Cells wide.
    cells: u8,
}

/// Draws cells into a [`Canvas`].
pub struct Rasterizer {
    fonts: FontSet,
    size: f32,
    metrics: CellMetrics,
    ctx: ScaleContext,
    cache: HashMap<GlyphKey, Arc<Mask>>,
    pub palette: Palette,
}

impl std::fmt::Debug for Rasterizer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Rasterizer")
            .field("size", &self.size)
            .field("metrics", &self.metrics)
            .finish_non_exhaustive()
    }
}

impl Rasterizer {
    /// `size` in pixels per em, clamped to [`MIN_SIZE`]..=[`MAX_SIZE`].
    pub fn new(fonts: FontSet, size: f32, palette: Palette) -> Result<Self, RasterError> {
        let size = size.clamp(MIN_SIZE, MAX_SIZE);
        let metrics = CellMetrics::of(&fonts.regular, size)?;
        Ok(Self {
            fonts,
            size,
            metrics,
            ctx: ScaleContext::new(),
            cache: HashMap::new(),
            palette,
        })
    }

    pub fn metrics(&self) -> CellMetrics {
        self.metrics
    }

    pub fn size(&self) -> f32 {
        self.size
    }

    /// Changes the font size (accessibility); the cell size follows.
    pub fn set_size(&mut self, size: f32) -> Result<(), RasterError> {
        let size = size.clamp(MIN_SIZE, MAX_SIZE);
        self.metrics = CellMetrics::of(&self.fonts.regular, size)?;
        self.size = size;
        self.cache.clear();
        Ok(())
    }

    /// How many cells fit in `width`×`height` pixels, and the margins
    /// that centre them.
    pub fn layout(&self, width: u32, height: u32) -> Layout {
        let cols = (width / self.metrics.width).clamp(1, u32::from(u16::MAX));
        let rows = (height / self.metrics.height).clamp(1, u32::from(u16::MAX));
        Layout {
            cols: cols as u16,
            rows: rows as u16,
            x: width.saturating_sub(cols * self.metrics.width) / 2,
            y: height.saturating_sub(rows * self.metrics.height) / 2,
        }
    }

    /// The coverage mask of one cell's text, `cells` cells wide.
    fn glyph(&mut self, symbol: &str, bold: bool, italic: bool, cells: u8) -> Arc<Mask> {
        let key = GlyphKey {
            symbol: symbol.into(),
            bold,
            italic,
            cells,
        };
        if let Some(m) = self.cache.get(&key) {
            return Arc::clone(m);
        }
        if self.cache.len() >= CACHE_LIMIT {
            self.cache.clear();
        }
        let m = Arc::new(self.rasterize(symbol, bold, italic, cells));
        self.cache.insert(key, Arc::clone(&m));
        m
    }

    fn rasterize(&mut self, symbol: &str, bold: bool, italic: bool, cells: u8) -> Mask {
        let CellMetrics {
            width,
            height,
            baseline,
            stroke,
            ..
        } = self.metrics;
        let w = width * u32::from(cells.max(1));
        let mut chars = symbol.chars();
        if let (Some(c), None) = (chars.next(), chars.next())
            && let Some(m) = boxdraw::draw(c, w, height, if bold { stroke * 2 } else { stroke })
        {
            return m;
        }
        let mut mask = Mask::new(w, height);
        for c in symbol.chars() {
            // The face that has the character: bold, regular, fallbacks.
            let bold_face = self
                .fonts
                .bold
                .as_ref()
                .filter(|_| bold)
                .filter(|f| f.has(c));
            let (face, synthetic_bold) = match bold_face {
                Some(f) => (f.clone(), false),
                None => match std::iter::once(&self.fonts.regular)
                    .chain(&self.fonts.fallbacks)
                    .find(|f| f.has(c))
                {
                    Some(f) => (f.clone(), bold),
                    None => {
                        if !c.is_whitespace() && !is_zero_width(c) {
                            tofu(&mut mask, stroke);
                        }
                        continue;
                    }
                },
            };
            let font = face.font();
            let gid = font.charmap().map(c);
            let mut scaler = self.ctx.builder(font).size(self.size).hint(true).build();
            let mut render = Render::new(&[Source::Outline, Source::Bitmap(StrikeWith::BestFit)]);
            render.format(Format::Alpha);
            if synthetic_bold {
                render.embolden((self.size / 24.0).max(0.5));
            }
            if italic {
                render.transform(Some(Transform::skew(
                    Angle::from_degrees(12.0),
                    Angle::ZERO,
                )));
            }
            let Some(img) = render.render(&mut scaler, gid) else {
                continue;
            };
            let p = img.placement;
            // A glyph wider than its cells (some fonts' wide glyphs) is
            // centred; others keep their own bearing.
            let shift = if cells > 1 && p.width < w {
                (i64::from(w) - i64::from(p.width)) / 2 - i64::from(p.left)
            } else {
                0
            };
            for gy in 0..p.height {
                let y = i64::from(baseline) - i64::from(p.top) + i64::from(gy);
                if y < 0 || y >= i64::from(height) {
                    continue;
                }
                for gx in 0..p.width {
                    let x = i64::from(p.left) + i64::from(gx) + shift;
                    if x < 0 || x >= i64::from(w) {
                        continue;
                    }
                    let a = img.data[(gy * p.width + gx) as usize];
                    let i = y as usize * w as usize + x as usize;
                    mask.alpha[i] = mask.alpha[i].max(a);
                }
            }
        }
        mask
    }

    /// Paints the cell at grid `(x, y)` with its top-left pixel at
    /// `(px, py)`; `cursor` draws the block cursor over it.
    pub fn paint_cell(
        &mut self,
        canvas: &mut Canvas<'_>,
        px: u32,
        py: u32,
        cell: &Cell,
        cursor: bool,
    ) {
        let CellMetrics {
            width,
            height,
            underline,
            strike,
            stroke,
            ..
        } = self.metrics;
        let cells = cell.width.max(1);
        let w = width * u32::from(cells);
        let style = cell.style;
        let mut fg = style.fg.resolve(&self.palette, true);
        let mut bg = style.bg.resolve(&self.palette, false);
        if style.attrs.contains(Attrs::REVERSE) != cursor {
            std::mem::swap(&mut fg, &mut bg);
        }
        if style.attrs.contains(Attrs::DIM) {
            fg = fg.blend(bg, 0.5);
        }
        canvas.fill(px, py, w, height, pack(bg));
        let hidden = style.attrs.contains(Attrs::HIDDEN);
        let blank = cell.symbol.is_empty() || cell.symbol == " " || cell.symbol == "\u{0}";
        if !hidden && !blank {
            let mask = self.glyph(
                &cell.symbol,
                style.attrs.contains(Attrs::BOLD),
                style.attrs.contains(Attrs::ITALIC),
                cells,
            );
            for my in 0..mask.height.min(canvas.height.saturating_sub(py)) {
                let row = (py + my) as usize * canvas.stride;
                for mx in 0..mask.width.min(canvas.width.saturating_sub(px)) {
                    let a = mask.alpha[(my * mask.width + mx) as usize];
                    if a > 0 {
                        canvas.pixels[row + (px + mx) as usize] = mix(bg, fg, a);
                    }
                }
            }
        }
        if hidden {
            return;
        }
        let ul = match style.ul {
            crate::color::Color::Default => fg,
            c => c.resolve(&self.palette, true),
        };
        let line =
            |canvas: &mut Canvas<'_>, y: u32, c: Rgb| canvas.fill(px, py + y, w, stroke, pack(c));
        let a = style.attrs;
        if a.contains(Attrs::UNDERLINE) {
            line(canvas, underline, ul);
        }
        if a.contains(Attrs::DOUBLE_UNDERLINE) {
            let top = underline.saturating_sub(stroke * 2);
            line(canvas, top, ul);
            line(canvas, underline, ul);
        }
        if a.intersects(Attrs::DOTTED_UNDERLINE | Attrs::DASHED_UNDERLINE) {
            let (on, period) = if a.contains(Attrs::DOTTED_UNDERLINE) {
                (stroke, stroke * 2)
            } else {
                (width / 2, width)
            };
            let mut x = 0;
            while x < w {
                canvas.fill(px + x, py + underline, on.min(w - x), stroke, pack(ul));
                x += period.max(1);
            }
        }
        if a.contains(Attrs::CURLY_UNDERLINE) {
            // One wave per cell, between the underline and two strokes above.
            let amp = (stroke * 2).max(2) as f32 / 2.0;
            let mid = underline as f32 - amp + stroke as f32 / 2.0;
            for x in 0..w {
                let phase = (x as f32 / width as f32) * std::f32::consts::TAU;
                let y = (mid + amp * phase.sin()).round().max(0.0) as u32;
                canvas.fill(px + x, py + y.min(height - 1), 1, stroke, pack(ul));
            }
        }
        if a.contains(Attrs::STRIKE) {
            line(canvas, strike, fg);
        }
    }

    /// Paints `count` cells of row `y` from column `x` (a span of damage),
    /// widened to whole wide characters.
    pub fn paint_cells(
        &mut self,
        canvas: &mut Canvas<'_>,
        layout: Layout,
        grid: &Grid,
        x: u16,
        y: u16,
        count: u16,
    ) {
        if y >= grid.rows() {
            return;
        }
        let row = grid.row(y);
        let mut col = x;
        // Start on the leading half of a wide character.
        while col > 0 && row.get(usize::from(col)).is_some_and(Cell::is_continuation) {
            col -= 1;
        }
        let end = x.saturating_add(count).min(grid.cols());
        while col < end {
            let Some(cell) = row.get(usize::from(col)) else {
                break;
            };
            let (px, py) = layout.pixel(&self.metrics, col, y);
            if cell.is_continuation() {
                // An orphaned trailing half: blank in its own colours.
                self.paint_cell(canvas, px, py, &Cell::blank(cell.style), false);
                col += 1;
                continue;
            }
            self.paint_cell(canvas, px, py, cell, false);
            col += u16::from(cell.width.max(1));
        }
    }

    /// Paints the whole grid and the margins around it.
    pub fn paint_all(&mut self, canvas: &mut Canvas<'_>, layout: Layout, grid: &Grid) {
        let bg = pack(self.palette.bg);
        let (w, h) = (canvas.width, canvas.height);
        canvas.fill(0, 0, w, h, bg);
        for y in 0..grid.rows() {
            self.paint_cells(canvas, layout, grid, 0, y, grid.cols());
        }
    }

    /// Draws the cursor; repainting its cell (without `cursor`) removes it.
    pub fn paint_cursor(
        &mut self,
        canvas: &mut Canvas<'_>,
        layout: Layout,
        grid: &Grid,
        cursor: CursorState,
    ) {
        let Some(cell) = grid.get(cursor.x, cursor.y) else {
            return;
        };
        let (px, py) = layout.pixel(&self.metrics, cursor.x, cursor.y);
        let CellMetrics {
            width,
            height,
            stroke,
            ..
        } = self.metrics;
        let fg = cell.style.fg.resolve(&self.palette, true);
        let thick = (stroke * 2).max(2);
        match cursor.shape {
            CursorShape::Block => self.paint_cell(canvas, px, py, cell, true),
            CursorShape::Bar => canvas.fill(px, py, thick.min(width), height, pack(fg)),
            CursorShape::Underline => {
                canvas.fill(px, py + height - thick.min(height), width, thick, pack(fg))
            }
        }
    }
}

/// Where the grid sits on the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub cols: u16,
    pub rows: u16,
    /// The grid's top-left pixel.
    pub x: u32,
    pub y: u32,
}

impl Layout {
    /// The top-left pixel of cell `(col, row)`.
    pub fn pixel(&self, m: &CellMetrics, col: u16, row: u16) -> (u32, u32) {
        (
            self.x + u32::from(col) * m.width,
            self.y + u32::from(row) * m.height,
        )
    }

    /// The cell at pixel `(x, y)`, clamped to the grid.
    pub fn cell_at(&self, m: &CellMetrics, x: u32, y: u32) -> (u16, u16) {
        let col = (x.saturating_sub(self.x) / m.width).min(u32::from(self.cols.saturating_sub(1)));
        let row = (y.saturating_sub(self.y) / m.height).min(u32::from(self.rows.saturating_sub(1)));
        (col as u16, row as u16)
    }
}

fn is_zero_width(c: char) -> bool {
    matches!(c, '\u{200b}'..='\u{200f}' | '\u{fe00}'..='\u{fe0f}' | '\u{0300}'..='\u{036f}')
}

/// A hollow box for a character no font has.
fn tofu(m: &mut Mask, stroke: u32) {
    let (w, h) = (m.width, m.height);
    if w < 4 || h < 4 {
        return;
    }
    let (x0, y0, x1, y1) = (1, h / 6, w - 1, h - h / 6);
    for y in y0..y1 {
        for x in x0..x1 {
            let edge = x < x0 + stroke || x >= x1 - stroke || y < y0 + stroke || y >= y1 - stroke;
            if edge {
                m.alpha[(y * w + x) as usize] = 255;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::Style;
    use crate::color::{Color, XTERM_ANSI};

    fn palette() -> Palette {
        Palette {
            fg: Rgb::new(200, 200, 200),
            bg: Rgb::new(10, 10, 10),
            ansi: XTERM_ANSI,
        }
    }

    fn raster(px: f32) -> Rasterizer {
        Rasterizer::new(FontSet::bundled(), px, palette()).expect("bundled font")
    }

    fn art(m: &Mask) -> String {
        let mut s = String::new();
        for y in 0..m.height {
            for x in 0..m.width {
                s.push(match m.at(x, y) {
                    0 => '.',
                    1..=127 => '+',
                    _ => '#',
                });
            }
            s.push('\n');
        }
        s
    }

    #[test]
    fn metrics_follow_the_size() {
        let r = raster(16.0);
        let m = r.metrics();
        // DejaVu Sans Mono: advance 0.602 em, ascent + descent 1.164 em.
        assert_eq!((m.width, m.height), (10, 19), "{m:?}");
        assert!(
            m.baseline < m.height && m.underline > m.baseline && m.underline < m.height,
            "{m:?}"
        );
        assert!(m.strike < m.baseline);
        let big = raster(32.0).metrics();
        assert_eq!((big.width, big.height), (19, 38), "{big:?}");
        let l = r.layout(1024, 768);
        assert_eq!((l.cols, l.rows), (102, 40));
        assert_eq!((l.x, l.y), (2, 4), "centred");
        assert_eq!(l.cell_at(&m, 2 + 25, 4 + 40), (2, 2));
        assert_eq!(l.cell_at(&m, 5000, 5000), (101, 39), "clamped");
    }

    #[test]
    fn letters_bold_italic_and_missing() {
        let mut r = raster(14.0);
        let a = r.glyph("A", false, false, 1);
        insta::assert_snapshot!(art(&a));
        let bold = r.glyph("A", true, false, 1);
        assert!(bold.ink() > a.ink(), "bold has more ink");
        // Slanted: the top of `|` sits further right than its bottom.
        let l = r.glyph("|", false, false, 1);
        let li = r.glyph("|", false, true, 1);
        let centre = |m: &Mask, y: u32| {
            let xs: Vec<u32> = (0..m.width).filter(|x| m.at(*x, y) > 127).collect();
            xs.iter().sum::<u32>() as f32 / xs.len().max(1) as f32
        };
        let (top, bottom) = (3, r.metrics().baseline - 1);
        assert!((centre(&l, top) - centre(&l, bottom)).abs() < 1.0);
        assert!(centre(&li, top) > centre(&li, bottom) + 1.0, "{}", art(&li));
        // A CJK character DejaVu does not have: a box, two cells wide.
        let tofu = r.glyph("中", false, false, 2);
        assert_eq!(tofu.width, r.metrics().width * 2);
        assert!(tofu.at(1, r.metrics().height / 2) == 255);
        // Box drawing comes from geometry, bold drawn thicker.
        let h = r.glyph("─", false, false, 1);
        let hb = r.glyph("─", true, false, 1);
        assert!(hb.ink() > h.ink());
        assert!((0..h.height).any(|y| h.at(0, y) == 255 && h.at(h.width - 1, y) == 255));
    }

    #[test]
    fn cells_paint_colours_attributes_and_cursor() {
        let mut r = raster(16.0);
        let m = r.metrics();
        let mut grid = Grid::new(4, 2);
        let red = Style::new().fg(Color::Indexed(1)).bg(Color::rgb(0, 0, 255));
        grid.set(0, 0, Cell::new('█', red));
        grid.set(1, 0, Cell::new('x', red.attrs(Attrs::REVERSE)));
        grid.set(
            2,
            0,
            Cell::new('_', Style::new().attrs(Attrs::UNDERLINE | Attrs::STRIKE)),
        );
        let layout = r.layout(m.width * 4, m.height * 2);
        let mut px = vec![0u32; (m.width * 4 * m.height * 2) as usize];
        let mut c = Canvas {
            pixels: &mut px,
            stride: (m.width * 4) as usize,
            width: m.width * 4,
            height: m.height * 2,
        };
        r.paint_all(&mut c, layout, &grid);
        // A full block is all foreground (ANSI red).
        assert_eq!(c.at(3, 3), pack(XTERM_ANSI[1]));
        // Reversed: the background is red, the glyph blue.
        assert_eq!(c.at(m.width, 0), pack(XTERM_ANSI[1]));
        // Underline and strikethrough rows are drawn across the cell.
        let x2 = 2 * m.width;
        assert_eq!(c.at(x2 + 1, m.underline), pack(palette().fg));
        assert_eq!(c.at(x2 + m.width - 1, m.strike), pack(palette().fg));
        // Empty cells and the margin are the default background.
        assert_eq!(c.at(3 * m.width + 2, m.height + 2), pack(palette().bg));
        // The block cursor reverses its cell; repainting removes it.
        let cur = CursorState {
            x: 3,
            y: 1,
            shape: CursorShape::Block,
            blinking: false,
        };
        r.paint_cursor(&mut c, layout, &grid, cur);
        assert_eq!(c.at(3 * m.width + 2, m.height + 2), pack(palette().fg));
        r.paint_cells(&mut c, layout, &grid, 3, 1, 1);
        assert_eq!(c.at(3 * m.width + 2, m.height + 2), pack(palette().bg));
        // A bar cursor is a thin line at the cell's left.
        r.paint_cursor(
            &mut c,
            layout,
            &grid,
            CursorState {
                shape: CursorShape::Bar,
                ..cur
            },
        );
        assert_eq!(c.at(3 * m.width, m.height + 2), pack(palette().fg));
        assert_eq!(c.at(3 * m.width + 5, m.height + 2), pack(palette().bg));
    }

    #[test]
    fn damage_on_a_wide_character_repaints_all_of_it() {
        let mut r = raster(16.0);
        let m = r.metrics();
        let mut grid = Grid::new(4, 1);
        let wide = Style::new().bg(Color::rgb(0, 128, 0));
        grid.put_str(0, 0, "中", wide, 4);
        let layout = r.layout(m.width * 4, m.height);
        let mut px = vec![0u32; (m.width * 4 * m.height) as usize];
        let mut c = Canvas {
            pixels: &mut px,
            stride: (m.width * 4) as usize,
            width: m.width * 4,
            height: m.height,
        };
        // Damage reported on the trailing half only.
        r.paint_cells(&mut c, layout, &grid, 1, 0, 1);
        let green = pack(Rgb::new(0, 128, 0));
        assert_eq!(c.at(0, 0), green, "the leading half was painted too");
        assert_eq!(c.at(m.width * 2 - 1, 0), green);
        assert_eq!(c.at(m.width * 2 + 1, 0), 0, "nothing beyond it");
    }

    /// `TERMOS_RASTER_PNG=out.png cargo test -p termos-render --features
    /// raster sample_screen`: a screen to look at (borders, colours,
    /// attributes, wide characters, the cursor).
    #[test]
    fn sample_screen() {
        let Some(path) = std::env::var_os("TERMOS_RASTER_PNG") else {
            return;
        };
        let mut r = raster(16.0);
        let m = r.metrics();
        let mut grid = Grid::new(48, 12);
        let border = Style::new().fg(Color::Indexed(6));
        grid.draw_box(
            crate::grid::Rect::new(0, 0, 48, 11),
            crate::grid::BorderKind::Rounded,
            border,
            Some((" shell ", Style::new().bold())),
        );
        grid.put_str(2, 2, "$ ls --color", Style::new(), 46);
        grid.put_str(2, 3, "Cargo.toml", Style::new(), 46);
        grid.put_str(
            14,
            3,
            "crates/",
            Style::new().fg(Color::Indexed(4)).bold(),
            46,
        );
        grid.put_str(
            23,
            3,
            "docs/",
            Style::new().fg(Color::Indexed(4)).bold(),
            46,
        );
        grid.put_str(2, 4, "italic", Style::new().attrs(Attrs::ITALIC), 46);
        grid.put_str(10, 4, "underline", Style::new().attrs(Attrs::UNDERLINE), 46);
        grid.put_str(
            21,
            4,
            "curly",
            Style::new()
                .attrs(Attrs::CURLY_UNDERLINE)
                .fg(Color::Indexed(1)),
            46,
        );
        grid.put_str(28, 4, "strike", Style::new().attrs(Attrs::STRIKE), 46);
        grid.put_str(36, 4, "dim", Style::new().attrs(Attrs::DIM), 46);
        grid.put_str(2, 5, "wide: 中文 tofu, emoji-free ✓ ±→", Style::new(), 46);
        grid.put_str(
            2,
            6,
            "▁▂▃▄▅▆▇█ ░▒▓ ▖▗▘▙▚▛▜▝▞▟ ⣿⠿⠛",
            Style::new().fg(Color::Indexed(2)),
            46,
        );
        grid.put_str(2, 7, "┌─┬─┐ ┏━┳━┓ ╔═╦═╗ ╭─╮", Style::new(), 46);
        grid.put_str(2, 8, "├─┼─┤ ┣━╋━┫ ╠═╬═╣ ╰─╯", Style::new(), 46);
        grid.put_str(2, 9, "└─┴─┘ ┗━┻━┛ ╚═╩═╝ ╱╲╳", Style::new(), 46);
        grid.put_str(0, 11, " 1:main  2:dev ", Style::new().reversed(), 48);
        let (w, h) = (m.width * 48 + 8, m.height * 12 + 8);
        let layout = r.layout(w, h);
        let mut px = vec![0u32; (w * h) as usize];
        let mut c = Canvas {
            pixels: &mut px,
            stride: w as usize,
            width: w,
            height: h,
        };
        r.paint_all(&mut c, layout, &grid);
        r.paint_cursor(
            &mut c,
            layout,
            &grid,
            CursorState {
                x: 15,
                y: 2,
                shape: CursorShape::Block,
                blinking: false,
            },
        );
        let mut img = image::RgbImage::new(w, h);
        for (i, p) in px.iter().enumerate() {
            let (x, y) = (i as u32 % w, i as u32 / w);
            img.put_pixel(
                x,
                y,
                image::Rgb([(p >> 16) as u8, (p >> 8) as u8, *p as u8]),
            );
        }
        img.save(path).expect("save the PNG");
    }

    #[test]
    fn cells_beyond_the_canvas_are_clipped() {
        let mut r = raster(16.0);
        let mut grid = Grid::new(10, 4);
        grid.put_str(
            0,
            0,
            "abcdefghij",
            Style::new().attrs(Attrs::UNDERLINE | Attrs::CURLY_UNDERLINE),
            10,
        );
        // Room for 3×1 cells and a bit: the rest falls outside.
        let (w, h) = (35, 25);
        let mut px = vec![0u32; (w * h) as usize];
        let mut c = Canvas {
            pixels: &mut px,
            stride: w as usize,
            width: w,
            height: h,
        };
        let layout = Layout {
            cols: 10,
            rows: 4,
            x: 0,
            y: 0,
        };
        r.paint_all(&mut c, layout, &grid);
        r.paint_cursor(
            &mut c,
            layout,
            &grid,
            CursorState {
                x: 9,
                y: 3,
                shape: CursorShape::Underline,
                blinking: false,
            },
        );
    }

    #[test]
    fn sizes_change_and_clamp() {
        let mut r = raster(16.0);
        let w = r.metrics().width;
        r.set_size(24.0).expect("size");
        assert!(r.metrics().width > w);
        r.set_size(1000.0).expect("size");
        assert_eq!(r.size(), MAX_SIZE);
        assert!(FontData::new(&b"not a font"[..]).is_err());
    }
}
