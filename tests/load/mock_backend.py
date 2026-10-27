#!/usr/bin/env python3
"""Deterministic mock OpenRouter backend for overhead comparison.

POST /v1/chat/completions sleeps a fixed latency, then serves a canned
JSON completion — or a flushed-chunk SSE stream when the request asks for
stream:true. Requires an Authorization header (presence only). Stdlib only.

Usage:
  mock_backend.py --port PORT --latency-ms N [--bind ADDR]
    [--sse-chunks N] [--sse-gap-ms N]

Prints PORT=<bound port> on stdout, then serves until killed.
"""

import argparse
import json
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

COMPLETION = {
    "id": "gen-compare-mock",
    "object": "chat.completion",
    "created": 1761500000,
    "model": "mock",
    "choices": [
        {
            "index": 0,
            "finish_reason": "stop",
            "message": {"role": "assistant", "content": "ok"},
        }
    ],
    "usage": {"prompt_tokens": 8, "completion_tokens": 1, "total_tokens": 9},
}

UNAUTHORIZED = {"error": {"message": "Missing Authorization header.", "code": 401}}


def sse_body(chunks):
    parts = []
    for index in range(chunks):
        parts.append(
            'data: {"object":"chat.completion.chunk","choices":[{"index":0,'
            f'"delta":{{"content":"tok{index}"}}}}]}}\n\n'
        )
    parts.append("data: [DONE]\n\n")
    return "".join(parts).encode()


class Handler(BaseHTTPRequestHandler):
    server_version = "MockBackend/1"

    def do_POST(self):
        if self.path != "/v1/chat/completions":
            self.send_error(404)
            return
        if not self.headers.get("Authorization"):
            data = json.dumps(UNAUTHORIZED).encode()
            self.send_response(401)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            return
        length = int(self.headers.get("Content-Length", 0))
        try:
            req = json.loads(self.rfile.read(length) or b"{}")
        except ValueError:
            req = {}
        time.sleep(self.server.latency_seconds)
        if req.get("stream"):
            data = sse_body(self.server.sse_chunks)
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            # Fixed total length (keep-alive survives) but flushed in
            # increments so clients observe a real stream.
            step = len(data) // (self.server.sse_chunks + 1)
            for offset in range(0, len(data), step):
                self.wfile.write(data[offset : offset + step])
                self.wfile.flush()
                time.sleep(self.server.sse_gap_seconds)
        else:
            data = json.dumps(COMPLETION).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

    def log_message(self, *args):
        pass


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, default=0)
    parser.add_argument("--bind", default="127.0.0.1")
    parser.add_argument("--latency-ms", type=float, default=20)
    parser.add_argument("--sse-chunks", type=int, default=5)
    parser.add_argument("--sse-gap-ms", type=float, default=5)
    args = parser.parse_args(argv)
    server = ThreadingHTTPServer((args.bind, args.port), Handler)
    server.latency_seconds = args.latency_ms / 1000.0
    server.sse_chunks = args.sse_chunks
    server.sse_gap_seconds = args.sse_gap_ms / 1000.0
    server.daemon_threads = True
    print(f"PORT={server.server_port}", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
