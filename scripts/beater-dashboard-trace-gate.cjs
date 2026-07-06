#!/usr/bin/env node
const { spawn } = require("node:child_process");
const fs = require("node:fs");
const http = require("node:http");
const os = require("node:os");
const path = require("node:path");

const root = path.resolve(__dirname, "..");
const beater = process.env.BEATER_BIN ?? path.join(root, "target/debug/beater");
const beaterRepo = process.env.BEATER_REPO ?? path.resolve(root, "../beater");
const dashboardBaseUrl =
  process.env.BEATER_DASHBOARD_URL ?? "http://127.0.0.1:3000";
const macPythonFrameworkPath =
  "/Library/Developer/CommandLineTools/Library/Frameworks";
const beaterdHealthTimeoutMs = Number(
  process.env.BEATERD_HEALTH_TIMEOUT_MS ?? "180000",
);
if (!Number.isFinite(beaterdHealthTimeoutMs) || beaterdHealthTimeoutMs <= 0) {
  throw new Error("BEATERD_HEALTH_TIMEOUT_MS must be a positive number");
}

function run(command, args, options = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd: options.cwd ?? root,
      env: options.env ?? process.env,
      stdio: options.stdio ?? ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    if (child.stdout) child.stdout.on("data", (chunk) => (stdout += chunk));
    if (child.stderr) child.stderr.on("data", (chunk) => (stderr += chunk));
    child.once("error", reject);
    child.once("close", (code, signal) => {
      if (code === 0) {
        resolve({ stdout, stderr });
      } else {
        const error = new Error(
          `${command} ${args.join(" ")} failed with code=${code} signal=${signal}`,
        );
        error.stdout = stdout;
        error.stderr = stderr;
        reject(error);
      }
    });
  });
}

function listen(server) {
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      server.off("error", reject);
      resolve(server.address().port);
    });
  });
}

function freePort() {
  const server = http.createServer();
  return listen(server).then(
    (port) =>
      new Promise((resolve, reject) => {
        server.close((error) => (error ? reject(error) : resolve(port)));
      }),
  );
}

function configuredPort(name) {
  const raw = process.env[name];
  if (!raw) return null;
  const port = Number(raw);
  if (!Number.isInteger(port) || port <= 0 || port > 65535) {
    throw new Error(`${name} must be a TCP port number, got ${raw}`);
  }
  return port;
}

function sse(event, data) {
  return `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`;
}

function anthropicStream(response) {
  const content = response.content ?? [];
  let out = "";
  out += sse("message_start", {
    type: "message_start",
    message: {
      id: response.id ?? "msg_mock",
      type: "message",
      role: "assistant",
      model: response.model ?? "mock",
      content: [],
      stop_reason: null,
      stop_sequence: null,
      usage: { input_tokens: 1, output_tokens: 0 },
    },
  });
  content.forEach((block, index) => {
    const startBlock = { ...block };
    if (block.type === "text") {
      startBlock.text = "";
      out += sse("content_block_start", {
        type: "content_block_start",
        index,
        content_block: startBlock,
      });
      out += sse("content_block_delta", {
        type: "content_block_delta",
        index,
        delta: { type: "text_delta", text: block.text ?? "" },
      });
    } else if (block.type === "tool_use") {
      startBlock.input = {};
      out += sse("content_block_start", {
        type: "content_block_start",
        index,
        content_block: startBlock,
      });
      out += sse("content_block_delta", {
        type: "content_block_delta",
        index,
        delta: {
          type: "input_json_delta",
          partial_json: JSON.stringify(block.input ?? {}),
        },
      });
    }
    out += sse("content_block_stop", { type: "content_block_stop", index });
  });
  out += sse("message_delta", {
    type: "message_delta",
    delta: {
      stop_reason: response.stop_reason ?? "end_turn",
      stop_sequence: response.stop_sequence ?? null,
    },
    usage: { output_tokens: 1 },
  });
  out += sse("message_stop", { type: "message_stop" });
  return out;
}

