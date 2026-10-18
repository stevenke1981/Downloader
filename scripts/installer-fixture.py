"""A loopback-only, synthetic download fixture for CI installer checks."""
import argparse
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument("--directory", required=True)
parser.add_argument("--ready-file", required=True)
args = parser.parse_args()
handler = partial(SimpleHTTPRequestHandler, directory=args.directory)
with ThreadingHTTPServer(("127.0.0.1", 0), handler) as server:
    ready = Path(args.ready_file)
    temporary = ready.with_suffix(".tmp")
    temporary.write_text(str(server.server_port), encoding="ascii")
    temporary.replace(ready)
    server.serve_forever()
