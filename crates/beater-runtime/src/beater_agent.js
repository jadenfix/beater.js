// The `beater:agent` module — the DX surface agents/<name>/agent.ts imports.
// These produce plain config objects; the loop itself runs in Rust.

export function defineAgent(cfg) {
  if (!cfg || typeof cfg !== "object") {
    throw new Error("defineAgent(config) requires a config object");
  }
  return {
    name: cfg.name ?? "agent",
    model: cfg.model ?? "claude-opus-4-8",
    system: cfg.system ?? "",
    tools: cfg.tools ?? [],
  };
}

// Python tool: full-fat CPython embedded in the host (numpy/torch work).
// Not idempotent unless declared — the resume-safety contract.
export function pyTool(name, path, opts = {}) {
  return { kind: "python", name, path, idempotent: opts.idempotent ?? false };
}

// Rust built-in tool, compiled into the host.
export function rustTool(name, opts = {}) {
  return { kind: "rust", name, idempotent: opts.idempotent ?? true };
}

// Remote MCP tool source. Metadata is declared locally so the agent can expose
// stable tool schemas before it calls the networked provider.
export function remoteMcpTool(name, opts = {}) {
  if (!opts.endpoint) {
    throw new Error("remoteMcpTool requires opts.endpoint");
  }
  if (!opts.tool) {
    throw new Error("remoteMcpTool requires opts.tool");
  }
  if (!opts.description) {
    throw new Error("remoteMcpTool requires opts.description");
  }
  if (!opts.inputSchema) {
    throw new Error("remoteMcpTool requires opts.inputSchema");
  }
  return {
    kind: "remote_mcp",
    name,
    idempotent: opts.idempotent ?? false,
    description: opts.description,
    inputSchema: opts.inputSchema,
    endpoint: opts.endpoint,
    tool: opts.tool,
    auth: opts.auth ?? null,
    timeoutMs: opts.timeoutMs ?? 10000,
    retry: opts.retry ?? null,
    egress: opts.egress ?? [],
  };
}
