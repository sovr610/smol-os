// SPDX-License-Identifier: GPL-3.0-or-later
//! The markdown viewer: headings, emphasis, lists and task lists, quotes,
//! code, tables, links and local images, wrapped to the window.
//!
//! The text is parsed once into blocks ([`parse`]); blocks are laid out
//! into rows for a width ([`layout`]) and drawn with the theme. Images
//! next to the file are decoded on a thread and shown with the desktop's
//! image ladder; images on the web are not fetched.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::sync::{Arc, Mutex};

use pulldown_cmark::{Alignment, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect as TuiRect;
use termos_core::app::{App, AppEffect, AppImage, AppWaker, new_image_key, tui_style};
use termos_core::input::{KeyCode, KeyEvent, MouseEvent, MouseKind};
use termos_render::cell::{Attrs, Style};
use termos_render::graphics::Picture;
use termos_render::grid::Rect;
use termos_render::theme::Theme;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Markdown files bigger than this are not opened.
pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// Images bigger than this are not decoded.
const MAX_IMAGE_BYTES: u64 = 16 * 1024 * 1024;
/// The tallest an image gets, in rows.
const MAX_IMAGE_ROWS: u16 = 24;

/// How a run of text looks; mapped to theme colours when drawn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Look {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    pub link: bool,
    pub muted: bool,
    /// 1 to 6 inside a heading.
    pub heading: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub look: Look,
}

impl Span {
    fn new(text: impl Into<String>, look: Look) -> Self {
        Self {
            text: text.into(),
            look,
        }
    }

    fn width(&self) -> usize {
        UnicodeWidthStr::width(self.text.as_str())
    }
}

/// A hard line break inside a paragraph.
const BREAK: &str = "\n";

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    /// A paragraph or heading: `first` starts its first row, `rest` the
    /// others (list markers and quote bars).
    Text {
        first: Vec<Span>,
        rest: Vec<Span>,
        spans: Vec<Span>,
    },
    Code {
        first: Vec<Span>,
        rest: Vec<Span>,
        lines: Vec<String>,
    },
    Rule {
        indent: Vec<Span>,
    },
    Table {
        indent: Vec<Span>,
        align: Vec<Alignment>,
        header: Vec<Vec<Span>>,
        rows: Vec<Vec<Vec<Span>>>,
    },
    /// A picture on its own line: `index` into [`Doc::images`].
    Image {
        indent: Vec<Span>,
        index: usize,
    },
    Blank,
}

/// An image the document shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRef {
    pub src: String,
    pub alt: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Doc {
    pub blocks: Vec<Block>,
    pub images: Vec<ImageRef>,
}

enum Container {
    Quote,
    /// A list item: its marker, and whether the marker was drawn yet.
    Item {
        marker: String,
        drawn: bool,
    },
}

struct TableBuild {
    align: Vec<Alignment>,
    header: Vec<Vec<Span>>,
    rows: Vec<Vec<Vec<Span>>>,
    row: Vec<Vec<Span>>,
}

/// The prefixes for a new block: (first row, other rows). Drawing the
/// first row of a list item uses up its marker.
fn prefixes(containers: &mut [Container]) -> (Vec<Span>, Vec<Span>) {
    let bar = Look {
        muted: true,
        ..Look::default()
    };
    let (mut first, mut rest) = (Vec::new(), Vec::new());
    for c in containers.iter_mut() {
        match c {
            Container::Quote => {
                first.push(Span::new("▎ ", bar));
                rest.push(Span::new("▎ ", bar));
            }
            Container::Item { marker, drawn } => {
                let pad = " ".repeat(UnicodeWidthStr::width(marker.as_str()));
                if *drawn {
                    first.push(Span::new(pad.clone(), Look::default()));
                } else {
                    first.push(Span::new(marker.clone(), bar));
                    *drawn = true;
                }
                rest.push(Span::new(pad, Look::default()));
            }
        }
    }
    (first, rest)
}

fn in_item(containers: &[Container]) -> bool {
    containers
        .iter()
        .any(|c| matches!(c, Container::Item { .. }))
}

/// Ends the paragraph or heading being read, if any: a text block, or an
/// image block when it held only an image. Blank lines separate blocks
/// except inside list items.
fn flush_text(
    doc: &mut Doc,
    containers: &mut [Container],
    para: &mut Option<Vec<Span>>,
    lone_image: &mut Option<usize>,
    heading: bool,
) {
    let Some(spans) = para.take() else { return };
    let image = lone_image.take();
    if spans.iter().all(|s| s.text.trim().is_empty()) {
        return;
    }
    let (first, rest) = prefixes(containers);
    if heading && !doc.blocks.is_empty() && !in_item(containers) {
        push_blank(&mut doc.blocks);
    }
    match image {
        Some(index) if spans.len() == 1 => doc.blocks.push(Block::Image {
            indent: first,
            index,
        }),
        _ => doc.blocks.push(Block::Text { first, rest, spans }),
    }
    if !in_item(containers) {
        push_blank(&mut doc.blocks);
    }
}

/// Parses markdown into blocks.
pub fn parse(text: &str) -> Doc {
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut doc = Doc::default();
    let mut containers: Vec<Container> = Vec::new();
    // Ordered lists: the next number; bullets: None.
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut look = Look::default();
    let mut para: Option<Vec<Span>> = None;
    let mut code: Option<String> = None;
    let mut table: Option<TableBuild> = None;
    let mut cell: Option<Vec<Span>> = None;
    let mut link: Option<String> = None;
    let mut link_text = String::new();
    // An image being read: its source and alt text.
    let mut image: Option<(String, String)> = None;
    // A paragraph holding just an image becomes an image block.
    let mut lone_image: Option<usize> = None;

    for event in Parser::new_ext(text, opts) {
        match event {
            Event::Start(tag) => {
                // A new block ends the text of a tight list item.
                if matches!(
                    tag,
                    Tag::Paragraph
                        | Tag::Heading { .. }
                        | Tag::BlockQuote(_)
                        | Tag::CodeBlock(_)
                        | Tag::List(_)
                        | Tag::Item
                        | Tag::Table(_)
                ) {
                    flush_text(&mut doc, &mut containers, &mut para, &mut lone_image, false);
                }
                match tag {
                    Tag::Paragraph => para = Some(Vec::new()),
                    Tag::Heading { level, .. } => {
                        para = Some(Vec::new());
                        look.heading = heading_level(level);
                        look.bold = true;
                    }
                    Tag::BlockQuote(_) => containers.push(Container::Quote),
                    Tag::CodeBlock(_) => code = Some(String::new()),
                    Tag::List(start) => lists.push(start),
                    Tag::Item => {
                        let depth = lists.len();
                        let marker = match lists.last_mut() {
                            Some(Some(n)) => {
                                let m = format!("{n}. ");
                                *n += 1;
                                m
                            }
                            _ => (if depth > 1 { "◦ " } else { "• " }).to_owned(),
                        };
                        containers.push(Container::Item {
                            marker,
                            drawn: false,
                        });
                        // Tight list items have no paragraph: start one.
                        para = Some(Vec::new());
                    }
                    Tag::Table(align) => {
                        table = Some(TableBuild {
                            align,
                            header: Vec::new(),
                            rows: Vec::new(),
                            row: Vec::new(),
                        });
                    }
                    Tag::TableCell => cell = Some(Vec::new()),
                    Tag::Emphasis => look.italic = true,
                    Tag::Strong => look.bold = true,
                    Tag::Strikethrough => look.strike = true,
                    Tag::Link { dest_url, .. } => {
                        look.link = true;
                        link = Some(dest_url.into_string());
                    }
                    Tag::Image { dest_url, .. } => {
                        image = Some((dest_url.into_string(), String::new()));
                    }
                    _ => {}
                }
            }
            Event::End(end) => match end {
                TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::Item => {
                    let heading = look.heading > 0;
                    flush_text(
                        &mut doc,
                        &mut containers,
                        &mut para,
                        &mut lone_image,
                        heading,
                    );
                    if heading {
                        look.heading = 0;
                        look.bold = false;
                    }
                    if matches!(end, TagEnd::Item) {
                        containers.pop();
                    }
                }
                TagEnd::BlockQuote(_) => {
                    containers.pop();
                }
                TagEnd::CodeBlock => {
                    if let Some(text) = code.take() {
                        let (first, rest) = prefixes(&mut containers);
                        let lines = text
                            .trim_end_matches('\n')
                            .lines()
                            .map(str::to_owned)
                            .collect();
                        doc.blocks.push(Block::Code { first, rest, lines });
                        if !in_item(&containers) {
                            push_blank(&mut doc.blocks);
                        }
                    }
                }
                TagEnd::List(_) => {
                    lists.pop();
                    if !in_item(&containers) {
                        push_blank(&mut doc.blocks);
                    }
                }
                TagEnd::Table => {
                    if let Some(t) = table.take() {
                        let (indent, _) = prefixes(&mut containers);
                        doc.blocks.push(Block::Table {
                            indent,
                            align: t.align,
                            header: t.header,
                            rows: t.rows,
                        });
                        push_blank(&mut doc.blocks);
                    }
                }
                TagEnd::TableHead => {
                    if let Some(t) = &mut table {
                        t.header = std::mem::take(&mut t.row);
                    }
                }
                TagEnd::TableRow => {
                    if let Some(t) = &mut table {
                        let row = std::mem::take(&mut t.row);
                        t.rows.push(row);
                    }
                }
                TagEnd::TableCell => {
                    if let (Some(t), Some(c)) = (&mut table, cell.take()) {
                        t.row.push(c);
                    }
                }
                TagEnd::Emphasis => look.italic = false,
                TagEnd::Strong => look.bold = look.heading > 0,
                TagEnd::Strikethrough => look.strike = false,
                TagEnd::Link => {
                    look.link = false;
                    // External links show where they go, unless the text
                    // already says it.
                    if let Some(url) = link.take()
                        && (url.starts_with("http://")
                            || url.starts_with("https://")
                            || url.starts_with("mailto:"))
                        && link_text.trim() != url
                        && let Some(spans) = shown_spans(&mut para, &mut cell)
                    {
                        spans.push(Span::new(
                            format!(" ({url})"),
                            Look {
                                muted: true,
                                ..Look::default()
                            },
                        ));
                    }
                    link_text.clear();
                }
                TagEnd::Image => {
                    if let Some((src, alt)) = image.take() {
                        let index = doc.images.len();
                        doc.images.push(ImageRef {
                            src,
                            alt: alt.clone(),
                        });
                        let in_cell = cell.is_some();
                        if let Some(spans) = shown_spans(&mut para, &mut cell) {
                            if spans.is_empty() && !in_cell {
                                lone_image = Some(index);
                            }
                            spans.push(Span::new(
                                format!("🖼 {}", if alt.is_empty() { "image" } else { &alt }),
                                Look {
                                    link: true,
                                    ..Look::default()
                                },
                            ));
                        }
                    }
                }
                _ => {}
            },
            Event::Text(t) => {
                if let Some((_, alt)) = &mut image {
                    alt.push_str(&t);
                } else if let Some(c) = &mut code {
                    c.push_str(&t);
                } else if let Some(spans) = shown_spans(&mut para, &mut cell) {
                    if look.link {
                        link_text.push_str(&t);
                    }
                    spans.push(Span::new(t.into_string(), look));
                    lone_image = None;
                }
            }
            Event::Code(t) => {
                if let Some(spans) = shown_spans(&mut para, &mut cell) {
                    spans.push(Span::new(t.into_string(), Look { code: true, ..look }));
                    lone_image = None;
                }
            }
            Event::SoftBreak => {
                if let Some(spans) = shown_spans(&mut para, &mut cell) {
                    spans.push(Span::new(" ", look));
                }
            }
            Event::HardBreak => {
                if let Some(spans) = shown_spans(&mut para, &mut cell) {
                    spans.push(Span::new(BREAK, look));
                }
            }
            Event::TaskListMarker(done) => {
                if let Some(spans) = shown_spans(&mut para, &mut cell) {
                    spans.push(Span::new(if done { "☑ " } else { "☐ " }, look));
                }
            }
            Event::Rule => {
                let (indent, _) = prefixes(&mut containers);
                doc.blocks.push(Block::Rule { indent });
                push_blank(&mut doc.blocks);
            }
            Event::Html(t) | Event::InlineHtml(t) => {
                // Shown as written, like code: TermOS does not render HTML.
                if let Some(spans) = shown_spans(&mut para, &mut cell) {
                    spans.push(Span::new(
                        t.trim_end().to_owned(),
                        Look {
                            muted: true,
                            ..Look::default()
                        },
                    ));
                }
            }
            _ => {}
        }
    }
    while doc.blocks.last() == Some(&Block::Blank) {
        doc.blocks.pop();
    }
    doc
}

