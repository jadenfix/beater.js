#!/usr/bin/env bash
# Offline checks for m2-live-gate.sh safety behavior. This does not call the
# live Anthropic API.
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

bash -c 'set -euo pipefail; source "$1"; cleanup' _ "$SCRIPT"

bash -c 'set -euo pipefail; source "$1"; sleep 30 & pid=$!; track_pid "$pid"; cleanup; if kill -0 "$pid" 2>/dev/null; then kill -9 "$pid" 2>/dev/null || true; exit 1; fi' _ "$SCRIPT"

bash -c 'set -euo pipefail; source "$1"; sleep 30 & pid=$!; track_pid "$pid"; untrack_pid "$pid"; cleanup; kill -0 "$pid" 2>/dev/null; alive=$?; kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; exit "$alive"' _ "$SCRIPT"

echo "m2 live gate self-test passed"
