#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""A TermOS plugin in plain Python: raw JSON-RPC over the session socket.

Opens a window, sets a status widget and registers a `ping` command. Keys
pressed in the window are echoed into it; `ping` answers with a
notification. When the server refuses the window (no `windows`
permission), it says so in a notification instead.
"""

import json
import os
import socket
import sys

name = os.environ["TERMOS_PLUGIN_NAME"]
endpoint = os.environ["TERMOS_SOCKET"]
if endpoint.startswith("\\\\.\\pipe\\"):
    # Windows: the server's named pipe, opened like a file. One thread
    # reads and writes in turn, which a synchronous pipe handles fine.
    pipe = open(endpoint, "r+b", buffering=0)
    incoming = pipe

    def send(obj):
        pipe.write((json.dumps(obj) + "\n").encode())
else:
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect(endpoint)
    incoming = sock.makefile("rb")

    def send(obj):
        sock.sendall((json.dumps(obj) + "\n").encode())

queued = []
next_id = 0


def read():
    line = incoming.readline()
    if not line:
        sys.exit(0)  # the server went away
    return json.loads(line)


def call(method, params):
    global next_id
    next_id += 1
    send({"jsonrpc": "2.0", "id": next_id, "method": method, "params": params})
    while True:
        msg = read()
        if "method" in msg:
            queued.append(msg)
        elif msg.get("id") == next_id:
            return msg


hello = call("hello", {"version": 1, "kind": "plugin", "client": name,
                       "token": os.environ["TERMOS_PLUGIN_TOKEN"]})
if "error" in hello:
    sys.exit(f"hello refused: {hello['error']['message']}")

opened = call("window.open", {
    "title": f"py {name}",
    "lines": ["Hello from Python",
              [{"text": "styled", "fg": "green", "bold": True}]],
})
if "error" in opened:
    call("notify.send", {"text": f"window refused ({opened['error']['code']})"})
else:
    call("status.set", {"text": f"py:{name}"})
    call("command.register", {"name": "ping", "description": "Ping the plugin"})

pings = 0
while True:
    msg = queued.pop(0) if queued else read()
    method, params = msg.get("method"), msg.get("params") or {}
    if method == "command.invoked" and params.get("name") == "ping":
        pings += 1
        call("notify.send", {"text": f"pong #{pings} in {params.get('session')}"})
    elif method == "window.key":
        call("window.draw", {"window": params["window"],
                             "lines": [f"you pressed {params['key']}"]})
    elif method == "window.closed":
        call("notify.send", {"text": f"window {params['window']} closed"})
