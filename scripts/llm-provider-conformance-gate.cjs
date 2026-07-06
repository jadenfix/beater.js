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
      id: response.id ?? "msg_provider_conformance",
      type: "message",
      role: "assistant",
      model: response.model ?? "mock-anthropic",
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
    } else {
      out += sse("content_block_start", {
        type: "content_block_start",
        index,
        content_block: startBlock,
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

function openaiStream(chunks) {
  return `${chunks.map((chunk) => `data: ${JSON.stringify(chunk)}\n\n`).join("")}data: [DONE]\n\n`;
}

function startAnthropicServer(toolName) {
  const requests = [];
  const responses = [
    {
      content: [
        {
          type: "tool_use",
          id: "toolu_anthropic_provider_gate",
          name: toolName,
          input: { numbers: [3, 1, 4, 1, 5] },
        },
      ],
      stop_reason: "tool_use",
    },
    {
      content: [{ type: "text", text: "anthropic provider gate verified mean 2.8" }],
      stop_reason: "end_turn",
    },
  ];
  const server = http.createServer((req, res) => {
    let body = "";
    req.setEncoding("utf8");
    req.on("data", (chunk) => (body += chunk));
    req.on("end", () => {
      const parsed = JSON.parse(body);
      requests.push({ url: req.url, headers: req.headers, body: parsed });
      if (req.url !== "/v1/messages") {
        res.writeHead(404, { "content-type": "text/plain" });
        res.end("unexpected Anthropic path");
        return;
      }
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

function startOpenAiServer(originalToolName) {
  const requests = [];
  let providerToolName = null;
  const server = http.createServer((req, res) => {
    let body = "";
    req.setEncoding("utf8");
    req.on("data", (chunk) => (body += chunk));
    req.on("end", () => {
      const parsed = JSON.parse(body);
      requests.push({ url: req.url, headers: req.headers, body: parsed });
      if (req.url !== "/v1/chat/completions") {
        res.writeHead(404, { "content-type": "text/plain" });
        res.end("unexpected OpenAI-compatible path");
        return;
      }
      if (requests.length === 1) {
        providerToolName = parsed.tools?.[0]?.function?.name;
        if (!providerToolName || providerToolName === originalToolName) {
          res.writeHead(500, { "content-type": "text/plain" });
          res.end("OpenAI-compatible tool name was not provider-sanitized");
          return;
        }
        res.writeHead(200, { "content-type": "text/event-stream" });
        res.end(
          openaiStream([
            {
              id: "chatcmpl_provider_gate_1",
              object: "chat.completion.chunk",
              model: parsed.model,
              choices: [
                {
                  index: 0,
                  delta: {
                    tool_calls: [
                      {
                        index: 0,
                        type: "function",
                        function: {
                          name: providerToolName,
                          arguments: JSON.stringify({ numbers: [3, 1, 4, 1, 5] }),
                        },
                      },
                    ],
                  },
                  finish_reason: "tool_calls",
                },
              ],
            },
          ]),
        );
        return;
      }
      res.writeHead(200, { "content-type": "text/event-stream" });
      res.end(
        openaiStream([
          {
            id: "chatcmpl_provider_gate_2",
            object: "chat.completion.chunk",
            model: parsed.model,
            choices: [
              {
                index: 0,
                delta: { content: "openai-compatible provider gate verified mean 2.8" },
                finish_reason: "stop",
              },
            ],
          },
        ]),
      );
    });
  });
  return listen(server).then((port) => ({
    base: `http://127.0.0.1:${port}/v1`,
    requests,
    get providerToolName() {
      return providerToolName;
    },
    close: () => new Promise((resolve) => server.close(resolve)),
  }));
}

function writeApp(app, { provider, model, toolName }) {
  fs.mkdirSync(path.join(app, "agents/support/tools"), { recursive: true });
  fs.writeFileSync(
    path.join(app, "beater.toml"),
    `[app]
name = "provider-conformance-gate"
port = 3000
`,
  );
  fs.writeFileSync(
    path.join(app, "agents/support/agent.ts"),
    `import { defineAgent, pyTool } from "beater:agent";

export default defineAgent({
  name: "support",
  provider: "${provider}",
  model: "${model}",
  system: "Always call the declared summarize tool for numeric summaries.",
  tools: [
    pyTool("${toolName}", "./tools/summarize_numbers.py", {
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

function gateEnv(overrides) {
  const env = {
    PATH: process.env.PATH,
    HOME: process.env.HOME,
    TMPDIR: process.env.TMPDIR,
    TMP: process.env.TMP,
    TEMP: process.env.TEMP,
    RUST_BACKTRACE: process.env.RUST_BACKTRACE,
    ...overrides,
  };
  if (process.platform === "darwin" && !env.DYLD_FRAMEWORK_PATH) {
    env.DYLD_FRAMEWORK_PATH = macPythonFrameworkPath;
  }
  delete env.BEATER_TRACE_EXPORT_URL;
  delete env.BEATER_OTLP_EXPORT_URL;
  delete env.OTEL_EXPORTER_OTLP_ENDPOINT;
  delete env.OTEL_EXPORTER_OTLP_TRACES_ENDPOINT;
  return env;
}

async function runAgent(app, env, expectedText) {
  const result = await run(
    beater,
    ["agent", "run", "--app", app, "support", "summarize 3,1,4,1,5 with the tool"],
    { cwd: root, env },
  );
  const runId = /^run ([0-9a-f-]+)/m.exec(result.stdout)?.[1];
  if (!runId) throw new Error(`could not parse run id from output:\n${result.stdout}`);
  if (!result.stdout.includes(expectedText)) {
    throw new Error(`final agent text missing from output:\n${result.stdout}`);
  }
  return { runId, stdout: result.stdout };
}

async function journalRows(app, runId, toolName, expectedText) {
  const journal = path.join(app, ".beater/journal.db");
  const query = `SELECT json_object('kind', kind, 'status', status, 'tool_name', tool_name, 'tool_use_id', tool_use_id, 'result', result) FROM steps WHERE run_id='${runId}' ORDER BY seq;`;
  const { stdout } = await run("sqlite3", [journal, query], { cwd: root });
  const rows = stdout
    .trim()
    .split("\n")
    .filter(Boolean)
    .map((line) => JSON.parse(line));
  if (rows.length !== 3) {
    throw new Error(`expected llm/tool/llm journal shape, saw ${rows.length} rows: ${stdout}`);
  }
  const [firstLlm, toolRow, finalLlm] = rows;
  const shape = rows.map((row) => `${row.kind}:${row.status}`);
  const expectedShape = ["llm_call:completed", "tool_call:completed", "llm_call:completed"];
  if (shape.join("|") !== expectedShape.join("|")) {
    throw new Error(`unexpected journal shape ${shape.join(", ")}: ${stdout}`);
  }
  const toolRows = rows.filter((row) => row.kind === "tool_call" && row.tool_name === toolName);
  if (toolRows.length !== 1) {
    throw new Error(`expected one completed ${toolName} tool row, saw ${toolRows.length}: ${stdout}`);
  }
  if (toolRow.status !== "completed") {
    throw new Error(`tool row was not completed: ${JSON.stringify(toolRow)}`);
  }
  const firstPayload = JSON.parse(firstLlm.result);
  const firstTool = firstPayload.content?.find((block) => block.type === "tool_use");
  if (
    firstPayload.stop_reason !== "tool_use" ||
    !firstTool ||
    firstTool.name !== toolName ||
    firstTool.id !== toolRow.tool_use_id ||
    firstTool.input?.numbers?.join(",") !== "3,1,4,1,5"
  ) {
    throw new Error(`first LLM result is not canonical tool_use: ${firstLlm.result}`);
  }
  const toolResult = JSON.parse(toolRow.result);
  const payload = JSON.parse(toolResult.content);
  if (payload.mean !== 2.8 || payload.count !== 5) {
    throw new Error(`unexpected tool result payload: ${JSON.stringify(payload)}`);
  }
  const finalPayload = JSON.parse(finalLlm.result);
  const finalText = finalPayload.content?.map((block) => block.text ?? "").join("");
  if (finalPayload.stop_reason !== "end_turn" || !finalText.includes(expectedText)) {
    throw new Error(`final LLM result is not canonical end_turn text: ${finalLlm.result}`);
  }
  const partialQuery = `SELECT COUNT(*) FROM step_partials WHERE run_id='${runId}';`;
  const { stdout: partialStdout } = await run("sqlite3", [journal, partialQuery], { cwd: root });
  const partials = Number(partialStdout.trim());
  if (!Number.isFinite(partials) || partials < 2) {
    throw new Error(`expected LLM partial rows for ${runId}, saw ${partialStdout}`);
  }
  return { rows, toolRow, partials, shape };
}

function assertAnthropicRequests(server, toolName) {
  if (server.requests.length !== 2) {
    throw new Error(`expected 2 Anthropic requests, saw ${server.requests.length}`);
  }
  const first = server.requests[0].body;
  if (first.model !== "mock-anthropic" || first.stream !== true) {
    throw new Error(`unexpected Anthropic first request: ${JSON.stringify(first)}`);
  }
  if (server.requests.some((request) => request.url !== "/v1/messages")) {
    throw new Error(`unexpected Anthropic paths: ${server.requests.map((request) => request.url).join(", ")}`);
  }
  if (!first.tools?.some((tool) => tool.name === toolName)) {
    throw new Error(`Anthropic request did not expose ${toolName}: ${JSON.stringify(first.tools)}`);
  }
  const second = server.requests[1].body;
  const blocks = second.messages?.flatMap((message) => message.content ?? []) ?? [];
  const toolResult = blocks.find(
    (block) => block.type === "tool_result" && block.tool_use_id === "toolu_anthropic_provider_gate",
  );
  const toolPayload = toolResult?.content ? JSON.parse(toolResult.content) : null;
  if (!toolResult || toolPayload?.mean !== 2.8 || toolPayload?.count !== 5) {
    throw new Error(`Anthropic second request missed canonical tool_result: ${JSON.stringify(second)}`);
  }
}

function assertOpenAiRequests(server, originalToolName) {
  if (server.requests.length !== 2) {
    throw new Error(`expected 2 OpenAI-compatible requests, saw ${server.requests.length}`);
  }
  const first = server.requests[0].body;
  const providerToolName = server.providerToolName;
  if (first.model !== "mock-openai" || first.stream !== true) {
    throw new Error(`unexpected OpenAI first request: ${JSON.stringify(first)}`);
  }
  if (
    !providerToolName ||
    providerToolName === originalToolName ||
    !/^[A-Za-z0-9_-]{1,64}$/.test(providerToolName)
  ) {
    throw new Error(`OpenAI-compatible provider tool name was not sanitized: ${providerToolName}`);
  }
  const second = server.requests[1].body;
  const assistant = second.messages?.find((message) => Array.isArray(message.tool_calls));
  const toolMessage = second.messages?.find((message) => message.role === "tool");
  const call = assistant?.tool_calls?.[0];
  if (!call || call.function?.name !== providerToolName) {
    throw new Error(`OpenAI second request missed provider tool call: ${JSON.stringify(second)}`);
  }
  if (!String(call.id ?? "").startsWith("toolu_openai_")) {
    throw new Error(`OpenAI fallback tool id was not synthesized: ${JSON.stringify(call)}`);
  }
  const toolPayload = toolMessage?.content ? JSON.parse(toolMessage.content) : null;
  if (!toolMessage || toolMessage.tool_call_id !== call.id || toolPayload?.mean !== 2.8) {
    throw new Error(`OpenAI second request missed matching tool result: ${JSON.stringify(second)}`);
  }
}

async function runAnthropicGate(workdir) {
  const toolName = "summarize_numbers";
  const app = path.join(workdir, "anthropic-app");
  writeApp(app, { provider: "anthropic", model: "mock-anthropic", toolName });
  const server = await startAnthropicServer(toolName);
  try {
    const env = gateEnv({
      BEATER_LLM_PROVIDER: "anthropic",
      BEATER_LLM_MODEL: "mock-anthropic",
      ANTHROPIC_API_KEY: "test-key",
      ANTHROPIC_BASE_URL: server.base,
      BEATER_ANTHROPIC_ALLOW_INSECURE_LOOPBACK: "1",
    });
    const { runId } = await runAgent(app, env, "anthropic provider gate verified mean 2.8");
    const journal = await journalRows(app, runId, toolName, "anthropic provider gate verified mean 2.8");
    if (journal.toolRow.tool_use_id !== "toolu_anthropic_provider_gate") {
      throw new Error(`Anthropic tool_use_id changed unexpectedly: ${journal.toolRow.tool_use_id}`);
    }
    assertAnthropicRequests(server, toolName);
    return { runId, app, partials: journal.partials, shape: journal.shape };
  } finally {
    await server.close();
  }
}

async function runOpenAiGate(workdir) {
  const toolName = "math.summarize/numbers";
  const app = path.join(workdir, "openai-app");
  writeApp(app, { provider: "openai-compatible", model: "mock-openai", toolName });
  const server = await startOpenAiServer(toolName);
  try {
    const env = gateEnv({
      BEATER_LLM_PROVIDER: "openai-compatible",
      BEATER_LLM_MODEL: "mock-openai",
      BEATER_OPENAI_API_KEY: "test-key",
      BEATER_OPENAI_BASE_URL: server.base,
      BEATER_OPENAI_ALLOW_INSECURE_LOOPBACK: "1",
    });
    const { runId } = await runAgent(app, env, "openai-compatible provider gate verified mean 2.8");
    const journal = await journalRows(app, runId, toolName, "openai-compatible provider gate verified mean 2.8");
    if (!String(journal.toolRow.tool_use_id ?? "").startsWith("toolu_openai_")) {
      throw new Error(`OpenAI fallback tool_use_id missing from journal: ${journal.toolRow.tool_use_id}`);
    }
    assertOpenAiRequests(server, toolName);
    return { runId, app, partials: journal.partials, shape: journal.shape, providerToolName: server.providerToolName };
  } finally {
    await server.close();
  }
}

async function main() {
  if (!fs.existsSync(beater)) {
    throw new Error(`beater binary not found at ${beater}; run cargo build --bin beater first`);
  }
  const workdir = fs.mkdtempSync(path.join(os.tmpdir(), "beater-llm-provider-gate-"));
  try {
    const anthropic = await runAnthropicGate(workdir);
    const openai = await runOpenAiGate(workdir);
    if (anthropic.shape.join("|") !== openai.shape.join("|")) {
      throw new Error(
        `provider journal shapes diverged: anthropic=${anthropic.shape.join(", ")} openai=${openai.shape.join(", ")}`,
      );
    }
    console.log(
      `llm provider conformance gate passed: anthropic_run=${anthropic.runId} openai_run=${openai.runId} openai_tool=${openai.providerToolName}`,
    );
  } finally {
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
