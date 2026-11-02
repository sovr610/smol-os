// SPDX-License-Identifier: GPL-3.0-or-later
//! Repository automation for TermOS, run as `cargo xtask <command>`.
//!
//! Commands:
//! - `ci`          — everything CI checks: fmt, clippy, tests, repo checks, cargo-deny.
//! - `check-todos` — repo checks only: work-marker format, ROADMAP sync, SPDX headers.
//! - `todos`       — print the open work markers as the ROADMAP table
//!   (`--write` updates ROADMAP.md in place).
//! - `perf [secs]`  — performance budgets on an optimized build (Linux):
//!   keystroke latency, idle CPU over `secs` (default 60), memory, floods.
//! - `widgets`     — build the sample WASM widget and refresh the test
//!   fixture `crates/termos-shell/tests/fixtures/pomodoro.wasm`.
//! - `image build|boot <target>` — bare-metal images with Buildroot and
//!   their QEMU boot test (Linux; see termos-image, D-074 to D-076).

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
  perf [SECS]     Check the performance budgets on a release build (Linux);
                  idle CPU is measured over SECS seconds (default 60)
  widgets         Build the sample WASM widget (needs the wasm32-wasip2
                  target) and refresh its test fixture
  image build TARGET [--legal-info]
                  Build a bare-metal image with Buildroot (Linux)
  image boot TARGET [--firmware uefi|bios|all]
                  Boot it in QEMU: ready marker, screenshot, memory
  image list      The image targets
  help            Show this message
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("ci") => ci(),
        Some("check-todos") => check_repo(),
        Some("todos") => todos(&args[1..]),
        Some("perf") => perf(args.get(1).map(String::as_str)),
        Some("widgets") => widgets(),
        Some("image") => image(&args[1..]),
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
    cross_clippy(&root, WINDOWS_TARGET, cfg!(windows))?;
    cross_clippy(&root, MACOS_TARGET, cfg!(target_os = "macos"))?;
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

const WINDOWS_TARGET: &str = "x86_64-pc-windows-msvc";
const MACOS_TARGET: &str = "aarch64-apple-darwin";

/// Crates whose tests are also checked for the other targets. The rest
/// would pull in benchmark dependencies that need a C compiler for that
/// target. The WASM widgets are left out the same way (wasmtime builds C
/// for Windows): the native CI jobs check them.
const CROSS_TEST_CRATES: &[&str] = &[
    "termos-ai",
    "termos-core",
    "termos-devkit",
    "termos-hw",
    "termos-models",
    "termos-term",
    "termos-wm",
    "termos-plugin",
    "termos-apps",
    "termos-shell",
    "termos-cli",
];

/// Clippy for another platform's target, when its standard library is
/// installed, so code for Windows or macOS is checked without such a
/// machine. `native` skips it where the plain clippy run covers it.
fn cross_clippy(root: &Path, target: &str, native: bool) -> Result<()> {
    if native {
        return Ok(());
    }
    if !target_installed(target) {
        println!(
            "xtask: SKIPPED clippy for {target}: the target is not installed \
             (CI runs it; install with `rustup target add {target}`)"
        );
        return Ok(());
    }
    let common = [
        "--no-default-features",
        "--locked",
        "--target",
        target,
        "--",
        "-D",
        "warnings",
    ];
    let mut args = vec!["clippy", "--workspace", "--lib", "--bins"];
    args.extend(common);
    run_cargo(root, &args)?;
    let mut args = vec!["clippy"];
    for krate in CROSS_TEST_CRATES {
        args.extend(["-p", krate]);
    }
    args.push("--all-targets");
    args.extend(common);
    run_cargo(root, &args)
}

/// Whether the standard library for `target` is installed.
fn target_installed(target: &str) -> bool {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let Ok(out) = Command::new(rustc)
        .args(["--print", "target-libdir", "--target", target])
        .output()
    else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    let dir = String::from_utf8_lossy(&out.stdout);
    std::fs::read_dir(dir.trim()).is_ok_and(|mut entries| {
        entries.any(|e| e.is_ok_and(|e| e.file_name().to_string_lossy().starts_with("libstd-")))
    })
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

/// The performance budgets (DECISIONS.md D-019) on an optimized build.
fn perf(idle_secs: Option<&str>) -> Result<()> {
    let secs = idle_secs.unwrap_or("60");
    if secs.parse::<u64>().map_or(true, |s| s == 0) {
        return Err(format!("`{secs}` is not a number of seconds").into());
    }
    let root = repo::root();
    let args = [
        "test",
        "--release",
        "--locked",
        "-p",
        "termos-cli",
        "--test",
        "perf",
        "--",
        "--nocapture",
    ];
    println!(
        "xtask: $ TERMOS_PERF_IDLE_SECS={secs} cargo {}",
        args.join(" ")
    );
    let status = Command::new(cargo())
        .args(args)
        .env("TERMOS_PERF_IDLE_SECS", secs)
        .current_dir(root)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("performance budgets not met ({status})").into())
    }
}

