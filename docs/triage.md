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

---

## Second sweep — new issues filed post-#54 (#56, #73–#77)

A second multi-agent review sweep of `main` (after #54 merged) found bugs the first
sweep missed. Each was verified against the tree or refuted; only survivors were
filed. They map onto the same five families above.

| Issue | Sev | Family | One-line |
|---|---|---|---|
| #73 | Med | A (durability) | `remote_mcp` malformed-after-HTTP-200 → `is_error` not `needs_review` (duplicate side effect); `ToolCallContext.idempotency_key` ignored on the wire |
| #74 | Med | D (HTTP) | unchecked `content-length` on full bodies truncates responses; identical route patterns shadow nondeterministically; RSC-flight renders with empty request context |
| #75 | Med | C/E (crawl/contract) | API routes published in `sitemap.xml`+`llms.txt`; `0.0.0.0` base_url advertised; `robots.txt` ignores `crawl:false`; security headers skipped on agent routes |
| #76 | Med | B (isolate) | no SSR/RSC backpressure (unbounded channel + constant `desiredSize`) → memory DoS; unbounded `cancelledTimers` set |
| #77 | Med | E (contract) | beater-connect `public(false)` resources leak into `llms.txt`/`openapi.json`/mcp catalog; `agent-card.json` not A2A-conformant |
| #56 | Med | — | npm resolver (from #54): `browser`-over-`node` export condition; no wildcard subpath exports |

### Notes for the fixer
- **#73 item 1** is the highest-value new bug: it is the same duplicate-side-effect
  class as the #26/#41/#39 durability family, and the *inverse* of #52 item 1. Fix
  alongside family A. Route only **2xx-with-parse-failure** to `needs_review`
  (a 4xx→`is_error` is correct — the call never applied).
- **#75 item 1** (API routes in crawl surfaces) and **#77 item 1** (private resources
  leaking) are both "one generator honors the visibility rule, the others don't" —
  audit *every* surface generator for the same filter, don't patch one at a time.
- **#74 item 1** (content-length) mirrors the existing chunked-body strip — extend the
  same guard to `RouteBody::Full`.
- **#76** overlaps the isolate-resilience family (B); land with #33/#34.

