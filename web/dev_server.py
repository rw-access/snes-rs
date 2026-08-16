#!/usr/bin/env python3
"""Small static server for the snes-rs WASM frontend and browser diagnostics."""
import argparse
import json
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


ROOT = Path(__file__).resolve().parent


class Handler(SimpleHTTPRequestHandler):
    def end_headers(self):
        self.send_header("Cache-Control", "no-store")
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Access-Control-Allow-Headers", "Content-Type")
        self.send_header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
        super().end_headers()

    def do_OPTIONS(self):
        self.send_response(204)
        self.end_headers()

    def do_POST(self):
        if self.path.split("?", 1)[0] != "/__debug":
            self.send_error(404)
            return
        try:
            length = int(self.headers.get("Content-Length", "0"))
            if length < 0 or length > 1_000_000:
                raise ValueError("request body is too large")
            record = json.loads(self.rfile.read(length))
            if not isinstance(record, dict):
                raise ValueError("debug report must be a JSON object")
        except (ValueError, json.JSONDecodeError) as error:
            self.send_error(400, str(error))
            return
        print(f"[browser-debug] {json.dumps(record, ensure_ascii=False, sort_keys=True)}", flush=True)
        self.send_response(204)
        self.end_headers()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=8000)
    parser.add_argument("--host", "--bind", dest="host", default="127.0.0.1")
    args = parser.parse_args()
    server = ThreadingHTTPServer((args.host, args.port), lambda *a, **kw: Handler(*a, directory=ROOT, **kw))
    print(f"Serving {ROOT} at http://{args.host}:{args.port}/", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
