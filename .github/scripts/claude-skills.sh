#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Runs Claude Code once against the stand-in model server (a fixture key,
# no account) and checks that what it sent the model lists TermOS's
# skills (D-098). Usage: claude-skills.sh CLAUDE PORT
set -euo pipefail
claude="$1"
port="$2"
log="$RUNNER_TEMP/claude-requests-$port.jsonl"
py=$(command -v python3 || command -v python)
rm -f "$log"
"$py" .github/scripts/refusing-model.py "$port" "$log" &
server=$!
trap 'kill $server 2>/dev/null || true' EXIT
sleep 2
ANTHROPIC_BASE_URL="http://127.0.0.1:$port" ANTHROPIC_API_KEY=fixture-key \
  CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 DISABLE_AUTOUPDATER=1 \
  "$claude" -p "say hello" < /dev/null > "$RUNNER_TEMP/claude-$port.txt" 2>&1 &
pid=$!
for _ in $(seq 1 120); do kill -0 $pid 2>/dev/null || break; sleep 1; done
kill $pid 2>/dev/null || true
cat "$RUNNER_TEMP/claude-$port.txt"
"$py" .github/scripts/skills-listed.py "$log" /v1/messages
