# Issue Triage — open bug backlog (as of main `0333182`)

This document triages the 29 open issues (#25–#53), all filed from a multi-agent
review sweep of merged `main`. Every issue carries a precise `file:line` anchor and
a proposed fix. A representative high-severity issue in each subsystem was
re-verified against the current tree while producing this triage (`runner.rs`
resume path, `registry.rs` tool build, `bootstrap.js` timer shims) — all matched
the reports exactly, so the backlog is treated as accurate.

The reports are correct about *what* the code does. This triage adds *ordering*:
which bugs to fix first, which batch together, and which gate the product thesis
("one runtime for durable, crash-resumable agents").

## Severity distribution

| Severity | Count | Issues |
|---|---|---|
| High | 2 | #25, #26 |
| Medium | 19 | #27–#45 |
| Low (grouped) | 8 | #46–#53 (≈26 sub-items) |

Security-labeled: #36 (high-impact), #35, #51.

## Themes (how these actually cluster)

The subsystem labels scatter across five failure families. Fixing by family is
cheaper than fixing by issue number, because the fixes share code paths and tests.

### A. Crash-resume correctness — undermines the core durability thesis
The product's headline claim is a journaled, resumable agent loop. These bugs make
resume silently wrong, which is worse than a crash.

- **#26 (High)** — `resume_async` marks refusal/`max_tokens`/`pause_turn` runs as
  **completed** because it checks "no tool_use blocks" instead of `stop_reason`.
  Verified at `crates/beater-agent/src/runner.rs:150`. A *failed* run is reported as
  a success. This is the single most important correctness bug in the backlog.
- **#41 (Med)** — resume aborts with `?` instead of journaling an `is_error`
  tool_result when a dangling tool re-run fails or the tool was renamed/removed;
  deterministic failures make a run **permanently unresumable** (`runner.rs:200`).
- **#42 (Med)** — journal is unsafe under concurrent access: racy `MAX(seq)+1`, no
  `busy_timeout`/WAL, `resume()` can't tell a crashed run from a live one.
- **#39 (Med)** — sandbox `Timeout`/`Error`/`Oom`/`Denied` results are returned as
  **successful** tool results and journaled completed; a policy `Denied` looks like a
  clean call.
- **#26/#41/#39** all share one root cause: the resume/tool paths conflate
  "we got a response" with "the response was a success." Fix together.
- **#52 item 5 (Low)** — re-issued `llm_call` after crash is journaled `attempt=1`,
  losing retry lineage (§5 rule 3). Same file, fold into the #26/#41 pass.

### B. Shared-isolate fragility — one bad request wedges/poisons everything
`beater dev` runs a single V8 worker (PR #22 adds an optional pool but keeps the
default at 1). In that model, cross-request blast radius is the recurring danger.

- **#25 (High)** — an unhandled promise rejection (e.g. a throwing `setTimeout`
  callback) makes `poll_event_loop` return `Err`, which aborts **all** in-flight
  streams and can 500 an unrelated request. Verified: `bootstrap.js:28/36` wrap user
  callbacks in promise chains with no try/catch and no
  `setUnhandledPromiseRejectionHandler`. Cheap, high-value fix.
- **#29 (Med)** — request timeout abandons the reply but never terminates V8; a hung
  handler wedges the worker forever and hot reload leaks a spinning thread.
- **#28 (Med)** — `worker_tx` RwLock read guard held across a bounded-channel send can
  deadlock hot-reload recovery. (Note: PR #22 refactors this path — see cross-refs.)
- **#33 (Med)** — no stream cancellation on client disconnect; a stalled render
  busy-polls the worker forever.
- **#34 (Med)** — idle worker never drives the JS event loop, so handler-scheduled
  timers stall until the next request. Pairs with #25 (when they finally fire, they
  can poison an unrelated request).
- **#32 (Med)** — hot reload truncates in-flight streams as cleanly-terminated 200s
  (silent corruption).
- **#37 (Med)** — Python tools have no timeout and are cancellation-unsafe; four hung
  calls exhaust the global 4-permit semaphore permanently.

### C. Security / trust boundary
- **#36 (Med, security)** — Python tool paths bypass the agent-directory containment
  check the sandbox loader enforces; a traversing/absolute `path` in `agent.ts`
  executes an arbitrary `.py` on the host at registry-build time. Verified:
  `registry.rs:220` joins with only a cosmetic `trim_start_matches("./")`. Highest
  security impact (arbitrary code exec, even if `agent.ts` is nominally trusted).
