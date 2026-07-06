#!/usr/bin/env node
const { spawn } = require("node:child_process");
const fs = require("node:fs");
const net = require("node:net");
const path = require("node:path");

const root = path.resolve(__dirname, "..");
const beater = process.env.BEATER_BIN ?? path.join(root, "target/debug/beater");
const stamp = new Date().toISOString().replace(/[-:.]/g, "");
const defaultOut = path.join(
  root,
  "examples/hello/.beater/live-provider-smoke",
  `${stamp}-pid${process.pid}`,
);
const macPythonFrameworkPath =
  "/Library/Developer/CommandLineTools/Library/Frameworks";

function usage() {
  return `Usage: scripts/llm-live-provider-smoke.cjs [--provider anthropic|openai-compatible|all-configured] [--dry-run] [--out DIR]

Runs one real provider smoke test through beater agent run, a Python tool, and
the SQLite journal. Secrets are read only from environment variables and are
redacted from saved logs.

Environment:
  BEATER_LIVE_PROVIDER=anthropic|openai-compatible|all-configured
  BEATER_LLM_MODEL=<model>                         shared model override
  BEATER_LIVE_ANTHROPIC_MODEL=<model>              Anthropic-specific model
  BEATER_LIVE_OPENAI_MODEL=<model>                 OpenAI-compatible model
  ANTHROPIC_API_KEY=...                            Anthropic live key
  BEATER_OPENAI_API_KEY=... or OPENAI_API_KEY=...  OpenAI-compatible live key
  BEATER_OPENAI_BASE_URL=https://.../v1            e.g. NVIDIA endpoint
  BEATER_OPENAI_ALLOW_CUSTOM_BASE_URL=1            required for custom HTTPS origins
`;
}

function parseArgs(argv) {
  const args = {
    provider: process.env.BEATER_LIVE_PROVIDER ?? "all-configured",
    dryRun: false,
    out: process.env.BEATER_LIVE_PROVIDER_OUT ?? defaultOut,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--help" || arg === "-h") {
      args.help = true;
    } else if (arg === "--dry-run") {
      args.dryRun = true;
    } else if (arg === "--provider") {
      args.provider = argv[++index];
    } else if (arg.startsWith("--provider=")) {
      args.provider = arg.slice("--provider=".length);
    } else if (arg === "--out") {
      args.out = argv[++index];
    } else if (arg.startsWith("--out=")) {
      args.out = arg.slice("--out=".length);
    } else {
      throw new Error(`unknown argument: ${arg}\n\n${usage()}`);
    }
  }
  return args;
}

function canonicalProvider(provider) {
  const value = String(provider ?? "").trim().toLowerCase().replace(/_/g, "-");
  return value === "openai" ? "openai-compatible" : value;
}

function envFlag(name) {
  const value = process.env[name];
  return value === "1" || value?.toLowerCase() === "true";
}

function modelFor(provider) {
  if (provider === "anthropic") {
    return process.env.BEATER_LIVE_ANTHROPIC_MODEL ?? process.env.BEATER_LLM_MODEL;
  }
  if (provider === "openai-compatible") {
    return process.env.BEATER_LIVE_OPENAI_MODEL ?? process.env.BEATER_LLM_MODEL;
  }
  return process.env.BEATER_LLM_MODEL;
}

function keyFor(provider) {
  if (provider === "anthropic") return process.env.ANTHROPIC_API_KEY;
  if (provider === "openai-compatible") {
    return process.env.BEATER_OPENAI_API_KEY ?? process.env.OPENAI_API_KEY;
  }
  return undefined;
}

function baseUrlFor(provider) {
  if (provider === "anthropic") {
    return process.env.ANTHROPIC_BASE_URL ?? "https://api.anthropic.com";
  }
  if (provider === "openai-compatible") {
    return (
      process.env.BEATER_OPENAI_BASE_URL ??
      process.env.OPENAI_BASE_URL ??
      "https://api.openai.com/v1"
    );
  }
  return undefined;
}

function isLoopback(hostname) {
  const host = hostname.toLowerCase();
  if (host === "localhost" || host === "::1" || host === "[::1]") return true;
  if (net.isIP(host) === 4) return host.split(".")[0] === "127";
  if (net.isIP(host) === 6) return host === "::1";
  return false;
}

function requireHttpScheme(provider, parsed) {
  return (
    parsed.protocol === "https:" ||
    parsed.protocol === "http:" ||
    failUnsupportedScheme(provider, parsed.protocol)
  );
}

