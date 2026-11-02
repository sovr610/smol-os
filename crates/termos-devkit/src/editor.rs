// SPDX-License-Identifier: GPL-3.0-or-later
//! The editors TermOS sets up (D-012, D-054): its own Neovim
//! (`NVIM_APPNAME=termos-nvim`, so `~/.config/nvim` is never touched) and
//! Helix.
//!
//! Servers and debuggers start through `mise exec` with the exact
//! versions installed, so editors need nothing on `PATH` and never
//! trigger an install while starting. The Neovim config is a few static
//! files (embedded here) plus `lua/termos/generated.lua`, rewritten from
//! what mise has installed whenever TermOS sets the editor up.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use crate::lang::{Installed, LangError, Runner};
use crate::langs::{LANGUAGES, Tool};

/// The Neovim TermOS installs (0.12 brings `vim.pack`).
pub const NEOVIM: Tool = Tool {
    spec: "neovim",
    version: "0.12.6",
    cmd: &["nvim"],
};

/// The Git UI (D-053).
pub const LAZYGIT: Tool = Tool {
    spec: "lazygit",
    version: "latest",
    cmd: &["lazygit"],
};

/// Helix, when chosen instead (`[dev] editor = "helix"`).
pub const HELIX: Tool = Tool {
    spec: "helix",
    version: "latest",
    cmd: &["hx"],
};

/// Neovim's `NVIM_APPNAME` for TermOS's config.
pub const APPNAME: &str = "termos-nvim";

/// The static part of the Neovim config: (path inside the config folder,
/// contents). `lua/user.lua` is written only when missing.
pub const NVIM_FILES: [(&str, &str); 5] = [
    ("init.lua", include_str!("../nvim/init.lua")),
    (
        "lua/termos/plugins.lua",
        include_str!("../nvim/lua/termos/plugins.lua"),
    ),
    (
        "lua/termos/lsp.lua",
        include_str!("../nvim/lua/termos/lsp.lua"),
    ),
    (
        "lua/termos/dap.lua",
        include_str!("../nvim/lua/termos/dap.lua"),
    ),
    ("lua/user.lua", include_str!("../nvim/lua/user.lua")),
];

/// A server or debugger with its exact command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub name: String,
    pub cmd: Vec<String>,
    pub filetypes: Vec<String>,
    pub root_markers: Vec<String>,
    pub helix: Vec<String>,
    /// Debuggers that listen on a port: the arguments for it.
    pub port_args: Option<Vec<String>>,
    /// Start only inside a project.
    pub needs_root: bool,
}

/// The installed version of `tool` to start a server with. When the
/// registry pins a line (`temurin-25`, `1.63.0`), the newest installed
/// version on it, so a server keeps the runtime it needs whatever the
/// user's default is; otherwise the active one, else the newest.
fn pick<'a>(installed: &'a BTreeMap<String, Vec<Installed>>, tool: &Tool) -> Option<&'a Installed> {
    let spec = tool.spec;
    let key = spec.split('[').next().unwrap_or(spec);
    let list = installed.get(key).or_else(|| installed.get(spec))?;
    let floating = matches!(tool.version, "latest" | "lts" | "stable");
    if !floating
        && let Some(i) = list.iter().rev().find(|i| {
            i.version == tool.version
                || i.version
                    .strip_prefix(tool.version)
                    .is_some_and(|rest| rest.starts_with(['.', '+', '-']))
        })
    {
        return Some(i);
    }
    list.iter().find(|i| i.active).or_else(|| list.last())
}

/// `mise exec <spec@version>… -- <cmd>`, with `{install}` in `cmd`
/// replaced by the last tool's install folder.
fn exec(mise: &Path, tools: &[(&Tool, &Installed)], cmd: &[&str]) -> Vec<String> {
    let mut out = vec![mise.display().to_string(), "exec".to_owned()];
    for (t, i) in tools {
        out.push(format!("{}@{}", t.spec, i.version));
    }
    out.push("--".to_owned());
    let install = tools
        .last()
        .map(|(_, i)| i.path.display().to_string())
        .unwrap_or_default();
    out.extend(cmd.iter().map(|c| c.replace("{install}", &install)));
    out
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_owned()).collect()
}

