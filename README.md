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

**Milestone M0: planning, scaffold and CI.** There's nothing to use yet:
`termos --version` is the only working feature. See [ROADMAP.md](ROADMAP.md)
for what comes next.

## Build

TermOS needs the Rust toolchain pinned in `rust-toolchain.toml`; rustup
installs it automatically.

```bash
cargo build
```

```bash
cargo run -p termos-cli -- --version
```

```bash
cargo xtask ci
```

`cargo xtask ci` runs every check CI runs.

## Documentation

- [ARCHITECTURE.md](ARCHITECTURE.md): how the system is put together.
- [ROADMAP.md](ROADMAP.md): milestones M0–M9 and open work.
- [DECISIONS.md](DECISIONS.md): the decision log and open questions.
- [CLAUDE.md](CLAUDE.md): contributor conventions.

## License

TermOS is free software: you can redistribute it and/or modify it under the
terms of the GNU General Public License as published by the Free Software
Foundation, either version 3 of the License, or (at your option) any later
version. See [LICENSE](LICENSE).
