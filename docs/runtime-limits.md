# Runtime Limits

beater.js is pre-alpha. The current runtime is optimized for a correct vertical slice, not production throughput.

## Current Concurrency Model

`beater dev` accepts concurrent HTTP connections through axum. By default it runs one V8 worker isolate, but `[app].workers = N` can start a small pool and round-robin route work across those isolates. When `workers = 1`, route handlers and React SSR render work are serialized.

What is concurrent today:

- the Rust HTTP server can accept multiple client connections
- non-JS control surfaces such as simple error responses do not need the route isolate
- Python tool execution is offloaded away from the async runtime
- agent run durability lives in Rust and is not reset by JS hot reload

What is not concurrent today:

- with `workers = 1`, multiple JS/TS route handlers do not execute in parallel
- with `workers = 1`, multiple React SSR renders do not execute in parallel
- one `beater dev` process serves one app directory
- hot reload swaps the active worker set rather than preserving in-flight work across versions

## Operational Guidance

For v0.1, treat `beater dev` as a local development server and remote-management test target, not a production load target.

- Do not use current request throughput as a benchmark for the future runtime.
- Avoid long-running route handlers; put durable external work in tools or agent runs where it can be journaled.
- Run separate beater processes for separate apps.
- When binding beyond localhost, still use `BEATER_MCP_TOKEN`, trusted origins, and `--base-url` as documented in `docs/security.md`.

## Planned Fix

The channel protocol between the host and the JS worker is already shaped for an isolate pool. The production path is:

- start N V8 worker threads per app
- dispatch route requests across the pool
- keep hot reload correct by swapping the whole pool
- preserve route metadata extraction for crawl surfaces
- prove scaling with a load test that shows near-linear improvement up to core count

The Phase C acceptance criterion remains: `wrk` or an equivalent load test shows near-linear route throughput scaling to core count.
