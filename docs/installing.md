# Installing TermOS

TermOS installs the way other programs do: a package for your system, or
the release's download. Every channel takes the same binary from the same
signed release (D-125, D-126).

> New in M13g. The `.deb` and `.rpm` are built and installed in CI with
> each release; Homebrew, Scoop, winget, the AUR and Nix are filled in and
> tested by the `packages` workflow once a release is published, and are
> published only when the owner's account for each is set up
> ([releasing.md](releasing.md#package-channels)).

| System | Install | Update |
| --- | --- | --- |
| Debian, Ubuntu (glibc 2.31+) | `sudo apt install ./termos_<version>_amd64.deb` (or `_arm64`) from the release | the next release's `.deb` |
| Fedora | `sudo dnf install ./termos-<version>-1.x86_64.rpm` (or `.aarch64`) | the next release's `.rpm` |
| macOS, Linux (Homebrew) | `brew install sovr610/termos/termos` | `brew upgrade termos` |
| Windows (Scoop) | `scoop bucket add termos https://github.com/sovr610/scoop-termos`, then `scoop install termos` | `scoop update termos` |
| Windows (winget) | `winget install sovr610.TermOS` | `winget upgrade sovr610.TermOS` |
| Arch Linux | `yay -S termos-bin` (any AUR helper) | the helper's update |
| Nix | `nix profile install github:sovr610/smol-os` | `nix profile upgrade termos` |
| Anything else | the release's `.tar.gz` or `.zip` ([getting-started.md](getting-started.md#install-and-run)) | the next release's |

Every package puts the shell completions (bash, zsh, fish) and the man page
(`man termos`) in place. For a download, make them yourself:

```bash
termos completions bash > ~/.local/share/bash-completion/completions/termos
```

```bash
termos completions zsh > ~/.zfunc/_termos
```

```bash
termos completions fish > ~/.config/fish/completions/termos.fish
```

```bash
termos man > ~/.local/share/man/man1/termos.1
```

PowerShell and elvish completions come the same way (`termos completions
powershell`, `elvish`).

**TermOS as the whole system** (a PC, an Arm machine or a Raspberry Pi) is
an image, not a package: see [bare-metal.md](bare-metal.md). Images update
themselves with `termos update`.

## Moving to another machine

Your settings go with you in one file:

```bash
termos settings export ~/termos-settings.tar.gz
```

```bash
termos settings import ~/termos-settings.tar.gz
```

Or from the desktop: Settings → **Export settings**, **Import settings**.

- **In the archive:** `config.toml`, your layouts and themes, what you
  installed from the [gallery](gallery.md) (its list; they are installed
  again from the gallery), and the names of your AI accounts.
- **Never in it:** API keys, the secrets file, the keyring, agents'
  sign-ins, conversations, histories, logs or saved sessions. If a file
  holds something shaped like a key (one pasted into the config by hand),
  the export stops and says which line: keep keys in the keyring
  (`termos secret set NAME`, then `keyring:NAME` in the config).
- **Importing** shows what each file would change. The config is merged
  into yours: the archive's values win, and everything else of yours stays,
  comments included. A layout or theme you already have and that differs
  is replaced only if you say so (`--yes` replaces without asking). AI
  accounts are listed, to sign in to again (`termos account add CLI NAME`).

### In a dotfiles repository

The config folder can be a link to a folder in your dotfiles repository:

```bash
mv ~/.config/termos ~/dotfiles/termos
```

```bash
ln -s ~/dotfiles/termos ~/.config/termos
```

Everything TermOS writes there is text you can commit (`config.toml`,
`layouts/`, `themes/`); keys are never in it. `XDG_CONFIG_HOME` moves the
whole folder too.
