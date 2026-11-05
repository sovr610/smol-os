# AI accounts and usage

Keep several accounts of Claude Code, Codex CLI and Gemini CLI, three
Claude subscriptions say, sign each in once from TermOS, switch between
them in a click, carry a conversation from one to the other, and see what
each has used on one window.

## Accounts

Each account is a profile: a folder of its own that holds its login, its
conversations and its settings. `default` is the CLI's usual folder
(`~/.claude`, `~/.codex`, `~/.gemini`), so the login you already have
keeps working.

```bash
termos account add claude work --label "Work (Max)"
```

That makes the folder, shares your settings, instructions (`CLAUDE.md`,
`AGENTS.md`) and skills into it (TermOS's own skills too,
[skills.md](skills.md)), registers TermOS's MCP server there, and offers to
sign in. Sign in later with `termos account sign-in claude work`.
Your login is never shared or copied between accounts.

## Signing in

Sign each account in once; from then on every TermOS window starts that
CLI already signed in to the account in use, and switching accounts
switches the subscription with it.

```bash
termos account sign-in claude work     # your Claude subscription
termos account sign-in codex work      # your ChatGPT plan
termos account sign-in gemini work     # your Google account
termos account status                  # who each account is signed in as
```

The usage window's **[Sign in]** does the same. TermOS runs each vendor's
own sign-in in the account's folder, and the CLI keeps the login there:
TermOS never sees or stores your password or your subscription's tokens
(the vendors' terms ask for exactly this).

- **Claude Code** (`claude auth login`) shows a link: open it in any
  browser, on any device, sign in, and paste the code you get back.
  `--console` signs in to an Anthropic Console account, `--sso` with single
  sign-on.
- **Codex** (`codex login`) opens your browser. Where there is none (over
  SSH, on TermOS's own images) it uses a device code instead, which you
  enter on any device; `--device` asks for that anywhere. A ChatGPT
  workspace may have to allow device codes first.
- **Gemini CLI** starts and signs you in with your Google account (where no
  browser opens, it shows a link and asks for the code). Type `/quit` once
  it is done.

`termos account status` asks each CLI itself (`claude auth status`,
`codex login status`); Gemini CLI has no such command, so TermOS looks for
its login file, and for the Google account it names, without reading the
login.

## API keys

An account can use an API key instead of a subscription (an Anthropic
Console key, an OpenAI key, a Gemini API key):

```bash
termos account key claude api          # asks for the key; or pipe it in
termos account key claude api --remove
```

- **Claude Code and Gemini CLI:** the key is kept in TermOS's secret store
  (your system's keyring, or TermOS's secrets file locked with a
  passphrase; [agents.md](agents.md)) under `account.<cli>.<name>`, and
  given to the CLI as `ANTHROPIC_API_KEY` or `GEMINI_API_KEY` when it
  starts in that account. Claude Code asks once whether to use it: choose
  **Yes** (it suggests No). Gemini CLI's account is set to use the key.
- **Codex** keeps the key itself: TermOS hands it to
  `codex login --with-api-key` in the account's folder.

A key comes before a sign-in, so `sign-in` asks you to remove an account's
key first. With the secrets file, starting a CLI with a key asks for its
passphrase.

**Inherited credentials:** in an account TermOS made, or one with a key,
`claude`, `codex` and `gemini` ignore credentials set in your environment
that would override the account's own (`ANTHROPIC_API_KEY`,
`CLAUDE_CODE_OAUTH_TOKEN`, `CODEX_API_KEY`, `GEMINI_API_KEY` and the
like), so the account you chose is the one that answers. `default` keeps
your environment as it is.

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
  `TERMOS_ACCOUNT_CODEX`, `TERMOS_ACCOUNT_GEMINI`) before starting it.
- **For a running conversation:** right-click a Claude Code, Codex or
  Gemini CLI window → **Continue as work** (`agent.swap work`). The window ends, and a
  new one in its place runs as `work`, continuing the conversation you had
  in that folder: TermOS copies it into the other account (the first keeps
  its copy) and starts the CLI resuming it.

You choose when to switch. TermOS does not switch accounts by itself when
one reaches its plan's limit: rotating accounts to get around limits can
break the providers' terms.

If you set `CLAUDE_CONFIG_DIR`, `CODEX_HOME` or `GEMINI_CLI_HOME` yourself,
that folder wins over the account in use.

**Gemini CLI's accounts** are homes of their own: Gemini CLI keeps its
files in `.gemini` inside `GEMINI_CLI_HOME`. Its instructions
(`GEMINI.md`), commands, skills and extensions are shared from yours; its
settings are copied (they hold TermOS's server and the sign-in method),
and its logins are kept in files in the account rather than in your
keychain, whose entries Gemini CLI would share between accounts.

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
- **Buttons:** **[Use]**, **[Open]** (a Claude Code, Codex or Gemini CLI
  window as that account), **[Sign in]**, **[API key]**, **[Budget]**,
  **[Add account]**. Keys: `↑` `↓` to select, `u` use, `o` open, `s` sign
  in, `k` API key, `b` budget, `a` add, `r` refresh, `q` close. An account
  given an API key shows "API key" where others show who is signed in.

Below them, TermOS's own agents by provider, with their cost where prices
are known. The command center counts every account's tokens too.

## What TermOS reads

The numbers come from the logs the CLIs keep in each account's folder
(Claude Code's `projects/`, Codex's `sessions/`, Gemini CLI's
`tmp/*/chats/`): token counts, models and times, never the text. TermOS
never reads a login and calls no vendor's usage service. `[center] cli-usage = false` turns their usage off.
