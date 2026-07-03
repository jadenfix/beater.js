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
- [ ] Run and record A3 happy path with the live Anthropic API.
- [ ] Run and record A4 crash/resume idempotent proof.
- [ ] Run and record A5 non-idempotent `needs_review` proof.
- [ ] Update README.md, ARCHITECTURE.md, and this file with exact evidence.

**Likely touched files:** `examples/hello/agents/support/**`, `README.md`, `ARCHITECTURE.md`, `final.md`, and only agent/runtime code if the live gate exposes a real bug.

**Do not claim done unless:** transcripts exist, `beater agent runs` shows the expected terminal states, the journal query proves the resume invariant, and the branch has passed the relevant local checks.

### Agent 2 — v0.1 release-hardening owner

**Owner:** second goal-oriented agent started separately by Jaden.

**Goal:** make [B] shippable by removing author-machine assumptions and adding automated confidence that does not depend on the live Anthropic API. This hardening work must also unblock the next agent era: remotely managed agents, networked tool integrations, remote MCP servers, browser-control providers, and production deployments that can be tested without vendor-specific live credentials.

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
- [ ] Add isolate pool behind the existing worker protocol.
- [ ] Add `beater build` deploy story.

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
| Source-mapped errors | broken route returns a stack pointing at `boom.ts:8:9` (original TS line) |
| React 19 SSR | `curl /` returns server-rendered HTML from `index.tsx`; vendored ESM, no Node/npm anywhere |
| MCP server (2025-11-25) | official MCP inspector completes initialize + tools/list + tools/call; bogus Origin → 403; GET → 405; bearer-token mode returns 401 without `Authorization`; trusted remote browser origins get preflight/CORS support |
| Python-over-MCP | `summarize_numbers` (a `.py` file) executes in embedded CPython when called by an external MCP client |
| Agent Access Layer | /robots.txt, /sitemap.xml, /llms.txt, /.well-known/beater.json generated from the route table; `export const agent = {crawl: false}` excludes a route from sitemap + llms.txt; remote deployments can override the advertised public base URL |
| Agent config pipeline | `agent.ts` (via `beater:agent` shim) evaluates in a one-shot isolate → JSON config → Rust registry; Python TOOL metadata loads through embedded CPython |
| Durability machinery (code) | SQLite journal with started/completed/failed lifecycle + attempts; resume logic for dangling LLM calls and idempotent-only tool re-runs; `needs_review` parking |
| M2 crash/resume fixtures | `slow_summarize.py` and `slow_summarize_once.py` are declared from `examples/hello/agents/support/agent.ts`; `scripts/m2-live-gate.sh` drives A3-A5 once `ANTHROPIC_API_KEY` is present and writes raw transcripts plus `evidence.md` |
| Streaming SSR | `scripts/streaming-ssr-gate.sh` starts `beater dev`, reads the raw HTTP socket, and proved shell marker at 0.026s before Suspense-delayed marker at 0.489s while `/api/health` returned in 0.002s |
| Client hydration | `/_beater/client/index.js` serves `app/routes/index.client.ts`; the hello page loads it as a module and `scripts/client-hydration-gate.cjs` verifies the counter increments in a browser |
| RSC transport foundation | `/_beater/rsc/index.flight` serves `app/routes/index.server.tsx` as `text/x-component` frames over the worker stream channel; `scripts/rsc-flight-gate.cjs` verifies the browser renders the server island and the client counter still hydrates |
| npm/node-compat wedge | `scripts/npm-compat-gate.sh` scaffolds a temp app, installs `zod@4.4.3`, adds a route importing `import { z } from "zod"`, and verifies `/api/zod` returns the parsed payload; loader unit tests cover server-side export conditions and wildcard subpath exports |

---

## [A] MVP e2e-done — ONE gate remains

**The M2 live gate is the only thing between here and "e2e done" for the MVP.** Everything below it in this section is already coded; it has never been exercised against the live Anthropic API.

### A1. Prerequisite: `ANTHROPIC_API_KEY` in the shell environment

The only external input needed. Install once:

```sh
echo 'export ANTHROPIC_API_KEY=sk-ant-...' >> ~/.zshenv
```

Once the key is present and `./target/debug/beater` is built, `scripts/m2-live-gate.sh` runs A3-A5 and writes transcripts plus an `evidence.md` manifest under `examples/hello/.beater/m2-gate/<timestamp-pid>/`.

