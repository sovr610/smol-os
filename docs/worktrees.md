# Worktrees: a branch and a workspace per task

A worktree is a second checkout of your repository, on a branch of its own,
in a folder of its own. TermOS makes one per task. An agent can work there
while you, or other agents, work in the others. Nothing touches your main
checkout until you merge.

Each worktree TermOS makes comes with:

- **a branch**: `termos/<name>`, from the branch you are on;
- **a folder**: in TermOS's data folder, not inside your repository;
- **its setup**: ignored files such as `.env` copied in, then your
  project's setup commands (`npm install`);
- **a block of ports**: `PORT` and `TERMOS_PORT_0` … `TERMOS_PORT_9`, so
  two worktrees' dev servers never fight over a port;
- **a workspace**: a TermOS tab with the agent, a shell and your servers.

When the work is done you review it, answer the agent if it needs another
pass, then merge it, open a pull request, or discard it.

## Making one

- ◆ Menu → AI agents → **New worktree for a task**, or `worktree.new fix the
  login form` in the palette. A window shows the setup; when it is done the
  worktree opens as a workspace.
- From any terminal, in your repository:
  `termos worktree new --title "Fix the login form"`. Add `--open` in a
  TermOS pane to open its workspace too.

The name comes from the title (`fix-the-login-form`), else `task-1`,
`task-2`…. `--from BRANCH` starts it from another branch; `--branch NAME`
checks out a branch that already exists.

## The workspace

`worktree.open NAME` (or `termos worktree open NAME`) opens it as a tab
named after it:

```
 ◆ Menu  [main]  0:zsh  1:fix-the-login-form*  +
┌ agent: claude ───────────────┐┌ zsh ───────────────────────────┐
│ > Fix the login form …       ││ $ git status                   │
│                              │├ npm run dev ───────────────────┤
│                              ││ Listening on :20010            │
└──────────────────────────────┘└────────────────────────────────┘
```

