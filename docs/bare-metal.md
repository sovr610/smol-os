# TermOS on bare metal

The TermOS images boot straight into TermOS: no window system and no login
prompt, just the desktop on the screen. It is the same desktop as in a
terminal, drawn by TermOS's own console (`termos --kms`) through the
kernel's display drivers.

| Image | Hardware | Built in CI |
| --- | --- | --- |
| `x86_64` | PCs and virtual machines, BIOS or UEFI; also as a live ISO | built, booted in QEMU (UEFI and BIOS), updated, installed from the ISO |
| `aarch64-virt` | 64-bit Arm with UEFI firmware (servers, QEMU's `virt`) | built, and booted in QEMU |
| `rpi5`, `rpi4` | Raspberry Pi 5 and 4 | on request |
| `rpi3`, `rpi-zero2w` | Raspberry Pi 3 and Zero 2 W (best effort: 512 MB–1 GB) | on request |

## Installing on a PC

1. Write the release's `termos-<version>-x86_64.iso` to a USB stick (with
   any image writer, or `dd`), or attach it to a virtual machine, and start
   from it. TermOS runs from memory and keeps nothing; the disks are not
   touched.
2. Open a terminal pane and run `termos install`. It lists the disks it can
   use (never the one it started from); type the name of the one to put
   TermOS on. **Everything on that disk is erased**: TermOS takes the whole
   disk.
3. Take out the stick (or the ISO) and restart: `termos update restart`.

On its first start the installed system grows its data partition to the
end of the disk.

