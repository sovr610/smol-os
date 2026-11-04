# SPDX-License-Identifier: GPL-3.0-or-later
"""A stand-in model server for CI: it answers every request with an
error, so an agent CLI fails at once without an account or a key (after
starting its MCP servers, which is what the check looks at)."""

import http.server
import json
import sys


class Refuse(http.server.BaseHTTPRequestHandler):
    def _refuse(self):
        body = json.dumps(
            {"error": {"message": "fixture model server", "type": "invalid_request_error"}}
        ).encode()
        self.send_response(400)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    do_GET = _refuse
    do_POST = _refuse

    def log_message(self, *args):
        pass


http.server.HTTPServer(("127.0.0.1", int(sys.argv[1])), Refuse).serve_forever()
