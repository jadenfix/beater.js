# Tools

beater.js tools are first-party code declared by an agent and exposed through the same registry that powers LLM provider tool calls and `/mcp`.

The same `/mcp` endpoint also exposes static workflow prompts through `prompts/list` and `prompts/get`. These prompts cover repetitive engineering work around PR review, documentation sync, systems design, and stack/algorithm selection. They are user-selected prompt templates, not hidden agent `system` prompts, and they do not execute tools or write journal rows.

## Agent declarations

`agents/<name>/agent.ts` imports helpers from `beater:agent`:

```ts
import {
  browserTool,
  defineAction,
  defineAgent,
  pyTool,
  remoteMcpTool,
  rustTool,
  wasmtimeTool,
} from "beater:agent";

export default defineAgent({
  name: "support",
  provider: "anthropic",
  model: "claude-opus-4-8",
  system: "Use tools for data work.",
  tools: [
    pyTool("summarize_numbers", "./tools/summarize_numbers.py", {
      idempotent: true,
    }),
    rustTool("get_time"),
  ],
});
```

Tool names are global within the served app registry. If two agents declare the same tool name, the first loaded tool wins and the duplicate is logged.

## Route Actions

API routes can expose the same handler to humans and agents with `defineAction` metadata:

```ts
import { defineAction } from "beater:agent";

export const agent = {
  actions: [
    defineAction({
      name: "hello.contact",
      description: "Send a contact request.",
      method: "POST",
      confirm: true,
      idempotencyRequired: true,
      auth: {type: "public"},
      inputSchema: {
        type: "object",
        properties: {
          email: {type: "string"},
          message: {type: "string"},
          confirm: {type: "boolean"},
        },
        required: ["email", "message", "confirm"],
      },
    }),
  ],
};

export function POST(request) {
  return {
    status: 200,
    headers: {"content-type": "application/json"},
    body: JSON.stringify({ok: true}),
  };
}
```

The route remains a normal HTML form target. The dev server also imports `agent.actions` into `/mcp tools/list`; `/mcp tools/call` journals a synthetic MCP run, enforces confirmation and idempotency metadata, then dispatches the call through the route handler with JSON arguments. The same action metadata is published through `/openapi.json`, `/llms.txt`, and `/.well-known/beater.json`.

## Python tools

Python tools are `.py` files loaded into embedded CPython. Each file must define:

- `TOOL`: metadata with `description` and `input_schema`.
- `run(input)`: function that accepts a JSON-like Python object and returns a JSON-serializable object.

Example:

```py
TOOL = {
    "description": "Summarize a list of numbers.",
    "input_schema": {
        "type": "object",
        "properties": {
            "numbers": {"type": "array", "items": {"type": "number"}},
        },
        "required": ["numbers"],
    },
}

def run(input):
    nums = [float(n) for n in input["numbers"]]
    return {"count": len(nums), "sum": sum(nums)}
```

The tool file is executed in a fresh namespace for every call, so code edits are picked up without restarting. Runtime packages come from the configured venv's `site-packages`, and that venv must match the embedded Python minor version reported by `beater doctor`.

## Rust tools

Rust tools are built into the host binary. Current built-ins:

- `get_time`: returns the current UTC time as JSON.
- `cpp_double`: calls a C++ function through `cxx` and returns `{"value": n * 2}`.

Rust built-ins are idempotent by default because they are first-party host code with no external side effects unless explicitly implemented otherwise.

## Wasmtime tools

Use `wasmtimeTool` for untrusted scalar wasm functions that do not need host capabilities:

```ts
wasmtimeTool("double_wasm", {
  source: {
    kind: "wasm_wat",
    text: `
      (module
        (func (export "run") (param i64) (result i64)
          local.get 0
          i64.const 2
          i64.mul))
    `,
  },
  description: "Double an integer in the local Wasmtime sandbox.",
  inputSchema: {
    type: "object",
    properties: {n: {type: "integer"}},
    required: ["n"],
  },
  policy: {
    limits: {wall_ms: 1000, memory_bytes: 1048576, fuel: 100000},
  },
  idempotent: true,
})
```

The first Wasmtime tier is hermetic: no WASI, no host imports, no filesystem mounts, no network, no environment variables, and no secrets. Supported entrypoints are `run() -> ()`, `run() -> i64`, and `run(i64) -> i64`; the one-argument form accepts either a raw integer input or `{n: integer}`. Broader capability-scoped WASI handles and richer value passing are future work.

## Idempotency

Every non-read-only tool must be explicit about idempotency. This is the crash-resume contract:

- `idempotent: true`: beater may re-run the tool after a crash if the journal has a started tool step without a completed result. The tool should use the `tool_use_id` as an idempotency key when it talks to external systems.
- `idempotent: false`: beater will not re-run the tool after a crash. The run is parked as `needs_review` so a human can inspect whether the side effect happened.

