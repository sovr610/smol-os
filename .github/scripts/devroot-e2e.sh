#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# The dev rootfs and the package manager end to end (M5b, D-110):
# download Debian 13's pinned layer, check it, unpack it in a user
# namespace with the runner's subordinate IDs, enter it with bubblewrap;
# through `termos pkg` (what the package manager app lists with and runs
# in its windows) search, install and remove a package with apt and a
# tool with mise; upgrade; delete it. Runs in an isolated data folder.
set -euo pipefail
T="$(realpath "${1:?usage: devroot-e2e.sh <termos binary>}")"

base="$(mktemp -d)"
export XDG_CONFIG_HOME="$base/config" XDG_STATE_HOME="$base/state" XDG_DATA_HOME="$base/data"
export XDG_RUNTIME_DIR="$base/run"
unset TERMOS TERMOS_SOCKET
mkdir -p "$XDG_RUNTIME_DIR" && chmod 700 "$XDG_RUNTIME_DIR"

"$T" devroot status | tee "$base/before"
grep -q 'bubblewrap: ready' "$base/before"

"$T" devroot create
"$T" devroot status | tee "$base/status"
grep -q 'Debian 13 (trixie) (bubblewrap)' "$base/status"

# The project folder (outside the home here) is the host's, at its own
# path; root inside is the runner outside.
cd "$base"
"$T" devroot enter -- sh -c '. /etc/os-release; echo "$PRETTY_NAME"; id -u; pwd; touch made-inside' | tee "$base/inside"
grep -q '^Debian GNU/Linux 13' "$base/inside"
grep -qx 0 "$base/inside"
grep -qx "$base" "$base/inside"
test "$(stat -c %u "$base/made-inside")" = "$(id -u)"

"$T" pkg search devroot hello | tee "$base/found"
grep -q '^  hello ' "$base/found"
"$T" pkg install devroot hello < <(yes)
"$T" pkg list devroot | tee "$base/listed"
grep -q '^  hello ' "$base/listed"
"$T" devroot enter -- hello | tee "$base/hello"
grep -q 'Hello, world!' "$base/hello"
"$T" pkg remove devroot hello < <(yes)
if "$T" pkg list devroot | grep -q '^  hello '; then exit 1; fi

# mise's source: TermOS fetches its pinned mise when there is none.
"$T" pkg setup mise
"$T" pkg search mise jq | tee "$base/mise-found"
grep -q '^  jq ' "$base/mise-found"
"$T" pkg install mise jq
"$T" pkg list mise | tee "$base/mise-listed"
grep -q '^  jq ' "$base/mise-listed"
"$T" pkg remove mise jq

"$T" devroot update
"$T" devroot status | tee "$base/updated"
if grep -q 'updated:  never' "$base/updated"; then exit 1; fi

"$T" devroot remove --yes
test ! -e "$XDG_DATA_HOME/termos/devroot"
echo "devroot: ok"
