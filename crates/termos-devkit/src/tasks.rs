// SPDX-License-Identifier: GPL-3.0-or-later
//! Build, run and test for a project (ARCHITECTURE.md §10): found from the
//! files in its folder, and overridable or extended with `[tasks]` in a
//! `termos.toml` there.
//!
//! The nearest folder up from where you are that has a project file is
//! the project. Commands run in a shell (`sh -c`, `cmd /C` on Windows) in
//! that folder, inside a TermOS pane, where mise's shims make the
//! project's tool versions current.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A project and its tasks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub root: PathBuf,
    /// What it was recognised as (`cargo`, `go`, `npm`, …), or `custom`.
    pub kind: &'static str,
    /// Task name → shell command.
    pub tasks: BTreeMap<String, String>,
}

fn exists(dir: &Path, name: &str) -> bool {
    dir.join(name).exists()
}

fn any_with_ext(dir: &Path, ext: &str) -> bool {
    std::fs::read_dir(dir).is_ok_and(|rd| {
        rd.flatten()
            .any(|e| e.path().extension().is_some_and(|x| x == ext))
    })
}

/// `scripts` in a package.json.
fn npm_scripts(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("package.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| {
            v.get("scripts")?
                .as_object()
                .map(|o| o.keys().cloned().collect())
        })
        .unwrap_or_default()
}

/// `project(NAME …)` in a CMakeLists.txt.
fn cmake_project(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("CMakeLists.txt")).ok()?;
    let start = text.find("project(")? + "project(".len();
    let rest = &text[start..];
    let name: String = rest
        .trim_start()
        .chars()
        .take_while(|c| !c.is_whitespace() && *c != ')')
        .collect();
    (!name.is_empty()).then_some(name)
}

