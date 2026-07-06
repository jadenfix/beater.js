# beater.js

**One runtime for the agent-first web.** A single Rust binary that serves your web app, runs your agents durably, and executes TypeScript, Python, and native Rust side by side.

```
app/routes/index.tsx          → streamed React SSR
app/routes/api/health.ts      → HTTP handler in embedded V8
agents/support/agent.ts       → durable agent loop (runs in Rust, survives crashes)
agents/support/tools/*.py     → full-fat Python tools (numpy/torch work) in embedded CPython
```

Why: the Node/Next stack was designed for documents and forms. Agent apps are long-running polyglot loops — the unit of work is a journaled, resumable run, not a request; the ML half lives in Python and native code, not JS. beater.js is one Rust host process with four execution tiers: **V8** (routes, SSR), **CPython** (ML tools), **native Rust** (the agent loop itself), and **Wasmtime** (hermetic W0 sandboxed untrusted code). Tools speak [MCP](https://modelcontextprotocol.io) natively.

Read the full design: [ARCHITECTURE.md](./ARCHITECTURE.md)

## Status

Pre-alpha, built in the open. Current milestone progress:

- [x] **M0** — scaffold, pinned deps, architecture contract
- [x] **M1** — `beater dev`: TS routes in embedded V8, source-mapped errors, hot reload
- [ ] **M2** — durable agent loop + embedded-Python tools + step-lifecycle journal (code complete; live-API kill-9/resume gate pending a funded `ANTHROPIC_API_KEY`)
- [x] **M3** — MCP server endpoint (spec 2025-11-25, verified with the official MCP inspector) + agent-ready crawl layer (robots.txt, sitemap.xml, llms.txt, .well-known manifest — auto-generated from the route table)
- [x] **M4** — streamed React 19 SSR (`renderToReadableStream`; shell chunks flush before Suspense-delayed subtrees)
- [x] **M5** — route-scoped client module (`/_beater/client/index.js`) hydrates a counter on the hello route
- [x] **M6** — route-scoped RSC transport (`/_beater/rsc/index.flight`) streams server islands to the browser
- [x] **M7** — server routes can import local ESM packages from `node_modules` with bare specifiers
- [x] **M8** — `beater build` deploy story (runnable host bundle + Docker cold-start gate)

## Quickstart (target DX)

```sh
export PYO3_PYTHON=$(command -v python3.11)
cargo build --workspace

./target/debug/beater new my-app                         # scaffold from the hello template
python3.11 -m venv my-app/.venv                          # optional: enables third-party Python packages
./target/debug/beater dev my-app                         # serve routes with hot reload
BEATER_MCP_TOKEN=dev-token ./target/debug/beater dev my-app --host 0.0.0.0 --base-url https://hello.example.com
export ANTHROPIC_API_KEY=sk-ant-...                     # default Anthropic provider for live agent runs
./target/debug/beater agent run --app my-app support "summarize 3,1,4,1,5"
./target/debug/beater agent resume --app my-app <run_id>
./target/debug/beater doctor my-app                      # verify Python/venv/V8 wiring
./target/debug/beater build my-app --out /tmp/my-app-bundle
BEATER_HOST=127.0.0.1 BEATER_PORT=3000 /tmp/my-app-bundle/run.sh
docker build -t my-app-beater /tmp/my-app-bundle
docker run --rm -e BEATER_MCP_TOKEN=dev-token -p 127.0.0.1:3000:3000 my-app-beater
```

Set `BEATER_TRACE_EXPORT_URL` to export finished agent runs to Beater's native `/v1/traces/native` ingest endpoint, or `BEATER_OTLP_EXPORT_URL`/`OTEL_EXPORTER_OTLP_*` for OTLP/HTTP `/v1/traces`. See [Observability](docs/observability.md) for the full environment contract and the local OTLP/Beater dashboard-read gates.

When exposing `/mcp` beyond localhost, require a bearer token and add browser origins explicitly:

```sh
export BEATER_MCP_TOKEN="$(openssl rand -hex 32)"
export BEATER_MCP_TRUSTED_ORIGINS="https://ops.example.com" # browser-based operators only
./target/debug/beater dev my-app --host 0.0.0.0 --base-url https://hello.example.com
```

## Current limits

`beater dev` defaults to one JS route isolate, so TS routes and React SSR serialize unless you set `[app].workers = N` in `beater.toml`. One dev server serves one app directory. See [Runtime limits](docs/runtime-limits.md) for the exact concurrency model and scaling gate.

Server-side routes can import local ESM packages from `node_modules` with bare package specifiers. The resolver handles exact and wildcard package `exports` entries, array export targets, `node`, `import`, `module`, and `default` conditions, plus `module`/`main` fallbacks; CommonJS `require`, Node built-ins, and client-side dependency bundling are still outside this wedge.

Client modules are route companions such as `app/routes/index.client.ts`. They are transpiled and served as same-origin browser modules, but they are not bundled with npm dependencies yet.

RSC transport is starting as route companions such as `app/routes/index.server.tsx`, streamed from `/_beater/rsc/index.flight` with `text/x-component` frames over the same isolate-to-host stream channel. This is the transport wedge; full React Flight client runtime and client-reference manifests are still Phase C work.

`beater build` currently emits a host-platform bundle: copied app assets, the current `beater` binary, `run.sh`, `beater-build.json`, `.dockerignore`, and a Dockerfile that runs as a non-root `beater` user. Runtime state and common local credential files are excluded. The bundle launcher is tested by starting it and hitting `/api/health`, and CI runs a Linux Docker cold-start gate that builds an image, starts the container, verifies `/api/health`, and proves `/mcp` rejects unauthenticated requests while accepting a bearer token.

## Docker deploy gate

The end-to-end deploy proof lives in `scripts/docker-cold-start-gate.sh`. It builds the release CLI inside a Linux Docker builder, runs `beater build` against `examples/hello`, builds the generated Dockerfile, starts the image on a loopback-only published port, waits for `/api/health`, and checks authenticated MCP tool discovery.

Useful knobs:

- `BEATER_DOCKER_RUST_IMAGE=rust:1-bookworm` chooses the Linux builder image.
- `BEATER_DOCKER_COLD_START_MS=1000` sets the health deadline measured from `docker run`; CI uses `3000` to avoid runner-scheduling flakes while still proving a cold container boot.
- `BEATER_DOCKER_GATE_WORKDIR=/path/to/workspace` uses an existing workspace for build artifacts, logs, and `evidence.md`.
- `BEATER_DOCKER_KEEP=1` keeps the target cache and generated image after a successful run.
- `BEATER_DOCKER_IMAGE=name:tag` uses an explicit image tag and leaves that tag in place.
- `BEATER_DOCKER_MIN_FREE_KIB=12582912` sets the free-space preflight for the Linux release build.

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

## LLM providers

Agents default to Anthropic:

```ts
export default defineAgent({
  name: "support",
  provider: "anthropic",
  model: "claude-opus-4-8",
});
```

The runner keeps one canonical journal/tool shape and adapts at the network boundary. Anthropic uses `ANTHROPIC_API_KEY` plus optional `ANTHROPIC_BASE_URL`; custom Anthropic HTTPS origins require `BEATER_ANTHROPIC_ALLOW_CUSTOM_BASE_URL=1`, and HTTP loopback mocks require `BEATER_ANTHROPIC_ALLOW_INSECURE_LOOPBACK=1`. OpenAI-compatible chat-completions providers use:

```sh
export BEATER_LLM_PROVIDER=openai-compatible
export BEATER_LLM_MODEL=z-ai/glm-5.2
export BEATER_OPENAI_BASE_URL=https://integrate.api.nvidia.com/v1
export BEATER_OPENAI_ALLOW_CUSTOM_BASE_URL=1
export BEATER_OPENAI_API_KEY=...
```

`BEATER_LLM_PROVIDER` and `BEATER_LLM_MODEL` override `agent.ts` for smoke tests and deployments. `BEATER_OPENAI_*` take precedence over `OPENAI_API_KEY` and `OPENAI_BASE_URL`.

More docs:

- [Tool contract](docs/tools.md)
- [Integration registry](docs/integrations.md)
- [Observability](docs/observability.md)
- [Runtime limits](docs/runtime-limits.md)
- [Security and trust model](docs/security.md)
- [Changelog and versioning policy](CHANGELOG.md)

## License

Apache-2.0

## Ecosystem

beater.js is part of the [ecosystem](https://github.com/jadenfix/ecosystem) — a family of Rust-first, local-first agent-infrastructure projects. It is fully standalone: one Rust binary that serves your app and runs durable polyglot agents, with no sibling project required. Within the family it can connect for:

- feeding its journaled runs to [beater-memory](https://github.com/jadenfix/beater-memory) (journal import exists today) and its traces to [beater](https://github.com/jadenfix/beater) for evals and CI gates
- using the local Wasmtime tier for hermetic untrusted scalar wasm tools, with [beatbox](https://github.com/jadenfix/beatbox) still available as the remote sandbox lane
- giving its agents web hands via [tempo](https://github.com/jadenfix/tempo) and running under [beaterOS](https://github.com/jadenfix/beaterOS) authority and policy