function failUnsupportedScheme(provider, protocol) {
  throw new Error(`${provider} base URL must use http or https, got ${protocol}`);
}

function validateBaseUrl(provider, raw) {
  let parsed;
  try {
    parsed = new URL(raw);
  } catch (_error) {
    throw new Error(`${provider} base URL is invalid: ${raw}`);
  }
  if (parsed.username || parsed.password || parsed.search || parsed.hash) {
    throw new Error(
      `${provider} base URL must not contain credentials, query parameters, or fragments`,
    );
  }
  requireHttpScheme(provider, parsed);
  const host = parsed.hostname.toLowerCase();
  if (provider === "anthropic") {
    if (parsed.protocol === "http:" && !isLoopback(host)) {
      throw new Error("Anthropic live smoke refuses non-loopback HTTP base URLs");
    }
    if (
      parsed.protocol === "http:" &&
      isLoopback(host) &&
      !envFlag("BEATER_ANTHROPIC_ALLOW_INSECURE_LOOPBACK")
    ) {
      throw new Error(
        "Anthropic HTTP loopback requires BEATER_ANTHROPIC_ALLOW_INSECURE_LOOPBACK=1",
      );
    }
    if (
      parsed.protocol === "https:" &&
      host !== "api.anthropic.com" &&
      !envFlag("BEATER_ANTHROPIC_ALLOW_CUSTOM_BASE_URL")
    ) {
      throw new Error(
        "custom Anthropic HTTPS origins require BEATER_ANTHROPIC_ALLOW_CUSTOM_BASE_URL=1",
      );
    }
  }
  if (provider === "openai-compatible") {
    if (parsed.protocol === "http:" && !isLoopback(host)) {
      throw new Error("OpenAI-compatible live smoke refuses non-loopback HTTP base URLs");
    }
    if (
      parsed.protocol === "http:" &&
      isLoopback(host) &&
      !envFlag("BEATER_OPENAI_ALLOW_INSECURE_LOOPBACK")
    ) {
      throw new Error(
        "OpenAI-compatible HTTP loopback requires BEATER_OPENAI_ALLOW_INSECURE_LOOPBACK=1",
      );
    }
    if (
      parsed.protocol === "https:" &&
      host !== "api.openai.com" &&
      !envFlag("BEATER_OPENAI_ALLOW_CUSTOM_BASE_URL")
    ) {
      throw new Error(
        "custom OpenAI-compatible HTTPS origins require BEATER_OPENAI_ALLOW_CUSTOM_BASE_URL=1",
      );
    }
  }
}

function providerConfig(provider, explicit) {
  const canonical = canonicalProvider(provider);
  if (!["anthropic", "openai-compatible"].includes(canonical)) {
    throw new Error(`unsupported live provider ${provider}; use anthropic or openai-compatible`);
  }
  const model = modelFor(canonical);
  const key = keyFor(canonical);
  const baseUrl = baseUrlFor(canonical);
  if (!key) {
    if (!explicit) return null;
    const keyName =
      canonical === "anthropic"
        ? "ANTHROPIC_API_KEY"
        : "BEATER_OPENAI_API_KEY or OPENAI_API_KEY";
    throw new Error(`${keyName} is required for ${canonical} live smoke`);
  }
  if (!model) {
    if (!explicit) return null;
    throw new Error(
      `BEATER_LLM_MODEL or a provider-specific BEATER_LIVE_*_MODEL is required for ${canonical} live smoke`,
    );
  }
  validateBaseUrl(canonical, baseUrl);
  return {
    provider: canonical,
    model,
    baseUrl,
    toolName: "summarize_numbers",
  };
}

function selectedProviders(requested) {
  const values = String(requested)
    .split(",")
    .map(canonicalProvider)
    .map((value) => value.trim())
    .filter(Boolean);
  if (values.length === 0 || values.includes("all-configured")) {
    const configured = ["anthropic", "openai-compatible"]
      .map((provider) => providerConfig(provider, false))
      .filter(Boolean);
    if (configured.length === 0) {
      throw new Error(
        "no live providers are fully configured; set BEATER_LIVE_PROVIDER, a model, and the provider key",
      );
    }
    return configured;
  }
  return values.map((provider) => providerConfig(provider, true));
}

