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
| 4 | **Wasmtime** *(future)* | untrusted / agent-generated code | capability-scoped sandbox |

The agent loop deliberately lives in tier 3, not tier 1: it survives hot reloads of user code, it is journaled by construction, and it cannot be starved by user JS.

## 3. Primitives

- **Routes** — files under `app/routes/`. `api/*.ts` export HTTP method handlers; `*.tsx` export React components (streamed SSR). File path = URL path; `[param]` segments are dynamic.
- **Agents** — directories under `agents/<name>/`. `agent.ts` declares system prompt, model, and tools. The framework extracts this config once per (re)load; the loop runs in Rust.
- **Tools** — three kinds, one registry: `pyTool(name, "./tools/x.py")` (tier 2), `rustTool(name)` (tier 3 built-ins), inline TS functions (tier 1, called back into the isolate). Every tool declares `input_schema` and an `idempotent` flag (see §5). The registry is exposed over MCP (§6).
- **Runs** — every agent invocation is a run with a journal. Runs are resumable (`beater agent resume <id>`) and inspectable (`beater agent runs`).

## 4. Design decisions

| Decision | Pick | Why |
|---|---|---|
| JS engine | `deno_core` =0.406.0 (pinned exact) | ops, ESM loading, snapshots, tokio integration; raw rusty_v8 would mean rebuilding all of that. deno_core is **not Deno**: no web APIs are included, we add only what we need |
| Web APIs in isolate | minimal shims: console, timers, TextEncoder | route contract is plain objects, not WHATWG fetch classes (that fidelity comes with the npm-compat era). Full streams needed only for SSR |
| TS/TSX | `deno_ast` (SWC) transpile in the module loader, source maps wired into error stacks | useful errors are an acceptance criterion, not polish |
| HTTP | axum 0.8; response bodies stream from the isolate over an mpsc channel | |
| Threading | `JsRuntime` is `!Send` → one dedicated OS thread (current-thread tokio rt); host↔worker via mpsc | single isolate for now; the channel protocol is already pool-shaped |
| Hot reload | `notify` watcher → drop worker thread → fresh isolate (~50–200ms) | trivially correct; agent runs are unaffected (loop lives in Rust) |
| Python | pyo3 0.29 `auto-initialize` (Py_InitializeEx(0) — no Python signal handlers); one interpreter; every call via `spawn_blocking` + semaphore | GIL never touches the async runtime. Venv: **build-time** linking is `PYO3_PYTHON`; **runtime** packages come from `site.addsitedir(<venv>/site-packages)` with a version-match check (`beater doctor`) |
| LLM | reqwest → Anthropic Messages API (`claude-opus-4-8`, adaptive thinking); non-streaming per step; loop on `stop_reason == "tool_use"` | each request is one journaled step |
| Durability | rusqlite (bundled), step-lifecycle journal (§5); append committed **before** every side effect | crash-kill-9 between any two steps loses nothing |
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
- `GET /mcp` → 405 (we offer no server-initiated SSE stream — explicitly allowed by spec)
- stateless: no `MCP-Session-Id` issued

Consuming remote MCP servers as tool sources is a planned follow-up (the registry is impl-agnostic).

## 6b. The Agent Access Layer (agent-ready sites by default)

There are two distinct problems in making a site agent-friendly, and MCP only solves the second:

1. **Crawl layer** — *can an agent understand what's on this site?* This is plain web primitives: robots.txt, sitemap.xml, llms.txt, clean markdown views, JSON-LD.
2. **Action layer** — *can an agent safely do things?* This is MCP (§6), with auth, scopes, and idempotency.

Every other framework treats the crawl layer as hand-maintained files or plugins. beater generates it, because the framework already owns the two sources of truth it derives from: the **route table** and the **tool/agent registry**. Zero-config outputs:

| Endpoint | Derived from | Milestone |
|---|---|---|
| `/robots.txt` | crawl policy + sitemap pointer | M3 |
| `/sitemap.xml` | route table (lastmod = file mtime) | M3 |
| `/llms.txt` | route table + per-route `agent` metadata | M3 |
| `/.well-known/beater.json` | manifest: MCP endpoint, sitemap, llms.txt, auth requirements | M3 |
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

The end state (post-MVP): a single `defineAction({name, input, auth, confirm, handler})` on a route exposes the same action to humans (HTML form), agents (MCP tool), APIs (OpenAPI), and crawlers (metadata) — with dry-run previews, idempotency keys, and human confirmation for destructive scopes. The journal (§5) already gives every agent-initiated action an audit trail.

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

CLI: `beater dev` · `beater agent run <name> "<prompt>"` · `beater agent resume <run_id>` · `beater agent runs` · `beater doctor`.

## 8. Not yet (each with its future path)

- **npm ecosystem / node-compat** — the adoption wedge; adopt a Deno-style compat layer rather than reimplementing.
- **WHATWG fetch classes in routes** — comes with npm-compat.
- **RSC + client hydration** — the chunked isolate→host streaming plumbing is the substrate; add the flight protocol + client bundle step after SSR lands.
- **Wasmtime sandbox** — fourth `impl` kind in the tool registry, for untrusted/agent-generated code.
- **C++ tools** — via `cxx` on the Rust built-in path when a real use case appears.
- **Agentic browsing** — reuse beater-agents' CDP/Playwright crates as a tool provider.
- **Deploy** — the host is one binary + assets; `beater build` → container image with the venv baked in.
- **Isolate pool / per-request isolation** — channel protocol already supports N workers.
- **LLM streaming (SSE to browser)** — journal needs partial-step records first.
- **MCP sessions/SSE + the 2026-07-28 spec** — adopt when released.
- **Observability/evals** — integrate beater-agents (OTLP out of the agent loop) rather than rebuilding.

## 9. Milestones

| # | Slice | Proves | Status |
|---|---|---|---|
| M0 | scaffold, pinned deps, this doc | — | **done** |
| M1 | `beater dev`: TS route in embedded V8, source-mapped errors, hot reload | the runtime | **done** |
| M2 | durable agent loop + Python tool + kill-9 resume | **the thesis** | code done; live-API gate pending |
| M3 | `/mcp` endpoint (inspector-verified) + crawl layer (robots/sitemap/llms.txt/.well-known) | ecosystem | **done** |
| M4 | React SSR (renderToString; streaming is the upgrade path) | the web half | **done** |
