#!/bin/sh
# Lays out the disk image for a board (D-075): its boot partition, root
# slots A and B, and the data partition. $2 is the board (BR2_ROOTFS_POST_SCRIPT_ARGS).
set -eu
BOARD=$2
BOARD_DIR="$BR2_EXTERNAL_TERMOS_PATH/board/$BOARD"
case "$BOARD" in rpi*) BOARD_DIR="$BR2_EXTERNAL_TERMOS_PATH/board/rpi" ;; esac

# GRUB's environment block (D-078): 1024 bytes, padded with '#'. The first
# boot takes slot A, which is good; slot B is empty until an update.
write_grubenv() {
	printf '# GRUB Environment Block\nORDER=A B\nA_OK=1\nA_TRY=0\nB_OK=0\nB_TRY=0\n' >"$1"
	head -c $((1024 - $(wc -c <"$1"))) /dev/zero | tr '\0' '#' >>"$1"
}

# The boot partition's files, gathered in one folder. On a PC or a UEFI
# machine that is GRUB, its configuration and its environment; the kernels
# are inside the root slots.
ESP="$BINARIES_DIR/esp"
rm -rf "$ESP"
mkdir -p "$ESP"
case "$BOARD" in
x86_64)
	mkdir -p "$ESP/EFI/BOOT" "$ESP/boot/grub"
	cp "$BINARIES_DIR/efi-part/EFI/BOOT/bootx64.efi" "$ESP/EFI/BOOT/"
	cp "$BOARD_DIR/grub.cfg" "$ESP/EFI/BOOT/grub.cfg"
	cp "$BOARD_DIR/grub.cfg" "$ESP/boot/grub/grub.cfg"
	write_grubenv "$ESP/grubenv"
	# GRUB's BIOS first stage, for the MBR; the package leaves it in its
	# build folder (its core image, grub.img, is in the images already).
	cp "$BUILD_DIR"/grub2-*/build-i386-pc/grub-core/boot.img "$BINARIES_DIR/boot.img"
	;;
aarch64-virt)
	mkdir -p "$ESP/EFI/BOOT"
	cp "$BINARIES_DIR/efi-part/EFI/BOOT/bootaa64.efi" "$ESP/EFI/BOOT/"
	cp "$BOARD_DIR/grub.cfg" "$ESP/EFI/BOOT/grub.cfg"
	write_grubenv "$ESP/grubenv"
	;;
rpi*)
	# The firmware's files at the top; each slot's kernel, device trees,
	# overlays and command line under slot-a/ and slot-b/, picked by
	# config.txt's os_prefix (D-078). The image starts on slot A.
	for f in "$BINARIES_DIR"/rpi-firmware/*; do
		case "${f##*/}" in overlays | cmdline.txt) ;; *) cp -r "$f" "$ESP/" ;; esac
	done
	mkdir -p "$ESP/slot-a"
	cp -r "$BINARIES_DIR/rpi-firmware/overlays" "$ESP/slot-a/"
	cp "$BINARIES_DIR"/*.dtb "$ESP/slot-a/" 2>/dev/null || true
	cp "$BINARIES_DIR/Image" "$ESP/slot-a/"
	sed 's/@PART@/02/; s/@SLOT@/A/' "$BOARD_DIR/cmdline.txt" >"$ESP/slot-a/cmdline.txt"
	# The KMS display driver.
	grep -q '^dtoverlay=vc4-kms-v3d' "$ESP/config.txt" || echo 'dtoverlay=vc4-kms-v3d' >>"$ESP/config.txt"
	echo 'os_prefix=slot-a/' >>"$ESP/config.txt"
	printf 'A=good\nprimary=A\n' >"$ESP/termos-ab.txt"
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

# genimage fills file systems that name no files (the data partition)
# from --rootpath: an empty folder, so the data partition starts empty
# (the root file system is Buildroot's rootfs.squashfs).
GENIMAGE_TMP="$BUILD_DIR/genimage.tmp"
EMPTY="$BUILD_DIR/genimage.empty"
rm -rf "$GENIMAGE_TMP" "$EMPTY"
mkdir -p "$EMPTY"
genimage \
	--rootpath "$EMPTY" \
	--tmppath "$GENIMAGE_TMP" \
	--inputpath "$BINARIES_DIR" \
	--outputpath "$BINARIES_DIR" \
	--config "$BOARD_DIR/genimage.cfg"
