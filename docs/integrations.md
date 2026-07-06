# Integration Registry

beater.js should expose one integration registry, not separate queues or sidecar services for web actions, local tools, remote MCP servers, and browser-control providers.

The implemented registry today supports first-party Python tools, Rust built-ins, hermetic local Wasmtime tools, declared remote MCP tools, remote MCP provider discovery, a mock CDP browser provider for deterministic agent-loop and lifecycle tests, and a Playwright provider backed by the upstream Beater browser crates. Richer production credential modes such as cookies or extra headers still need to stay scoped to the provider/session before they ship.

## Contract

Every integration exposed to an agent should have:

- `name`: stable, globally unique tool name within the app.
- `description`: human-readable capability summary for LLM/tool clients.
- `input_schema`: JSON Schema for validation and MCP/tool metadata.
- `kind`: implementation kind, such as `python`, `rust`, `wasmtime`, `remote_mcp`, or `browser`.
- `idempotent`: crash-resume safety signal used by the journal.
- `timeout`: maximum time for one call before it fails closed.
- `retry`: explicit retry policy for network failures, including whether retries use an idempotency key.
- `secrets`: named secret sources from environment or deployment secret managers, never literal values in config.
- `egress`: allowed remote hosts or provider names for networked tools.
- `audit`: what must be journaled before and after the call.

The execution path stays the same for every kind:

```text
agent.ts declaration
  -> Rust registry
  -> journal started step
  -> implementation executor
  -> journal completed/failed step
  -> model result or MCP response
```

Nothing agent-visible should bypass this path. If a tool can mutate external state, the started journal row must be committed before the side effect can happen.

## Current Kinds

### Beater Connect Actions

`beater-connect` is the static bridge for action definitions today. A single `Action` definition emits OpenAPI, MCP catalog metadata, crawl documents, and `forms.html`. The generated form posts to the action path and preserves the same action semantics as the MCP catalog with `data-auth`, `data-scopes`, `data-confirm`, `data-dry-run`, `data-side-effect`, and `data-idempotency-required` attributes.

```sh
beater-connect demo --out .agent
beater-connect print forms
beater-connect print mcp
```

Runtime route actions use `defineAction` in an API route's `agent.actions` metadata. The route remains a normal form target for humans, and the dev server exposes the same action through live `/mcp tools/list` and `/mcp tools/call` with journaled execution, confirmation checks, idempotency keys, runtime `/openapi.json`, `/llms.txt`, and `/.well-known/beater.json`.

### First-Party Python

Use `pyTool` for app-owned Python code:

```ts
pyTool("summarize_numbers", "./tools/summarize_numbers.py", {
  idempotent: true,
})
```

Python is appropriate for local ML, data processing, and first-party integration glue. It runs with the beater process privileges, so do not use it for untrusted code.

### First-Party Rust

Use `rustTool` for host built-ins:

```ts
rustTool("get_time")
rustTool("cpp_double")
```

Rust built-ins are appropriate for stable host capabilities, low-level system integration, and functionality that should ship inside the binary. `cpp_double` is the current C++ proof: it runs through `cxx` on the Rust built-in path, so it keeps the same registry schema and journal behavior as other host tools.

### Remote MCP Tools

Remote MCP providers are declared as registry-backed tools with scoped credentials, explicit egress, and mock-server coverage. Metadata is declared locally so agents and `/mcp tools/list` can expose a stable schema without making startup depend on a remote provider:

```ts
remoteMcpTool("linear.create_issue", {
  tool: "create_issue",
  endpoint: "https://mcp.linear.example/mcp",
  description: "Create a Linear issue.",
  inputSchema: {
    type: "object",
    properties: {title: {type: "string"}},
    required: ["title"],
  },
  auth: {type: "bearer", env: "LINEAR_MCP_TOKEN"},
  timeoutMs: 10_000,
  retry: {attempts: 2, backoffMs: 250, idempotencyKey: "tool_use_id"},
  session: {scope: "run", cleanup: "always"},
  egress: ["mcp.linear.example"],
  idempotent: false,
})
```

When startup should import the provider's catalog directly, use `remoteMcpProvider`. The registry sends `initialize`, then `tools/list`, and exposes each returned tool as `<prefix>.<provider tool name>` while execution still calls the original provider tool name:

```ts
remoteMcpProvider("linear", {
  endpoint: "https://mcp.linear.example/mcp",
  auth: {type: "bearer", env: "LINEAR_MCP_TOKEN"},
  timeoutMs: 10_000,
  retry: {attempts: 2, backoffMs: 250, idempotencyKey: "tool_use_id"},
  session: {scope: "run", cleanup: "always"},
  egress: ["mcp.linear.example"],
  idempotent: false,
})
```