function startAnthropicServer() {
  const requests = [];
  const responses = [
    {
      content: [
        {
          type: "tool_use",
          id: "toolu_dashboard_get_time",
          name: "get_time",
          input: {},
        },
      ],
      stop_reason: "tool_use",
    },
    {
      content: [{ type: "text", text: "beater dashboard trace verified" }],
      stop_reason: "end_turn",
    },
  ];
  const server = http.createServer((req, res) => {
    let body = "";
    req.setEncoding("utf8");
    req.on("data", (chunk) => (body += chunk));
    req.on("end", () => {
      requests.push({ url: req.url, body });
      const response = responses.shift();
      if (!response) {
        res.writeHead(500, { "content-type": "text/plain" });
        res.end("unexpected Anthropic request");
        return;
      }
      res.writeHead(200, { "content-type": "text/event-stream" });
      res.end(anthropicStream(response));
    });
  });
  return listen(server).then((port) => ({
    base: `http://127.0.0.1:${port}`,
    requests,
    close: () => new Promise((resolve) => server.close(resolve)),
  }));
}

function writeApp(app) {
  fs.mkdirSync(path.join(app, "agents/support"), { recursive: true });
  fs.writeFileSync(
    path.join(app, "beater.toml"),
    `[app]
name = "beater-dashboard-trace-gate"
port = 3000
`,
  );
  fs.writeFileSync(
    path.join(app, "agents/support/agent.ts"),
    `import { defineAgent, rustTool } from "beater:agent";

export default defineAgent({
  name: "support",
  system: "Use get_time when asked.",
  tools: [rustTool("get_time")],
});
`,
  );
}

function beaterdCommand(dataDir, httpPort, grpcPort) {
  const args = [
    "--addr",
    `127.0.0.1:${httpPort}`,
    "--otlp-grpc-addr",
    `127.0.0.1:${grpcPort}`,
    "--data-dir",
    dataDir,
    "--auth-mode",
    "local",
    "--trace-write-drain-interval-ms",
    "25",
    "--trace-ingested-drain-interval-ms",
    "25",
  ];
  if (process.env.BEATERD_BIN) {
    return {
      command: process.env.BEATERD_BIN,
      args,
      cwd: root,
      note: process.env.BEATERD_BIN,
    };
  }
  const manifest = path.join(beaterRepo, "Cargo.toml");
  if (!fs.existsSync(manifest)) {
    throw new Error(
      `beaterd not found: set BEATERD_BIN or BEATER_REPO (expected ${manifest})`,
    );
  }
  return {
    command: "cargo",
    args: [
      "run",
      "-q",
      "--manifest-path",
      manifest,
      "-p",
      "beaterd",
      "--bin",
      "beaterd",
      "--",
      ...args,
    ],
    cwd: beaterRepo,
    note: `cargo run -p beaterd in ${beaterRepo}`,
  };
}

function startBeaterd(dataDir, httpPort, grpcPort) {
  const config = beaterdCommand(dataDir, httpPort, grpcPort);
  const child = spawn(config.command, config.args, {
    cwd: config.cwd,
    env: process.env,
    stdio: ["ignore", "pipe", "pipe"],
  });
  let stdout = "";
  let stderr = "";
  let spawnError = null;
  child.stdout.on("data", (chunk) => {
    stdout += chunk;
    stdout = stdout.slice(-20_000);
  });
  child.stderr.on("data", (chunk) => {
    stderr += chunk;
    stderr = stderr.slice(-20_000);
  });
  child.on("error", (error) => {
    spawnError = error;
    stderr += String(error);
    stderr = stderr.slice(-20_000);
  });
  return {
    child,
    note: config.note,
    spawnError: () => spawnError,
    output: () => ({ stdout, stderr }),
    stop: () =>
      new Promise((resolve) => {
        if (child.exitCode !== null || child.signalCode !== null) {
          resolve();
          return;
        }
        child.once("close", () => resolve());
        child.kill("SIGTERM");
        setTimeout(() => {
          if (child.exitCode === null && child.signalCode === null) {
            child.kill("SIGKILL");
          }
        }, 1500).unref();
      }),
  };
}

