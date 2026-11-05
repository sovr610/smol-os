// SPDX-License-Identifier: GPL-3.0-or-later
//! Links and paths found in a pane's text (M13a, D-118): web and email
//! addresses, file paths with an optional line and column, git hashes and
//! IP addresses.
//!
//! Only the rows in view are scanned, and only when asked (hint mode, a
//! Ctrl-hover, a Ctrl+click), never on every frame. A path counts only if
//! it exists, resolved against the pane's folder. Rows the terminal wrapped
//! are joined first, so an address split across rows is found whole.
//!
//! [`find_in_line`] is fuzzed by `fuzz/fuzz_targets/link_detect.rs`.

use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

/// What a match is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// A web address, or a link a program printed (OSC 8).
    Url,
    Email,
    Path,
    /// A git commit hash.
    Hash,
    Ip,
}

impl Kind {
    /// The kinds `pane.hints KIND` labels: `url` takes web and email
    /// addresses, `any` everything.
    pub fn in_filter(self, filter: &str) -> bool {
        match filter {
            "" | "any" => true,
            "url" => matches!(self, Kind::Url | Kind::Email),
            "path" => self == Kind::Path,
            "hash" => self == Kind::Hash,
            "ip" => self == Kind::Ip,
            _ => false,
        }
    }
}

/// The filters `pane.hints` takes.
pub const FILTERS: [&str; 5] = ["any", "url", "path", "hash", "ip"];

/// What opening a match does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// An address to open: `http`, `https`, `mailto` or `file`.
    Url(String),
    /// A file or folder, with the line and column printed after it.
    Path {
        path: PathBuf,
        line: Option<u32>,
        column: Option<u32>,
    },
    /// Nothing to open (a hash, an address): opening copies the text.
    Text(String),
}

/// A match in raw text (before paths are checked).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawMatch {
    pub kind: Kind,
    /// Byte range in the line.
    pub start: usize,
    pub end: usize,
    /// For paths: the path's own end (before `:line`), its line and
    /// column.
    pub path_end: usize,
    pub line: Option<u32>,
    pub column: Option<u32>,
}

/// One row in view, as cells: its text and where each character is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ViewRow {
    pub text: String,
    /// For each character of `text`, in order: its column and width (0
    /// for a combining mark, 2 for a wide character).
    pub cells: Vec<(u16, u8)>,
    /// The row continues on the next one (the terminal wrapped it).
    pub wraps: bool,
    /// Links programs printed on this row (OSC 8): first column, one past
    /// the last, and the address.
    pub links: Vec<(u16, u16, String)>,
}

impl ViewRow {
    /// A row of plain text, one column per character (tests, apps).
    pub fn plain(text: &str) -> Self {
        let cells = (0..text.chars().count())
            .map(|i| (u16::try_from(i).unwrap_or(u16::MAX), 1))
            .collect();
        Self {
            text: text.to_owned(),
            cells,
            wraps: false,
            links: Vec::new(),
        }
    }
}

/// A match on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub kind: Kind,
    /// The text as shown (what copying gives).
    pub text: String,
    pub target: Target,
    /// The cells it covers: (row, first column, one past the last).
    pub cells: Vec<(u16, u16, u16)>,
}

impl Match {
    /// Whether the cell (x, y) is part of it.
    pub fn covers(&self, x: u16, y: u16) -> bool {
        self.cells
            .iter()
            .any(|(row, x0, x1)| *row == y && (*x0..*x1).contains(&x))
    }

    /// Its first cell.
    pub fn start(&self) -> (u16, u16) {
        self.cells.first().map_or((0, 0), |(y, x, _)| (*x, *y))
    }
}

/// At most this many path candidates are checked on the file system per
/// scan, so a screen of text never costs much.
const MAX_PATH_CHECKS: usize = 256;

/// The longest text one match may have.
const MAX_MATCH: usize = 4096;

static URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\b(?i-u:https?://|file://|mailto:)[^\s<>"'`]+"#).expect("a valid pattern")
});

static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}")
        .expect("a valid pattern")
});

static HASH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b[0-9a-f]{7,40}\b").expect("a valid pattern"));

static IPV4: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:[0-9]{1,3}\.){3}[0-9]{1,3}\b").expect("a valid pattern"));

static IPV6: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[0-9A-Fa-f:.]*:[0-9A-Fa-f:.]*").expect("a valid pattern"));

