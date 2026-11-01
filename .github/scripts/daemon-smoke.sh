#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Drives a real TermOS daemon through the CLI: start a detached session,
# list it, call the control API, stop the server. $TERMOS_BIN is the
# binary. Runs in an isolated home, so it never meets a real server.
set -euo pipefail
: "${TERMOS_BIN:?set TERMOS_BIN to the termos binary}"

base="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/termos-smoke-$$"
# Git Bash on Windows: hand the native binary C:/style paths.
if command -v cygpath > /dev/null 2>&1; then
  base="$(cygpath -m "$base")"
fi
export XDG_CONFIG_HOME="$base/config" XDG_STATE_HOME="$base/state" XDG_RUNTIME_DIR="$base/run"
mkdir -p "$XDG_CONFIG_HOME/termos" "$XDG_RUNTIME_DIR"
printf '[tutorial]\nshow-on-first-run = false\n[status]\nclock = false\n' \
  > "$XDG_CONFIG_HOME/termos/config.toml"
# $TERMOS and $TERMOS_SOCKET would point the CLI at another server.
unset TERMOS TERMOS_SOCKET

termos() { "$TERMOS_BIN" "$@"; }

termos new -d -s ci
termos ls | tee /dev/stderr | grep -q '^ci: 1 workspaces, 1 panes'
termos msg pane.list | grep -q '"session": "ci"'
termos msg notify.send '{"text": "hello from CI"}' > /dev/null
termos msg server.info | grep -q '"sessions": 1'
termos kill-server
if termos ls > /dev/null 2>&1; then
  echo "the server is still running after kill-server" >&2
  exit 1
fi
echo "daemon smoke: ok"
