# The harness: one coding tool over Claude Code, Codex and API models

Give the harness a goal ("add retries to the HTTP client"). A planning model
splits it into tasks: searching, coding, tests, review and so on. Each task
goes to the backend and model that suit it:

- Claude Code or Codex CLI, headless, under your subscriptions first;
- an API model through TermOS's own agent, when your settings say so or
  when a CLI can't take the task.

You see the plan and change it if you like. Then it runs: tasks that change
files one at a time, the others alongside. Every hand-off is shown and
recorded.

## Opening it

- ◆ Menu → AI agents → **Harness: plan a goal**, or `harness.open` in the
  palette (`harness.open add a --verbose flag` plans that goal).
- `tos harness` in a pane opens it for that pane's folder;
  `tos harness add a --verbose flag` plans the goal at once.
- From any terminal: `termos harness run "add a --verbose flag"` plans,
  shows the plan, asks, and runs it there (see [Commands](#commands)).

The project is the folder of the pane you open it from.

## The window

```
 Goal › add retries to the client                                     [Plan ⏎]
 Plan: Retry failed requests · 3 tasks — change them if you like, then Run (r)
──────────────────────────────────────────────────────────────────────────────
 ✓ t1  search      Find the HTTP calls  │ t2 · coding · Add retries
 ◐ t2  coding      Add retries          │ after t1 · changes files
 ○ t3  review      Review the change    │
                                        │ Running on
                                        │ Codex CLI (its default model)
                                        │ ✗ Claude Code: usage limit reached
                                        │
                                        │ Prompt
                                        │ Retry failed requests three times…
 [Stop task s] [Stop all S]                                Tab: goal · q: close
```

- **Goal line:** type the goal and press Enter. `Tab` moves between the
  goal and the tasks.
- **Tasks:** `↑` `↓` select one. The right side shows where it goes and
  why, its prompt, what it did (files read and changed, commands run) and
  its answer. `PgUp` `PgDn` or the wheel scroll it.
- **While the plan waits:**
  - `r` runs it;
  - `e` edits the selected task's prompt;
  - `t` changes its kind;
  - `b` picks its backend by hand (once per backend, then back to automatic);
  - `d` drops it;
  - `K` and `J` move it up and down (as long as it stays after the tasks
    it needs);
  - `n` starts over.
- **While it runs:** `s` stops the selected task, `S` stops everything.
  The CLIs and the programs they started are stopped.
- **When it is done:** `c` copies the selected task's answer, `n` starts a
  new plan.

Every key has a button at the bottom.

## How a plan runs

**Kinds and tiers.** Each task has a kind, and each kind has a tier: how
strong (and how costly) a model it gets. These are the defaults, drawn from
the research in [model-guide.md](model-guide.md):

| Kind | What it is for | Tier |
| --- | --- | --- |
| `planning` | deciding the approach for a hard problem | frontier |
| `coding` | writing or changing code | balanced |
| `review` | looking for bugs and risks in changes | frontier |
| `tests` | writing or fixing tests, and running them | balanced |
| `search` | finding where things are and how they work | fast |
| `small-edits` | a rename, a value, a one-line fix | fast |
| `docs` | documentation and comments | fast |

**Which backend.** The backends are tried in order: Claude Code, then
Codex CLI, then each API backend. Subscriptions come first because they
cost nothing per token. Each backend fills the tier with the model you set
for it. Without one, a CLI uses its own default model, and an API backend
uses its nearest tier that has a model.

A backend is passed over when:

- it is not installed;
- it has no model for any tier;
- the plan's API budget is spent;
- its plan's limit was reached earlier in this run.

**Fallback.** When a task fails on a backend, it goes to the next one. When
Claude Code or Codex reaches its plan's usage limit, it is not used again in
the plan. The harness never moves to another account of the same CLI by
itself: switching accounts to get around a limit is your choice
([accounts.md](accounts.md)). Use `[harness] fallback = false` to stop at
the first failure.

**Reviews by someone else.** A review goes first to a backend other than
the one that wrote the code it reviews: Codex reviews Claude Code's work,
and the other way round. Models tend to favour their own output. The
evidence for this is general research on models judging their own work,
not a study of code review ([model-guide.md](model-guide.md#4-recommended-task-type--tier-defaults)).
Set `[harness] cross-review = false` to turn it off.

**Order.** A task starts when the tasks it depends on are done. Tasks that
change files (coding, tests, small edits, docs) also wait for every earlier
task that changes files, and run one at a time in your project folder.
Read-only tasks (planning, review, search) run alongside them, up to
`[harness] parallel` at once. A task whose dependency failed is not run.

**What a task is told.** Its prompt, which plan it belongs to, the project
folder, and the answers of the tasks it builds on. Agents end with a short
summary of what they did, which the next tasks see.

## What the agents may do

No one is there to approve anything while a plan runs, so the harness gives
each backend a fixed set of permissions:

| Backend | Tasks that change files | Read-only tasks |
| --- | --- | --- |
| Claude Code (`claude -p`) | `--permission-mode acceptEdits`, and `--allowedTools` for the allowed commands | editing and shell tools taken away (`--disallowedTools`) |
| Codex CLI (`codex exec`) | its `workspace-write` sandbox: the project writable, no network | its `read-only` sandbox |
| API (TermOS's agent) | read and change files in the project; run only the allowed commands, in TermOS's sandbox | read, list and search files |

Anything else the agent tries is refused and shown in the task's log ("not
allowed: …").

The allowed commands are the project's own build, test, lint, check and
format tasks (`termos task`, [languages.md](languages.md#build-run-and-test)),
plus any in `[harness] allow-commands`. Dangerous commands, and commands
that need the network, are refused even when they match.

## Settings

All of these are optional. `termos harness status` shows the result: each
backend, whether it can run, its models, where each kind of task goes, and
the allowed commands.

```toml
[harness]
order = ["claude", "codex", "anthropic"]   # backends in the order they are tried
planner = "claude"           # the backend that plans (default: the planning route's)
fallback = true              # try the next backend when one fails
cross-review = true          # reviews go first to another backend than the author's
parallel = 3                 # read-only tasks at the same time
max-tasks = 8                # the most tasks a plan may have
task-minutes = 30            # a task running longer is stopped
budget-usd = 2.0             # what API backends may spend on one plan (0: no limit)
auto-run = 0                 # plans of up to this many tasks start without asking
allow-commands = ["make check", "npm run lint *"]
allow-project-tasks = true

# The model behind each tier, per backend. These are examples from
# docs/model-guide.md (November 2026); check what your plan and provider
# offer. An unset tier uses the CLI's own default model.
[harness.backends.claude]
account = "work"             # default: the account in use
models = { frontier = "opus", balanced = "sonnet", fast = "haiku" }

[harness.backends.codex]
models = { frontier = "gpt-6-astra", balanced = "gpt-6.1-sol", fast = "gpt-6-luna" }

# Every provider under [agents.providers] is an API backend of the same
# name; give it models to use it.
[harness.backends.anthropic]
models = { frontier = "claude-opus-5-5", balanced = "claude-sonnet-5-5", fast = "claude-haiku-5-5" }

# A second Codex account as a backend of its own. Once a plan reaches
# Codex's limit, no Codex backend is used again in that plan.
[harness.backends.codex-team]
kind = "codex"
account = "team"

# Which tier each kind gets, and optionally which backends to try.
[harness.routes]
coding = "frontier"
review = { tier = "frontier", backends = ["codex", "anthropic"] }
search = { tier = "fast", backends = ["anthropic"] }
```

- `disabled = true` on a backend keeps it out.
- A route's `backends` list replaces the order for that kind.
- A task you gave a backend in the window goes only there.

## Commands

```bash
termos harness status                      # backends, models, routes, allowed commands
termos harness plan "add a --verbose flag" # a plan, printed (nothing runs)
termos harness plan --json "…" > plan.json # the same, as JSON
termos harness run "add a --verbose flag"  # plan, show, ask, run
termos harness run --yes --plan plan.json  # run a plan as it is, without asking
```

`--project DIR` works on another folder. `termos harness run` prints each
hand-off and step as it happens, and each task's answer at the end. It
exits with an error when a task did not finish, and Ctrl-C stops the run.

## What is recorded

- **The audit log.** Every hand-off goes into a hash-chained log in
  TermOS's audit folder (`termos audit show harness-<id>`; the id is shown
  at the end of a run): the goal, the plan, and for each task the backend,
  model and account it went to, what came of it, and why it moved on.
  `termos audit verify` checks it with the agents' logs.
- **The command center.** The journal gets the same hand-offs (names and
  outcomes only). The CLIs' own logs give their tokens, and TermOS's agent
  records its usage, so the command center counts each task once.
- **Saved sessions.** API tasks are saved conversations
  (`termos agent history`).

**Costs.** The window and `termos harness run` show the tokens used and the
dollars API backends spent, which counts toward `budget-usd`. Claude Code
reports an estimated cost even under a subscription. The harness shows it
as an estimate and doesn't count it, since your plan doesn't charge per
token.

## Limits

- **Secrets file:** if the API keys are in a passphrase-locked secrets
  file, `termos harness run` asks for the passphrase in the terminal. The
  window uses the server's secrets, unlocked once for every agent window.
- **Windows:** stopping a task there stops the CLI itself, but not the
  programs it started.
- **Edits run one at a time** in your project folder. Parallel edits in
  separate git worktrees are not supported.
