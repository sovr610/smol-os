// SPDX-License-Identifier: GPL-3.0-or-later
//! The SSH hosts the menus and the palette offer (M13b, D-120): the `Host`
//! names in `~/.ssh/config` and the files it `Include`s.
//!
//! Only plain names are kept: patterns (`*`, `?`, `!`) are not hosts one
//! can open, and `Match` blocks are skipped. Opening one runs the user's
//! own `ssh HOST`, so everything else in the file still applies. It lives
//! in termos-core because the desktop's menus read it (D-117).
//!
//! [`parse`] is fuzzed by `fuzz/fuzz_targets/ssh_config.rs`.

use std::path::{Path, PathBuf};

/// The deepest `Include` followed.
const MAX_DEPTH: usize = 8;

/// The most hosts kept.
pub const MAX_HOSTS: usize = 500;

/// A file's lines: the keyword and its arguments. `Key value`,
/// `Key=value` and quoted arguments are understood; comments are not
/// lines.
fn lines(text: &str) -> impl Iterator<Item = (String, Vec<String>)> + '_ {
    text.lines().filter_map(|line| {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return None;
        }
        let split = line
            .find(|c: char| c.is_whitespace() || c == '=')
            .unwrap_or(line.len());
        let key = line[..split].to_ascii_lowercase();
        let rest = line[split..].trim_start_matches(|c: char| c.is_whitespace() || c == '=');
        Some((key, words(rest)))
    })
}

/// Arguments split at spaces, `"quoted parts"` kept whole.
fn words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut any = false;
    for c in text.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            '#' if !quoted && cur.is_empty() => break,
            c if c.is_whitespace() && !quoted => {
                if any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(cur);
    }
    out
}

/// Whether a `Host` argument is a plain name, not a pattern.
fn plain(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['*', '?', '!', '%'])
        && !name.chars().any(|c| c.is_control() || c.is_whitespace())
}

/// The hosts in one file's text, and the files it includes (as written).
pub fn parse(text: &str) -> (Vec<String>, Vec<String>) {
    let mut hosts = Vec::new();
    let mut includes = Vec::new();
    let mut in_match = false;
    for (key, args) in lines(text) {
        match key.as_str() {
            "host" => {
                in_match = false;
                for a in args {
                    if plain(&a) && !hosts.contains(&a) && hosts.len() < MAX_HOSTS {
                        hosts.push(a);
                    }
                }
            }
            "match" => in_match = true,
            // An Include inside a Match applies only when it matches.
            "include" if !in_match => includes.extend(args),
            _ => {}
        }
    }
    (hosts, includes)
}

/// The files an `Include` argument names: relative ones are in `~/.ssh`,
/// `~` is the home folder, and `*` and `?` in the last part match names.
fn expand(arg: &str, ssh_dir: &Path, home: &Path) -> Vec<PathBuf> {
    let path = match arg.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => {
            let p = Path::new(arg);
            if p.is_absolute() {
                p.to_owned()
            } else {
                ssh_dir.join(p)
            }
        }
    };
    let Some(pattern) = path.file_name().and_then(|n| n.to_str()) else {
        return Vec::new();
    };
    if !pattern.contains(['*', '?']) {
        return vec![path];
    }
    let Some(dir) = path.parent() else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_str().is_some_and(|n| glob(pattern, n)))
        .map(|e| e.path())
        .collect();
    found.sort();
    found
}

/// `*` and `?` matching, as ssh does for file names.
fn glob(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    let (mut pi, mut ni) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ni;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// Every host in `config` and what it includes, in order, each once.
pub fn hosts_in(config: &Path, home: &Path) -> Vec<String> {
    let ssh_dir = config
        .parent()
        .map_or_else(|| home.join(".ssh"), Path::to_owned);
    let mut out = Vec::new();
    let mut seen = Vec::new();
    walk(config, &ssh_dir, home, 0, &mut out, &mut seen);
    out
}

fn walk(
    file: &Path,
    ssh_dir: &Path,
    home: &Path,
    depth: usize,
    out: &mut Vec<String>,
    seen: &mut Vec<PathBuf>,
) {
    if depth > MAX_DEPTH || seen.iter().any(|s| s == file) {
        return;
    }
    seen.push(file.to_owned());
    let Ok(text) = std::fs::read_to_string(file) else {
        return;
    };
    let (hosts, includes) = parse(&text);
    for h in hosts {
        if !out.contains(&h) && out.len() < MAX_HOSTS {
            out.push(h);
        }
    }
    for inc in includes {
        for f in expand(&inc, ssh_dir, home) {
            walk(&f, ssh_dir, home, depth + 1, out, seen);
        }
    }
}

/// The user's hosts: `~/.ssh/config` and its includes.
pub fn user_hosts() -> Vec<String> {
    let Some(home) = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
    else {
        return Vec::new();
    };
    hosts_in(&home.join(".ssh").join("config"), &home)
}

/// Whether `host` is safe to give `ssh` as its destination argument: not
/// an option, no spaces or control characters.
pub fn valid_host(host: &str) -> bool {
    plain(host) && !host.starts_with('-') && host.len() <= 255
}

/// `hosts` with the ones in `recent` (newest first) first.
pub fn recent_first(hosts: Vec<String>, recent: &[String]) -> Vec<String> {
    let mut out: Vec<String> = recent
        .iter()
        .filter(|r| hosts.contains(r))
        .cloned()
        .collect();
    out.extend(hosts.into_iter().filter(|h| !recent.contains(h)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_patterns_and_match_blocks() {
        let (hosts, includes) = parse(
            "# mine\n\
             Host web db \"lab box\"\n  User me\n\
             Host *.internal !bad\n\
             Host=jump\n\
             Match host x\n  Include matched.conf\n\
             Host after\n\
             Include conf.d/*.conf extra\n\
             host web\n",
        );
        assert_eq!(hosts, vec!["web", "db", "jump", "after"]);
        assert_eq!(includes, vec!["conf.d/*.conf", "extra"]);
    }

    #[test]
    fn includes_are_followed_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ssh = dir.path().join(".ssh");
        std::fs::create_dir_all(ssh.join("conf.d")).expect("mkdir");
        std::fs::write(
            ssh.join("config"),
            "Include conf.d/*.conf\nInclude ~/.ssh/config\nHost main\n",
        )
        .expect("write");
        std::fs::write(ssh.join("conf.d/a.conf"), "Host alpha\n").expect("write");
        std::fs::write(ssh.join("conf.d/b.conf"), "Host beta main\n").expect("write");
        std::fs::write(ssh.join("conf.d/skip.txt"), "Host nope\n").expect("write");
        let hosts = hosts_in(&ssh.join("config"), dir.path());
        assert_eq!(hosts, vec!["main", "alpha", "beta"]);
    }

    #[test]
    fn globs_hosts_and_recent() {
        assert!(glob("*.conf", "a.conf"));
        assert!(glob("a?c", "abc"));
        assert!(!glob("*.conf", "a.txt"));
        assert!(glob("*", ""));
        assert!(valid_host("web-1.example.org"));
        assert!(!valid_host("-oProxyCommand=x"));
        assert!(!valid_host("a b"));
        assert_eq!(
            recent_first(
                vec!["a".into(), "b".into(), "c".into()],
                &["c".into(), "gone".into()]
            ),
            vec!["c", "a", "b"]
        );
    }
}
