# Security And Trust Model

beater.js is currently a local-first development runtime. Binding beyond localhost is useful for containers and remote testing, but it changes the threat model.

## Network Exposure

`beater dev` defaults to `127.0.0.1`. Use `--host 0.0.0.0` only inside a trusted network, container boundary, or behind a TLS-terminating reverse proxy.

Current `/mcp` behavior:

- no authentication by default for localhost development
- bearer-token authentication when `BEATER_MCP_TOKEN` is set; clients must send `Authorization: Bearer <token>`
- JSON-RPC over `POST /mcp`
- `Origin` allow-list to reduce browser DNS-rebinding risk:
  - clients without an `Origin` header are allowed, which covers curl, MCP inspectors, and server-side remote managers
  - browser origins are allowed only when they are loopback (`localhost` or loopback IPs) or exactly listed in `BEATER_MCP_TRUSTED_ORIGINS`
  - browser preflight requests to `OPTIONS /mcp` succeed only for allowed origins and return CORS headers for `Authorization` + JSON requests
- no server-initiated SSE stream

Before exposing `/mcp` to a remote manager or another machine:

```sh
export BEATER_MCP_TOKEN="$(openssl rand -hex 32)"
export BEATER_MCP_TRUSTED_ORIGINS="https://ops.example.com" # only needed for browser-based operators
./target/debug/beater dev examples/hello --host 0.0.0.0 --base-url https://hello.example.com
```

Do not put MCP bearer tokens in `beater.toml`. Keep them in the process environment or the deployment secret manager. Set `--base-url`, `BEATER_BASE_URL`, or `[app] base_url` so generated manifests advertise the externally reachable URL instead of a bind address such as `0.0.0.0`. The dev smoke tests cover the remote-management path: missing tokens fail with 401, valid bearer tokens succeed, trusted browser-origin preflight and POST requests receive CORS headers, untrusted browser origins fail with 403, and the manifest uses the configured public URL.

## Browser Client Modules

Route-scoped client modules are public browser code. `/_beater/client/<route>.js` only serves the route companion entry and dependency IDs reachable from that entry's static import graph; query parameters are not decoded as filesystem paths. The graph resolver enforces app-root containment after symlink resolution, package-root containment for `node_modules`, and explicit graph size caps. Browser client graphs reject `.cjs`, `require()`, `node:` and bare Node built-ins, URL imports, dynamic `import()`, and unsupported module types instead of trying to emulate Node in the browser.

## Python Tools

Python tools are first-party code and run with the same OS privileges as the beater process. They can read files, open sockets, import packages from the configured venv, and mutate external systems.

Do not run untrusted Python tools. Use `wasmtimeTool` for the current untrusted-code path: it runs hermetic scalar wasm with an empty linker, denied host imports, no filesystem mounts, no network, no environment variables, no secrets, and fuel/memory/wall-clock limits.

## Journal Data

The journal is plaintext SQLite under `<app>/.beater/journal.db`. It stores prompts, model responses, LLM stream partials, tool inputs, tool outputs, run status, and step attempts.

Operational implications:

- do not commit `.beater/`
- treat journal backups as sensitive
- redact secrets before passing them to agents or tools
- add redaction hooks before production deployments that handle private data

LLM provider API keys are read from the environment and must not be stored in `agent.ts` or `beater.toml`. Anthropic uses `ANTHROPIC_API_KEY`; custom Anthropic HTTPS origins require `BEATER_ANTHROPIC_ALLOW_CUSTOM_BASE_URL=1`, and insecure Anthropic HTTP is accepted only for loopback fixtures with `BEATER_ANTHROPIC_ALLOW_INSECURE_LOOPBACK=1`. OpenAI-compatible providers use `BEATER_OPENAI_API_KEY` or `OPENAI_API_KEY`; custom HTTPS origins require `BEATER_OPENAI_ALLOW_CUSTOM_BASE_URL=1`, and insecure HTTP is accepted only for loopback fixtures with `BEATER_OPENAI_ALLOW_INSECURE_LOOPBACK=1`. Secret-bearing LLM clients disable redirects and proxy routing, and provider error bodies are omitted from agent errors so a misconfigured endpoint cannot echo secrets into the terminal or journal.

The optional live provider gates, `scripts/llm-live-provider-smoke.cjs` and `scripts/m2-live-gate.sh`, read the same environment keys and write logs plus `evidence.md` under `.beater/`. They must not be run with keys pasted into command lines or committed files. The gates record provider name, model, base URL, run id, journal shape, and tool payload or crash/resume evidence; they do not record request headers, raw key values, or provider error bodies.

The optional trace exporters read from the same journal data and post prompts, tool inputs, model responses, and tool outputs to the configured Beater native or OTLP ingest endpoint. Treat `BEATER_TRACE_EXPORT_URL`, `BEATER_OTLP_EXPORT_URL`, and `OTEL_EXPORTER_OTLP_*` as sensitive deployment decisions, set `BEATER_API_KEY` or OTLP headers through the environment or secret manager when required, and do not enable export for private data until redaction policy is in place.

## Remote Integrations

Networked tools have explicit timeouts, retry policy, idempotency keys, secret handling, and egress allowlists. Remote MCP tools read bearer tokens from environment variables, require HTTPS for bearer auth except loopback test servers, fail before connecting when a required secret is missing, and never follow redirects. Transient failures retry only when the tool is idempotent or a configured `tool_use_id` idempotency key is available; ambiguous non-idempotent failures park the run as `needs_review`. SaaS API integrations and browser automation sessions should follow the same registry path so calls are journaled before side effects happen.

`GET /_beater/agent/runs`, `GET /_beater/agent/runs/<run_id>`, and `GET /_beater/agent/runs/<run_id>/events` expose journaled run history and LLM partials for browser run UIs. They contain the same sensitive content as the journal and reuse the MCP origin and bearer-token policy.

## Agentic Browsing

Browser automation is powerful enough to read authenticated pages and perform destructive actions. The `mock_cdp` provider is only a deterministic contract and lifecycle test provider. The `playwright` provider launches a real Chromium session through the upstream Beater browser driver, reuses it within the same run, writes app-scoped runner markers for resume cleanup, and can resolve env-backed `textSecret` values for `type` actions while redacting result payloads. Richer credential modes such as cookies or extra HTTP headers must stay scoped to the provider/session when added.
