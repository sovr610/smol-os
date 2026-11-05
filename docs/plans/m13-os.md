# Plan: M13, the OS beyond AI

The owner asked on 2026-11-05 for the improvements to TermOS that are not
about AI, then for a plan to build all of them (D-117). This plan splits
them into seven milestones, M13a to M13g, in the ROADMAP.

Each milestone works like every other one:
- Its "Ask first" questions go to the owner before it starts, and the
  answers become a `D-xxx` entry.
- It ends with green `cargo xtask ci`, a demo, a commit, a summary and a
  stop for review.

The design below is the starting point, not a decision: where the
answers change it, the milestone's decision entry says so.

## The milestones

| | What | Runs on | Size | Builds on |
| --- | --- | --- | --- | --- |
| **M13a** | Clickable links and paths, hint mode, search across panes, alerts when long commands finish | hosted and console | M | M12b's command blocks |
| **M13b** | Layout files, drop-down terminal, typing into several panes, saved commands, SSH hosts, pane recording | hosted and console | L | M12a's worktree workspaces |
| **M13c** | Process manager; file manager: bookmarks, two panes, search, archives, git status, bulk rename, disk usage | hosted and console | L | M4b's monitor and Files |
| **M13d** | On the image: lock screen, power menu, laptops (battery, brightness, lid), clock, USB drives, GPT disks, AMD graphics | bare metal | L | M8, M9 |
| **M13e** | On the image: several displays and hotplug, Bluetooth, sound | bare metal | L | M13d |
| **M13f** | Accessibility (screen readers, speech on the console) and translations | everywhere | L | everything with text |
| **M13g** | Packages (Homebrew, winget, Scoop, .deb, .rpm, AUR, Nix), a gallery for plugins, widgets and themes, settings export and import | everywhere | M | M9's release workflow |

Sizes: **M** is about one milestone of the usual length, **L** about one
and a half.

**The order** is this:
- **First (M13a to M13c):** they help everyone, in any terminal, and need
  no new hardware in CI.
- **Then (M13d, M13g):** M13d fixes the image's one real security gap (no
  password, no lock), and M13g makes TermOS easier to get.
- **Last (M13e, M13f):** M13e needs hardware that CI only emulates. M13f
  touches every string, so it goes after the UI has settled.

The owner can reorder them, and they can interleave with M12c to M12e.

## Shared ground

These rules hold in every M13 milestone.

- **Privileged actions on the image** go through `termos system`, which
  the user `termos` may run through sudo without a password
  (`/etc/sudoers.d/termos`). It already powers off, restarts and saves
  Wi-Fi networks.
  - New verbs: `suspend`, `set-time-zone`, `mount`, `unmount`,
    `brightness`, `check-password`, `set-password`.
  - Each verb takes fixed, checked arguments and never runs a shell.
  - Each verb has a unit test for refusing what it must refuse: paths
    outside `/media`, unknown zones, devices that are not removable.
- **No cost while idle** (D-028):
  - Devices, drives, lids and displays are events (udev netlink, libinput
    switch events, DRM hotplug), not polling.
  - Idle locking and suspending are deadlines in the desktop's timer
    list.
  - The battery is read on the status clock's minute tick, and only while
    something shows it.
- **Protocol changes** bump `PROTOCOL_VERSION` once in each milestone
  that has any:
  - M13a: links in frames, and "open this" for the client;
  - M13f: the client's locale.
- **Privacy:**
  - Recordings (M13b) and exports (M13g) are files only the user can
    read, and their contents are listed in the docs.
  - Exports never contain a secret, and a test checks it.
  - Links (M13a) open only `http`, `https`, `mailto` and `file` addresses.
- **Platforms:**
  - Hosted features work on Linux, macOS and Windows, or say plainly
    where they do not.
  - Image features are tested in QEMU in CI, as M8 does, with what QEMU
    can emulate: suspend, USB storage, two displays, an HDA sound card.
- **Docs:** each milestone updates the guide its features belong to, or
  adds one: `docs/links.md`, `docs/layouts.md`, the apps guide, the
  bare-metal guide, `docs/accessibility.md`, `docs/install.md`.

---

## M13a — Links, hints, search and finished commands

**Goal:** what is on the screen can be acted on. Links open, paths open
in the editor at their line, any pane's text can be found, and a long
command that finishes while you look elsewhere tells you.

### Links that programs print (OSC 8)
**Today:** `alacritty_terminal` keeps the links programs print
(`ls --hyperlink`, gcc, cargo, systemd), but TermOS drops them when it
copies cells into its grid. ARCHITECTURE §5.1 says cells carry a link id,
but `termos_render::cell::Cell` has none.

**In the grid and on the wire:**
- `Cell` gains `link: u32`, an index into a link table kept per grid (0
  means no link). The diff compares it like the style.
