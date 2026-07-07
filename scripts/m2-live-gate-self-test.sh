#!/usr/bin/env bash
# Offline checks for m2-live-gate.sh safety behavior. This does not call the
# live LLM provider API.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$ROOT/scripts/m2-live-gate.sh"
TMP="$(mktemp -d)"

cleanup_tmp() {
  rm -rf "$TMP"
}
trap cleanup_tmp EXIT

fail() {
  echo "error: $*" >&2
  exit 1
}

bash -n "$SCRIPT"

(
  cd "$TMP"
  set +e
  set +u
  set +o pipefail
  source "$SCRIPT"
  [[ "$PWD" == "$TMP" ]] || fail "sourcing m2-live-gate.sh changed the caller directory"
  [[ "$-" != *e* ]] || fail "sourcing m2-live-gate.sh enabled errexit in caller"
  [[ "$-" != *u* ]] || fail "sourcing m2-live-gate.sh enabled nounset in caller"
  pipefail_state="$(set -o | awk '$1 == "pipefail" { print $2 }')"
  [[ "$pipefail_state" == "off" ]] || fail "sourcing m2-live-gate.sh enabled pipefail in caller"
)

if bash -c 'source "$1"; JOURNAL="$2/missing.db"; sql_count "SELECT COUNT(*) FROM steps"' _ "$SCRIPT" "$TMP" >"$TMP/sql-count.out" 2>"$TMP/sql-count.err"; then
  fail "sql_count should fail on SQLite errors"
fi
grep -q "sqlite count query failed" "$TMP/sql-count.err" || {
  cat "$TMP/sql-count.err" >&2
  fail "sql_count failure did not explain the SQLite error"
}

bash -c 'source "$1"; JOURNAL="$2/missing.db"; if try_sql_count "SELECT COUNT(*) FROM steps"; then exit 1; else exit 0; fi' _ "$SCRIPT" "$TMP"

bash -c 'set -euo pipefail; source "$1"; [[ "$(canonical_provider openai)" == "openai-compatible" ]]' _ "$SCRIPT"

if bash -c 'set -euo pipefail; source "$1"; unset M2_GATE_PROVIDER M2_GATE_MODEL BEATER_LLM_MODEL BEATER_LLM_API_KEY BEATER_OPENAI_API_KEY OPENAI_API_KEY; export BEATER_LLM_PROVIDER=openai-compatible; configure_provider' _ "$SCRIPT" >"$TMP/provider-missing.out" 2>"$TMP/provider-missing.err"; then
  fail "configure_provider should fail when openai-compatible has no key/model"
fi
grep -q "BEATER_LLM_API_KEY" "$TMP/provider-missing.err" || {
  cat "$TMP/provider-missing.err" >&2
  fail "openai-compatible provider failure did not explain the missing key"
}

if bash -c 'set -euo pipefail; source "$1"; unset M2_GATE_PROVIDER M2_GATE_MODEL BEATER_LLM_MODEL; export BEATER_LLM_PROVIDER=openai-compatible; export BEATER_LLM_API_KEY=fixture-key; configure_provider' _ "$SCRIPT" >"$TMP/provider-missing-model.out" 2>"$TMP/provider-missing-model.err"; then
  fail "configure_provider should fail when openai-compatible has no explicit model"
fi
grep -q "BEATER_LLM_MODEL or M2_GATE_MODEL" "$TMP/provider-missing-model.err" || {
  cat "$TMP/provider-missing-model.err" >&2
  fail "openai-compatible provider failure did not explain the missing model"
}

if bash -c 'set -euo pipefail; source "$1"; unset M2_GATE_PROVIDER M2_GATE_MODEL BEATER_OPENAI_ALLOW_CUSTOM_BASE_URL; export BEATER_LLM_PROVIDER=openai; export BEATER_LLM_MODEL=model-fixture; export BEATER_LLM_API_KEY=fixture-key; export BEATER_LLM_BASE_URL=https://integrate.api.nvidia.com/v1; configure_provider' _ "$SCRIPT" >"$TMP/custom-openai-without-flag.out" 2>"$TMP/custom-openai-without-flag.err"; then
  fail "configure_provider should reject custom OpenAI-compatible HTTPS origins without an opt-in flag"
fi
grep -q "BEATER_OPENAI_ALLOW_CUSTOM_BASE_URL" "$TMP/custom-openai-without-flag.err" || {
  cat "$TMP/custom-openai-without-flag.err" >&2
  fail "custom OpenAI-compatible HTTPS failure did not explain the missing opt-in"
}