/// A path, then `:line[:col]`, `(line[,col])` or `", line N` (Python).
static PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?x)
        (?P<path>(?:[A-Za-z]:)?[\w.~+@%=/\\-]+)
        (?:
            :(?P<line>[0-9]{1,9})(?::(?P<col>[0-9]{1,9}))?
          | \((?P<pline>[0-9]{1,9})(?:,\s*(?P<pcol>[0-9]{1,9}))?\)
          | ",\ line\ (?P<qline>[0-9]{1,9})
        )?"#,
    )
    .expect("a valid pattern")
});

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn char_before(text: &str, i: usize) -> Option<char> {
    text[..i].chars().next_back()
}

fn char_after(text: &str, i: usize) -> Option<char> {
    text[i..].chars().next()
}

/// The end of a URL once trailing punctuation and closing brackets it did
/// not open are cut off.
fn trim_url(text: &str, start: usize, mut end: usize) -> usize {
    loop {
        let s = &text[start..end];
        let Some(last) = s.chars().next_back() else {
            return end;
        };
        let cut = match last {
            '.' | ',' | ':' | ';' | '!' | '?' | '*' => true,
            ')' | ']' | '}' | '>' => {
                let open = match last {
                    ')' => '(',
                    ']' => '[',
                    '}' => '{',
                    _ => '<',
                };
                s.matches(open).count() < s.matches(last).count()
            }
            _ => false,
        };
        if !cut {
            return end;
        }
        end -= last.len_utf8();
    }
}

fn overlaps(taken: &[RawMatch], start: usize, end: usize) -> bool {
    taken.iter().any(|m| start < m.end && m.start < end)
}

fn number(text: Option<regex::Match<'_>>) -> Option<u32> {
    text.and_then(|m| m.as_str().parse().ok())
}

/// Whether a path candidate looks like a path at all (before it is looked
/// for): it has a separator, starts with `~`, or has an extension.
fn path_like(p: &str) -> bool {
    if !p.chars().any(char::is_alphanumeric) {
        return false;
    }
    if p.contains('/') || p.contains('\\') || p.starts_with('~') {
        return true;
    }
    // `name.ext`: a dot with something on each side, the extension
    // ending in a letter or digit.
    p.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty()
            && !ext.is_empty()
            && ext.chars().all(|c| c.is_ascii_alphanumeric())
            && ext.chars().any(|c| c.is_ascii_alphabetic())
    })
}

/// Finds the candidates in one line of text, sorted by where they start.
/// Paths are not checked here: [`find`] does that.
pub fn find_in_line(text: &str) -> Vec<RawMatch> {
    let mut out: Vec<RawMatch> = Vec::new();
    let raw = |kind, start, end| RawMatch {
        kind,
        start,
        end,
        path_end: end,
        line: None,
        column: None,
    };
    for m in URL.find_iter(text) {
        let end = trim_url(text, m.start(), m.end());
        let body = text[m.start()..end].split_once(':').map_or("", |(_, b)| b);
        if body.trim_start_matches('/').is_empty() || end - m.start() > MAX_MATCH {
            continue;
        }
        out.push(raw(Kind::Url, m.start(), end));
    }
    for m in EMAIL.find_iter(text) {
        if overlaps(&out, m.start(), m.end())
            || char_before(text, m.start()).is_some_and(|c| is_word(c) || c == '@')
        {
            continue;
        }
        out.push(raw(Kind::Email, m.start(), m.end()));
    }
    for m in IPV4.find_iter(text) {
        let before_dot = char_before(text, m.start()) == Some('.');
        let after_dot = text[m.end()..].starts_with('.')
            && char_after(text, m.end() + 1).is_some_and(|c| c.is_ascii_digit());
        if before_dot
            || after_dot
            || m.as_str().parse::<Ipv4Addr>().is_err()
            || overlaps(&out, m.start(), m.end())
        {
            continue;
        }
        out.push(raw(Kind::Ip, m.start(), m.end()));
    }
    for m in IPV6.find_iter(text) {
        let s = m.as_str();
        let (start, end) = (m.start(), m.end());
        let colons = s.matches(':').count();
        if !(s.contains("::") || colons == 7)
            || !s.chars().any(|c| c.is_ascii_hexdigit())
            || char_before(text, start).is_some_and(is_word)
            || char_after(text, end).is_some_and(is_word)
            || s.parse::<Ipv6Addr>().is_err()
            || overlaps(&out, start, end)
        {
            continue;
        }
        out.push(raw(Kind::Ip, start, end));
    }
    for m in HASH.find_iter(text) {
        let s = m.as_str();
        if !s.chars().any(|c| c.is_ascii_alphabetic()) || overlaps(&out, m.start(), m.end()) {
            continue;
        }
        out.push(raw(Kind::Hash, m.start(), m.end()));
    }
    for c in PATH.captures_iter(text) {
        let Some(p) = c.name("path") else { continue };
        // Trailing punctuation belongs to the sentence.
        let mut path_end = p.end();
        while path_end > p.start() && text[..path_end].ends_with(['.', ',', ';']) {
            path_end -= 1;
        }
        let path = &text[p.start()..path_end];
        if !path_like(path) || path.len() > MAX_MATCH {
            continue;
        }
        let trimmed = path_end != p.end();
        let line = if trimmed {
            None
        } else {
            number(c.name("line"))
                .or_else(|| number(c.name("pline")))
                .or_else(|| number(c.name("qline")))
        };
        let column = if trimmed || line.is_none() {
            None
        } else {
            number(c.name("col")).or_else(|| number(c.name("pcol")))
        };
        let end = match (c.get(0), line) {
            // Python's `", line N` is not shown as part of the path.
            (Some(_), Some(_)) if c.name("qline").is_some() => path_end,
            (Some(all), Some(_)) => all.end(),
            _ => path_end,
        };
        if overlaps(&out, p.start(), end) {
            continue;
        }
        out.push(RawMatch {
            kind: Kind::Path,
            start: p.start(),
            end,
            path_end,
            line,
            column,
        });
    }
    out.sort_by_key(|m| m.start);
    out
}

