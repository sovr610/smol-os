# TermOS

**A terminal-first operating system.** The whole desktop lives in the
terminal, with movable, resizable and overlapping windows, workspaces, a
command palette, built-in apps and high-end terminal graphics. Think "tmux
grown into a full desktop environment". It's developer-first, with
toolchains, editor and LSP/DAP built in, and AI-native, with sandboxed agents
and local model inference.

TermOS runs in two modes:
- **Hosted:** a normal program inside any terminal emulator on Linux, macOS or WSL2.
- **Bare metal:** a Linux + Buildroot image that boots straight into TermOS and draws to the display through DRM/KMS. Targets are x86_64, aarch64 and Raspberry Pi.

## Status

**Milestone M1: a tmux-style desktop in hosted mode.** You get:
- panes with splits and layout presets;
- floating windows you can drag, resize and snap;
- workspaces;
- a command palette, key hints and copy mode;
- full mouse support;
- live-reloading configuration.

Detach and reattach come next, in M3. See [ROADMAP.md](ROADMAP.md).

## Try it

TermOS needs the Rust toolchain pinned in `rust-toolchain.toml`; rustup
installs it automatically. It runs on Linux, macOS or WSL2.

```bash
cargo run --release -p termos-cli
```

Press **Ctrl+B**, then:
- **%** splits the pane;
- **f** floats it;
- **c** opens a new workspace.

**Ctrl+Space** opens the command palette. Pause after Ctrl+B and a panel lists
every key. [docs/getting-started.md](docs/getting-started.md) has the full
guide.

## Build and test

```bash
cargo xtask ci
```

That runs everything CI runs: fmt, clippy, tests and the repo checks.

```bash
cargo xtask perf
```

That checks the latency, idle-CPU and memory budgets on a release build
(Linux only).

## Documentation

- [docs/getting-started.md](docs/getting-started.md): using TermOS.
- [docs/agents.md](docs/agents.md): AI agents: providers, keys, permissions, the sandbox, audit logs.
- [docs/models.md](docs/models.md): local models: Hugging Face downloads, what fits your machine, llama.cpp, the model router.
- [ARCHITECTURE.md](ARCHITECTURE.md): how the system is put together.
- [ROADMAP.md](ROADMAP.md): milestones M0–M9 and open work.
- [DECISIONS.md](DECISIONS.md): the decision log and open questions.
- [CLAUDE.md](CLAUDE.md): contributor conventions.

## License

TermOS is free software: you can redistribute it and/or modify it under the
terms of the GNU General Public License as published by the Free Software
Foundation, either version 3 of the License, or (at your option) any later
version. See [LICENSE](LICENSE).
