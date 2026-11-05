# The gallery

Themes, layouts and status-line widgets made by others, to install with one
command or from the desktop (M13g, D-125, D-126).

> New in M13g. The gallery's repository (`sovr610/termos-gallery`) and its
> signing key are the owner's to set up: until this build of TermOS carries
> the key, `termos gallery` says so and installs nothing.

```bash
termos gallery list
```

```bash
termos gallery search dark blue
```

```bash
termos gallery install nord --use
```

| Command | |
| --- | --- |
| `termos gallery list [--kind theme\|layout\|widget]` | Everything, installed ones marked (✓, or ↑ when newer) |
| `termos gallery search WORDS` | Entries whose name or summary has all the words |
| `termos gallery show NAME` | Its author, licence, address and checksum |
| `termos gallery install NAME [--use]` | Download, check and install it (`--use`: a theme is used at once) |
| `termos gallery use NAME` | Use an installed theme |
| `termos gallery update [NAME]` | Install newer versions of what you have |
| `termos gallery remove NAME` | Take it away again |

From the desktop: ◆ Menu → Apps → **Gallery**, or Settings → Gallery.
Type to search; `Enter` installs, `Ctrl+U` uses a theme, `Ctrl+D` removes,
`Ctrl+R` loads the index again.

## Where things go

| Kind | File | Used by |
| --- | --- | --- |
| theme | `themes/NAME.toml` beside your config | `[theme] name = "NAME"` (the colours you set in `[theme]` still win) |
| layout | `layouts/NAME.toml` | `termos layout open NAME`, the palette ([layouts.md](layouts.md)) |
| widget | `widgets/NAME.wasm` beside your config | a `[widgets.NAME]` table added to your config ([widgets.md](widgets.md)) |

What was installed is recorded in TermOS's state folder
(`gallery/installed.json`), which `termos settings export` carries.

## Trust

- **Signed:** every entry of the index is signed with the gallery's
  Ed25519 key, over all its fields, and TermOS checks every signature with
  the key built into it before it shows anything. One entry that does not
  verify and the whole index is refused.
- **Checked:** a download must match the SHA-256 its entry gives, and be
  what its kind says (a theme file, a layout TermOS reads, a WebAssembly
  module), before anything is written.
- **Contained:** themes and layouts are data. Widgets run in the WASM
  sandbox (D-051): no network, no files beyond the folders their
  `[widgets.NAME] dirs` grant, which installing does not set.
- **Reviewed:** entries are added by pull request to the gallery's
  repository, and the owner reviews each before signing it.
- **Native plugins and saved commands** are not in the gallery yet: they
  are programs and commands, and come later with a review of what each asks
  for.

## For the gallery's maintainers

The index is one JSON file (`index.json`):

```json
{
  "format": 1,
  "key": "<the signing key's id>",
  "entries": [
    {
      "name": "nord",
      "kind": "theme",
      "version": "1.0.0",
      "summary": "An arctic, north-bluish palette",
      "url": "https://raw.githubusercontent.com/sovr610/termos-gallery/main/themes/nord.toml",
      "sha256": "<64 lowercase hex digits>",
      "author": "Someone",
      "license": "MIT",
      "signature": ""
    }
  ]
}
```

- **Names:** lowercase letters, digits and `-`, at most 40. Addresses are
  `https`. No field holds a line break.
- **The key, once:** `termos gallery keygen gallery.key` writes a new key
  (keep the file offline) and prints the line to add to `KEYS` in
  `crates/termos-apps/src/gallery.rs`. A release with that line is the first
  that trusts the gallery. To replace a key, list the new one beside the
  old, sign with the new one, and drop the old in a later release.
- **Signing:** after reviewing a change, `termos gallery sign index.json
  --key gallery.key` fills in every entry's signature and the key's id.
- **Trying an index:** `[gallery] index = "/path/to/index.json"` makes
  TermOS read a local file (still checked against the built-in keys).