/// The home folder, for `~`.
fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// `text` as a path: `~` is the home folder, a relative path is in
/// `base`.
pub fn resolve(text: &str, base: Option<&Path>) -> Option<PathBuf> {
    let p = match text.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => {
            home()?.join(rest.trim_start_matches(['/', '\\']))
        }
        Some(_) => return None,
        None => PathBuf::from(text),
    };
    if p.is_absolute() {
        Some(p)
    } else {
        base.map(|b| b.join(p))
    }
}

/// The path a candidate names, if it exists: as written, or without a
/// diff's `a/` or `b/`.
fn existing(
    text: &str,
    base: Option<&Path>,
    exists: &mut dyn FnMut(&Path) -> bool,
) -> Option<PathBuf> {
    let direct = resolve(text, base)?;
    if exists(&direct) {
        return Some(direct);
    }
    let rest = text
        .strip_prefix("a/")
        .or_else(|| text.strip_prefix("b/"))?;
    let stripped = resolve(rest, base)?;
    exists(&stripped).then_some(stripped)
}

/// A logical line: rows joined where the terminal wrapped them, and where
/// each character came from.
struct Logical {
    text: String,
    /// For each character: its byte offset, row, column and width.
    chars: Vec<(usize, u16, u16, u8)>,
}

fn logical_lines(rows: &[ViewRow]) -> Vec<Logical> {
    let mut out = Vec::new();
    let mut cur = Logical {
        text: String::new(),
        chars: Vec::new(),
    };
    for (y, row) in rows.iter().enumerate() {
        let y = u16::try_from(y).unwrap_or(u16::MAX);
        for (ch, (x, w)) in row.text.chars().zip(row.cells.iter().copied()) {
            cur.chars.push((cur.text.len(), y, x, w));
            cur.text.push(ch);
        }
        if !row.wraps {
            out.push(std::mem::replace(
                &mut cur,
                Logical {
                    text: String::new(),
                    chars: Vec::new(),
                },
            ));
        }
    }
    if !cur.text.is_empty() {
        out.push(cur);
    }
    out
}

/// The cells a byte range of a logical line covers, row by row.
fn cells_of(line: &Logical, start: usize, end: usize) -> Vec<(u16, u16, u16)> {
    let mut out: Vec<(u16, u16, u16)> = Vec::new();
    for &(at, y, x, w) in &line.chars {
        if at < start || at >= end {
            continue;
        }
        let x1 = x.saturating_add(u16::from(w.max(1)));
        match out.last_mut() {
            Some((row, _, last)) if *row == y && x <= *last => *last = (*last).max(x1),
            _ => out.push((y, x, x1)),
        }
    }
    out
}

