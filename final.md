# final.md — What "done" means for beater.js

This is the honest completion contract: what is verified today, the exact gap between here and an end-to-end-done MVP, and the concrete checklist that would make the framework genuinely complete against its thesis (ARCHITECTURE.md §1). Three levels, in order:

1. **[A] MVP e2e-done** — the vertical slice fully proven (one remaining gate)
2. **[B] v0.1 release-done** — someone who isn't Jaden can use it
3. **[C] Thesis-done (1.0)** — a credible Node/Next alternative

---

## Working model: 3 goal-oriented agents

This file is also the coordination contract for finishing `beater.js` in parallel. Each agent should work in small, reviewable PRs; run the relevant tests before publishing; request or perform an independent review; merge only after the slice is verified; then update this file if the completion evidence changed.

### North star: the agentic web runtime

The work below is not just about matching Node/Next request handling. The end state is a runtime for the next era of agentic software: browser-capable agents, remotely managed workers, networked tool sessions, and first-class integrations living beside the web app they operate. Every PR should preserve this direction:

- **Agentic browsing:** agents can inspect, navigate, and act on web surfaces with durable browser sessions, not one-off scripts bolted onto the side.
- **Remote management:** runs, tools, browser sessions, and deployments can be started, resumed, inspected, cancelled, and audited from remote operators and AI clients.
- **Networking:** local and remote MCP/tool providers, browser/CDP endpoints, webhooks, and service-to-service calls are treated as durable capabilities with explicit auth and retry semantics.
- **Integrations:** first-party app actions, external SaaS APIs, Python/ML tools, and browser automation share one registry, one permission model, and one journaled execution path.
- **Operational trust:** every agent-visible capability has provenance, auth, observability, idempotency or review semantics, and evidence that it works under crash/restart conditions.

### Agent 1 — MVP e2e gate owner

**Owner:** this Codex thread.

**Goal:** make [A] actually true: prove the M2 live gate end to end, record the evidence, and flip the docs from "pending live gate" to "done" only after A3-A5 pass.

**Primary PR sequence:**
- [x] Add the slow-tool fixtures for A2 with the smallest possible example-app surface.
- [x] Make `scripts/m2-live-gate.sh` self-recording: raw transcripts plus `evidence.md`.
- [ ] Run and record A3 happy path with a live supported LLM provider.
- [ ] Run and record A4 crash/resume idempotent proof.
- [ ] Run and record A5 non-idempotent `needs_review` proof.
- [ ] Update README.md, ARCHITECTURE.md, and this file with exact evidence.

**Likely touched files:** `examples/hello/agents/support/**`, `README.md`, `ARCHITECTURE.md`, `final.md`, and only agent/runtime code if the live gate exposes a real bug.

**Do not claim done unless:** transcripts exist, `beater agent runs` shows the expected terminal states, the journal query proves the resume invariant, and the branch has passed the relevant local checks.

### Agent 2 — v0.1 release-hardening owner

**Owner:** second goal-oriented agent started separately by Jaden.

**Goal:** make [B] shippable by removing author-machine assumptions and adding automated confidence that does not depend on one live vendor API. This hardening work must also unblock the next agent era: remotely managed agents, networked tool integrations, remote MCP servers, browser-control providers, and production deployments that can be tested without vendor-specific live credentials.

**Primary PR sequence:**
- [x] Add focused unit tests for router matching, journal lifecycle/resume invariants, and loader transpile-cache behavior.
- [x] Add `ANTHROPIC_BASE_URL` support plus mocked journal-resume tests.
- [x] Add the no-key integration test that spawns `beater dev` and checks `/api/health`, `/`, and `/mcp`.
- [x] Add CI for fmt, clippy, and tests on macOS/Linux with rusty_v8 caching.
- [x] Improve portability/docs: Python discovery guidance, host binding, quickstart, `docs/tools.md`, and security notes.
- [x] Keep every release-hardening PR pointed at agent-platform foundations: deterministic network tests, explicit host binding, auth-ready remote surfaces, integration-friendly tool contracts, and browser/e2e hooks.

**Likely touched files:** `crates/**`, `.github/workflows/**`, docs under `docs/**`, `README.md`, `ARCHITECTURE.md`, and `final.md`.

**Do not claim done unless:** the tests prove the requirement they are attached to, CI or local equivalents are green, and any docs marked complete have been checked from a clean-user perspective.

### Agent 3 — Phase C thesis owner

**Owner:** this Codex thread when working in `codex/agent3-*` branches.

**Goal:** pay down [C] in dependency order with one reviewable PR per vertical slice, starting with the minimum Node/Next replacement path: streaming SSR, hydration, RSC, npm/node-compat, isolate pool, and deploy. Each slice should also strengthen the next-era agent platform: agentic browsing, remote management, networked tool sessions, integrations, auditability, and deployable operations.

**Primary PR sequence:**
- [x] Add streaming React SSR over the worker chunk channel and prove shell-before-delayed-subtree delivery.
- [x] Add client hydration with a per-route client bundle.
- [x] Add RSC flight protocol over the same chunk channel.
- [x] Add npm/node-compat adoption wedge.
- [x] Add isolate pool behind the existing worker protocol.
- [x] Add `beater build` runnable host-bundle foundation.
- [x] Prove the deploy story with a Linux container image and `docker run` cold-start gate.

