// SPDX-License-Identifier: GPL-3.0-or-later
//! The monitor's process view (M13c, D-122): every process, filtered by
//! name or command, sorted, or as a tree by parent; optionally only one
//! process and its descendants (a pane's).

use std::collections::{HashMap, HashSet};

use termos_hw::ProcessInfo;

/// How the list is ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sort {
    #[default]
    Cpu,
    Memory,
    Time,
    Pid,
    Name,
}

impl Sort {
    pub fn next(self) -> Self {
        match self {
            Sort::Cpu => Sort::Memory,
            Sort::Memory => Sort::Time,
            Sort::Time => Sort::Pid,
            Sort::Pid => Sort::Name,
            Sort::Name => Sort::Cpu,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Sort::Cpu => "CPU",
            Sort::Memory => "memory",
            Sort::Time => "time",
            Sort::Pid => "pid",
            Sort::Name => "name",
        }
    }

    fn cmp(self, a: &ProcessInfo, b: &ProcessInfo) -> std::cmp::Ordering {
        let by = match self {
            Sort::Cpu => b.cpu.total_cmp(&a.cpu),
            Sort::Memory => b.memory.cmp(&a.memory),
            Sort::Time => b.run_time.cmp(&a.run_time),
            Sort::Pid => a.pid.cmp(&b.pid),
            Sort::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        };
        by.then(a.pid.cmp(&b.pid))
    }
}

/// What the view shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct View {
    /// Text the name or command contains (case ignored), or a pid.
    pub filter: String,
    pub sort: Sort,
    pub tree: bool,
    /// Only this process and its descendants.
    pub root: Option<u32>,
}

/// One row: a process and how deep it is in the tree.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub depth: usize,
    pub info: ProcessInfo,
}

/// `root` and every process below it.
pub fn descendants(all: &[ProcessInfo], root: u32) -> HashSet<u32> {
    let mut kids: HashMap<u32, Vec<u32>> = HashMap::new();
    for p in all {
        if let Some(parent) = p.parent {
            kids.entry(parent).or_default().push(p.pid);
        }
    }
    let mut out = HashSet::new();
    let mut stack = vec![root];
    while let Some(pid) = stack.pop() {
        if out.insert(pid)
            && let Some(k) = kids.get(&pid)
        {
            stack.extend(k);
        }
    }
    out
}

fn matches(p: &ProcessInfo, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    if let Ok(pid) = filter.trim().parse::<u32>() {
        return p.pid == pid;
    }
    let f = filter.to_lowercase();
    p.name.to_lowercase().contains(&f) || p.command.to_lowercase().contains(&f)
}

/// The rows `view` shows of `all`.
pub fn rows(all: &[ProcessInfo], view: &View) -> Vec<Row> {
    let scope: Vec<&ProcessInfo> = match view.root {
        Some(r) => {
            let keep = descendants(all, r);
            all.iter().filter(|p| keep.contains(&p.pid)).collect()
        }
        None => all.iter().collect(),
    };
    if !view.tree {
        let mut out: Vec<&ProcessInfo> = scope
            .into_iter()
            .filter(|p| matches(p, &view.filter))
            .collect();
        out.sort_by(|a, b| view.sort.cmp(a, b));
        return out
            .into_iter()
            .map(|p| Row {
                depth: 0,
                info: p.clone(),
            })
            .collect();
    }
    // A tree: what matches and the processes above it.
    let by_pid: HashMap<u32, &ProcessInfo> = scope.iter().map(|p| (p.pid, *p)).collect();
    let mut keep: HashSet<u32> = HashSet::new();
    for p in &scope {
        if matches(p, &view.filter) {
            let mut at = Some(p.pid);
            while let Some(pid) = at {
                if !keep.insert(pid) {
                    break;
                }
                at = by_pid
                    .get(&pid)
                    .and_then(|q| q.parent)
                    .filter(|pp| by_pid.contains_key(pp));
            }
        }
    }
    let mut kids: HashMap<Option<u32>, Vec<&ProcessInfo>> = HashMap::new();
    for p in scope.iter().filter(|p| keep.contains(&p.pid)) {
        let parent = p.parent.filter(|pp| keep.contains(pp) && *pp != p.pid);
        kids.entry(parent).or_default().push(p);
    }
    for v in kids.values_mut() {
        v.sort_by(|a, b| view.sort.cmp(a, b));
    }
    let mut out = Vec::new();
    let mut stack: Vec<(usize, &ProcessInfo)> = kids
        .get(&None)
        .map(|v| v.iter().rev().map(|p| (0, *p)).collect())
        .unwrap_or_default();
    let mut seen = HashSet::new();
    while let Some((depth, p)) = stack.pop() {
        if !seen.insert(p.pid) {
            continue;
        }
        out.push(Row {
            depth,
            info: p.clone(),
        });
        if let Some(children) = kids.get(&Some(p.pid)) {
            stack.extend(children.iter().rev().map(|c| (depth + 1, *c)));
        }
    }
    out
}

