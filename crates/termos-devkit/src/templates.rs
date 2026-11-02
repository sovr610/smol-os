// SPDX-License-Identifier: GPL-3.0-or-later
//! `termos create <template> [dir]`: a small working project per language,
//! with a `mise.toml` pinning its tools.
//!
//! The files are embedded, so a project builds the same today and next
//! year. Where a language's own project generator is the better source
//! (Zig's build API and .NET's project files change between versions),
//! the template runs it instead.

use std::path::{Path, PathBuf};

use crate::langs::{self, Language};

/// A project template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Template {
    pub name: &'static str,
    /// The language whose tools it pins.
    pub lang: &'static str,
    pub about: &'static str,
    /// Files: (path, contents); `{name}` is the project's name.
    pub files: &'static [(&'static str, &'static str)],
    /// A generator to run in the new folder instead (or before the
    /// files), through `mise exec`.
    pub generate: Option<&'static [&'static str]>,
}

pub const TEMPLATES: &[Template] = &[
    Template {
        name: "rust-cli",
        lang: "rust",
        about: "a Rust program built with Cargo",
        files: &[
            (
                "Cargo.toml",
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\n",
            ),
            (
                "src/main.rs",
                "fn main() {\n    println!(\"Hello from {name}!\");\n}\n",
            ),
            (".gitignore", "/target\n"),
        ],
        generate: None,
    },
    Template {
        name: "go-cli",
        lang: "go",
        about: "a Go module with a main package",
        files: &[
            ("go.mod", "module {name}\n\ngo 1.22\n"),
            (
                "main.go",
                "package main\n\nimport \"fmt\"\n\nfunc main() {\n\tfmt.Println(\"Hello from {name}!\")\n}\n",
            ),
            (
                "main_test.go",
                "package main\n\nimport \"testing\"\n\nfunc TestGreeting(t *testing.T) {\n\tif 1+1 != 2 {\n\t\tt.Fatal(\"arithmetic is broken\")\n\t}\n}\n",
            ),
        ],
        generate: None,
    },
    Template {
        name: "python-cli",
        lang: "python",
        about: "a Python script with a pyproject.toml",
        files: &[
            (
                "pyproject.toml",
                "[project]\nname = \"{name}\"\nversion = \"0.1.0\"\nrequires-python = \">=3.10\"\n",
            ),
            (
                "main.py",
                "def main() -> None:\n    print(\"Hello from {name}!\")\n\n\nif __name__ == \"__main__\":\n    main()\n",
            ),
            (
                "test_main.py",
                "import unittest\n\n\nclass TestMain(unittest.TestCase):\n    def test_runs(self) -> None:\n        import main\n\n        main.main()\n\n\nif __name__ == \"__main__\":\n    unittest.main()\n",
            ),
            (".gitignore", "__pycache__/\n.venv/\n"),
        ],
        generate: None,
    },
    Template {
        name: "node-cli",
        lang: "node",
        about: "a Node.js program with package.json scripts",
        files: &[
            (
                "package.json",
                "{\n  \"name\": \"{name}\",\n  \"version\": \"0.1.0\",\n  \"type\": \"module\",\n  \"scripts\": {\n    \"start\": \"node index.js\",\n    \"test\": \"node --test\"\n  }\n}\n",
            ),
            ("index.js", "console.log(\"Hello from {name}!\");\n"),
            (
                "index.test.js",
                "import { test } from \"node:test\";\nimport assert from \"node:assert\";\n\ntest(\"arithmetic\", () => assert.strictEqual(1 + 1, 2));\n",
            ),
            (".gitignore", "node_modules/\n"),
        ],
        generate: None,
    },
    Template {
        name: "bun-cli",
        lang: "bun",
        about: "a TypeScript program run by Bun",
        files: &[
            (
                "package.json",
                "{\n  \"name\": \"{name}\",\n  \"version\": \"0.1.0\",\n  \"scripts\": {\n    \"start\": \"bun run index.ts\",\n    \"test\": \"bun test\"\n  }\n}\n",
            ),
            ("index.ts", "console.log(\"Hello from {name}!\");\n"),
            (
                "index.test.ts",
                "import { expect, test } from \"bun:test\";\n\ntest(\"arithmetic\", () => expect(1 + 1).toBe(2));\n",
            ),
            (".gitignore", "node_modules/\n"),
        ],
        generate: None,
    },
    Template {
        name: "deno-cli",
        lang: "deno",
        about: "a TypeScript program run by Deno",
        files: &[
            (
                "deno.json",
                "{\n  \"tasks\": {\n    \"start\": \"deno run main.ts\"\n  }\n}\n",
            ),
            ("main.ts", "console.log(\"Hello from {name}!\");\n"),
            (
                "main_test.ts",
                "Deno.test(\"arithmetic\", () => {\n  if (1 + 1 !== 2) throw new Error(\"arithmetic is broken\");\n});\n",
            ),
        ],
        generate: None,
    },
    Template {
        name: "c-cli",
        lang: "c",
        about: "a C program built with CMake (needs a system C compiler)",
        files: &[
            (
                "CMakeLists.txt",
                "cmake_minimum_required(VERSION 3.16)\nproject({name} C)\nset(CMAKE_EXPORT_COMPILE_COMMANDS ON)\nadd_executable({name} main.c)\n",
            ),
            (
                "main.c",
                "#include <stdio.h>\n\nint main(void) {\n    printf(\"Hello from {name}!\\n\");\n    return 0;\n}\n",
            ),
            (".gitignore", "/build\n"),
        ],
        generate: None,
    },
    Template {
        name: "cpp-cli",
        lang: "cpp",
        about: "a C++ program built with CMake (needs a system C++ compiler)",
        files: &[
            (
                "CMakeLists.txt",
                "cmake_minimum_required(VERSION 3.16)\nproject({name} CXX)\nset(CMAKE_CXX_STANDARD 17)\nset(CMAKE_EXPORT_COMPILE_COMMANDS ON)\nadd_executable({name} main.cpp)\n",
            ),
            (
                "main.cpp",
                "#include <iostream>\n\nint main() {\n    std::cout << \"Hello from {name}!\" << std::endl;\n    return 0;\n}\n",
            ),
            (".gitignore", "/build\n"),
        ],
        generate: None,
    },
    Template {
        name: "zig-cli",
        lang: "zig",
        about: "a Zig program (from `zig init`)",
        files: &[],
        generate: Some(&["zig", "init"]),
    },
    Template {
        name: "java-cli",
        lang: "java",
        about: "a single-file Java program",
        files: &[(
            "Main.java",
            "public class Main {\n    public static void main(String[] args) {\n        System.out.println(\"Hello from {name}!\");\n    }\n}\n",
        )],
        generate: None,
    },
    Template {
        name: "kotlin-cli",
        lang: "kotlin",
        about: "a single-file Kotlin program",
        files: &[(
            "main.kt",
            "fun main() {\n    println(\"Hello from {name}!\")\n}\n",
        )],
        generate: None,
    },
    Template {
        name: "csharp-cli",
        lang: "csharp",
        about: "a .NET console program (from `dotnet new console`)",
        files: &[],
        generate: Some(&[
            "dotnet", "new", "console", "--name", "{name}", "--output", ".",
        ]),
    },
    Template {
        name: "lua-cli",
        lang: "lua",
        about: "a Lua script",
        files: &[("main.lua", "print(\"Hello from {name}!\")\n")],
        generate: None,
    },
    Template {
        name: "shell-script",
        lang: "shell",
        about: "a POSIX shell script",
        files: &[(
            "main.sh",
            "#!/bin/sh\nset -eu\necho \"Hello from {name}!\"\n",
        )],
        generate: None,
    },
    Template {
        name: "web-page",
        lang: "web",
        about: "an HTML page with a stylesheet",
        files: &[
            (
                "index.html",
                "<!doctype html>\n<html lang=\"en\">\n<head>\n  <meta charset=\"utf-8\">\n  <title>{name}</title>\n  <link rel=\"stylesheet\" href=\"style.css\">\n</head>\n<body>\n  <h1>Hello from {name}!</h1>\n</body>\n</html>\n",
            ),
            (
                "style.css",
                "body {\n  font-family: system-ui, sans-serif;\n}\n",
            ),
        ],
        generate: None,
    },
];

