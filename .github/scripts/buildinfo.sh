#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Prints the BUILDINFO.txt shipped with CI test builds and releases. $1 is
# the target.
set -euo pipefail
# TERMOS_RELEASE (a version) marks a release build.
if [ -n "${TERMOS_RELEASE:-}" ]; then
	what="TermOS $TERMOS_RELEASE"
else
	what="TermOS test build (not a release)"
fi
cat <<INFO
$what

target:  $1
commit:  ${GITHUB_SHA:-unknown}
branch:  ${GITHUB_REF_NAME:-unknown}
built:   $(date -u +%Y-%m-%dT%H:%M:%SZ)
CI run:  ${GITHUB_SERVER_URL:-https://github.com}/${GITHUB_REPOSITORY:-}/actions/runs/${GITHUB_RUN_ID:-}

Run \`termos\` in a terminal to start (Ctrl+B d detaches, \`termos\` again
reattaches). See docs/getting-started.md for unblocking downloaded
binaries on macOS and Windows.
INFO
