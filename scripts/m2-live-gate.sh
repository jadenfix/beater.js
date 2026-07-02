#!/usr/bin/env bash
# Run the live M2 gate from final.md: happy path, idempotent crash/resume,
# and non-idempotent needs_review. Requires a real Anthropic API key.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

APP="${BEATER_APP:-examples/hello}"
BIN="${BEATER_BIN:-./target/debug/beater}"
JOURNAL="$APP/.beater/journal.db"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
OUT="${M2_GATE_OUT:-$APP/.beater/m2-gate/$STAMP}"

fail() {
  echo "error: $*" >&2
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || fail "missing command: $1"
}

sql() {
  sqlite3 "$JOURNAL" "$1"
}

sql_count() {
  sqlite3 "$JOURNAL" "$1" 2>/dev/null || printf '0\n'
}

run_status() {
  sql "SELECT status FROM runs WHERE id='$1'"
}

run_id_from_log() {
  sed -n 's/^run //p' "$1" | head -n1
}

assert_count_at_least_one() {
  local query="$1"
  local label="$2"
  local count
  count="$(sql_count "$query")"
  count="${count:-0}"
  if (( count < 1 )); then
    fail "$label; count=$count"
  fi
}

assert_count_equals() {
  local query="$1"
  local expected="$2"
  local label="$3"
  local count
  count="$(sql_count "$query")"
  count="${count:-0}"
  if [[ "$count" != "$expected" ]]; then
    fail "$label; count=$count expected=$expected"
  fi
}

wait_for_run_id() {
  local log="$1"
  local pid="$2"
  local deadline=$((SECONDS + 120))
  local run_id=""
  while (( SECONDS < deadline )); do
    run_id="$(run_id_from_log "$log")"
    if [[ -n "$run_id" ]]; then
      printf '%s\n' "$run_id"
      return 0
    fi
    kill -0 "$pid" 2>/dev/null || {
      cat "$log" >&2 || true
      fail "agent process exited before printing a run id"
    }
    sleep 0.2
  done
  cat "$log" >&2 || true
  fail "timed out waiting for run id"
}

wait_for_started_tool() {
  local run_id="$1"
  local tool="$2"
  local pid="$3"
  local log="$4"
  local deadline=$((SECONDS + 180))
  local count
  while (( SECONDS < deadline )); do
    count="$(sql_count "SELECT COUNT(*) FROM steps WHERE run_id='$run_id' AND kind='tool_call' AND status='started' AND tool_name='$tool'")"
    count="${count:-0}"
    if (( count >= 1 )); then
      return 0
    fi
    kill -0 "$pid" 2>/dev/null || {
      cat "$log" >&2 || true
      fail "agent process exited before starting $tool"
    }
    sleep 0.2
  done
  cat "$log" >&2 || true
  fail "timed out waiting for started $tool tool_call"
}

verify_one_llm_before_first_tool() {
  local run_id="$1"
  local tool="$2"
  local first_tool_seq
  local prior_llms
  first_tool_seq="$(sql "SELECT MIN(seq) FROM steps WHERE run_id='$run_id' AND kind='tool_call' AND tool_name='$tool'")"
  [[ -n "$first_tool_seq" ]] || fail "no $tool tool_call found for $run_id"
  prior_llms="$(sql "SELECT COUNT(*) FROM steps WHERE run_id='$run_id' AND kind='llm_call' AND seq < $first_tool_seq")"
  [[ "$prior_llms" == "1" ]] || fail "expected exactly one llm_call before first $tool; got $prior_llms"
}

verify_exact_idempotent_retry() {
  local run_id="$1"
  local tool="$2"
  local crashed_seq="$3"
  local crashed_tool_use_id="$4"
  local retry_seq

  retry_seq="$(sql "SELECT seq FROM steps WHERE run_id='$run_id' AND kind='tool_call' AND status='completed' AND tool_name='$tool' AND tool_use_id='$crashed_tool_use_id' AND attempt=2 ORDER BY seq")"
  [[ "$retry_seq" =~ ^[0-9]+$ ]] || fail "expected exactly one completed retry for $tool; got ${retry_seq:-none}"
  [[ "$retry_seq" == "$((crashed_seq + 1))" ]] || fail "expected retry seq $((crashed_seq + 1)) immediately after crashed seq $crashed_seq; got $retry_seq"

  assert_count_equals \
    "SELECT COUNT(*) FROM steps WHERE run_id='$run_id' AND attempt > 1 AND NOT (seq=$retry_seq AND kind='tool_call' AND tool_name='$tool' AND tool_use_id='$crashed_tool_use_id' AND attempt=2)" \
    "0" \
    "unexpected retried steps besides $tool seq $retry_seq"
}

