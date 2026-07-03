#!/usr/bin/env bash
# Prove Phase C item 1: React SSR streams the shell before a Suspense-delayed
# subtree. This intentionally reads from a raw socket so buffering regressions
# are visible.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

APP="${BEATER_APP:-examples/hello}"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
BIN="${BEATER_BIN:-$TARGET_DIR/debug/beater}"
PORT="${BEATER_STREAM_PORT:-$(python3 - <<'PY'
import socket

with socket.socket() as sock:
    sock.bind(("127.0.0.1", 0))
    print(sock.getsockname()[1])
PY
)}"
LOG="${BEATER_STREAM_LOG:-$TARGET_DIR/streaming-ssr-gate-$PORT.log}"
GATE_RUST_LOG="${BEATER_STREAM_RUST_LOG:-beater_runtime::server=info,info}"

if [[ "${BEATER_SKIP_BUILD:-0}" != "1" ]]; then
  cargo build -p beater-cli
elif [[ ! -x "$BIN" ]]; then
  cargo build -p beater-cli
fi

mkdir -p "$(dirname "$LOG")"

env RUST_LOG="$GATE_RUST_LOG" "$BIN" dev "$APP" --host 127.0.0.1 --port "$PORT" >"$LOG" 2>&1 &
pid=$!

cleanup() {
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
}
trap cleanup EXIT

python3 - "$PORT" "$LOG" "$pid" <<'PY'
import os
import socket
import sys
import time

port = int(sys.argv[1])
log = sys.argv[2]
pid = int(sys.argv[3])
deadline = time.monotonic() + 20
listening = f"beater dev listening on http://127.0.0.1:{port}"

def require_child_alive():
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        print(f"server process {pid} exited before accepting connections; log follows", file=sys.stderr)
        try:
            print(open(log, encoding="utf-8").read(), file=sys.stderr)
        except OSError:
            pass
        sys.exit(1)

def log_contains_listening_line():
    try:
        return listening in open(log, encoding="utf-8").read()
    except OSError:
        return False

while time.monotonic() < deadline:
    require_child_alive()
    if not log_contains_listening_line():
        time.sleep(0.1)
        continue
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=0.25):
            require_child_alive()
            break
    except OSError:
        time.sleep(0.1)
else:
    print(f"server did not accept connections on {port}; log follows", file=sys.stderr)
    try:
        print(open(log, encoding="utf-8").read(), file=sys.stderr)
    except OSError:
        pass
    sys.exit(1)
PY

python3 - "$PORT" <<'PY'
import socket
import sys
import time

port = int(sys.argv[1])
shell_marker = b'data-stream-marker="shell"'
delayed_marker = b'data-stream-marker="delayed"'
started = time.monotonic()
shell_at = None
delayed_at = None
health_elapsed = None
data = b""

def raw_request(method, path, timeout=5):
    request_started = time.monotonic()
    chunks = []
    with socket.create_connection(("127.0.0.1", port), timeout=timeout) as sock:
        sock.settimeout(timeout)
        request = (
            f"{method} {path} HTTP/1.1\r\n"
            "Host: 127.0.0.1\r\n"
            "Connection: close\r\n\r\n"
        ).encode("ascii")
        sock.sendall(request)
        while True:
            chunk = sock.recv(4096)
            if not chunk:
                break
            chunks.append(chunk)
    return b"".join(chunks), time.monotonic() - request_started

def check_worker_available():
    response, elapsed = raw_request("GET", "/api/health", timeout=2)
    if b"HTTP/1.1 200 OK" not in response:
        sys.exit("expected HTTP 200 from /api/health while page stream was open")
    if b'"ok":true' not in response:
        sys.exit("expected health JSON body while page stream was open")
    if elapsed > 0.25:
        sys.exit(f"worker was blocked by page stream: /api/health took {elapsed:.3f}s")
    return elapsed

with socket.create_connection(("127.0.0.1", port), timeout=5) as sock:
    sock.settimeout(5)
    sock.sendall(
        b"GET / HTTP/1.1\r\n"
        b"Host: 127.0.0.1\r\n"
        b"Accept: text/html\r\n"
        b"Connection: close\r\n\r\n"
    )
    while True:
        chunk = sock.recv(96)
        if not chunk:
            break
        now = time.monotonic() - started
        data += chunk
        if shell_at is None and shell_marker in data:
            shell_at = now
            health_elapsed = check_worker_available()
        if delayed_at is None and delayed_marker in data:
            delayed_at = now

if b"HTTP/1.1 200 OK" not in data:
    sys.exit("expected HTTP 200 from /")
if shell_at is None:
    sys.exit("did not observe streaming shell marker")
if delayed_at is None:
    sys.exit("did not observe delayed Suspense marker")
if delayed_at <= shell_at:
    sys.exit(f"delayed marker arrived before shell marker: shell={shell_at:.3f}s delayed={delayed_at:.3f}s")
if delayed_at - shell_at < 0.25:
    sys.exit(f"markers were not observably streamed apart: shell={shell_at:.3f}s delayed={delayed_at:.3f}s")
if b"</html>" not in data:
    sys.exit("stream ended before a complete HTML document was observed")

head_response, _ = raw_request("HEAD", "/", timeout=5)
if b"HTTP/1.1 200 OK" not in head_response:
    sys.exit("expected HTTP 200 from HEAD /")
head_body = head_response.split(b"\r\n\r\n", 1)[1] if b"\r\n\r\n" in head_response else b""
if head_body:
    sys.exit("expected HEAD / to return headers without a body")
if b"content-length: 0" in head_response.lower():
    sys.exit("HEAD / must not advertise content-length: 0 for a streamed GET body")

print(
    "streaming SSR gate passed: "
    f"shell={shell_at:.3f}s delayed={delayed_at:.3f}s "
    f"health={health_elapsed:.3f}s"
)
PY
