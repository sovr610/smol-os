#!/bin/sh
# After the root file system is assembled, before it is packed (read-only).
# $2 is the board (BR2_ROOTFS_POST_SCRIPT_ARGS). `cargo xtask image build`
# passes TERMOS_IMAGE_VERSION and TERMOS_RAUC_KEYRING (D-078).
set -eu
TARGET_DIR=$1
BOARD=$2
VERSION=${TERMOS_IMAGE_VERSION:?set by cargo xtask image build}
KEYRING=${TERMOS_RAUC_KEYRING:?set by cargo xtask image build}

# Mount points for the data partition and what is bound from it: iwd's
# networks, dhcpcd's leases and DUID. (D-Bus's /var/lib/dbus is Buildroot's
# link to /tmp/dbus, which the data partition is bound onto at boot.) The
# boot partition is mounted on /boot/esp, for RAUC and the boot loader.
mkdir -p "$TARGET_DIR/data" "$TARGET_DIR/var/lib/iwd" "$TARGET_DIR/var/db/dhcpcd" \
	"$TARGET_DIR/boot/esp"

# What this system is: `termos update` reads these (D-078).
mkdir -p "$TARGET_DIR/etc/termos"
echo "$BOARD" >"$TARGET_DIR/etc/termos/board"
echo "$VERSION" >"$TARGET_DIR/etc/termos/version"
cat >"$TARGET_DIR/usr/lib/os-release" <<EOF
NAME=TermOS
ID=termos
VERSION_ID=$VERSION
PRETTY_NAME="TermOS $VERSION"
HOME_URL="https://github.com/sovr610/smol-os"
EOF

# RAUC (D-078): the two root slots, the boot loader that chooses between
# them, and the keyring bundles must be signed for.
mkdir -p "$TARGET_DIR/etc/rauc"
cp "$KEYRING" "$TARGET_DIR/etc/rauc/keyring.pem"
case "$BOARD" in
rpi*)
	BOOTLOADER="bootloader=custom"
	HANDLERS="[handlers]
bootloader-custom-backend=/usr/libexec/termos/rauc-tryboot
post-install=/usr/libexec/termos/rauc-tryboot-install"
	;;
*)
	BOOTLOADER="bootloader=grub
grubenv=/boot/esp/grubenv"
	HANDLERS=""
	;;
esac
cat >"$TARGET_DIR/etc/rauc/system.conf" <<EOF
[system]
compatible=termos-$BOARD
$BOOTLOADER
bundle-formats=-plain
mountprefix=/run/rauc
data-directory=/data/rauc

[keyring]
path=/etc/rauc/keyring.pem
check-purpose=codesign

$HANDLERS

[slot.rootfs.0]
device=/dev/disk/by-partuuid/7e57c0de-02
type=raw
bootname=A

[slot.rootfs.1]
device=/dev/disk/by-partuuid/7e57c0de-03
type=raw
bootname=B
EOF

# A Pi's slots carry their boot files (D-078): the kernel and device trees
# are in /boot already; add the firmware's overlays and the command line's
# template, which `termos system rauc-tryboot-install` copies to the boot
# partition when the slot is installed.
case "$BOARD" in
rpi*)
	mkdir -p "$TARGET_DIR/boot/overlays" "$TARGET_DIR/usr/share/termos"
	cp -r "$BINARIES_DIR/rpi-firmware/overlays/." "$TARGET_DIR/boot/overlays/"
	cp "$BR2_EXTERNAL_TERMOS_PATH/board/rpi/cmdline.txt" "$TARGET_DIR/usr/share/termos/rpi-cmdline.txt"
	;;
esac

# `termos system` is the only way to root (D-078); sudo wants its rule
# files read-only.
chmod 0440 "$TARGET_DIR/etc/sudoers.d/termos"

# GRUB's tools are installed for grub-editenv alone (RAUC's GRUB backend).
if [ -e "$TARGET_DIR/usr/bin/grub-editenv" ]; then
	find "$TARGET_DIR/usr/bin" "$TARGET_DIR/usr/sbin" -name 'grub-*' ! -name grub-editenv -delete
	rm -rf "$TARGET_DIR/usr/lib/grub" "$TARGET_DIR/usr/share/grub" "$TARGET_DIR/etc/grub.d"
fi
