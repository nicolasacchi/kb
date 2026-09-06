#!/usr/bin/env python3
"""Tiny fixture HTTP server for kb-code-why.sh's test matrix.

Usage: mock-daemon.py <port> <repos.json> <why.json>

Serves:
  GET /api/repos -> the bytes of <repos.json>
  GET /api/why   -> the bytes of <why.json> (query params ignored — each
                     test scenario runs its own server instance with its
                     own fixture files, so there's nothing to branch on)
Anything else -> 404.

No third-party deps (stdlib http.server only) so the test matrix doesn't
need anything beyond python3 + the repo's usual jq/curl/git.
"""
import http.server
import sys


def main() -> None:
    port = int(sys.argv[1])
    repos_path = sys.argv[2]
    why_path = sys.argv[3]

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_args):  # keep test output quiet
            pass

        def do_GET(self):
            path = self.path.split("?", 1)[0]
            if path == "/api/repos":
                body = open(repos_path, "rb").read()
            elif path == "/api/why":
                body = open(why_path, "rb").read()
            else:
                self.send_response(404)
                self.end_headers()
                return
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    httpd = http.server.HTTPServer(("127.0.0.1", port), Handler)
    httpd.serve_forever()


if __name__ == "__main__":
    main()