Use `idempotent: false` for tools that send email, charge money, mutate external records, start browser sessions, or call APIs that cannot be safely de-duplicated.

## Integration roadmap

Remote MCP servers and browser-control providers enter through the same registry shape: name, description, input schema, implementation kind, timeout/retry policy, secret source, egress allowlist, and idempotency. They should not bypass the journal. Remote MCP tools are mock-server tested. Browser tools include a mock CDP provider for contract, cleanup, and agent-loop tests plus a Playwright provider with a real-browser authenticated gate; richer cookie/header credential modes remain future production work.

## Remote MCP tools

Use `remoteMcpTool` for networked MCP providers:

```ts
remoteMcpTool("crm.lookup", {
  endpoint: "https://mcp.crm.example/mcp",
  tool: "lookup_contact",
  description: "Look up a CRM contact.",
  inputSchema: {
    type: "object",
    properties: {email: {type: "string"}},
    required: ["email"],
  },
  auth: {type: "bearer", env: "CRM_MCP_TOKEN"},
  timeoutMs: 10_000,
  retry: {attempts: 2, backoffMs: 250, idempotencyKey: "tool_use_id"},
  session: {scope: "run", cleanup: "always"},
  egress: ["mcp.crm.example"],
  idempotent: true,
})
```

Use `remoteMcpProvider` when the provider's `tools/list` catalog should be imported at startup:

```ts
remoteMcpProvider("crm", {
  endpoint: "https://mcp.crm.example/mcp",
  auth: {type: "bearer", env: "CRM_MCP_TOKEN"},
  timeoutMs: 10_000,
  retry: {attempts: 2, backoffMs: 250, idempotencyKey: "tool_use_id"},
  egress: ["mcp.crm.example"],
  idempotent: true,
})
```

Each discovered provider tool is exposed as `<prefix>.<provider tool name>` in `/mcp tools/list`; execution still calls the original provider tool name. Secrets are read from environment variables at execution time and are never stored in `agent.ts` or the journal. Missing secrets fail before a network connection is opened. Bearer-auth endpoints must use HTTPS except for loopback test servers. The endpoint host must match `egress`; use `host` or `host:port` entries. Redirects are not followed. When `session` is set, beater initializes the remote MCP provider once, stores the returned `Mcp-Session-Id` in memory for the tool, and sends it on later `tools/call` requests.

## Browser tools

Use `browserTool` for browser-provider declarations. `mock_cdp` is deterministic and intended for CI coverage of the browser contract:

```ts
browserTool("browser.checkout", {
  provider: "mock_cdp",
  description: "Verify checkout in a browser.",
  inputSchema: {
    type: "object",
    properties: {url: {type: "string"}, task: {type: "string"}},
    required: ["url", "task"],
  },
  session: {scope: "run", cleanup: "always"},
  allowedOrigins: ["https://shop.example"],
  timeoutMs: 30_000,
  idempotent: false,
})
```

For native browser execution, set `provider: "playwright"`. The Playwright provider reuses the pinned `beater-browser` / `beater-browser-playwright` driver contract, launches Chromium through the upstream Node runner, navigates to `input.url`, and can execute one optional action:

```json
{"url": "https://shop.example/cart", "action": "click", "selector": "#checkout"}
```

The optional action also accepts the upstream driver shape, for example `{"action": {"action": "type", "args": {"selector": "#email", "text": "a@example.com"}}}`. Install the upstream runner dependencies before live use; default tests do not launch Playwright.

Browser tools default to non-idempotent because they create sessions and may perform side effects. `allowedOrigins` is enforced before navigation. With `session: {scope: "run", cleanup: "always"}`, browser tool results use the journal run id as the session id and reuse that session across multiple browser calls in the same run. Sessions are closed when an agent run or synthetic MCP run reaches a terminal state. App-scoped Playwright runs also write runner markers under `.beater/browser-sessions`; `beater agent resume` removes stale markers and terminates marked runners for the run before replay/review.

Browser `secrets` may name env-backed values, for example `secrets: {password: {type: "env", env: "SHOP_PASSWORD"}}`. A `type` action can use `textSecret: "password"` instead of `text`; beater reads the environment value at execution time, passes it only to the browser action, and records the result action as `"<redacted:password>"`.

Run `scripts/playwright-browser-gate.cjs` for the live provider proof. It installs the upstream Playwright runner dependencies in a temp directory, drives an authenticated real Chromium session through `beater agent run`, and verifies three completed browser tool results reused one run-scoped session without leaking the password in the journal.

See [Integration Registry](integrations.md) for the full contract and target declaration shapes.