pub fn find(name: &str) -> Option<&'static Template> {
    TEMPLATES.iter().find(|t| t.name == name)
}

/// A project name from its folder: letters, digits, `-` and `_`.
pub fn project_name(dir: &Path) -> String {
    let raw = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".to_owned());
    let name: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let name = name.trim_matches('-').to_owned();
    if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
        format!("app-{name}")
    } else {
        name
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    #[error("{0} is not empty; TermOS creates projects in new or empty folders")]
    NotEmpty(PathBuf),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// Writes `template` into `dir` (new or empty) with `mise.toml` pinning
/// `pins` (`(spec, version)`). Returns the files written. A generator,
/// if any, is the caller's to run afterwards (see [`Template::generate`]).
pub fn write(
    template: &Template,
    dir: &Path,
    pins: &[(String, String)],
) -> Result<Vec<PathBuf>, CreateError> {
    if dir.exists() && std::fs::read_dir(dir)?.next().is_some() {
        return Err(CreateError::NotEmpty(dir.to_owned()));
    }
    std::fs::create_dir_all(dir)?;
    let name = project_name(dir);
    let mut written = Vec::new();
    for (rel, text) in template.files {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, text.replace("{name}", &name))?;
        #[cfg(unix)]
        if rel.ends_with(".sh") {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        }
        written.push(path);
    }
    if !pins.is_empty() {
        let mut toml = String::from("# Tool versions for this project (mise).\n[tools]\n");
        for (spec, version) in pins {
            let key = if spec
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                spec.clone()
            } else {
                format!("\"{spec}\"")
            };
            toml.push_str(&format!("{key} = \"{version}\"\n"));
        }
        let path = dir.join("mise.toml");
        std::fs::write(&path, toml)?;
        written.push(path);
    }
    Ok(written)
}

