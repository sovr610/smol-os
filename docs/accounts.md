# AI accounts and usage

Keep several accounts of Claude Code and Codex CLI, three Claude
subscriptions say, switch between them in a click, carry a conversation
from one to the other, and see what each has used on one window.

## Accounts

Each account is a profile: a folder of its own that holds its login, its
conversations and its settings. `default` is the CLI's usual folder
(`~/.claude`, `~/.codex`), so the login you already have keeps working.

```bash
termos account add claude work --label "Work (Max)"
```

That makes the folder, shares your settings, instructions (`CLAUDE.md`,
`AGENTS.md`) and skills into it, registers TermOS's MCP server there, and
offers to sign in. Sign in later with `termos account sign-in claude work`.
Your login is never shared or copied between accounts.

```bash
termos account list                       # every account; ● marks the one in use
termos account use claude work            # new windows use it
termos account label claude work "Work"   # what the menus show
termos account remove claude work         # its folder, login and conversations
```

The folders are in `~/.local/share/termos/accounts/` (Windows:
`%LOCALAPPDATA%\termos\data\accounts`). Settings are shared by a link;
on Windows without Developer Mode they are copied instead, so later
changes to the default's settings do not reach the others.

## Switching

- **For new windows:** ◆ Menu → AI agents → Accounts, the usage app's
  **[Use]**, the palette (`account.use claude work`) or
  `termos account use claude work`. `claude` and `codex` typed in any TermOS
  window follow it too.
- **For one window:** set `TERMOS_ACCOUNT_CLAUDE=work` (or
  `TERMOS_ACCOUNT_CODEX`) before starting it.
- **For a running conversation:** right-click a Claude Code or Codex
  window → **Continue as work** (`agent.swap work`). The window ends, and a
  new one in its place runs as `work`, continuing the conversation you had
  in that folder: TermOS copies it into the other account (the first keeps
  its copy) and starts the CLI resuming it.

You choose when to switch. TermOS does not switch accounts by itself when
one reaches its plan's limit: rotating accounts to get around limits can
break the providers' terms.

If you set `CLAUDE_CONFIG_DIR` or `CODEX_HOME` yourself, that folder wins
over the account in use.

## The usage app

◆ Menu → AI agents → **Usage and accounts**, `usage.open` in the palette,
or `tos usage`. For every account of each CLI:

- **Its plan's limits**, as the CLI itself reports them: how much of the
  5-hour and the weekly limit is used, and when each resets. Codex writes
  these into its logs, with the plan's name. Claude Code passes them only
  to its status line: **[Show limits]** (or
  `termos account statusline claude on`) puts TermOS's status line command
  into Claude Code's settings, which keeps the figures for the usage app.
  A status line of your own stays and still shows; `off` puts things back.
  Only subscriptions have these limits.
- **Its 5-hour window and week against a budget**, when the CLI reports no
  limits: set one with **[Budget]** or
  `termos account budget claude work --five-hour 2M --week 15M`. Nothing
  is enforced; it is a gauge. Claude Code's window starts with your first
  message after the last window ended (counted from that hour).
- **Its tokens** today and over 7 days, its main model, a 7-day sparkline
  and who it is signed in as. Tokens are input, output and cache writes;
  cache reads, a fraction of the price, are shown apart.
- **Buttons:** **[Use]**, **[Open]** (a Claude Code or Codex window as that
  account), **[Sign in]**, **[Budget]**, **[Add account]**. Keys: `↑` `↓`
  to select, `u` use, `o` open, `s` sign in, `b` budget, `a` add, `r`
  refresh, `q` close.

Below them, TermOS's own agents by provider, with their cost where prices
are known. The command center counts every account's tokens too.

## What TermOS reads

The numbers come from the logs the CLIs keep in each account's folder
(Claude Code's `projects/`, Codex's `sessions/`): token counts, models and
times, never the text. TermOS never reads a login and calls no vendor's
usage service. `[center] cli-usage = false` turns their usage off.
