################################################################################
#
# termos
#
################################################################################

# The binary comes from `cargo xtask image build` (D-074): Buildroot's Rust
# is older than the one TermOS needs, so cargo builds it against this
# configuration's sysroot, and this package installs it.
TERMOS_VERSION = local
TERMOS_SITE = $(call qstrip,$(BR2_PACKAGE_TERMOS_PREBUILT_DIR))
TERMOS_SITE_METHOD = local
TERMOS_LICENSE = GPL-3.0-or-later
TERMOS_LICENSE_FILES = LICENSE
TERMOS_DEPENDENCIES = libinput libxkbcommon seatd xkeyboard-config

define TERMOS_INSTALL_TARGET_CMDS
	$(INSTALL) -D -m 0755 $(@D)/termos $(TARGET_DIR)/usr/bin/termos
endef

# The console runs as `termos`; seatd lets the group `seat` have the
# display and input devices.
define TERMOS_USERS
	termos -1 termos -1 ! /home/termos /bin/sh seat,video,input,audio TermOS
endef

$(eval $(generic-package))