function secretValues() {
  return [
    process.env.ANTHROPIC_API_KEY,
    process.env.BEATER_OPENAI_API_KEY,
    process.env.OPENAI_API_KEY,
  ].filter((value) => value && value.length >= 6);
}

function redact(text) {
  let out = String(text ?? "");
  for (const secret of secretValues()) {
    out = out.split(secret).join("[REDACTED]");
  }
  const anthropicPrefix = ["sk", "ant", "api"].join("-");
  const nvidiaPrefix = "nvapi" + "-";
  out = out.replace(
    new RegExp(`${anthropicPrefix}[0-9A-Za-z_-]+`, "g"),
    `${anthropicPrefix}[REDACTED]`,
  );
  out = out.replace(
    new RegExp(`${nvidiaPrefix}[0-9A-Za-z_-]+`, "g"),
    `${nvidiaPrefix}[REDACTED]`,
  );
  out = out.replace(/sk-[A-Za-z0-9_-]{20,}/g, "sk-[REDACTED]");
  return out;
}

function run(command, args, options = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd: options.cwd ?? root,
      env: options.env ?? process.env,
      stdio: ["ignore", "pipe", "pipe"],
    });
    const timeoutMs = options.timeoutMs ?? 180_000;
    let stdout = "";
    let stderr = "";
    const timer = setTimeout(() => {
      child.kill("SIGKILL");
    }, timeoutMs);
    child.stdout.on("data", (chunk) => {
      stdout += chunk;
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk;
    });
    child.once("error", (error) => {
      clearTimeout(timer);
      reject(error);
    });
    child.once("close", (code, signal) => {
      clearTimeout(timer);
      const result = {
        command,
        args,
        stdout: redact(stdout),
        stderr: redact(stderr),
        code,
        signal,
      };
      if (code === 0) {
        resolve(result);
      } else {
        const error = new Error(
          `${command} ${args.join(" ")} failed with code=${code} signal=${signal}`,
        );
        Object.assign(error, result);
        reject(error);
      }
    });
  });
}

function gateEnv(config) {
  const env = {
    PATH: process.env.PATH,
    HOME: process.env.HOME,
    TMPDIR: process.env.TMPDIR,
    TMP: process.env.TMP,
    TEMP: process.env.TEMP,
    RUST_BACKTRACE: process.env.RUST_BACKTRACE,
    BEATER_LLM_PROVIDER: config.provider,
    BEATER_LLM_MODEL: config.model,
  };
  if (process.platform === "darwin" && !env.DYLD_FRAMEWORK_PATH) {
    env.DYLD_FRAMEWORK_PATH = macPythonFrameworkPath;
  }
  if (config.provider === "anthropic") {
    env.ANTHROPIC_API_KEY = process.env.ANTHROPIC_API_KEY;
    if (process.env.ANTHROPIC_BASE_URL) env.ANTHROPIC_BASE_URL = process.env.ANTHROPIC_BASE_URL;
    if (process.env.BEATER_ANTHROPIC_ALLOW_CUSTOM_BASE_URL) {
      env.BEATER_ANTHROPIC_ALLOW_CUSTOM_BASE_URL =
        process.env.BEATER_ANTHROPIC_ALLOW_CUSTOM_BASE_URL;
    }
    if (process.env.BEATER_ANTHROPIC_ALLOW_INSECURE_LOOPBACK) {
      env.BEATER_ANTHROPIC_ALLOW_INSECURE_LOOPBACK =
        process.env.BEATER_ANTHROPIC_ALLOW_INSECURE_LOOPBACK;
    }
  }
  if (config.provider === "openai-compatible") {
    if (process.env.BEATER_OPENAI_API_KEY) {
      env.BEATER_OPENAI_API_KEY = process.env.BEATER_OPENAI_API_KEY;
    } else {
      env.OPENAI_API_KEY = process.env.OPENAI_API_KEY;
    }
    if (process.env.BEATER_OPENAI_BASE_URL) {
      env.BEATER_OPENAI_BASE_URL = process.env.BEATER_OPENAI_BASE_URL;
    }
    if (process.env.OPENAI_BASE_URL) env.OPENAI_BASE_URL = process.env.OPENAI_BASE_URL;
    if (process.env.BEATER_OPENAI_ALLOW_CUSTOM_BASE_URL) {
      env.BEATER_OPENAI_ALLOW_CUSTOM_BASE_URL =
        process.env.BEATER_OPENAI_ALLOW_CUSTOM_BASE_URL;
    }
    if (process.env.BEATER_OPENAI_ALLOW_INSECURE_LOOPBACK) {
      env.BEATER_OPENAI_ALLOW_INSECURE_LOOPBACK =
        process.env.BEATER_OPENAI_ALLOW_INSECURE_LOOPBACK;
    }
  }
  return env;
}

