# beater.js — Architecture & Manifesto

**One runtime for the agent-first web.** A single Rust host process that serves your web app, runs your agents durably, and executes JS/TS, Python, and native Rust side by side — the way Node+Next would have been designed if it were invented after LLMs.

Status: pre-alpha. This document is the design contract; the [Milestones](#milestones) section tracks what is real today.

---

## 1. Why (first principles)

The Node/Next stack was designed for a web of documents and forms: short stateless requests, one language, rendering as the hard problem. Agent applications break every one of those assumptions:

1. **The unit of work is a loop, not a request.** An agent run is a long-lived, crash-prone sequence of LLM calls and tool executions. HTTP request/response is the wrong primitive; a durable, resumable journal is the right one.
2. **The workload is polyglot by nature.** The web half wants TypeScript and React. The ML half — embeddings, rerankers, dataframes, torch — lives in Python and native code, and no amount of JS enthusiasm changes that. Today you bridge this with microservices and queues; the complexity is accidental, not essential.
3. **Speed and control matter again.** Agent hosts multiplex many concurrent loops, stream tokens, and broker tool sandboxes. A GC'd single-threaded runtime you don't control is the wrong foundation. Rust owning the event loop, with V8 and CPython as guests, is the right one.
4. **Tools are the new packages.** MCP is doing to tools what npm did to libraries. A framework should expose and consume tools natively, not through adapters.

Nobody offers this combination today: Vercel eve and Cloudflare's agent stack are TS-only and cloud-gravity; Mastra/LangGraph/AI SDK are libraries that still live inside Node; Bun/Deno are faster Nodes, not polyglot agent hosts. beater.js takes the open slot: **web framework + agent host + polyglot ML runtime, one cloud-neutral binary.**

## 2. The four execution tiers

One Rust host process owns the event loop, the HTTP server, the agent scheduler, and the journal. Code runs in four tiers, chosen per task:

| Tier | Engine | What runs there | Why |
|---|---|---|---|
| 1 | **V8** (deno_core) | routes (TS/TSX), agent definitions, React SSR | the web's language, JIT-fast, isolate-scoped |
| 2 | **CPython** (PyO3, embedded) | ML tools — numpy, torch, pandas work as-is | full-fat Python; wasm cannot run the ML ecosystem |
| 3 | **Native Rust** | the agent loop itself, built-in tools, all framework machinery | performance, correctness, survives isolate reloads |
| 4 | **Wasmtime** | untrusted / agent-generated scalar wasm code | hermetic W0 sandbox: empty imports, no filesystem, no network, no env/secrets, fuel + memory + wall limits |

The agent loop deliberately lives in tier 3, not tier 1: it survives hot reloads of user code, it is journaled by construction, and it cannot be starved by user JS.

## 3. Primitives

- **Routes** — files under `app/routes/`. `api/*.ts` export HTTP method handlers; `*.tsx` export React components (streamed SSR). File path = URL path; `[param]` segments are dynamic.
- **Agents** — directories under `agents/<name>/`. `agent.ts` declares system prompt, provider, model, and tools. The framework extracts this config once per (re)load; the loop runs in Rust.
- **Tools** — three kinds, one registry: `pyTool(name, "./tools/x.py")` (tier 2), `rustTool(name)` (tier 3 built-ins), inline TS functions (tier 1, called back into the isolate). Every tool declares `input_schema` and an `idempotent` flag (see §5). The registry is exposed over MCP (§6).
- **Runs** — every agent invocation is a run with a journal. Runs are resumable (`beater agent resume <id>`) and inspectable (`beater agent runs`).

## 4. Design decisions

| Decision | Pick | Why |
|---|---|---|
| JS engine | `deno_core` =0.406.0 (pinned exact) | ops, ESM loading, snapshots, tokio integration; raw rusty_v8 would mean rebuilding all of that. deno_core is **not Deno**: no web APIs are included, we add only what we need |
| Web APIs in isolate | minimal shims: console, timers, TextEncoder, ReadableStream | route contract is plain objects, not WHATWG fetch classes (that fidelity comes with the npm-compat era). The stream shim is scoped to React SSR |
| TS/TSX | `deno_ast` (SWC) transpile in the module loader, source maps wired into error stacks | useful errors are an acceptance criterion, not polish |
| HTTP | axum 0.8; response bodies stream from the isolate over an mpsc channel | |
| Threading | `JsRuntime` is `!Send` → one dedicated OS thread per worker (current-thread tokio rt); host↔workers via mpsc | single worker by default; `[app].workers = N` starts a small isolate pool. See `docs/runtime-limits.md` |
| Hot reload | `notify` watcher → drop worker thread → fresh isolate (~50–200ms) | trivially correct; agent runs are unaffected (loop lives in Rust) |
| Python | pyo3 0.29 `auto-initialize` (Py_InitializeEx(0) — no Python signal handlers); one interpreter; every call via `spawn_blocking` + semaphore | GIL never touches the async runtime. Venv: **build-time** linking is `PYO3_PYTHON`; **runtime** packages come from `site.addsitedir(<venv>/site-packages)` with a version-match check (`beater doctor`) |
| LLM | reqwest → provider adapter; Anthropic Messages is the default, and OpenAI-compatible Chat Completions providers can be selected per agent or env override | the journal uses one canonical tool-use/tool-result shape; provider-specific streaming events are appended as partial-step records before final completion |
| Durability | rusqlite (bundled), step-lifecycle journal (§5); append committed **before** every side effect; LLM stream partials committed before the final response | crash-kill-9 between any two steps loses nothing |
| MCP | serve (not consume) the tool registry per spec **2025-11-25**, stateless | §6 |
| Free-threaded Python | punted until ML wheels are reliable | flip pyo3 to 3.14t, replace spawn_blocking with parallel attach |

## 5. The durability contract (journal)

```sql
CREATE TABLE runs(id TEXT PRIMARY KEY, agent TEXT, status TEXT, -- running|completed|failed|needs_review
                  input TEXT, created_at INTEGER, updated_at INTEGER);
CREATE TABLE steps(run_id TEXT, seq INTEGER,
                   kind TEXT,        -- llm_call | tool_call
                   status TEXT,      -- started | completed | failed
                   request TEXT, result TEXT,          -- exact JSON in/out
                   tool_name TEXT, tool_use_id TEXT,   -- tool_use_id = idempotency key
                   attempt INTEGER DEFAULT 1, started_at INTEGER, finished_at INTEGER,
                   PRIMARY KEY(run_id, seq));
```

Rules:
1. A `started` row is committed **before** anything executes; `completed` + result written after.
2. **Resume** rebuilds `messages[]` from completed steps.
3. A dangling (`started`, no result) `llm_call` is always safe to re-issue (`attempt+1`) — we own the request, it had no observable side effect on our state.
4. A dangling `tool_call` re-runs **only if the tool declared `idempotent: true`**. Otherwise the run is marked `needs_review` and stops. This is the side-effect contract: tools that mutate the outside world must either be idempotent (keyed by `tool_use_id`) or accept human review on crash-resume.

## 6. MCP

The tool registry is served at `POST /mcp` per spec 2025-11-25 (Streamable HTTP):
- single endpoint; JSON-RPC over POST (`initialize`, `tools/list`, `tools/call`)
- optional bearer-token auth via `BEATER_MCP_TOKEN`; missing or bad tokens fail with 401
- `Origin` header parsed and validated → 403 on mismatch (MUST, DNS-rebinding defense); loopback origins are allowed by default, remote browser origins must be listed in `BEATER_MCP_TRUSTED_ORIGINS`
- `OPTIONS /mcp` handles browser preflight for allowed origins and advertises `Authorization` + JSON headers
- generated MCP/crawl metadata uses the public base URL from `--base-url`, `BEATER_BASE_URL`, or `[app] base_url` when serving behind a remote address or proxy
- `GET /mcp` → 405 (we offer no server-initiated SSE stream — explicitly allowed by spec)
- stateless: no `MCP-Session-Id` issued

Consuming remote MCP servers as declared tool sources is implemented through direct `tools/call`, with optional bearer auth, egress policy, retries, idempotency keys, and provider session initialization. Provider `tools/list` discovery and future MCP transport upgrades remain follow-ups.

## 6b. The Agent Access Layer (agent-ready sites by default)

There are two distinct problems in making a site agent-friendly, and MCP only solves the second:

1. **Crawl layer** — *can an agent understand what's on this site?* This is plain web primitives: robots.txt, sitemap.xml, llms.txt, clean markdown views, JSON-LD.
2. **Action layer** — *can an agent safely do things?* This is MCP (§6), with auth, scopes, and idempotency.

Every other framework treats the crawl layer as hand-maintained files or plugins. beater generates it, because the framework already owns the two sources of truth it derives from: the **route table** and the **tool/agent registry**. Zero-config outputs:

| Endpoint | Derived from | Milestone |
|---|---|---|
| `/robots.txt` | crawl policy + sitemap pointer | M3 |
| `/sitemap.xml` | route table (lastmod = file mtime) | M3 |
| `/llms.txt` | route table + per-route `agent` metadata + route actions | M3 |
| `/.well-known/beater.json` | manifest: MCP endpoint, OpenAPI, sitemap, llms.txt, auth requirements, route actions | M3 |
| `/openapi.json` | route action definitions | M3 |
| markdown views (`Accept: text/markdown` / `.md`) | rendered routes | post-SSR |
| MCP `resources/list` / `resources/read` | route table → clean markdown | post-SSR |
| JSON-LD (schema.org) in pages | per-route `agent.schema` | later |

Routes opt in to richer description with one export (all fields optional; `crawl` defaults true for GET pages):

```ts
export const agent = {
  title: "Product catalog",
  description: "Browse and compare products.",
  crawl: true,
};
```

The implemented route-action slice lets a route export `agent.actions: [defineAction(...)]`; the same route can receive a human HTML form post, appear in live `/mcp tools/list`, dispatch through `/mcp tools/call` with journaled execution, confirmation checks, and idempotency keys, and publish the same action metadata through runtime `/openapi.json`, `/llms.txt`, and `/.well-known/beater.json`.

## 7. Developer experience

```
my-app/
├── beater.toml                        # [app] port; [python] venv = ".venv"
├── app/routes/
│   ├── index.tsx                      # export default → GET /          (SSR)
│   ├── users/[id].tsx                 # dynamic segment
│   └── api/health.ts                  # export GET/POST → /api/health
└── agents/support/
    ├── agent.ts                       # defineAgent({system, tools: [...]})
    └── tools/summarize_numbers.py     # def run(input) -> dict; TOOL = {...}
```

CLI: `beater new <app>` · `beater dev` · `beater build` · `beater agent run <name> "<prompt>"` · `beater agent resume <run_id>` · `beater agent runs` · `beater doctor`.

## 8. Not yet (each with its future path)

- **Full npm ecosystem / node-compat** — server routes can import local ESM packages from `node_modules` with bare specifiers; CommonJS `require`, Node built-ins, package install/build hooks, import maps, and client-side dependency bundling remain.
- **WHATWG fetch classes in routes** — comes with broader npm-compat.
- **Full RSC** — the chunked isolate→host streaming plumbing, route-scoped client modules, and initial `text/x-component` flight transport are the substrate; add official React Flight client references/manifests after broader npm-compat.
- **Wasmtime sandbox expansion** — local `wasmtime` tools now run hermetic scalar wasm with empty imports; broader WASI/capability handles for files, sockets, and richer value passing remain future work.
- **C++ tools** — `cpp_double` proves the `cxx` bridge on the Rust built-in path; richer C++ tool packaging remains future work.
- **Production agentic browsing** — the registry has a mock CDP provider for contract tests and a native `playwright` provider backed by the upstream Beater browser crates; `scripts/playwright-browser-gate.cjs` proves authenticated real Chromium calls through the agent loop reuse one run-scoped session, scoped env secrets are redacted from results, and resume cleans stale browser runner markers before review/replay.
- **defineAction runtime binding** — route modules can now export `agent.actions: [defineAction(...)]`; the hello form posts to the route handler and the same action is exposed through live MCP `tools/list` + journaled `tools/call`, runtime `/openapi.json`, `/llms.txt`, and `/.well-known/beater.json`. `beater-connect` still covers static action-doc generation; richer production API docs remain future work.
- **Deploy** — first slice exists: `beater build --out <dir>` emits a runnable host-platform bundle with copied app assets, the current binary, a launcher, a manifest, and a non-root Docker context while excluding runtime state and common local credential files. `scripts/docker-cold-start-gate.sh` builds the Linux release CLI, generates the bundle, builds the generated Dockerfile, starts the image with loopback-only publishing, verifies `/api/health`, and proves `/mcp` rejects unauthenticated calls while accepting bearer-token `tools/list`.
- **Isolate pool production hardening / per-request isolation** — `[app].workers = N` starts N route isolates; smoke tests prove round-robin dispatch, and `scripts/isolate-pool-scaling-gate.cjs` proved 7.65x route throughput on ten local workers. Per-request isolation hardening and worker-count tuning remain production work.
- **LLM streaming to browser** — provider-adapted LLM streaming, partial-step journal records, protected run list/detail/events endpoints under `/_beater/agent/runs`, and the hello example's recent-run EventSource panel are in place; richer production run-management UI remains.
- **MCP discovery/SSE + the 2026-07-28 spec** — remote MCP `tools/call`, provider session initialization, and startup `tools/list` schema import via `remoteMcpProvider` are in place; adopt the next transport spec when released.
- **Observability/evals** — opt-in trace exporters post finished journal runs and steps to Beater native `/v1/traces/native` or OTLP/HTTP `/v1/traces`; local gates prove OTLP collector export and Beater native ingest/read paths used by the dashboard, while rendered beater-agents dashboard proof remains future work.

## 9. Milestones

| # | Slice | Proves | Status |
|---|---|---|---|
| M0 | scaffold, pinned deps, this doc | — | **done** |
| M1 | `beater dev`: TS route in embedded V8, source-mapped errors, hot reload | the runtime | **done** |
| M2 | durable agent loop + Python tool + kill-9 resume | **the thesis** | code done; live-API gate pending |
| M3 | `/mcp` endpoint (inspector-verified) + crawl layer (robots/sitemap/llms.txt/.well-known) | ecosystem | **done** |
| M4 | React streaming SSR (`renderToReadableStream`) | the web half | **done** |
| M5 | route-scoped client module (`/_beater/client/<route>.js`) + hydrated counter | interactivity | **done** |
| M6 | route-scoped RSC transport (`/_beater/rsc/<route>.flight`) + browser-rendered server island | RSC substrate | transport done; official React Flight manifests pending |
| M7 | bare ESM package imports from local `node_modules` in server routes | adoption wedge | **done** |
| M8 | `beater build` host bundle + Docker context | deploy substrate | **done:** CI passed the Docker cold-start gate with the generated Linux image serving `/api/health` in 413ms and enforcing MCP bearer auth |