verify_non_idempotent_not_retried() {
  local run_id="$1"
  local tool="$2"
  local crashed_seq="$3"

  assert_count_equals \
    "SELECT COUNT(*) FROM steps WHERE run_id='$run_id' AND kind='tool_call' AND tool_name='$tool' AND attempt > 1" \
    "0" \
    "$tool was retried despite being non-idempotent"
  assert_count_equals \
    "SELECT COUNT(*) FROM steps WHERE run_id='$run_id' AND seq > $crashed_seq" \
    "0" \
    "resume executed additional steps after non-idempotent $tool crash"
}

run_happy_path() {
  local log="$OUT/a3-happy.log"
  echo "== A3 happy path =="
  "$BIN" agent run --app "$APP" support "use summarize_numbers to summarize 3,1,4,1,5" | tee "$log"

  local run_id
  run_id="$(run_id_from_log "$log")"
  [[ -n "$run_id" ]] || fail "A3 did not print a run id"
  [[ "$(run_status "$run_id")" == "completed" ]] || fail "A3 run $run_id did not complete"
  assert_count_at_least_one \
    "SELECT COUNT(*) FROM steps WHERE run_id='$run_id' AND kind='tool_call' AND status='completed' AND tool_name='summarize_numbers'" \
    "A3 did not complete summarize_numbers"
  sql "SELECT seq,kind,status,attempt,tool_name FROM steps WHERE run_id='$run_id'" | tee "$OUT/a3-steps.tsv"
  echo "A3 run: $run_id"
}

run_crash_resume() {
  local gate="$1"
  local tool="$2"
  local prompt="$3"
  local expected_status="$4"
  local log="$OUT/$gate-run.log"
  local resume_log="$OUT/$gate-resume.log"
  local runs_before_log="$OUT/$gate-runs-before-resume.log"
  local runs_after_log="$OUT/$gate-runs-after-resume.log"
  local steps_log="$OUT/$gate-steps.tsv"

  echo "== $gate crash/resume: $tool =="
  "$BIN" agent run --app "$APP" support "$prompt" >"$log" 2>&1 &
  local pid=$!

  local run_id
  run_id="$(wait_for_run_id "$log" "$pid")"
  wait_for_started_tool "$run_id" "$tool" "$pid" "$log"

  local crashed_seq
  local crashed_tool_use_id
  crashed_seq="$(sql "SELECT seq FROM steps WHERE run_id='$run_id' AND kind='tool_call' AND status='started' AND tool_name='$tool' ORDER BY seq DESC LIMIT 1")"
  [[ "$crashed_seq" =~ ^[0-9]+$ ]] || fail "could not capture crashed $tool seq for $run_id"
  crashed_tool_use_id="$(sql "SELECT tool_use_id FROM steps WHERE run_id='$run_id' AND seq=$crashed_seq")"
  [[ -n "$crashed_tool_use_id" ]] || fail "could not capture crashed $tool tool_use_id for $run_id"

  kill -9 "$pid"
  wait "$pid" 2>/dev/null || true

  "$BIN" agent runs --app "$APP" | tee "$runs_before_log"
  "$BIN" agent resume --app "$APP" "$run_id" | tee "$resume_log"
  "$BIN" agent runs --app "$APP" | tee "$runs_after_log"
  sql "SELECT seq,kind,status,attempt,tool_name FROM steps WHERE run_id='$run_id'" | tee "$steps_log"

  local status
  status="$(run_status "$run_id")"
  [[ "$status" == "$expected_status" ]] || fail "$gate run $run_id status=$status, expected $expected_status"
  verify_one_llm_before_first_tool "$run_id" "$tool"
  grep -q "$run_id" "$runs_after_log" || fail "$gate final runs log did not include $run_id"
  grep -q "$expected_status" "$runs_after_log" || fail "$gate final runs log did not include $expected_status"

  if [[ "$expected_status" == "completed" ]]; then
    verify_exact_idempotent_retry "$run_id" "$tool" "$crashed_seq" "$crashed_tool_use_id"
  else
    verify_non_idempotent_not_retried "$run_id" "$tool" "$crashed_seq"
    grep -q "needs review" "$resume_log" || fail "$gate resume log did not explain needs_review"
  fi

  echo "$gate run: $run_id"
}

need sqlite3
need sed
need grep
need tee

[[ -x "$BIN" ]] || fail "missing executable $BIN; run: cargo build -p beater-cli"
[[ -n "${ANTHROPIC_API_KEY:-}" ]] || fail "ANTHROPIC_API_KEY is not set"
[[ -d "$APP" ]] || fail "missing app directory: $APP"

mkdir -p "$OUT"
echo "writing transcripts to $OUT"

run_happy_path
run_crash_resume \
  "a4" \
  "slow_summarize" \
  "use slow_summarize by name on numbers 3,1,4,1,5; do not use summarize_numbers" \
  "completed"
run_crash_resume \
  "a5" \
  "slow_summarize_once" \
  "use slow_summarize_once by name on numbers 3,1,4,1,5; do not use summarize_numbers" \
  "needs_review"

echo "M2 live gate passed"
