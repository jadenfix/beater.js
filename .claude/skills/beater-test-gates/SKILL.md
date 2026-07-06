---
name: beater-test-gates
description: Select and run beater.js verification gates on this local macOS workspace, including docs-only checks, focused Cargo tests, browser/provider gates, deploy gates, and the PyO3 configuration needed for embedded Python tests.
---

# beater.js test gates

Use this skill when a change needs verification in `jadenfix/beater.js`, especially if `cargo test` touches `pyo3` or `beater-py`, when checking the local e2e gates listed in `final.md`, or when a PR needs an explicit evidence plan before claiming done.

## Gate selection

Pick the smallest gate that proves the risk, then escalate only when the risk crosses a boundary:

- Docs/contracts only: check the affected public docs and use `docs-contracts`; do not claim runtime behavior without runtime evidence.
- Rust library or CLI logic: run the focused package/test first, then full workspace gate if the touched path is broad.
- Python, PyO3, or embedded tool behavior: use the PyO3 config shown below so tests link the intended framework.
- V8 route/SSR/RSC/client behavior: use the focused Cargo test plus the relevant script gate in `scripts/`.
- Browser provider behavior: run the browser provider gate because helper-only tests do not prove the real Playwright path.
- Deploy/build behavior: run the Docker cold-start gate or wait for CI, because `beater build` claims must prove the generated bundle/image path.
- Security/network/control-plane behavior: prove the production call path with auth, origin/host, timeout, retry, and denial cases.

Record ordinary PR evidence in the PR body after the command has passed. Update `final.md` only when the evidence changes a durable completion claim. If a gate is intentionally not run, name the missing external dependency or cost.

## Full workspace gate

Run formatting, whitespace checks, and the workspace tests with the local CommandLineTools Python framework explicitly configured:

```sh
tmp_config=$(mktemp /tmp/beater-pyo3-config.XXXXXX)
printf '%s\n' \
  'implementation=CPython' \
  'version=3.9' \
  'shared=true' \
  'abi3=false' \
  'lib_name=python3.9' \
  'lib_dir=/Library/Developer/CommandLineTools/Library/Frameworks/Python3.framework/Versions/3.9/lib' \
  'executable=/Library/Developer/CommandLineTools/Library/Frameworks/Python3.framework/Versions/3.9/bin/python3.9' > "$tmp_config"
cargo fmt --check
git diff --check
PYO3_CONFIG_FILE="$tmp_config" \
  DYLD_FRAMEWORK_PATH=/Library/Developer/CommandLineTools/Library/Frameworks \
  cargo test
rm -f "$tmp_config"
```

## Focused gate

For a targeted test, reuse the same `tmp_config` setup and replace the final command with the focused package/test filter, for example:

```sh
PYO3_CONFIG_FILE="$tmp_config" \
  DYLD_FRAMEWORK_PATH=/Library/Developer/CommandLineTools/Library/Frameworks \
  cargo test -p beater-cli dev_server_serves_routes_ssr_and_mcp_without_api_key -- --nocapture
```

For browser lifecycle work, run the focused agent cleanup filters before the full gate:

```sh
PYO3_CONFIG_FILE="$tmp_config" \
  DYLD_FRAMEWORK_PATH=/Library/Developer/CommandLineTools/Library/Frameworks \
  cargo test -p beater-agent browser_session -- --nocapture
PYO3_CONFIG_FILE="$tmp_config" \
  DYLD_FRAMEWORK_PATH=/Library/Developer/CommandLineTools/Library/Frameworks \
  cargo test -p beater-agent resume_cleans_stale_browser_session_before_review -- --nocapture
```

## Browser provider gate

`scripts/playwright-browser-gate.cjs` is the live provider proof for `browserTool(..., {provider: "playwright"})`. It installs the upstream Playwright runner dependencies in a temp directory, starts a local authenticated browser fixture and Anthropic-compatible SSE mock, runs `beater agent run`, and verifies three completed Chromium tool results reused one run-scoped session without leaking the password in SQLite.

Build the local binary with the same PyO3 settings first, then run the gate:

```sh
tmp_config=$(mktemp /tmp/beater-pyo3-config.XXXXXX)
printf '%s\n' \
  'implementation=CPython' \
  'version=3.9' \
  'shared=true' \
  'abi3=false' \
  'lib_name=python3.9' \
  'lib_dir=/Library/Developer/CommandLineTools/Library/Frameworks/Python3.framework/Versions/3.9/lib' \
  'executable=/Library/Developer/CommandLineTools/Library/Frameworks/Python3.framework/Versions/3.9/bin/python3.9' > "$tmp_config"
PYO3_CONFIG_FILE="$tmp_config" \
  DYLD_FRAMEWORK_PATH=/Library/Developer/CommandLineTools/Library/Frameworks \
  cargo build --bin beater
rm -f "$tmp_config"
scripts/playwright-browser-gate.cjs
```

If the gate fails and you need to inspect its temp app/journal, rerun with `BEATER_KEEP_GATE_WORKDIR=1`.

## OTLP trace gate

