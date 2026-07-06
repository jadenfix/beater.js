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

async function resolveRunnerSource() {
  const { stdout } = await run("cargo", ["metadata", "--format-version", "1", "--locked"], {
    cwd: root,
  });
  const metadata = JSON.parse(stdout);
  const pkg = metadata.packages.find((candidate) => candidate.name === "beater-browser-playwright");
  if (!pkg) throw new Error("beater-browser-playwright was not present in cargo metadata");
  return path.join(path.dirname(pkg.manifest_path), "runner");
}

async function prepareRunner(workdir) {
  const source = await resolveRunnerSource();
  const runner = path.join(workdir, "playwright-runner");
  fs.cpSync(source, runner, { recursive: true });
  await run("npm", ["install", "--ignore-scripts", "--silent"], {
    cwd: runner,
    stdio: "inherit",
  });
  await run(path.join(runner, "node_modules/.bin/playwright"), ["install", "chromium"], {
    cwd: runner,
    stdio: "inherit",
  });
  return path.join(runner, "runner.js");
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

function startFixtureServer() {
  const requests = [];
  const server = http.createServer((req, res) => {
    requests.push(req.url);
    res.writeHead(200, {"content-type": "text/html; charset=utf-8"});
    res.end(`<!doctype html>
<html>
  <head><title>Beater Playwright Gate</title></head>
  <body>
    <main id="checkout">checkout locked</main>
    <label>Password <input id="password" type="password" /></label>
    <button id="checkout-button">Pay</button>
    <script>
      const checkout = document.querySelector("#checkout");
      const password = document.querySelector("#password");
      if (sessionStorage.getItem("checkoutAuth") === "ok") {
        checkout.textContent = "authenticated checkout ready";
      }
      password.addEventListener("input", () => {
        if (password.value.length > 0) {
          sessionStorage.setItem("checkoutAuth", "ok");
          checkout.textContent = "authenticated checkout ready";
        }
      });
      document.querySelector("#checkout-button").addEventListener("click", () => {
        checkout.textContent = sessionStorage.getItem("checkoutAuth") === "ok"
          ? "authenticated checkout ready"
          : "checkout denied";
      });
    </script>
  </body>
</html>`);
  });
  return listen(server).then((port) => ({
    base: `http://127.0.0.1:${port}`,
    requests,
    close: () => new Promise((resolve) => server.close(resolve)),
  }));
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

function startAnthropicServer(fixtureBase) {
  const requests = [];
  const responses = [
    {
      content: [
        {
          type: "tool_use",
          id: "toolu_playwright_gate_password",
          name: "browser.checkout",
          input: {
            url: fixtureBase,
            action: "type",
            selector: "#password",
            textSecret: "password",
          },
        },
        {
          type: "tool_use",
          id: "toolu_playwright_gate_click",
          name: "browser.checkout",
          input: {
            url: fixtureBase,
            action: "click",
            selector: "#checkout-button",
          },
        },
        {
          type: "tool_use",
          id: "toolu_playwright_gate_extract",
          name: "browser.checkout",
          input: {
            url: fixtureBase,
            action: "extract",
            selector: "#checkout",
          },
        },
      ],
      stop_reason: "tool_use",
    },
    {
      content: [{ type: "text", text: "playwright browser task verified" }],
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

function writeApp(app, allowedOrigin) {
  fs.mkdirSync(path.join(app, "agents/support"), { recursive: true });
  fs.writeFileSync(
    path.join(app, "beater.toml"),
    `[app]
name = "playwright-gate"
port = 3000
`,
  );
  fs.writeFileSync(
    path.join(app, "agents/support/agent.ts"),
    `import { browserTool, defineAgent } from "beater:agent";

export default defineAgent({
  name: "support",
  system: "Use the browser checkout tool when asked.",
  tools: [
    browserTool("browser.checkout", {
      provider: "playwright",
      description: "Verify checkout in a browser.",
      inputSchema: {
        type: "object",
        properties: {
          url: {type: "string"},
          action: {type: "string"},
          selector: {type: "string"},
        },
        required: ["url"],
      },
      session: {scope: "run", cleanup: "always"},
      allowedOrigins: ["${allowedOrigin}"],
      secrets: {
        password: {type: "env", env: "BEATER_GATE_BROWSER_PASSWORD"},
      },
      timeoutMs: 30000,
      idempotent: false,
    }),
  ],
});
`,
  );
}

async function main() {
  if (!fs.existsSync(beater)) {
    throw new Error(`beater binary not found at ${beater}; run cargo build --bin beater first`);
  }
  const workdir = fs.mkdtempSync(path.join(os.tmpdir(), "beater-playwright-gate-"));
  let fixture;
  let anthropic;
  try {
    const runner = await prepareRunner(workdir);
    fixture = await startFixtureServer();
    anthropic = await startAnthropicServer(fixture.base);
    const app = path.join(workdir, "app");
    writeApp(app, fixture.base);
    const env = {
      ...process.env,
      ANTHROPIC_API_KEY: "test-key",
      ANTHROPIC_BASE_URL: anthropic.base,
      BEATER_ANTHROPIC_ALLOW_INSECURE_LOOPBACK: "1",
      BEATER_PLAYWRIGHT_RUNNER: runner,
      BEATER_GATE_BROWSER_PASSWORD: "gate-password",
    };
    if (process.platform === "darwin" && !env.DYLD_FRAMEWORK_PATH) {
      env.DYLD_FRAMEWORK_PATH = macPythonFrameworkPath;
    }
    delete env.BEATER_TRACE_EXPORT_URL;
    const result = await run(
      beater,
      ["agent", "run", "--app", app, "support", "verify checkout in a real browser"],
      { cwd: root, env },
    );
    const runId = /^run ([0-9a-f-]+)/m.exec(result.stdout)?.[1];
    if (!runId) throw new Error(`could not parse run id from output:\n${result.stdout}`);
    if (!result.stdout.includes("playwright browser task verified")) {
      throw new Error(`final agent text missing from output:\n${result.stdout}`);
    }
    const journal = path.join(app, ".beater/journal.db");
    const query = `SELECT json_object('status', status, 'tool_use_id', tool_use_id, 'result', result) FROM steps WHERE run_id='${runId}' AND kind='tool_call' AND tool_name='browser.checkout' ORDER BY seq;`;
    const { stdout } = await run("sqlite3", [journal, query], { cwd: root });
    const rows = stdout
      .trim()
      .split("\n")
      .filter(Boolean)
      .map((line) => JSON.parse(line));
    if (rows.length !== 3) {
      throw new Error(`expected three browser tool rows, saw ${rows.length}: ${stdout}`);
    }
    if (JSON.stringify(rows).includes("gate-password")) {
      throw new Error("browser secret leaked into journal rows");
    }
    const payloads = rows.map((row) => {
      const toolResult = JSON.parse(row.result);
      return JSON.parse(toolResult.content);
    });
    for (const [index, row] of rows.entries()) {
      const providerPayload = payloads[index];
      if (
        row.status !== "completed" ||
        providerPayload.provider !== "playwright" ||
        providerPayload.session?.id !== runId ||
        providerPayload.session?.scope !== "run" ||
        providerPayload.outcome?.status !== "ok" ||
        providerPayload.title !== "Beater Playwright Gate"
      ) {
        throw new Error(
          `unexpected journal browser result: ${JSON.stringify({ row, providerPayload })}`,
        );
      }
    }
    if (
      payloads[0].session?.calls !== 1 ||
      payloads[0].session?.reused !== false ||
      payloads[1].session?.calls !== 2 ||
      payloads[1].session?.reused !== true ||
      payloads[2].session?.calls !== 3 ||
      payloads[2].session?.reused !== true
    ) {
      throw new Error(`browser session was not reused: ${JSON.stringify(payloads)}`);
    }
    if (payloads[0].outcome?.action?.args?.text !== "<redacted:password>") {
      throw new Error(`browser secret action was not redacted: ${JSON.stringify(payloads[0])}`);
    }
    if (!String(payloads[2].text ?? "").includes("authenticated checkout ready")) {
      throw new Error(`authenticated checkout state missing: ${JSON.stringify(payloads[2])}`);
    }
    if (anthropic.requests.length !== 2) {
      throw new Error(`expected 2 Anthropic requests, saw ${anthropic.requests.length}`);
    }
    if (!fixture.requests.includes("/")) {
      throw new Error(`fixture server was not visited: ${fixture.requests.join(", ")}`);
    }
    console.log(`playwright browser gate passed: run=${runId} app=${app}`);
  } finally {
    if (anthropic) await anthropic.close();
    if (fixture) await fixture.close();
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
