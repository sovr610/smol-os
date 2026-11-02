# AI agents

An agent window is a conversation with a language model that can look at and
change your project: read files, search code, edit, run commands and fetch
pages. Every tool call goes through a permission profile, risky ones always
ask you first, commands run in a sandbox, and everything the agent does is
written to a tamper-evident audit log.

TermOS talks to the providers' APIs directly: Anthropic, OpenAI, Moonshot's
Kimi, and anything OpenAI-compatible (Ollama, vLLM, LM Studio, OpenRouter, and
TermOS's own [model router](models.md) for models on your machine). Models are never built in: the
window lists what your provider offers.

You can also open Claude Code, Codex CLI and other command-line agents in
TermOS windows ([external agents](#external-agents)).

## Quick start

1. Add a provider to `~/.config/termos/config.toml`:

   ```toml
   [agents.providers.anthropic]
   kind = "anthropic"
   key = "keyring:anthropic"
   ```

2. Store its key (it is asked for without echo):

   ```sh
   termos secret set anthropic
   ```

3. In TermOS press `C-b a` (or the palette: "agent"). Pick a model from the
   list, then say what you want done.

`termos agent models anthropic` lists the models from the shell, which also
checks the key.

## The window

```text
╭─ Agent · anthropic <model> ───────────────────────────────────────────╮
│ default · anthropic <model> · 12.4k in (9.1k cached) / 840 out · $0.02 │
│ › The tests fail. Please fix the bug.                                 │
│   Let me run the tests first.                                         │
│   ✓ $ cargo test                                                      │
│       … the command's output, as it runs …                            │
│   ✓ read src/lib.rs                                                   │
│   ? edit src/lib.rs                                                   │
│ ───────────────────────────────────────────────────────────────────── │
│ Allow? edit src/lib.rs                                                │
│ It asks because the profile asks before write.                        │
│ -    a - b                                                            │
│ +    a + b                                                            │
│ y allow · a allow for this session · n refuse · Esc stop the turn     │
│ › ▏                                                                   │
╰───────────────────────────────────────────────────────────────────────╯
```

- **Enter** sends; **Alt+Enter** (or Ctrl+J) starts a new line; pasting works.
- **Esc** stops the current turn (the reply, or a running command). With
  nothing running and nothing typed, Esc closes the window. The conversation
  is saved either way.
- **PgUp / PgDn** or the mouse wheel scroll.
- When a call asks: **y** allows it once, **a** allows it for the rest of the
  session (not offered for calls that always ask), **n** refuses it (the model
  is told), **Esc** stops the turn. File changes show as a diff.
- The header shows the tokens used, the cost when the model's prices are known,
  and what sandbox commands run in.

The workspace, the folder the agent works in, is the focused pane's folder
when you open the window.

**Resuming:** `C-b A` (or "agent.resume") lists saved conversations; Enter
continues one. `termos agent history` lists them in the shell. Conversations
are kept in `~/.local/state/termos/agents/` (mode 0600, tool output redacted);
`[agents] history = false` keeps none.

## Providers

```toml
[agents.providers.<name>]
kind = "anthropic"        # anthropic | openai | kimi | openai-compatible | local
key = "keyring:<secret>"  # not needed for local servers
base-url = "…"            # each kind but openai-compatible has one (local: the model router)
max-tokens = 32000        # most tokens per reply (default: the model's limit, at most 64000)

[agents.providers.<name>.prices."<model id>"]   # US$ per million tokens
input = 3.0
output = 15.0
cache-read = 0.3          # optional
cache-write = 3.75        # optional
```

| `kind` | API | Default address |
| --- | --- | --- |
| `anthropic` | Messages API (version 2026-10-21), prompt caching on | `https://api.anthropic.com` |
| `openai` | Chat Completions | `https://api.openai.com/v1` |
| `kimi` | Chat Completions; its reasoning is sent back with its tool calls | `https://api.moonshot.ai/v1` |
| `openai-compatible` | Chat Completions: Ollama, vLLM, LM Studio, OpenRouter… | none: set `base-url` |
| `local` | TermOS's [model router](models.md), started when needed | found by itself (or set `base-url`) |

Examples:

```toml
[agents.providers.ollama]
kind = "openai-compatible"
base-url = "http://127.0.0.1:11434/v1"

[agents.providers.openrouter]
kind = "openai-compatible"
base-url = "https://openrouter.ai/api/v1"
key = "keyring:openrouter"

# Models you downloaded with `termos model pull` (docs/models.md).
[agents.providers.local]
kind = "local"
```

Costs are shown only for models with known prices: from `prices` in the config,
or from the provider's model list where it has them (OpenRouter's does). When a
provider cannot list its models, type the model's id in the picker.

## Keys

API keys never go in the config. It names them, `keyring:<name>`, and they are
kept in:

- **the OS keyring**: the Secret Service on Linux desktops (GNOME Keyring,
  KWallet), the Keychain on macOS, the Credential Manager on Windows;
- or, where there is no keyring (bare metal, servers, containers), **the
  secrets file** `~/.local/share/termos/secrets.age`: an
  [age](https://age-encryption.org) file locked with a passphrase you choose.
  The first agent window asks for the passphrase, and the file stays unlocked
  until the server stops.

```toml
[secrets]
store = "auto"   # auto | keyring | file
```

`auto` uses the secrets file if there is one, else the keyring when it works,
else the file.

```sh
termos secret set NAME      # asks for the value (or reads it from stdin)
termos secret rm NAME
termos secret list          # which keys the config names, and whether each is set
```

Keys are kept out of logs, audit records, saved conversations and snapshots.
Agents' commands do not inherit environment variables that look like
credentials (`*_KEY`, `*_TOKEN`, `*SECRET*`, `AWS_*`, `SSH_AUTH_SOCK`…).

## Profiles and permissions

A profile says which provider and model to use and what the agent may do.
`agent.open` uses `[agents] default-profile` ("default"); `agent.open NAME`
picks another.

```toml
[agents.profiles.default]
provider = "anthropic"         # default: the only provider configured
model = ""                     # empty: pick from the provider's list
instructions = "Prefer small commits. Run cargo fmt before you finish."
tools = { read = "allow", write = "ask", command = "ask", network = "ask", mcp = "ask" }
read-paths = ["~/notes"]       # readable besides the workspace
write-paths = []               # writable besides the workspace
allow-commands = ["cargo test*", "cargo build*", "git status", "git diff*"]
deny-commands = ["git push*"]
domains = ["docs.rs", "*.rust-lang.org"]   # fetch_url; empty: any, after asking
mcp-servers = []
sandbox = "auto"               # auto | required | off
```

Each class of tool is `allow`, `ask` or `deny`:

| Class | Tools |
| --- | --- |
| `read` | `read_file`, `list_dir`, `search_code` |
| `write` | `write_file`, `edit_file` |
| `command` | `run_command` |
| `network` | `fetch_url`, and commands that ask for the network |
| `mcp` | MCP servers' tools |

Whatever the profile allows, these **always ask** (and `deny` still refuses):

- deleting files (`rm`, `find -delete`…), discarding changes (`git reset --hard`,
  `git checkout -- …`), rewriting history, force-pushing, pushing;
- installing or removing packages (`apt`, `pip install`, `npm install`,
  `cargo install`…), running a downloaded script (`curl … | sh`);
- running as another user (`sudo`), writing disks, stopping processes, changing
  the system;
- writing outside the workspace and `write-paths`, and reading or writing
  credentials (`~/.ssh`, `~/.aws`, keyrings…);
- anything on the network, except `fetch_url` to a domain in `domains` when
  `network = "allow"`;
- every command, when the sandbox is missing or partial.

`allow-commands` patterns match the whole command line (`*` matches anything).
A chained command (`a && b | c`) is allowed only if each part is, and one with
a command substitution (`$(…)`, backticks) always asks. TermOS's own secrets
file is never read or written by tools.

## The sandbox

On Linux, `run_command` runs under **bubblewrap**:

- new mount, PID, IPC and network namespaces;
- the whole system read-only; the workspace and `write-paths` read-write; a
  private `/tmp`;
- `~/.ssh`, `~/.gnupg`, `~/.aws`, keyrings and similar folders hidden, and the
  runtime folder (D-Bus, ssh-agent and TermOS's own sockets) hidden;
- no network unless the call asked for it and you agreed.

Inside it, TermOS adds a **Landlock** ruleset: writes only beneath the
writable folders, no TCP without the network, no signals to or abstract
sockets of processes outside.

```sh
termos sandbox status                  # what this machine offers
termos sandbox run -- make test        # a command in the sandbox, this folder writable
termos sandbox run --net -- cargo fetch
```

Install bubblewrap (`apt install bubblewrap`, `dnf install bubblewrap`). On
Ubuntu 24.04 and later, unprivileged user namespaces are restricted by
AppArmor; the `bubblewrap` package normally allows it, otherwise see
`sysctl kernel.apparmor_restrict_unprivileged_userns`.

Without bubblewrap, Landlock alone still confines writes and blocks TCP, but
reads are not hidden and UDP is not blocked, so every command asks. Without
either (macOS, Windows, kernels before 5.13) commands run as they are and every
one asks, with a warning in the window. `sandbox = "required"` refuses to run
commands without bubblewrap; `sandbox = "off"` runs them unconfined (they still
ask as the profile says).

Commands run on a pseudo-terminal, so colours and progress bars look as they do
in a pane; the window shows the output as it comes, the model gets it as plain
text (its last 30 KiB). The default timeout is two minutes, at most thirty.

## The audit log

Each session writes `~/.local/state/termos/audit/<session>.jsonl` (mode 0600):
one line per event (the session starting, every tool call with its redacted
arguments, the decision and why, a summary of the result), each holding the
SHA-256 of the line before it. Editing, removing or reordering a line breaks
the chain; the conversation file keeps the last hash, so cutting lines off the
end shows too.

```sh
termos audit list            # every log, checked
termos audit verify [ID]     # exits non-zero if a log was tampered with
termos audit show ID         # the entries, as JSON lines
```

The viewer ("audit.open" in the palette) lists the logs with their check and
shows each one's entries.

## MCP servers

```toml
[agents.mcp.files]
command = ["npx", "-y", "@modelcontextprotocol/server-filesystem", "."]
network = false          # its sandbox may reach the network
write-paths = []         # writable besides the workspace

[agents.mcp.tracker]
url = "https://mcp.example.com/mcp"   # streamable HTTP
key = "keyring:tracker"               # sent as a bearer token

[agents.profiles.default]
mcp-servers = ["files", "tracker"]
```

Only servers the profile lists start, when a session starts, and they stop
with it. Local servers run in the sandbox, like commands. Their tools appear
to the model as `mcp__<server>__<tool>` and follow the `mcp` rule.

## External agents

`agent.external NAME` (palette, or `termos msg agent.external claude`) opens a
command-line agent in a floating terminal window, in the focused pane's folder:

| Name | Program |
| --- | --- |
| `claude` | Claude Code (`claude`) |
| `codex` | Codex CLI (`codex`) |
| `gemini` | Gemini CLI (`gemini`) |
| `aider` | aider |
| `opencode` | opencode |

Change one or add your own:

```toml
[agents.external.claude]
sandbox = true                        # the workspace and write-paths writable, network on
write-paths = ["~/.claude", "~/.claude.json"]

[agents.external.mine]
command = ["my-agent", "--tui"]
description = "My agent"
```

In the sandbox they keep their environment (their own keys) but see the system
read-only, can write only the workspace and their own settings folders, and do
not see your other credentials. `termos agent presets` lists them.

## Limits

- Conversations are not compacted: a very long one eventually exceeds the
  model's context; start a new one.
- The agent sees text, not images.
- On Windows there is no sandbox yet (milestone M9b); on macOS commands are
  confirm-only.