### A2. Test fixture: slow tools for deterministic kill -9

**Done.** `examples/hello/agents/support/tools/slow_summarize.py` waits before returning and is declared in `agent.ts` as `pyTool("slow_summarize", "./tools/slow_summarize.py", { idempotent: true })`. `slow_summarize_once.py` covers the non-idempotent path with `idempotent: false`. These fixtures are also included in the `beater new` hello template.

A3-A5 are still pending until the live gate is run with `ANTHROPIC_API_KEY` against the real Anthropic Messages API.

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

- [ ] Run `scripts/m2-live-gate.sh` with the live key and preserve `examples/hello/.beater/m2-gate/<timestamp-pid>/evidence.md` plus the referenced raw logs.
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
- [x] **Concurrency**: one isolate = JS route requests serialize; one dev server = one app. The limitation is documented prominently in README and `docs/runtime-limits.md`; the isolate pool remains Phase C work.
- [x] **Port/host binding**: `beater dev --host <ip>` and `[app] host = "..."`; the no-key integration test binds `0.0.0.0` and curls through localhost.
- [x] `beater new <app>` scaffolding command (embedded hello template) — the first-five-minutes experience is tested by scaffolding and serving a generated app.

### Agent-platform enablers
- [x] Mockable outbound LLM networking: `ANTHROPIC_BASE_URL` lets resume and integration tests run against local servers instead of live vendor APIs.
- [x] Network bind control: `--host` / `[app] host` makes container, VM, and remote-management smoke tests possible.
- [x] Remote-management mode: documented bearer-token auth for `/mcp`, explicit trusted-host/origin rules, browser preflight/CORS support, public base URL metadata, and a safe way to expose a dev/prod agent endpoint beyond localhost.
- [x] Networked integration contract (v0.1 direct `tools/call`): `remote_mcp` tool sources, request timeouts/retries, bearer-secret handling, `tool_use_id` idempotency keys, review parking, and egress policy are tested against mock servers. Provider discovery and MCP sessions remain Phase C item 8.
- [x] Agentic browsing foundation (v0.1 mock CDP): `browserTool` provider contract, allowed-origin policy, per-tool-call mock session cleanup on success/failure/timeout, and mocked agent-loop e2e prove an agent can complete a browser task through a tool declaration. Real run-attached Playwright/CDP providers remain Phase C item 9.
- [x] Integration registry docs: `docs/integrations.md` shows how first-party Python/Rust tools, remote MCP servers, and browser providers coexist in one agent config without queues or sidecar services.

### Security floor (currently: dev-mode assumptions everywhere)
- [x] /mcp local-dev mode can remain unauthenticated; `BEATER_MCP_TOKEN` enables bearer auth, `BEATER_MCP_TRUSTED_ORIGINS` pins browser operators, and smoke tests prove unauthorized remote calls fail closed.
- [x] Python tools run with full process privileges — trust model documented in `docs/security.md` (tools are first-party code until the wasm sandbox tier lands)
- [x] Journal stores full prompts/results in plaintext SQLite — documented in `docs/security.md`; redaction hooks remain later work

### Docs
- [x] README quickstart actually runnable start-to-finish by a stranger (install Rust, install Python 3.11+, cargo build, `beater new`, `beater dev`)
- [x] `docs/tools.md`: the pyTool/rustTool contract (TOOL dict, run(), idempotency rules)
- [x] `docs/integrations.md`: one-registry contract for first-party tools, remote MCP sources, mock browser providers, production browser-provider acceptance criteria, secrets, retries, idempotency, egress, and journal audit rules
- [x] `docs/runtime-limits.md`: current single-isolate route serialization, one-app-per-dev-server limit, operational guidance, and isolate-pool acceptance path
- [x] CHANGELOG + versioning policy (deno_core pin-bump cadence)

---

## [C] Thesis-done (1.0) — the punt list, paid off

These are the items ARCHITECTURE.md §8 explicitly deferred, in dependency order. Each has a one-line acceptance criterion.

The through-line is not just parity with Node/Next; it is an agent-native runtime where browsers, remote MCP servers, SaaS APIs, local ML tools, and human-facing web actions are all first-class, durable, inspectable capabilities. Phase C work should therefore prefer slices that improve remote management, networking, integrations, browser automation, and deployability over isolated demos.

Phase C progress so far:

