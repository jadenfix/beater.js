---
name: beater-test-gates
description: Run beater.js verification gates on this local macOS workspace, including the PyO3 configuration needed for embedded Python tests.
---

# beater.js test gates

Use this skill when a change needs local verification in `jadenfix/beater.js`, especially if `cargo test` touches `pyo3` or `beater-py`, or when checking the local e2e gates listed in `final.md`.

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

## Deploy gate probe

The deploy proof is `scripts/docker-cold-start-gate.sh`. Before running it, check both free space and Docker availability:

```sh
df -h / /Users/jadenfix
docker version --format '{{.Server.Version}}'
scripts/docker-cold-start-gate.sh
```

The Docker gate needs roughly 12 GiB free by default. It is safe to run `cargo clean` to remove generated Rust build artifacts when local disk pressure blocks progress; expect the next Rust build/test run to rebuild dependencies.

## Known local blockers

- Docker Desktop may fail locally with a missing daemon socket or containerd metadata I/O errors; do not treat that as proof the Docker cold-start gate is good or bad.
- Live Anthropic gates require `ANTHROPIC_API_KEY`; without it, only mock/unit/smoke coverage can run.
