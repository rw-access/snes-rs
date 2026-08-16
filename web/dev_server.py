#!/usr/bin/env python3
"""Serve the snes-rs WASM frontend and collect browser diagnostics/recordings."""
import argparse
import json
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from datetime import datetime, timezone


ROOT = Path(__file__).resolve().parent
DEFAULT_RECORD_DIR = Path("/tmp/snes-rs-captures")


class Handler(SimpleHTTPRequestHandler):
    def __init__(self, *args, record_dir=DEFAULT_RECORD_DIR, **kwargs):
        self.record_dir = Path(record_dir)
        super().__init__(*args, directory=str(ROOT), **kwargs)

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
        endpoint = self.path.split("?", 1)[0]
        if endpoint == "/__debug":
            self._handle_debug()
            return
        if endpoint == "/__record":
            self._handle_record()
            return
        self.send_error(404)

    def _read_json(self, limit):
        try:
            length = int(self.headers.get("Content-Length", "0"))
        except ValueError as error:
            raise ValueError("invalid Content-Length") from error
        if length < 0 or length > limit:
            raise ValueError("request body is too large")
        return json.loads(self.rfile.read(length))

    def _handle_debug(self):
        try:
            record = self._read_json(1_000_000)
            if not isinstance(record, dict):
                raise ValueError("debug report must be a JSON object")
        except (ValueError, json.JSONDecodeError) as error:
            self.send_error(400, str(error))
            return
        print(f"[browser-debug] {json.dumps(record, ensure_ascii=False, sort_keys=True)}", flush=True)
        self.send_response(204)
        self.end_headers()

    def _handle_record(self):
        print(f"[browser-record] request from {self.client_address[0]} length={self.headers.get('Content-Length', '0')}", flush=True)
        try:
            record = self._read_json(10_000_000)
            if not isinstance(record, dict) or not isinstance(record.get("frames"), list):
                raise ValueError("recording must contain a frames list")
        except (ValueError, json.JSONDecodeError) as error:
            body = json.dumps({"ok": False, "error": str(error)}).encode()
            self.send_response(400)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return

        try:
            self.record_dir.mkdir(parents=True, exist_ok=True)
            stamp = datetime.now(timezone.utc).strftime("%Y%m%d-%H%M%S-%f")
            filename = f"browser-capture-{stamp}.json"
            path = self.record_dir / filename
            path.write_text(json.dumps(record, ensure_ascii=False, indent=2) + "\n")
            frames = len(record["frames"])
            print(f"[browser-record] saved {path} ({frames} frames)", flush=True)
            body = json.dumps({"ok": True, "filename": filename, "frames": frames}).encode()
            self.send_response(201)
        except Exception as error:
            print(f"[browser-record] failed: {error!r}", flush=True)
            body = json.dumps({"ok": False, "error": str(error)}).encode()
            self.send_response(500)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=8000)
    parser.add_argument("--host", "--bind", dest="host", default="127.0.0.1")
    parser.add_argument("--record-dir", type=Path, default=DEFAULT_RECORD_DIR)
    args = parser.parse_args()
    server = ThreadingHTTPServer((args.host, args.port), lambda *a, **kw: Handler(*a, record_dir=args.record_dir, **kw))
    print(f"Serving {ROOT} at http://{args.host}:{args.port}/", flush=True)
    print(f"Recordings will be saved to {args.record_dir}", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