/// The installed language servers and debuggers, with their commands.
/// Languages whose runtime (or server) is not installed are left out.
pub fn resolve(
    mise: &Path,
    installed: &BTreeMap<String, Vec<Installed>>,
) -> (Vec<Resolved>, Vec<Resolved>) {
    let mut servers: Vec<Resolved> = Vec::new();
    let mut debuggers: Vec<Resolved> = Vec::new();
    for lang in LANGUAGES {
        // What the server process needs on its PATH: the runtime and the
        // helpers, when installed.
        let mut base: Vec<(&Tool, &Installed)> = Vec::new();
        if let Some(rt) = &lang.runtime {
            match pick(installed, rt) {
                Some(i) => base.push((rt, i)),
                None => continue,
            }
        }
        for h in lang.helpers {
            if let Some(i) = pick(installed, h) {
                base.push((h, i));
            }
        }
        for srv in lang.servers {
            let cmd = match &srv.tool {
                Some(t) => {
                    let Some(i) = pick(installed, t) else {
                        continue;
                    };
                    with_tool(mise, &base, t, i, t.cmd)
                }
                None if !base.is_empty() => exec(mise, &base, srv.cmd),
                None => continue,
            };
            match servers.iter_mut().find(|s| s.name == srv.name) {
                // One server for several languages (vtsls for Node and
                // Bun): its filetypes add up.
                Some(existing) => {
                    for ft in lang.filetypes {
                        if !existing.filetypes.iter().any(|f| f == ft) {
                            existing.filetypes.push((*ft).to_owned());
                        }
                    }
                    for h in lang.helix {
                        if !existing.helix.iter().any(|x| x == h) {
                            existing.helix.push((*h).to_owned());
                        }
                    }
                }
                None => servers.push(Resolved {
                    name: srv.name.to_owned(),
                    cmd,
                    filetypes: strings(lang.filetypes),
                    root_markers: strings(lang.root_markers),
                    helix: strings(lang.helix),
                    port_args: None,
                    needs_root: srv.needs_root,
                }),
            }
        }
        if let Some(d) = &lang.debugger
            && let Some(i) = pick(installed, &d.tool)
        {
            match debuggers.iter_mut().find(|x| x.name == d.name) {
                Some(existing) => {
                    for ft in lang.filetypes {
                        if !existing.filetypes.iter().any(|f| f == ft) {
                            existing.filetypes.push((*ft).to_owned());
                        }
                    }
                }
                None => {
                    debuggers.push(Resolved {
                        name: d.name.to_owned(),
                        cmd: with_tool(mise, &base, &d.tool, i, d.tool.cmd),
                        filetypes: strings(lang.filetypes),
                        root_markers: Vec::new(),
                        helix: Vec::new(),
                        port_args: d.port_args.map(strings),
                        needs_root: false,
                    });
                }
            }
        }
    }
    (servers, debuggers)
}

/// `base` plus `tool` (last, so `{install}` is its folder).
fn with_tool(
    mise: &Path,
    base: &[(&Tool, &Installed)],
    tool: &Tool,
    installed: &Installed,
    cmd: &[&str],
) -> Vec<String> {
    let mut tools: Vec<(&Tool, &Installed)> = base
        .iter()
        .copied()
        .filter(|(x, _)| x.spec != tool.spec)
        .collect();
    tools.push((tool, installed));
    exec(mise, &tools, cmd)
}

/// A Lua string literal.
pub fn lua_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\{}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn lua_list(v: &[String]) -> String {
    let items: Vec<String> = v.iter().map(|s| lua_str(s)).collect();
    format!("{{ {} }}", items.join(", "))
}

/// `lua/termos/generated.lua`.
pub fn generated_lua(servers: &[Resolved], debuggers: &[Resolved]) -> String {
    let mut out = String::from(
        "-- Written by TermOS from what `termos lang install` installed. Do not\n\
         -- edit: TermOS rewrites it. Your settings go in lua/user.lua.\nreturn {\n  servers = {\n",
    );
    for s in servers {
        out.push_str(&format!(
            "    {{ name = {}, cmd = {}, filetypes = {}, root_markers = {}, workspace_required = {} }},\n",
            lua_str(&s.name),
            lua_list(&s.cmd),
            lua_list(&s.filetypes),
            lua_list(&s.root_markers),
            s.needs_root
        ));
    }
    out.push_str("  },\n  debuggers = {\n");
    for d in debuggers {
        let port = d
            .port_args
            .as_ref()
            .map_or_else(|| "nil".to_owned(), |a| lua_list(a));
        out.push_str(&format!(
            "    {{ name = {}, cmd = {}, port_args = {port}, filetypes = {} }},\n",
            lua_str(&d.name),
            lua_list(&d.cmd),
            lua_list(&d.filetypes)
        ));
    }
    out.push_str("  },\n}\n");
    out
}

