// SPDX-License-Identifier: GPL-3.0-or-later
//! The languages TermOS sets up (ARCHITECTURE.md §10): what mise installs
//! for each (the runtime or compiler, helpers, language servers and a
//! debugger), and how editors start the servers.
//!
//! Tool specs are mise's: a registry name (`go`, `rust-analyzer`) or a
//! backend and package (`go:golang.org/x/tools/gopls`,
//! `npm:@vtsls/language-server`, `pipx:basedpyright`,
//! `github:clangd/clangd`, `dotnet:csharp-ls`, `http:…`). The nightly
//! language CI installs every one on x86_64 and aarch64 (D-053).

/// A program mise installs, and the command that starts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tool {
    /// The mise tool spec (without a version).
    pub spec: &'static str,
    /// The version to install (`latest`, `lts`, `stable`, …).
    pub version: &'static str,
    /// The executable it provides, and its arguments to start a server.
    pub cmd: &'static [&'static str],
}

const fn tool(spec: &'static str, cmd: &'static [&'static str]) -> Tool {
    Tool {
        spec,
        version: "latest",
        cmd,
    }
}

/// A language server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Server {
    /// The name Neovim and Helix know it by (`gopls`, `rust_analyzer`).
    pub name: &'static str,
    /// Installed by mise, or `None` when the runtime itself serves
    /// (`deno lsp`).
    pub tool: Option<Tool>,
    /// Its command when `tool` is `None`.
    pub cmd: &'static [&'static str],
}

/// A debug adapter (DAP), for nvim-dap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Debugger {
    /// nvim-dap's adapter name.
    pub name: &'static str,
    pub tool: Tool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Language {
    /// `termos lang install <name>`.
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub title: &'static str,
    /// The runtime or compiler mise installs; `None` uses the system's
    /// (C and C++ need a system compiler; shell scripts need a shell).
    pub runtime: Option<Tool>,
    /// Other tools the servers need (node for npm servers, uv for pipx).
    pub helpers: &'static [Tool],
    pub servers: &'static [Server],
    pub debugger: Option<Debugger>,
    /// Neovim filetypes.
    pub filetypes: &'static [&'static str],
    /// Files that mark a project's root.
    pub root_markers: &'static [&'static str],
    /// Platforms (`os-arch`) where something above has no build, with
    /// what is missing (for `docs/languages.md` and `termos lang list`).
    pub limits: &'static [(&'static str, &'static str)],
}

impl Language {
    /// Every mise tool `termos lang install` installs for it.
    pub fn tools(&self) -> Vec<Tool> {
        let mut v: Vec<Tool> = self.runtime.into_iter().collect();
        v.extend(self.helpers.iter().copied());
        v.extend(self.servers.iter().filter_map(|s| s.tool));
        v.extend(self.debugger.map(|d| d.tool));
        let mut seen = Vec::new();
        v.retain(|t| {
            let new = !seen.contains(&t.spec);
            seen.push(t.spec);
            new
        });
        v
    }
}

const NODE: Tool = Tool {
    spec: "node",
    version: "lts",
    cmd: &["node"],
};
const UV: Tool = tool("uv", &["uv"]);
const CODELLDB: Debugger = Debugger {
    name: "codelldb",
    tool: tool("github:vadimcn/codelldb", &["codelldb"]),
};
const JS_DEBUG: Debugger = Debugger {
    name: "js-debug",
    tool: tool("github:microsoft/vscode-js-debug", &["js-debug"]),
};
const VTSLS: Server = Server {
    name: "vtsls",
    tool: Some(tool("npm:@vtsls/language-server", &["vtsls", "--stdio"])),
    cmd: &[],
};
const CLANGD: Server = Server {
    name: "clangd",
    tool: Some(tool("github:clangd/clangd", &["clangd"])),
    cmd: &[],
};
const C_LIMITS: &[(&str, &str)] = &[(
    "linux-aarch64",
    "clangd has no aarch64 Linux build on GitHub: install it with your system's packages",
)];

