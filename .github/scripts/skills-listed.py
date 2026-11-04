# SPDX-License-Identifier: GPL-3.0-or-later
"""Succeeds when a request a CLI sent to the stand-in model server (see
refusing-model.py) lists TermOS's skills (D-098): their names and the
start of the `termos` skill's description, as the CLI passes them to the
model.

Usage: skills-listed.py REQUESTS.jsonl PATH-PREFIX
"""

import json
import sys

log, prefix = sys.argv[1], sys.argv[2]
NEEDLES = ["termos-dev", "termos-agents", "Use the TermOS terminal desktop you are running in"]

try:
    with open(log, encoding="utf-8") as f:
        requests = [json.loads(line) for line in f if line.strip()]
except FileNotFoundError:
    requests = []
to_model = [r for r in requests if r["path"].startswith(prefix)]
print(f"{len(requests)} requests reached the stand-in, {len(to_model)} to {prefix}")
for r in to_model:
    if all(n in r["body"] for n in NEEDLES):
        at = r["body"].find("termos-agents")
        print("TermOS's skills are listed:", r["body"][max(0, at - 200):at + 200])
        sys.exit(0)
print("no request listed TermOS's skills; paths:", [r["path"] for r in requests])
for r in to_model[:1]:
    print(r["body"][:3000])
sys.exit(1)
