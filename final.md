# final.md — What "done" means for beater.js

This is the honest completion contract: what is verified today, the exact gap between here and an end-to-end-done MVP, and the concrete checklist that would make the framework genuinely complete against its thesis (ARCHITECTURE.md §1). Three levels, in order:

1. **[A] MVP e2e-done** — the vertical slice fully proven (one remaining gate)
2. **[B] v0.1 release-done** — someone who isn't Jaden can use it
3. **[C] Thesis-done (1.0)** — a credible Node/Next alternative

---

## Working model: 2 goal-oriented agents

This file is also the coordination contract for finishing `beater.js` in parallel. Each agent should work in small, reviewable PRs; run the relevant tests before publishing; request or perform an independent review; merge only after the slice is verified; then update this file if the completion evidence changed.

### Agent 1 — MVP e2e gate owner

**Owner:** this Codex thread.

**Goal:** make [A] actually true: prove the M2 live gate end to end, record the evidence, and flip the docs from "pending live gate" to "done" only after A3-A5 pass.

**Primary PR sequence:**
- [ ] Add the slow-tool fixtures for A2 with the smallest possible example-app surface.
- [ ] Run and record A3 happy path with the live Anthropic API.
- [ ] Run and record A4 crash/resume idempotent proof.
- [ ] Run and record A5 non-idempotent `needs_review` proof.
- [ ] Update README.md, ARCHITECTURE.md, and this file with exact evidence.

**Likely touched files:** `examples/hello/agents/support/**`, `README.md`, `ARCHITECTURE.md`, `final.md`, and only agent/runtime code if the live gate exposes a real bug.

**Do not claim done unless:** transcripts exist, `beater agent runs` shows the expected terminal states, the journal query proves the resume invariant, and the branch has passed the relevant local checks.

### Agent 2 — v0.1 release-hardening owner

**Owner:** second goal-oriented agent started separately by Jaden.

**Goal:** make [B] shippable by removing author-machine assumptions and adding automated confidence that does not depend on the live Anthropic API.

**Primary PR sequence:**
- [ ] Add focused unit tests for router matching, journal lifecycle/resume invariants, and loader transpile-cache behavior.
- [ ] Add `ANTHROPIC_BASE_URL` support plus mocked journal-resume tests.
- [ ] Add the no-key integration test that spawns `beater dev` and checks `/api/health`, `/`, and `/mcp`.
- [ ] Add CI for fmt, clippy, and tests on macOS/Linux with rusty_v8 caching.
- [ ] Improve portability/docs: Python discovery guidance, host binding, quickstart, `docs/tools.md`, and security notes.

**Likely touched files:** `crates/**`, `.github/workflows/**`, docs under `docs/**`, `README.md`, `ARCHITECTURE.md`, and `final.md`.

**Do not claim done unless:** the tests prove the requirement they are attached to, CI or local equivalents are green, and any docs marked complete have been checked from a clean-user perspective.

### Coordination rules

- Branches: use `codex/agent1-<slice>` and `codex/agent2-<slice>` so PR ownership is obvious.
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
| MCP server (2025-11-25) | official MCP inspector completes initialize + tools/list + tools/call; bogus Origin → 403; GET → 405 |
| Python-over-MCP | `summarize_numbers` (a `.py` file) executes in embedded CPython when called by an external MCP client |
| Agent Access Layer | /robots.txt, /sitemap.xml, /llms.txt, /.well-known/beater.json generated from the route table; `export const agent = {crawl: false}` excludes a route from sitemap + llms.txt |
| Agent config pipeline | `agent.ts` (via `beater:agent` shim) evaluates in a one-shot isolate → JSON config → Rust registry; Python TOOL metadata loads through embedded CPython |
| Durability machinery (code) | SQLite journal with started/completed/failed lifecycle + attempts; resume logic for dangling LLM calls and idempotent-only tool re-runs; `needs_review` parking |

---

## [A] MVP e2e-done — ONE gate remains