pub const LANGUAGES: &[Language] = &[
    Language {
        name: "rust",
        aliases: &["rs"],
        title: "Rust",
        runtime: Some(Tool {
            spec: "rust",
            version: "stable",
            cmd: &["cargo"],
        }),
        helpers: &[],
        servers: &[Server {
            name: "rust_analyzer",
            tool: Some(tool("rust-analyzer", &["rust-analyzer"])),
            cmd: &[],
        }],
        debugger: Some(CODELLDB),
        filetypes: &["rust"],
        root_markers: &["Cargo.toml"],
        limits: &[],
    },
    Language {
        name: "c",
        aliases: &[],
        title: "C",
        runtime: None,
        helpers: &[tool("cmake", &["cmake"])],
        servers: &[CLANGD],
        debugger: Some(CODELLDB),
        filetypes: &["c"],
        root_markers: &["CMakeLists.txt", "compile_commands.json", "Makefile"],
        limits: C_LIMITS,
    },
    Language {
        name: "cpp",
        aliases: &["c++", "cxx"],
        title: "C++",
        runtime: None,
        helpers: &[tool("cmake", &["cmake"])],
        servers: &[CLANGD],
        debugger: Some(CODELLDB),
        filetypes: &["cpp"],
        root_markers: &["CMakeLists.txt", "compile_commands.json", "Makefile"],
        limits: C_LIMITS,
    },
    Language {
        name: "zig",
        aliases: &[],
        title: "Zig",
        runtime: Some(tool("zig", &["zig"])),
        helpers: &[],
        servers: &[Server {
            name: "zls",
            tool: Some(tool("zls", &["zls"])),
            cmd: &[],
        }],
        debugger: Some(CODELLDB),
        filetypes: &["zig"],
        root_markers: &["build.zig"],
        limits: &[],
    },
    Language {
        name: "go",
        aliases: &["golang"],
        title: "Go",
        runtime: Some(tool("go", &["go"])),
        helpers: &[],
        servers: &[Server {
            name: "gopls",
            tool: Some(tool("go:golang.org/x/tools/gopls", &["gopls"])),
            cmd: &[],
        }],
        debugger: Some(Debugger {
            name: "delve",
            tool: tool("go:github.com/go-delve/delve/cmd/dlv", &["dlv", "dap"]),
        }),
        filetypes: &["go", "gomod"],
        root_markers: &["go.mod"],
        limits: &[],
    },
    Language {
        name: "python",
        aliases: &["py"],
        title: "Python",
        runtime: Some(tool("python", &["python3"])),
        helpers: &[UV],
        servers: &[
            Server {
                name: "basedpyright",
                tool: Some(tool(
                    "pipx:basedpyright",
                    &["basedpyright-langserver", "--stdio"],
                )),
                cmd: &[],
            },
            Server {
                name: "ruff",
                tool: Some(tool("ruff", &["ruff", "server"])),
                cmd: &[],
            },
        ],
        debugger: Some(Debugger {
            name: "debugpy",
            tool: tool("pipx:debugpy", &["debugpy-adapter"]),
        }),
        filetypes: &["python"],
        root_markers: &["pyproject.toml", "setup.py", "requirements.txt"],
        limits: &[],
    },
    Language {
        name: "java",
        aliases: &[],
        title: "Java",
        runtime: Some(Tool {
            spec: "java",
            version: "temurin-21",
            cmd: &["java"],
        }),
        helpers: &[],
        servers: &[Server {
            name: "jdtls",
            tool: Some(Tool {
                spec: "http:jdtls[url=https://download.eclipse.org/jdtls/snapshots/jdt-language-server-latest.tar.gz]",
                version: "latest",
                cmd: &["jdtls"],
            }),
            cmd: &[],
        }],
        debugger: None,
        filetypes: &["java"],
        root_markers: &[
            "pom.xml",
            "build.gradle",
            "build.gradle.kts",
            "settings.gradle",
        ],
        limits: &[],
    },
    Language {
        name: "kotlin",
        aliases: &["kt"],
        title: "Kotlin",
        runtime: Some(tool("kotlin", &["kotlinc"])),
        helpers: &[Tool {
            spec: "java",
            version: "temurin-21",
            cmd: &["java"],
        }],
        servers: &[Server {
            name: "kotlin_language_server",
            tool: Some(tool(
                "github:fwcd/kotlin-language-server",
                &["kotlin-language-server"],
            )),
            cmd: &[],
        }],
        debugger: None,
        filetypes: &["kotlin"],
        root_markers: &["build.gradle.kts", "build.gradle", "settings.gradle.kts"],
        limits: &[],
    },
    Language {
        name: "csharp",
        aliases: &["c#", "cs", "dotnet"],
        title: "C# / .NET",
        runtime: Some(tool("dotnet", &["dotnet"])),
        helpers: &[],
        servers: &[Server {
            name: "csharp_ls",
            tool: Some(tool("dotnet:csharp-ls", &["csharp-ls"])),
            cmd: &[],
        }],
        debugger: Some(Debugger {
            name: "netcoredbg",
            tool: tool(
                "github:Samsung/netcoredbg",
                &["netcoredbg", "--interpreter=vscode"],
            ),
        }),
        filetypes: &["cs"],
        root_markers: &["*.sln", "*.csproj"],
        limits: &[],
    },
    Language {
        name: "node",
        aliases: &["javascript", "js", "typescript", "ts"],
        title: "JavaScript / TypeScript (Node)",
        runtime: Some(NODE),
        helpers: &[],
        servers: &[VTSLS],
        debugger: Some(JS_DEBUG),
        filetypes: &[
            "javascript",
            "javascriptreact",
            "typescript",
            "typescriptreact",
        ],
        root_markers: &["package.json", "tsconfig.json"],
        limits: &[],
    },
    Language {
        name: "bun",
        aliases: &[],
        title: "JavaScript / TypeScript (Bun)",
        runtime: Some(tool("bun", &["bun"])),
        helpers: &[NODE],
        servers: &[VTSLS],
        debugger: Some(JS_DEBUG),
        filetypes: &["javascript", "typescript"],
        root_markers: &["bun.lock", "bun.lockb", "package.json"],
        limits: &[],
    },
    Language {
        name: "deno",
        aliases: &[],
        title: "Deno",
        runtime: Some(tool("deno", &["deno"])),
        helpers: &[],
        servers: &[Server {
            name: "denols",
            tool: None,
            cmd: &["deno", "lsp"],
        }],
        debugger: None,
        filetypes: &["javascript", "typescript"],
        root_markers: &["deno.json", "deno.jsonc"],
        limits: &[],
    },
    Language {
        name: "web",
        aliases: &["html", "css"],
        title: "HTML and CSS",
        runtime: None,
        helpers: &[NODE],
        servers: &[
            Server {
                name: "html",
                tool: Some(tool(
                    "npm:vscode-langservers-extracted",
                    &["vscode-html-language-server", "--stdio"],
                )),
                cmd: &[],
            },
            Server {
                name: "cssls",
                tool: Some(tool(
                    "npm:vscode-langservers-extracted",
                    &["vscode-css-language-server", "--stdio"],
                )),
                cmd: &[],
            },
        ],
        debugger: None,
        filetypes: &["html", "css", "scss"],
        root_markers: &["index.html", "package.json"],
        limits: &[],
    },
    Language {
        name: "lua",
        aliases: &[],
        title: "Lua",
        runtime: Some(tool("lua", &["lua"])),
        helpers: &[],
        servers: &[Server {
            name: "lua_ls",
            tool: Some(tool("lua-language-server", &["lua-language-server"])),
            cmd: &[],
        }],
        debugger: None,
        filetypes: &["lua"],
        root_markers: &[".luarc.json", "init.lua"],
        limits: &[],
    },
    Language {
        name: "shell",
        aliases: &["bash", "sh"],
        title: "Shell",
        runtime: None,
        helpers: &[NODE, tool("shellcheck", &["shellcheck"])],
        servers: &[Server {
            name: "bashls",
            tool: Some(tool(
                "npm:bash-language-server",
                &["bash-language-server", "start"],
            )),
            cmd: &[],
        }],
        debugger: None,
        filetypes: &["sh", "bash"],
        root_markers: &[".git"],
        limits: &[],
    },
];