**Likely touched files:** `crates/beater-runtime/**`, `crates/beater-cli/**`, `examples/hello/app/**`, `README.md`, `ARCHITECTURE.md`, `final.md`, and focused scripts under `scripts/**`.

**Do not claim Phase C done unless:** every [C] table item has direct evidence, the relevant e2e gate exists and passes locally or in CI, and the PR has independent subagent review before merge.

### Coordination rules

- Branches: use `codex/agent1-<slice>`, `codex/agent2-<slice>`, and `codex/agent3-<slice>` so PR ownership is obvious.
- PR size: one vertical slice per PR; avoid bundling unrelated roadmap items.
- Review: every PR gets an independent subagent review before merge; fix or explicitly document any finding.
- Merge order: Agent 1 has priority on files needed for [A]. Agent 2 should avoid editing `examples/hello/agents/support/**` while Agent 1 is proving M2.
- Rebase/pull after each merge before starting the next PR.
- Evidence beats intention: update checkboxes only when the command output, transcript, test, or merged PR proves the item.

---

## 0. Verified today (evidence, not aspiration)

| Slice | Evidence |
|---|---|
| One binary, three runtimes | `beater doctor` reports embedded V8 14.9 + embedded CPython 3.11 (`sys.executable` = the beater binary) |
| TS routes in embedded V8 | `curl /api/health` returns JSON from a `.ts` handler; live edit hot-reloads in ~1s (fresh isolate) |
| URL path decoding | router tests prove percent-encoded static segments match routes, dynamic params decode UTF-8 path segments, and malformed escapes, `%2F`, and `%00` are rejected |
| Source-mapped errors | broken route returns a stack pointing at `boom.ts:8:9` (original TS line) |
| React 19 SSR | `curl /` returns server-rendered HTML from `index.tsx`; vendored ESM, no Node/npm anywhere |
| MCP server (2025-11-25) | official MCP inspector completes initialize + tools/list + tools/call; bogus Origin → 403; GET → 405; bearer-token mode returns 401 without `Authorization`; trusted remote browser origins get preflight/CORS support |
| MCP route resources | `resources/list` advertises `beater://routes` and `resources/read` returns a markdown crawlable route/action index generated from live route metadata; the no-key dev smoke test proves the endpoint works through `/mcp` and omits a `crawl: false` route |
| MCP workflow prompts | `initialize` advertises prompt capability, `prompts/list` exposes `beater.review_pr`, `beater.update_docs`, `beater.systems_design`, and `beater.choose_stack`, `prompts/get` returns bounded text prompt messages with required-argument validation through the same authenticated `/mcp` endpoint, and `/.well-known/beater.json` publishes the prompt metadata for pre-JSON-RPC discovery |
| MCP tools/call idempotency isolation | runtime tests prove repeated `/mcp tools/call` requests with JSON-RPC id `1` generate unique `beater:mcp:<uuid>` remote tool ids and matching idempotency keys |
| Python-over-MCP | `summarize_numbers` (a `.py` file) executes in embedded CPython when called by an external MCP client |
| Python tool path containment | registry tests reject relative and absolute Python tool paths outside the agent directory before loading, then re-check containment before execute after symlink replacement |
| Route collision safety | router tests reject duplicate route patterns such as API/page pairs at the same URL and file-vs-`index` routes that both normalize to one path |
| Agent Access Layer | /robots.txt, /sitemap.xml, /llms.txt, /.well-known/beater.json generated from the route table; `export const agent = {crawl: false}` excludes a route from sitemap + llms.txt; remote deployments can override the advertised public base URL; the well-known manifest advertises MCP capabilities, resource URIs, and workflow prompts without disclosing trusted origins |
| Agent config pipeline | `agent.ts` (via `beater:agent` shim) evaluates in a one-shot isolate → JSON config → Rust registry; Python TOOL metadata loads through embedded CPython |
| LLM provider conformance | `scripts/llm-provider-conformance-gate.cjs` runs the real `beater agent run` loop against loopback Anthropic and OpenAI-compatible SSE mocks, proves Python tool execution through both adapters, verifies OpenAI provider tool-name sanitization plus fallback tool IDs, and asserts canonical journal rows/partials |
| Durability machinery (code) | SQLite journal with started/completed/failed lifecycle + attempts; resume logic for dangling LLM calls and idempotent-only tool re-runs; `needs_review` parking |
| Anthropic network hardening | the Messages client has a request timeout; focused tests prove stalled requests and truncated successful responses retry, while truncated non-retryable API errors do not |
| Resume stop_reason safety | `resume_preserves_failed_refusal_instead_of_marking_completed`, `resume_marks_running_max_tokens_failed_and_does_not_run_truncated_tools`, `resume_marks_completed_end_turn_finished_without_reissuing_llm`, and `resume_reissues_pause_turn_instead_of_marking_completed` prove resume no longer turns refusal/max_tokens/pause_turn journal states into false completions |
| M2 crash/resume fixtures | `slow_summarize.py` and `slow_summarize_once.py` are declared from `examples/hello/agents/support/agent.ts`; `scripts/m2-live-gate.sh` drives A3-A5 once a supported live provider key/model is present and writes raw transcripts plus `evidence.md` |
| M2 live gate harness safety | `scripts/m2-live-gate-self-test.sh` proves cleanup kills tracked background runs, untracked PIDs survive cleanup, and strict journal count queries fail instead of passing expected-zero assertions on SQLite errors |
| Streaming SSR | `scripts/streaming-ssr-gate.sh` starts `beater dev`, reads the raw HTTP socket, and proved shell marker at 0.026s before Suspense-delayed marker at 0.489s while `/api/health` returned in 0.002s |
| Client hydration + browser graph | `/_beater/client/index.js` serves `app/routes/index.client.ts`, rewrites reachable static imports to same-origin `?dep=<id>` module URLs, rejects browser-unsafe imports, and `scripts/client-hydration-gate.cjs` verifies the counter increments using an imported helper in a browser |
| RSC transport foundation | `/_beater/rsc/index.flight` serves `app/routes/index.server.tsx` as `text/x-component` frames over the worker stream channel; `scripts/rsc-flight-gate.cjs` verifies the browser renders the server island and the client counter still hydrates |
| npm/node-compat wedge | `scripts/npm-compat-gate.sh` scaffolds a temp app, installs `zod@4.4.3`, adds a route importing `import { z } from "zod"`, verifies `/api/zod`, adds a leaf `.cjs` package imported as a default export, verifies `/api/cjs`, verifies fixture ESM packages can import `node:buffer`, minimal `node:events`, sanitized deterministic `node:os`, string-only POSIX `node:path`, deterministic file URL helpers from `node:url`, sanitized `node:process`, and deterministic `node:util`/`node:util/types`, and proves unsupported CommonJS `require()` fails closed through `/api/cjs-require`; loader unit tests cover server-side export conditions, wildcard subpath exports, import maps, `.cjs` wrapping, client `.cjs` rejection, the vendored buffer/events/os/path/url/util/process shims, and symlink boundary rejection |
| OpenAPI path grouping | `beater-connect` tests parse generated OpenAPI and prove resources plus multiple actions sharing one path emit a single `paths` key with all methods preserved |
| Playwright browser provider | `scripts/playwright-browser-gate.cjs` installs the upstream runner dependencies in a temp dir, runs a local authenticated browser fixture plus Anthropic-compatible SSE mock, drives `beater agent run` through `provider: "playwright"`, and verifies three completed Chromium tool results reused one run-scoped session in SQLite without leaking the password |
| Live LLM provider smoke harness | `scripts/llm-live-provider-smoke.cjs` is an opt-in, no-committed-secret gate for real Anthropic or OpenAI-compatible providers, including the first-class `nvidia` alias for NVIDIA's Chat Completions endpoint: it requires an explicit model, reads keys from env, rejects unsafe base URLs before the run, drives one live `beater agent run` through a Python tool, verifies completed journal/tool/partial rows, redacts known key patterns, and writes `evidence.md` under `examples/hello/.beater/live-provider-smoke/<timestamp-pid>/` |
| Remote MCP provider gate | `scripts/remote-mcp-provider-gate.cjs` creates a temp app, imports a loopback `remoteMcpProvider`, starts `beater dev`, proves authenticated local `/mcp initialize` + `tools/list` + `tools/call` can discover and execute the imported provider tool, verifies provider bearer/session/idempotency headers, and checks the synthetic MCP journal rows do not leak fixture tokens |