function writeApp(app, config) {
  fs.mkdirSync(path.join(app, "agents/support/tools"), { recursive: true });
  fs.writeFileSync(
    path.join(app, "beater.toml"),
    `[app]
name = "live-provider-smoke"
port = 3000
`,
  );
  fs.writeFileSync(
    path.join(app, "agents/support/agent.ts"),
    `import { defineAgent, pyTool } from "beater:agent";

export default defineAgent({
  name: "support",
  provider: "${config.provider}",
  model: "${config.model}",
  system: "You are running a beater.js live provider smoke test. You must call summarize_numbers exactly once with the numbers [3,1,4,1,5] before answering. After the tool result, answer briefly.",
  tools: [
    pyTool("summarize_numbers", "./tools/summarize_numbers.py", {
      idempotent: true,
    }),
  ],
});
`,
  );
  fs.writeFileSync(
    path.join(app, "agents/support/tools/summarize_numbers.py"),
    `TOOL = {
    "description": "Summarize a list of numbers: count, sum, mean, min, max.",
    "input_schema": {
        "type": "object",
        "properties": {
            "numbers": {"type": "array", "items": {"type": "number"}}
        },
        "required": ["numbers"],
    },
}


def run(input):
    nums = [float(n) for n in input["numbers"]]
    return {
        "count": len(nums),
        "sum": sum(nums),
        "mean": sum(nums) / len(nums),
        "min": min(nums),
        "max": max(nums),
    }
`,
  );
}

async function runSql(journal, query) {
  const result = await run("sqlite3", [journal, query], { timeoutMs: 30_000 });
  return result.stdout.trim();
}

function parseRunId(stdout) {
  const match = /^run ([0-9a-f-]+)/m.exec(stdout);
  if (!match) {
    throw new Error(`could not parse run id from output:\n${stdout}`);
  }
  return match[1];
}

function parseToolPayload(row) {
  const envelope = JSON.parse(row.result);
  const content = envelope.content;
  if (typeof content === "string") return JSON.parse(content);
  return content;
}

async function inspectJournal(app, runId, toolName) {
  const journal = path.join(app, ".beater/journal.db");
  const status = await runSql(journal, `SELECT status FROM runs WHERE id='${runId}'`);
  if (status !== "completed") {
    throw new Error(`expected completed run ${runId}, got ${status || "missing"}`);
  }
  const rowsJson = await runSql(
    journal,
    `SELECT json_object('seq', seq, 'kind', kind, 'status', status, 'tool_name', tool_name, 'tool_use_id', tool_use_id, 'attempt', attempt, 'result', result) FROM steps WHERE run_id='${runId}' ORDER BY seq;`,
  );
  const rows = rowsJson
    .split("\n")
    .filter(Boolean)
    .map((line) => JSON.parse(line));
  const toolRows = rows.filter(
    (row) => row.kind === "tool_call" && row.status === "completed" && row.tool_name === toolName,
  );
  if (toolRows.length !== 1) {
    throw new Error(`expected exactly one completed ${toolName} row, saw ${toolRows.length}`);
  }
  const toolRow = toolRows[0];
  const priorLlm = rows.some(
    (row) => row.kind === "llm_call" && row.status === "completed" && row.seq < toolRow.seq,
  );
  const laterLlm = rows.some(
    (row) => row.kind === "llm_call" && row.status === "completed" && row.seq > toolRow.seq,
  );
  if (!priorLlm || !laterLlm) {
    throw new Error(`expected completed llm_call rows around the tool call: ${rowsJson}`);
  }
  const payload = parseToolPayload(toolRow);
  if (payload.mean !== 2.8 || payload.count !== 5 || payload.sum !== 14) {
    throw new Error(`unexpected tool payload: ${JSON.stringify(payload)}`);
  }
  const partials = Number(
    await runSql(journal, `SELECT COUNT(*) FROM step_partials WHERE run_id='${runId}'`),
  );
  if (!Number.isFinite(partials) || partials < 1) {
    throw new Error(`expected provider stream partials for ${runId}, saw ${partials}`);
  }
  return {
    status,
    rows: rows.map((row) => ({
      seq: row.seq,
      kind: row.kind,
      status: row.status,
      tool_name: row.tool_name,
      attempt: row.attempt,
      tool_use_id: row.tool_use_id,
    })),
    partials,
    toolPayload: payload,
  };
}

