---
name: termos-agents
description: Hand a question to TermOS's own AI agent or a local model, and see the user's AI accounts and usage, when running inside TermOS (the TERMOS environment variable is set). Use it to get a second opinion or a cheap answer from another provider's model or a model running on this machine (termos agent ask), for read-only sub-tasks such as summarizing files, explaining code or reviewing a diff, and to check how much of their Claude Code and Codex plans the user has used.
---

# TermOS's agents, models and accounts

Check that `TERMOS` is set first. Outside TermOS these commands may still
exist, but are not set up for you.

## Asking TermOS's agent

`termos agent ask` runs TermOS's own agent once and prints its answer. It
uses the providers and models the user configured in TermOS: an API (such as
Anthropic, OpenAI, OpenRouter or Kimi) or a model running on this machine
through TermOS's model router (a local model, which costs nothing).

```bash
termos agent profiles                                   # profiles: provider and model of each
termos agent ask --profile local "Summarize what src/net does"
git diff | termos agent ask --profile review "Review this diff for bugs:" -
termos agent ask --provider openrouter --model MODEL_ID --json "…"
```

- It is **read-only**: it can read, list and search files in the folder it
  runs in (`--workspace DIR` for another), and nothing else. It cannot
  change files, run commands or use the network, and anything that would
  need the user's approval is refused. Give it the question and the context
  it needs (paths to read, or text piped in with `-`), and expect plain text
  back.
- `--json` prints the answer with the model, its tokens, its cost when known
  and the saved conversation's id. `-v` shows the files it reads on stderr.
- `termos agent models PROVIDER` lists a provider's models; `termos model
  list` lists the local ones. Pick a model only from those lists; never
  guess an id.
- If it says to pass `--model` or `--provider`, or that the secrets file is
  locked, tell the user what it said, or pick a profile or model from the
  lists above. Do not try to work around it.
- An answer can take a minute or more. Run it in the background if your
  tools allow, and do something else meanwhile.
- Use it for sub-tasks that suit a cheaper or different model, or when the
  user asks for another model's view. Check its answer before you rely on
  it; you remain responsible for the result.

Every ask is recorded in TermOS's audit log and in the command center
(`tos center`), with its tokens and cost. The conversation is saved, so the
user can continue it in an agent window (`tos ai`).

## The user's accounts and usage

TermOS keeps several accounts of Claude Code and Codex CLI (for example a
work and a personal subscription), each in its own folder.

- `termos account list` shows them, with `●` on the one in use.
- `tos usage` opens the usage window for the user: each account's plan
  limits, its tokens today and this week, and its budget.
- When the user asks how much they have used, or whether they are near a
  limit, show them `tos usage`, or read `termos account list` for them.

Switching accounts is always the user's decision. Do not run
`termos account use` or open another account's window unless they ask: an
account's limits are theirs to manage, and switching to get around a limit
can break a provider's terms.