---

## [A] MVP e2e-done — ONE gate remains

**The M2 live gate is the only thing between here and "e2e done" for the MVP.** Everything below it in this section is already coded; the gate still has not produced passing live supported-provider evidence. Attempts that fail at provider authentication, billing, quota, or model access before the first completed tool call do not count as A3-A5 evidence.

### A1. Prerequisite: funded supported-provider API access in the shell environment

The only external input needed is a funded live provider that satisfies the canonical tool-call contract. Use the provider-agnostic deployment surface so the provider, model, key, and base URL travel together with the selected adapter:

```sh
export BEATER_LLM_PROVIDER=anthropic
export BEATER_LLM_MODEL=claude-opus-4-8
export BEATER_LLM_API_KEY=...
```

OpenAI-compatible providers are also valid when the provider, model, base URL, custom-origin opt-in, and key are configured together:

```sh
export BEATER_LLM_PROVIDER=openai-compatible
export BEATER_LLM_MODEL=z-ai/glm-5.2
export BEATER_LLM_BASE_URL=https://integrate.api.nvidia.com/v1
export BEATER_OPENAI_ALLOW_CUSTOM_BASE_URL=1
export BEATER_LLM_API_KEY=...
```

For NVIDIA, use the first-class alias so the provider name selects the OpenAI-compatible adapter without pretending the key is Anthropic:

```sh
export BEATER_LLM_PROVIDER=nvidia
export BEATER_LLM_MODEL=z-ai/glm-5.2
export BEATER_NVIDIA_API_KEY=...
# BEATER_NVIDIA_BASE_URL is optional; default is https://integrate.api.nvidia.com/v1
```

Once the funded provider config is present and `./target/debug/beater` is built, run `scripts/m2-live-gate.sh --dry-run` first to validate the local binary, app fixtures, provider selection, model, base URL, and output path without making provider API calls. Then `scripts/m2-live-gate.sh` runs A3-A5 and writes transcripts plus an `evidence.md` manifest under `examples/hello/.beater/m2-gate/<timestamp-pid>/`. Authentication, billing, quota, or model-access failures before the first completed tool call are external-provider blockers, not M2 evidence; preserve the failed transcript if useful, but do not flip M2 to done.

