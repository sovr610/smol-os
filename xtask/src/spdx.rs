// SPDX-License-Identifier: GPL-3.0-or-later
//! Every Rust source file must start with the project's SPDX header.

use std::path::{Path, PathBuf};

use crate::Result;

pub(crate) const HEADER: &str = "// SPDX-License-Identifier: GPL-3.0-or-later";

/// Returns one problem description per `.rs` file lacking the header.
pub(crate) fn check_files(root: &Path, files: &[PathBuf]) -> Result<Vec<String>> {
    let mut problems = Vec::new();
    for rel in files
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e == "rs"))
    {
        let text = std::fs::read_to_string(root.join(rel))
            .map_err(|e| format!("cannot read {}: {e}", rel.display()))?;
        if !has_header(&text) {
            problems.push(format!(
                "{}:1: missing SPDX header (first line must be `{HEADER}`)",
                rel.display()
            ));
        }
    }
    Ok(problems)
}

fn has_header(text: &str) -> bool {
    text.lines()
        .next()
        .is_some_and(|line| line.trim_end() == HEADER)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_must_be_first_line() {
        assert!(has_header(&format!("{HEADER}\n//! docs\n")));
        assert!(!has_header(&format!("//! docs\n{HEADER}\n")));
        assert!(!has_header(""));
        assert!(!has_header("// SPDX-License-Identifier: MIT\n"));
    }
}
