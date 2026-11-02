# TermOS on bare metal

The TermOS images boot straight into TermOS: no window system and no login
prompt, just the desktop on the screen. It is the same desktop as in a
terminal, drawn by TermOS's own console (`termos --kms`) through the
kernel's display drivers.

| Image | Hardware | Built in CI |
| --- | --- | --- |
| `x86_64` | PCs and virtual machines, BIOS or UEFI | built, and booted in QEMU (UEFI and BIOS) |
| `aarch64-virt` | 64-bit Arm with UEFI firmware (servers, QEMU's `virt`) | built, and booted in QEMU |
| `rpi5`, `rpi4` | Raspberry Pi 5 and 4 | on request |
| `rpi3`, `rpi-zero2w` | Raspberry Pi 3 and Zero 2 W (best effort: 512 MB–1 GB) | on request |

## Building an image

On a Linux machine with Buildroot's usual host tools (`build-essential`,
`rsync`, `bc`, `cpio`, `unzip`, `file`, `wget`), Rust as the repository
pins it, and the Rust target for the board:

```sh
rustup target add aarch64-unknown-linux-gnu   # for the Arm images
cargo xtask image list
cargo xtask image build x86_64
```

The first build downloads Buildroot (a pinned release, checked against its
SHA-256), Bootlin's prebuilt toolchain, the kernel and the packages into
`target/image/dl`, and takes about an hour; later builds reuse what they
can. The image is `target/image/<target>/images/disk.img`.

`cargo xtask image build <target> --legal-info` also writes Buildroot's
license report (every package's license and source) to
`target/image/<target>/legal-info`.

## Trying it in QEMU

```sh
cargo xtask image boot x86_64              # UEFI, then BIOS
cargo xtask image boot aarch64-virt
cargo xtask image boot x86_64 --firmware bios
```

That is the boot test CI runs: it waits for the console's first frame,
takes a screenshot (`target/image/<target>-boot/screen-*.png`), and checks
the memory in use once the desktop has settled (the budget is 150 MB). It
needs `qemu-system-x86` / `qemu-system-arm` and the UEFI firmware (`ovmf`,
`qemu-efi-aarch64`; or set `TERMOS_OVMF` / `TERMOS_AAVMF`).

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
1  boot      FAT: the boot loader, the kernel (a Pi: its firmware, device trees, config.txt, cmdline.txt)
2  rootfs-a  the system, read-only (squashfs)
3  rootfs-b  the second system slot, for updates (M9)
4  data      ext4, label termos-data: /home, Wi-Fi networks, and later the dev rootfs, models, containers
```

- The system partition is read-only; what you keep lives on the data
  partition, mounted on `/data`, with `/home` and iwd's state bound from it.
- BusyBox init starts udev, D-Bus, iwd (Wi-Fi), dhcpcd (addresses) and
  seatd, then runs TermOS's console on tty1 as the user `termos`. If the
  console stops, init starts it again and it reattaches to the running
  desktop.
- Logs are in `~/.local/state/termos/log/` (on the data partition) and on
  the system console.

## Limits

- The data partition is 512 MB in the image; growing it to the whole disk
  on first boot comes with the installer (M9). So do A/B updates.
- AMD graphics cards need firmware the images do not carry yet; they get
  the UEFI framebuffer (simpledrm) instead.
- Wired networks get an address by DHCP; choosing a Wi-Fi network needs
  the network settings page, which is M9 work.
- The Raspberry Pi images are built on request and have not been tested on
  hardware by CI; the Pi 4's keystroke latency is to be measured on a real
  board.
