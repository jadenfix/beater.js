// The `beater:connect` module — route-level metadata helpers for the Agent
// Access Layer. These helpers produce plain JSON only; execution, auth, and
// receipts stay in Rust/runtime layers.

const MUTATING_SIDE_EFFECTS = new Set(["write", "send", "purchase", "delete"]);
const CONFIRM_BY_DEFAULT = new Set(["send", "purchase", "delete"]);
const SIDE_EFFECTS = new Set(["read", "draft", "write", "send", "purchase", "delete"]);
const HTTP_METHODS = new Set(["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"]);

function objectSchema() {
  return { type: "object", additionalProperties: false, properties: {}, required: [] };
}

function normalizeAuth(auth) {
  if (!auth || typeof auth !== "object") {
    return { type: "public", scopes: [] };
  }
  const type = typeof auth.type === "string" ? auth.type : "public";
  const scopes = Array.isArray(auth.scopes)
    ? auth.scopes.filter((scope) => typeof scope === "string")
    : [];
  return { type, scopes };
}

function normalizeMethod(method) {
  const normalized = typeof method === "string" ? method.trim().toUpperCase() : "POST";
  if (!HTTP_METHODS.has(normalized)) {
    throw new Error(`defineAction method must be one of ${Array.from(HTTP_METHODS).join(", ")}`);
  }
  return normalized;
}

function normalizePath(path) {
  if (typeof path !== "string" || path.trim() !== path || !path.startsWith("/")) {
    throw new Error("defineAction path must be an absolute path starting with /");
  }
  if (path.includes("?") || path.includes("#")) {
    throw new Error("defineAction path must not include query strings or fragments");
  }
  return path;
}

export function defineAction(config) {
  if (!config || typeof config !== "object") {
    throw new Error("defineAction(config) requires a config object");
  }
  if (typeof config.id !== "string" || config.id.length === 0) {
    throw new Error("defineAction requires a non-empty string id");
  }
  const sideEffect = SIDE_EFFECTS.has(config.sideEffect) ? config.sideEffect : "read";
  return {
    id: config.id,
    title: typeof config.title === "string" ? config.title : config.id,
    description: typeof config.description === "string" ? config.description : "",
    method: normalizeMethod(config.method),
    path: normalizePath(config.path),
    sideEffect,
    auth: normalizeAuth(config.auth),
    confirm:
      typeof config.confirm === "boolean"
        ? config.confirm
        : CONFIRM_BY_DEFAULT.has(sideEffect),
    dryRun: config.dryRun === true,
    idempotencyRequired:
      typeof config.idempotencyRequired === "boolean"
        ? config.idempotencyRequired
        : MUTATING_SIDE_EFFECTS.has(sideEffect),
    inputSchema:
      config.inputSchema && typeof config.inputSchema === "object"
        ? config.inputSchema
        : objectSchema(),
    outputSchema:
      config.outputSchema && typeof config.outputSchema === "object"
        ? config.outputSchema
        : objectSchema(),
  };
}