### A2. Test fixture: slow tools for deterministic kill -9

**Done.** `examples/hello/agents/support/tools/slow_summarize.py` waits before returning and is declared in `agent.ts` as `pyTool("slow_summarize", "./tools/slow_summarize.py", { idempotent: true })`. `slow_summarize_once.py` covers the non-idempotent path with `idempotent: false`. These fixtures are also included in the `beater new` hello template.

A3-A5 are still pending until the live gate is run against a real supported provider and records passing evidence.

### A3. Gate 1 — happy path (TS agent → Rust loop → Python tool → LLM)

```sh
./target/debug/beater agent run --app examples/hello support "summarize 3,1,4,1,5"
```

**Pass:** transcript shows a `tool_use` for `summarize_numbers` → Python executes (mean 2.8) → final `end_turn` text; `beater agent runs` shows the run `completed`.

### A4. Gate 2 — crash + resume (THE thesis proof)

```sh
log=/tmp/beater-slow-run.log
rm -f "$log"
./target/debug/beater agent run --app examples/hello support "use slow_summarize on 3,1,4,1,5" >"$log" 2>&1 &
pid=$!

until run_id=$(sed -n 's/^run //p' "$log" | head -n1) && test -n "$run_id"; do sleep 0.2; done
until sqlite3 examples/hello/.beater/journal.db \
  "SELECT 1 FROM steps WHERE run_id='$run_id' AND kind='tool_call' AND status='started' AND tool_name='slow_summarize' LIMIT 1" \
  2>/dev/null | grep -qx 1; do sleep 0.2; done
kill -9 "$pid"        # mid-tool, after the started row is committed

./target/debug/beater agent runs --app examples/hello        # status: running (stale)
./target/debug/beater agent resume --app examples/hello "$run_id"
sqlite3 examples/hello/.beater/journal.db "SELECT seq,kind,status,attempt FROM steps WHERE run_id='$run_id'"
```

**Pass:** resume completes the run; the journal shows the interrupted tool_call re-ran with `attempt=2` and **only** that step re-executed (no duplicate LLM calls before the crash point).

### A5. Gate 3 — non-idempotent safety

Same kill -9 flow, but prompt for `slow_summarize_once` and wait for `tool_name='slow_summarize_once'` before killing.

**Pass:** `beater agent resume` refuses to re-run the tool, prints the reason, and `beater agent runs` shows `needs_review`. Nothing executes twice.

### A6. Close-out

- [ ] Run `scripts/m2-live-gate.sh --dry-run`, then run `scripts/m2-live-gate.sh` with the live provider key/model and preserve `examples/hello/.beater/m2-gate/<timestamp-pid>/evidence.md` plus the referenced raw logs.
- [ ] Flip M2 to **done** in README.md + ARCHITECTURE.md §9
- [x] Keep the slow-tool fixtures under `examples/hello` as living crash/resume fixtures

**When A3–A5 pass, the MVP is end-to-end done**: every claim in the manifesto's vertical slice is demonstrated, not asserted.

---

## [B] v0.1 release-done — usable by someone who isn't Jaden

The MVP proves the thesis on this machine. A release requires removing the machine- and author-specific assumptions:

### Correctness & tests
- [x] Unit tests: router matching (params, index, collisions), journal lifecycle (start/complete/fail), journal resume invariants (idempotent retry + non-idempotent `needs_review`), loader transpile-cache behavior
- [x] Integration test: spawn `beater dev`, curl /api/health + / + /mcp tools/call (no API key needed — this is exactly the M3 gate, automated)
- [x] Journal resume tests with a mocked Anthropic endpoint (`ANTHROPIC_BASE_URL` override)
- [x] CI: GitHub Actions running fmt + clippy + tests on macOS and Linux, with the rusty_v8 archive cached

### Portability (currently: works on this Mac)
- [x] **Python discovery**: `.cargo/config.toml` no longer hardcodes `PYO3_PYTHON`; README documents macOS/Linux setup; `beater doctor` reports embedded Python + venv mismatches; remote macOS/Linux CI is green.
- [x] **Concurrency**: one isolate by default serializes JS route requests, and `[app].workers = N` can round-robin route work across a pool; one dev server still serves one app. The default and the scaling gate are documented prominently in README and `docs/runtime-limits.md`.
- [x] **Port/host binding**: `beater dev --host <ip>` and `[app] host = "..."`; the no-key integration test binds `0.0.0.0` and curls through localhost.
- [x] `beater new <app>` scaffolding command (embedded hello template) — the first-five-minutes experience is tested by scaffolding and serving a generated app.