/// `cargo xtask image …`.
#[cfg(unix)]
fn image(args: &[String]) -> Result<()> {
    use termos_image::targets::{self, Firmware};
    use termos_image::{boot, build};
    let paths = build::Paths::new(&repo::root());
    let (cmd, rest) = args.split_first().ok_or("image: build, boot or list")?;
    if cmd == "list" {
        for t in targets::TARGETS {
            let how = if t.qemu.is_some() {
                "built and booted in CI"
            } else {
                "a real board"
            };
            println!("{:<12} from Buildroot's {} ({how})", t.name, t.base);
        }
        return Ok(());
    }
    let name = rest
        .first()
        .ok_or("image: which target? (`cargo xtask image list`)")?;
    let t = targets::find(name)?;
    let flag = |f: &str| rest.iter().any(|a| a == f);
    let value = |f: &str| {
        rest.iter()
            .position(|a| a == f)
            .and_then(|i| rest.get(i + 1))
    };
    match cmd.as_str() {
        "build" => {
            let images = build::build(&paths, t, flag("--legal-info"))?;
            println!("xtask: the image is {}", images.join("disk.img").display());
            Ok(())
        }
        "boot" => {
            let q = t
                .qemu
                .ok_or_else(|| format!("{} is a real board: no QEMU boot", t.name))?;
            let firmwares: Vec<Firmware> = match value("--firmware").map(String::as_str) {
                None | Some("all") => q.firmwares.to_vec(),
                Some("uefi") => vec![Firmware::Uefi],
                Some("bios") => vec![Firmware::Bios],
                Some(other) => {
                    return Err(format!("no firmware `{other}`: uefi, bios or all").into());
                }
            };
            let dir = paths.work.join(format!("{}-boot", t.name));
            let mut failed = Vec::new();
            for f in firmwares {
                // Each firmware gets its turn even when another failed.
                let r = match boot::boot(&paths.images(t), t, f, &dir) {
                    Ok(r) => r,
                    Err(e) => {
                        failed.push(format!("{} ({}): {e}", t.name, f.name()));
                        continue;
                    }
                };
                println!(
                    "xtask: {} ({}): ready after {:.1}s; screen: {} colours, status bar {:?}; {} MiB of {} MiB in use",
                    t.name,
                    f.name(),
                    r.ready_after.as_secs_f32(),
                    r.screen.colours,
                    r.screen.status_bar,
                    r.memory.used_kb / 1024,
                    r.memory.total_kb / 1024
                );
                failed.extend(
                    r.problems()
                        .into_iter()
                        .map(|p| format!("{} ({}): {p}", t.name, f.name())),
                );
            }
            println!("xtask: logs and screenshots in {}", dir.display());
            if failed.is_empty() {
                Ok(())
            } else {
                Err(failed.join("\n").into())
            }
        }
        other => Err(format!("unknown image command `{other}`: build, boot or list").into()),
    }
}

#[cfg(not(unix))]
fn image(_: &[String]) -> Result<()> {
    Err("images are built and booted on Linux (Buildroot and QEMU)".into())
}

fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| String::from("cargo"))
}

const WASM_TARGET: &str = "wasm32-wasip2";
const SAMPLE_WIDGET: &str = "examples/widgets/pomodoro";
const WIDGET_FIXTURE: &str = "crates/termos-shell/tests/fixtures/pomodoro.wasm";

/// Builds the sample widget for WASI and copies it to the test fixture
/// (review and commit the result).
fn widgets() -> Result<()> {
    let root = repo::root();
    if !target_installed(WASM_TARGET) {
        return Err(format!("install the target first: rustup target add {WASM_TARGET}").into());
    }
    let manifest = format!("{SAMPLE_WIDGET}/Cargo.toml");
    run_cargo(
        &root,
        &[
            "build",
            "--release",
            "--locked",
            "--target",
            WASM_TARGET,
            "--manifest-path",
            &manifest,
        ],
    )?;
    let built = root.join(format!(
        "{SAMPLE_WIDGET}/target/{WASM_TARGET}/release/termos_widget_pomodoro.wasm"
    ));
    std::fs::copy(&built, root.join(WIDGET_FIXTURE))?;
    println!(
        "xtask: copied {} ({} bytes) to {WIDGET_FIXTURE}",
        built.display(),
        std::fs::metadata(&built)?.len()
    );
    Ok(())
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
