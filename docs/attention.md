# Agents that need you, command blocks, agents restored

With several agents at work, the one waiting for you should be easy to
find. TermOS marks it, takes you there with one key, and brings agents
back after a restart. It also tells commands apart in your shells, so
you (and agents) can jump between them, copy one command's output and
see which ones failed.

## An agent needs you

When Claude Code, Codex or Gemini CLI asks for permission or a question,
or a program sends a notification (OSC 9, 99 or 777, as `printf
'\e]9;Build done\a'` does):

- **its pane** gets a ring in the warn colour and a **● needs you**
  badge at its top right;
- **its tab** gets a **●**;
- **a toast** says who and where (`claude in 1:api needs you: wants to
  use Bash`), and the **notification center** (prefix `N`) keeps it;
- **your terminal** shows a **system notification**, where it can:
  kitty, iTerm2, WezTerm, Ghostty, Warp, foot and rxvt. Elsewhere, and
  inside tmux, there is none. `[notifications] system = false` turns
  them off.

Typing into the pane answers it, and the marks go. So does the agent
moving on.

When an agent **finishes a turn**, it is quieter: a **✓** on its tab
(unless you are looking at that workspace) and an entry in the
notification center. No toast, no system notification.

A **long command** that finishes out of sight gets a ✓ or a ✗ on its
tab too, and a toast; see [links.md](links.md#when-a-long-command-finishes).

**Prefix `j`** (`agent.waiting`, or the first item of ◆ Menu → AI agents
while one waits) goes to the agent waiting longest: first those that
need you, then finished turns. It switches workspace and brings back a
minimized window.

### How TermOS hears from the agents

The CLIs run a command at points of their work (their *hooks*). TermOS
adds `termos agent event` to each account's settings:

| CLI | File | Events |
| --- | --- | --- |
| Claude Code | `settings.json` in the account's folder (`~/.claude` for `default`) | a permission request, a question, the end of a turn, a prompt, the start and end of a conversation |
| Codex | `hooks.json` in its folder (`~/.codex`) | the same |
| Gemini CLI | `.gemini/settings.json` | the same |

- **When:** installing a CLI with TermOS, adding an account and opening
  an agent window add them. `termos agent hooks` adds them to every
  account by hand; `termos agent hooks --remove` takes them out.
  `[agents.hooks] register = false` stops TermOS adding them.
- **Your own hooks** and other settings stay as they are.
- **Codex** asks you, once, whether to trust new hooks: say yes, or
  Codex will not run them.
- **Outside TermOS** the command does nothing. It prints nothing and
  always succeeds, so it can never stop or change the agent's work.

## Agents come back after a restore

When a session is restored (after a crash or a reboot; [sessions.md](sessions.md)),
an agent window comes back as the same CLI, in the same account and
folder, and continues its conversation:

- the conversation its hooks reported, when the CLI still has it;
- else that folder's latest conversation in that account;
- else a new one.

An agent you ran in a shell (`claude` typed at a prompt) is typed into
the restored shell again, so the shell is there when it exits.

`[session] restore-agents = false` brings back shells only.

## Command blocks

TermOS starts **bash, zsh, fish and PowerShell** with a small script.
The script loads your own configuration first (`~/.bashrc`, your
`ZDOTDIR`, your profile), then marks each prompt, command line, output
and exit status with the OSC 133 sequences that other terminals use
too. Your dotfiles are not changed. `[shell] integration = false` turns
it off:

```toml
[shell]
program = "/bin/zsh"    # optional: the shell (also `shell = "/bin/zsh"`)
integration = false
```

With the marks, TermOS knows each command's **block**: its prompt, its
command line, its output and its exit status.

- **Failed commands** show `✗ 2` (the status) at the right end of their
  line, where the line is blank.
- **Jump between prompts:** in copy mode (prefix `[`), `[` goes to the
  previous prompt and `]` to the next. `pane.prompt-prev` and
  `pane.prompt-next` in the palette start copy mode there.
- **Copy one command's output:** in copy mode, `o` copies the output of
  the command the cursor is in. Prefix `y` (`pane.copy-output`) copies
  the last command's.
- **Agents read the last command:** `tos read --command` prints this
  pane's last command, its output and its exit status;
  `tos read 3 --command` does the same for pane 3. TermOS's MCP tool
  `read_pane` takes `last_command: true`. Keys and passwords in the
  output are masked, as for the rest of `read_pane`.

```
$ tos read --command
$ cargo test
   Compiling app v0.1.0
error[E0425]: cannot find value `x` in this scope
[exit status 101]
```

## Limits

- **bash before 4.4** (macOS's `/bin/bash` is 3.2) marks prompts but
  not where output starts, so its commands have no exit status. Use a
  newer bash (Homebrew's) or zsh.
- **cmd and `sh`** are not marked. A program can still print the marks
  itself (`printf '\e]133;A\a'`).
- **Prompts that redraw themselves** in unusual ways (some zsh plugins
  replace the line editor's start hook) may lose the mark between the
  prompt and the command line. The command line then comes from zsh
  itself, which reports it.
- **The block of a command still running** ends at the cursor. A
  full-screen program (an editor, `less`) is not part of its output.
- **Codex's hooks** run only after you trust them in Codex.
- **System notifications** need a terminal that shows them (above).
  TermOS cannot tell what the terminal around tmux is.
- **An agent restored in a shell** is typed into it with `tos`, which
  must be on the shell's `PATH`. TermOS puts it there for its panes, but
  a shell configuration that replaces `PATH` can drop it.