function providerSlug(provider) {
  return provider.replace(/[^a-z0-9]+/g, "-");
}

function writeProviderEvidence(out, results) {
  const lines = [
    "# beater.js live LLM provider smoke evidence",
    "",
    `Generated: ${new Date().toISOString()}`,
    `beater binary: \`${beater}\``,
    "",
  ];
  for (const result of results) {
    lines.push(`## ${result.provider}`);
    lines.push("");
    lines.push(`- Model: \`${result.model}\``);
    lines.push(`- Base URL: \`${result.baseUrl}\``);
    lines.push(`- Run ID: \`${result.runId}\``);
    lines.push(`- App fixture: \`${result.app}\``);
    lines.push(`- Stdout: \`${path.relative(out, result.stdoutPath)}\``);
    lines.push(`- Stderr: \`${path.relative(out, result.stderrPath)}\``);
    lines.push(`- Stream partial rows: \`${result.journal.partials}\``);
    lines.push(`- Tool payload: \`${JSON.stringify(result.journal.toolPayload)}\``);
    lines.push("");
    lines.push("| seq | kind | status | tool | attempt | tool_use_id |");
    lines.push("|---:|---|---|---|---:|---|");
    for (const row of result.journal.rows) {
      lines.push(
        `| ${row.seq} | ${row.kind} | ${row.status} | ${row.tool_name ?? ""} | ${row.attempt ?? ""} | ${row.tool_use_id ?? ""} |`,
      );
    }
    lines.push("");
  }
  fs.writeFileSync(path.join(out, "evidence.md"), `${lines.join("\n")}\n`);
}

async function runProvider(config, out) {
  const slug = providerSlug(config.provider);
  const app = path.join(out, "apps", slug);
  const logs = path.join(out, "logs");
  fs.mkdirSync(logs, { recursive: true });
  writeApp(app, config);
  const result = await run(
    beater,
    [
      "agent",
      "run",
      "--app",
      app,
      "support",
      "Use the summarize_numbers tool on 3,1,4,1,5, then answer with the mean.",
    ],
    {
      cwd: root,
      env: gateEnv(config),
      timeoutMs: Number(process.env.BEATER_LIVE_PROVIDER_TIMEOUT_MS ?? 180_000),
    },
  );
  const stdoutPath = path.join(logs, `${slug}-stdout.log`);
  const stderrPath = path.join(logs, `${slug}-stderr.log`);
  fs.writeFileSync(stdoutPath, result.stdout);
  fs.writeFileSync(stderrPath, result.stderr);
  const runId = parseRunId(result.stdout);
  const journal = await inspectJournal(app, runId, config.toolName);
  return {
    provider: config.provider,
    model: config.model,
    baseUrl: config.baseUrl,
    app,
    runId,
    stdoutPath,
    stderrPath,
    journal,
  };
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.help) {
    console.log(usage());
    return;
  }
  const providers = selectedProviders(args.provider);
  if (args.dryRun) {
    console.log("live provider smoke dry-run:");
    for (const provider of providers) {
      console.log(`- ${provider.provider}: model=${provider.model} base=${provider.baseUrl}`);
    }
    return;
  }
  if (!fs.existsSync(beater)) {
    throw new Error(`beater binary not found at ${beater}; run cargo build --bin beater first`);
  }
  fs.mkdirSync(args.out, { recursive: true });
  const results = [];
  try {
    for (const provider of providers) {
      results.push(await runProvider(provider, args.out));
      writeProviderEvidence(args.out, results);
    }
  } catch (error) {
    fs.writeFileSync(
      path.join(args.out, "failure.txt"),
      `${redact(error.stack ?? error.message ?? error)}\n${redact(error.stdout ?? "")}\n${redact(error.stderr ?? "")}\n`,
    );
    throw error;
  }
  writeProviderEvidence(args.out, results);
  console.log(`live provider smoke passed: ${results.map((result) => result.provider).join(", ")}`);
  console.log(`evidence: ${path.join(args.out, "evidence.md")}`);
}

main().catch((error) => {
  console.error(redact(error.stdout ?? ""));
  console.error(redact(error.stderr ?? ""));
  console.error(redact(error.stack ?? error.message ?? error));
  process.exit(1);
});