bash -c 'set -euo pipefail; source "$1"; unset M2_GATE_PROVIDER M2_GATE_MODEL; export BEATER_LLM_PROVIDER=openai; export BEATER_LLM_MODEL=model-fixture; export BEATER_LLM_API_KEY=fixture-key; export BEATER_LLM_BASE_URL=https://integrate.api.nvidia.com/v1; export BEATER_OPENAI_ALLOW_CUSTOM_BASE_URL=1; configure_provider; [[ "$LLM_PROVIDER" == "openai-compatible" ]] && [[ "$BEATER_LLM_PROVIDER" == "openai-compatible" ]] && [[ "$BEATER_LLM_MODEL" == "model-fixture" ]] && [[ "$LLM_BASE_URL" == "https://integrate.api.nvidia.com/v1" ]]' _ "$SCRIPT"

bash -c 'set -euo pipefail; source "$1"; unset M2_GATE_PROVIDER M2_GATE_MODEL BEATER_LLM_PROVIDER BEATER_LLM_API_KEY BEATER_LLM_BASE_URL BEATER_OPENAI_BASE_URL OPENAI_BASE_URL BEATER_OPENAI_ALLOW_CUSTOM_BASE_URL BEATER_OPENAI_ALLOW_INSECURE_LOOPBACK ANTHROPIC_API_KEY BEATER_NVIDIA_API_KEY NVIDIA_API_KEY; export OPENAI_API_KEY=fixture-key; export BEATER_LLM_MODEL=model-fixture; configure_provider; [[ "$LLM_PROVIDER" == "openai-compatible" ]]' _ "$SCRIPT"

if bash -c 'set -euo pipefail; source "$1"; unset M2_GATE_PROVIDER M2_GATE_MODEL BEATER_OPENAI_ALLOW_CUSTOM_BASE_URL; export BEATER_LLM_PROVIDER=openai-compatible; export BEATER_LLM_MODEL=model-fixture; export BEATER_LLM_API_KEY=fixture-key; export BEATER_LLM_BASE_URL=http://127.evil.test/v1; export BEATER_OPENAI_ALLOW_INSECURE_LOOPBACK=1; configure_provider' _ "$SCRIPT" >"$TMP/evil-loopback.out" 2>"$TMP/evil-loopback.err"; then
  fail "configure_provider should reject hostnames that only look like loopback addresses"
fi
grep -q "OpenAI-compatible base URL must use https" "$TMP/evil-loopback.err" || {
  cat "$TMP/evil-loopback.err" >&2
  fail "fake loopback hostname failure did not explain the runtime policy"
}

if bash -c 'set -euo pipefail; source "$1"; unset M2_GATE_PROVIDER M2_GATE_MODEL BEATER_OPENAI_ALLOW_CUSTOM_BASE_URL; export BEATER_LLM_PROVIDER=openai-compatible; export BEATER_LLM_MODEL=model-fixture; export BEATER_LLM_API_KEY=fixture-key; export BEATER_LLM_BASE_URL=http://127.0.0.08/v1; export BEATER_OPENAI_ALLOW_INSECURE_LOOPBACK=1; configure_provider' _ "$SCRIPT" >"$TMP/octal-loopback.out" 2>"$TMP/octal-loopback.err"; then
  fail "configure_provider should reject invalid leading-zero IPv4 loopback-looking addresses"
fi
grep -q "OpenAI-compatible base URL must use https" "$TMP/octal-loopback.err" || {
  cat "$TMP/octal-loopback.err" >&2
  fail "invalid leading-zero IPv4 failure did not explain the runtime policy"
}

if bash -c 'set -euo pipefail; source "$1"; unset M2_GATE_PROVIDER BEATER_LLM_PROVIDER ANTHROPIC_API_KEY BEATER_OPENAI_API_KEY OPENAI_API_KEY; export BEATER_LLM_API_KEY=fixture-key; selected_provider' _ "$SCRIPT" >"$TMP/generic-without-provider.out" 2>"$TMP/generic-without-provider.err"; then
  fail "selected_provider should require BEATER_LLM_PROVIDER when generic key is used"
fi
grep -q "BEATER_LLM_PROVIDER is required" "$TMP/generic-without-provider.err" || {
  cat "$TMP/generic-without-provider.err" >&2
  fail "generic key without provider failure did not explain the issue"
}

if bash -c 'set -euo pipefail; source "$1"; unset M2_GATE_PROVIDER BEATER_LLM_PROVIDER BEATER_LLM_API_KEY BEATER_LLM_BASE_URL; export ANTHROPIC_API_KEY=fixture-anthropic; export NVIDIA_API_KEY=fixture-nvidia; selected_provider' _ "$SCRIPT" >"$TMP/ambiguous-provider.out" 2>"$TMP/ambiguous-provider.err"; then
  fail "selected_provider should require an explicit provider when multiple provider keys are configured"
fi
grep -q "multiple provider key families" "$TMP/ambiguous-provider.err" || {
  cat "$TMP/ambiguous-provider.err" >&2
  fail "ambiguous provider failure did not explain the issue"
}

if bash -c 'set -euo pipefail; source "$1"; unset M2_GATE_PROVIDER BEATER_LLM_PROVIDER BEATER_LLM_MODEL M2_GATE_MODEL BEATER_LLM_API_KEY BEATER_LLM_BASE_URL BEATER_OPENAI_API_KEY OPENAI_API_KEY BEATER_NVIDIA_API_KEY NVIDIA_API_KEY; export ANTHROPIC_API_KEY=fixture-anthropic; configure_provider' _ "$SCRIPT" >"$TMP/anthropic-missing-model.out" 2>"$TMP/anthropic-missing-model.err"; then
  fail "configure_provider should fail when anthropic has no explicit model"
fi
grep -q "BEATER_LLM_MODEL or M2_GATE_MODEL" "$TMP/anthropic-missing-model.err" || {
  cat "$TMP/anthropic-missing-model.err" >&2
  fail "anthropic provider failure did not explain the missing model"
}

if bash -c 'set -euo pipefail; source "$1"; validate_provider_base_url_for_evidence "https://user:secret@example.com/v1"' _ "$SCRIPT" >"$TMP/base-url-userinfo.out" 2>"$TMP/base-url-userinfo.err"; then
  fail "base URL validation should reject credentials before evidence logging"
fi
grep -q "must not contain credentials" "$TMP/base-url-userinfo.err" || {
  cat "$TMP/base-url-userinfo.err" >&2
  fail "credential-bearing base URL failure did not explain the issue"
}

if bash -c 'set -euo pipefail; source "$1"; validate_provider_base_url_for_evidence "https://example.com/v1?api_key=secret"' _ "$SCRIPT" >"$TMP/base-url-query.out" 2>"$TMP/base-url-query.err"; then
  fail "base URL validation should reject query parameters before evidence logging"
fi
grep -q "must not contain query parameters" "$TMP/base-url-query.err" || {
  cat "$TMP/base-url-query.err" >&2
  fail "query-bearing base URL failure did not explain the issue"
}

bash -c 'set -euo pipefail; source "$1"; cleanup' _ "$SCRIPT"

bash -c 'set -euo pipefail; source "$1"; sleep 30 & pid=$!; track_pid "$pid"; cleanup; if kill -0 "$pid" 2>/dev/null; then kill -9 "$pid" 2>/dev/null || true; exit 1; fi' _ "$SCRIPT"

bash -c 'set -euo pipefail; source "$1"; sleep 30 & pid=$!; track_pid "$pid"; untrack_pid "$pid"; cleanup; kill -0 "$pid" 2>/dev/null; alive=$?; kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; exit "$alive"' _ "$SCRIPT"

DRY_APP="$TMP/dry-app"
mkdir -p "$DRY_APP/agents/support/tools"
touch "$DRY_APP/agents/support/agent.ts"
touch "$DRY_APP/agents/support/tools/summarize_numbers.py"
touch "$DRY_APP/agents/support/tools/slow_summarize.py"
touch "$DRY_APP/agents/support/tools/slow_summarize_once.py"

BEATER_APP="$DRY_APP" \
BEATER_BIN="/bin/sh" \
M2_GATE_OUT="$TMP/dry-out" \
M2_GATE_PROVIDER="" \
M2_GATE_MODEL="" \
BEATER_LLM_PROVIDER="openai-compatible" \
BEATER_LLM_MODEL="model-fixture" \
BEATER_LLM_API_KEY="fixture-key" \
BEATER_LLM_BASE_URL="https://integrate.api.nvidia.com/v1" \
BEATER_OPENAI_ALLOW_CUSTOM_BASE_URL="1" \
bash "$SCRIPT" --dry-run >"$TMP/dry-run.out"

grep -q "M2 live gate preflight passed" "$TMP/dry-run.out" || {
  cat "$TMP/dry-run.out" >&2
  fail "dry-run did not report preflight success"
}
grep -q "No provider API calls were made." "$TMP/dry-run.out" || {
  cat "$TMP/dry-run.out" >&2
  fail "dry-run did not make the no-network behavior explicit"
}
[[ ! -e "$TMP/dry-out" ]] || fail "dry-run created the output directory"

mkdir -p "$TMP/non-empty-out"
touch "$TMP/non-empty-out/existing"
if BEATER_APP="$DRY_APP" \
  BEATER_BIN="/bin/sh" \
  M2_GATE_OUT="$TMP/non-empty-out" \
  M2_GATE_PROVIDER="" \
  M2_GATE_MODEL="" \
  BEATER_LLM_PROVIDER="openai-compatible" \
  BEATER_LLM_MODEL="model-fixture" \
  BEATER_LLM_API_KEY="fixture-key" \
  BEATER_LLM_BASE_URL="https://api.openai.com/v1" \
  bash "$SCRIPT" --dry-run >"$TMP/non-empty.out" 2>"$TMP/non-empty.err"; then
  fail "dry-run should reject non-empty output directories"
fi
grep -q "output directory already contains files" "$TMP/non-empty.err" || {
  cat "$TMP/non-empty.err" >&2
  fail "non-empty dry-run failure did not explain the issue"
}

echo "m2 live gate self-test passed"
