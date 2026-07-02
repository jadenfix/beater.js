# Integration Registry

beater.js should expose one integration registry, not separate queues or sidecar services for web actions, local tools, remote MCP servers, and browser-control providers.

The implemented registry today supports first-party Python tools, Rust built-ins, declared remote MCP tools, and a mock CDP browser provider for deterministic agent-loop and lifecycle tests. Production Playwright/CDP providers still need to fit the same contract before they ship.

## Contract

Every integration exposed to an agent should have:

- `name`: stable, globally unique tool name within the app.
- `description`: human-readable capability summary for LLM/tool clients.
- `input_schema`: JSON Schema for validation and MCP/tool metadata.
- `kind`: implementation kind, such as `python`, `rust`, `remote_mcp`, or `browser`.
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
```

Rust built-ins are appropriate for stable host capabilities, low-level system integration, and functionality that should ship inside the binary.

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
  egress: ["mcp.linear.example"],
  idempotent: false,
})
```

Implemented behavior:

- calls are tested against a local mock MCP server
- bearer auth reads tokens from environment variables only; missing or empty secrets fail before any network connection
- bearer auth requires HTTPS for non-loopback endpoints
- endpoint hosts must match the declaration's `egress` allowlist
- HTTP redirects are not followed, so an allowed endpoint cannot redirect tool arguments or credentials to a host outside the egress policy
- HTTP timeouts fail closed
- transient server errors and rate limits retry only when the tool is idempotent or a configured `tool_use_id` idempotency key is available
- outbound MCP `tools/call` requests reuse `tool_use_id` as the JSON-RPC id and `Idempotency-Key` header when configured
- non-idempotent calls park as `needs_review` on crash-resume and after ambiguous network/provider failures

Planned next steps:

- remote `initialize` and `tools/list` discovery for provider health checks
- MCP sessions and resumable transport metadata

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
- `session: {scope: "run", cleanup: "always"}` is accepted as the target provider policy
- mock browser sessions are cleaned up on success, failure, and timeout
- non-empty `secrets` are rejected by `mock_cdp`; real providers must validate and scope credentials explicitly
- a mocked agent-loop test proves an agent can complete a browser task through a tool declaration
- destructive actions require non-idempotent handling or explicit review semantics

Production Playwright/CDP release criteria:

- real browser sessions are attached to run IDs
- session cleanup survives process interruption and resume
- credentials are scoped to the provider/session
- browser e2e tests prove an agent can complete a real browser task

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