Updated totals: **35 open bug issues** across the backlog (29 original #25–#53, plus
#56 and #73–#77 from this sweep). Severity still skews medium — the two Highs (#25,
#26) remain the top of the "fix now" list.

---

## Swarm re-triage (independent double-review of all 35 open issues)

A 70-agent swarm re-triaged every open issue: one agent triages an issue against
the current-`main` code, then a **different** agent independently re-derives the
verdict from the code before comparing — so no agent ever reviews its own work.

**Integrity of this pass:** 70/70 agents completed, 0 errors. Triager and independent
reviewer **agreed on 34 of 35** issues (the one split, #48, is a medium-vs-low nuance).

### Headline results
- **All 35 issues are confirmed real defects in current `main`.** Zero were refuted,
  duplicated, or found already-fixed by either pass. The earlier hand sweeps hold up.
- **Independent severity grade: 0 high · 28 medium · 7 low.** The swarm downgraded both
  filed "high" issues to medium with high confidence:
  - **#25** (unhandled-rejection blast radius) — medium: needs app route code to throw in
    a *detached* promise; real cross-request stream abort, but no data loss / RCE.
  - **#26** (resume marks failed runs completed) — medium: wrong-status/incomplete-output
    on the *resume path only*; no crash, data loss, or money.
- **#36** (Python path traversal → code exec) graded **low** by both agents: exploiting it
  requires a hostile `agent.ts`, which the docs treat as trusted. Keeps its `security`
  label; severity is conditional on that trust boundary.
- **27 of 35 are quick wins** (confirmed + medium + S/M effort).
- **8 carry security implications:** #35, #36, #37, #51, #74, #75, #76, #77.

### Full matrix (severity = independent swarm grade; ✓ = triager/reviewer agreed; 🔒 = security)

| Issue | Sev | Effort | Agree | Sec | Confirmed defect (one-line) |
|---|---|---|---|---|---|
| #25 | medium | S | ✓ |  | In the shared isolate, an exception thrown in a setTimeout/setInterval callback (or any stray rejected promise) … |
| #26 | medium | S | ✓ |  | On resume, a run whose last journaled llm_call ended in refusal/max_tokens/pause_turn (no client tool_use blocks… |
| #27 | medium | S | ✓ |  | HEAD requests to API routes forward the raw "HEAD" method to the isolate, so any route exporting only GET throws… |
| #28 | medium | S | ✓ |  | The RwLock read guard on worker_tx is held across the bounded-channel `send().await`, so a wedged JS worker (ful… |
| #29 | medium | M | ✓ |  | The 30s REQUEST_TIMEOUT only abandons the reply channel and never terminates V8 execution, so a hung route handl… |
| #30 | medium | M | ✓ |  | The dev hot-reloader rescans routes and swaps the isolate but never rebuilds the MCP tool registry or agents lis… |
| #31 | medium | S | ✓ |  | URL path segments are matched and captured raw (no percent-decoding), so dynamic [param] routes receive encoded … |
| #32 | medium | S | ✓ |  | On hot reload the old worker's WorkerMsg channel closes and worker_main breaks out of its loop immediately even … |
| #33 | medium | M | ✓ |  | A client disconnecting mid-stream while the React render is stalled leaks the WorkerState stream/JS reader forev… |
| #34 | medium | M | ✓ |  | When no streams are active, worker_main blocks on rx.recv().await and never polls the JsRuntime event loop, so f… |
| #35 | medium | M | ✓ | 🔒 | The MCP /mcp tools/call handler invokes side-effecting tools via execute_with_context with no journal row, viola… |
| #36 | low | S | ✓ | 🔒 | Python tool paths in registry.rs are joined onto agent_dir with only a cosmetic trim_start_matches("./") and no … |
| #37 | medium | M | ✓ | 🔒 | Python tool execution has no timeout and is cancellation-unsafe: hung/cancelled calls leak or permanently hold t… |
| #38 | medium | S | ✓ |  | The /mcp tools/call path reuses the caller's non-unique JSON-RPC request id as tool_use_id, which becomes the ou… |
| #39 | medium | S | ✓ |  | execute_sandbox returns Ok(serialized result) without inspecting result.status, so sandbox Timeout/Error/Oom/Kil… |
| #40 | medium | S | ✓ |  | The Anthropic client is built with reqwest::Client::new() (no timeout) and its create_message retry loop only re… |
| #41 | medium | S | ✓ |  | In resume_async, a failed dangling-tool re-run or an unknown/removed tool name propagates via `?` instead of bei… |
| #42 | medium | M | ✓ |  | Journal step-seq allocation is a non-atomic MAX(seq)+1 read followed by a separate INSERT, the SQLite connection… |
| #43 | medium | M | ✓ |  | openapi_json emits a separate top-level path key per resource and per action, so operations sharing a path (e.g.… |
| #44 | medium | S | ✓ |  | m2-live-gate.sh backgrounds a live `beater agent run` but installs no EXIT trap, so any `fail`/set -e exit betwe… |
| #45 | medium | S | ✓ |  | sql_count() maps any sqlite3 error to 0, so the resume-safety assert_count_equals checks that expect 0 pass vacu… |
| #46 | low | S | ✓ |  | beater-connect discovery documents (beater_manifest_json, agent_card_json, openapi_json) unconditionally adverti… |
| #47 | low | S | ✓ |  | The module-level delayedByRequest map in the hello example only deletes an entry during the post-suspense re-ren… |
| #48 | low | M | ✗ |  | Dev server has four confirmed HTTP-semantics defects: oversized bodies return 500 instead of 413, non-UTF-8 requ… |
| #49 | low | S | ✓ |  | Two dev-server nits: the file-watcher keeper thread parks with a bare std::thread::park() (a spurious wakeup dro… |
| #50 | low | M | ✓ |  | Three latent runtime correctness bugs: encodeInto over-reports `read`, stream-id uses saturating_add (id collisi… |
| #51 | low | M | ✓ | 🔒 | Five confirmed MCP/crawl hardening defects: token-length timing leak in constant_time_eq, non-object JSON-RPC bo… |
| #52 | medium | M | ✓ |  | Five confirmed agent-stack correctness defects: connect/DNS failures on non-idempotent remote MCP tools wrongly … |
| #53 | low | M | ✓ |  | Six independent test/CI-hardening defects: fixed SSR-gate port with no liveness check, vendor.sh's leftover-impo… |
| #56 | medium | M | ✓ |  | The npm-compat export resolver lacks wildcard (`./*`) subpath support and silently drops array-form export targe… |
| #73 | medium | M | ✓ |  | A remote_mcp 2xx response with a malformed/id-mismatched JSON-RPC body is classified Fatal and returned as a ret… |
| #74 | medium | M | ✓ | 🔒 | Three request/response correctness defects in beater-runtime: handler-supplied content-length is trusted verbati… |
| #75 | medium | M | ✓ | 🔒 | Crawl/agent-discovery surfaces are inconsistent and leaky: API routes get published in sitemap.xml/llms.txt, def… |
| #76 | medium | M | ✓ | 🔒 | SSR/RSC stream bodies use an unbounded mpsc channel while the ReadableStream controller reports a constant desir… |
| #77 | medium | M | ✓ | 🔒 | Resource::public(false) is honored only by sitemap_xml; llms.txt, openapi.json, mcp catalog, agent-card, and res… |

### The one disagreement — #48
Both agents confirmed all four sub-defects. Triager rated **medium**; the independent
reviewer argued **low** because the entire surface is the dev server (small real blast
radius: no security, no crash, no persistent data loss; the binary-body and route-
precedence items are self-revealing in dev). Resolution: **low-medium** — treat as low
unless `beater` gains a production serve path, at which point 413/binary-body/`Allow`
matter more.

### Recommended relabels (from this pass)
- **#25 high → medium**, **#26 high → medium** — applied (unanimous, high-confidence).
- **#36** — keep `security`; severity is low *given* the trusted-`agent.ts` boundary.
- **#48** — low (per reviewer's blast-radius argument).

### Bottom line
The backlog is 100% real and skews medium. The "fix now" set is unchanged in spirit but
re-anchored: **#26 + #41 + #39 + #73** (the resume/tool "response ≠ success" durability
family — the product thesis), **#25** (isolate blast radius), **#44 + #45** (paid-agent
orphan / vacuous gate), then the 27 quick wins by subsystem.
