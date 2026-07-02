# Tools

beater.js tools are first-party code declared by an agent and exposed through the same registry that powers Anthropic tool calls and `/mcp`.

## Agent declarations

`agents/<name>/agent.ts` imports helpers from `beater:agent`:

```ts
import { defineAgent, pyTool, rustTool } from "beater:agent";

export default defineAgent({
  name: "support",
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

Rust built-ins are idempotent by default because they are first-party host code with no external side effects unless explicitly implemented otherwise.

## Idempotency

Every non-read-only tool must be explicit about idempotency. This is the crash-resume contract:

- `idempotent: true`: beater may re-run the tool after a crash if the journal has a started tool step without a completed result. The tool should use the `tool_use_id` as an idempotency key when it talks to external systems.
- `idempotent: false`: beater will not re-run the tool after a crash. The run is parked as `needs_review` so a human can inspect whether the side effect happened.

Use `idempotent: false` for tools that send email, charge money, mutate external records, start browser sessions, or call APIs that cannot be safely de-duplicated.

## Integration roadmap

Remote MCP servers and browser-control providers should enter through the same registry shape: name, description, input schema, implementation kind, timeout/retry policy, secret source, and idempotency. They should not bypass the journal. A remote MCP or browser tool is release-ready only when it has mock-server or browser e2e coverage proving failures are resumable and sessions are cleaned up.

See [Integration Registry](integrations.md) for the full contract and target declaration shapes.