async function fetchJson(url, options = {}) {
  const response = await fetch(url, options);
  const body = await response.text();
  if (!response.ok) {
    throw new Error(`GET ${url} failed with ${response.status}: ${body}`);
  }
  return JSON.parse(body);
}

async function fetchText(url, options = {}) {
  const response = await fetch(url, options);
  const body = await response.text();
  if (!response.ok) {
    throw new Error(`GET ${url} failed with ${response.status}: ${body}`);
  }
  return body;
}

async function waitForHealth(apiBase, beaterd) {
  const deadline = Date.now() + beaterdHealthTimeoutMs;
  let lastError;
  while (Date.now() < deadline) {
    if (beaterd.child.exitCode !== null || beaterd.child.signalCode !== null) {
      const { stdout, stderr } = beaterd.output();
      throw new Error(
        `beaterd exited before health check passed\nstdout:\n${stdout}\nstderr:\n${stderr}`,
      );
    }
    if (beaterd.spawnError()) {
      const { stdout, stderr } = beaterd.output();
      throw new Error(
        `beaterd failed to spawn: ${beaterd.spawnError()}\nstdout:\n${stdout}\nstderr:\n${stderr}`,
      );
    }
    try {
      const response = await fetch(`${apiBase}/health`);
      if (response.ok) return;
      lastError = new Error(`health returned ${response.status}`);
    } catch (error) {
      lastError = error;
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  const { stdout, stderr } = beaterd.output();
  throw new Error(
    `beaterd did not become healthy at ${apiBase}: ${lastError}\nstdout:\n${stdout}\nstderr:\n${stderr}`,
  );
}

async function waitForTrace(apiBase, traceId) {
  const url = `${apiBase}/v1/traces/demo/${encodeURIComponent(traceId)}`;
  const deadline = Date.now() + 15_000;
  let lastError;
  while (Date.now() < deadline) {
    try {
      const trace = await fetchJson(url);
      if (Array.isArray(trace.spans) && trace.spans.length >= 4) {
        return trace;
      }
      lastError = new Error(`trace has ${trace.spans?.length ?? 0} spans`);
    } catch (error) {
      lastError = error;
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`trace ${traceId} did not become readable at ${url}: ${lastError}`);
}

function attr(span, key) {
  const attributes = span.attributes ?? {};
  return attributes[key];
}

function dashboardUrl(traceId, spanId) {
  const url = new URL(dashboardBaseUrl);
  url.searchParams.set("tenant", "demo");
  url.searchParams.set("project", "demo");
  url.searchParams.set("environment", "local");
  url.searchParams.set("trace", traceId);
  if (spanId) url.searchParams.set("span", spanId);
  return url.toString();
}

async function maybeProbeDashboard(traceId, spanId) {
  if (!process.env.BEATER_DASHBOARD_PROBE) return null;
  const url = dashboardUrl(traceId, spanId);
  const html = await fetchText(url);
  if (!html.includes("Agent Trace Debugger") || !html.includes(traceId)) {
    throw new Error(`dashboard response did not render the expected trace at ${url}`);
  }
  return url;
}

async function main() {
  if (!fs.existsSync(beater)) {
    throw new Error(`beater binary not found at ${beater}; run cargo build --bin beater first`);
  }
  const workdir = fs.mkdtempSync(path.join(os.tmpdir(), "beater-dashboard-trace-gate-"));
  let anthropic;
  let beaterd;
  try {
    anthropic = await startAnthropicServer();
    const app = path.join(workdir, "app");
    const beaterdData = path.join(workdir, "beaterd");
    writeApp(app);

    const httpPort = configuredPort("BEATERD_HTTP_PORT") ?? await freePort();
    const grpcPort = configuredPort("BEATERD_OTLP_GRPC_PORT") ?? await freePort();
    const apiBase = `http://127.0.0.1:${httpPort}`;
    beaterd = startBeaterd(beaterdData, httpPort, grpcPort);
    await waitForHealth(apiBase, beaterd);

    const env = {
      ...process.env,
      ANTHROPIC_API_KEY: "test-key",
      ANTHROPIC_BASE_URL: anthropic.base,
      BEATER_ANTHROPIC_ALLOW_INSECURE_LOOPBACK: "1",
      BEATER_TRACE_EXPORT_URL: apiBase,
      BEATER_TENANT_ID: "demo",
      BEATER_PROJECT_ID: "demo",
      BEATER_ENVIRONMENT_ID: "local",
    };
    if (process.platform === "darwin" && !env.DYLD_FRAMEWORK_PATH) {
      env.DYLD_FRAMEWORK_PATH = macPythonFrameworkPath;
    }
    delete env.BEATER_OTLP_EXPORT_URL;
    delete env.OTEL_EXPORTER_OTLP_ENDPOINT;
    delete env.OTEL_EXPORTER_OTLP_TRACES_ENDPOINT;
    delete env.BEATER_API_KEY;

    const result = await run(
      beater,
      ["agent", "run", "--app", app, "support", "prove Beater dashboard trace export"],
      { cwd: root, env },
    );
    const runId = /^run ([0-9a-f-]+)/m.exec(result.stdout)?.[1];
    if (!runId) throw new Error(`could not parse run id from output:\n${result.stdout}`);
    if (!result.stdout.includes("beater dashboard trace verified")) {
      throw new Error(`final agent text missing from output:\n${result.stdout}`);
    }
    if (anthropic.requests.length !== 2) {
      throw new Error(`expected 2 Anthropic requests, saw ${anthropic.requests.length}`);
    }

    const traceId = `beater-js-${runId}`;
    const trace = await waitForTrace(apiBase, traceId);
    const spans = trace.spans ?? [];
    const kinds = spans.map((span) => span.kind);
    if (
      !kinds.includes("agent.run") ||
      kinds.filter((kind) => kind === "llm.call").length !== 2 ||
      !kinds.includes("tool.call")
    ) {
      throw new Error(`unexpected Beater span kinds: ${kinds.join(", ")}`);
    }
    const tool = spans.find((span) => attr(span, "beater.tool_use_id") === "toolu_dashboard_get_time");
    if (!tool || attr(tool, "beater.tool_name") !== "get_time") {
      throw new Error(`missing get_time tool span in Beater trace: ${JSON.stringify(trace)}`);
    }

    const listUrl =
      `${apiBase}/v1/traces/demo?project_id=demo&environment_id=local` +
      `&trace_id=${encodeURIComponent(traceId)}&limit=50`;
    const list = await fetchJson(listUrl);
    const items = list.items ?? [];
    if (!items.some((item) => item.trace_id === traceId)) {
      throw new Error(`trace ${traceId} missing from dashboard trace list: ${JSON.stringify(list)}`);
    }

    const span = await fetchJson(
      `${apiBase}/v1/spans/demo/${encodeURIComponent(traceId)}/${encodeURIComponent(tool.span_id)}`,
    );
    if (span.span_id !== tool.span_id || span.kind !== "tool.call") {
      throw new Error(`dashboard span read returned wrong span: ${JSON.stringify(span)}`);
    }
    const io = await fetchJson(
      `${apiBase}/v1/spans/demo/${encodeURIComponent(traceId)}/${encodeURIComponent(tool.span_id)}/io`,
    );
    if (!io.input || !io.output) {
      throw new Error(`dashboard span I/O read missed input or output: ${JSON.stringify(io)}`);
    }

    const renderedDashboardUrl = await maybeProbeDashboard(traceId, tool.span_id);
    const url = renderedDashboardUrl ?? dashboardUrl(traceId, tool.span_id);
    console.log(
      `beater dashboard trace gate passed: run=${runId} trace=${traceId} spans=${spans.length}`,
    );
    console.log(`dashboard_url=${url}`);
    console.log(`beaterd=${beaterd.note}`);
  } finally {
    if (beaterd) await beaterd.stop();
    if (anthropic) await anthropic.close();
    if (!process.env.BEATER_KEEP_GATE_WORKDIR) {
      fs.rmSync(workdir, { recursive: true, force: true });
    } else {
      console.log(`kept gate workdir: ${workdir}`);
    }
  }
}

main().catch((error) => {
  console.error(error.stdout ?? "");
  console.error(error.stderr ?? "");
  console.error(error);
  process.exit(1);
});
