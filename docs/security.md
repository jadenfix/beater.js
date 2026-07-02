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

## Python Tools

Python tools are first-party code and run with the same OS privileges as the beater process. They can read files, open sockets, import packages from the configured venv, and mutate external systems.

Do not run untrusted Python tools. The planned Wasmtime tier is the sandbox path for untrusted or agent-generated code.

## Journal Data

The journal is plaintext SQLite under `<app>/.beater/journal.db`. It stores prompts, model responses, tool inputs, tool outputs, run status, and step attempts.

Operational implications:

- do not commit `.beater/`
- treat journal backups as sensitive
- redact secrets before passing them to agents or tools
- add redaction hooks before production deployments that handle private data

## Remote Integrations

Networked tools have explicit timeouts, retry policy, idempotency keys, secret handling, and egress allowlists. Remote MCP tools read bearer tokens from environment variables, require HTTPS for bearer auth except loopback test servers, fail before connecting when a required secret is missing, and never follow redirects. Transient failures retry only when the tool is idempotent or a configured `tool_use_id` idempotency key is available; ambiguous non-idempotent failures park the run as `needs_review`. SaaS API integrations and browser automation sessions should follow the same registry path so calls are journaled before side effects happen.

## Agentic Browsing

Browser automation is powerful enough to read authenticated pages and perform destructive actions. The current `mock_cdp` provider is only a deterministic contract and lifecycle test provider. Production Playwright/CDP providers must include real session cleanup across crashes, scoped credentials, and browser e2e tests before they are considered production-ready.