### Agent-platform enablers
- [x] Mockable outbound LLM networking: `ANTHROPIC_BASE_URL` plus explicit loopback opt-in lets resume and integration tests run against local servers instead of live vendor APIs without normalizing unsafe production key destinations.
- [x] Network bind control: `--host` / `[app] host` makes container, VM, and remote-management smoke tests possible.
- [x] Remote-management mode: documented bearer-token auth for `/mcp`, explicit trusted-host/origin rules, browser preflight/CORS support, public base URL metadata, and a safe way to expose a dev/prod agent endpoint beyond localhost.
- [x] Networked integration contract (v0.1 direct `tools/call`): `remote_mcp` tool sources, request timeouts/retries, bearer-secret handling, `tool_use_id` idempotency keys, in-memory provider session initialization, startup `tools/list` schema import through `remoteMcpProvider`, review parking, and egress policy are tested against mock servers. Next-spec MCP transport adoption remains Phase C item 8.
- [x] Agentic browsing foundation: `browserTool` provider contract, allowed-origin policy, per-tool-call mock session cleanup on success/failure/timeout, deterministic mock CDP coverage, and the Playwright provider gate prove an agent can complete a real browser task through a tool declaration with run-scoped session reuse and secret redaction. Richer production credential modes remain future hardening.
- [x] Integration registry docs: `docs/integrations.md` shows how first-party Python/Rust tools, remote MCP servers, and browser providers coexist in one agent config without queues or sidecar services.

### Security floor (currently: dev-mode assumptions everywhere)
- [x] /mcp local-dev mode can remain unauthenticated; `BEATER_MCP_TOKEN` enables bearer auth, `BEATER_MCP_TRUSTED_ORIGINS` pins browser operators, and smoke tests prove unauthorized remote calls fail closed.
- [x] Python tools run with full process privileges — trust model documented in `docs/security.md`; untrusted scalar wasm now uses the hermetic local `wasmtime` tier
- [x] Journal stores full prompts/results in plaintext SQLite — documented in `docs/security.md`; redaction hooks remain later work

### Docs
- [x] README quickstart actually runnable start-to-finish by a stranger (install Rust, install Python 3.11+, cargo build, `beater new`, `beater dev`)
- [x] `docs/tools.md`: the pyTool/rustTool contract (TOOL dict, run(), idempotency rules)
- [x] `docs/integrations.md`: one-registry contract for first-party tools, remote MCP sources, mock browser providers, production browser-provider acceptance criteria, secrets, retries, idempotency, egress, and journal audit rules
- [x] `docs/runtime-limits.md`: default single-isolate route serialization, `[app].workers` pool support, one-app-per-dev-server limit, operational guidance, and the remaining scaling-proof acceptance path
- [x] CHANGELOG + versioning policy (deno_core pin-bump cadence)

---

## [C] Thesis-done (1.0) — the punt list, paid off

These are the items ARCHITECTURE.md §8 explicitly deferred, in dependency order. Each has a one-line acceptance criterion.

The through-line is not just parity with Node/Next; it is an agent-native runtime where browsers, remote MCP servers, SaaS APIs, local ML tools, and human-facing web actions are all first-class, durable, inspectable capabilities. Phase C work should therefore prefer slices that improve remote management, networking, integrations, browser automation, and deployability over isolated demos.

Phase C progress so far:

- Route responses can now carry ordered `body_chunks`; the Rust server forwards them as chunked response bodies and strips stale `content-length` headers.
- Route-scoped client modules can now live beside page routes as `*.client.ts` files and are served from `/_beater/client/<route>.js`; the browser module graph rewrites reachable static imports to same-origin dependency URLs and supports relative app helpers, import-map aliases, and browser-safe ESM packages while rejecting `.cjs`, `require()`, Node built-ins, URL imports, dynamic imports, symlink escapes, and oversized graphs. The hello page uses an imported helper to prove same-origin browser code can hydrate a counter without Node.
- Route-scoped server components can now live beside page routes as `*.server.tsx` files and stream `text/x-component` flight frames from `/_beater/rsc/<route>.flight`; this proves the transport and browser island path, not full official React Flight manifests.
- Server routes can now import local ESM packages from `node_modules` with bare specifiers, exact and wildcard `exports`, array export targets, server-side conditions, `module`/`main` fallbacks, and app-local `import_map.json` aliases with exact and prefix entries. Leaf `.cjs` modules are wrapped as ESM default exports of `module.exports`, which lets simple CommonJS packages load without adding a Node sidecar. The current server-side Node built-in shim set covers `node:buffer`/`buffer` for ESM packages that need `Buffer.from`, UTF-8/hex/base64 conversion, `byteLength`, `isBuffer`, and `concat`; minimal `node:events`/`events` `EventEmitter` semantics for listener registration, one-shot listeners, removal, listener counts, max-listener configuration, `once()` promises, and fail-closed unsupported async iterator/abort helpers; sanitized deterministic `node:os`/`os` probes for platform, arch, temp/home paths, user info, CPU/memory/load/network shape, and priority failure semantics without exposing host topology; string-only POSIX `node:path`/`path` operations such as `join`, `normalize`, `resolve`, `relative`, `basename`, `dirname`, and `extname`; deterministic `node:url`/`url` exports for minimal `URL`/`URLSearchParams`, `fileURLToPath`, and absolute POSIX `pathToFileURL`; sanitized `node:process`/`process` for `process.env.NODE_ENV`, `nextTick`, and basic version/platform probes without exposing host environment variables; and deterministic `node:util`/`util` plus `node:util/types` helpers for `format`, bounded `inspect`, `inherits`, `promisify`, `callbackify`, UTF-8 text coding, and safe type predicates. Client routes now have a separate browser-safe ESM graph resolver using `browser`/`import`/`module`/`default` conditions. CommonJS `require` fails closed; broader Node built-ins, install hooks, CommonJS browser shims, and broader production bundling remain compatibility work.
- MCP clients can now discover app structure without scraping: `resources/list` advertises `beater://routes`, and `resources/read` returns a markdown crawlable route/action index generated from live route metadata through the same authenticated `/mcp` endpoint.
- MCP clients can now discover repeatable engineering workflows without provider lock-in: `prompts/list` advertises built-in PR review, docs sync, systems design, and stack/algorithm selection prompts, and `prompts/get` validates required arguments before returning bounded text messages without executing tools or exposing private agent system prompts.
- The well-known manifest now advertises the same MCP capability set, `beater://routes`/`beater://actions` resource URIs, and workflow prompt metadata that `/mcp` serves, while preserving the existing rule that trusted browser origins are not disclosed.
- `beater dev` can now start `[app].workers = N` JS isolates and round-robin route work across them; `dev_server_round_robins_js_routes_across_worker_pool` proves two workers keep separate module state. `scripts/isolate-pool-scaling-gate.cjs` proved the load path on this 10-core machine: 103.53 rps with one worker, 792.10 rps with ten workers, 7.65x against a 6.0x threshold.
- Hot reload now aborts still-active SSR/RSC stream bodies if the old worker channel closes, so clients see a stream error instead of a cleanly truncated 200 response.
- Worker sends now clone the current isolate channel before awaiting bounded-queue capacity, so a wedged worker cannot hold the hot-reload sender lock while the reloader tries to swap in a fresh isolate.
- `beater build --out <dir>` now emits a runnable host-platform bundle with `bin/beater`, copied app assets, `run.sh`, `beater-build.json`, `.dockerignore`, and a non-root Dockerfile. Runtime state and common local credential files are excluded; symlinked app files and symlinked outputs are refused. `build_creates_runnable_bundle_and_refuses_unsafe_output` starts the generated launcher and hits `/api/health`; `scripts/docker-cold-start-gate.sh` builds the Linux release CLI, generates the bundle, builds the runtime image, starts it with loopback-only publishing, checks `/api/health`, and proves `/mcp` rejects unauthenticated calls while accepting bearer-token `tools/list`. GitHub Actions passed the gate on Ubuntu in 413ms.
- Dropping an SSR/RSC response body before it reaches EOF now sends `CancelStream` to the owning worker, so disconnected clients do not leave stalled stream state registered indefinitely.
- SSR/RSC stream chunks now cross from the isolate to the HTTP body through a bounded async channel, the local `ReadableStream` shim exposes queue pressure through `desiredSize`, and stale timer clears no longer accumulate cancellation ids in long-lived workers.
- Remote MCP tools now treat malformed JSON-RPC bodies after HTTP 2xx as ambiguous for non-idempotent calls, and send the journaled `ToolCallContext.idempotency_key` as the provider-facing idempotency header.
- Remote MCP tools can now opt into provider sessions with `session: {scope: "run", cleanup: "always"}`; beater sends `initialize`, stores the returned `Mcp-Session-Id` in memory for that tool, and reuses it on later `tools/call` requests.
- `scripts/remote-mcp-provider-gate.cjs` now proves remote MCP provider discovery and execution through the real dev-server `/mcp` endpoint with bearer auth on both local and provider sides, imported `tools/list` schema, provider-session reuse, journaled idempotency headers, and no fixture-token leakage into MCP responses or SQLite rows.
- Agent resume now converts failed idempotent tool re-runs and removed-tool lookups into `is_error` tool results, while still parking genuinely non-idempotent interrupted tools for review.
- Direct `/mcp tools/call` requests now create synthetic MCP journal runs, commit a `tool_call` started row before executing side-effecting tools, and finish the row/run as completed, failed, or `needs_review` before returning the MCP tool result.
- Dev hot reload now refreshes the agent/tool registry and agent metadata alongside routes and the worker, preserving the last good agent snapshot if a reload-time config rebuild fails.
- Local `wasmtime` tools now provide the fourth registry implementation kind for hermetic untrusted scalar wasm: `wasmtime_tool_runs_hermetic_wasm_function` proves execution with fuel/memory/wall limits, and `wasmtime_tool_rejects_filesystem_imports_before_execution` plus `wasmtime_policy_rejects_filesystem_mounts` prove filesystem capability denial.
- `rustTool("cpp_double")` now calls a C++ function through `cxx` on the Rust built-in path; `cpp_builtin_executes_through_rust_tool_registry` proves schema exposure and execution through the same registry path as other host tools.
- `remoteMcpProvider(prefix, ...)` now sends startup `initialize` + `tools/list`, imports each provider tool schema as `<prefix>.<provider tool name>`, and still executes calls through the existing remote MCP journal/retry/session path; tests cover JS serialization, schema import, and execution against the original provider tool name.
- `beater-connect` now emits `forms.html` from the same `Action` definitions that generate OpenAPI, MCP catalog metadata, llms.txt, robots.txt, and sitemap.xml; `generated_forms_post_actions_with_mcp_semantics` proves auth, scopes, confirm, dry-run, side-effect, and idempotency semantics line up with the MCP catalog.
- Route modules can now export `agent.actions: [defineAction(...)]`; the hello contact action proves one route handler accepts a human form POST, appears in live `/mcp tools/list` with confirm/idempotency metadata, executes through `/mcp tools/call` via the same journaled route handler path, and publishes runtime `/openapi.json`, `/llms.txt`, and well-known action metadata.
- Agent LLM calls now route through a provider adapter while preserving one canonical journal shape for messages, `tool_use`, and `tool_result` blocks. Anthropic Messages and OpenAI-compatible Chat Completions are selected explicitly by `agent.ts` or `BEATER_LLM_PROVIDER`; NVIDIA can be selected with `provider: "nvidia"` or `BEATER_LLM_PROVIDER=nvidia` as a named alias for that same protocol, and provider overrides require an explicit model override so stale model names do not cross adapters. Provider-specific stream chunks are appended as durable `step_partials` before final `llm_call` completion. `scripts/llm-provider-conformance-gate.cjs` proves both adapters through the real agent loop with loopback providers, including OpenAI tool-name sanitization and synthesized fallback tool IDs.
- Real-provider smoke testing now has a no-secret harness separate from CI mocks: `scripts/llm-live-provider-smoke.cjs` can run one configured Anthropic, OpenAI-compatible, or NVIDIA provider through `beater agent run`, a Python tool, SQLite journal checks, stream partial checks, redacted logs, and an evidence manifest. This does not replace the M2 crash/resume gate, and broader provider-matrix evidence still requires funded live credentials.
- `GET /_beater/agent/runs`, `GET /_beater/agent/runs/<run_id>`, and `GET /_beater/agent/runs/<run_id>/events` now expose protected run history, step summaries, and journaled LLM partials for browser run UIs, reusing the MCP origin/bearer policy and closing streams with a terminal event once the run reaches `completed`, `failed`, or `needs_review`.
- The hello example now includes a route-scoped recent-runs EventSource panel that lists journaled runs, opens a selected run, and renders `llm_partial` events in the browser; dev smoke tests prove the panel and transpiled client code ship in both the checked-in fixture and scaffolded app.
- Agent runs can now opt into Beater native trace export with `BEATER_TRACE_EXPORT_URL` or OTLP/HTTP export with `BEATER_OTLP_EXPORT_URL`/`OTEL_EXPORTER_OTLP_*`: after run/resume, beater.js projects journal runs and steps into `agent.run`, `llm.call`, and `tool.call` spans for `/v1/traces/native` or `/v1/traces`. Unit, runner integration, `scripts/otlp-trace-gate.cjs`, and `scripts/beater-dashboard-trace-gate.cjs` coverage prove projection, auth/header forwarding, scope/resource fields, tool metadata, an end-to-end local OTLP collector flow, Beater native ingest/read paths that back the dashboard, and a rendered Beater dashboard page for an exported run trace.
- `browserTool(..., {provider: "playwright"})` now reuses the pinned upstream Beater browser crates to launch Chromium through the Playwright runner, navigate to `input.url`, execute one optional browser action, reuse the browser session across calls in one run, and close sessions when the agent run or synthetic MCP run reaches a terminal state. App runs also write Playwright runner markers under `.beater/browser-sessions`, and `beater agent resume` removes stale markers and terminates marked runners for that run before replay/review. Unit tests cover provider configuration, scoped env secrets with redaction, run-scoped session reuse, stale runner cleanup, resume cleanup, action input shapes, and pre-driver origin rejection; `scripts/playwright-browser-gate.cjs` passed with an authenticated real Chromium flow through `beater agent run`, verified session reuse in SQLite, and asserted the password did not leak into journal rows.