**The M2 live gate is the only thing between here and "e2e done" for the MVP.** Everything below it in this section is already coded; it has never been exercised against the live Anthropic API.

### A1. Prerequisite: `ANTHROPIC_API_KEY` in the shell environment

The only external input needed. Install once:

```sh
echo 'export ANTHROPIC_API_KEY=sk-ant-...' >> ~/.zshenv
```

### A2. Test fixture: a slow tool (needed to kill -9 deterministically mid-tool)

Add `examples/hello/agents/support/tools/slow_summarize.py` — same as `summarize_numbers` but with `time.sleep(15)` inside `run()`, declared in `agent.ts` as `pyTool("slow_summarize", "./tools/slow_summarize.py", { idempotent: true })`. Add a second variant (or a flag) declared `{ idempotent: false }` for the needs_review test.

### A3. Gate 1 — happy path (TS agent → Rust loop → Python tool → LLM)

```sh
./target/debug/beater agent run --app examples/hello support "summarize 3,1,4,1,5"
```

**Pass:** transcript shows a `tool_use` for `summarize_numbers` → Python executes (mean 2.8) → final `end_turn` text; `beater agent runs` shows the run `completed`.

### A4. Gate 2 — crash + resume (THE thesis proof)

```sh
./target/debug/beater agent run --app examples/hello support "use slow_summarize on 3,1,4,1,5" &
sleep 8 && kill -9 %1        # mid-tool, after the started row is committed
./target/debug/beater agent runs --app examples/hello        # status: running (stale)
./target/debug/beater agent resume --app examples/hello <run_id>
sqlite3 examples/hello/.beater/journal.db 'SELECT seq,kind,status,attempt FROM steps WHERE run_id="<run_id>"'
```

**Pass:** resume completes the run; the journal shows the interrupted tool_call re-ran with `attempt=2` and **only** that step re-executed (no duplicate LLM calls before the crash point).

### A5. Gate 3 — non-idempotent safety

Same kill -9, but against the `idempotent: false` variant.

**Pass:** `beater agent resume` refuses to re-run the tool, prints the reason, and `beater agent runs` shows `needs_review`. Nothing executes twice.

### A6. Close-out

- [ ] Record the three gate transcripts in this file (or a `docs/m2-gate.md`)
- [ ] Flip M2 to **done** in README.md + ARCHITECTURE.md §9
- [ ] Delete the slow-tool fixtures or keep them under `examples/hello` as living tests

**When A3–A5 pass, the MVP is end-to-end done**: every claim in the manifesto's vertical slice is demonstrated, not asserted.

---

## [B] v0.1 release-done — usable by someone who isn't Jaden

The MVP proves the thesis on this machine. A release requires removing the machine- and author-specific assumptions:

### Correctness & tests (currently: zero automated tests)
- [ ] Unit tests: router matching (params, index, collisions), journal lifecycle (start/complete/fail/resume invariants), loader transpile cache behavior
- [ ] Integration test: spawn `beater dev`, curl /api/health + / + /mcp tools/call (no API key needed — this is exactly the M3 gate, automated)
- [ ] Journal resume tests with a mocked Anthropic endpoint (`ANTHROPIC_BASE_URL` override — **needs a small code change**: the API URL is currently a hardcoded const in `anthropic.rs`)
- [ ] CI: GitHub Actions running fmt + clippy + tests on macOS and Linux, with the rusty_v8 archive cached

### Portability (currently: works on this Mac)
- [ ] **Python discovery**: `.cargo/config.toml` hardcodes `PYO3_PYTHON=/opt/homebrew/bin/python3.11`. Replace with documented per-platform setup + a `beater doctor` check that explains mismatches. Linux build verified.
- [ ] **Concurrency**: one isolate = requests serialize; one dev server = one app. Either ship the isolate pool (the channel protocol is already pool-shaped) or document the limitation prominently.
- [ ] **Port/host binding**: 127.0.0.1 hardcoded; `--host` flag for containers.
- [ ] `beater new <app>` scaffolding command (copy of examples/hello) — the first-five-minutes experience.

