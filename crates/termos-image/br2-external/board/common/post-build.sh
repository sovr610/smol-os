#!/bin/sh
# After the root file system is assembled, before it is packed (read-only).
set -eu
TARGET_DIR=$1

# Mount points for the data partition and what is bound from it: iwd's
# networks, dhcpcd's leases and DUID. (D-Bus's /var/lib/dbus is Buildroot's
# link to /tmp/dbus, which the data partition is bound onto at boot.)
mkdir -p "$TARGET_DIR/data" "$TARGET_DIR/var/lib/iwd" "$TARGET_DIR/var/db/dhcpcd"
