#!/usr/bin/env bash
# Run the live M2 gate from final.md: happy path, idempotent crash/resume,
# and non-idempotent needs_review. Requires a real supported LLM provider key.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

APP="${BEATER_APP:-examples/hello}"
BIN="${BEATER_BIN:-./target/debug/beater}"
JOURNAL="$APP/.beater/journal.db"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_LABEL="$STAMP-pid$$"
OUT="${M2_GATE_OUT:-$APP/.beater/m2-gate/$RUN_LABEL}"
EVIDENCE="$OUT/evidence.md"

LLM_PROVIDER=""
LLM_MODEL=""
LLM_MODEL_DISPLAY="unset"
LLM_BASE_URL=""
DRY_RUN=0

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

usage() {
  cat <<'EOF'
Usage: scripts/m2-live-gate.sh [--dry-run]

Runs the live M2 gate from final.md: A3 happy path, A4 idempotent
crash/resume, and A5 non-idempotent needs_review.

Options:
  --dry-run   Validate local provider/app/output prerequisites without making
              provider API calls, creating evidence output, or starting agents.
  -h, --help  Show this help text.

Configuration is read from environment variables:
  BEATER_APP, BEATER_BIN, M2_GATE_OUT
  M2_GATE_PROVIDER or BEATER_LLM_PROVIDER (anthropic, openai-compatible, or nvidia)
  M2_GATE_MODEL or BEATER_LLM_MODEL
  BEATER_LLM_API_KEY, BEATER_LLM_BASE_URL
  ANTHROPIC_API_KEY, ANTHROPIC_BASE_URL
  BEATER_OPENAI_API_KEY or OPENAI_API_KEY
  BEATER_OPENAI_BASE_URL or OPENAI_BASE_URL
  BEATER_NVIDIA_API_KEY or NVIDIA_API_KEY
  BEATER_NVIDIA_BASE_URL or NVIDIA_BASE_URL
  BEATER_NVIDIA_ALLOW_CUSTOM_BASE_URL, BEATER_NVIDIA_ALLOW_INSECURE_LOOPBACK
EOF
}

parse_args() {
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --dry-run)
        DRY_RUN=1
        ;;
      -h|--help)
        usage
        exit 0
        ;;
      *)
        fail "unknown argument: $1"
        ;;
    esac
    shift
  done
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

canonical_provider() {
  local raw="${1:-}"
  local provider
  provider="$(printf '%s' "$raw" | tr '[:upper:]' '[:lower:]' | tr '_' '-')"
  case "$provider" in
    anthropic)
      printf 'anthropic\n'
      ;;
    openai|openai-compatible)
      printf 'openai-compatible\n'
      ;;
    nvidia|nvidia-nim)
      printf 'nvidia\n'
      ;;
    "")
      fail "LLM provider is empty"
      ;;
    *)
      fail "unsupported LLM provider: $raw; use anthropic, openai-compatible, or nvidia"
      ;;
  esac
}