/// Where inline text goes: the table cell being read, else the paragraph.
fn shown_spans<'a>(
    para: &'a mut Option<Vec<Span>>,
    cell: &'a mut Option<Vec<Span>>,
) -> Option<&'a mut Vec<Span>> {
    match cell {
        Some(c) => Some(c),
        None => para.as_mut(),
    }
}

fn push_blank(blocks: &mut Vec<Block>) {
    if !matches!(blocks.last(), None | Some(Block::Blank)) {
        blocks.push(Block::Blank);
    }
}

fn heading_level(l: HeadingLevel) -> u8 {
    match l {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

// ----- layout -----------------------------------------------------------

/// One row on screen.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    Text(Vec<Span>),
    /// Row `row` of an image `rows` tall, at column `x`, `w` wide.
    Image {
        index: usize,
        row: u16,
        rows: u16,
        x: u16,
        w: u16,
    },
}

fn spans_width(spans: &[Span]) -> u16 {
    u16::try_from(spans.iter().map(Span::width).sum::<usize>()).unwrap_or(u16::MAX)
}

/// Appends text to a row, merging with the last span when it looks alike.
fn push_text(row: &mut Vec<Span>, text: &str, look: Look) {
    match row.last_mut() {
        Some(last) if last.look == look => last.text.push_str(text),
        _ => row.push(Span::new(text, look)),
    }
}

/// Words and the spaces between them, each with its look.
fn tokens(spans: &[Span]) -> Vec<(String, Look, bool)> {
    let mut out: Vec<(String, Look, bool)> = Vec::new();
    for s in spans {
        if s.text == BREAK {
            out.push((BREAK.to_owned(), s.look, false));
            continue;
        }
        for ch in s.text.chars() {
            let space = ch == ' ' || ch == '\t';
            let ch = if ch == '\t' { ' ' } else { ch };
            match out.last_mut() {
                Some((t, look, sp)) if *sp == space && *look == s.look && t != BREAK => t.push(ch),
                _ => out.push((ch.to_string(), s.look, space)),
            }
        }
    }
    out
}

/// Wraps `spans` to `width`, starting rows with `first`, then `rest`.
pub fn wrap(first: &[Span], rest: &[Span], spans: &[Span], width: u16) -> Vec<Vec<Span>> {
    let mut rows: Vec<Vec<Span>> = Vec::new();
    let mut row: Vec<Span> = first.to_vec();
    let mut used = spans_width(first);
    let mut fresh = true;
    let new_row = |rows: &mut Vec<Vec<Span>>, row: &mut Vec<Span>, used: &mut u16| {
        rows.push(std::mem::replace(row, rest.to_vec()));
        *used = spans_width(rest);
    };
    for (text, look, space) in tokens(spans) {
        if text == BREAK {
            new_row(&mut rows, &mut row, &mut used);
            fresh = true;
            continue;
        }
        let w = u16::try_from(UnicodeWidthStr::width(text.as_str())).unwrap_or(u16::MAX);
        if space {
            if fresh {
                continue;
            }
            if used + w >= width {
                new_row(&mut rows, &mut row, &mut used);
                fresh = true;
            } else {
                push_text(&mut row, &text, look);
                used += w;
            }
            continue;
        }
        if used + w > width && !fresh {
            new_row(&mut rows, &mut row, &mut used);
        }
        if used + w <= width {
            push_text(&mut row, &text, look);
            used += w;
        } else {
            // Longer than a whole row: break it anywhere.
            for ch in text.chars() {
                let cw = u16::try_from(ch.width().unwrap_or(0)).unwrap_or(1);
                if used + cw > width && used > spans_width(rest) {
                    new_row(&mut rows, &mut row, &mut used);
                }
                push_text(&mut row, ch.encode_utf8(&mut [0; 4]), look);
                used += cw;
            }
        }
        fresh = false;
    }
    rows.push(row);
    rows
}

/// Cuts `text` to `max` cells, ending in `…` when cut.
fn ellipsize(text: &str, max: usize) -> String {
    if UnicodeWidthStr::width(text) <= max {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in text.chars() {
        let cw = ch.width().unwrap_or(0);
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

fn plain(spans: &[Span]) -> String {
    spans.iter().map(|s| s.text.as_str()).collect()
}

/// Table columns, shrunk (widest first) to fit `avail`.
fn column_widths(header: &[Vec<Span>], rows: &[Vec<Vec<Span>>], avail: usize) -> Vec<usize> {
    let n = header
        .len()
        .max(rows.iter().map(Vec::len).max().unwrap_or(0));
    let mut w = vec![1usize; n];
    for r in std::iter::once(header).chain(rows.iter().map(Vec::as_slice)) {
        for (i, c) in r.iter().enumerate() {
            w[i] = w[i].max(UnicodeWidthStr::width(plain(c).as_str()));
        }
    }
    let seps = 3 * n.saturating_sub(1);
    while w.iter().sum::<usize>() + seps > avail {
        let Some((i, widest)) = w.iter().copied().enumerate().max_by_key(|(_, v)| *v) else {
            break;
        };
        if widest <= 3 {
            break;
        }
        w[i] = widest - 1;
    }
    w
}

fn aligned(text: &str, width: usize, align: Alignment) -> String {
    let text = ellipsize(text, width);
    let pad = width.saturating_sub(UnicodeWidthStr::width(text.as_str()));
    match align {
        Alignment::Right => format!("{}{text}", " ".repeat(pad)),
        Alignment::Center => format!("{}{text}{}", " ".repeat(pad / 2), " ".repeat(pad - pad / 2)),
        _ => format!("{text}{}", " ".repeat(pad)),
    }
}

/// Lays `doc` out for `width` columns. `sizes` holds each image's pixel
/// size once it is decoded; until then (or if it failed) an image is one
/// line of text.
pub fn layout(doc: &Doc, width: u16, sizes: &[Option<(u32, u32)>]) -> Vec<Row> {
    let width = width.max(4);
    let mut out = Vec::new();
    let muted = Look {
        muted: true,
        ..Look::default()
    };
    for block in &doc.blocks {
        match block {
            Block::Blank => out.push(Row::Text(Vec::new())),
            Block::Text { first, rest, spans } => {
                out.extend(wrap(first, rest, spans, width).into_iter().map(Row::Text));
            }
            Block::Code { first, rest, lines } => {
                let code = Look {
                    code: true,
                    ..Look::default()
                };
                let mut firstrow = true;
                for line in lines
                    .iter()
                    .map(String::as_str)
                    .chain(lines.is_empty().then_some(""))
                {
                    let prefix = if firstrow { first } else { rest };
                    let mut lead = prefix.to_vec();
                    lead.push(Span::new("│ ", muted));
                    let mut cont = rest.to_vec();
                    cont.push(Span::new("│ ", muted));
                    let spans = [Span::new(line.replace('\t', "    "), code)];
                    // Code keeps its spaces: wrap by characters only.
                    let mut rows = Vec::new();
                    let mut row = lead.clone();
                    let mut used = spans_width(&row);
                    for ch in spans[0].text.chars() {
                        let cw = u16::try_from(ch.width().unwrap_or(0)).unwrap_or(1);
                        if used + cw > width {
                            rows.push(std::mem::replace(&mut row, cont.clone()));
                            used = spans_width(&cont);
                        }
                        push_text(&mut row, ch.encode_utf8(&mut [0; 4]), code);
                        used += cw;
                    }
                    rows.push(row);
                    out.extend(rows.into_iter().map(Row::Text));
                    firstrow = false;
                }
            }
            Block::Rule { indent } => {
                let mut row = indent.clone();
                let n = width.saturating_sub(spans_width(indent));
                row.push(Span::new("─".repeat(usize::from(n)), muted));
                out.push(Row::Text(row));
            }
            Block::Table {
                indent,
                align,
                header,
                rows,
            } => {
                let avail = usize::from(width.saturating_sub(spans_width(indent)));
                let widths = column_widths(header, rows, avail);
                let line = |cells: &[Vec<Span>], bold: bool| {
                    let mut row = indent.clone();
                    for (i, w) in widths.iter().enumerate() {
                        if i > 0 {
                            row.push(Span::new(" │ ", muted));
                        }
                        let text = cells.get(i).map(|c| plain(c)).unwrap_or_default();
                        let look = Look {
                            bold,
                            ..cells
                                .get(i)
                                .and_then(|c| c.first())
                                .map_or(Look::default(), |s| s.look)
                        };
                        let a = align.get(i).copied().unwrap_or(Alignment::None);
                        row.push(Span::new(aligned(&text, *w, a), look));
                    }
                    Row::Text(row)
                };
                if !header.is_empty() {
                    out.push(line(header, true));
                    let mut sep = indent.clone();
                    let rule: Vec<String> = widths.iter().map(|w| "─".repeat(*w)).collect();
                    sep.push(Span::new(rule.join("─┼─"), muted));
                    out.push(Row::Text(sep));
                }
                for r in rows {
                    out.push(line(r, false));
                }
            }
            Block::Image { indent, index } => {
                let x = spans_width(indent);
                let avail = width.saturating_sub(x).max(1);
                match sizes.get(*index).copied().flatten() {
                    Some((pw, ph)) if pw > 0 && ph > 0 => {
                        // About 10×20 pixels per cell; the desktop fits the
                        // picture exactly for each terminal.
                        let natural = u16::try_from(pw.div_ceil(10)).unwrap_or(u16::MAX);
                        let w = natural.clamp(1, avail);
                        let rows = (f64::from(w) * f64::from(ph) / f64::from(pw) / 2.0)
                            .round()
                            .clamp(1.0, f64::from(MAX_IMAGE_ROWS))
                            as u16;
                        for row in 0..rows {
                            out.push(Row::Image {
                                index: *index,
                                row,
                                rows,
                                x,
                                w,
                            });
                        }
                    }
                    _ => {
                        let alt = doc.images.get(*index).map_or("", |i| i.alt.as_str());
                        let mut row = indent.clone();
                        row.push(Span::new(
                            format!("🖼 {}", if alt.is_empty() { "image" } else { alt }),
                            Look {
                                link: true,
                                ..Look::default()
                            },
                        ));
                        out.push(Row::Text(row));
                    }
                }
            }
        }
    }
    out
}

/// The theme style for a look.
pub fn style(look: Look, theme: &Theme) -> Style {
    let c = &theme.colors;
    let mut s = Style::new().fg(c.fg);
    if look.heading > 0 {
        s = s.fg(if look.heading <= 2 { c.accent } else { c.fg });
    }
    if look.code {
        s = s.fg(c.warn);
    }
    if look.link {
        s = s.fg(c.accent).attrs(Attrs::UNDERLINE);
    }
    if look.muted {
        s = s.fg(c.muted);
    }
    let mut attrs = s.attrs;
    for (on, a) in [
        (look.bold, Attrs::BOLD),
        (look.italic, Attrs::ITALIC),
        (look.strike, Attrs::STRIKE),
        (look.heading == 1, Attrs::UNDERLINE),
    ] {
        if on {
            attrs |= a;
        }
    }
    s.attrs = attrs;
    s
}

// ----- the app ----------------------------------------------------------

enum Pic {
    Loading,
    Ready {
        key: u64,
        picture: Arc<Picture>,
    },
    /// Not shown: why (on the web, unreadable, not an image).
    Missing,
}

type Decoded = (usize, Result<Picture, String>);

pub struct MarkdownViewer {
    path: PathBuf,
    doc: Result<Doc, String>,
    pics: Vec<Pic>,
    rx: Option<Receiver<Decoded>>,
    waker: Arc<Mutex<Option<AppWaker>>>,
    rows: Option<(u16, Vec<Row>)>,
    scroll: usize,
    height: u16,
    shown: Vec<AppImage>,
}

impl std::fmt::Debug for MarkdownViewer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MarkdownViewer")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// Where an image's source points, if it is a local file.
fn local_image(base: &Path, src: &str) -> Option<PathBuf> {
    let src = src.strip_prefix("file://").unwrap_or(src);
    if src.is_empty() || src.contains("://") || src.starts_with("data:") {
        return None;
    }
    let p = Path::new(src);
    Some(if p.is_absolute() {
        p.to_owned()
    } else {
        base.join(p)
    })
}

fn read_picture(path: &Path) -> Result<Picture, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > MAX_IMAGE_BYTES {
        return Err("too large".into());
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    Picture::decode(&bytes).map_err(|e| e.to_string())
}

impl MarkdownViewer {
    pub fn open(path: PathBuf) -> Self {
        let mut v = Self {
            path,
            doc: Err(String::new()),
            pics: Vec::new(),
            rx: None,
            waker: Arc::new(Mutex::new(None)),
            rows: None,
            scroll: 0,
            height: 0,
            shown: Vec::new(),
        };
        v.load();
        v
    }

    fn load(&mut self) {
        self.rows = None;
        self.doc = match std::fs::metadata(&self.path) {
            Ok(m) if m.len() > MAX_FILE_BYTES => Err(format!(
                "{} is too large ({} MiB; at most {} MiB)",
                self.path.display(),
                m.len() / (1024 * 1024),
                MAX_FILE_BYTES / (1024 * 1024)
            )),
            Ok(_) => std::fs::read(&self.path)
                .map(|b| parse(&String::from_utf8_lossy(&b)))
                .map_err(|e| format!("cannot read {}: {e}", self.path.display())),
            Err(e) => Err(format!("cannot read {}: {e}", self.path.display())),
        };
        let base = self.path.parent().map(Path::to_owned).unwrap_or_default();
        let jobs: Vec<(usize, PathBuf)> = match &self.doc {
            Ok(doc) => doc
                .images
                .iter()
                .enumerate()
                .filter_map(|(i, r)| local_image(&base, &r.src).map(|p| (i, p)))
                .collect(),
            Err(_) => Vec::new(),
        };
        let n = self.doc.as_ref().map_or(0, |d| d.images.len());
        self.pics = (0..n).map(|_| Pic::Missing).collect();
        for (i, _) in &jobs {
            self.pics[*i] = Pic::Loading;
        }
        self.rx = None;
        if jobs.is_empty() {
            return;
        }
        let (tx, rx) = channel();
        self.rx = Some(rx);
        let waker = Arc::clone(&self.waker);
        let spawned = std::thread::Builder::new()
            .name("markdown-images".into())
            .spawn(move || {
                for (i, path) in jobs {
                    if tx.send((i, read_picture(&path))).is_err() {
                        return;
                    }
                    if let Some(w) = waker.lock().ok().and_then(|w| w.clone()) {
                        w.wake();
                    }
                }
            });
        if spawned.is_err() {
            self.rx = None;
            for p in &mut self.pics {
                if matches!(p, Pic::Loading) {
                    *p = Pic::Missing;
                }
            }
        }
    }

    /// Takes decoded images; a new one changes the layout.
    fn poll(&mut self) {
        let Some(rx) = &self.rx else { return };
        loop {
            match rx.try_recv() {
                Ok((i, result)) => {
                    if let Some(p) = self.pics.get_mut(i) {
                        *p = match result {
                            Ok(picture) => Pic::Ready {
                                key: new_image_key(),
                                picture: Arc::new(picture),
                            },
                            Err(e) => {
                                tracing::debug!("a markdown image was not shown: {e}");
                                Pic::Missing
                            }
                        };
                    }
                    self.rows = None;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.rx = None;
                    break;
                }
            }
        }
    }

    fn sizes(&self) -> Vec<Option<(u32, u32)>> {
        self.pics
            .iter()
            .map(|p| match p {
                Pic::Ready { picture, .. } => Some((picture.width(), picture.height())),
                _ => None,
            })
            .collect()
    }

    fn row_count(&self) -> usize {
        self.rows.as_ref().map_or(0, |(_, r)| r.len())
    }

    fn max_scroll(&self) -> usize {
        self.row_count().saturating_sub(usize::from(self.height))
    }

    fn scroll_by(&mut self, delta: isize) -> AppEffect {
        let before = self.scroll;
        self.scroll = self
            .scroll
            .saturating_add_signed(delta)
            .min(self.max_scroll());
        if self.scroll == before {
            AppEffect::None
        } else {
            AppEffect::Redraw
        }
    }
}

impl App for MarkdownViewer {
    fn icon(&self) -> termos_render::icons::Icon {
        termos_render::icons::Icon::Markdown
    }

    fn title(&self) -> String {
        let name = self.path.file_name().map_or_else(
            || self.path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let total = self.row_count();
        if total <= usize::from(self.height) || self.height == 0 {
            format!("Markdown · {name}")
        } else {
            let pct = (self.scroll * 100) / self.max_scroll().max(1);
            format!("Markdown · {name} · {pct}%")
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> AppEffect {
        let page = isize::try_from(self.height.max(2) - 1).unwrap_or(1);
        if key == KeyEvent::ctrl('d') {
            return self.scroll_by(page / 2);
        }
        if key == KeyEvent::ctrl('u') {
            return self.scroll_by(-(page / 2));
        }
        if !key.mods.is_empty() {
            return AppEffect::None;
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down | KeyCode::Enter => self.scroll_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.scroll_by(-1),
            KeyCode::Char(' ' | 'f') | KeyCode::PageDown => self.scroll_by(page),
            KeyCode::Char('b') | KeyCode::PageUp | KeyCode::Backspace => self.scroll_by(-page),
            KeyCode::Char('g') | KeyCode::Home => self.scroll_by(isize::MIN / 2),
            KeyCode::Char('G') | KeyCode::End => self.scroll_by(isize::MAX / 2),
            KeyCode::Char('r') => {
                self.load();
                AppEffect::Redraw
            }
            KeyCode::Char('q') | KeyCode::Esc => AppEffect::Close,
            _ => AppEffect::None,
        }
    }

    fn handle_mouse(&mut self, event: MouseEvent) -> AppEffect {
        match event.kind {
            MouseKind::ScrollDown => self.scroll_by(3),
            MouseKind::ScrollUp => self.scroll_by(-3),
            _ => AppEffect::None,
        }
    }

    fn render(&mut self, area: TuiRect, buf: &mut Buffer, theme: &Theme) {
        self.poll();
        self.height = area.height;
        self.shown.clear();
        if area.is_empty() {
            return;
        }
        let doc = match &self.doc {
            Ok(d) => d,
            Err(e) => {
                let style = tui_style(Style::new().fg(theme.colors.error));
                let text = ellipsize(e, usize::from(area.width));
                buf.set_string(area.x, area.y, text, style);
                return;
            }
        };
        if self.rows.as_ref().is_none_or(|(w, _)| *w != area.width) {
            let rows = layout(doc, area.width, &self.sizes());
            self.rows = Some((area.width, rows));
        }
        self.scroll = self.scroll.min(self.max_scroll());
        let Some((_, rows)) = &self.rows else { return };
        let h = usize::from(area.height);
        for (i, row) in rows.iter().skip(self.scroll).take(h).enumerate() {
            let y = area.y + u16::try_from(i).unwrap_or(u16::MAX);
            match row {
                Row::Text(spans) => {
                    let mut x = area.x;
                    for s in spans {
                        let w = u16::try_from(s.width()).unwrap_or(u16::MAX);
                        let end = area.right();
                        if x >= end {
                            break;
                        }
                        buf.set_stringn(
                            x,
                            y,
                            &s.text,
                            usize::from(end - x),
                            tui_style(style(s.look, theme)),
                        );
                        x = x.saturating_add(w);
                    }
                }
                Row::Image {
                    index,
                    row,
                    rows: total,
                    x,
                    w,
                } => {
                    // One placement per image, from its first visible row.
                    if *row != 0 && i != 0 {
                        continue;
                    }
                    if let Some(Pic::Ready { key, picture }) = self.pics.get(*index) {
                        self.shown.push(AppImage {
                            key: *key,
                            picture: Arc::clone(picture),
                            rect: Rect::new(*x, y - area.y, *w, *total),
                            upscale: true,
                            scroll_top: *row,
                            pixels_only: false,
                        });
                    }
                }
            }
        }
    }

    fn images(&self) -> Vec<AppImage> {
        self.shown.clone()
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

    fn text(rows: &[Row]) -> String {
        rows.iter()
            .map(|r| match r {
                Row::Text(spans) => plain(spans).trim_end().to_owned(),
                Row::Image { index, row, .. } => format!("[image {index} row {row}]"),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    const SAMPLE: &str = "# TermOS\n\nA *terminal-first* desktop with **real** `code` and a [link](https://example.com).\n\n## Lists\n\n- one\n- two\n  - nested\n- [x] done\n- [ ] todo\n\n1. first\n2. second\n\n> quoted text\n> that goes on\n\n```rust\nfn main() {}\n```\n\n| Name | Size |\n| :--- | ---: |\n| a.txt | 10 |\n| b.png | 2048 |\n\n---\n\n![a cat](cat.png)\n";

    #[test]
    fn a_document_lays_out() {
        let doc = parse(SAMPLE);
        insta::assert_snapshot!("markdown-sample-40", text(&layout(&doc, 40, &[None])));
        // With the picture decoded, it takes rows of its own.
        let rows = layout(&doc, 40, &[Some((200, 100))]);
        let images = rows
            .iter()
            .filter(|r| matches!(r, Row::Image { .. }))
            .count();
        assert_eq!(images, 5, "20 columns × 100/200 / 2");
    }

    #[test]
    fn wrapping_keeps_words_and_prefixes() {
        let s = [Span::new("the quick brown fox jumps", Look::default())];
        let first = [Span::new("• ", Look::default())];
        let rest = [Span::new("  ", Look::default())];
        let rows = wrap(&first, &rest, &s, 12);
        let lines: Vec<String> = rows.iter().map(|r| plain(r)).collect();
        assert_eq!(lines, ["• the quick", "  brown fox", "  jumps"]);
        // A word longer than a row is broken.
        let long = [Span::new("abcdefghijkl", Look::default())];
        let lines: Vec<String> = wrap(&[], &[], &long, 5).iter().map(|r| plain(r)).collect();
        assert_eq!(lines, ["abcde", "fghij", "kl"]);
    }

    #[test]
    fn only_local_images_are_read() {
        let base = Path::new("/docs");
        assert_eq!(
            local_image(base, "a.png"),
            Some(PathBuf::from("/docs/a.png"))
        );
        assert_eq!(
            local_image(base, "/abs/b.png"),
            Some(PathBuf::from("/abs/b.png"))
        );
        assert_eq!(
            local_image(base, "file:///x/c.png"),
            Some(PathBuf::from("/x/c.png"))
        );
        assert_eq!(local_image(base, "https://example.com/d.png"), None);
        assert_eq!(local_image(base, "data:image/png;base64,AAAA"), None);
    }
}
