#!/usr/bin/env bash
# Prove the Phase C npm/node-compat wedge: a route can import a real ESM
# package plus a leaf CommonJS default export from node_modules with bare
# specifiers, while unsupported CommonJS require() fails closed.
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

mkdir -p "$APP/node_modules/legacy-cjs"
cat >"$APP/node_modules/legacy-cjs/package.json" <<'JSON'
{"name":"legacy-cjs","main":"index.cjs"}
JSON
cat >"$APP/node_modules/legacy-cjs/index.cjs" <<'JS'
module.exports = {
  label: "legacy-cjs",
  double(value) {
    return value * 2;
  },
};
JS

mkdir -p "$APP/node_modules/require-cjs"
cat >"$APP/node_modules/require-cjs/package.json" <<'JSON'
{"name":"require-cjs","main":"index.cjs"}
JSON
cat >"$APP/node_modules/require-cjs/index.cjs" <<'JS'
module.exports = require("fs");
JS

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

cat >"$APP/app/routes/api/cjs.ts" <<'TS'
import legacy from "legacy-cjs";

export function GET() {
  return {
    status: 200,
    headers: { "content-type": "application/json; charset=utf-8" },
    body: JSON.stringify({
      label: legacy.label,
      doubled: legacy.double(21),
    }),
  };
}
TS

cat >"$APP/app/routes/api/cjs-require.ts" <<'TS'
import blocked from "require-cjs";

export function GET() {
  return {
    status: 200,
    headers: { "content-type": "text/plain; charset=utf-8" },
    body: String(blocked),
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

conn = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
conn.request("GET", "/api/cjs")
response = conn.getresponse()
body = response.read().decode("utf-8")
conn.close()
if response.status != 200:
    sys.exit(f"expected 200 from /api/cjs, got {response.status}: {body}")
cjs_payload = json.loads(body)
if cjs_payload != {"label": "legacy-cjs", "doubled": 42}:
    sys.exit(f"unexpected /api/cjs payload: {cjs_payload!r}")

conn = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
conn.request("GET", "/api/cjs-require")
response = conn.getresponse()
body = response.read().decode("utf-8")
conn.close()
if response.status < 500:
    sys.exit(f"expected /api/cjs-require to fail closed, got {response.status}: {body}")
if "CommonJS require" not in body:
    sys.exit(f"expected /api/cjs-require failure to mention CommonJS require, got: {body}")
print(
    "npm compat passed: "
    f"zod import returned {payload['value']}; "
    f"cjs doubled {cjs_payload['doubled']}; "
    "require failed closed"
)
PY
