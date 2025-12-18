#!/usr/bin/env python3
import http.server
import json
import sys
import os

CONFIG_PATH = os.environ.get("CONFIG_PATH", "/tmp/mqtt-jwt-test/config.json")

class ConfigHandler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/config.json" or self.path.startswith("/config.json?"):
            try:
                self.send_response(200)
                self.send_header("Content-type", "application/json")
                self.send_header("Cache-Control", "no-cache, no-store, must-revalidate")
                self.end_headers()
                with open(CONFIG_PATH, "rb") as f:
                    self.wfile.write(f.read())
            except FileNotFoundError:
                self.send_response(404)
                self.end_headers()
        else:
            super().do_GET()

    def log_message(self, format, *args):
        pass

if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8000
    server = http.server.HTTPServer(("", port), ConfigHandler)
    print(f"Serving on port {port}")
    server.serve_forever()
