#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Local models end to end (M7): pull a tiny GGUF chat model and a small
# embedding model from the Hugging Face Hub, install llama.cpp for this
# machine, serve both through the router, let agents' `local` provider
# find (and start) it, then stop everything. Runs in an isolated home.
set -euo pipefail
T="$(realpath "${1:?usage: models-e2e.sh <termos binary>}")"

base="$(mktemp -d)"
export XDG_CONFIG_HOME="$base/config" XDG_STATE_HOME="$base/state" XDG_DATA_HOME="$base/data"
export XDG_RUNTIME_DIR="$base/run" HF_HUB_CACHE="$base/hub"
unset HF_HOME HF_TOKEN TERMOS TERMOS_SOCKET TERMOS_LLAMA_SERVER
mkdir -p "$XDG_RUNTIME_DIR" && chmod 700 "$XDG_RUNTIME_DIR"
cfg="$base/config.toml"
cat > "$cfg" <<'TOML'
[models]
context = 256

[agents.providers.local]
kind = "local"
TOML
termos() { "$T" --config "$cfg" "$@"; }
json() { python3 -c "import json,sys; d=json.load(sys.stdin); print($1)"; }

finish() {
  local status=$?
  if [ "$status" -ne 0 ]; then
    echo "models e2e: FAILED (exit $status)" >&2
    find "$base" -name '*.log*' -type f | while read -r log; do
      echo "--- $log" >&2
      tail -n 80 "$log" >&2
    done
  fi
  termos models stop > /dev/null 2>&1 || true
  exit "$status"
}
trap finish EXIT

step() { printf '\n== %s\n' "$*"; }

step "this machine"
termos model recommend

step "search"
termos model search "all-MiniLM-L6-v2" --all --limit 3

CHAT=ggml-org/models:tinyllamas/stories260K.gguf
EMBED=sentence-transformers/all-MiniLM-L6-v2

step "info (nothing downloaded)"
termos model info "$CHAT"

step "pull"
termos model pull "$CHAT" --yes
termos model pull "$EMBED" --yes
# Again: everything is already there and verified.
termos model pull "$EMBED" --yes
termos model list
chat_id="$(termos model list | awk '/stories260K/ {print $1; exit}')"
test -n "$chat_id"

step "llama.cpp for this machine"
termos models setup

step "agents' local provider starts the router and lists the models"
termos agent models local | tee "$base/listed"
grep -qF "$chat_id" "$base/listed"
grep -qF "$EMBED" "$base/listed"
termos models status

endpoint="$XDG_RUNTIME_DIR/termos/models.json"
[ -f "$endpoint" ] || endpoint="$(find "$XDG_RUNTIME_DIR" -name models.json | head -n 1)"
test "$(stat -c %a "$endpoint")" = 600
url="$(json "d['url']" < "$endpoint")"
token="$(json "d['token']" < "$endpoint")"
post() {
  curl -sS --fail-with-body "$url/$1" -H "authorization: Bearer $token" \
    -H 'content-type: application/json' -d "$2"
}

step "without the token"
test "$(curl -s -o /dev/null -w '%{http_code}' "$url/models")" = 401

step "chat (llama-server, started on demand)"
post chat/completions "{\"model\": \"$chat_id\", \"max_tokens\": 24,
  \"messages\": [{\"role\": \"user\", \"content\": \"Once upon a time\"}]}" > "$base/chat.json"
json "d['choices'][0]['message']['content']" < "$base/chat.json"
test "$(json "d['usage']['completion_tokens']" < "$base/chat.json")" -gt 0

step "streamed"
post chat/completions "{\"model\": \"$chat_id\", \"max_tokens\": 8, \"stream\": true,
  \"messages\": [{\"role\": \"user\", \"content\": \"Hello\"}]}" > "$base/stream.txt"
grep -q '^data: ' "$base/stream.txt"
grep -q '^data: \[DONE\]' "$base/stream.txt"
# The key llama-server checks is not on its command line.
if pgrep -af llama-server | grep -q -- --api-key; then
  echo "llama-server's key is on its command line" >&2
  exit 1
fi

step "embeddings (candle, in-process)"
post embeddings "{\"model\": \"$EMBED\",
  \"input\": [\"A cat sits on the mat.\", \"A kitten is sitting on a rug.\", \"Stock prices fell sharply today.\"]}" \
  > "$base/embed.json"
python3 - "$base/embed.json" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
v = [e["embedding"] for e in d["data"]]
assert len(v) == 3 and all(len(x) == 384 for x in v), [len(x) for x in v]
cos = lambda a, b: sum(x * y for x, y in zip(a, b))
near, far = cos(v[0], v[1]), cos(v[0], v[2])
print(f"cat~kitten {near:.3f}, cat~stocks {far:.3f}")
assert near > far + 0.2, (near, far)
PY

step "status, stop"
termos models status | tee "$base/status"
grep -F "$chat_id" "$base/status" | grep -q loaded
termos models stop
if pgrep -f llama-server > /dev/null; then
  echo "llama-server still runs after stop" >&2
  exit 1
fi
test ! -e "$endpoint"

step "remove, gc"
termos model rm "$EMBED"
termos model gc
termos model list
echo
echo "models e2e: OK"