| # | Item | Done when |
|---|---|---|
| 1 | **Streaming SSR** — renderToReadableStream over the chunked worker channel | **done:** `scripts/streaming-ssr-gate.sh` proved the shell chunk arrived before the Suspense-delayed subtree chunk |
| 2 | **Client hydration** — route-scoped client bundles (`/_beater/client/<route>.js`) | **done for the browser-safe static graph:** `/_beater/client/index.js` serves the route companion client module, rewrites reachable static imports to same-origin dependency modules, and the hello counter increments in the browser gate using an imported helper |
| 3 | **RSC** — flight protocol over the same chunked channel | **partial:** `/_beater/rsc/index.flight` streams the hello server island and the browser gate renders it; official React Flight client references/manifests remain after npm-compat |
| 4 | **npm/node-compat** — the adoption wedge (Deno-style compat layer, not a reimplementation) | **done for the wedge:** `import { z } from "zod"` works in a route, leaf `.cjs` packages can be imported as default exports, ESM fixture packages can import `node:buffer`, minimal `node:events`, sanitized deterministic `node:os`, string-only POSIX `node:path`, deterministic file URL helpers from `node:url`, sanitized `node:process`, and deterministic `node:util`/`node:util/types`, and resolver tests cover exact/wildcard `exports`, array export targets, server-side conditions, package-boundary rejection including symlinks, `module`/`main` fallbacks, app-local `import_map.json` exact/prefix aliases, `.cjs` wrapping, the vendored buffer/events/os/path/url/util/process shims, browser-safe client graph bundling, and fail-closed client `.cjs`/`require`/Node builtin/URL/dynamic imports; CommonJS browser shims, broader Node built-ins, install hooks, and broader production bundling remain later work |
| 5 | **Isolate pool** — N workers behind the existing channel protocol | **done:** `scripts/isolate-pool-scaling-gate.cjs` showed 7.65x route throughput on ten local workers versus one worker |
| 6 | **Wasm sandbox tier** — Wasmtime as the 4th tool impl kind | **done for W0:** local `wasmtime` tools run hermetic scalar wasm with empty imports, no filesystem mounts, no network/env/secrets, and fuel/memory/wall limits; tests prove filesystem imports and mounts are denied |
| 7 | **LLM streaming** — provider streaming to browser + partial-step journal records | **done for the first browser history surface:** provider-adapted stream ingestion, partial-step journal records, protected run list/detail/events endpoints, and the hello recent-runs EventSource panel are implemented; `scripts/llm-provider-conformance-gate.cjs` proves Anthropic/OpenAI-compatible adapter conformance through the real loop; richer production run-management UI can build on these surfaces |
| 8 | **MCP consume + sessions** — use remote MCP servers as tool sources; add session/auth plumbing for remote management; adopt the next MCP spec when released | **partial:** declared remote MCP tools support scoped credentials, egress, retries, idempotency keys, review parking, provider session initialization, and startup `tools/list` schema import via `remoteMcpProvider`; done requires next-spec transport adoption |
| 9 | **Agentic browsing** — reuse beater-agents' CDP/Playwright crates as a tool provider | **done for the Playwright provider:** `provider: "playwright"` reuses the upstream Beater browser crates, `scripts/playwright-browser-gate.cjs` proves authenticated real Chromium tool calls through the agent loop with journal verification and run-scoped session reuse, scoped env secrets are redacted from result payloads, and resume cleans stale browser runner markers before replay/review |
| 10 | **defineAction** — one definition → HTML form + MCP tool + OpenAPI + crawler metadata (§6b end state) | **done for the first route-action path:** route-bound `defineAction` now powers a human form POST, live MCP `tools/list` + journaled `tools/call`, runtime `/openapi.json`, `/llms.txt`, and well-known action metadata; `beater-connect` static `Action` definitions still emit `forms.html`, OpenAPI, MCP catalog, and crawler metadata with matching semantics |
| 11 | **Deploy story** — `beater build` → single container (binary + assets + venv) | **done for the generated-image path:** `scripts/docker-cold-start-gate.sh` builds the Linux release CLI, runs `beater build`, builds the generated Dockerfile, starts the image with loopback-only publishing, verifies `/api/health`, and proves `/mcp` auth; GitHub Actions passed it on Ubuntu in 413ms |
| 12 | **Observability** — OTLP out of the agent loop into beater-agents | **done for the implemented exporter paths:** opt-in native and OTLP/HTTP exporters post finished journal runs and steps to `/v1/traces/native` or `/v1/traces`; `scripts/otlp-trace-gate.cjs` proves the local OTLP/HTTP JSON collector path, and `scripts/beater-dashboard-trace-gate.cjs` proves a real Beater native ingest/read/span-I/O flow through the endpoints the dashboard uses. With `BEATER_DASHBOARD_PROBE=1`, the same gate also proved a rendered Beater dashboard page for an exported run trace. Beater's tenant-scoped protobuf OTLP ingest remains a separate compatibility target if needed |
| 13 | **Free-threaded Python** — pyo3 on 3.14t once ML wheels are reliable | two Python tools execute truly in parallel under load |
| 14 | **C++ tools** — via cxx on the Rust builtin path | **done for the first builtin:** `rustTool("cpp_double")` exposes a schema and calls a C++ function through `cxx`; broader C++ packaging remains future work |