### Security floor (currently: dev-mode assumptions everywhere)
- [ ] /mcp has no auth — fine on localhost, must be stated loudly + bearer-token option before anyone binds 0.0.0.0
- [ ] Python tools run with full process privileges — document the trust model (tools are first-party code until the wasm sandbox tier lands)
- [ ] Journal stores full prompts/results in plaintext SQLite — document; add redaction hooks later

### Docs
- [ ] README quickstart actually runnable start-to-finish by a stranger (install Rust, install Python 3.11+, cargo build, beater dev)
- [ ] `docs/tools.md`: the pyTool/rustTool contract (TOOL dict, run(), idempotency rules)
- [ ] CHANGELOG + versioning policy (deno_core pin-bump cadence)

---

## [C] Thesis-done (1.0) — the punt list, paid off

These are the items ARCHITECTURE.md §8 explicitly deferred, in dependency order. Each has a one-line acceptance criterion.

| # | Item | Done when |
|---|---|---|
| 1 | **Streaming SSR** — renderToReadableStream over the chunked worker channel (needs ReadableStream shim or deno_web) | `curl -N /` shows the shell chunk arrive before a Suspense-delayed subtree chunk |
| 2 | **Client hydration** — per-route client bundle (`/_beater/client.js`) | a counter button on index.tsx works in a browser |
| 3 | **RSC** — flight protocol over the same chunked channel | server components with client islands render + hydrate |
| 4 | **npm/node-compat** — the adoption wedge (Deno-style compat layer, not a reimplementation) | `import { z } from "zod"` works in a route |
| 5 | **Isolate pool** — N workers behind the existing channel protocol | wrk shows near-linear scaling to core count |
| 6 | **Wasm sandbox tier** — Wasmtime as the 4th tool impl kind | an untrusted tool runs capability-scoped and cannot read the filesystem |
| 7 | **LLM streaming** — SSE to browser + partial-step journal records | tokens stream to a page while every step stays crash-resumable |
| 8 | **MCP consume + sessions** — use remote MCP servers as tool sources; adopt the next MCP spec when released | an agent uses a third-party MCP server's tool via config only |
| 9 | **Agentic browsing** — reuse beater-agents' CDP/Playwright crates as a tool provider | an agent completes a real browsing task from a pyTool-style declaration |
| 10 | **defineAction** — one definition → HTML form + MCP tool + OpenAPI + crawler metadata (§6b end state) | a form posts for humans AND appears in tools/list with auth + confirm semantics |
| 11 | **Deploy story** — `beater build` → single container (binary + assets + venv) | `docker run` of the built image serves the app cold in <1s |
| 12 | **Observability** — OTLP out of the agent loop into beater-agents | a run's trace appears in the beater-agents dashboard |
| 13 | **Free-threaded Python** — pyo3 on 3.14t once ML wheels are reliable | two Python tools execute truly in parallel under load |
| 14 | **C++ tools** — via cxx on the Rust builtin path | a C++ function is callable as a tool with schema |

**Definition of thesis-done:** a team can build and deploy a production app where the web UI, the agents, and the ML tools live in one repo, run in one process, survive crashes mid-agent-loop, and are discoverable/callable by third-party AI agents — without Node, without a queue between the web and ML halves, and without a cloud lock-in. Items 1–5 + 11 are the minimum for that sentence to be true; 6–10 + 12–14 make it competitive.

---

## TL;DR

- **To be e2e done (MVP):** install `ANTHROPIC_API_KEY`, add one slow-tool fixture, run the three A3–A5 gates, flip the docs. Everything else is already built and verified.
- **To ship v0.1:** tests + CI, portable Python config, isolate-pool-or-documented-limits, `beater new`, honest security notes.
- **To kill Node/Next:** pay off punts 1–5 and 11 first; the rest is compounding advantage.