fn tasks(pairs: &[(&str, String)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

/// What `dir` is, if it is a project TermOS knows.
pub fn detect_here(dir: &Path) -> Option<(&'static str, BTreeMap<String, String>)> {
    let s = |x: &str| x.to_owned();
    let windows = cfg!(windows);
    if exists(dir, "Cargo.toml") {
        return Some((
            "cargo",
            tasks(&[
                ("build", s("cargo build")),
                ("run", s("cargo run")),
                ("test", s("cargo test")),
            ]),
        ));
    }
    if exists(dir, "go.mod") {
        return Some((
            "go",
            tasks(&[
                ("build", s("go build ./...")),
                ("run", s("go run .")),
                ("test", s("go test ./...")),
            ]),
        ));
    }
    if exists(dir, "deno.json") || exists(dir, "deno.jsonc") {
        return Some((
            "deno",
            tasks(&[("run", s("deno task start")), ("test", s("deno test"))]),
        ));
    }
    if exists(dir, "package.json") {
        // Bun when it has Bun's lock file or its scripts use Bun.
        let text = std::fs::read_to_string(dir.join("package.json")).unwrap_or_default();
        let bun = exists(dir, "bun.lock") || exists(dir, "bun.lockb") || text.contains("bun ");
        let runner = if bun { "bun" } else { "npm" };
        let mut t = Vec::new();
        if npm_scripts(dir).iter().any(|x| x == "build") {
            t.push(("build", format!("{runner} run build")));
        }
        t.push(("run", format!("{runner} run start")));
        t.push(("test", format!("{runner} test")));
        return Some((runner, tasks(&t)));
    }
    if exists(dir, "pyproject.toml") || exists(dir, "main.py") || exists(dir, "setup.py") {
        let py = if windows { "python" } else { "python3" };
        let mut t = vec![("test", format!("{py} -m unittest"))];
        if exists(dir, "main.py") {
            t.push(("run", format!("{py} main.py")));
        }
        return Some(("python", tasks(&t)));
    }
    if exists(dir, "CMakeLists.txt") {
        let mut t = vec![
            ("build", s("cmake -S . -B build && cmake --build build")),
            ("test", s("ctest --test-dir build")),
        ];
        if let Some(name) = cmake_project(dir) {
            let exe = if windows {
                format!("build\\Debug\\{name}.exe")
            } else {
                format!("./build/{name}")
            };
            t.push((
                "run",
                format!("cmake -S . -B build && cmake --build build && {exe}"),
            ));
        }
        return Some(("cmake", tasks(&t)));
    }
    if any_with_ext(dir, "csproj") || any_with_ext(dir, "sln") {
        return Some((
            "dotnet",
            tasks(&[
                ("build", s("dotnet build")),
                ("run", s("dotnet run")),
                ("test", s("dotnet test")),
            ]),
        ));
    }
    if exists(dir, "build.gradle")
        || exists(dir, "build.gradle.kts")
        || exists(dir, "settings.gradle.kts")
    {
        let g = if exists(dir, "gradlew") {
            if windows {
                s("gradlew.bat")
            } else {
                s("./gradlew")
            }
        } else {
            s("gradle")
        };
        return Some((
            "gradle",
            tasks(&[
                ("build", format!("{g} build")),
                ("run", format!("{g} run")),
                ("test", format!("{g} test")),
            ]),
        ));
    }
    if exists(dir, "pom.xml") {
        return Some((
            "maven",
            tasks(&[("build", s("mvn -q package")), ("test", s("mvn -q test"))]),
        ));
    }
    if exists(dir, "build.zig") {
        return Some((
            "zig",
            tasks(&[
                ("build", s("zig build")),
                ("run", s("zig build run")),
                ("test", s("zig build test")),
            ]),
        ));
    }
    if exists(dir, "Main.java") {
        return Some((
            "java",
            tasks(&[
                ("build", s("javac -d out Main.java")),
                ("run", s("java Main.java")),
            ]),
        ));
    }
    if exists(dir, "main.kt") {
        let build = s("kotlinc main.kt -include-runtime -d main.jar");
        return Some((
            "kotlin",
            tasks(&[
                ("build", build.clone()),
                ("run", format!("{build} && java -jar main.jar")),
            ]),
        ));
    }
    if exists(dir, "main.lua") {
        return Some(("lua", tasks(&[("run", s("lua main.lua"))])));
    }
    if exists(dir, "main.sh") {
        return Some((
            "shell",
            tasks(&[("run", s("sh main.sh")), ("test", s("shellcheck main.sh"))]),
        ));
    }
    None
}

/// `[tasks]` from a `termos.toml` in `dir`.
pub fn custom_tasks(dir: &Path) -> Result<Option<BTreeMap<String, String>>, String> {
    let path = dir.join("termos.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let table: toml::Table =
        toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let Some(tasks) = table.get("tasks") else {
        return Ok(Some(BTreeMap::new()));
    };
    let tasks = tasks
        .as_table()
        .ok_or_else(|| format!("{}: [tasks] must be a table", path.display()))?;
    let mut out = BTreeMap::new();
    for (k, v) in tasks {
        let cmd = v
            .as_str()
            .ok_or_else(|| format!("{}: tasks.{k} must be a command (a string)", path.display()))?;
        out.insert(k.clone(), cmd.to_owned());
    }
    Ok(Some(out))
}

/// The project around `start`: the nearest folder up that is one.
pub fn find(start: &Path) -> Result<Option<Project>, String> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let custom = custom_tasks(d)?;
        let detected = detect_here(d);
        if custom.is_some() || detected.is_some() {
            let (kind, mut tasks) = detected.unwrap_or(("custom", BTreeMap::new()));
            if let Some(c) = custom {
                tasks.extend(c);
            }
            return Ok(Some(Project {
                root: d.to_owned(),
                kind,
                tasks,
            }));
        }
        dir = d.parent();
    }
    Ok(None)
}

/// The shell command line to run `command`: (program, args).
pub fn shell(command: &str) -> (String, Vec<String>) {
    if cfg!(windows) {
        ("cmd".to_owned(), vec!["/C".to_owned(), command.to_owned()])
    } else {
        (
            "/bin/sh".to_owned(),
            vec!["-c".to_owned(), command.to_owned()],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        for (name, text) in files {
            let p = dir.path().join(name);
            std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
            std::fs::write(p, text).expect("write");
        }
        dir
    }

    #[test]
    fn projects_are_recognised_from_their_files() {
        let d = project(&[("Cargo.toml", ""), ("src/main.rs", "")]);
        let p = find(&d.path().join("src")).expect("ok").expect("a project");
        assert_eq!((p.kind, p.root.as_path()), ("cargo", d.path()));
        assert_eq!(p.tasks["test"], "cargo test");

        let d = project(&[(
            "package.json",
            r#"{"scripts": {"build": "tsc", "start": "node ."}}"#,
        )]);
        let p = find(d.path()).expect("ok").expect("a project");
        assert_eq!(p.kind, "npm");
        assert_eq!(p.tasks["build"], "npm run build");

        let d = project(&[("package.json", "{}"), ("bun.lock", "")]);
        assert_eq!(find(d.path()).expect("ok").expect("bun").kind, "bun");

        let d = project(&[(
            "CMakeLists.txt",
            "cmake_minimum_required(VERSION 3.16)\nproject(hello C)\n",
        )]);
        let p = find(d.path()).expect("ok").expect("cmake");
        assert!(p.tasks["run"].ends_with(if cfg!(windows) {
            "hello.exe"
        } else {
            "./build/hello"
        }));

        let d = project(&[("app.csproj", "")]);
        assert_eq!(find(d.path()).expect("ok").expect("dotnet").kind, "dotnet");

        let d = project(&[("notes.txt", "")]);
        // Nothing here (the temporary folder's parents are not projects).
        let found = find(d.path()).expect("ok");
        assert!(found.is_none_or(|p| !p.root.starts_with(d.path())));
    }

    #[test]
    fn termos_toml_overrides_and_adds() {
        let d = project(&[
            ("go.mod", "module x\n"),
            (
                "termos.toml",
                "[tasks]\ntest = \"go test -race ./...\"\nlint = \"go vet ./...\"\n",
            ),
        ]);
        let p = find(d.path()).expect("ok").expect("a project");
        assert_eq!(p.kind, "go");
        assert_eq!(p.tasks["test"], "go test -race ./...");
        assert_eq!(p.tasks["lint"], "go vet ./...");
        assert_eq!(p.tasks["build"], "go build ./...");
        // Only termos.toml: a custom project.
        let d = project(&[("termos.toml", "[tasks]\nbuild = \"make\"\n")]);
        let p = find(d.path()).expect("ok").expect("a project");
        assert_eq!((p.kind, p.tasks["build"].as_str()), ("custom", "make"));
        // Mistakes are reported.
        let d = project(&[("termos.toml", "[tasks]\nbuild = 3\n")]);
        assert!(find(d.path()).is_err_and(|e| e.contains("tasks.build")));
    }
}