**Definition of thesis-done:** a team can build and deploy a production app where the web UI, the agents, the browser automation, the remote integrations, and the ML tools live in one repo, run in one process, survive crashes mid-agent-loop, and are discoverable/callable by third-party AI agents — without Node, without a queue between the web and ML halves, and without a cloud lock-in. Items 1–5 + 11 are the minimum for the web-runtime replacement to be true; 6–10 + 12–14 make it an agent-native platform for remote management, networking, integrations, and browsing.

---

## TL;DR

- **To be e2e done (MVP):** configure a funded supported live provider, run `scripts/m2-live-gate.sh`, preserve the emitted `evidence.md` + raw logs, then flip README.md/ARCHITECTURE.md/final.md from pending to done. The remaining MVP blocker is live-provider evidence for M2, not missing local harness code.
- **To ship v0.1:** keep the current tests + CI, portable Python config, isolate-pool support plus scaling gate, deploy gate, Playwright gate, OTLP gate, Beater dashboard-read gate, and `beater new` path green from a clean-user checkout.
- **To kill Node/Next:** pay off the remaining partial Phase C items, including full React Flight manifests, CommonJS `require`/broader Node built-in compatibility beyond the current minimal `node:buffer`, minimal `node:events`, sanitized deterministic `node:os`, string-only POSIX `node:path`, deterministic `node:url`, sanitized `node:process`, and deterministic `node:util` shims, install hooks, CommonJS browser shims and broader production bundling, recorded live results from the broader model/provider smoke harness beyond the local Anthropic/OpenAI-compatible mocks and NVIDIA alias coverage, next-spec MCP transport adoption, Beater tenant-scoped protobuf OTLP ingest compatibility if required, free-threaded Python, and broader C++ packaging, while keeping remote management, networking, integrations, and agentic browsing as first-class platform requirements rather than later add-ons.