- `FrameUpdate` gains `links: Option<Vec<(u32, String)>>`, sent only when
  the table changes, as images are (a protocol bump).

**Where the links come from:**
- `AlacrittyVt::convert_cell` maps each cell's hyperlink to an id.
- TermOS's own anchors are left out: `termos-image:` and
  `termos-block:`.
- An address is kept only if its scheme is allowed, it has no control
  characters, and it is at most 2 KiB.

**Drawing them:**
- **A terminal around TermOS:** the encoder opens and closes `OSC 8`
  around linked runs, so the outer terminal shows and opens them
  natively.
- **The console:** it underlines them on hover.
- **`[ui] links = false`** turns this off.

### Links and paths TermOS finds itself
`termos_term::links` scans the visible rows, never the whole history, and
only when asked:
- **What it finds:**
  - web addresses;
  - email addresses;
  - git hashes (7 to 40 hex characters);
  - IPv4 and IPv6 addresses;
  - file paths with an optional `:line[:column]`. A path counts only if
    it exists, resolved against the pane's folder (OSC 7, or its
    foreground process's).
- **Its patterns:** `regex` (already a dependency), with unit tests per
  kind and a fuzz target (`link_detect`).

### Clicking
- **Ctrl+click** on a link or a found path opens it.
- **Hover** with Ctrl held underlines it, using the existing hover
  tracking.
- **Over a program that reads the mouse:** hold Shift as well. Shift
  already keeps mouse events for TermOS (D-085), and a plain Ctrl+click
  still goes to the program.

### Opening
- **Where:**
  - A web or mail address opens on the client's machine:
    `ServerMsg::Open { url }`, then `xdg-open`, `open` or `start`. This
    is right for a local client and for SSH.
  - On the console there is no browser, so the address is copied and a
    toast says so.
  - A path opens in the editor (`file.edit`).
- **Editors at a line:** `file.edit` learns `PATH:LINE[:COL]`:
  - Neovim: `+LINE`;
  - Helix: `PATH:LINE:COL`;
  - VS Code: `-g PATH:LINE:COL`;
  - others: the path alone.

### Hint mode
`pane.hints [url|path|hash|ip|any]` (prefix `u`) labels every match in
view with one or two letters, home row first:
- the label alone opens the match;
- Shift and the label copies it;
- Ctrl and the label pastes it into the pane (kitty's hints, WezTerm's
  quick select).

It is drawn as an overlay like copy mode, and Esc leaves it.

### Search across panes
`search.all` (prefix `/`) opens a box like the palette and searches
every terminal pane's text (history and screen) in the session. With
`--all-sessions` it searches every session.
- **Matching:** plain text, or a regex with a leading `/`, case-folded
  unless the query has capitals.
- **Results:** grouped by pane, each with its line. Enter goes there:
  the pane gets focus, enters copy mode at the line and highlights the
  match.
- **How it runs:** on a thread, over each pane's `logical_lines()`
  snapshot, stopping at 500 results.

### When a long command finishes
- **Timing:** M12b's blocks also record when output started (`C`) and
  when the command finished (`D`).
- **The alert:** when a block that ran longer than
  `[notifications] long-command = 15` seconds finishes in a pane that no
  client is looking at:
  - its tab gets a `✓`, or a `✗` in the error colour;
  - a toast and a center entry say what ran, where, its status and how
    long it took ("`make test` in 1:api failed (2) after 3m12s").
  - a failure also gets a system notification, if `[notifications]
    system` is on (the owner decides in "Ask first").
- **Asking for one:** `pane.notify-when-done` (pane menu, `tos notify
  --when-done`) arms a one-off alert for the pane's next finished
  command whatever its length. It also works without shell integration:
  it then fires when the pane goes quiet for 5 seconds after output.

### Settings
- `[ui] links`
- `[hints] alphabet = "asdfghjkl"`
- `[notifications] long-command = 15` (seconds; `0` turns it off)

### Tests
- **Encoder:** golden bytes for links. Two cells with the same link make
  one run; a link that changes mid-line closes and reopens it.
- **Wire:** a frame round trip with a link table, in `proto_decode`'s
  corpus.
- **Emulator:** a program's OSC 8 in a pane ends up in the grid with its
  address. Anchors and disallowed schemes are left out.
- **Detection:** a table of lines for each kind. Paths are resolved
  against a temporary folder.
- **Hint mode:** a snapshot of the labels; a label opens through a fake
  opener; Shift and a label copies.
- **Search:** on a desktop with three panes, the results come grouped
  by pane, and Enter lands on the right line in copy mode.
- **Long commands:** bash with the integration and a threshold of 1 s.
  `sleep 2` in a background workspace gives a tick; `false` after
  `sleep 2` gives a cross and a system notification.
- **Client:** end to end, `Open` reaches a stand-in opener on `PATH`.

