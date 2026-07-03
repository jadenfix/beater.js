#!/usr/bin/env bash
# Run the live M2 gate from final.md: happy path, idempotent crash/resume,
# and non-idempotent needs_review. Requires a real Anthropic API key.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

APP="${BEATER_APP:-examples/hello}"
BIN="${BEATER_BIN:-./target/debug/beater}"
JOURNAL="$APP/.beater/journal.db"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_LABEL="$STAMP-pid$$"
OUT="${M2_GATE_OUT:-$APP/.beater/m2-gate/$RUN_LABEL}"
EVIDENCE="$OUT/evidence.md"

A3_RUN_ID=""
A4_RUN_ID=""
A4_CRASHED_SEQ=""
A4_CRASHED_TOOL_USE_ID=""
A5_RUN_ID=""
A5_CRASHED_SEQ=""
A5_CRASHED_TOOL_USE_ID=""
CLEANUP_PIDS=()

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
  local query="$1"
  local output
  if ! output="$(sqlite3 "$JOURNAL" "$query" 2>&1)"; then
    fail "sqlite count query failed: $output; query=$query"
  fi
  printf '%s\n' "$output"
}

try_sql_count() {
  sqlite3 "$JOURNAL" "$1" 2>/dev/null
}

track_pid() {
  CLEANUP_PIDS+=("$1")
}

untrack_pid() {
  local remove="$1"
  local pid
  local remaining=()
  for pid in "${CLEANUP_PIDS[@]}"; do
    if [[ "$pid" != "$remove" ]]; then
      remaining+=("$pid")
    fi
  done
  CLEANUP_PIDS=()
  if (( ${#remaining[@]} > 0 )); then
    CLEANUP_PIDS=("${remaining[@]}")
  fi
}

cleanup() {
  local pid
  if (( ${#CLEANUP_PIDS[@]} == 0 )); then
    return 0
  fi
  for pid in "${CLEANUP_PIDS[@]}"; do
    kill -0 "$pid" 2>/dev/null || continue
    kill -9 "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  done
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
    if count="$(try_sql_count "SELECT COUNT(*) FROM steps WHERE run_id='$run_id' AND kind='tool_call' AND status='started' AND tool_name='$tool'")"; then
      count="${count:-0}"
    else
      count=0
    fi
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
  sql "SELECT seq,kind,status,attempt,tool_name,tool_use_id FROM steps WHERE run_id='$run_id'" | tee "$OUT/a3-steps.tsv"
  A3_RUN_ID="$run_id"
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
  track_pid "$pid"

  local run_id
  run_id="$(wait_for_run_id "$log" "$pid")"
  wait_for_started_tool "$run_id" "$tool" "$pid" "$log"

  local crashed_seq
  local crashed_tool_use_id
  crashed_seq="$(sql "SELECT seq FROM steps WHERE run_id='$run_id' AND kind='tool_call' AND status='started' AND tool_name='$tool' ORDER BY seq DESC LIMIT 1")"
  [[ "$crashed_seq" =~ ^[0-9]+$ ]] || fail "could not capture crashed $tool seq for $run_id"
  crashed_tool_use_id="$(sql "SELECT tool_use_id FROM steps WHERE run_id='$run_id' AND seq=$crashed_seq")"
  [[ -n "$crashed_tool_use_id" ]] || fail "could not capture crashed $tool tool_use_id for $run_id"

  kill -9 "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  untrack_pid "$pid"

  "$BIN" agent runs --app "$APP" | tee "$runs_before_log"
  "$BIN" agent resume --app "$APP" "$run_id" | tee "$resume_log"
  "$BIN" agent runs --app "$APP" | tee "$runs_after_log"
  sql "SELECT seq,kind,status,attempt,tool_name,tool_use_id FROM steps WHERE run_id='$run_id'" | tee "$steps_log"

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

  case "$gate" in
    a4)
      A4_RUN_ID="$run_id"
      A4_CRASHED_SEQ="$crashed_seq"
      A4_CRASHED_TOOL_USE_ID="$crashed_tool_use_id"
      ;;
    a5)
      A5_RUN_ID="$run_id"
      A5_CRASHED_SEQ="$crashed_seq"
      A5_CRASHED_TOOL_USE_ID="$crashed_tool_use_id"
      ;;
  esac

  echo "$gate run: $run_id"
}

write_evidence() {
  cat >"$EVIDENCE" <<EOF
# M2 live gate evidence

Generated: $STAMP

App: \`$APP\`
Binary: \`$BIN\`
Journal: \`$JOURNAL\`
Output: \`$OUT\`
Messages API base: \`${ANTHROPIC_BASE_URL:-https://api.anthropic.com}\`

## A3 happy path

- Run ID: \`$A3_RUN_ID\`
- Transcript: \`a3-happy.log\`
- Journal steps: \`a3-steps.tsv\`
- Verified: run status is \`completed\` and \`summarize_numbers\` completed.

## A4 idempotent crash/resume

- Run ID: \`$A4_RUN_ID\`
- Interrupted tool: \`slow_summarize\`
- Interrupted step: \`$A4_CRASHED_SEQ\`
- Tool use ID: \`$A4_CRASHED_TOOL_USE_ID\`
- Killed-run transcript: \`a4-run.log\`
- Run before resume: \`a4-runs-before-resume.log\`
- Resume transcript: \`a4-resume.log\`
- Run after resume: \`a4-runs-after-resume.log\`
- Journal steps: \`a4-steps.tsv\`
- Verified: resume completed the run, exactly one LLM call existed before the first tool call, and only the interrupted idempotent tool was retried with \`attempt=2\`.

## A5 non-idempotent crash/resume

- Run ID: \`$A5_RUN_ID\`
- Interrupted tool: \`slow_summarize_once\`
- Interrupted step: \`$A5_CRASHED_SEQ\`
- Tool use ID: \`$A5_CRASHED_TOOL_USE_ID\`
- Killed-run transcript: \`a5-run.log\`
- Run before resume: \`a5-runs-before-resume.log\`
- Resume transcript: \`a5-resume.log\`
- Run after resume: \`a5-runs-after-resume.log\`
- Journal steps: \`a5-steps.tsv\`
- Verified: resume parked the run as \`needs_review\`, did not retry the non-idempotent tool, and did not execute any additional steps after the interrupted tool call.

EOF
}

main() {
  set -euo pipefail
  cd "$ROOT"
  trap cleanup EXIT

  need sqlite3
  need sed
  need grep
  need tee
  need find

  [[ -x "$BIN" ]] || fail "missing executable $BIN; run: cargo build -p beater-cli"
  [[ -n "${ANTHROPIC_API_KEY:-}" ]] || fail "ANTHROPIC_API_KEY is not set"
  [[ -d "$APP" ]] || fail "missing app directory: $APP"
  if [[ -d "$OUT" ]] && find "$OUT" -mindepth 1 -print -quit | grep -q .; then
    fail "output directory already contains files: $OUT"
  fi

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

  write_evidence
  echo "wrote evidence manifest to $EVIDENCE"
  echo "M2 live gate passed"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi
