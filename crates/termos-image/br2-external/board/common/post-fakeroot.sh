#!/bin/sh
# Inside fakeroot, after Buildroot has written the users' files and
# before the root file system is packed (read-only). $1 is the tree.
set -eu
TARGET_DIR=$1

# The users' passwords live on the data partition (D-124): the lock
# screen's password is the system user's, set while the system runs.
# S11termos-data copies this default there on first boot.
mkdir -p "$TARGET_DIR/usr/share/termos"
if [ ! -L "$TARGET_DIR/etc/shadow" ]; then
	mv "$TARGET_DIR/etc/shadow" "$TARGET_DIR/usr/share/termos/shadow"
	ln -s /data/etc/shadow "$TARGET_DIR/etc/shadow"
fi
