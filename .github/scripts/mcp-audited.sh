#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Succeeds when TermOS's audit logs record an MCP connection from the
# client named (claude, codex): `termos mcp serve` writes one log per
# connection (mcp-*.jsonl), whose first entry names the client.
set -euo pipefail
client="$1"
logs=$(find "$HOME" -maxdepth 7 -name 'mcp-*.jsonl' -path '*termos*' 2>/dev/null || true)
for log in $logs; do
  if grep -q "\"provider\":\"[^\"]*$client" "$log"; then
    echo "$client connected to TermOS's MCP server:"
    head -n 1 "$log"
    exit 0
  fi
done
echo "no MCP connection from $client in TermOS's audit logs; there are:"
echo "$logs"
exit 1
