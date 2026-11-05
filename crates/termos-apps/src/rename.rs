// SPDX-License-Identifier: GPL-3.0-or-later
//! Bulk rename (M13c, D-122), as `vidir` does it: the names go into a
//! file, one numbered line each, the editor changes them, and TermOS
//! checks the result, shows the changes and renames.
//!
//! Renames are two-phase, each name first to a temporary one in the same
//! folder, so swaps work and nothing is overwritten; a failure puts back
//! what was done. The renames made are kept for `u` (undo).

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// The file the editor gets: a note, then `N<tab>name` per entry.
pub fn names_file(names: &[String]) -> String {
    let mut out = String::from(
        "# Change the names after the tabs, save and close the editor, then press Enter in Files.\n\
         # Keep every line and its number. Lines starting with # are ignored.\n",
    );
    for (i, n) in names.iter().enumerate() {
        out.push_str(&format!("{}\t{n}\n", i + 1));
    }
    out
}

/// Why an edited list cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error("line {0}: not `number<tab>name`")]
    BadLine(usize),
    #[error("entry {0} is listed twice")]
    Twice(usize),
    #[error("entry {0} is missing: keep every line")]
    Missing(usize),
    #[error("entry {0}: a name cannot be empty or contain / or \\")]
    BadName(usize),
    #[error("{0} is given twice")]
    Clash(String),
    #[error("{0} exists already")]
    Exists(String),
}

/// The renames an edited list asks for: (old name, new name), only those
/// that change. `taken` says whether a name exists in the folder.
pub fn plan(
    old: &[String],
    edited: &str,
    taken: &dyn Fn(&str) -> bool,
) -> Result<Vec<(String, String)>, PlanError> {
    let mut new: BTreeMap<usize, String> = BTreeMap::new();
    for (i, line) in edited.lines().enumerate() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let (n, name) = line.split_once('\t').ok_or(PlanError::BadLine(i + 1))?;
        let n: usize = n.trim().parse().map_err(|_| PlanError::BadLine(i + 1))?;
        if n == 0 || n > old.len() {
            return Err(PlanError::BadLine(i + 1));
        }
        if new.insert(n, name.to_owned()).is_some() {
            return Err(PlanError::Twice(n));
        }
    }
    for n in 1..=old.len() {
        let name = new.get(&n).ok_or(PlanError::Missing(n))?;
        if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', '\0']) {
            return Err(PlanError::BadName(n));
        }
    }
    let mut seen = HashSet::new();
    for name in new.values() {
        if !seen.insert(name.as_str()) {
            return Err(PlanError::Clash(name.clone()));
        }
    }
    let renamed: HashSet<&str> = old.iter().map(String::as_str).collect();
    let mut out = Vec::new();
    for (n, name) in &new {
        let from = &old[n - 1];
        if from == name {
            continue;
        }
        // A name that exists is free only if its own entry is renamed (or
        // it is this entry, in other letter case, on a file system that
        // ignores case; phase two checks again).
        if taken(name) && !renamed.contains(name.as_str()) && !name.eq_ignore_ascii_case(from) {
            return Err(PlanError::Exists(name.clone()));
        }
        out.push((from.clone(), name.clone()));
    }
    Ok(out)
}

/// Does the renames in `dir`, all or none. The renames made, for undo.
pub fn apply(dir: &Path, changes: &[(String, String)]) -> Result<Vec<(String, String)>, String> {
    let tag = format!(".termos-rename-{}", std::process::id());
    let temp = |i: usize| dir.join(format!("{tag}-{i}"));
    // Phase one: every source to a temporary name.
    let mut moved: Vec<usize> = Vec::new();
    for (i, (from, _)) in changes.iter().enumerate() {
        if let Err(e) = std::fs::rename(dir.join(from), temp(i)) {
            for j in moved.into_iter().rev() {
                let _ = std::fs::rename(temp(j), dir.join(&changes[j].0));
            }
            return Err(format!("cannot rename {from}: {e}"));
        }
        moved.push(i);
    }
    // Phase two: each temporary name to its new one, never over a file.
    let mut done: Vec<usize> = Vec::new();
    for (i, (_, to)) in changes.iter().enumerate() {
        let target: PathBuf = dir.join(to);
        let result = if target.exists() {
            Err(std::io::Error::other("it exists"))
        } else {
            std::fs::rename(temp(i), &target)
        };
        if let Err(e) = result {
            for j in done.into_iter().rev() {
                let _ = std::fs::rename(dir.join(&changes[j].1), temp(j));
            }
            for (j, (from, _)) in changes.iter().enumerate() {
                let _ = std::fs::rename(temp(j), dir.join(from));
            }
            return Err(format!("cannot rename to {to}: {e}"));
        }
        done.push(i);
    }
    Ok(changes.to_vec())
}