/// Everything to label or click in `rows` (the rows in view, top first):
/// the links programs printed, then what [`find_in_line`] finds, with
/// paths kept only where `exists` finds them (relative ones in `base`).
/// With `at`, only matches covering that cell (column, row) are returned,
/// and only their paths are looked for.
pub fn find(
    rows: &[ViewRow],
    base: Option<&Path>,
    at: Option<(u16, u16)>,
    exists: &mut dyn FnMut(&Path) -> bool,
) -> Vec<Match> {
    let mut out: Vec<Match> = Vec::new();
    let wanted = |m: &Match| at.is_none_or(|(x, y)| m.covers(x, y));
    for (y, row) in rows.iter().enumerate() {
        let y = u16::try_from(y).unwrap_or(u16::MAX);
        for (x0, x1, url) in &row.links {
            // A link that goes on from the row before is one match.
            if let Some(prev) = out.iter_mut().rev().find(|m| {
                m.kind == Kind::Url
                    && m.target == Target::Url(url.clone())
                    && m.cells
                        .last()
                        .is_some_and(|(py, _, _)| *py + 1 == y && *x0 == 0)
            }) {
                prev.cells.push((y, *x0, *x1));
                continue;
            }
            let text: String = row
                .text
                .chars()
                .zip(&row.cells)
                .filter(|(_, (x, _))| (*x0..*x1).contains(x))
                .map(|(c, _)| c)
                .collect();
            out.push(Match {
                kind: Kind::Url,
                text: text.trim().to_owned(),
                target: Target::Url(url.clone()),
                cells: vec![(y, *x0, *x1)],
            });
        }
    }
    out.retain(|m| wanted(m));
    let printed = out.clone();
    let mut checks = 0usize;
    for line in logical_lines(rows) {
        for raw in find_in_line(&line.text) {
            let cells = cells_of(&line, raw.start, raw.end);
            let text = line.text[raw.start..raw.end].to_owned();
            let candidate = Match {
                kind: raw.kind,
                text: text.clone(),
                target: Target::Text(text.clone()),
                cells,
            };
            if !wanted(&candidate)
                || printed.iter().any(|p| {
                    candidate
                        .cells
                        .iter()
                        .any(|(y, x0, x1)| (*x0..*x1).any(|x| p.covers(x, *y)))
                })
            {
                continue;
            }
            let target = match raw.kind {
                Kind::Url => Target::Url(text.clone()),
                Kind::Email => Target::Url(format!("mailto:{text}")),
                Kind::Hash | Kind::Ip => Target::Text(text.clone()),
                Kind::Path => {
                    if checks >= MAX_PATH_CHECKS {
                        continue;
                    }
                    checks += 1;
                    let Some(path) = existing(&line.text[raw.start..raw.path_end], base, exists)
                    else {
                        continue;
                    };
                    Target::Path {
                        path,
                        line: raw.line,
                        column: raw.column,
                    }
                }
            };
            out.push(Match {
                target,
                ..candidate
            });
        }
    }
    out.sort_by_key(|m| {
        let (x, y) = m.start();
        (y, x)
    });
    out
}

/// `%XX` escapes decoded (invalid UTF-8 replaced).
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = s.get(i + 1..i + 3)
            && let Ok(v) = u8::from_str_radix(hex, 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A `file:` address's host (empty for none) and path.
pub fn file_url(url: &str) -> Option<(String, PathBuf)> {
    let scheme = url.get(..5)?;
    if !scheme.eq_ignore_ascii_case("file:") {
        return None;
    }
    let rest = &url[5..];
    let (host, path) = match rest.strip_prefix("//") {
        Some(r) => {
            let i = r.find('/')?;
            (&r[..i], &r[i..])
        }
        None => ("", rest),
    };
    let path = percent_decode(path.split(['?', '#']).next().unwrap_or(path));
    // `file:///C:/x` on Windows.
    let path = match path.strip_prefix('/') {
        Some(p) if p.len() >= 2 && p.as_bytes()[1] == b':' && cfg!(windows) => p.to_owned(),
        _ => path,
    };
    (!path.is_empty()).then(|| (host.to_owned(), PathBuf::from(path)))
}

/// This machine's name, for `file:` addresses that name it.
pub fn local_host() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/sys/kernel/hostname")
            .ok()
            .map(|h| h.trim().to_owned())
            .filter(|h| !h.is_empty())
    }
    #[cfg(any(target_os = "macos", windows))]
    {
        sysinfo::System::host_name().ok().filter(|h| !h.is_empty())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        std::env::var("HOSTNAME").ok().filter(|h| !h.is_empty())
    }
}

