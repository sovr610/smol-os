#!/usr/bin/env python3
"""A stand-in for llama-server and other OpenAI-compatible servers.

It takes llama-server's arguments (--port, --api-key or LLAMA_API_KEY,
--alias, anything else is kept), checks the key, and answers /health, /v1/models,
/v1/chat/completions (streamed or not) and /v1/embeddings, saying which
model it serves and the arguments it was started with.
"""
import json
import os
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

args = sys.argv[1:]


def opt(name, default=None):
    return args[args.index(name) + 1] if name in args else default


PORT = int(opt("--port"))
KEY = opt("--api-key", os.environ.get("LLAMA_API_KEY", ""))
ALIAS = opt("--alias", "fake")


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def send(self, code, body, ctype="application/json"):
        data = body.encode()
        self.send_response(code)
        self.send_header("content-type", ctype)
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def authorized(self):
        return not KEY or self.headers.get("authorization") == "Bearer " + KEY

    def do_GET(self):
        if self.path == "/health":
            return self.send(200, '{"status":"ok"}')
        if not self.authorized():
            return self.send(401, '{"error":"bad key"}')
        if self.path == "/v1/models":
            return self.send(200, json.dumps({"data": [{"id": ALIAS}]}))
        self.send(404, "{}")

    def do_POST(self):
        if not self.authorized():
            return self.send(401, '{"error":"bad key"}')
        body = json.loads(self.rfile.read(int(self.headers.get("content-length", 0))))
        said = "served %s as %s with %s" % (body.get("model"), ALIAS, " ".join(args))
        if self.path == "/v1/embeddings":
            return self.send(200, json.dumps({"data": [{"embedding": [0.5, 0.5]}], "model": ALIAS}))
        if body.get("stream"):
            chunks = [
                {"choices": [{"index": 0, "delta": {"content": said}, "finish_reason": None}]},
                {"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
            ]
            data = "".join("data: %s\n\n" % json.dumps(c) for c in chunks) + "data: [DONE]\n\n"
            return self.send(200, data, "text/event-stream")
        self.send(200, json.dumps({"choices": [{"index": 0, "message": {"role": "assistant", "content": said}, "finish_reason": "stop"}]}))


ThreadingHTTPServer(("127.0.0.1", PORT), H).serve_forever()
