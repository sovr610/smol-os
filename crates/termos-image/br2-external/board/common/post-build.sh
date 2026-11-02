#!/bin/sh
# After the root file system is assembled, before it is packed (read-only).
set -eu
TARGET_DIR=$1

# Mount points for the data partition and what is bound from it: iwd's
# networks, D-Bus's machine ID, dhcpcd's leases and DUID.
mkdir -p "$TARGET_DIR/data" "$TARGET_DIR/var/lib/iwd" "$TARGET_DIR/var/lib/dbus" \
	"$TARGET_DIR/var/db/dhcpcd"
