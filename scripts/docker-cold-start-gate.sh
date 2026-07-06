#!/usr/bin/env bash
# Build a Linux beater bundle, build its Docker image, and prove the container
# serves the generated app and MCP endpoint from a cold start.

set -euo pipefail

ROOT=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
RUST_IMAGE=${BEATER_DOCKER_RUST_IMAGE:-rust:1-bookworm}
if [ -n "${BEATER_DOCKER_IMAGE:-}" ]; then
  IMAGE=$BEATER_DOCKER_IMAGE
  CLEAN_IMAGE=0
else
  IMAGE="beater-hello:docker-gate-$(date -u +%Y%m%dT%H%M%SZ)-$$"
  CLEAN_IMAGE=1
fi
COLD_START_MS=${BEATER_DOCKER_COLD_START_MS:-1000}
MIN_FREE_KIB=${BEATER_DOCKER_MIN_FREE_KIB:-12582912}
WORKDIR=${BEATER_DOCKER_GATE_WORKDIR:-}
MCP_TOKEN=${BEATER_DOCKER_MCP_TOKEN:-docker-gate-token}
CID=""

fail() {
  echo "error: $*" >&2
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || fail "missing required command: $1"
}

now_ms() {
  python3 - <<'PY'
import time
print(int(time.monotonic() * 1000))
PY
}

free_kib() {
  df -Pk "$1" | awk 'NR == 2 { print $4 }'
}

free_port() {
  python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
}

cleanup() {
  status=$?
  if [ -n "$CID" ]; then
    docker rm -f "$CID" >/dev/null 2>&1 || true
  fi
  if [ "$status" -eq 0 ] && [ "${BEATER_DOCKER_KEEP:-0}" != "1" ]; then
    rm -rf "$WORKDIR/target"
    if [ "$CLEAN_IMAGE" = "1" ]; then
      docker image rm "$IMAGE" >/dev/null 2>&1 || true
    fi
  fi
  if [ "$status" -ne 0 ]; then
    echo "kept gate workdir: $WORKDIR" >&2
  fi
}

verify_mcp_auth() {
  port=$1
  python3 - "$port" "$MCP_TOKEN" >"$WORKDIR/logs/mcp-auth.json" 2>"$WORKDIR/logs/mcp-auth.err" <<'PY'
import sys
from urllib.error import HTTPError
from urllib.request import Request, urlopen

port, token = sys.argv[1], sys.argv[2]
body = b'{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
headers = {
    "accept": "application/json, text/event-stream",
    "content-type": "application/json",
}
url = f"http://127.0.0.1:{port}/mcp"

try:
    urlopen(Request(url, data=body, headers=headers, method="POST"), timeout=2)
except HTTPError as exc:
    if exc.code != 401:
        raise SystemExit(f"expected unauthenticated /mcp to return 401, got {exc.code}")
else:
    raise SystemExit("expected unauthenticated /mcp to return 401, got success")

headers["authorization"] = f"Bearer {token}"
with urlopen(Request(url, data=body, headers=headers, method="POST"), timeout=2) as response:
    payload = response.read().decode("utf-8")
    if response.status != 200 or '"tools"' not in payload:
        raise SystemExit(f"authenticated /mcp response did not include tools: {response.status} {payload}")
    print(payload)
PY
}

need docker
need python3

if ! docker version >/dev/null 2>&1; then
  fail "docker daemon is not available"
fi

if [ -z "$WORKDIR" ]; then
  WORKDIR=$(mktemp -d "${TMPDIR:-/tmp}/beater-docker-gate.XXXXXX")
else
  case "$WORKDIR" in
    "" | "/")
      fail "refusing unsafe gate workdir: $WORKDIR"
      ;;
  esac
  mkdir -p "$WORKDIR"
fi
trap cleanup EXIT

available=$(free_kib "$WORKDIR")
if [ "$available" -lt "$MIN_FREE_KIB" ]; then
  echo "not enough free space for Linux release build: ${available} KiB available, ${MIN_FREE_KIB} KiB required" >&2
  echo "set BEATER_DOCKER_GATE_WORKDIR to a filesystem with more room, or lower BEATER_DOCKER_MIN_FREE_KIB after validating locally" >&2
  exit 1
fi

mkdir -p "$WORKDIR/target" "$WORKDIR/out" "$WORKDIR/logs"

