# Images, themes and motion

TermOS shows images in every terminal: in pixels where the terminal can draw
them, and in coloured block characters where it can't. Programs running in a
pane can print images too. This page also covers the colour themes and the
animations.

## What your terminal can do

When you attach, TermOS asks your terminal a few questions: its name, whether
it speaks the Kitty graphics protocol or Sixel, its cell size in pixels and
its background colour. This takes a few milliseconds and you won't see it. To
see what was found:

```bash
termos debug caps
```

`termos debug caps --raw` also prints the terminal's raw answers. If TermOS
gets your terminal wrong, please report them.

## How images are shown

| Your terminal | How images look |
| --- | --- |
| kitty, WezTerm, Ghostty, Konsole (Kitty graphics) | Full pixels. A window over part of an image covers exactly that part. |
| foot, xterm, Windows Terminal, iTerm2, mlterm (Sixel or iTerm2 images) | Full pixels while the whole image is visible; blocks while something covers part of it or is being dragged. |
| Anything else, and inside tmux without Sixel | Coloured blocks (`▀▄▌▐` and friends), 2×2 pixels per cell. |

Inside tmux, TermOS never uses the Kitty or iTerm2 protocols (tmux doesn't
pass them on). It uses Sixel only if tmux itself supports it.

To force one way, for every terminal:

```toml
[graphics]
protocol = "blocks"    # auto | kitty | sixel | iterm2 | blocks
```

To force it for one terminal only, set `TERMOS_GRAPHICS=blocks` (or `kitty`,
`sixel`, `iterm2`) before running `termos`.

## The image viewer

```bash
termos msg image.open '{"arg": "/home/me/Pictures/cat.png"}'
```

Or type `image.open cat.png` after `Ctrl+Space`. Relative paths are relative
to the focused pane's directory.

| Key | What it does |
| --- | --- |
| `n`, `→`, `Space` | Next image in the folder |
| `p`, `←` | Previous image |
| `z` | Fit to the window, or at most the image's own size |
| `q`, `Esc` | Close |

It reads png, jpeg, gif (the first frame), webp and bmp, up to 64 MiB per file
and 8192 pixels per side. Images are decoded in the background, so a large
one never makes the desktop wait.

## Images from programs

Programs that print images work in TermOS panes:

```bash
kitten icat photo.png        # Kitty graphics
chafa -f sixel photo.png     # Sixel
img2sixel photo.png
```

The images scroll with the text around them, and `clear` clears them, just as
in a terminal that shows images. TermOS shows them in whatever way your
terminal allows. Programs that check first get the answers they expect: Sixel
support, the cell size in pixels, and Kitty graphics support.

Limits, per pane: 16 MiB of image data and 128 images; the oldest go first.
One image may be at most 32 MiB as sent.

For safety, a program can only send image data itself. The Kitty protocol's
"read this file" and "shared memory" options are refused, so printing
something (say, `cat` of a downloaded file) can never make TermOS read a file.

On Windows, images printed inside panes aren't supported yet: Windows'
console layer can remove these sequences before TermOS sees them.

## Themes

TermOS has three themes: dark, light and high contrast. By default it picks
dark or light to match each terminal's background, so two terminals attached
to the same session can each get the one that suits them.

```toml
[theme]
base = "auto"          # auto | dark | light | high-contrast
accent = "#ff8800"     # optional: override any colour
```

To switch while running, open the palette (`Ctrl+Space`) and type `theme`.
The choice applies to that terminal only. In high contrast every text colour
has at least a 7:1 contrast ratio against its background.

Programs in panes that ask for the terminal's colours (as `vim` does to
choose its background) get your terminal's real colours.

## Motion

Floating windows grow open and shrink closed, and messages slide into the
status line. Each takes about a tenth of a second. The animations are:

- off for terminals connected over SSH (they would only slow the link);
- off with `[ui] reduced-motion = true`, or `TERMOS_REDUCED_MOTION=1` for one
  terminal;
- off everywhere with `[ui] animations = false`.

When nothing moves, nothing runs: animations cost no CPU while idle.

## Keyboard protocol for programs

Programs that use the Kitty keyboard protocol (Neovim, Helix, fish, kakoune
and others, when they ask for it) receive keys in that form inside TermOS
panes. They can then tell `Ctrl+I` from `Tab`, or `Esc` from the start of an
escape sequence. TermOS supports the protocol's "disambiguate", "all keys as
escape codes" and "associated text" modes; key release events come later.
