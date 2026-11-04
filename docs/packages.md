# Packages and the dev rootfs

TermOS installs software from four places, and one window shows them all:

- **mise**: languages, their tools and command-line programs, in your home
  folder ([languages.md](languages.md));
- **the dev rootfs**: Debian 13, where apt installs compilers, libraries and
  system packages without touching the system you run on, and without root;
- **the host**: your system's own package manager, where it has one (apt,
  dnf, pacman, zypper, apk, Homebrew, winget or Scoop);
- **Podman**: its images and containers, where Podman is installed.

On a TermOS image the system itself is read-only, so the dev rootfs is where
packages go. On Linux, macOS and Windows it is there when you want a Debian
next to your own system.

## The dev rootfs

```bash
termos devroot create        # download Debian 13 and set it up (about 130 MB on disk)
termos devroot enter         # a shell inside: you are root there
apt install build-essential  # inside: apt works as on Debian
termos devroot update        # upgrade everything inside
termos devroot status        # what there is, or why it cannot run here
termos devroot remove        # delete it and everything installed in it
```

`termos devroot enter -- make` runs one command instead of a shell; its exit
code is the command's. From the desktop: the **Project → Dev rootfs shell**
menu item, or `devroot.enter` in the palette.

**What it is.** The official `debian:trixie` image, the exact build TermOS
was released with: TermOS downloads it with `curl`, checks its size and
SHA-256 against the values it was built with, and unpacks it into
`~/.local/share/termos/devroot/rootfs`. Nothing is unpacked if the check
fails. `termos devroot create --from FILE` uses a copy you already have
(the same file, or another rootfs `.tar.gz` with `--sha256 HEX`).

**Inside**, you are root of your own user namespace: files you make in your
home folder belong to you outside, programs inside cannot touch the rest of
your system, and nothing runs as your system's root. Your home folder is
there at its own path, and so is the current folder when it is outside your
home (a project in `/data` or `/tmp`). The network is your system's. `/tmp`
is new for each shell. The prompt shows `(devroot)`.

**What it needs (Linux):**

- `bwrap` (the `bubblewrap` package);
- `unshare` from util-linux 2.38 or newer (Debian 12, Ubuntu 23.04, Fedora
  36 and later);
- `newuidmap` and `newgidmap` (the `uidmap` package on Debian and Ubuntu,
  `shadow-utils` elsewhere);
- a range of 65 536 subordinate IDs for you in `/etc/subuid` and
  `/etc/subgid`. Most systems give every user one; otherwise:

  ```bash
  sudo usermod --add-subuids 100000-165535 --add-subgids 100000-165535 "$USER"
  ```

- user namespaces. Ubuntu 23.10 and later restrict them through AppArmor;
  `termos devroot status` says so, and
  `sudo sysctl kernel.apparmor_restrict_unprivileged_userns=0` lifts it.

TermOS images have all of these.

**With Podman.** `termos devroot create --podman` runs the same image (by its
digest) in a Podman container called `termos-devroot` instead, with your home
folder in it at the same path. This is the way on macOS and Windows, and on
Linux systems without the programs above. `termos devroot enter` starts the
container when it is stopped; `termos devroot stop` stops it; `remove`
deletes the container and leaves the image to Podman (`podman rmi`).

## The package manager

Open **Apps → Packages** from the menu, `packages.open` in the palette, or
`tos packages` in a TermOS window. Each source has a tab:

```
  mise   Dev rootfs (apt)   Host (apt)   Podman
 Installed: 87
 adduser   3.152              add and remove users and groups
 apt       3.0.3              commandline package manager
 …
 / search · d remove · u upgrade all · r reload · tab source · q close
```

| Key | |
| --- | --- |
| `Tab`, `←` `→`, `1`–`4` | Another source |
| `/` | Search this source |
| `Enter` | Install the selected search result (`●` marks what is installed) |
| `d` | Remove the selected package |
| `u` | Upgrade everything from this source |
| `r` | List again |
| `Esc` | Back from the search results; close |

Installing, removing and upgrading ask first, then open a window that runs
the source's own command, so you can answer apt's questions or type `sudo`'s
password, and read what happened. The list follows when the source's package
database changes, whether from that window or from elsewhere.

A source that cannot be used here says why: the dev rootfs offers to create
itself (`c`), mise to fetch itself (`f`).

## `termos pkg`

The window's work, in any terminal:

```bash
termos pkg sources                    # the sources and whether each is ready
termos pkg list [SOURCE]              # what is installed
termos pkg search devroot clang
termos pkg install devroot clang      # apt install clang, in the dev rootfs
termos pkg install mise jq            # mise use --global jq@latest
termos pkg remove host htop           # sudo apt-get remove htop (or dnf, pacman…)
termos pkg remove podman NAME --container
termos pkg upgrade devroot
termos pkg setup mise                 # fetch mise (pinned and checked)
```

The sources are `mise`, `devroot`, `host` and `podman`. A package name is one
word: no options and no shell characters.
