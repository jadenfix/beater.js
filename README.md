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
- [x] **M4** — server-rendered React 19 (renderToString; streaming SSR is the upgrade path)

## Quickstart (target DX)

```sh
beater dev examples/hello                 # serve routes with hot reload
beater dev examples/hello --host 0.0.0.0  # bind for containers/VMs
beater agent run support "summarize 3,1,4,1,5"
beater agent resume <run_id>              # crash-safe: picks up mid-loop
beater doctor                             # verify Python/venv/V8 wiring
```

## Build from source

```sh
cargo build --workspace      # first build downloads a prebuilt V8; takes a while
```

Requires: Rust (pinned via rust-toolchain.toml) and CPython with a shared library for the embedded interpreter. If your Python is not the local default in `.cargo/config.toml`, set `PYO3_PYTHON=$(which python3.11)` before building.

Agent tests and local mock runs can point at a non-Anthropic endpoint with `ANTHROPIC_BASE_URL`; production runs still require `ANTHROPIC_API_KEY`.

## License

Apache-2.0
