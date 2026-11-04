# SPDX-License-Identifier: GPL-3.0-or-later
"""A stand-in model server for CI, so the agent CLIs run without an
account or a key.

It refuses every request with an error, so a CLI stops at once (after
starting its MCP servers, which is one thing CI looks at), except
Anthropic's Messages API, which gets a short answer, so Claude Code
finishes its turn cleanly. With a second argument, every request is
appended to that file as a JSON line (method, path, body), so CI can
check what a CLI sent: the skills it lists, for one (D-098).

Usage: refusing-model.py PORT [REQUESTS.jsonl]
"""

import http.server
import json
import sys

LOG = sys.argv[2] if len(sys.argv) > 2 else None

ANSWER = "Hello from the stand-in."

SSE = [
    ("message_start", {"type": "message_start", "message": {
        "id": "msg_fixture", "type": "message", "role": "assistant", "model": "fixture-model",
        "content": [], "stop_reason": None, "stop_sequence": None,
        "usage": {"input_tokens": 1, "output_tokens": 1}}}),
    ("content_block_start", {"type": "content_block_start", "index": 0,
                             "content_block": {"type": "text", "text": ""}}),
    ("content_block_delta", {"type": "content_block_delta", "index": 0,
                             "delta": {"type": "text_delta", "text": ANSWER}}),
    ("content_block_stop", {"type": "content_block_stop", "index": 0}),
    ("message_delta", {"type": "message_delta",
                       "delta": {"stop_reason": "end_turn", "stop_sequence": None},
                       "usage": {"output_tokens": 5}}),
    ("message_stop", {"type": "message_stop"}),
]


class Handler(http.server.BaseHTTPRequestHandler):
    def _body(self):
        n = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(n) if n else b""
        if LOG:
            with open(LOG, "a", encoding="utf-8") as f:
                f.write(json.dumps({
                    "method": self.command,
                    "path": self.path,
                    "body": raw.decode("utf-8", "replace"),
                }) + "\n")
        try:
            return json.loads(raw) if raw else {}
        except ValueError:
            return {}

    def _json(self, status, value):
        body = json.dumps(value).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _refuse(self):
        self._json(400, {"error": {"message": "fixture model server",
                                   "type": "invalid_request_error"}})

    def do_GET(self):
        self._body()
        if self.path.split("?")[0] == "/v1/models":
            self._json(200, {"data": [], "has_more": False})
        else:
            self._refuse()

    def do_POST(self):
        req = self._body()
        path = self.path.split("?")[0]
        if path == "/v1/messages/count_tokens":
            self._json(200, {"input_tokens": 10})
        elif path == "/v1/messages" and req.get("stream"):
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            for event, data in SSE:
                self.wfile.write(f"event: {event}\ndata: {json.dumps(data)}\n\n".encode())
                self.wfile.flush()
            self.close_connection = True
        elif path == "/v1/messages":
            self._json(200, {
                "id": "msg_fixture", "type": "message", "role": "assistant",
                "model": "fixture-model", "content": [{"type": "text", "text": ANSWER}],
                "stop_reason": "end_turn", "stop_sequence": None,
                "usage": {"input_tokens": 1, "output_tokens": 5}})
        else:
            self._refuse()

    def log_message(self, *args):
        pass


http.server.ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()