### Risks
- **Terminals without OSC 8** might print the sequence. Almost all
  modern ones understand it; `[ui] links` and the capability probe's
  terminal name cover the rest.
- **Path detection** must not touch slow file systems on every frame: it
  runs only in hint mode and on Ctrl-hover, with a small cache per
  folder.

### Ask first
1. Where links open: on the client's machine (the plan), or on the
   server's?
2. A finished long command: loud for failures only (the plan), loud for
   both, or quiet for both?
3. The keys: `u` for hints and `/` for search, or others?

---

## M13b — Layouts and workspace tools

**Goal:** a project's workspaces in one file; a terminal one key away;
the same keys to several panes; saved commands; SSH hosts; recordings.

### Layout files
**Where they live:**
- `~/.config/termos/layouts/NAME.toml`, and `[layout.NAME]` in a
  project's `termos.toml`.

**The format:** TOML, like the rest of TermOS's settings:
```toml
[layout.dev]
workspace = "api"             # its tab's name
cwd = "."                     # relative to the project
[[layout.dev.pane]]           # the first pane, then each split from the one before
command = "nvim ."
[[layout.dev.pane]]
split = "right"               # right | down | float
size = "35%"
command = "cargo watch -x test"
[[layout.dev.pane]]
split = "down"
of = 2                        # split pane 2 (default: the one before)
command = "npm run dev"
focus = true
```

**Opening one:**
- `layout.open NAME` (and `termos layout open NAME [DIR]`) builds a new
  workspace with that tree, in `termos_shell` beside the worktree
  workspace code (D-114).
- **Commands are typed into shells**, so they can be run again after
  they stop. `exec = true` runs one as the pane's program instead.
- **Several workspaces:** `[[layout.NAME.workspace]]` arrays give a
  layout more than one.

**Saving one:**
- `layout.save NAME` writes the active workspace: its tree, folders, and
  the foreground programs on `restore-programs`. Other programs are
  named in a comment only, as snapshots do, so no secret on a command
  line is saved.
- `termos layout edit NAME` opens the file in the editor.

**The worktree workspace** (agent, shell, servers) becomes the built-in
layout `worktree`.

**Parsing** is fuzzed (`layout_parse`), with errors that name the line.