The disk images (`termos-<version>-<target>.img.zst`) are the other way,
and the only one for Arm machines and Raspberry Pis: unpack and write one
to the disk or SD card (see [below](#writing-it-to-a-disk-or-sd-card)).
They grow the same way.

## Updates

A TermOS machine has two copies of its system, A and B: it runs from one
while an update is written to the other (D-078).

```sh
termos update check      # is there a newer release for this machine?
termos update install    # download it and write it to the other copy
termos update restart    # start the new one
termos update status     # which copy runs, and how both are doing
```

From the desktop, **Updates** in the palette (or Settings → System →
Updates) opens a window that does the same: it says whether a newer release
exists, asks before installing it, and offers to restart.

Nothing is checked or downloaded unless you ask. Updates come from the
repository's GitHub releases and must be signed by its owner's key, which
the system checks before writing anything; `termos update install <file or
URL>` installs a bundle you already have.

The new copy is tried once. When its desktop comes up it becomes the one
the machine starts from; if it does not come up within five minutes, or the
kernel cannot start it, the machine restarts and goes back to the copy that
worked, and your data is untouched either way (it is on its own partition).
On a Raspberry Pi this uses the firmware's `tryboot`; it has not been tried
on a board yet.

## Network

Wired networks get an address by themselves (DHCP). For Wi-Fi, open
**Network** from the palette (`Ctrl+Space`, type `network`): it shows each
connection with its addresses and the Wi-Fi networks in range, strongest
first. Choose one and press `Enter` (a passphrase is asked for, and shown
as dots); TermOS saves it and joins it whenever it is in range. `d` forgets
a saved network, `r` scans again. Enterprise (802.1X) networks are not
supported here yet.

## Building an image

On a Linux machine with Buildroot's usual host tools (`build-essential`,
`rsync`, `bc`, `cpio`, `unzip`, `file`, `wget`), Rust as the repository
pins it, and the Rust target for the board:

```sh
rustup target add aarch64-unknown-linux-gnu   # for the Arm images
cargo xtask image list
cargo xtask image build x86_64
```

Building also makes the update bundle, `termos-<target>-<version>.raucb`
next to the image, and on x86_64 the live ISO, `termos-x86_64.iso`. Bundles
are signed with development keys made in `target/image/keys/` unless
`TERMOS_RAUC_KEYRING`, `TERMOS_RAUC_CERT` and `TERMOS_RAUC_KEY` name a
release's (see [releasing.md](releasing.md)); an image accepts only bundles
signed with its own keys. `TERMOS_IMAGE_VERSION` sets the version.

The first build downloads Buildroot (a pinned release, checked against its
SHA-256), Bootlin's prebuilt toolchain, the kernel and the packages into
`target/image/dl`, and takes 40 minutes to an hour on four cores (the
generic Arm kernel, with all its modules, is the slow part); later builds
reuse what they can. The image is `target/image/<target>/images/disk.img`.

`cargo xtask image build <target> --legal-info` also writes Buildroot's
license report (every package's license and source) to
`target/image/<target>/legal-info`.

## Trying it in QEMU

```sh
cargo xtask image boot x86_64              # UEFI, then BIOS
cargo xtask image boot aarch64-virt
cargo xtask image boot x86_64 --firmware bios
cargo xtask image boot x86_64 --update --install
```

That is the boot test CI runs: it waits for the console's first frame,
takes a screenshot (`target/image/<target>-boot/screen-*.png`), and checks
the memory in use once the desktop has settled (the budget is 150 MB) and
that the data partition grew into a disk 1 GiB bigger than the image.
`--update` then puts a newer version on the same disk with `termos update`
and starts it, then a broken one, which falls back; `--install` starts the
live ISO with an empty disk, installs onto it with `termos install`, and
starts the disk on its own. It needs `qemu-system-x86` / `qemu-system-arm`,
`qemu-utils` and the UEFI firmware (`ovmf`, `qemu-efi-aarch64`; or set
`TERMOS_OVMF` / `TERMOS_AAVMF`). When the console
does not come up, it says so with the end of the serial log, and keeps a
screenshot of what the machine got to.

CI's `images` workflow keeps, for two weeks, each boot test's screenshots
and serial logs (the `boot-<target>` artifact) and the image itself,
compressed with zstd, with its license report (`image-<target>`).

To use the desktop yourself, run QEMU with a window:

```sh
qemu-system-x86_64 -M q35 -m 2048 -accel kvm -bios /usr/share/ovmf/OVMF.fd \
  -drive file=target/image/x86_64/images/disk.img,format=raw,if=virtio,snapshot=on \
  -vga none -device virtio-gpu-pci -device virtio-keyboard-pci -device virtio-tablet-pci
```

## Writing it to a disk or SD card

```sh
sudo dd if=target/image/rpi4/images/disk.img of=/dev/sdX bs=4M conv=fsync status=progress
```

Check `/dev/sdX` twice: everything on it is replaced.

## The console

| Keys | |
| --- | --- |
| Ctrl+= / Ctrl+- | bigger / smaller text |
| Ctrl+0 | the configured size again |
| Ctrl+Alt+F1…F12 | another virtual terminal (TermOS is on tty1) |

Everything else is the desktop's (`C-b` is the prefix, Ctrl+Space the
palette; see [getting-started.md](getting-started.md)).

Settings, in `~/.config/termos/config.toml`:

```toml
[kms]
font-size = 16.0         # pixels at 96 dpi; scaled up on dense screens
keyboard-layout = "us"   # XKB layout, as for setxkbmap: "de", "fr", "gb"…
keyboard-variant = ""    # "dvorak", "nodeadkeys"…
keyboard-options = ""    # "ctrl:nocaps,compose:ralt"…
repeat-delay = 400       # ms before a held key repeats
repeat-rate = 25         # repeats per second
# font = "/data/fonts/MyFont.ttf"        # default: built-in DejaVu Sans Mono
# bold-font = "/data/fonts/MyFont-Bold.ttf"
# fallback-fonts = ["/data/fonts/NotoSansCJK-Regular.ttc"]
```

The console draws the desktop's cells into the display's memory on the
CPU, so it works on any display the kernel drives (virtio-gpu, Intel,
simple framebuffers from UEFI, the Raspberry Pi's VideoCore) without GPU
drivers. Pictures are shown as blocks of colour for now; drawing them as
pixels, a console clipboard and a visible bell come later.

`termos --kms` also works on a desktop Linux machine, from a virtual
terminal (Ctrl+Alt+F3), when TermOS is built with the `kms` feature:

```sh
cargo build --release -p termos-cli --features kms
```

It needs libinput, libudev, libxkbcommon and libseat (with seatd or
logind) to build and run.

## Inside the image

```
1  boot      FAT: GRUB, its configuration and its A/B state (a Pi: its firmware, config.txt,
             and each slot's kernel and device trees under slot-a/ and slot-b/)
2  rootfs-a  system copy A, read-only (squashfs), with its kernel in /boot
3  rootfs-b  system copy B, written by updates
4  data      ext4, label termos-data: /home, Wi-Fi networks, update records, and later the
             dev rootfs, models, containers; grown to the end of the disk on first boot
```

- The system partition is read-only; what you keep lives on the data
  partition, mounted on `/data`, with `/home`, iwd's networks, D-Bus's
  machine ID and dhcpcd's leases bound from it.
- BusyBox init starts udev, D-Bus, iwd (Wi-Fi), dhcpcd (addresses) and
  seatd, then runs TermOS's console on tty1 as the user `termos`. If the
  console stops, init starts it again and it reattaches to the running
  desktop.
- Logs are in `~/.local/state/termos/log/` (on the data partition) and on
  the system console.

## Limits

- The disks use an MBR partition table, so the data partition stops at
  2 TiB, and every TermOS disk has the same partition IDs: keep only one in
  a machine.
- AMD graphics cards need firmware the images do not carry yet; they get
  the UEFI framebuffer (simpledrm) instead.
- The Raspberry Pi images are built on request and have not been tested on
  hardware by CI, nor have their updates.
- How fast keys reach the screen is measured by the console itself: every
  100 keys it logs `input to frame: p50 … p99 … worst …; on the screen: …`
  in `~/.local/state/termos/log/`. The first figures run to the frame being
  handed to the display, which is what the budget counts; the second add
  the wait for the screen's next refresh. The Pi 4's budget is 33 ms (p99);
  it is to be measured on a real board.