/// The language a template is for.
pub fn language(t: &Template) -> Option<&'static Language> {
    langs::find(t.lang)
}

/// A generator command with `{name}` filled in.
pub fn generator(t: &Template, dir: &Path) -> Option<Vec<String>> {
    let name = project_name(dir);
    t.generate
        .map(|g| g.iter().map(|a| a.replace("{name}", &name)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_template_has_a_language_and_unique_name() {
        let mut names = Vec::new();
        for t in TEMPLATES {
            assert!(language(t).is_some(), "{} names a known language", t.name);
            assert!(!names.contains(&t.name));
            names.push(t.name);
            assert!(
                !t.files.is_empty() || t.generate.is_some(),
                "{} makes something",
                t.name
            );
        }
        // One per language of ARCHITECTURE.md §10.
        for l in crate::langs::LANGUAGES {
            assert!(
                TEMPLATES.iter().any(|t| t.lang == l.name),
                "{} has a template",
                l.name
            );
        }
    }

    #[test]
    fn writing_a_project() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("my demo");
        let files = write(
            find("go-cli").expect("go-cli"),
            &p,
            &[("go".to_owned(), "1.23.2".to_owned())],
        )
        .expect("writes");
        assert_eq!(files.len(), 4);
        assert_eq!(
            std::fs::read_to_string(p.join("go.mod")).expect("read"),
            "module my-demo\n\ngo 1.22\n"
        );
        assert!(
            std::fs::read_to_string(p.join("mise.toml"))
                .expect("read")
                .contains("go = \"1.23.2\"")
        );
        // Not over an existing project.
        assert!(matches!(
            write(find("go-cli").expect("go-cli"), &p, &[]),
            Err(CreateError::NotEmpty(_))
        ));
        // Specs with backends are quoted keys.
        let q = dir.path().join("q");
        write(
            find("lua-cli").expect("lua"),
            &q,
            &[("aqua:x/y".to_owned(), "1".to_owned())],
        )
        .expect("writes");
        assert!(
            std::fs::read_to_string(q.join("mise.toml"))
                .expect("read")
                .contains("\"aqua:x/y\" = \"1\"")
        );
        assert_eq!(project_name(Path::new("/x/2fast")), "app-2fast");
        assert_eq!(
            generator(find("csharp-cli").expect("cs"), Path::new("/x/hello app"))
                .expect("generator"),
            [
                "dotnet",
                "new",
                "console",
                "--name",
                "hello-app",
                "--output",
                "."
            ]
        );
    }
}