### Drop-down terminal
- **Opening it:** `quick.toggle` (prefix `` ` ``; `[quick] key` adds a
  key without the prefix) shows or hides one float per session that
  keeps running.
- **Its look:** full width, `[quick] height = "40%"`, from the top. It
  slides in where animations are on.
- **Hiding:** focus goes back where it was.
- **Restores:** it is saved with the session like other floats.

### Typing into several panes
- **Turning it on:**
  - `workspace.sync` (prefix `S`) sends keys and pastes for the focused
    terminal pane to every terminal pane in the workspace.
  - `pane.sync` adds or removes one pane from a smaller set.
- **What you see:** a `SYNC` badge on the status line, and the panes
  ringed in the accent colour.
- **What it covers:** mouse events and app windows never sync.
- **Turning it off:** the same key again.
- **The cost:** sending to several PTYs reuses `write_to_pane`, so a
  synced pane that waits for you is also answered.

### Saved commands
**Defined** in `[[snippets]]` in the config, and in a project's
`termos.toml`:
```toml
[[snippets]]
name = "Tail a service's log"
command = "journalctl -fu {service:termos}"
run = true                 # press Enter after typing it (default false)
where = "pane"             # pane (the focused one) | new-pane | float
```

**Running one:**
- The palette lists them as `snippet: NAME`.
- Each `{param}` or `{param:default}` is asked for in the status-line
  prompt, in order, then the line is pasted bracketed.
- `termos snippet list|run NAME [param=value…]` does it from a shell.

Parameters are plain text: the docs say not to put secrets in them.

### SSH hosts
- **The list:** `termos_core::ssh_config` reads `~/.ssh/config` and its
  `Include`s. It lives in termos-core because the shell's menus read it,
  and the shell may not depend on devkit. It keeps `Host` entries without wildcards, and skips
  `Match` blocks.
- **Where they show:** the ◆ Menu gets an **SSH** submenu, and the
  palette `ssh.open HOST`. Each opens a pane running the user's own
  `ssh HOST`.
- **Recent hosts** come first.
- **Parsing** is tested on fixtures and fuzzed (`ssh_config`).
- **Remote attach** (M12c) builds on this list later.

### Recording a pane
- **Starting:** `pane.record` (prefix `R`) starts or stops recording the
  pane's output.
- **The file:** asciinema v2 (`.cast`, JSON lines of time and bytes, with
  resizes), in `~/.local/state/termos/recordings/`, mode 0600.
- **While it records:** the pane's border shows `● REC`, and the toast
  when it stops names the file.
- **Input** is not recorded. What a shell echoes is output and is in the
  file, which the docs say plainly.
- **Playing:** `termos play FILE [--speed 2] [--idle-limit 2]` plays one
  back in a pane, through the same emulator, at the recorded times.

### Settings
- `[quick] height`, `key`
- `[[snippets]]`
- `[record] dir`

### Tests
- **Layouts:** parse errors with line numbers. A three-pane layout opens
  with the right tree, folders and typed commands (desktop test,
  snapshot). Save then open gives the same tree.
- **Drop-down:** toggles, keeps its program running, and survives a
  restore.
- **Sync:** typing reaches three `sh` panes; app windows are skipped;
  turning it off stops it.
- **Snippets:** prompts for two parameters with a default, then pastes
  the line; `run = true` presses Enter.
- **SSH config:** fixtures with `Include`, wildcards and `Match`.
- **Recording:** the cast file is valid asciinema v2; playback
  reproduces the screen (`vt100` compare).

### Risks
- **Layout files and snapshots:** both describe workspaces, and must not
  drift apart. Both build the tree through one function.

### Ask first
1. The layout format: TermOS's own TOML (the plan), or Zellij's KDL for
   compatibility?
2. The drop-down's key: prefix `` ` ``, or a key without the prefix by
   default?
3. Recording: output only (the plan), or input too, behind a setting?

---

## M13c — Processes and files

**Goal:** see and stop what is running, and manage files without
leaving TermOS.

### Process manager
- **Where:** a `Processes` tab in the system monitor (`processes.open`
  opens it directly).
- **The data:** `termos_hw::Sampler`'s process list grows the parent
  pid, user, start time, state, threads and full command line.
- **The view:**
  - **Columns:** pid, user, CPU, memory, time and command.
  - **Keys:** `/` filters by name or command; `s` sorts; `t` switches
    to a tree by parent.
  - **This pane's processes:** the pane's menu opens the view filtered
    to its process tree (the PTY child and its descendants; on Windows,
    the job object's processes).
- **Actions:** each asks first.
  - End (SIGTERM); kill (SIGKILL); pause and resume (SIGSTOP,
    SIGCONT).
  - Priority up and down (renice).
  - On Windows, End is TerminateProcess and pause and priority are
    left out.
  - Signals go through `sysinfo`'s `kill_with`, or `rustix` on Unix.
  - Another user's process gives the system's error. There is no sudo
    from here.
- **Ports:** the TCP ports each process listens on, on Linux
  (`/proc/net/tcp*` and the process's sockets). Elsewhere a note says
  they're not available.
- **Cost:** it samples only while visible, as the monitor does.

### Files
- **Selecting several:** Space marks, `*` inverts; copy, move, trash and
  rename work on the marks.
- **Bookmarks:**
  - A sidebar: Home, Bookmarks, Drives (M13d), Trash.
  - `b` bookmarks the folder, and `[files] bookmarks` keeps them.
  - `'` opens the bookmark list.
- **Two panes:** `Tab` switches. `F5` copies and `F6` moves to the other
  pane, as Midnight Commander does. `[files] two-pane` sets the default.
- **Search:** both kinds run on a thread and list results you can open.
  - `?` searches names under the folder (the `ignore` walker, honouring
    `.gitignore` unless `.` shows hidden files).
  - `Ctrl+F` searches text inside files (the `grep` crates, already
    used by TermOS's agent tools).
- **Archives:**
  - **Browsing:** zip, tar, tar.gz and tar.zst open like folders,
    read-only (`zip`, `tar` and `flate2`, already dependencies, and
    `ruzstd`, a zstd decoder in Rust). Opening a file
    inside one extracts it to a temporary folder.
  - **Extracting and making:** `x` extracts here or to the other pane;
    `z` makes a zip of the marks.
  - **Safety:** names that would leave the target are refused, as in
    D-112.
- **Git status:** a column (`M`, `A`, `?`, `!`) from `git status
  --porcelain=v2 -z`, run on a thread when a folder in a repository is
  shown and cached until the folder changes.
- **Bulk rename:** `F2` on several marks writes their names to a
  temporary file and opens the editor (as `vidir` does). TermOS checks
  the edited names for clashes and missing lines, shows the changes,
  then renames, two-phase for swaps. `u` undoes the last rename.
- **Disk usage:** `D` sizes each entry of the folder (all its contents),
  biggest first, with bars, as ncdu does. Enter drills down; `d`
  trashes. It is computed on a thread, with a count shown while it runs.
- **Open with:** `O` lists the apps that can open the file (TermOS's
  viewers, the editor, the system's opener) and remembers the choice per
  extension.

### Settings
- `[files] bookmarks`, `two-pane`
- `[processes] confirm = true`

### Tests
- **Processes:**
  - the tree and filters from fixtures (the `HwSource` trait);
  - ending and killing a real `sleep` child;
  - a pane's processes for a shell that started `sleep`;
  - ports on Linux against a listener the test opens.
- **Files:**
  - snapshots of the two-pane and sidebar views;
  - archive listing and extraction, with an entry that tries to leave
    the folder;
  - git status with real git;
  - bulk rename on a plan with a swap and a clash (proptest: no lost
    files);
  - disk usage on a temporary tree;
  - content search finds a line and opens it at that line (M13a's
    `file.edit PATH:LINE`).

### Risks
- **Bulk rename and moves** across file systems must never lose a file.
  Copy then delete, and keep the undo list.
- **Huge folders:** listing and sizing run off the UI, and results come
  in as they are found.

### Ask first
1. The process manager: a tab in the monitor (the plan), or its own
   window?
2. Bulk rename: in the editor (the plan), or in a window of its own?
3. Archive formats: zip, tar, tar.gz and tar.zst (the plan), or also 7z
   and rar (more dependencies, rar's licence)?

---

## M13d — On the image: lock, power, laptops, clock, drives, disks

**Goal:** the image behaves like a computer's OS:
- it locks;
- it can be shut down from the menu;
- it knows its battery, brightness, lid, time and USB drives;
- its disks are GPT.

### Lock screen
- **Turning it on:**
  - First boot (and the installer) asks for a password for the user
    `termos`, saved with `termos system set-password`, which runs
    `chpasswd`. A password can be skipped; the lock then warns that
    anyone can unlock it.
- **Locking:**
  - `session.lock` (prefix `L`, ◆ Menu → Power → Lock);
  - after `[lock] after-minutes = 10` idle;
  - on suspend and when the lid closes;
  - at start, with `[lock] at-start = true`, which makes the console
    act as a login.
- **The lock screen:** the console (`termos --kms`) draws it full
  screen, with the clock and a password field, over nothing of the
  desktop. While locked, the console sends no input to the server.
- **Checking the password:** `sudo termos system check-password` reads
  it on standard input and compares it with `/etc/shadow`.
  - The image sets shadow's `ENCRYPT_METHOD SHA512`, so one method is
    needed: RustCrypto's `sha-crypt`.
  - After three failures it waits, longer each time.
- **Other virtual terminals:**
  - Their gettys ask for a login (they must not be open shells).
  - While locked, Ctrl+Alt+Fn must not leave the lock screen. The
    ioctl that stops VT switching (`VT_LOCKSWITCH`) needs privileges, so
    it is a `termos system vt-lock on|off` verb.
- **What the lock does not do:** agents, servers and SSH sessions keep
  running. The docs say so.

### Power menu
◆ Menu → **Power**:
- **Lock;**
- **Suspend:** `termos system suspend`, which writes `mem` (or `freeze`
  where `mem` is missing) to `/sys/power/state`;
- **Restart;**
- **Shut down.**

Restart and Shut down already exist as `termos system restart` and
`power-off`. Each saves the sessions first and asks before going ahead.

In hosted mode the submenu offers only **Quit TermOS**.

### Laptops
- **Battery:**
  - `termos_hw::power` reads `/sys/class/power_supply` (capacity,
    status, energy and power for time left; AC online). It takes a root
    path for tests, as the GPU reader does.
  - On macOS and Windows the `starship-battery` crate supplies the same
    figures (hosted mode too).
  - A status-line item shows the charge and whether it is charging.
- **Low battery:** a toast at 20%, a system notification at 10%, and
  suspend at 5% with a 30-second countdown (`[power] critical`).
- **Brightness:**
  - **Keys:** XF86MonBrightnessUp and Down in the console.
  - **Commands:** `display.brightness ±10`.
  - **The setting:** `[power] brightness` at start.
  - **Permission:** written to `/sys/class/backlight/*/brightness`. A
    udev rule gives group `video` write access, and `termos` is in it,
    so no sudo is needed.
- **Lid and idle:**
  - libinput's switch events give the lid: `[power] lid = "suspend" |
    "lock" | "nothing"`.
  - Idle suspends after `[power] suspend-after-minutes` (on battery;
    `0` never).

### Clock
- **Time sync:** BusyBox `ntpd` starts from init with `[clock]
  ntp-servers` (default `pool.ntp.org`) and steps the clock at boot.
- **Time zones:**
  - The image carries the zoneinfo database (`tzdata`).
  - `/etc/localtime` is a link to `/data/etc/localtime`, which `termos
    system set-time-zone ZONE` writes.
  - A **Clock** page in Settings searches zones and sets 24- or 12-hour
    time for the status clock.

### USB drives
- **Seeing them:** the server listens to udev (netlink) for removable
  block devices on the image, and on hosted Linux through udisks2's
  D-Bus API where it runs (`zbus`).
- **Mounting:**
  - Plugging one in gives a toast and shows it under **Drives** in
    Files; clicking mounts it.
  - On the image this is `termos system mount DEV`, mounted at
    `/media/LABEL` with the user's ids, for vfat, exfat, ext4 and ntfs3.
    The kernel configuration adds exFAT and NTFS3.
- **Ejecting:** `e` syncs and unmounts.
- **Elsewhere:** macOS and Windows show their own volumes as bookmarks
  in Files, and the system mounts them.

### Disks
- **GPT:** the x86_64 and aarch64-virt images move to GPT (genimage
  `partition-table-type = "gpt"`). A BIOS boot partition keeps legacy
  BIOS boots working with GRUB.
- **Finding partitions:** by `PARTLABEL` (`boot`, `rootfs-a`,
  `rootfs-b`, `data`) instead of fixed ids.
- **Unique ids:** the installer gives each disk and partition new GUIDs
  (`sfdisk --disk-id`, `--part-uuid`), so two TermOS disks can share a
  machine.
- **Raspberry Pis** stay MBR, which their firmware needs.
- **Existing installs** keep their MBR disks: updates do not change
  partition tables, and RAUC finds slots by label on both.

### AMD graphics
The x86_64 image adds the amdgpu firmware files (linux-firmware's
`amdgpu` subset), listed in the image's licences. AMD cards then get
their own driver instead of the UEFI framebuffer.

### Settings
- `[lock] after-minutes`, `at-start`
- `[power] lid`, `suspend-after-minutes`, `critical`, `brightness`
- `[clock] ntp-servers`, `time-format`

### Tests
- **Lock (QEMU):** boot, lock with the key, keys sent through QEMU's
  monitor (`sendkey`) do not reach the desktop. A wrong password waits;
  the right one unlocks.
- **Suspend (QEMU):** suspend and resume with S3 enabled, and the
  desktop is the same after.
- **Battery:** sysfs fixture trees (charging, discharging, no battery,
  two batteries); low-battery steps on a fake clock.
- **Clock:** `set-time-zone` changes the status clock's zone; bad zone
  names are refused.
- **Drives (QEMU):** `device_add usb-storage` with a vfat image shows up
  in Files, mounts, lists a file, and unmounts.
- **Disks:** the GPT image boots in QEMU with UEFI and with BIOS, and
  updates; an installed disk gets new GUIDs.
- **Password check:** fixtures of sha512crypt hashes written by
  `chpasswd`; wrong passwords; the waits between tries.

### Risks
- **A broken password check is a lockout.** Recovery stays possible: a
  key combination at boot (GRUB menu) starts without the lock,
  documented, and the check is tested against hashes `chpasswd` wrote.
- **Suspend** varies by hardware: QEMU tests the path, and the docs list
  what was tried on real machines.
- **Image size:** amdgpu firmware adds tens of MB.

### Ask first
1. The lock's password: the system user's (`/etc/shadow`, the plan), or
   a TermOS passphrase of its own?
2. Hosted mode: a lock too, or the host OS's only (the plan)?
3. AMD firmware in every x86_64 image, or in a separate image?
4. Defaults: lock after 10 minutes idle, suspend after 20 on battery?

---

## M13e — On the image: displays, Bluetooth, sound

**Goal:** the image uses the hardware a desktop has: more than one
screen, Bluetooth keyboards and headsets, sound.

### Displays
- **More than one:** the console (`kms/display.rs`) drives every
  connected connector instead of the first.
- **Plugging in:** DRM hotplug (udev `change` events on the card) adds
  and removes outputs while running.
- **Per display:**
  - `[[kms.display]] name = "HDMI-A-1" mode = "1920x1080@60" scale = 1.5
    enabled = true`, with the preferred mode by default;
  - font size scaled to each display's DPI;
  - a **Displays** page in Settings for arrangement, which is primary,
    mode and on or off.
- **Mirror or extend:**
  - **Mirror:** the same picture on every display.
  - **Extend:** each display is its own client of the session, showing
    its own workspace.
  - **What extend changes:** today a session's clients share its active
    workspace, as tmux's do. Extend adds an active workspace per client,
    and a workspace shown on one display is not shown on another.
- **Cost:** two frames in software cost twice the CPU, so the Pi 4's
  latency budget (33 ms at p99) is checked with two outputs.

### Bluetooth
- **On the image:** BlueZ (`bluetoothd`, D-Bus), its kernel modules and
  the common firmware (Intel, Realtek, Broadcom on Pis).
- **The Bluetooth page** (in the Network app): power, scan, pair, trust,
  connect, forget. Pairing codes are shown or asked for through an agent
  TermOS registers on D-Bus (`zbus`).
- **Keyboards and mice** need nothing more: libinput picks them up.
- **Headsets** need the sound stack below.

### Sound
- **The stack:** ALSA (`alsa-lib`, with the `alsa` crate for the
  mixer), or PipeWire with WirePlumber, which Bluetooth audio needs.
  The owner chooses (Ask first).
- **Controls:**
  - a status-line volume item;
  - the media keys (volume up and down, mute) in the console;
  - a **Sound** page in Settings for the output device and volume;
  - `[kms] audible-bell` beeps for the bell.

### Settings
- `[[kms.display]]`, `[kms] displays = "extend" | "mirror"`
- `[bluetooth] enabled`
- `[sound] device`, `[kms] audible-bell`

### Tests
- **Displays (QEMU):** two outputs (`virtio-gpu-pci,max_outputs=2`);
  extend gives two clients with different workspaces; unplugging one
  (QEMU monitor) leaves one.
- **Bluetooth:** the pairing agent against a stand-in D-Bus service in
  tests (no Bluetooth in CI). The page's states are snapshots.
- **Sound (QEMU):** `intel-hda` and `hda-duplex`; volume set and read
  back; mute.

### Risks
- **Real hardware:** displays and Bluetooth vary most of all.
  - Pis are tested by hand and listed.
  - The docs say which chips the image has firmware for.
- **Image size:** PipeWire adds several MB and services.

### Ask first
1. Several displays: extend (the plan) or mirror by default?
2. Sound: ALSA only (small; no Bluetooth audio), or PipeWire (larger;
   Bluetooth headsets work)?
3. Bluetooth audio in this milestone, or keyboards and mice only?

---

## M13f — Accessibility and translations

**Goal:** TermOS works with screen readers and speaks on the console,
and its interface can be in other languages.

### Screen readers in a terminal
**The problem:** screen readers (NVDA and Narrator on Windows,
VoiceOver on macOS, Orca on Linux) read what the outer terminal shows.
TermOS's chrome (wallpaper, icons, box drawing, a status line rewritten
in place) is noise to them. Its small redraws also make them read
fragments.

**`[ui] screen-reader = true`**, also offered on first run:
- **Plain looks:** no wallpaper, icons, shadows, gradients or
  animations; borders as plain lines.
- **The status line in words:** "workspace 2 of 3, api; 1 agent needs
  you".
- **Focus changes, toasts and menus** are announced by moving the
  terminal cursor to a dedicated last line and writing the text there,
  which readers announce.
- **The focused pane** is drawn with its own cursor, so the reader
  follows the program as in a plain terminal.

**A research spike starts the milestone:** a week with NVDA, VoiceOver
and Orca on the three platforms. Its findings go in the decision entry;
the mode's details follow what they show.

### Speech and braille on the console
- **Speech** is spoken by the `espeak-ng` program (GPL-3.0-or-later),
  started once and fed lines on its standard input, so there is no FFI.
  It covers:
  - the focused pane's new output, throttled and interruptible;
  - menu and palette selections, focus changes and notifications;
  - review keys for the current line, word and character, and the
    whole screen, as Fenrir does.
- **Settings:** `[kms] speech = true`, with `rate` and `voice`.
- **Braille** later, through BrlAPI (brltty).

### Translations
- **The library:** `termos_core::i18n` on Fluent (`fluent-bundle`,
  `fluent-langneg`, `unic-langid`; Apache-2.0 and MIT), with `.ftl` files
  built into the binary from `crates/termos-core/i18n/LANG/`.
- **In code:** a `tr!("menu-ai-agents")` macro, and `tr!(id, n = 3)`
  for plurals. A missing translation falls back to English and is
  counted in the log.
- **Which language:**
  - `[ui] language = "auto"` follows `LC_ALL`, `LC_MESSAGES` and `LANG`.
  - Each client sends its locale in `Hello` (a protocol bump), and the
    server renders each client in its own language.
- **What is translated:** the desktop's chrome, menus, palette and
  which-key labels, prompts and toasts, every app and window, and
  `termos` messages. Command names, config keys, CLI flags and the docs
  stay English.
- **Layout:** text widths come from Unicode, as they already do, so
  longer and double-width text lays out. A pseudo-locale (`en-XA`:
  accented and 40% longer) catches clipping in snapshot tests.
- **First languages:** to be chosen with the owner (Ask first), with a
  native-speaker review for each before it ships.
- **Checks:**
  - `cargo xtask i18n` compares each language's keys with English's.
    It flags keys missing elsewhere, and fails on keys that exist only
    outside English. It is std-only, like xtask's other checks.
  - A termos-core test parses every `.ftl` file with Fluent.
  - CI runs both.

### Tests
- **Screen-reader mode:** snapshots of the plain mode, and the
  announcement line's text for focus, toasts and menus.
- **Speech:** through a stand-in speech backend that records what it
  would say (no audio in CI): a toast and a menu move are spoken; new
  output is throttled.
- **Translations:** every app in `en-XA` (snapshots: nothing clipped
  that fits in English); a German client and an English client of one
  session each see their own language; the xtask check fails on a key
  missing from English.

### Risks
- **What users need:** screen-reader users' needs are what matter, and
  the spike and a tester's review are part of the milestone, not
  extras.
- **Extracting strings** touches every crate with text. It goes crate by
  crate, in separate commits, with snapshots unchanged in English.

### Ask first
1. Who can test with a screen reader, on which platforms?
2. Which languages first, and who reviews each?
3. Speech on the console in this milestone, or the screen-reader mode
   first?

---

## M13g — Packages, a gallery, settings that move

**Goal:** TermOS installs the way people install software. Plugins,
widgets, themes and layouts can be found and installed. Settings move
to a new machine.

### Packages
The release workflow (M9) gets a job per channel. Each job builds from
the release's signed binaries and is tested by installing on a clean
runner.
- **Homebrew:** a tap (`sovr610/homebrew-termos`), with the formula
  bumped by the release job.
- **winget:** a manifest pull request opened by `wingetcreate`.
- **Scoop:** a bucket.
- **Debian and Ubuntu:** `.deb` (`cargo-deb`), attached to the release
  first; an apt repository later.
- **Fedora:** `.rpm` (`cargo-generate-rpm`).
- **Arch:** an AUR package `termos-bin`.
- **Nix:** a flake in this repository (`flake.nix`, building from
  source), later nixpkgs.

Each package puts the shell completions and the man page in place.
Generating them (clap) is part of the milestone.

### Gallery
- **The index:** a repository `termos-gallery` holding one JSON file.
  Each entry has its name, kind (`plugin`, `widget`, `theme`, `layout`,
  `snippets`), version, address, SHA-256, author, licence and an Ed25519
  signature (`ed25519-dalek`) by the gallery's key. Entries are added by
  pull requests that maintainers review.
- **From a shell:** `termos gallery list|search|show|install|update|remove`.
  - It fetches the index with the system's `curl`, as TermOS's other
    downloads do.
  - It checks the signature and the hash before installing anything.
  - Each kind goes where it belongs: plugins, widgets, themes and
    layouts in their folders; snippets merged into the config.
- **In the desktop:** a **Gallery** window (◆ Menu → Settings →
  Gallery) to browse and install.
- **Trust:**
  - Themes, layouts and snippets are data.
  - Widgets run in the WASM sandbox (D-051).
  - Native plugins are programs: installing one shows what it asks for
    and needs a yes.
- **Parsing:** the index is fuzzed (`gallery_index`).

### Settings export and import
- **Export:** `termos settings export FILE` writes a `.tar.gz` of the
  config, layouts, snippets, themes, bookmarks, the list of installed
  plugins and widgets, and the names of AI accounts.
  - Never included: credentials, keys, the secrets file, histories or
    snapshots. A test scans the archive for them.
- **Import:** `termos settings import FILE` shows what each file would
  change, merges the config with `toml_edit` (the user's comments kept),
  and asks before overwriting.
- **In the desktop:** the Settings window gets **Export** and
  **Import**.
- **Keeping settings in a dotfiles repository** already works (the
  config folder can be a link); the docs show how.

### Tests
- **Packages:**
  - the `.deb` installs on Ubuntu and the `.rpm` on Fedora (containers),
    and `termos --version` runs;
  - the formula installs from a local tap on macOS;
  - the Scoop manifest installs on Windows;
  - the flake builds.
- **Gallery:**
  - an index signed with a fixture key verifies;
  - a tampered entry or a wrong hash is refused;
  - installing a theme and a widget lands them where TermOS reads them.
- **Export and import:** a round trip on a temporary home. The archive
  has no secrets, using the patterns of `termos_ai::redact`.

### Risks
- **Accounts and secrets:** publishing needs the owner's accounts on
  each channel (an AUR SSH key, a tap repository, winget's pull
  requests). The jobs stay off until those secrets exist.
- **The gallery's signing key** must be kept offline. A compromised key
  means rotating it, and the index carries the key's id.

### Ask first
1. Which channels, and who owns the accounts?
2. The gallery: hosted on GitHub (the plan), and who reviews entries?
3. Which kinds at first: all five, or themes, layouts and widgets only?

---

## Not in this plan

These stay unscheduled:
- **More than one user** at the console (seats).
- **Typing Chinese, Japanese or Korean** on the console: it would need an
  input method.
- **Printing.**
- **A text editor of TermOS's own:** Neovim and Helix are managed
  (M5a).
- **Drawing with the GPU:** the console draws on the CPU by design
  (D-073).

Some of the earlier "After 1.0" list is now planned here: the
screen-reader mode is in M13f, and the lock screen in M13d.