- **Left:** the agent, in the worktree, as the account in use
  ([accounts.md](accounts.md)). `[worktrees] agent` picks it: `auto` (the
  first of Claude Code, Codex and Gemini CLI that is installed), one of
  them, `termos` (TermOS's own agent) or `none`.
- **Right:** a shell, and below it each of your project's servers, typed
  into a shell so you can run it again after it stops.
- **Every pane you open in the worktree's folder** gets its environment:
  `TERMOS_WORKTREE`, `TERMOS_WORKTREE_BRANCH`, `TERMOS_WORKTREE_BASE`,
  `TERMOS_REPO`, `PORT`, `TERMOS_PORT_0` … and `TERMOS_PORTS`
  (`20010-20019`). Point your dev server at `$PORT`.

Opening a worktree that is already open goes to its tab.

## What a project sets up

Put a `[worktree]` table in the `termos.toml` at the top of your repository
(the same file as `[tasks]`, [languages.md](languages.md#build-run-and-test)):

```toml
[worktree]
setup = ["npm ci", "npm run db:migrate"]   # run in each new worktree, in order
copy = ["**/.env", "**/.env.*", "certs"]   # ignored files copied from your checkout
servers = ["npm run dev", "npm run worker"] # each in a pane of its own
```

- `setup` runs with the worktree's ports, before it opens. It stops at the
  first command that fails. `termos worktree setup` runs it again.
- `copy` matches files (and whole folders) that git ignores, as `git status
  --ignored` lists them. The default copies `.env` files at any depth.
  Tracked files are already there.
- `servers` start when the worktree opens as a workspace.

## Reviewing

`worktree.review` (◆ Menu → AI agents → **Review this worktree**) opens the
review window for the worktree of the pane you are in; `worktree.review NAME`
for another.

```
 fix-login  termos/fix-login → main  ports 20010-20019   1 commit, changes not committed, +3 -1
 [Send to agent (2)] [Commit] [Merge] [Pull request] [Archive] [Discard] [Open]
M src/login.rs     +1 -1 │ @@ -1,3 +1,3 @@
? notes.md         +2 -0 │    1    1   fn login() {
                         │    2      -     check(user)
                         │         2 +     check(user, password)
 2 comments for the agent (last: also check the password); s sends them
 ↑↓ move · Tab files/diff · c comment · s send to agent · C commit · m merge · p pull request …
```

- **What it shows:** every file changed against the branch the worktree
  started from: its commits, edits not committed yet, and new files. It
  looks again every few seconds while it is on screen.
- **Comments:** `Tab` to the diff, move to a line, `c`, type, Enter. On the
  file list, `c` comments on the whole file. `u` removes the last one.
- **Send to agent (`s`):** the comments go to the worktree's agent as its
  next message, with the file, line and the line's text, and the review
  window closes so you can watch it work. When no agent is running there, a
  new one starts in the worktree with them as its first message.
- **Commit (`C`), Merge (`m`), Pull request (`p`), Archive (`a`), Discard
  (`d`):** each runs `termos worktree …` in a window, where git's editor,
  hooks and questions work as usual. **Open (`o`)** goes to its workspace.

## Finishing

| | What happens |
| --- | --- |
| **Commit** | `git add -A`, then `git commit` with your editor (or `-m`); your hooks run. |
| **Merge** | Its branch into the branch it started from. Where that branch is checked out (your main checkout, usually) it must have no changes not committed; elsewhere TermOS merges without a checkout. A conflict changes nothing and lists the files. Then the worktree is archived and its branch deleted (`[worktrees] archive-merged`; `--keep` keeps it). |
| **Pull request** | Pushes its branch, then `gh pr create` (GitHub) or `glab mr create` (GitLab) with its title and commits. Without them, it shows the page that opens one. `--draft` too. TermOS keeps no forge token: `gh` and `glab` have their own sign-in. |
| **Archive** | Removes its folder and keeps its branch. Its ports go back. |
| **Discard** | Removes its folder and its branch, changes included (it asks first). |
| **Restore** | Checks an archived or discarded worktree out again, with new ports. TermOS keeps each one's last commit as `refs/termos/archive/<name>`, so a discarded branch comes back too. |
| **Forget** | Drops an archived or discarded worktree for good. |

## Commands

| Command | |
| --- | --- |
| `termos worktree new [NAME] [--title T] [--from REF] [--branch B] [--no-setup] [--no-ports] [--open]` | Make one |
| `termos worktree list [--all] [--everywhere]` | This repository's (all of them with `--everywhere`; archived too with `--all`) |
| `termos worktree status [NAME]` | Branch, ports, commits, and each changed file |
| `termos worktree diff [NAME]` | The whole diff against its base, new files included |
| `termos worktree env [NAME]` | Its environment, as `NAME=value` lines |
| `termos worktree setup [NAME]` | Copy the ignored files and run the setup again |
| `termos worktree commit [NAME] [-m MSG]` | Commit everything |
| `termos worktree merge [NAME] [--keep]` | Merge into its base, then archive it |
| `termos worktree pr [NAME] [--title T] [--draft]` | Push and open a pull request |
| `termos worktree archive [NAME] [--force]` / `discard [NAME] [--yes]` | Put it away |
| `termos worktree restore NAME` / `forget NAME` | Bring it back / drop it |
| `termos worktree open [NAME]` | Open it as a workspace (in a TermOS pane) |

Without a name, a command works on the worktree you are in.

In the palette: `worktree.new [title]`, `worktree.open [name]`,
`worktree.review [name]`, and `worktree.run ARGS` (`termos worktree ARGS`
in a window).

## Settings

```toml
[worktrees]
root = "~/worktrees"          # where they go (default: TermOS's data folder)
branch-prefix = "termos/"
port-range = [20000, 29999]   # handed out in blocks
ports = 10                    # per worktree
agent = "auto"                # auto | claude | codex | gemini | termos | none
archive-merged = true         # archive (and delete the branch) after a merge
```

Worktrees live in `<root>/<repository>-<id>/<name>`. TermOS records them in
`worktrees/registry.toml` in its data folder.

## The harness and best-of-N

The harness uses worktrees too ([harness.md](harness.md)):

- **A plan** gets a worktree and branch of its own (`plan-<id>`). Each task
  that changes files works in a worktree of its own, at the same time as
  the others, and is merged into the plan's branch when it finishes. The
  result is reviewed in the plan's worktree like any other.
- **Best-of-N** runs one prompt on several backends at once
  (`claude:2,codex:1`), each attempt in a worktree. You keep one; the others
  go.

## Limits

- **Uncommitted changes stay behind:** a worktree starts from your
  branch's last commit, not from what you have not committed.
- **Merging without a checkout** (into a branch not checked out anywhere)
  needs git 2.38 or newer. Older git says so; check the branch out and merge
  there.
- **Sending to an agent that is not running** starts a new conversation in
  the worktree with your comments; the earlier one is not continued.
- **Agents with a pane** are found by the worktree's workspace, or by their
  folder (which macOS and Windows report only for some programs). Keep the
  agent in its workspace.
- **Windows paths:** deep folders inside a worktree (such as
  `node_modules`) can pass Windows' 260-character limit; set `[worktrees]
  root` to a short folder, or turn on git's `core.longpaths`.
- **Ports are checked when a worktree is made.** A program that takes one
  of them later is not noticed.