selected_provider() {
  local requested="${M2_GATE_PROVIDER:-${BEATER_LLM_PROVIDER:-}}"
  if [[ -z "$requested" ]]; then
    if [[ -n "${BEATER_LLM_API_KEY:-}${BEATER_LLM_BASE_URL:-}" ]]; then
      fail "BEATER_LLM_PROVIDER is required when using BEATER_LLM_API_KEY or BEATER_LLM_BASE_URL"
    fi
    local configured=()
    if [[ -n "${ANTHROPIC_API_KEY:-}" ]]; then
      configured+=("anthropic")
    fi
    if [[ -n "${BEATER_OPENAI_API_KEY:-${OPENAI_API_KEY:-}}" ]]; then
      configured+=("openai-compatible")
    fi
    if [[ -n "${BEATER_NVIDIA_API_KEY:-${NVIDIA_API_KEY:-}}" ]]; then
      configured+=("nvidia")
    fi
    if (( ${#configured[@]} == 1 )); then
      requested="${configured[0]}"
    elif (( ${#configured[@]} == 0 )); then
      fail "M2_GATE_PROVIDER or BEATER_LLM_PROVIDER is required; no provider key environment was detected"
    else
      fail "M2_GATE_PROVIDER or BEATER_LLM_PROVIDER is required when multiple provider key families are configured: ${configured[*]}"
    fi
  fi
  canonical_provider "$requested"
}

validate_provider_base_url_for_evidence() {
  local raw="$1"
  if [[ "$raw" == *$'\n'* || "$raw" == *$'\r'* ]]; then
    fail "LLM base URL must not contain control characters"
  fi
  if [[ "$raw" == *"?"* || "$raw" == *"#"* ]]; then
    fail "LLM base URL must not contain query parameters or fragments"
  fi
  if [[ "$raw" == *"://"* ]]; then
    local authority="${raw#*://}"
    authority="${authority%%/*}"
    if [[ "$authority" == *"@"* ]]; then
      fail "LLM base URL must not contain credentials"
    fi
  fi
}

env_flag() {
  local value="${!1:-}"
  [[ "$value" == "1" || "$value" == "true" || "$value" == "TRUE" || "$value" == "yes" || "$value" == "YES" ]]
}

base_url_host() {
  local raw="$1"
  local authority="${raw#*://}"
  authority="${authority%%/*}"
  if [[ "$authority" == \[*\]* ]]; then
    local host="${authority%%]*}"
    printf '%s\n' "${host#[}"
  else
    printf '%s\n' "${authority%%:*}"
  fi
}

is_loopback_host() {
  local host
  local a b c d part value
  host="$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')"
  if [[ "$host" == "localhost" || "$host" == "::1" ]]; then
    return 0
  fi
  if [[ "$host" =~ ^127\.([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
    IFS='.' read -r a b c d <<<"$host"
    for part in "$a" "$b" "$c" "$d"; do
      [[ "$part" =~ ^[0-9]+$ ]] || return 1
      [[ "$part" == "0" || "$part" != 0* ]] || return 1
      value=$((10#$part))
      (( value >= 0 && value <= 255 )) || return 1
    done
    return 0
  fi
  return 1
}

validate_provider_base_url_policy() {
  local provider="$1"
  local raw="$2"
  local scheme="${raw%%:*}"
  local host

  [[ "$raw" == *"://"* ]] || fail "$provider base URL must include a scheme and host"
  host="$(base_url_host "$raw")"
  [[ -n "$host" ]] || fail "$provider base URL must include a host"

  case "$provider" in
    anthropic)
      if [[ "$scheme" == "https" && "$host" == "api.anthropic.com" ]]; then
        return 0
      fi
      if [[ "$scheme" == "https" ]]; then
        env_flag BEATER_ANTHROPIC_ALLOW_CUSTOM_BASE_URL || fail "custom Anthropic HTTPS origins require BEATER_ANTHROPIC_ALLOW_CUSTOM_BASE_URL=1"
        return 0
      fi
      if [[ "$scheme" == "http" ]] && is_loopback_host "$host"; then
        env_flag BEATER_ANTHROPIC_ALLOW_INSECURE_LOOPBACK || fail "Anthropic HTTP loopback requires BEATER_ANTHROPIC_ALLOW_INSECURE_LOOPBACK=1"
        return 0
      fi
      fail "Anthropic base URL must use https, or http loopback with BEATER_ANTHROPIC_ALLOW_INSECURE_LOOPBACK=1"
      ;;
    openai-compatible)
      if [[ "$scheme" == "https" && "$host" == "api.openai.com" ]]; then
        return 0
      fi
      if [[ "$scheme" == "https" ]]; then
        env_flag BEATER_OPENAI_ALLOW_CUSTOM_BASE_URL || fail "custom OpenAI-compatible HTTPS origins require BEATER_OPENAI_ALLOW_CUSTOM_BASE_URL=1"
        return 0
      fi
      if [[ "$scheme" == "http" ]] && is_loopback_host "$host"; then
        env_flag BEATER_OPENAI_ALLOW_INSECURE_LOOPBACK || fail "OpenAI-compatible HTTP loopback requires BEATER_OPENAI_ALLOW_INSECURE_LOOPBACK=1"
        return 0
      fi
      fail "OpenAI-compatible base URL must use https, or http loopback with BEATER_OPENAI_ALLOW_INSECURE_LOOPBACK=1"
      ;;
    nvidia)
      if [[ "$scheme" == "https" && "$host" == "integrate.api.nvidia.com" ]]; then
        return 0
      fi
      if [[ "$scheme" == "https" ]]; then
        env_flag BEATER_NVIDIA_ALLOW_CUSTOM_BASE_URL || env_flag BEATER_OPENAI_ALLOW_CUSTOM_BASE_URL || fail "custom NVIDIA/OpenAI-compatible HTTPS origins require BEATER_NVIDIA_ALLOW_CUSTOM_BASE_URL=1 or BEATER_OPENAI_ALLOW_CUSTOM_BASE_URL=1"
        return 0
      fi
      if [[ "$scheme" == "http" ]] && is_loopback_host "$host"; then
        env_flag BEATER_NVIDIA_ALLOW_INSECURE_LOOPBACK || env_flag BEATER_OPENAI_ALLOW_INSECURE_LOOPBACK || fail "NVIDIA/OpenAI-compatible HTTP loopback requires BEATER_NVIDIA_ALLOW_INSECURE_LOOPBACK=1 or BEATER_OPENAI_ALLOW_INSECURE_LOOPBACK=1"
        return 0
      fi
      fail "NVIDIA/OpenAI-compatible base URL must use https, or http loopback with BEATER_NVIDIA_ALLOW_INSECURE_LOOPBACK=1"
      ;;
  esac
}

configure_provider() {
  LLM_PROVIDER="$(selected_provider)"
  LLM_MODEL="${M2_GATE_MODEL:-${BEATER_LLM_MODEL:-}}"
  LLM_MODEL_DISPLAY="${LLM_MODEL:-agent.ts default}"

  case "$LLM_PROVIDER" in
    anthropic)
      [[ -n "${BEATER_LLM_API_KEY:-${ANTHROPIC_API_KEY:-}}" ]] || fail "BEATER_LLM_API_KEY or ANTHROPIC_API_KEY is not set for provider anthropic"
      [[ -n "$LLM_MODEL" ]] || fail "BEATER_LLM_MODEL or M2_GATE_MODEL is required for provider anthropic"
      LLM_BASE_URL="${BEATER_LLM_BASE_URL:-${ANTHROPIC_BASE_URL:-https://api.anthropic.com}}"
      ;;
    openai-compatible)
      [[ -n "${BEATER_LLM_API_KEY:-${BEATER_OPENAI_API_KEY:-${OPENAI_API_KEY:-}}}" ]] || fail "BEATER_LLM_API_KEY, BEATER_OPENAI_API_KEY, or OPENAI_API_KEY is not set for provider openai-compatible"
      [[ -n "$LLM_MODEL" ]] || fail "BEATER_LLM_MODEL or M2_GATE_MODEL is required for provider openai-compatible"
      LLM_BASE_URL="${BEATER_LLM_BASE_URL:-${BEATER_OPENAI_BASE_URL:-${OPENAI_BASE_URL:-https://api.openai.com/v1}}}"
      ;;
    nvidia)
      if [[ -n "${BEATER_LLM_API_KEY:-}" && -n "${BEATER_NVIDIA_API_KEY:-${NVIDIA_API_KEY:-}}" ]]; then
        fail "ambiguous NVIDIA key configuration: set either BEATER_LLM_API_KEY or BEATER_NVIDIA_API_KEY/NVIDIA_API_KEY, not both"
      fi
      if [[ -n "${BEATER_LLM_BASE_URL:-}" && -n "${BEATER_NVIDIA_BASE_URL:-${NVIDIA_BASE_URL:-}}" ]]; then
        fail "ambiguous NVIDIA base URL configuration: set either BEATER_LLM_BASE_URL or BEATER_NVIDIA_BASE_URL/NVIDIA_BASE_URL, not both"
      fi
      [[ -n "${BEATER_LLM_API_KEY:-${BEATER_NVIDIA_API_KEY:-${NVIDIA_API_KEY:-}}}" ]] || fail "BEATER_LLM_API_KEY, BEATER_NVIDIA_API_KEY, or NVIDIA_API_KEY is not set for provider nvidia"
      [[ -n "$LLM_MODEL" ]] || fail "BEATER_LLM_MODEL or M2_GATE_MODEL is required for provider nvidia"
      LLM_BASE_URL="${BEATER_NVIDIA_BASE_URL:-${NVIDIA_BASE_URL:-${BEATER_LLM_BASE_URL:-https://integrate.api.nvidia.com/v1}}}"
      ;;
  esac

  validate_provider_base_url_for_evidence "$LLM_BASE_URL"
  validate_provider_base_url_policy "$LLM_PROVIDER" "$LLM_BASE_URL"
  export BEATER_LLM_PROVIDER="$LLM_PROVIDER"
  if [[ -n "$LLM_MODEL" ]]; then
    export BEATER_LLM_MODEL="$LLM_MODEL"
  fi
}

check_prerequisites() {
  need sqlite3
  need sed
  need grep
  need tee
  need find

  configure_provider

  [[ -x "$BIN" ]] || fail "missing executable $BIN; run: cargo build -p beater-cli"
  [[ -d "$APP" ]] || fail "missing app directory: $APP"
  [[ -f "$APP/agents/support/agent.ts" ]] || fail "missing support agent fixture: $APP/agents/support/agent.ts"
  [[ -f "$APP/agents/support/tools/summarize_numbers.py" ]] || fail "missing happy-path tool fixture: $APP/agents/support/tools/summarize_numbers.py"
  [[ -f "$APP/agents/support/tools/slow_summarize.py" ]] || fail "missing idempotent crash fixture: $APP/agents/support/tools/slow_summarize.py"
  [[ -f "$APP/agents/support/tools/slow_summarize_once.py" ]] || fail "missing non-idempotent crash fixture: $APP/agents/support/tools/slow_summarize_once.py"
  if [[ -d "$OUT" ]] && find "$OUT" -mindepth 1 -print -quit | grep -q .; then
    fail "output directory already contains files: $OUT"
  fi
}

print_preflight() {
  echo "M2 live gate preflight passed"
  echo "App: $APP"
  echo "Binary: $BIN"
  echo "Journal: $JOURNAL"
  echo "Output: $OUT"
  echo "LLM provider: $LLM_PROVIDER"
  echo "LLM model: $LLM_MODEL_DISPLAY"
  echo "LLM base URL: $LLM_BASE_URL"
  echo "No provider API calls were made."
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
LLM provider: \`$LLM_PROVIDER\`
LLM model: \`$LLM_MODEL_DISPLAY\`
LLM base URL: \`$LLM_BASE_URL\`

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
  parse_args "$@"
  cd "$ROOT"
  trap cleanup EXIT

  check_prerequisites
  if [[ "$DRY_RUN" == "1" ]]; then
    print_preflight
    return 0
  fi

  mkdir -p "$OUT"
  echo "writing transcripts to $OUT"
  echo "LLM provider: $LLM_PROVIDER"
  echo "LLM model: $LLM_MODEL_DISPLAY"
  echo "LLM base URL: $LLM_BASE_URL"

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
