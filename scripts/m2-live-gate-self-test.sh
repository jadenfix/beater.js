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

if bash -c 'set -euo pipefail; source "$1"; unset M2_GATE_PROVIDER M2_GATE_MODEL BEATER_LLM_MODEL BEATER_OPENAI_API_KEY OPENAI_API_KEY; export BEATER_LLM_PROVIDER=openai-compatible; configure_provider' _ "$SCRIPT" >"$TMP/provider-missing.out" 2>"$TMP/provider-missing.err"; then
  fail "configure_provider should fail when openai-compatible has no key/model"
fi
grep -q "BEATER_OPENAI_API_KEY or OPENAI_API_KEY" "$TMP/provider-missing.err" || {
  cat "$TMP/provider-missing.err" >&2
  fail "openai-compatible provider failure did not explain the missing key"
}

if bash -c 'set -euo pipefail; source "$1"; unset M2_GATE_PROVIDER M2_GATE_MODEL BEATER_LLM_MODEL; export BEATER_LLM_PROVIDER=openai-compatible; export BEATER_OPENAI_API_KEY=fixture-key; configure_provider' _ "$SCRIPT" >"$TMP/provider-missing-model.out" 2>"$TMP/provider-missing-model.err"; then
  fail "configure_provider should fail when openai-compatible has no explicit model"
fi
grep -q "BEATER_LLM_MODEL or M2_GATE_MODEL" "$TMP/provider-missing-model.err" || {
  cat "$TMP/provider-missing-model.err" >&2
  fail "openai-compatible provider failure did not explain the missing model"
}

bash -c 'set -euo pipefail; source "$1"; unset M2_GATE_PROVIDER M2_GATE_MODEL; export BEATER_LLM_PROVIDER=openai; export BEATER_LLM_MODEL=model-fixture; export BEATER_OPENAI_API_KEY=fixture-key; configure_provider; [[ "$LLM_PROVIDER" == "openai-compatible" ]] && [[ "$BEATER_LLM_PROVIDER" == "openai-compatible" ]] && [[ "$BEATER_LLM_MODEL" == "model-fixture" ]]' _ "$SCRIPT"

bash -c 'set -euo pipefail; source "$1"; unset M2_GATE_PROVIDER M2_GATE_MODEL BEATER_LLM_PROVIDER ANTHROPIC_API_KEY; export OPENAI_API_KEY=fixture-key; export BEATER_LLM_MODEL=model-fixture; configure_provider; [[ "$LLM_PROVIDER" == "openai-compatible" ]]' _ "$SCRIPT"

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

echo "m2 live gate self-test passed"
