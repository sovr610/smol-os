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
	# On GPT (D-124) the core image is in the BIOS boot partition, 1 MiB
	# in (sector 2048), as grub-bios-setup would put it: boot.img reads it
	# from the sector named at 0x5c, and the core image's first sector
	# reads the rest from the sector its blocklist names at 0x1f4. Both
	# are little-endian 64-bit sector numbers.
	printf '\000\010\000\000\000\000\000\000' |
		dd of="$BINARIES_DIR/boot.img" bs=1 seek=92 conv=notrunc 2>/dev/null
	printf '\001\010\000\000\000\000\000\000' |
		dd of="$BINARIES_DIR/grub.img" bs=1 seek=500 conv=notrunc 2>/dev/null
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

# The x86_64 live ISO (D-080): GRUB for BIOS (El Torito, and an MBR for USB
# sticks) and UEFI, the kernel, the root file system as an initramfs, and
# the disk image `termos install` writes. Buildroot's own ISO step runs
# before this script, so before the disk image exists.
if [ "$BOARD" = x86_64 ]; then
	ISO="$BUILD_DIR/termos-iso"
	rm -rf "$ISO"
	mkdir -p "$ISO/boot/grub" "$ISO/termos"
	cp "$BINARIES_DIR/bzImage" "$ISO/boot/bzImage"
	cp "$BINARIES_DIR/rootfs.cpio.zst" "$ISO/boot/initrd.img"
	cp "$BOARD_DIR/live-grub.cfg" "$ISO/boot/grub/grub.cfg"
	zstd -q -f -T0 -10 "$BINARIES_DIR/disk.img" -o "$ISO/termos/disk.img.zst"
	stat -c %s "$BINARIES_DIR/disk.img" >"$ISO/termos/disk.img.size"
	cp "$TARGET_DIR/etc/termos/version" "$ISO/termos/version"
	GRUB_PC=$(echo "$BUILD_DIR"/grub2-*/build-i386-pc/grub-core)
	GRUB_EFI=$(echo "$BUILD_DIR"/grub2-*/build-x86_64-efi/grub-core)
	"$HOST_DIR/bin/grub-mkimage" -d "$GRUB_PC" -O i386-pc-eltorito \
		-o "$ISO/boot/grub/eltorito.img" -p /boot/grub -c "$BOARD_DIR/live-early.cfg" \
		biosdisk iso9660 part_msdos normal linux search search_label configfile echo test
	"$HOST_DIR/bin/grub-mkimage" -d "$GRUB_EFI" -O x86_64-efi \
		-o "$BUILD_DIR/live-bootx64.efi" -p /boot/grub -c "$BOARD_DIR/live-early.cfg" \
		iso9660 part_msdos part_gpt fat normal linux search search_label configfile echo test efi_gop
	rm -f "$ISO/boot/efi.img"
	mkdosfs -n TERMOSEFI -C "$ISO/boot/efi.img" 4096 >/dev/null
	mmd -i "$ISO/boot/efi.img" ::/EFI ::/EFI/BOOT
	mcopy -i "$ISO/boot/efi.img" "$BUILD_DIR/live-bootx64.efi" ::/EFI/BOOT/BOOTX64.EFI
	"$HOST_DIR/bin/xorriso" -as mkisofs -quiet -r -J -V TERMOS_LIVE \
		-b boot/grub/eltorito.img -no-emul-boot -boot-load-size 4 -boot-info-table \
		--grub2-boot-info --grub2-mbr "$GRUB_PC/boot_hybrid.img" \
		--efi-boot boot/efi.img -efi-boot-part --efi-boot-image --protective-msdos-label \
		-o "$BINARIES_DIR/termos-x86_64.iso" "$ISO"
fi