/// Helix's `languages.toml` for the installed servers.
pub fn helix_languages(servers: &[Resolved]) -> String {
    let mut root = toml::Table::new();
    let mut ls = toml::Table::new();
    let mut by_lang: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for s in servers {
        let mut t = toml::Table::new();
        let (cmd, args) = s
            .cmd
            .split_first()
            .map_or((String::new(), Vec::new()), |(c, a)| {
                (c.clone(), a.to_vec())
            });
        t.insert("command".into(), toml::Value::String(cmd));
        t.insert(
            "args".into(),
            toml::Value::Array(args.into_iter().map(toml::Value::String).collect()),
        );
        ls.insert(s.name.clone(), toml::Value::Table(t));
        for h in &s.helix {
            by_lang.entry(h.clone()).or_default().push(s.name.clone());
        }
    }
    root.insert("language-server".into(), toml::Value::Table(ls));
    let langs: Vec<toml::Value> = by_lang
        .into_iter()
        .map(|(name, servers)| {
            let mut t = toml::Table::new();
            t.insert("name".into(), toml::Value::String(name));
            t.insert(
                "language-servers".into(),
                toml::Value::Array(servers.into_iter().map(toml::Value::String).collect()),
            );
            toml::Value::Table(t)
        })
        .collect();
    root.insert("language".into(), toml::Value::Array(langs));
    format!(
        "# Written by TermOS (`termos edit` with [dev] editor = \"helix\"): the\n\
         # language servers `termos lang install` installed.\n{}",
        toml::to_string(&root).unwrap_or_default()
    )
}

/// Where a program keeps its config: `XDG_CONFIG_HOME/<name>`, on
/// Windows `%LOCALAPPDATA%` (Neovim) or `%APPDATA%` (Helix).
pub fn config_dir(
    get: &dyn Fn(&str) -> Option<String>,
    name: &str,
    windows_var: &str,
) -> Option<PathBuf> {
    let abs = |k: &str| get(k).map(PathBuf::from).filter(|p| p.is_absolute());
    if let Some(x) = abs("XDG_CONFIG_HOME") {
        return Some(x.join(name));
    }
    if cfg!(windows) {
        abs(windows_var).map(|d| d.join(name))
    } else {
        abs("HOME").map(|h| h.join(".config").join(name))
    }
}

/// Writes TermOS's Neovim config into `dir`: the static files (but an
/// existing `lua/user.lua`) and `generated`.
pub fn write_nvim(dir: &Path, generated: &str) -> io::Result<()> {
    for (rel, text) in NVIM_FILES {
        let path = dir.join(rel);
        if rel == "lua/user.lua" && path.exists() {
            continue;
        }
        write(&path, text)?;
    }
    write(&dir.join("lua/termos/generated.lua"), generated)
}

/// Writes Helix's `languages.toml` into `dir` when there is none (or it
/// is TermOS's own); otherwise next to it as `languages.termos.toml`.
/// Returns the file written and whether Helix uses it.
pub fn write_helix(dir: &Path, languages: &str) -> io::Result<(PathBuf, bool)> {
    let main = dir.join("languages.toml");
    let ours =
        std::fs::read_to_string(&main).map_or(true, |t| t.starts_with("# Written by TermOS"));
    let path = if ours {
        main
    } else {
        dir.join("languages.termos.toml")
    };
    write(&path, languages)?;
    Ok((path, ours))
}

