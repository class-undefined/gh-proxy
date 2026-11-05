#!/usr/bin/env python3
"""Opt-in real GitHub read-only smoke test. No pushes or Release mutations."""
import argparse
import json
import os
from pathlib import Path
import socket
import ssl
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[1]


def port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--proxy-env", action="store_true", help="use proxy environment instead of direct egress")
    parser.add_argument("--github-auth", action="store_true", help="read gh CLI token in memory for the API query")
    parser.add_argument("--ssh-auth", action="store_true", help="verify existing SSH identity using ssh and gh-proxy connect")
    args = parser.parse_args()
    binary = ROOT / "target/debug/gh-proxy"
    if not binary.exists():
        raise SystemExit("Run cargo build first")
    # Direct mode is independent of ambient proxies; --proxy-env explicitly opts in.
    env = {k: v for k, v in os.environ.items()
           if k.upper() not in {"HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY"}}
    if args.proxy_env:
        env = os.environ.copy()
    http, socks = port(), port()
    while socks == http:
        socks = port()
    config = (ROOT / "tests/fixtures/gateway.toml").read_text().replace(
        "127.0.0.1:8080", f"127.0.0.1:{http}").replace(
        "127.0.0.1:1080", f"127.0.0.1:{socks}").replace("direct = false", "direct = false" if args.proxy_env else "direct = true")
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    base = f"https://127.0.0.1:{http}"
    config = config.replace(f"http://127.0.0.1:{http}", base)
    with tempfile.TemporaryDirectory(prefix="gh-proxy-smoke-") as directory:
        directory = Path(directory)
        key, cert = directory / "key.pem", directory / "cert.pem"
        subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
                        "-keyout", str(key), "-out", str(cert), "-days", "1",
                        "-subj", "/CN=localhost", "-addext", "subjectAltName=IP:127.0.0.1"],
                       check=True, capture_output=True, timeout=15)
        config += f'\n[tls]\ncert = "{cert}"\nkey = "{key}"\n'
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}),
                    urllib.request.HTTPSHandler(context=ssl.create_default_context(cafile=str(cert))))
        cfg = directory / "config.toml"
        cfg.write_text(config)
        log = directory / "gateway.log"
        with log.open("w+") as output:
            process = subprocess.Popen([str(binary), "--config", str(cfg)], env=env,
                                       stdout=output, stderr=subprocess.STDOUT)
            try:
                for _ in range(100):
                    if process.poll() is not None:
                        raise RuntimeError("gateway exited during startup")
                    try:
                        with opener.open(base + "/health/ready", timeout=1) as response:
                            assert response.status == 200
                        break
                    except (OSError, urllib.error.URLError):
                        time.sleep(0.1)
                else:
                    raise RuntimeError("gateway not ready")
                try:
                    opener.open(base + "/other/repo.git/info/refs?service=git-upload-pack", timeout=3)
                except urllib.error.HTTPError as error:
                    assert error.code == 403
                else:
                    raise AssertionError("unlisted repository allowed")
                api_request = urllib.request.Request(base + "/api/github/repos/class-undefined/gh-proxy/releases")
                if args.github_auth:
                    token = subprocess.run(["gh", "auth", "token"], check=True, capture_output=True, text=True).stdout.strip()
                    api_request.add_header("Authorization", f"Bearer {token}")
                with opener.open(api_request, timeout=30) as response:
                    assert response.status == 200
                    assert isinstance(json.load(response), list)
                subprocess.run(["git", "-c", "http.proxy=", "-c", f"http.sslCAInfo={cert}", "clone", "--depth=1", "--no-checkout",
                                base + "/class-undefined/gh-proxy.git", str(directory / "clone")],
                               env=env, check=True, timeout=60, capture_output=True)
                subprocess.run(["git", "-c", "http.proxy=", "-c", f"http.sslCAInfo={cert}", "-C", str(directory / "clone"), "fetch"],
                               env=env, check=True, timeout=60, capture_output=True)
                # Confirm a real remote SSH greeting traverses SOCKS without terminating SSH.
                with socket.create_connection(("127.0.0.1", socks), timeout=20) as sock:
                    sock.sendall(bytes([5, 1, 0]))
                    assert sock.recv(2) == bytes([5, 0])
                    host = b"github.com"
                    sock.sendall(bytes([5, 1, 0, 3, len(host)]) + host + (22).to_bytes(2, "big"))
                    reply = b""
                    while len(reply) < 10:
                        chunk = sock.recv(10-len(reply))
                        if not chunk:
                            raise RuntimeError("SSH proxy closed")
                        reply += chunk
                    assert reply[1] == 0
                    assert sock.recv(256).startswith(b"SSH-2.0-")
                if args.ssh_auth:
                    result = subprocess.run(["ssh", "-T", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes",
                        "-o", "ConnectTimeout=15", "-o", f'ProxyCommand="{binary}" connect --socks 127.0.0.1:{socks} %h %p',
                        "git@github.com"], env=env, capture_output=True, text=True, timeout=30)
                    assert result.returncode == 1 and "successfully authenticated" in result.stderr, "SSH identity verification failed"
                    print("PASS: existing SSH identity authenticated by GitHub")
                print("PASS: readiness, repository denial, GitHub API query, clone, fetch, SSH banner")
            finally:
                process.terminate()
                process.wait(timeout=15)
            assert process.returncode == 0, "gateway did not stop cleanly"
            assert "authorization" not in log.read_text().lower()
            if args.github_auth:
                assert token not in log.read_text(), "credential leaked into gateway log"
            print("PASS: SIGTERM shutdown")


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        # Child commands carry no token arguments; never print captured gh auth output.
        if error.cmd[0] == "git":
            print(error.stderr.decode(errors="replace"))
        raise
