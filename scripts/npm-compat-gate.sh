#!/usr/bin/env bash
# Prove the Phase C npm/node-compat wedge: a route can import a real ESM
# package from node_modules with a bare specifier.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
BIN="${BEATER_BIN:-$TARGET_DIR/debug/beater}"
APP="$(mktemp -d "${TMPDIR:-/tmp}/beater-npm-gate.XXXXXX")/zod-app"
PORT="${BEATER_NPM_PORT:-$(python3 - <<'PY'
import socket

with socket.socket() as sock:
    sock.bind(("127.0.0.1", 0))
    print(sock.getsockname()[1])
PY
)}"
LOG="${BEATER_NPM_LOG:-$TARGET_DIR/npm-compat-gate.log}"

cleanup() {
  if [[ -n "${pid:-}" ]]; then
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  fi
  rm -rf "$(dirname "$APP")"
}
trap cleanup EXIT

if [[ "${BEATER_SKIP_BUILD:-0}" != "1" || ! -x "$BIN" ]]; then
  cargo build -p beater-cli
fi

"$BIN" new "$APP" >/dev/null
npm install --prefix "$APP" zod@4.4.3 >/dev/null

cat >"$APP/app/routes/api/zod.ts" <<'TS'
import { z } from "zod";

const Payload = z.object({ value: z.string().min(3) });

export function GET() {
  const parsed = Payload.parse({ value: "beater" });
  return {
    status: 200,
    headers: { "content-type": "application/json; charset=utf-8" },
    body: JSON.stringify({ ok: true, value: parsed.value }),
  };
}
TS

mkdir -p "$(dirname "$LOG")"
env \
  -u ANTHROPIC_API_KEY \
  -u BEATER_BASE_URL \
  -u BEATER_MCP_TOKEN \
  -u BEATER_MCP_TRUSTED_ORIGINS \
  "$BIN" dev "$APP" --host 127.0.0.1 --port "$PORT" >"$LOG" 2>&1 &
pid=$!

python3 - "$PORT" "$LOG" <<'PY'
import http.client
import json
import sys
import time

port = int(sys.argv[1])
log = sys.argv[2]
deadline = time.monotonic() + 20

while True:
    try:
        conn = http.client.HTTPConnection("127.0.0.1", port, timeout=0.5)
        conn.request("GET", "/api/health")
        response = conn.getresponse()
        response.read()
        conn.close()
        if response.status == 200:
            break
    except OSError:
        pass
    if time.monotonic() > deadline:
        print(f"server did not become ready on {port}; log follows", file=sys.stderr)
        try:
            print(open(log, encoding="utf-8").read(), file=sys.stderr)
        except OSError:
            pass
        sys.exit(1)
    time.sleep(0.1)

conn = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
conn.request("GET", "/api/zod")
response = conn.getresponse()
body = response.read().decode("utf-8")
conn.close()
if response.status != 200:
    sys.exit(f"expected 200 from /api/zod, got {response.status}: {body}")
payload = json.loads(body)
if payload != {"ok": True, "value": "beater"}:
    sys.exit(f"unexpected /api/zod payload: {payload!r}")
print(f"npm compat passed: zod import returned {payload['value']}")
PY
