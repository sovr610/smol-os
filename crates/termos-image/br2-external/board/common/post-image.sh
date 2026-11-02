#!/bin/sh
# Lays out the disk image for a board (D-075): its boot partition, root
# slots A and B, and the data partition. $2 is the board (BR2_ROOTFS_POST_SCRIPT_ARGS).
set -eu
BOARD=$2
BOARD_DIR="$BR2_EXTERNAL_TERMOS_PATH/board/$BOARD"
case "$BOARD" in rpi*) BOARD_DIR="$BR2_EXTERNAL_TERMOS_PATH/board/rpi" ;; esac

# The boot partition's files, gathered in one folder.
ESP="$BINARIES_DIR/esp"
rm -rf "$ESP"
mkdir -p "$ESP"
case "$BOARD" in
x86_64)
	mkdir -p "$ESP/EFI/BOOT" "$ESP/boot/grub"
	cp "$BINARIES_DIR/efi-part/EFI/BOOT/bootx64.efi" "$ESP/EFI/BOOT/"
	cp "$BOARD_DIR/grub.cfg" "$ESP/EFI/BOOT/grub.cfg"
	cp "$BOARD_DIR/grub.cfg" "$ESP/boot/grub/grub.cfg"
	cp "$BINARIES_DIR/bzImage" "$ESP/"
	# GRUB's BIOS first stage, for the MBR; the package leaves it in its
	# build folder (its core image, grub.img, is in the images already).
	cp "$BUILD_DIR"/grub2-*/build-i386-pc/grub-core/boot.img "$BINARIES_DIR/boot.img"
	;;
aarch64-virt)
	mkdir -p "$ESP/EFI/BOOT"
	cp "$BINARIES_DIR/efi-part/EFI/BOOT/bootaa64.efi" "$ESP/EFI/BOOT/"
	cp "$BOARD_DIR/grub.cfg" "$ESP/EFI/BOOT/grub.cfg"
	cp "$BINARIES_DIR/Image" "$ESP/"
	;;
rpi*)
	cp -r "$BINARIES_DIR"/rpi-firmware/* "$ESP/"
	cp "$BINARIES_DIR"/*.dtb "$ESP/" 2>/dev/null || true
	cp "$BINARIES_DIR/Image" "$ESP/"
	cp "$BOARD_DIR/cmdline.txt" "$ESP/cmdline.txt"
	# The KMS display driver.
	grep -q '^dtoverlay=vc4-kms-v3d' "$ESP/config.txt" || echo 'dtoverlay=vc4-kms-v3d' >> "$ESP/config.txt"
	# The firmware wants its files at the top of the partition.
	rm -f "$BINARIES_DIR/boot.vfat"
	mkdosfs -n TERMOS -C "$BINARIES_DIR/boot.vfat" 131072
	mcopy -s -i "$BINARIES_DIR/boot.vfat" "$ESP"/* ::
	;;
*)
	echo "post-image: unknown board $BOARD" >&2
	exit 1
	;;
esac

GENIMAGE_TMP="$BUILD_DIR/genimage.tmp"
rm -rf "$GENIMAGE_TMP"
genimage \
	--rootpath "$TARGET_DIR" \
	--tmppath "$GENIMAGE_TMP" \
	--inputpath "$BINARIES_DIR" \
	--outputpath "$BINARIES_DIR" \
	--config "$BOARD_DIR/genimage.cfg"
