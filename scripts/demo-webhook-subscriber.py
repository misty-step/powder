#!/usr/bin/env python3
"""Tiny Powder webhook receiver for local delivery proof.

It validates the X-Signature-256 header as:

    sha256=<hex hmac-sha256(secret, raw_body)>

and exits after the first accepted event, printing the JSON body to stdout.
"""

from __future__ import annotations

import argparse
import hmac
import hashlib
import http.server
import json
import queue
import socketserver
import sys
import threading
import time
from typing import Any


class _Server(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True
    allow_reuse_address = True

    def __init__(self, address: tuple[str, int], secret: str, events: "queue.Queue[dict[str, Any]]"):
        super().__init__(address, _Handler)
        self.secret = secret
        self.events = events


class _Handler(http.server.BaseHTTPRequestHandler):
    server: _Server

    def do_POST(self) -> None:
        length = int(self.headers.get("content-length", "0"))
        body = self.rfile.read(length)
        presented = self.headers.get("x-signature-256", "")
        expected = "sha256=" + hmac.new(
            self.server.secret.encode("utf-8"),
            body,
            hashlib.sha256,
        ).hexdigest()
        if not hmac.compare_digest(presented, expected):
            self.send_response(401)
            self.end_headers()
            self.wfile.write(b"bad signature\n")
            return

        try:
            event = json.loads(body)
        except json.JSONDecodeError:
            self.send_response(400)
            self.end_headers()
            self.wfile.write(b"invalid json\n")
            return

        self.server.events.put(event)
        self.send_response(202)
        self.end_headers()
        self.wfile.write(b"accepted\n")

    def log_message(self, fmt: str, *args: Any) -> None:
        return


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--secret", required=True)
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=0)
    parser.add_argument("--timeout", type=float, default=5.0)
    args = parser.parse_args()

    events: "queue.Queue[dict[str, Any]]" = queue.Queue(maxsize=1)
    server = _Server((args.host, args.port), args.secret, events)
    host, port = server.server_address

    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    time.sleep(0.05)
    print(f"listening http://{host}:{port}/webhook", flush=True)
    try:
        event = events.get(timeout=args.timeout)
    except queue.Empty:
        print("timed out waiting for a signed Powder event", file=sys.stderr)
        return 2
    finally:
        server.shutdown()
        server.server_close()

    print(json.dumps(event, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
