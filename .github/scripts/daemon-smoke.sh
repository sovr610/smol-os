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

# On failure: show what the server logged, and don't leave it running.
finish() {
  local status=$?
  if [ "$status" -ne 0 ]; then
    echo "daemon smoke: FAILED (exit $status)" >&2
    for log in "$XDG_STATE_HOME"/termos/log/*; do
      [ -f "$log" ] && { echo "--- $log" >&2; cat "$log" >&2; }
    done
    termos kill-server > /dev/null 2>&1 || true
  fi
  return "$status"
}
trap finish EXIT

# Prints what it checks, so a failure shows the output.
expect() {
  local pattern="$1" out
  shift
  out="$(termos "$@")"
  printf '$ termos %s\n%s\n' "$*" "$out"
  grep -q -- "$pattern" <<< "$out"
}

termos new -d -s ci
expect '^ci: 1 workspaces, 1 panes' ls
expect '"session": "ci"' msg pane.list
termos msg notify.send '{"text": "hello from CI"}' > /dev/null
expect '"sessions": 1' msg server.info
termos kill-server
if termos ls > /dev/null 2>&1; then
  echo "the server is still running after kill-server" >&2
  false
fi
echo "daemon smoke: ok"