/// `90s` as `1:30`, hours as `2h05`, days as `3d4h`.
pub fn run_time(secs: u64) -> String {
    match secs {
        0..3600 => format!("{}:{:02}", secs / 60, secs % 60),
        3600..86_400 => format!("{}h{:02}", secs / 3600, (secs % 3600) / 60),
        _ => format!("{}d{}h", secs / 86_400, (secs % 86_400) / 3600),
    }
}

/// Processes for tests of the views that show them.
#[cfg(test)]
pub(crate) mod tests_support {
    use termos_hw::ProcessInfo;

    pub(crate) fn info(pid: u32, parent: Option<u32>, name: &str) -> ProcessInfo {
        ProcessInfo {
            pid,
            parent,
            name: name.into(),
            user: Some("me".into()),
            cpu: 1.0,
            memory: u64::from(pid) * 1024,
            run_time: u64::from(pid),
            state: "Sleeping".into(),
            threads: Some(1),
            command: format!("/usr/bin/{name} --flag"),
            ports: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, parent: Option<u32>, name: &str, cpu: f32) -> ProcessInfo {
        ProcessInfo {
            pid,
            parent,
            name: name.into(),
            user: Some("me".into()),
            cpu,
            memory: u64::from(pid) * 1024,
            run_time: u64::from(pid),
            state: "Sleeping".into(),
            threads: Some(1),
            command: format!("/usr/bin/{name} --flag"),
            ports: Vec::new(),
        }
    }

    fn fixture() -> Vec<ProcessInfo> {
        vec![
            p(1, None, "init", 0.1),
            p(10, Some(1), "termos", 2.0),
            p(11, Some(10), "bash", 0.0),
            p(12, Some(11), "cargo", 90.0),
            p(13, Some(12), "rustc", 150.0),
            p(20, Some(1), "sshd", 0.0),
        ]
    }

    fn pids(rows: &[Row]) -> Vec<(u32, usize)> {
        rows.iter().map(|r| (r.info.pid, r.depth)).collect()
    }

    #[test]
    fn flat_sorted_and_filtered() {
        let all = fixture();
        let v = View::default();
        assert_eq!(pids(&rows(&all, &v))[..2], [(13, 0), (12, 0)]);
        let v = View {
            filter: "CARGO".into(),
            ..View::default()
        };
        assert_eq!(pids(&rows(&all, &v)), vec![(12, 0)]);
        let v = View {
            filter: "flag".into(),
            sort: Sort::Pid,
            ..View::default()
        };
        assert_eq!(rows(&all, &v).len(), 6, "the command matches too");
        let v = View {
            filter: "20".into(),
            ..View::default()
        };
        assert_eq!(pids(&rows(&all, &v)), vec![(20, 0)]);
    }

    #[test]
    fn a_tree_keeps_the_parents_of_what_matches() {
        let all = fixture();
        let v = View {
            tree: true,
            sort: Sort::Pid,
            ..View::default()
        };
        assert_eq!(
            pids(&rows(&all, &v)),
            vec![(1, 0), (10, 1), (11, 2), (12, 3), (13, 4), (20, 1)]
        );
        let v = View {
            tree: true,
            filter: "rustc".into(),
            ..View::default()
        };
        assert_eq!(
            pids(&rows(&all, &v)),
            vec![(1, 0), (10, 1), (11, 2), (12, 3), (13, 4)]
        );
    }

    #[test]
    fn a_panes_processes() {
        let all = fixture();
        let v = View {
            root: Some(11),
            tree: true,
            ..View::default()
        };
        assert_eq!(pids(&rows(&all, &v)), vec![(11, 0), (12, 1), (13, 2)]);
        assert_eq!(descendants(&all, 12), [12, 13].into_iter().collect());
        assert_eq!(run_time(90), "1:30");
        assert_eq!(run_time(7500), "2h05");
        assert_eq!(run_time(3 * 86_400 + 4 * 3600), "3d4h");
    }
}