/// A language by name or alias (case does not matter).
pub fn find(name: &str) -> Option<&'static Language> {
    let n = name.to_ascii_lowercase();
    LANGUAGES
        .iter()
        .find(|l| l.name == n || l.aliases.contains(&n.as_str()))
}

/// `go@1.23` → (`go`, `Some("1.23")`).
pub fn split_version(arg: &str) -> (&str, Option<&str>) {
    match arg.split_once('@') {
        Some((n, v)) if !v.is_empty() => (n, Some(v)),
        Some((n, _)) => (n, None),
        None => (arg, None),
    }
}

/// `os-arch` for this machine, as in [`Language::limits`].
pub fn platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_aliases_are_unique_and_found() {
        let mut all = Vec::new();
        for l in LANGUAGES {
            for n in std::iter::once(&l.name).chain(l.aliases) {
                assert!(!all.contains(n), "{n} twice");
                all.push(*n);
                assert_eq!(find(n).map(|f| f.name), Some(l.name));
                assert_eq!(find(&n.to_uppercase()).map(|f| f.name), Some(l.name));
            }
            assert!(!l.servers.is_empty(), "{} has a language server", l.name);
            assert!(!l.filetypes.is_empty() && !l.root_markers.is_empty());
            for s in l.servers {
                assert!(s.tool.is_some() || !s.cmd.is_empty(), "{} starts", s.name);
            }
        }
        assert_eq!(find("TypeScript").map(|l| l.name), Some("node"));
        assert_eq!(find("cobol"), None);
        // The ARCHITECTURE.md §10 list, all here.
        for n in [
            "rust", "c", "cpp", "zig", "go", "python", "java", "kotlin", "csharp", "node", "deno",
            "web", "lua", "shell",
        ] {
            assert!(find(n).is_some(), "{n}");
        }
    }

    #[test]
    fn tools_come_once_runtime_first() {
        let go = find("go").expect("go");
        let specs: Vec<&str> = go.tools().iter().map(|t| t.spec).collect();
        assert_eq!(
            specs,
            [
                "go",
                "go:golang.org/x/tools/gopls",
                "go:github.com/go-delve/delve/cmd/dlv"
            ]
        );
        // HTML and CSS share one npm package: installed once.
        let web = find("web").expect("web");
        let specs: Vec<&str> = web.tools().iter().map(|t| t.spec).collect();
        assert_eq!(specs, ["node", "npm:vscode-langservers-extracted"]);
        assert_eq!(split_version("go@1.23"), ("go", Some("1.23")));
        assert_eq!(split_version("go@"), ("go", None));
        assert_eq!(split_version("go"), ("go", None));
    }
}