- Route responses can now carry ordered `body_chunks`; the Rust server forwards them as chunked response bodies and strips stale `content-length` headers.
- Route-scoped client modules can now live beside page routes as `*.client.ts` files and are served from `/_beater/client/<route>.js`; the hello page uses this to prove same-origin browser code can hydrate a counter without Node/npm. Full React hydration and bundling are still open.
- Route-scoped server components can now live beside page routes as `*.server.tsx` files and stream `text/x-component` flight frames from `/_beater/rsc/<route>.flight`; this proves the transport and browser island path, not full official React Flight manifests.
- Server routes can now import local ESM packages from `node_modules` with bare specifiers. This is the adoption wedge for real integrations and shared validation libraries without adding a Node sidecar; CommonJS, Node built-ins, install hooks, and client dependency bundling remain broader compatibility work.

| # | Item | Done when |
|---|---|---|
| 1 | **Streaming SSR** — renderToReadableStream over the chunked worker channel | **done:** `scripts/streaming-ssr-gate.sh` proved the shell chunk arrived before the Suspense-delayed subtree chunk |
| 2 | **Client hydration** — route-scoped client bundles (`/_beater/client/<route>.js`) | **done:** `/_beater/client/index.js` serves the route companion client module; the hello counter increments in the browser gate |
| 3 | **RSC** — flight protocol over the same chunked channel | **partial:** `/_beater/rsc/index.flight` streams the hello server island and the browser gate renders it; official React Flight client references/manifests remain after npm-compat |
| 4 | **npm/node-compat** — the adoption wedge (Deno-style compat layer, not a reimplementation) | **done for the wedge:** `import { z } from "zod"` works in a route; full CommonJS, Node built-ins, install hooks, and client bundling remain later work |
| 5 | **Isolate pool** — N workers behind the existing channel protocol | wrk shows near-linear scaling to core count |
| 6 | **Wasm sandbox tier** — Wasmtime as the 4th tool impl kind | an untrusted tool runs capability-scoped and cannot read the filesystem |
| 7 | **LLM streaming** — SSE to browser + partial-step journal records | tokens stream to a page while every step stays crash-resumable |
| 8 | **MCP consume + sessions** — use remote MCP servers as tool sources; add session/auth plumbing for remote management; adopt the next MCP spec when released | an agent uses a third-party MCP server's tool via config only, with scoped credentials and resumable error handling |
| 9 | **Agentic browsing** — reuse beater-agents' CDP/Playwright crates as a tool provider | an agent completes a real browsing task from a pyTool-style declaration, with browser sessions cleaned up after crashes |
| 10 | **defineAction** — one definition → HTML form + MCP tool + OpenAPI + crawler metadata (§6b end state) | a form posts for humans AND appears in tools/list with auth + confirm semantics |
| 11 | **Deploy story** — `beater build` → single container (binary + assets + venv) | `docker run` of the built image serves the app cold in <1s |
| 12 | **Observability** — OTLP out of the agent loop into beater-agents | a run's trace appears in the beater-agents dashboard |
| 13 | **Free-threaded Python** — pyo3 on 3.14t once ML wheels are reliable | two Python tools execute truly in parallel under load |
| 14 | **C++ tools** — via cxx on the Rust builtin path | a C++ function is callable as a tool with schema |

**Definition of thesis-done:** a team can build and deploy a production app where the web UI, the agents, the browser automation, the remote integrations, and the ML tools live in one repo, run in one process, survive crashes mid-agent-loop, and are discoverable/callable by third-party AI agents — without Node, without a queue between the web and ML halves, and without a cloud lock-in. Items 1–5 + 11 are the minimum for the web-runtime replacement to be true; 6–10 + 12–14 make it an agent-native platform for remote management, networking, integrations, and browsing.

---

## TL;DR

- **To be e2e done (MVP):** install `ANTHROPIC_API_KEY`, run `scripts/m2-live-gate.sh`, preserve the emitted `evidence.md` + raw logs, then flip README.md/ARCHITECTURE.md/final.md from pending to done. Everything else is already built and verified.
- **To ship v0.1:** tests + CI, portable Python config, isolate-pool-or-documented-limits, `beater new`.
- **To kill Node/Next:** pay off punts 1–5 and 11 first, while keeping remote management, networking, integrations, and agentic browsing as first-class platform requirements rather than later add-ons.
