# Integration Registry

beater.js should expose one integration registry, not separate queues or sidecar services for web actions, local tools, remote MCP servers, and browser-control providers.

The implemented registry today supports first-party Python and Rust tools. Remote MCP sources and browser providers are planned tool kinds, but they must fit the same contract before they ship.

## Contract

Every integration exposed to an agent should have:

- `name`: stable, globally unique tool name within the app.
- `description`: human-readable capability summary for LLM/tool clients.
- `input_schema`: JSON Schema for validation and MCP/tool metadata.
- `kind`: implementation kind, such as `python`, `rust`, planned `remote_mcp`, or planned `browser`.
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

## Planned Kinds

The declarations below are target shapes, not implemented APIs yet. They define the bar for future work.

### Remote MCP Tools

Remote MCP providers should be declared as registry-backed tools with scoped credentials and mock-server coverage:

```ts
remoteMcpTool("linear.create_issue", {
  server: "linear",
  tool: "create_issue",
  endpoint: "https://mcp.linear.example/mcp",
  auth: {type: "bearer", env: "LINEAR_MCP_TOKEN"},
  timeoutMs: 10_000,
  retry: {attempts: 2, backoffMs: 250, idempotencyKey: "tool_use_id"},
  idempotent: false,
})
```

Release criteria:

- initialize/list/call are tested against a local mock MCP server
- auth failures fail closed and do not reach tool execution
- timeouts and retry behavior are deterministic in tests
- idempotent tools reuse `tool_use_id` when the remote API supports it
- non-idempotent calls park as `needs_review` on crash-resume

### Browser Providers

Browser/CDP/Playwright providers should also enter as tools, not as a separate automation service:

```ts
browserTool("checkout_flow", {
  provider: "playwright",
  session: {scope: "run", cleanup: "always"},
  allowedOrigins: ["https://shop.example"],
  timeoutMs: 30_000,
  secrets: {profile: {env: "BROWSER_PROFILE_ID"}},
  idempotent: false,
})
```

Release criteria:

- browser sessions are attached to run IDs
- session cleanup runs on completion, failure, and resume after crash
- credentials are scoped to the provider/session
- destructive actions require non-idempotent handling or explicit review semantics
- e2e tests prove an agent can complete a browser task and cleanup survives interruption

## Coexistence

One agent config should be able to mix local and networked capabilities:

```ts
export default defineAgent({
  name: "operator",
  tools: [
    pyTool("score_lead", "./tools/score_lead.py", {idempotent: true}),
    rustTool("get_time"),
    // planned:
    remoteMcpTool("crm.update_contact", {
      server: "crm",
      tool: "update_contact",
      auth: {type: "bearer", env: "CRM_MCP_TOKEN"},
      timeoutMs: 10_000,
      retry: {attempts: 1},
      idempotent: false,
    }),
    browserTool("verify_checkout", {
      provider: "playwright",
      session: {scope: "run", cleanup: "always"},
      allowedOrigins: ["https://store.example"],
      timeoutMs: 30_000,
      idempotent: false,
    }),
  ],
});
```

This is the core integration rule: different providers, one registry, one permission model, one journaled execution path.