if ! docker image inspect "$RUST_IMAGE" >/dev/null 2>"$WORKDIR/logs/rust-image-inspect.err"; then
  if grep -qi "No such image" "$WORKDIR/logs/rust-image-inspect.err"; then
    docker pull "$RUST_IMAGE" 2>&1 | tee "$WORKDIR/logs/rust-image-pull.log"
  else
    echo "docker cannot inspect builder image $RUST_IMAGE" >&2
    cat "$WORKDIR/logs/rust-image-inspect.err" >&2
    echo "logs: $WORKDIR/logs" >&2
    exit 1
  fi
fi

echo "building Linux beater bundle in $RUST_IMAGE"
docker run --rm \
  --mount "type=bind,src=$ROOT,dst=/src,readonly" \
  --mount "type=bind,src=$WORKDIR/target,dst=/target" \
  --mount "type=bind,src=$WORKDIR/out,dst=/out" \
  -w /src \
  "$RUST_IMAGE" \
  bash -lc 'set -euo pipefail
    export PATH=/usr/local/cargo/bin:$PATH
    apt-get update
    apt-get install -y --no-install-recommends ca-certificates file pkg-config python3-dev
    PYO3_PYTHON=/usr/bin/python3 CARGO_TARGET_DIR=/target cargo build --locked --release -p beater-cli
    rm -rf /out/bundle
    /target/release/beater build /src/examples/hello --out /out/bundle --force
    file /out/bundle/bin/beater
  ' 2>&1 | tee "$WORKDIR/logs/linux-bundle-build.log"

echo "building runtime image $IMAGE"
docker build -t "$IMAGE" "$WORKDIR/out/bundle" 2>&1 | tee "$WORKDIR/logs/docker-build.log"

PORT=$(free_port)
start=$(now_ms)
CID=$(docker run -d \
  -e "BEATER_MCP_TOKEN=$MCP_TOKEN" \
  -e "BEATER_BASE_URL=http://127.0.0.1:$PORT" \
  -p "127.0.0.1:$PORT:3000" \
  "$IMAGE")

deadline=$((start + 10000))
while :; do
  if python3 - "$PORT" >"$WORKDIR/logs/health.json" 2>"$WORKDIR/logs/health.err" <<'PY'
import sys
from urllib.request import urlopen

port = sys.argv[1]
with urlopen(f"http://127.0.0.1:{port}/api/health", timeout=0.5) as response:
    body = response.read().decode("utf-8")
    if response.status != 200 or '"runtime":"beater.js"' not in body:
        raise SystemExit(f"unexpected health response: {response.status} {body}")
    print(body)
PY
  then
    end=$(now_ms)
    elapsed=$((end - start))
    break
  fi
  if [ "$(now_ms)" -ge "$deadline" ]; then
    docker logs "$CID" >"$WORKDIR/logs/container.log" 2>&1 || true
    echo "container did not serve /api/health within 10s" >&2
    echo "last health probe error:" >&2
    cat "$WORKDIR/logs/health.err" >&2 || true
    echo "container logs:" >&2
    cat "$WORKDIR/logs/container.log" >&2 || true
    echo "logs: $WORKDIR/logs" >&2
    exit 1
  fi
  sleep 0.05
done

docker logs "$CID" >"$WORKDIR/logs/container.log" 2>&1 || true

if [ "$elapsed" -gt "$COLD_START_MS" ]; then
  echo "docker cold start exceeded ${COLD_START_MS}ms: ${elapsed}ms" >&2
  echo "container logs:" >&2
  cat "$WORKDIR/logs/container.log" >&2 || true
  echo "logs: $WORKDIR/logs" >&2
  exit 1
fi

verify_mcp_auth "$PORT"

cat >"$WORKDIR/evidence.md" <<EOF2
# Docker Cold-Start Gate

- image: \`$IMAGE\`
- rust builder: \`$RUST_IMAGE\`
- health route: \`/api/health\`
- MCP route: \`/mcp\`
- MCP auth: unauthenticated 401 plus bearer-token tools/list success
- cold start: \`${elapsed}ms\`
- limit: \`${COLD_START_MS}ms\`
- logs: \`$WORKDIR/logs\`
EOF2

echo "docker cold-start gate passed in ${elapsed}ms"
echo "evidence: $WORKDIR/evidence.md"