`scripts/otlp-trace-gate.cjs` is the local OTLP proof. It starts a mock Anthropic SSE server and mock OTLP collector, runs the real `beater agent run` CLI against a temp app with the built-in `get_time` tool, then verifies one `/v1/traces` request contains `agent.run`, two `llm.call`, and one `tool.call` span with auth/header propagation.

Build the local binary with the same PyO3 settings first, then run the gate:

```sh
tmp_config=$(mktemp /tmp/beater-pyo3-config.XXXXXX)
printf '%s\n' \
  'implementation=CPython' \
  'version=3.9' \
  'shared=true' \
  'abi3=false' \
  'lib_name=python3.9' \
  'lib_dir=/Library/Developer/CommandLineTools/Library/Frameworks/Python3.framework/Versions/3.9/lib' \
  'executable=/Library/Developer/CommandLineTools/Library/Frameworks/Python3.framework/Versions/3.9/bin/python3.9' > "$tmp_config"
PYO3_CONFIG_FILE="$tmp_config" \
  DYLD_FRAMEWORK_PATH=/Library/Developer/CommandLineTools/Library/Frameworks \
  cargo build --bin beater
rm -f "$tmp_config"
scripts/otlp-trace-gate.cjs
```

If the gate fails and you need to inspect its temp app/journal, rerun with `BEATER_KEEP_GATE_WORKDIR=1`.

## Beater dashboard trace gate

`scripts/beater-dashboard-trace-gate.cjs` is the local Beater native ingest/read proof for the dashboard-backed path. It starts a mock Anthropic SSE server and a local `beaterd` from `BEATERD_BIN` or a sibling `../beater` checkout, runs the real `beater agent run` CLI against a temp app with `rustTool("get_time")`, exports native spans to `/v1/traces/native`, then verifies the Beater read endpoints the dashboard uses: trace list, trace detail, span detail, and span I/O. The script prints the dashboard URL for the exported trace. If a dashboard is already running against a fixed Beater API port, set `BEATERD_HTTP_PORT`, `BEATER_DASHBOARD_PROBE=1`, and `BEATER_DASHBOARD_URL` to require the rendered page check too.

Build the local binary with the same PyO3 settings first, then run the gate:

```sh
tmp_config=$(mktemp /tmp/beater-pyo3-config.XXXXXX)
printf '%s\n' \
  'implementation=CPython' \
  'version=3.9' \
  'shared=true' \
  'abi3=false' \
  'lib_name=python3.9' \
  'lib_dir=/Library/Developer/CommandLineTools/Library/Frameworks/Python3.framework/Versions/3.9/lib' \
  'executable=/Library/Developer/CommandLineTools/Library/Frameworks/Python3.framework/Versions/3.9/bin/python3.9' > "$tmp_config"
PYO3_CONFIG_FILE="$tmp_config" \
  DYLD_FRAMEWORK_PATH=/Library/Developer/CommandLineTools/Library/Frameworks \
  cargo build --bin beater
rm -f "$tmp_config"
scripts/beater-dashboard-trace-gate.cjs
```

If no sibling Beater checkout exists, set `BEATERD_BIN=/path/to/beaterd`. If the gate fails and you need to inspect its temp app or Beater data directory, rerun with `BEATER_KEEP_GATE_WORKDIR=1`.

To require the rendered dashboard page too, install the dashboard dependencies if needed, run the sibling dashboard against a fixed Beater API port, then run the gate with the probe enabled:

```sh
cd ../beater/web/dashboard
npm ci
NEXT_PUBLIC_BEATER_API_BASE_URL=http://127.0.0.1:18080 \
  BEATER_API_BASE_URL=http://127.0.0.1:18080 \
  npm run dev -- --hostname 127.0.0.1 --port 3100
```

In the beater.js repo:

```sh
BEATERD_HTTP_PORT=18080 \
  BEATER_DASHBOARD_PROBE=1 \
  BEATER_DASHBOARD_URL=http://127.0.0.1:3100 \
  scripts/beater-dashboard-trace-gate.cjs
```

That rendered probe passed on 2026-07-06 against the sibling Beater dashboard for an exported run trace with four spans.

## Deploy gate probe

The deploy proof is `scripts/docker-cold-start-gate.sh`. It builds the release CLI in a Linux Docker builder, runs `beater build` for `examples/hello`, builds the generated Dockerfile, starts the image on a loopback-only published port, checks `/api/health`, and proves `/mcp` rejects unauthenticated calls while accepting bearer-token `tools/list`.

Before running it, check both free space and Docker availability:

```sh
df -h / /Users/jadenfix
docker version --format '{{.Server.Version}}'
scripts/docker-cold-start-gate.sh
```

The Docker gate needs roughly 12 GiB free by default. It is safe to run `cargo clean` to remove generated Rust build artifacts when local disk pressure blocks progress; expect the next Rust build/test run to rebuild dependencies.

CI sets `BEATER_DOCKER_COLD_START_MS=3000` to avoid runner-scheduling flakes while preserving the cold-container proof. Local runs default to `1000`.

## Known local blockers

- Docker Desktop may fail locally with a missing daemon socket or containerd metadata I/O errors; do not treat that as proof the Docker cold-start gate is good or bad.
- Live Anthropic gates require `ANTHROPIC_API_KEY`; without it, only mock/unit/smoke coverage can run.