/// Whether a `file:` address's host is this machine: none, `localhost`,
/// or this machine's name (`me` or `me.example.org`).
pub fn is_local_host(host: &str, local: Option<&str>) -> bool {
    if host.is_empty() || host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let short = |h: &str| h.split('.').next().unwrap_or(h).to_ascii_lowercase();
    local.is_some_and(|l| l.eq_ignore_ascii_case(host) || short(l) == short(host))
}

/// `PATH:LINE[:COL]` split, if `text` ends with a line number. (The
/// caller checks first whether the whole of it names a file.)
pub fn split_line(text: &str) -> (&str, Option<u32>, Option<u32>) {
    let mut parts = text.rsplitn(3, ':');
    let last = parts.next();
    let mid = parts.next();
    let first = parts.next();
    let num = |s: Option<&str>| {
        s.filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|s| s.parse::<u32>().ok())
    };
    match (first, num(mid), num(last)) {
        (Some(path), Some(line), Some(col)) if !path.is_empty() => (path, Some(line), Some(col)),
        _ => match text.rsplit_once(':') {
            Some((path, l)) if !path.is_empty() => match num(Some(l)) {
                Some(line) => (path, Some(line), None),
                None => (text, None, None),
            },
            _ => (text, None, None),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<(Kind, &str)> {
        find_in_line(text)
            .into_iter()
            .map(|m| (m.kind, &text[m.start..m.end]))
            .collect()
    }

    #[test]
    fn web_addresses_lose_trailing_punctuation() {
        assert_eq!(
            kinds("see https://example.com/a?b=c. and (http://x.org/wiki/A_(b)), done"),
            vec![
                (Kind::Url, "https://example.com/a?b=c"),
                (Kind::Url, "http://x.org/wiki/A_(b)"),
            ]
        );
        assert_eq!(
            kinds("<https://example.com/path>"),
            vec![(Kind::Url, "https://example.com/path")]
        );
        assert_eq!(
            kinds("mailto:me@example.com"),
            vec![(Kind::Url, "mailto:me@example.com")]
        );
        assert!(kinds("https:// nothing").is_empty());
        assert!(kinds("javascript:alert(1)").is_empty());
    }

    #[test]
    fn emails_hashes_and_addresses() {
        assert_eq!(
            kinds("mail ann.lee+x@mail.example.org now"),
            vec![(Kind::Email, "ann.lee+x@mail.example.org")]
        );
        assert_eq!(
            kinds("commit 3f2a9c1d0b and 1234567 and deadbeef"),
            vec![(Kind::Hash, "3f2a9c1d0b"), (Kind::Hash, "deadbeef")]
        );
        assert_eq!(
            kinds("from 192.168.1.20 to 10.0.0.1:8080"),
            vec![(Kind::Ip, "192.168.1.20"), (Kind::Ip, "10.0.0.1")]
        );
        assert!(kinds("version 1.2.3.4.5 and 999.1.1.1").is_empty());
        assert_eq!(
            kinds("listening on [::1]:80 and fe80::1ff:fe23:4567:890a"),
            vec![(Kind::Ip, "::1"), (Kind::Ip, "fe80::1ff:fe23:4567:890a")]
        );
        // Rust paths, times and MAC addresses are not addresses.
        assert!(
            kinds("std::io::Error at 12:30:45 on aa:bb:cc:dd:ee:ff")
                .iter()
                .all(|(k, _)| *k != Kind::Ip)
        );
    }

    #[test]
    fn paths_take_their_line_and_column() {
        fn one(text: &str) -> (&str, &str, Option<u32>, Option<u32>) {
            let m = find_in_line(text)
                .into_iter()
                .find(|m| m.kind == Kind::Path)
                .expect("a path");
            (
                &text[m.start..m.path_end],
                &text[m.start..m.end],
                m.line,
                m.column,
            )
        }
        assert_eq!(
            one("  --> src/main.rs:10:5"),
            ("src/main.rs", "src/main.rs:10:5", Some(10), Some(5))
        );
        assert_eq!(
            one("main.c:7: error: x"),
            ("main.c", "main.c:7", Some(7), None)
        );
        assert_eq!(
            one(r#"  File "/srv/app/x.py", line 42, in f"#),
            ("/srv/app/x.py", "/srv/app/x.py", Some(42), None)
        );
        assert_eq!(
            one(r"C:\src\a.cpp(12,3): error"),
            (r"C:\src\a.cpp", r"C:\src\a.cpp(12,3)", Some(12), Some(3))
        );
        assert_eq!(
            one("edit ~/notes.md."),
            ("~/notes.md", "~/notes.md", None, None)
        );
        assert!(
            kinds("a sentence, with words. And 3.5 apples")
                .iter()
                .all(|(k, _)| *k != Kind::Path)
        );
    }

    #[test]
    fn found_paths_must_exist() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir_all(dir.path().join("src")).expect("mkdir");
        std::fs::write(dir.path().join("src/lib.rs"), "").expect("write");
        let rows = vec![
            ViewRow::plain("error at src/lib.rs:3:9 and src/gone.rs:1"),
            ViewRow::plain("--- a/src/lib.rs"),
        ];
        let mut exists = |p: &Path| p.exists();
        let found = find(&rows, Some(dir.path()), None, &mut exists);
        let paths: Vec<(&str, &Target)> = found
            .iter()
            .filter(|m| m.kind == Kind::Path)
            .map(|m| (m.text.as_str(), &m.target))
            .collect();
        let lib = dir.path().join("src/lib.rs");
        assert_eq!(
            paths,
            vec![
                (
                    "src/lib.rs:3:9",
                    &Target::Path {
                        path: lib.clone(),
                        line: Some(3),
                        column: Some(9),
                    }
                ),
                (
                    "a/src/lib.rs",
                    &Target::Path {
                        path: lib,
                        line: None,
                        column: None,
                    }
                ),
            ]
        );
        assert_eq!(found[0].cells, vec![(0, 9, 23)]);
    }

    #[test]
    fn wrapped_rows_join_and_printed_links_come_first() {
        let mut a = ViewRow::plain("go to https://exam");
        a.wraps = true;
        let mut b = ViewRow::plain("ple.com/x now ");
        b.links.push((10, 13, "https://linked.example/".into()));
        let rows = vec![a, b];
        let found = find(&rows, None, None, &mut |_| false);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].target, Target::Url("https://example.com/x".into()));
        assert_eq!(found[0].text, "https://example.com/x");
        assert_eq!(found[0].cells, vec![(0, 6, 18), (1, 0, 9)]);
        assert_eq!(
            found[1].target,
            Target::Url("https://linked.example/".into())
        );
        assert_eq!(found[1].text, "now");
        // Only what covers the pointer.
        let at = find(&rows, None, Some((3, 1)), &mut |_| false);
        assert_eq!(at.len(), 1);
        assert_eq!(at[0].kind, Kind::Url);
        assert!(at[0].covers(3, 1));
    }

    #[test]
    fn wide_characters_keep_their_columns() {
        let row = ViewRow {
            text: "界 https://x.org".into(),
            cells: std::iter::once((0, 2))
                .chain((2..16).map(|x| (x, 1)))
                .collect(),
            wraps: false,
            links: Vec::new(),
        };
        let found = find(&[row], None, None, &mut |_| false);
        assert_eq!(found[0].cells, vec![(0, 3, 16)]);
    }

    #[test]
    fn line_suffixes_split_off() {
        assert_eq!(split_line("src/a.rs:10:5"), ("src/a.rs", Some(10), Some(5)));
        assert_eq!(split_line("src/a.rs:10"), ("src/a.rs", Some(10), None));
        assert_eq!(split_line("src/a.rs"), ("src/a.rs", None, None));
        assert_eq!(split_line("a:b"), ("a:b", None, None));
        assert_eq!(split_line(":10"), (":10", None, None));
    }

    #[test]
    fn file_addresses() {
        assert_eq!(
            file_url("file:///home/me/a%20b.txt"),
            Some((String::new(), PathBuf::from("/home/me/a b.txt")))
        );
        assert_eq!(
            file_url("FILE://box/etc/hosts?x#y"),
            Some(("box".into(), PathBuf::from("/etc/hosts")))
        );
        assert_eq!(file_url("file://host"), None);
        assert_eq!(file_url("https://x/y"), None);
        assert!(is_local_host("", None));
        assert!(is_local_host("LocalHost", None));
        assert!(is_local_host("me", Some("me.example.org")));
        assert!(is_local_host("ME.example.org", Some("me")));
        assert!(!is_local_host("other", Some("me")));
        assert!(!is_local_host("other", None));
        assert_eq!(percent_decode("a%2Fb%zz%e2%9c%93"), "a/b%zz✓");
    }

    #[test]
    fn hint_filters() {
        assert!(Kind::Email.in_filter("url"));
        assert!(Kind::Path.in_filter("any"));
        assert!(!Kind::Hash.in_filter("ip"));
        assert!(!Kind::Url.in_filter("nonsense"));
    }
}
