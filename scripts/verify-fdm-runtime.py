"""Local end-to-end fixtures for the built Downloader CLI and optional engines.

Requires pyftpdlib in the invoking Python environment; never changes user settings.
Preserves fixture outputs and a JSON report beneath the supplied evidence directory.
"""
from __future__ import annotations

import argparse
import functools
import hashlib
import http.server
import json
import os
from pathlib import Path
import shutil
import socket
import struct
import subprocess
import sys
import threading
import time
import urllib.parse
import urllib.error
import urllib.request


def bencode(value):
    if isinstance(value, int):
        return b"i" + str(value).encode() + b"e"
    if isinstance(value, bytes):
        return str(len(value)).encode() + b":" + value
    if isinstance(value, str):
        return bencode(value.encode())
    if isinstance(value, list):
        return b"l" + b"".join(bencode(item) for item in value) + b"e"
    return b"d" + b"".join(bencode(key) + bencode(value[key]) for key in sorted(value)) + b"e"


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dlx", type=Path, required=True)
    parser.add_argument("--aria2", type=Path, required=True)
    parser.add_argument("--ytdlp", type=Path, required=True)
    parser.add_argument("--evidence", type=Path, required=True)
    parser.add_argument("--python-lib", type=Path)
    args = parser.parse_args()
    if args.python_lib:
        sys.path.insert(0, str(args.python_lib.resolve()))
    from pyftpdlib.authorizers import DummyAuthorizer
    from pyftpdlib.handlers import FTPHandler
    from pyftpdlib.servers import FTPServer

    evidence = args.evidence.resolve()
    evidence.mkdir(parents=True, exist_ok=True)
    fixture = evidence / "source"
    fixture.mkdir(exist_ok=True)
    payload = bytes(range(256)) * 4096
    (fixture / "payload.bin").write_bytes(payload)
    expected = hashlib.sha256(payload).hexdigest()
    peer_port = free_port()

    class Handler(http.server.SimpleHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_GET(self):
            if self.path.startswith("/announce"):
                peers = socket.inet_aton("127.0.0.1") + struct.pack("!H", peer_port)
                body = bencode({"complete": 1, "incomplete": 0, "interval": 1, "peers": peers})
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
            elif self.path.split("?", 1)[0] == "/payload.bin":
                start, end = 0, len(payload) - 1
                partial = self.headers.get("Range", "")
                if partial.startswith("bytes="):
                    first, last = partial[6:].split("-", 1)
                    start, end = int(first), int(last) if last else end
                self.send_response(206 if partial else 200)
                self.send_header("Accept-Ranges", "bytes")
                self.send_header("Content-Type", "application/octet-stream")
                if partial:
                    self.send_header("Content-Range", f"bytes {start}-{end}/{len(payload)}")
                self.send_header("Content-Length", str(end - start + 1))
                self.end_headers()
                self.wfile.write(payload[start:end + 1])
            else:
                super().do_GET()

    httpd = http.server.ThreadingHTTPServer(("127.0.0.1", 0), functools.partial(Handler, directory=str(fixture)))
    http_thread = threading.Thread(target=httpd.serve_forever, daemon=True)
    http_thread.start()
    base = f"http://127.0.0.1:{httpd.server_port}"
    authorizer = DummyAuthorizer()
    authorizer.add_anonymous(str(fixture))
    fixture_password = "fdm-local-ftp-fixture"
    authorizer.add_user("fixture", fixture_password, str(fixture), perm="elr")
    FTPHandler.authorizer = authorizer
    ftpd = FTPServer(("127.0.0.1", 0), FTPHandler)
    ftp_port = ftpd.socket.getsockname()[1]
    ftp_thread = threading.Thread(target=ftpd.serve_forever, kwargs={"timeout": 0.2}, daemon=True)
    ftp_thread.start()
    env = dict(os.environ, DOWNLOADER_ARIA2=str(args.aria2.resolve()), DOWNLOADER_YTDLP=str(args.ytdlp.resolve()))
    cases = []

    def download(name, source, expected_hash, *extra):
        output = evidence / name
        output.mkdir(exist_ok=True)
        argv = [str(args.dlx.resolve()), source, "-o", str(output), "--verify", "off", "--retries", "0", *extra]
        started = time.monotonic()
        try:
            result = subprocess.run(argv, env=env, capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=90)
            matching = [str(path.relative_to(evidence)) for path in output.rglob("*") if path.is_file() and hashlib.sha256(path.read_bytes()).hexdigest() == expected_hash]
            case = {"name": name, "status": "PASS" if result.returncode == 0 and matching else "FAIL", "exit_code": result.returncode, "matching_outputs": matching, "stdout": result.stdout[-3000:], "stderr": result.stderr[-3000:]}
        except subprocess.TimeoutExpired as error:
            case = {"name": name, "status": "FAIL", "exit_code": None, "error": f"timeout after {error.timeout}s"}
        case["elapsed_seconds"] = round(time.monotonic() - started, 3)
        cases.append(case)
        print(json.dumps(case, ensure_ascii=False), flush=True)

    seeder = None
    try:
        download("native-http", f"{base}/payload.bin", expected)
        download("ftp", f"ftp://127.0.0.1:{ftp_port}/payload.bin", expected)
        meta = fixture / "payload.meta4"
        meta.write_text(f'''<?xml version="1.0"?><metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="payload.bin"><size>{len(payload)}</size><hash type="sha-256">{expected}</hash><url priority="1">{base}/missing.bin</url><url priority="2">{base}/payload.bin</url></file></metalink>''', encoding="utf-8")
        download("metalink-mirror", str(meta), expected)
        download("metalink-http-descriptor", f"{base}/payload.meta4", expected)
        piece_size = 256 * 1024
        pieces = b"".join(hashlib.sha1(payload[offset:offset + piece_size]).digest() for offset in range(0, len(payload), piece_size))
        info = {"length": len(payload), "name": "payload.bin", "piece length": piece_size, "pieces": pieces}
        torrent = fixture / "payload.torrent"
        torrent.write_bytes(bencode({"announce": f"{base}/announce", "info": info, "url-list": [f"{base}/payload.bin"]}))
        download("torrent-webseed", str(torrent), expected)
        download("torrent-http-descriptor", f"{base}/payload.torrent", expected)
        seed_argv = [str(args.aria2.resolve()), "--dir=" + str(fixture), "--listen-port=" + str(peer_port), "--enable-dht=false", "--enable-dht6=false", "--bt-enable-lpd=false", "--seed-time=5", "--check-integrity=true", "--file-allocation=none", "--summary-interval=0", "--console-log-level=warn", str(torrent)]
        seed_log = (evidence / "seeder.log").open("w", encoding="utf-8")
        seeder = subprocess.Popen(seed_argv, stdout=seed_log, stderr=subprocess.STDOUT)
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            with socket.socket() as sock:
                if sock.connect_ex(("127.0.0.1", peer_port)) == 0:
                    break
            time.sleep(0.1)
        magnet = "magnet:?xt=urn:btih:" + hashlib.sha1(bencode(info)).hexdigest() + "&dn=payload.bin&tr=" + urllib.parse.quote(f"{base}/announce", safe="")
        download("magnet-local-peer", magnet, expected)
        control = args.dlx.resolve().with_name("dlxctl.exe" if os.name == "nt" else "dlxctl")
        if control.is_file():
            control_env = dict(env, FDM_FIXTURE_FTP_PASSWORD=fixture_password, DOWNLOADER_REMOTE_TOKEN=hashlib.sha256(b"fdm-local-api-fixture").hexdigest())
            ftp_url = f"ftp://127.0.0.1:{ftp_port}/"
            listing = subprocess.run([str(control), "ftp", ftp_url, "--username", "fixture", "--password-env", "FDM_FIXTURE_FTP_PASSWORD"], env=control_env, capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=30)
            cases.append({"name": "authenticated-ftp-list", "status": "PASS" if listing.returncode == 0 and "payload.bin" in listing.stdout else "FAIL", "exit_code": listing.returncode, "stdout": listing.stdout[-1000:], "stderr": listing.stderr[-1000:]})
            api_port = free_port()
            state = evidence / "manager-state"
            output = evidence / "authenticated-ftp"
            output.mkdir(exist_ok=True)
            with (evidence / "manager.log").open("w", encoding="utf-8") as log:
                manager = subprocess.Popen([str(control), "--data-dir", str(state), "serve", "--bind", f"127.0.0.1:{api_port}", "--site-origin", ftp_url, "--username", "fixture", "--password-env", "FDM_FIXTURE_FTP_PASSWORD"], env=control_env, stdout=log, stderr=subprocess.STDOUT)

                def api(path, body=None):
                    request = urllib.request.Request(f"http://127.0.0.1:{api_port}" + path, data=None if body is None else json.dumps(body).encode(), headers={"Authorization": "Bearer " + control_env["DOWNLOADER_REMOTE_TOKEN"], "Content-Type": "application/json"})
                    with urllib.request.urlopen(request, timeout=3) as response:
                        return json.load(response)

                try:
                    deadline = time.monotonic() + 10
                    while True:
                        try:
                            api("/downloads")
                            break
                        except (urllib.error.URLError, ConnectionError):
                            if time.monotonic() >= deadline:
                                raise
                            time.sleep(0.1)
                    unauthorized = False
                    try:
                        urllib.request.urlopen(f"http://127.0.0.1:{api_port}/downloads", timeout=3)
                    except urllib.error.HTTPError as error:
                        unauthorized = error.code == 401
                    task_id = api("/downloads", {"url": ftp_url + "payload.bin", "dir": str(output)})["id"]
                    deadline = time.monotonic() + 15
                    while True:
                        task = next(item for item in api("/downloads") if item["id"] == task_id)
                        if task["status"] in ("Completed", "Error") or time.monotonic() >= deadline:
                            break
                        time.sleep(0.1)
                    matched = any(path.is_file() and hashlib.sha256(path.read_bytes()).hexdigest() == expected for path in output.rglob("*"))
                    persisted = "".join(path.read_text(encoding="utf-8") for path in state.glob("*.json"))
                    no_password = fixture_password not in persisted
                    case = {"name": "authenticated-ftp-via-remote-manager", "status": "PASS" if matched and unauthorized and no_password and task["status"] == "Completed" else "FAIL", "download_status": task["status"], "unauthenticated_request_rejected": unauthorized, "password_excluded_from_persistence": no_password}
                    cases.append(case)
                    print(json.dumps(case), flush=True)
                finally:
                    manager.terminate()
                    manager.wait(timeout=10)
        else:
            cases.append({"name": "authenticated-ftp-manager", "status": "NOT_RUN", "reason": "dlxctl not built next to dlx"})
        ffmpeg = shutil.which("ffmpeg")
        if ffmpeg:
            video = fixture / "sample.mp4"
            generated = subprocess.run([ffmpeg, "-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "color=c=blue:s=128x96:r=10", "-t", "1", "-c:v", "mpeg4", "-y", str(video)], capture_output=True, timeout=30)
            if generated.returncode:
                cases.append({"name": "video", "status": "FAIL", "exit_code": generated.returncode, "error": generated.stderr.decode(errors="replace")[-1000:]})
            else:
                download("video", f"{base}/sample.mp4", hashlib.sha256(video.read_bytes()).hexdigest(), "--video")
        else:
            cases.append({"name": "video", "status": "NOT_RUN", "reason": "ffmpeg fixture generator unavailable"})
        cases.extend([{"name": "rtsp-live", "status": "NOT_RUN", "reason": "No live RTSP source fixture"}, {"name": "mms-live", "status": "NOT_RUN", "reason": "No live legacy MMS source fixture"}])
    finally:
        if seeder is not None:
            seeder.terminate()
            seeder.wait(timeout=10)
            seed_log.close()
        ftpd.close_all()
        httpd.shutdown()
        httpd.server_close()
        ftp_thread.join(timeout=2)
        http_thread.join(timeout=2)
        report = {"dlx": str(args.dlx.resolve()), "dlx_sha256": hashlib.sha256(args.dlx.read_bytes()).hexdigest(), "tools": {"aria2": str(args.aria2.resolve()), "yt_dlp": str(args.ytdlp.resolve()), "ffmpeg": shutil.which("ffmpeg")}, "cases": cases}
        (evidence / "runtime.json").write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
    return 1 if any(case["status"] == "FAIL" for case in cases) else 0


if __name__ == "__main__":
    raise SystemExit(main())