- **#35 (Med, security/docs)** — `/mcp tools/call` executes side-effecting tools with
  **no journal row**, contradicting the documented "nothing bypasses the journal"
  audit contract. Either journal MCP calls or amend the docs.
- **#38 (Med)** — `/mcp tools/call` reuses the caller's JSON-RPC id as `tool_use_id`,
  so two clients using id `1` with different args send identical `Idempotency-Key`
  headers → provider dedupes and drops the second side effect.
- **#51 (Low, security)** — token-length timing leak, missing `-32600` handling,
  `AccessConfig` `Debug` would print the bearer token, unescaped sitemap XML,
  `.well-known` discloses the trusted-origin allowlist.

### D. HTTP semantics
- **#27 (Med)** — `HEAD` on API routes 500s with a JS stack instead of behaving like
  GET-with-empty-body.
- **#31 (Med)** — path segments are never percent-decoded, so `[param]` routes get
  corrupted values and encoded static paths 404.
- **#48 (Low, grouped)** — 413 vs 500 for oversized bodies, binary-body UTF-8
  corruption, missing `Allow` on 405, route-precedence tie ordering.
- **#50 (Low, grouped)** — `encodeInto` read count, stream-id `saturating_add`
  (cross-request mixing after wrap), `with_extension` vs dotted route filenames.

### E. Contract/registry drift & test-gate reliability
- **#30 (Med, bug/docs)** — hot reload never rebuilds the tool registry or agents
  list, so edited tool declarations (including security-policy fields) keep executing
  stale until restart despite a "reloaded" log.
- **#43 (Med)** — `openapi_json` emits duplicate `paths` keys when operations share a
  path; most parsers keep the last, silently dropping operations. **Directly relevant
  to the in-progress `/openapi.json` discovery work — fix before that ships.**
- **#46 (Low, docs)** — beater-connect advertises unimplemented `receipts` and OAuth
  endpoints that its own ARCHITECTURE lists as non-goals.
- **#44 (Med, ci)** — `m2-live-gate.sh` has no `EXIT` trap; a failed assertion orphans
  a live agent making **paid** API calls. Fix promptly — it costs money.
- **#45 (Med, ci)** — `m2-live-gate.sh` `sql_count` masks sqlite errors as `0`, so the
  `expected 0` journal-safety assertions pass **vacuously**. The A5 gate can report
  success without verifying anything.
- **#53 (Low, grouped, ci)** — six gate/CLI hardening nits (fixed port, `vendor.sh`
  advisory-only + BSD `sed`, `free_port` TOCTOU, hanging no-bearer test, `doctor`
  always exits 0).
- **#47 (Low)** — hello example leaks one `delayedByRequest` entry per aborted render;
  copied into every scaffolded app.

## Recommended order of work

1. **Fix now (correctness + safety, cheap or thesis-critical):**
   #26, #25, #36, #44, #45. These are either the product thesis (resume correctness),
   a broad-blast-radius crash, arbitrary code exec, or actively costing money / hiding
   test failures.
2. **Next batch (durability family A):** #41, #42, #39, #52-item-5 in one resume/tool
   pass; they share the "response ≠ success" root cause and the same files.
3. **Isolate-resilience batch (family B):** #29, #33, #34, #32, #28, #37. Land after
   the isolate-pool decision (PR #22), since some touch the same worker/server code.
4. **Security/contract:** #35, #38, #30, #43, #51.
5. **HTTP + low-grouped cleanups:** #27, #31, #48, #50, #46, #47, #53. Each grouped
   issue is one PR.

## Cross-references to open PRs

- **PR #22 (worker pool)** rewrites the exact `server.rs`/`worker.rs` send/cancel
  paths named in **#28** (RwLock guard across send), **#33** (stream cancellation via
  the new per-request `cancel_tx`), and **#50 item 2** (removes the `saturating_add`
  stream-id counter in favor of the request id). Whoever fixes those issues must
  coordinate with #22's outcome; some are partially addressed by it.
- **PR #54 (npm compat)** is orthogonal to the backlog (module loader only) but flips
  `serde_json` to `preserve_order` globally, which is what a proper fix for **#43**
  (duplicate `paths` keys) would rely on to emit grouped, ordered path objects.
- The recurring footnote in every issue — uncommitted `defineAction` / `/openapi.json`
  discovery work touching `server.rs`, `crawl.rs`, `worker.rs`, `loader.rs` — overlaps
  families B, D, and E. **#43** should be fixed as part of that work, not after.