fn write(path: &Path, text: &str) -> io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("termos-tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// How to start an editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// Installs `tool` at its pinned version unless mise has it; returns
/// what mise has installed and the version to run.
pub fn ensure_tool(
    tool: &Tool,
    mise: &mut dyn Runner,
    say: &mut dyn FnMut(&str),
) -> Result<(BTreeMap<String, Vec<Installed>>, String), LangError> {
    let mut have = crate::lang::installed(mise)?;
    let wanted = tool.version;
    let found = have
        .get(tool.spec)
        .and_then(|v| {
            if wanted == "latest" {
                v.last()
            } else {
                v.iter().find(|i| i.version == wanted)
            }
        })
        .map(|i| i.version.clone());
    let version = match found {
        Some(v) => v,
        None => {
            say(&format!("termos: installing {}@{wanted}", tool.spec));
            let at = format!("{}@{wanted}", tool.spec);
            if !mise.run(&["install".to_owned(), at.clone()], None)? {
                return Err(LangError::Mise(format!("install {at}")));
            }
            have = crate::lang::installed(mise)?;
            have.get(tool.spec)
                .and_then(|v| v.last())
                .map(|i| i.version.clone())
                .ok_or_else(|| LangError::Mise(format!("find {at} after installing it")))?
        }
    };
    Ok((have, version))
}

/// Sets up TermOS's Neovim (installing it when needed, rewriting its
/// config from what is installed) and says how to start it on `files`.
pub fn prepare_nvim(
    mise_path: &Path,
    mise: &mut dyn Runner,
    get: &dyn Fn(&str) -> Option<String>,
    files: &[String],
    say: &mut dyn FnMut(&str),
) -> Result<Launch, LangError> {
    let (have, version) = ensure_tool(&NEOVIM, mise, say)?;
    let dir = config_dir(get, APPNAME, "LOCALAPPDATA").ok_or_else(|| {
        LangError::Mise("find a folder for the Neovim config (HOME is not set)".into())
    })?;
    let (servers, debuggers) = resolve(mise_path, &have);
    write_nvim(&dir, &generated_lua(&servers, &debuggers))?;
    let mut args = vec![
        "exec".to_owned(),
        format!("{}@{version}", NEOVIM.spec),
        "--".to_owned(),
    ];
    args.extend(NEOVIM.cmd.iter().map(|c| (*c).to_owned()));
    args.extend(files.iter().cloned());
    Ok(Launch {
        program: mise_path.to_owned(),
        args,
        env: vec![("NVIM_APPNAME".to_owned(), APPNAME.to_owned())],
    })
}

/// The same for Helix, with its `languages.toml`.
pub fn prepare_helix(
    mise_path: &Path,
    mise: &mut dyn Runner,
    get: &dyn Fn(&str) -> Option<String>,
    files: &[String],
    say: &mut dyn FnMut(&str),
) -> Result<Launch, LangError> {
    let (have, version) = ensure_tool(&HELIX, mise, say)?;
    let dir = config_dir(get, "helix", "APPDATA")
        .ok_or_else(|| LangError::Mise("find Helix's config folder (HOME is not set)".into()))?;
    let (servers, _) = resolve(mise_path, &have);
    let (file, used) = write_helix(&dir, &helix_languages(&servers))?;
    if !used {
        say(&format!(
            "termos: you have your own languages.toml; TermOS's servers are in {} to merge",
            file.display()
        ));
    }
    let mut args = vec![
        "exec".to_owned(),
        format!("{}@{version}", HELIX.spec),
        "--".to_owned(),
    ];
    args.extend(HELIX.cmd.iter().map(|c| (*c).to_owned()));
    args.extend(files.iter().cloned());
    Ok(Launch {
        program: mise_path.to_owned(),
        args,
        env: Vec::new(),
    })
}

/// After languages change: rewrites the editors' server lists where
/// TermOS set them up before.
pub fn refresh(
    mise_path: &Path,
    mise: &mut dyn Runner,
    get: &dyn Fn(&str) -> Option<String>,
) -> Result<(), LangError> {
    let nvim = config_dir(get, APPNAME, "LOCALAPPDATA").filter(|d| d.join("init.lua").is_file());
    let helix = config_dir(get, "helix", "APPDATA").filter(|d| {
        std::fs::read_to_string(d.join("languages.toml"))
            .is_ok_and(|t| t.starts_with("# Written by TermOS"))
    });
    if nvim.is_none() && helix.is_none() {
        return Ok(());
    }
    let have = crate::lang::installed(mise)?;
    let (servers, debuggers) = resolve(mise_path, &have);
    if let Some(d) = nvim {
        write(
            &d.join("lua/termos/generated.lua"),
            &generated_lua(&servers, &debuggers),
        )?;
    }
    if let Some(d) = helix {
        write_helix(&d, &helix_languages(&servers))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::parse_ls;

    fn installed() -> BTreeMap<String, Vec<Installed>> {
        parse_ls(
            r#"{
            "go": [{"version": "1.23.2", "install_path": "/m/go", "active": true}],
            "go:golang.org/x/tools/gopls": [{"version": "0.16.2", "install_path": "/m/gopls"}],
            "go:github.com/go-delve/delve/cmd/dlv": [{"version": "1.24.0", "install_path": "/m/dlv"}],
            "node": [{"version": "22.11.0", "install_path": "/m/node", "active": true}],
            "npm:@vtsls/language-server": [{"version": "0.2.6", "install_path": "/m/vtsls"}],
            "bun": [{"version": "1.1.0", "install_path": "/m/bun", "active": true}],
            "github:microsoft/vscode-js-debug": [{"version": "1.95.0", "install_path": "/m/jsdbg"}],
            "deno": [{"version": "2.0.0", "install_path": "/m/deno", "active": true}]
        }"#,
        )
    }

    #[test]
    fn servers_get_the_runtime_line_they_were_installed_with() {
        // The user's default Java is 21; jdtls is set up with Java 25.
        let have = parse_ls(
            r#"{
            "java": [
                {"version": "temurin-21.0.13+9.0.LTS", "install_path": "/m/j21", "active": true},
                {"version": "temurin-25.0.1+8.0.LTS", "install_path": "/m/j25"}
            ],
            "python": [{"version": "3.15.2", "install_path": "/m/py", "active": true}],
            "http:jdtls": [{"version": "1.63.0", "install_path": "/m/jdtls"}]
        }"#,
        );
        let (servers, _) = resolve(Path::new("/bin/mise"), &have);
        let jdtls = servers.iter().find(|s| s.name == "jdtls").expect("jdtls");
        assert!(
            jdtls
                .cmd
                .contains(&"java@temurin-25.0.1+8.0.LTS".to_owned()),
            "{:?}",
            jdtls.cmd
        );
        assert!(
            jdtls
                .cmd
                .ends_with(&["python3".to_owned(), "/m/jdtls/bin/jdtls".to_owned()])
        );
        // Floating versions (`latest`, `lts`) take the active one.
        let java = crate::langs::find("java").expect("java");
        let python = &java.helpers[0];
        assert_eq!(
            pick(&have, python).map(|i| i.version.as_str()),
            Some("3.15.2")
        );
        // A pinned line that is not installed falls back the same way.
        let only21 = parse_ls(
            r#"{"java": [{"version": "temurin-21.0.13+9.0.LTS", "install_path": "/m/j21", "active": true}]}"#,
        );
        let rt = java.runtime.expect("runtime");
        assert_eq!(
            pick(&only21, &rt).map(|i| i.version.as_str()),
            Some("temurin-21.0.13+9.0.LTS")
        );
    }

    #[test]
    fn servers_start_through_mise_with_exact_versions() {
        let (servers, debuggers) = resolve(Path::new("/bin/mise"), &installed());
        let names: Vec<&str> = servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["gopls", "vtsls", "denols"]);
        assert_eq!(
            servers[0].cmd,
            [
                "/bin/mise",
                "exec",
                "go@1.23.2",
                "go:golang.org/x/tools/gopls@0.16.2",
                "--",
                "gopls"
            ]
        );
        // vtsls serves Node and Bun: one server, both languages' files.
        assert!(servers[1].filetypes.contains(&"typescriptreact".to_owned()));
        // Deno serves itself.
        assert_eq!(servers[2].cmd.last().map(String::as_str), Some("lsp"));
        // Debuggers: delve on a port; js-debug's script from its folder.
        let delve = debuggers.iter().find(|d| d.name == "delve").expect("delve");
        assert_eq!(
            delve.port_args.as_deref(),
            Some(
                &[
                    "dap".to_owned(),
                    "-l".to_owned(),
                    "127.0.0.1:${port}".to_owned()
                ][..]
            )
        );
        let js = debuggers
            .iter()
            .find(|d| d.name == "pwa-node")
            .expect("js-debug");
        assert!(
            js.cmd
                .contains(&"/m/jsdbg/src/dapDebugServer.js".to_owned()),
            "{:?}",
            js.cmd
        );
        assert!(js.cmd.contains(&"node@22.11.0".to_owned()));
        // Nothing installed: nothing configured.
        let (s, d) = resolve(Path::new("/bin/mise"), &BTreeMap::new());
        assert!(s.is_empty() && d.is_empty());
    }

    #[test]
    fn the_generated_config_files() {
        let (servers, debuggers) = resolve(Path::new("C:\\mise\\mise.exe"), &installed());
        let lua = generated_lua(&servers, &debuggers);
        assert!(
            lua.contains(r#"cmd = { "C:\\mise\\mise.exe", "exec""#),
            "{lua}"
        );
        assert!(
            lua.contains("port_args = nil") || !debuggers.iter().any(|d| d.port_args.is_none())
        );
        let (s2, d2) = resolve(Path::new("/bin/mise"), &installed());
        insta::assert_snapshot!("generated-lua", generated_lua(&s2, &d2));
        let toml_text = helix_languages(&servers);
        let parsed: toml::Table = toml::from_str(&toml_text).expect("valid TOML");
        let go = parsed["language"]
            .as_array()
            .and_then(|a| a.iter().find(|l| l["name"].as_str() == Some("go")))
            .expect("go");
        assert_eq!(go["language-servers"].as_array().map(Vec::len), Some(1));
        assert_eq!(lua_str("a\"b\\c\n"), r#""a\"b\\c\n""#);
    }

    #[test]
    fn neovim_is_installed_once_and_started_with_its_config() {
        use crate::lang::tests::Fake;
        let home = tempfile::tempdir().expect("tempdir");
        let cfg = home.path().join("cfg");
        let get = |k: &str| (k == "XDG_CONFIG_HOME").then(|| cfg.display().to_string());
        let mut said = Vec::new();
        // Not installed yet: the fake's `ls` has no neovim, so it installs.
        let mut m = Fake {
            ls: r#"{"neovim": [{"version": "0.12.6", "install_path": "/m/nvim"}]}"#.into(),
            ..Fake::default()
        };
        let launch = prepare_nvim(
            Path::new("/bin/mise"),
            &mut m,
            &get,
            &["a.go".into()],
            &mut |s| said.push(s.to_owned()),
        )
        .expect("prepared");
        assert_eq!(launch.args, ["exec", "neovim@0.12.6", "--", "nvim", "a.go"]);
        assert_eq!(
            launch.env,
            [("NVIM_APPNAME".to_owned(), "termos-nvim".to_owned())]
        );
        assert!(cfg.join("termos-nvim/init.lua").is_file());
        assert!(cfg.join("termos-nvim/lua/termos/generated.lua").is_file());
        assert!(
            !m.calls.iter().any(|c| c.starts_with("install")),
            "already installed: {:?}",
            m.calls
        );
        let mut m = Fake {
            ls: "{}".into(),
            ..Fake::default()
        };
        // Installing works, but the fake still lists nothing: an error.
        assert!(prepare_nvim(Path::new("/bin/mise"), &mut m, &get, &[], &mut |_| {}).is_err());
        assert!(
            m.calls.contains(&"install neovim@0.12.6".to_owned()),
            "{:?}",
            m.calls
        );
        // A later `termos lang install` rewrites the server list.
        let mut m = Fake {
            ls: r#"{"go": [{"version": "1.23.2", "install_path": "/m/go", "active": true}],
                    "go:golang.org/x/tools/gopls": [{"version": "0.16.2", "install_path": "/m/gopls"}]}"#
                .into(),
            ..Fake::default()
        };
        refresh(Path::new("/bin/mise"), &mut m, &get).expect("refreshed");
        let text = std::fs::read_to_string(cfg.join("termos-nvim/lua/termos/generated.lua"))
            .expect("read");
        assert!(text.contains("gopls"), "{text}");
    }

    #[test]
    fn writing_keeps_the_users_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let d = dir.path();
        write_nvim(d, "return {}").expect("writes");
        std::fs::write(d.join("lua/user.lua"), "-- mine").expect("write");
        write_nvim(d, "return { servers = {} }").expect("rewrites");
        assert_eq!(
            std::fs::read_to_string(d.join("lua/user.lua")).expect("read"),
            "-- mine"
        );
        assert!(
            std::fs::read_to_string(d.join("lua/termos/generated.lua"))
                .expect("read")
                .contains("servers")
        );
        assert!(d.join("init.lua").is_file());
        // Helix: ours replaces ours, a user's file stays.
        let h = d.join("helix");
        let (p, used) = write_helix(&h, "# Written by TermOS\n").expect("writes");
        assert!(used && p.ends_with("languages.toml"));
        std::fs::write(h.join("languages.toml"), "[[language]]\nname = \"mine\"\n").expect("write");
        let (p, used) = write_helix(&h, "# Written by TermOS\n").expect("writes");
        assert!(!used && p.ends_with("languages.termos.toml"));
        assert!(
            std::fs::read_to_string(h.join("languages.toml"))
                .expect("read")
                .contains("mine")
        );
    }
}
