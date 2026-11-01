// SPDX-License-Identifier: GPL-3.0-or-later
//! Repository automation for TermOS, run as `cargo xtask <command>`.
//!
//! Commands:
//! - `ci`          — everything CI checks: fmt, clippy, tests, repo checks, cargo-deny.
//! - `check-todos` — repo checks only: work-marker format, ROADMAP sync, SPDX headers.
//! - `todos`       — print the open work markers as the ROADMAP table
//!   (`--write` updates ROADMAP.md in place).

mod markers;
mod repo;
mod spdx;

use std::error::Error;
use std::path::Path;
use std::process::{Command, ExitCode};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const HELP: &str = "\
Usage: cargo xtask <command>

Commands:
  ci              Run fmt, clippy, tests, check-todos and cargo-deny (as CI does)
  check-todos     Check work-marker format, ROADMAP.md sync and SPDX headers
  todos [--write] Print the open-work table; --write updates ROADMAP.md
  help            Show this message
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("ci") => ci(),
        Some("check-todos") => check_repo(),
        Some("todos") => todos(&args[1..]),
        Some("help" | "-h" | "--help") | None => {
            print!("{HELP}");
            Ok(())
        }
        Some(other) => Err(format!("unknown command `{other}`\n\n{HELP}").into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("xtask: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Runs the same gates as `.github/workflows/ci.yml` (minus the cross build).
fn ci() -> Result<()> {
    let root = repo::root();
    run_cargo(&root, &["fmt", "--all", "--check"])?;
    run_cargo(
        &root,
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--all-features",
            "--locked",
            "--",
            "-D",
            "warnings",
        ],
    )?;
    run_cargo(
        &root,
        &["test", "--workspace", "--all-features", "--locked"],
    )?;
    check_repo()?;
    if cargo_subcommand_available(&root, "deny") {
        run_cargo(&root, &["deny", "check"])?;
    } else {
        println!(
            "xtask: SKIPPED `cargo deny check`: cargo-deny is not installed \
             (CI runs it; install with `cargo install --locked cargo-deny`)"
        );
    }
    println!("xtask: all CI checks passed");
    Ok(())
}

/// Work-marker format and ROADMAP sync, plus SPDX headers.
fn check_repo() -> Result<()> {
    let root = repo::root();
    let files = repo::files(&root)?;
    let mut problems = Vec::new();

    let scan = markers::scan_files(&root, &files)?;
    problems.extend(scan.violations.iter().map(ToString::to_string));

    let roadmap_path = root.join(markers::ROADMAP);
    let roadmap = std::fs::read_to_string(&roadmap_path)
        .map_err(|e| format!("cannot read {}: {e}", roadmap_path.display()))?;
    problems.extend(markers::check_milestones_exist(&scan.markers, &roadmap));
    match markers::current_table(&roadmap) {
        Some(table) if table == markers::render_table(&scan.markers) => {}
        Some(_) => problems.push(format!(
            "{}: the open-work table is stale; run `cargo xtask todos --write`",
            markers::ROADMAP
        )),
        None => problems.push(format!(
            "{}: missing the `{}` / `{}` block",
            markers::ROADMAP,
            markers::TABLE_BEGIN,
            markers::TABLE_END
        )),
    }

    problems.extend(spdx::check_files(&root, &files)?);

    if problems.is_empty() {
        println!(
            "xtask: repo checks passed ({} open work markers, {} files scanned)",
            scan.markers.len(),
            files.len()
        );
        Ok(())
    } else {
        for problem in &problems {
            eprintln!("  {problem}");
        }
        Err(format!("{} repo check problem(s)", problems.len()).into())
    }
}

fn todos(args: &[String]) -> Result<()> {
    let write = match args {
        [] => false,
        [flag] if flag == "--write" => true,
        _ => return Err(format!("unexpected arguments: {args:?}\n\n{HELP}").into()),
    };
    let root = repo::root();
    let files = repo::files(&root)?;
    let scan = markers::scan_files(&root, &files)?;
    let table = markers::render_table(&scan.markers);
    if write {
        let path = root.join(markers::ROADMAP);
        let roadmap = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let updated = markers::replace_table(&roadmap, &table).ok_or_else(|| {
            format!(
                "{}: missing the `{}` / `{}` block",
                markers::ROADMAP,
                markers::TABLE_BEGIN,
                markers::TABLE_END
            )
        })?;
        std::fs::write(&path, updated)?;
        println!(
            "xtask: wrote {} open work markers to {}",
            scan.markers.len(),
            markers::ROADMAP
        );
    } else {
        print!("{table}");
    }
    if !scan.violations.is_empty() {
        for v in &scan.violations {
            eprintln!("  {v}");
        }
        return Err(format!("{} malformed work marker(s)", scan.violations.len()).into());
    }
    Ok(())
}

fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| String::from("cargo"))
}

fn run_cargo(root: &Path, args: &[&str]) -> Result<()> {
    println!("xtask: $ cargo {}", args.join(" "));
    let status = Command::new(cargo())
        .args(args)
        .current_dir(root)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`cargo {}` failed ({status})", args.join(" ")).into())
    }
}

fn cargo_subcommand_available(root: &Path, name: &str) -> bool {
    Command::new(cargo())
        .args([name, "--version"])
        .current_dir(root)
        .output()
        .is_ok_and(|out| out.status.success())
}