Implemented behavior:

- calls are tested against a local mock MCP server
- provider discovery can import `tools/list` schemas at registry-build time with `remoteMcpProvider(prefix, ...)`
- bearer auth reads tokens from environment variables only; missing or empty secrets fail before any network connection
- bearer auth requires HTTPS for non-loopback endpoints
- endpoint hosts must match the declaration's `egress` allowlist
- HTTP redirects are not followed, so an allowed endpoint cannot redirect tool arguments or credentials to a host outside the egress policy
- HTTP timeouts fail closed
- transient server errors and rate limits retry only when the tool is idempotent or a configured `tool_use_id` idempotency key is available
- outbound MCP `tools/call` requests reuse `tool_use_id` as the JSON-RPC id and `Idempotency-Key` header when configured
- `session: {scope: "run", cleanup: "always"}` lazily sends `initialize`, stores the returned `Mcp-Session-Id` in memory for the tool, and sends it on later `tools/call` requests
- non-idempotent calls park as `needs_review` on crash-resume and after ambiguous network/provider failures

Planned next steps:

- resumable transport metadata beyond the current in-memory provider session id

### Browser Providers

Browser providers enter as tools, not as a separate automation service. The implemented `mock_cdp` provider is for deterministic tests of declaration shape, allowed origins, per-tool-call session cleanup, and agent-loop execution:

```ts
browserTool("checkout_flow", {
  provider: "mock_cdp",
  session: {scope: "run", cleanup: "always"},
  allowedOrigins: ["https://shop.example"],
  description: "Verify checkout in a browser.",
  inputSchema: {
    type: "object",
    properties: {url: {type: "string"}, task: {type: "string"}},
    required: ["url", "task"],
  },
  timeoutMs: 30_000,
  idempotent: false,
})
```

Implemented behavior:

- browser tools are declared through the same registry and exposed in agent tool metadata
- `allowedOrigins` blocks navigation outside the declared origins
- `session: {scope: "run", cleanup: "always"}` uses the journal run id as the session id and reuses the session across multiple browser calls in that run
- browser sessions are cleaned up when an agent run or synthetic MCP run reaches a terminal state
- app-scoped Playwright runs write per-session runner markers under `.beater/browser-sessions`; `beater agent resume` removes stale markers and terminates marked runners for the run before replay/review
- `provider: "playwright"` reuses the pinned upstream `beater-browser` / `beater-browser-playwright` crates and launches Chromium through the upstream Node runner
- the Playwright input path supports `input.url` plus one optional driver action such as `click`, `type`, `extract`, `wait`, `scroll`, `select`, or `goto`
- browser `secrets` support named env-backed sources; `type` actions can use `textSecret` to resolve the secret at execution time while journal/result action payloads stay redacted
- a mocked agent-loop test proves an agent can complete a browser task through a tool declaration
- `scripts/playwright-browser-gate.cjs` installs the upstream runner dependencies in a temp directory, runs a local authenticated browser fixture plus Anthropic-compatible SSE mock, and verifies completed `playwright` tool results reused one run-scoped session without leaking the password in the journal
- destructive actions require non-idempotent handling or explicit review semantics

Production Playwright/CDP release criteria:

- richer credential modes such as cookies or extra HTTP headers are scoped to the provider/session when added

## Coexistence

One agent config should be able to mix local and networked capabilities:

```ts
export default defineAgent({
  name: "operator",
  tools: [
    pyTool("score_lead", "./tools/score_lead.py", {idempotent: true}),
    rustTool("get_time"),
    remoteMcpTool("crm.update_contact", {
      tool: "update_contact",
      endpoint: "https://mcp.crm.example/mcp",
      description: "Update a CRM contact.",
      inputSchema: {type: "object", properties: {}, additionalProperties: true},
      auth: {type: "bearer", env: "CRM_MCP_TOKEN"},
      timeoutMs: 10_000,
      retry: {attempts: 1},
      egress: ["mcp.crm.example"],
      idempotent: false,
    }),
    browserTool("verify_checkout", {
      provider: "mock_cdp",
      session: {scope: "run", cleanup: "always"},
      allowedOrigins: ["https://store.example"],
      description: "Verify checkout in a browser.",
      inputSchema: {type: "object", properties: {}, additionalProperties: true},
      timeoutMs: 30_000,
      idempotent: false,
    }),
  ],
});
```

This is the core integration rule: different providers, one registry, one permission model, one journaled execution path.
