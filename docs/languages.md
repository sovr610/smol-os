# Languages, editors and projects

TermOS sets up programming languages for you: the compiler or runtime, the
language servers your editor uses for completion and diagnostics, and a
debugger. It does this through [mise](https://mise.jdx.dev), so versions are
pinned per project and nothing is installed system-wide.

```bash
termos lang install go
termos create go-cli demo
cd demo
termos edit main.go
```

## mise

TermOS uses the `mise` on your `PATH` when it is version 2025.10.0 or newer.
Otherwise it downloads mise 2026.10.14 from GitHub with your system's `curl`,
checks it against the SHA-256 sums TermOS was built with, and keeps it in
TermOS's data folder (`~/.local/share/termos/mise/` on Linux and macOS,
`%LOCALAPPDATA%\termos\data\mise\` on Windows). Set `TERMOS_MISE` to use a
particular mise binary.

TermOS panes get mise's shims at the front of `PATH`, so `go`, `cargo` or
`python3` in any pane run the version mise picks for that folder.

## `termos lang`

| Command | What it does |
| --- | --- |
| `termos lang list` | Every language, the versions installed (`*` is the default) and whether its servers and debugger are installed. |
| `termos lang install LANG[@VERSION]…` | Installs the runtime and makes it your default, then installs its language servers and debugger. |
| `termos lang use LANG@VERSION [--dir DIR]` | Pins a version for the project in a folder (writes `mise.toml`). |
| `termos lang remove LANG[@VERSION]` | Uninstalls one version, or the language with its servers and debugger. |
| `termos lang check LANG` | Checks the language from start to finish on this machine (see below). |

If a language server or debugger does not install, `termos lang install` says
which one and exits with an error, but the language itself stays installed.
After an install, TermOS rewrites its editor configs so they know about the
new servers.

### What TermOS installs

| Language | `LANG` | Runtime | Language servers | Debugger | Template |
| --- | --- | --- | --- | --- | --- |
| Rust | `rust` | rust (stable) | rust-analyzer | codelldb | `rust-cli` |
| C | `c` | your system's compiler, plus CMake | clangd | codelldb | `c-cli` |
| C++ | `cpp` | your system's compiler, plus CMake | clangd | codelldb | `cpp-cli` |
| Zig | `zig` | zig | zls | codelldb | `zig-cli` |
| Go | `go` | go | gopls | delve | `go-cli` |
| Python | `python` | python, plus uv | basedpyright, ruff | debugpy | `python-cli` |
| Java | `java` | Temurin 25, plus Python for jdtls's launcher | jdtls (needs Java 25) | none yet | `java-cli` |
| Kotlin | `kotlin` | kotlin, plus Temurin 21 (kotlin-language-server cannot run on 25) | kotlin-language-server | none yet | `kotlin-cli` |
| C# / .NET | `csharp` | dotnet | csharp-ls | netcoredbg | `csharp-cli` |
| JavaScript / TypeScript (Node) | `node` | node (LTS) | vtsls | js-debug | `node-cli` |
| JavaScript / TypeScript (Bun) | `bun` | bun, plus node | vtsls | js-debug | `bun-cli` |
| Deno | `deno` | deno | `deno lsp` | none yet | `deno-cli` |
| HTML and CSS | `web` | node | vscode-html/css-language-server | none | `web-page` |
| Lua | `lua` | lua | lua-language-server | none yet | `lua-cli` |
| Shell | `shell` | your system's shell, plus node and shellcheck | bash-language-server | none yet | `shell-script` |

vtsls starts only in projects with a `package.json` or `tsconfig.json`, and
Deno's server only in projects with a `deno.json`, so the two never both
attach to the same file.

### Platforms

Every language above is checked from start to finish (`termos lang check`) on
Linux x86_64 and aarch64 by TermOS's language CI, every night and whenever the
developer environment changes. Rust, Python and Go are also checked on every
change to TermOS.

| Platform | Status |
| --- | --- |
| Linux x86_64 | Every language, server and debugger above. |
| Linux aarch64 | Everything except clangd, which has no aarch64 Linux build on GitHub. Install it with your system's packages (`apt install clangd`) and set it up in `lua/user.lua` (below). C and C++ still build, run and debug. |
| macOS, Windows | The commands work, but the language matrix does not run there yet, so some tool may lack a build. `termos lang check LANG` tells you. |

On small boards (a Raspberry Pi 3 or Zero 2 W, with 512 MB to 1 GB of memory),
jdtls and kotlin-language-server need more memory than the board has to
spare.

### `termos lang check`

```bash
termos lang check python
```

It runs every step a user would and prints each one:

1. **install:** as `termos lang install`;
2. **create:** the language's template in a scratch folder;
3. **build, run, test:** the project's own tasks, through mise; the run must
   print what the template prints;
4. **editor:** TermOS's Neovim opens the template's main file without a
   screen and waits until every language server for it has started (the LSP
   `initialize` handshake);
5. **debugger:** the debugger is started the way Neovim starts it and must
   answer the debug protocol's `initialize` request.

A step that cannot run on this platform is shown as skipped, with the reason.
If any step fails, the command exits with an error and keeps the project, and
says where it is; otherwise the scratch folder is removed. `--dir DIR`
creates the project in `DIR` instead, and `--timeout SECS` (default 240) is
how long a server or debugger may take to start.

## Editors

`termos edit [FILE]…` opens your editor. Which one is set in the config:

```toml
[dev]
editor = "auto"   # "auto", "termos-nvim", "helix" or "env"
```

| Value | Editor |
| --- | --- |
| `auto` | `$VISUAL` or `$EDITOR` when set, else TermOS's Neovim |
| `termos-nvim` | TermOS's Neovim |
| `helix` | Helix, with TermOS's language servers |
| `env` | `$VISUAL` or `$EDITOR` |

The file manager's edit key and Settings › Developer › Editor use the same
choice.

### TermOS's Neovim

TermOS installs Neovim 0.12.6 through mise and keeps its config apart from
yours (`NVIM_APPNAME=termos-nvim`, so `~/.config/nvim` is never touched). It
lives in `~/.config/termos-nvim/` (`%LOCALAPPDATA%\termos-nvim\` on Windows):

- `init.lua` and `lua/termos/` are TermOS's and are rewritten when TermOS sets
  the editor up;
- `lua/user.lua` is yours: TermOS creates it once and never changes it. Put
  your own settings, keys and plugins there.

Language servers start through mise with the exact versions installed, so the
editor needs nothing on `PATH`. A server that needs a particular runtime gets
the one TermOS installed for it: jdtls gets Java 25 and kotlin-language-server
Java 21, whatever your default Java is. A runtime a language needs (Kotlin's
Java, Node for some servers) becomes your default only if you have none yet. The debugging plugins (nvim-dap, nvim-dap-ui
and nvim-nio) are installed by Neovim's own plugin manager, pinned to exact
commits, the first time Neovim starts.

| Key | What it does |
| --- | --- |
| `K` | Documentation for the word under the cursor |
| `gd` / `gD` | Go to the definition / declaration |
| `grr` / `gri` | References / implementations |
| `grn` | Rename |
| `gra` | Code actions |
| `gO` | Symbols in this file |
| `[d` / `]d` | Previous / next diagnostic |
| `Space e` | Show the diagnostic under the cursor |
| `Space f` | Format the file |
| `F5` | Start debugging, or continue |
| `F9` or `Space b` | Toggle a breakpoint |
| `F10` / `F11` / `F12` | Step over / into / out |

To use a server TermOS does not install, such as a system clangd on aarch64,
add it to `lua/user.lua`:

```lua
vim.lsp.config("clangd", { cmd = { "clangd" }, filetypes = { "c", "cpp" } })
vim.lsp.enable("clangd")
```

### Helix

With `editor = "helix"`, TermOS installs Helix through mise and writes its
language servers into Helix's `languages.toml`. If you already have your own
`languages.toml`, TermOS leaves it alone and writes `languages.termos.toml`
beside it for you to merge.

## Projects

`termos create TEMPLATE [DIR]` makes a small working project in a new or empty
folder (the current one by default). `termos create --list` shows the
templates (one per language, in the table above). Each project gets a
`mise.toml` pinning the version you have installed, which TermOS marks as
trusted for mise because it wrote it.

The files come with TermOS, so a project builds the same next year. Zig and
.NET projects come from their own generators (`zig init`, `dotnet new
console`).

## Build, run and test

TermOS finds a project's build, run and test commands from its files, in the
nearest folder up from where you are:

| Files | build | run | test |
| --- | --- | --- | --- |
| `Cargo.toml` | `cargo build` | `cargo run` | `cargo test` |
| `go.mod` | `go build ./...` | `go run .` | `go test ./...` |
| `deno.json` | | `deno task start` | `deno test` |
| `package.json` | `npm run build` (if there is a build script) | `npm run start` | `npm test` |
| the same, with `bun.lock` | `bun run build` | `bun run start` | `bun test` |
| `pyproject.toml`, `main.py` | | `python3 main.py` | `python3 -m unittest` |
| `CMakeLists.txt` | `cmake -S . -B build && cmake --build build` | build, then the program | `ctest --test-dir build` |
| `*.csproj`, `*.sln` | `dotnet build` | `dotnet run` | `dotnet test` |
| `build.gradle(.kts)` | `gradle build` (or `./gradlew`) | `gradle run` | `gradle test` |
| `pom.xml` | `mvn -q package` | | `mvn -q test` |
| `build.zig` | `zig build` | `zig build run` | `zig build test` |
| `Main.java` | `javac -d out Main.java` | `java Main.java` | |
| `main.kt` | `kotlinc …` | build, then `java -jar main.jar` | |
| `main.lua` | | `lua main.lua` | |
| `main.sh` | | `sh main.sh` | `shellcheck main.sh` |

A `termos.toml` in the project folder changes them and adds your own:

```toml
[tasks]
test = "go test -race ./..."
lint = "go vet ./..."
```

- **In a terminal:** `termos task` lists them; `termos task NAME` runs one.
- **On the desktop:** the palette's "Build this project", "Run this project"
  and "Test this project" (`task.build`, `task.run`, `task.test`), or
  `task NAME` for your own, open a panel at the bottom of the screen running
  the task in the focused pane's project. It stays open until you press
  Enter, and the task's next run replaces it.

## Git

Prefix `g` (or "Git (lazygit)" in the palette, `git.open`) opens
[lazygit](https://github.com/jesseduffield/lazygit) in a floating window, in
the focused pane's folder; lazygit finds the repository that folder is in.
TermOS installs lazygit through mise the first time. `termos git` does the same in the current terminal.