/// The renames that undo `done`.
pub fn undo_of(done: &[(String, String)]) -> Vec<(String, String)> {
    done.iter().map(|(a, b)| (b.clone(), a.clone())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn the_file_round_trips_and_changes_are_found() {
        let old = names(&["a.txt", "b.txt", "c.txt"]);
        let file = names_file(&old);
        assert_eq!(plan(&old, &file, &|_| true), Ok(Vec::new()));
        let edited = file.replace("1\ta.txt", "1\tx.txt");
        assert_eq!(
            plan(&old, &edited, &|n| old.iter().any(|o| o == n)),
            Ok(vec![("a.txt".into(), "x.txt".into())])
        );
    }

    #[test]
    fn swaps_are_allowed_clashes_and_gaps_are_not() {
        let old = names(&["a", "b"]);
        let exists = |n: &str| ["a", "b", "c"].contains(&n);
        assert_eq!(
            plan(&old, "1\tb\n2\ta\n", &exists),
            Ok(vec![("a".into(), "b".into()), ("b".into(), "a".into())])
        );
        assert_eq!(
            plan(&old, "1\tc\n2\tb\n", &exists),
            Err(PlanError::Exists("c".into()))
        );
        assert_eq!(
            plan(&old, "1\tx\n2\tx\n", &exists),
            Err(PlanError::Clash("x".into()))
        );
        assert_eq!(plan(&old, "1\tx\n", &exists), Err(PlanError::Missing(2)));
        assert_eq!(
            plan(&old, "1\tx\n1\ty\n", &exists),
            Err(PlanError::Twice(1))
        );
        assert_eq!(
            plan(&old, "1\tx/y\n2\tb\n", &exists),
            Err(PlanError::BadName(1))
        );
        assert_eq!(plan(&old, "one\tx\n", &exists), Err(PlanError::BadLine(1)));
    }

    #[test]
    fn a_swap_on_disk_and_its_undo() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a"), "A").expect("write");
        std::fs::write(dir.path().join("b"), "B").expect("write");
        let done = apply(
            dir.path(),
            &[("a".into(), "b".into()), ("b".into(), "a".into())],
        )
        .expect("renamed");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a")).expect("a"),
            "B"
        );
        apply(dir.path(), &undo_of(&done)).expect("undone");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a")).expect("a"),
            "A"
        );
        // A target that appeared meanwhile: nothing changes.
        std::fs::write(dir.path().join("c"), "C").expect("write");
        assert!(apply(dir.path(), &[("a".into(), "c".into())]).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a")).expect("a"),
            "A"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("c")).expect("c"),
            "C"
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]
        /// Whatever names the edit gives, a plan that passes the checks
        /// renames without losing a file, and what it did undoes.
        #[test]
        fn no_file_is_ever_lost(new in proptest::collection::vec(0u8..6, 4)) {
            let dir = tempfile::tempdir().expect("tempdir");
            let old: Vec<String> = (0..4).map(|i| format!("f{i}")).collect();
            for o in &old {
                std::fs::write(dir.path().join(o), o).expect("write");
            }
            let edited: String = new
                .iter()
                .enumerate()
                .map(|(i, n)| format!("{}\tf{n}\n", i + 1))
                .collect();
            let exists = |n: &str| dir.path().join(n).exists();
            if let Ok(changes) = plan(&old, &edited, &exists) {
                let done = apply(dir.path(), &changes).expect("renamed");
                let mut contents: Vec<String> = std::fs::read_dir(dir.path())
                    .expect("read")
                    .flatten()
                    .map(|e| std::fs::read_to_string(e.path()).expect("read"))
                    .collect();
                contents.sort();
                prop_assert_eq!(&contents, &old);
                apply(dir.path(), &undo_of(&done)).expect("undone");
                for o in &old {
                    prop_assert_eq!(std::fs::read_to_string(dir.path().join(o)).expect("read"), o.clone());
                }
            }
        }
    }
}
