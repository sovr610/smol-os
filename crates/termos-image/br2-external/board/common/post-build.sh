#!/bin/sh
# After the root file system is assembled, before it is packed (read-only).
set -eu
TARGET_DIR=$1

# Mount points for the data partition and what is bound from it.
mkdir -p "$TARGET_DIR/data" "$TARGET_DIR/var/lib/iwd"
