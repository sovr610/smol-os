# Skills for Claude Code, Codex and the rest

TermOS teaches the coding CLIs you run in it what TermOS can do for them.
It does this with *skills*: short instructions that Claude Code, Codex CLI,
Gemini CLI and opencode load when a task calls for them. With them, an agent
starts your dev server in a pane beside its own instead of blocking on it,
reads that pane when it needs the output, opens a file or picture for you,
tells you when a long job is done, and can hand a question to one of your
own models.

## The skills

| Skill | What it teaches |
| --- | --- |
| `termos` | Working inside TermOS: servers, watchers and REPLs in panes of their own (`open_pane`, `tos run -p`), reading them (`read_pane`, `tos read`), opening files for you, notifications, the clipboard, TermOS's commands |
| `termos-dev` | Languages through `termos lang`, project templates, a project's build, run and test tasks, your editor and the Git UI (as panes, for you) |
| `termos-agents` | Asking TermOS's own agent or a local model with `termos agent ask`; your accounts and usage, which only you switch |

Each skill tells the agent to check that it runs inside TermOS (`TERMOS` is
set) first, so the skills do nothing in other terminals. `termos skills show
termos` prints one.

The agents use TermOS through its MCP server where they have it (Claude Code
and Codex, after `termos agent install`; see [agents.md](agents.md)) and
through the `tos` command otherwise ([tos.md](tos.md)).

## Where they go

- **Claude Code:** each account's `skills` folder: `~/.claude/skills` for the
  default account, and the folder of every other account you added
  ([accounts.md](accounts.md)). Accounts that share the default's folder
  (the usual case) share the skills.
- **Everyone else:** `~/.agents/skills`, the shared folder of the Agent
  Skills format, which Codex CLI, Gemini CLI and opencode read. opencode also
  reads `~/.claude/skills`, so it may list a TermOS skill twice; the two
  copies are the same.

TermOS installs them when you install Claude Code or Codex with `termos agent
install`, when you add an account, and when TermOS starts, as long as a CLI
that reads the folder is there. Starting TermOS also updates them after you
update TermOS.

## Your skills stay yours

TermOS writes a `.termos-skill` file into each skill's folder, with a checksum
of every file it wrote. It changes only folders that have that file and whose
files still match it.

- A skill of your own with one of these names is never touched.
- If you edit TermOS's copy, TermOS keeps your version.
  `termos skills install --force` replaces it.
- Files you add to a skill's folder stay when TermOS removes its own.

## Commands

```bash
termos skills status          # each folder, who reads it, each skill's state
termos skills install         # install or update them everywhere (and keep doing so)
termos skills install --force # also replace copies you changed
termos skills remove          # remove TermOS's copies, and stop installing them
termos skills show termos-agents
```

After `termos skills remove`, TermOS leaves them out until you run
`termos skills install`. To keep TermOS from installing them in the first
place, set this in `config.toml`:

```toml
[agents.skills]
install = false
```

## `termos agent ask`

The `termos-agents` skill lets a CLI hand a question to TermOS's own agent:
your API providers or a model on this machine, which costs nothing. You can
use it yourself too:

```bash
termos agent profiles                         # each profile's provider and model
termos agent ask --profile local "What does src/net/retry.rs do?"
git diff | termos agent ask --profile review "Review this diff:" -
termos agent ask --model MODEL_ID --json "…"   # with tokens, cost and the session
```

It is read-only. The agent may read, list and search the files in the
current folder (`--workspace DIR` for another), and nothing else: it cannot
change files, run commands, use the network or start MCP servers. A call
that would need your approval is refused, since no one is there to give it.

Like any agent session, the run is audited, its tokens and cost go into the
command center's history, and the conversation is saved (`termos agent
history`), so you can continue it in a window.

If the profile names no model, pass `--model`. The error lists the
provider's models. If the API key is in the secrets file, `ask` asks for the
passphrase when run in a terminal. A CLI cannot type it, so for agents keep
the key in the OS keyring (`[secrets] store = "keyring"`) or use a local
model.
