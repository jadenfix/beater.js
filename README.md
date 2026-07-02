# beater.js

**One runtime for the agent-first web.** A single Rust binary that serves your web app, runs your agents durably, and executes TypeScript, Python, and native Rust side by side.

```
app/routes/index.tsx          → streamed React SSR
app/routes/api/health.ts      → HTTP handler in embedded V8
agents/support/agent.ts       → durable agent loop (runs in Rust, survives crashes)
agents/support/tools/*.py     → full-fat Python tools (numpy/torch work) in embedded CPython
```

Why: the Node/Next stack was designed for documents and forms. Agent apps are long-running polyglot loops — the unit of work is a journaled, resumable run, not a request; the ML half lives in Python and native code, not JS. beater.js is one Rust host process with four execution tiers: **V8** (routes, SSR), **CPython** (ML tools), **native Rust** (the agent loop itself), and **Wasmtime** (sandboxed untrusted code, planned). Tools speak [MCP](https://modelcontextprotocol.io) natively.

Read the full design: [ARCHITECTURE.md](./ARCHITECTURE.md)

## Status

Pre-alpha, built in the open. Current milestone progress:

- [x] **M0** — scaffold, pinned deps, architecture contract
- [x] **M1** — `beater dev`: TS routes in embedded V8, source-mapped errors, hot reload
- [x] **M2** — durable agent loop + embedded-Python tools + step-lifecycle journal (code complete; live-API kill-9/resume gate pending an `ANTHROPIC_API_KEY`)
- [x] **M3** — MCP server endpoint (spec 2025-11-25, verified with the official MCP inspector) + agent-ready crawl layer (robots.txt, sitemap.xml, llms.txt, .well-known manifest — auto-generated from the route table)
- [x] **M4** — streamed React 19 SSR (`renderToReadableStream`; shell chunks flush before Suspense-delayed subtrees)
- [x] **M5** — route-scoped client module (`/_beater/client/index.js`) hydrates a counter on the hello route

## Quickstart (target DX)

```sh
export PYO3_PYTHON=$(command -v python3.11)
cargo build --workspace

./target/debug/beater new my-app                         # scaffold from the hello template
python3.11 -m venv my-app/.venv                          # optional: enables third-party Python packages
./target/debug/beater dev my-app                         # serve routes with hot reload
./target/debug/beater dev my-app --host 0.0.0.0          # bind for containers/VMs
export ANTHROPIC_API_KEY=sk-ant-...                     # required for live agent runs
./target/debug/beater agent run --app my-app support "summarize 3,1,4,1,5"
./target/debug/beater agent resume --app my-app <run_id>
./target/debug/beater doctor my-app                      # verify Python/venv/V8 wiring
```

When exposing `/mcp` beyond localhost, require a bearer token and add browser origins explicitly:

```sh
export BEATER_MCP_TOKEN="$(openssl rand -hex 32)"
export BEATER_MCP_TRUSTED_ORIGINS="https://ops.example.com" # browser-based operators only
./target/debug/beater dev my-app --host 0.0.0.0 --base-url https://hello.example.com
```

## Current limits

`beater dev` currently uses one JS route isolate, so TS routes and React SSR serialize through that worker. One dev server serves one app directory. See [Runtime limits](docs/runtime-limits.md) for the exact concurrency model and isolate-pool path.

Client modules are route companions such as `app/routes/index.client.ts`. They are transpiled and served as same-origin browser modules, but they are not bundled with npm dependencies yet; that remains the Phase C npm/node-compat item.

RSC transport is starting as route companions such as `app/routes/index.server.tsx`, streamed from `/_beater/rsc/index.flight` with `text/x-component` frames over the same isolate-to-host stream channel. This is the transport wedge; full React Flight client runtime, client-reference manifests, and npm package adoption are still Phase C work.

## Build from source

```sh
cargo build --workspace      # first build downloads a prebuilt V8; takes a while
```

Requires: Rust (pinned via rust-toolchain.toml) and CPython 3.11+ with a shared library for the embedded interpreter. Set `PYO3_PYTHON` before building so PyO3 links the intended interpreter:

```sh
# macOS with Homebrew
brew install python@3.11
export PYO3_PYTHON="$(brew --prefix python@3.11)/bin/python3.11"

# Linux
export PYO3_PYTHON="$(command -v python3.11)"
```

Agent tests and local mock runs can point at a non-Anthropic endpoint with `ANTHROPIC_BASE_URL`; production runs still require `ANTHROPIC_API_KEY`.

More docs:

- [Tool contract](docs/tools.md)
- [Integration registry](docs/integrations.md)
- [Runtime limits](docs/runtime-limits.md)
- [Security and trust model](docs/security.md)
- [Changelog and versioning policy](CHANGELOG.md)

## License

Apache-2.0
