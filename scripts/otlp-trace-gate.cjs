#!/usr/bin/env node
const { spawn } = require("node:child_process");
const fs = require("node:fs");
const http = require("node:http");
const os = require("node:os");
const path = require("node:path");

const root = path.resolve(__dirname, "..");
const beater = process.env.BEATER_BIN ?? path.join(root, "target/debug/beater");
const macPythonFrameworkPath =
  "/Library/Developer/CommandLineTools/Library/Frameworks";

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
        delta: { type: "input_json_delta", partial_json: JSON.stringify(block.input ?? {}) },
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
          id: "toolu_otlp_get_time",
          name: "get_time",
          input: {},
        },
      ],
      stop_reason: "tool_use",
    },
    {
      content: [{ type: "text", text: "otlp trace verified" }],
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
        res.writeHead(500, {"content-type": "text/plain"});
        res.end("unexpected Anthropic request");
        return;
      }
      res.writeHead(200, {"content-type": "text/event-stream"});
      res.end(anthropicStream(response));
    });
  });
  return listen(server).then((port) => ({
    base: `http://127.0.0.1:${port}`,
    requests,
    close: () => new Promise((resolve) => server.close(resolve)),
  }));
}

function startOtlpCollector() {
  const requests = [];
  const server = http.createServer((req, res) => {
    let body = "";
    req.setEncoding("utf8");
    req.on("data", (chunk) => (body += chunk));
    req.on("end", () => {
      requests.push({ url: req.url, headers: req.headers, body });
      res.writeHead(200, {"content-type": "application/json"});
      res.end("{}");
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
name = "otlp-gate"
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

function allSpans(payload) {
  return (payload.resourceSpans ?? []).flatMap((resourceSpan) =>
    (resourceSpan.scopeSpans ?? []).flatMap((scopeSpan) => scopeSpan.spans ?? []),
  );
}

function attr(span, key) {
  const match = (span.attributes ?? []).find((candidate) => candidate.key === key);
  return match?.value?.stringValue ?? match?.value?.intValue ?? match?.value?.boolValue;
}

async function main() {
  if (!fs.existsSync(beater)) {
    throw new Error(`beater binary not found at ${beater}; run cargo build --bin beater first`);
  }
  const workdir = fs.mkdtempSync(path.join(os.tmpdir(), "beater-otlp-gate-"));
  let anthropic;
  let otlp;
  try {
    anthropic = await startAnthropicServer();
    otlp = await startOtlpCollector();
    const app = path.join(workdir, "app");
    writeApp(app);
    const env = {
      ...process.env,
      ANTHROPIC_API_KEY: "test-key",
      ANTHROPIC_BASE_URL: anthropic.base,
      BEATER_ANTHROPIC_ALLOW_INSECURE_LOOPBACK: "1",
      BEATER_OTLP_EXPORT_URL: otlp.base,
      BEATER_TENANT_ID: "tenant",
      BEATER_PROJECT_ID: "project",
      BEATER_ENVIRONMENT_ID: "local",
      BEATER_API_KEY: "trace-key",
    };
    if (process.platform === "darwin" && !env.DYLD_FRAMEWORK_PATH) {
      env.DYLD_FRAMEWORK_PATH = macPythonFrameworkPath;
    }
    delete env.BEATER_TRACE_EXPORT_URL;
    delete env.OTEL_EXPORTER_OTLP_ENDPOINT;
    delete env.OTEL_EXPORTER_OTLP_TRACES_ENDPOINT;
    const result = await run(
      beater,
      ["agent", "run", "--app", app, "support", "prove OTLP export"],
      { cwd: root, env },
    );
    const runId = /^run ([0-9a-f-]+)/m.exec(result.stdout)?.[1];
    if (!runId) throw new Error(`could not parse run id from output:\n${result.stdout}`);
    if (!result.stdout.includes("otlp trace verified")) {
      throw new Error(`final agent text missing from output:\n${result.stdout}`);
    }
    if (anthropic.requests.length !== 2) {
      throw new Error(`expected 2 Anthropic requests, saw ${anthropic.requests.length}`);
    }
    if (otlp.requests.length !== 1) {
      throw new Error(`expected 1 OTLP request, saw ${otlp.requests.length}`);
    }
    const request = otlp.requests[0];
    if (request.url !== "/v1/traces") {
      throw new Error(`unexpected OTLP path: ${request.url}`);
    }
    if (request.headers["x-beater-api-key"] !== "trace-key") {
      throw new Error("OTLP request missed x-beater-api-key header");
    }
    const payload = JSON.parse(request.body);
    const spans = allSpans(payload);
    if (spans.length !== 4) {
      throw new Error(`expected 4 OTLP spans, saw ${spans.length}: ${request.body}`);
    }
    const kinds = spans.map((span) => attr(span, "beater.span_kind")).filter(Boolean);
    if (
      !kinds.includes("agent.run") ||
      kinds.filter((kind) => kind === "llm.call").length !== 2 ||
      !kinds.includes("tool.call")
    ) {
      throw new Error(`unexpected OTLP span kinds: ${kinds.join(", ")}`);
    }
    const tool = spans.find((span) => attr(span, "beater.tool_use_id") === "toolu_otlp_get_time");
    if (!tool || attr(tool, "beater.tool_name") !== "get_time") {
      throw new Error(`missing get_time tool span: ${request.body}`);
    }
    if (tool.parentSpanId !== spans[0].spanId) {
      throw new Error("tool span is not parented to the run span");
    }
    console.log(`otlp trace gate passed: run=${runId} app=${app}`);
  } finally {
    if (anthropic) await anthropic.close();
    if (otlp) await otlp.close();
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
