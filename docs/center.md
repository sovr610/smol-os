# The command center

One window over everything TermOS runs: the AI agents and what they cost,
and the machine and its sessions. It shows what is happening now, keeps a
history, and lets you act.

Open it with any of these:

- the menu: **◆ Menu → Command center**;
- `C-b C`;
- `tos center` in any terminal;
- `center.open` in the command palette.

Switch tabs with a click, the number keys `1` to `4`, or Tab. Every action is
a button you can click and a key.

## Overview

The first tab has the most important things on one screen:

- **AI:** agents running, and any that wait for your approval, with
  **[Approve]** and **[Deny]** right there;
- **Usage:** tokens and cost over the chosen range, and tool calls (and how
  many were refused);
- **Audit and models:** whether every audit log's chain verifies, and how
  many local models are installed and whether their router runs;
- **System:** CPU and memory, network traffic, the busiest processes, and
  how many sessions, panes and terminals are attached;
- **Charts:** CPU and tokens over the chosen range.

## AI

- **Running agents:** each session's profile, provider and model, state,
  tokens and cost. Buttons: **[Approve]** / **[Deny]** a call waiting for
  you, **[Stop]** a running turn, **[Audit]** for that session's audit log.
  Keys: `a` approves, `d` denies, `s` stops.
- **Usage by model** over the range: turns, tokens in and out, cost.
- **Tool calls** by decision (allowed, approved, denied, refused,
  cancelled) and the most used tools.
- **Audit:** every session's hash chain, checked; a broken chain is named.
- **Local models:** the router's address when it runs, and the models in
  the cache.
- **MCP servers** from your config, and **CLI agents** (Claude Code, Codex
  and the rest), with how many windows run each; a click opens one.

Costs come from the prices your providers list or that you set
(`[agents.providers.<name>.prices]`); without a price, tokens are shown and
the cost is "–".

## System

- **Processes**, busiest first. Select one with a click or `↑` `↓`, then
  **[End]** (`e`). TermOS asks first, then asks the process to stop
  (SIGTERM; on Windows it is terminated). TermOS itself is never ended.
- **Sessions and panes** of the whole server: **[Go]** brings a pane to
  the front (switching to its workspace), **[Close]** closes it after
  asking.
- **The machine:** disks, network (**[Network]** opens the network
  window), the TermOS version (**[Updates]**), **[Monitor]** for the system
  monitor.
- **Log:** the last lines of TermOS's newest log; **[Open]** opens it in
  your editor.

## History

Pick a range: **[1 h]**, **[24 h]** or **[7 days]** (or `[` and `]`):

- CPU and memory over the range (a picture on terminals that show images,
  braille dots elsewhere);
- tokens over the range, total cost and sessions;
- usage by model and tool calls by decision;
- what was done from the command center (processes ended, panes closed).

## The history on disk

TermOS keeps the history in `~/.local/state/termos/center/` (Windows:
`%LOCALAPPDATA%\termos\center`), one file per day, readable only by you:

- agent turns' tokens and cost, by provider, model and session;
- each tool call's tool name and decision;
- sessions starting and ending;
- what was done from the command center;
- every minute, CPU, memory and network use, also while the command center
  is closed (one short wakeup a minute).

It never holds what you or an agent typed, a tool's arguments, or a
secret.

```toml
[center]
keep-days = 7   # 1 to 365
sample = "1m"   # "1m", "5m", or "off" to not record the machine
```

Both are in Settings too, under Command center.
