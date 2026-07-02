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

// Beatbox sandbox tool: Tier-4 untrusted code runs out-of-process in beatboxd.
// Defaults are intentionally conservative; declare idempotent only when the
// beatbox result is deterministic for the chosen source and policy.
export function sandboxTool(name, opts = {}) {
  if (!opts || typeof opts !== "object") {
    throw new Error("sandboxTool(name, options) requires an options object");
  }
  if (opts.path && opts.source) {
    throw new Error("sandboxTool accepts either path or source, not both");
  }
  if (!opts.path && !opts.source) {
    throw new Error("sandboxTool requires a path or source");
  }
  const tool = {
    kind: "sandbox",
    name,
    lane: opts.lane ?? "wasm",
    policy: opts.policy ?? {},
    idempotent: opts.idempotent ?? false,
  };
  if (opts.path) tool.path = opts.path;
  if (opts.source) tool.source = opts.source;
  if (opts.entrypoint) tool.entrypoint = opts.entrypoint;
  if (opts.description) tool.description = opts.description;
  if (opts.inputSchema) tool.inputSchema = opts.inputSchema;
  return tool;
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

// Browser automation tool source. The Rust side currently ships a mock CDP
// provider for deterministic lifecycle tests; real Playwright/CDP providers
// use the same declaration shape.
export function browserTool(name, opts = {}) {
  if (!opts.provider) {
    throw new Error("browserTool requires opts.provider");
  }
  if (!opts.description) {
    throw new Error("browserTool requires opts.description");
  }
  if (!opts.inputSchema) {
    throw new Error("browserTool requires opts.inputSchema");
  }
  return {
    kind: "browser",
    name,
    idempotent: opts.idempotent ?? false,
    provider: opts.provider,
    description: opts.description,
    inputSchema: opts.inputSchema,
    session: opts.session ?? {scope: "run", cleanup: "always"},
    allowedOrigins: opts.allowedOrigins ?? [],
    timeoutMs: opts.timeoutMs ?? 30000,
    secrets: opts.secrets ?? {},
  };
}
