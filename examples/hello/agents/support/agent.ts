// M2: the agent loop runs in Rust (journaled, crash-resumable);
// this file declares config + tools. Python tools run in embedded CPython.
import { defineAgent, pyTool, rustTool, sandboxTool } from "beater:agent";

export default defineAgent({
  name: "support",
  provider: "anthropic",
  model: "claude-opus-4-8",
  system:
    "You are a concise support agent. Use tools whenever math or data is involved; do not compute by hand.",
  tools: [
    pyTool("summarize_numbers", "./tools/summarize_numbers.py", {
      idempotent: true,
    }),
    pyTool("slow_summarize", "./tools/slow_summarize.py", {
      idempotent: true,
    }),
    pyTool("slow_summarize_once", "./tools/slow_summarize_once.py", {
      idempotent: false,
    }),
    sandboxTool("fib_wasm", {
      path: "./tools/fib.wat",
      idempotent: true,
      description: "Run a deterministic Fibonacci function inside beatbox.",
      inputSchema: {
        type: "object",
        properties: { n: { type: "integer", minimum: 0, maximum: 40 } },
        required: ["n"],
      },
      policy: {
        limits: {
          wall_ms: 5000,
          cpu_ms: 5000,
          memory_bytes: 67108864,
          output_bytes: 1048576,
          pids: 1,
          disk_bytes: 67108864,
          fuel: 10000000,
        },
      },
    }),
    rustTool("get_time"),
  ],
});
