#!/usr/bin/env node
const { spawn } = require("node:child_process");
const fs = require("node:fs");
const http = require("node:http");
const os = require("node:os");
const path = require("node:path");
const { startBeaterDev, stopBeaterDev } = require("./gate-dev-server.cjs");

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
    child.stdout?.on("data", (chunk) => (stdout += chunk));
    child.stderr?.on("data", (chunk) => (stderr += chunk));
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

function readBody(req) {
  return new Promise((resolve, reject) => {
    let body = "";
    req.setEncoding("utf8");
    req.on("data", (chunk) => (body += chunk));
    req.on("error", reject);
    req.on("end", () => resolve(body));
  });
}

function writeJson(res, status, body, headers = {}) {
  res.writeHead(status, { "content-type": "application/json", ...headers });
  res.end(JSON.stringify(body));
}

function startRemoteMcpServer() {
  const token = "remote-mcp-fixture-token";
  const sessions = new Set();
  const requests = [];
  const issuedSessions = [];
  let nextSession = 1;
  const server = http.createServer(async (req, res) => {
    const bodyText = await readBody(req);
    let body = null;
    try {
      body = bodyText ? JSON.parse(bodyText) : null;
    } catch (_error) {
      writeJson(res, 400, { error: "invalid json" });
      return;
    }
    const record = {
      method: req.method,
      url: req.url,
      headers: req.headers,
      body,
    };
    requests.push(record);
    if (req.method !== "POST" || req.url !== "/mcp") {
      writeJson(res, 404, { error: "not found" });
      return;
    }
    if (req.headers.authorization !== `Bearer ${token}`) {
      writeJson(res, 401, { error: "missing bearer token" });
      return;
    }
    if (body?.jsonrpc !== "2.0") {
      writeJson(res, 400, { error: "bad jsonrpc" });
      return;
    }
    if (body.method === "initialize") {
      const session = `remote-session-${nextSession++}`;
      sessions.add(session);
      issuedSessions.push({ id: body.id, session });
      writeJson(
        res,
        200,
        {
          jsonrpc: "2.0",
          id: body.id,
          result: {
            protocolVersion: "2025-11-25",
            capabilities: { tools: {} },
            serverInfo: { name: "remote-mcp-provider-gate", version: "0.0.0" },
          },
        },
        { "mcp-session-id": session },
      );
      return;
    }
    const session = req.headers["mcp-session-id"];
    if (!session || !sessions.has(session)) {
      writeJson(res, 400, {
        jsonrpc: "2.0",
        id: body.id,
        error: { code: -32000, message: "missing or invalid session" },
      });
      return;
    }
    if (body.method === "tools/list") {
      writeJson(res, 200, {
        jsonrpc: "2.0",
        id: body.id,
        result: {
          tools: [
            {
              name: "lookup",
              description: "Look up a CRM contact from a remote MCP provider.",
              inputSchema: {
                type: "object",
                properties: {
                  email: { type: "string", format: "email" },
                },
                required: ["email"],
              },
            },
          ],
        },
      });
      return;
    }
    if (body.method === "tools/call") {
      const idempotencyKey = req.headers["idempotency-key"];
      if (!idempotencyKey) {
        writeJson(res, 400, {
          jsonrpc: "2.0",
          id: body.id,
          error: { code: -32000, message: "missing idempotency key" },
        });
        return;
      }
      if (body.params?.name !== "lookup") {
        writeJson(res, 400, {
          jsonrpc: "2.0",
          id: body.id,
          error: { code: -32602, message: "wrong provider tool name" },
        });
        return;
      }
      writeJson(res, 200, {
        jsonrpc: "2.0",
        id: body.id,
        result: {
          content: [
            {
              type: "text",
              text: JSON.stringify({
                email: body.params?.arguments?.email,
                source: "remote-mcp-provider-gate",
                session,
                idempotencyKey,
              }),
            },
          ],
          isError: false,
        },
      });
      return;
    }
    writeJson(res, 404, {
      jsonrpc: "2.0",
      id: body.id,
      error: { code: -32601, message: "method not found" },
    });
  });
  return listen(server).then((port) => ({
    endpoint: `http://127.0.0.1:${port}/mcp`,
    egress: `127.0.0.1:${port}`,
    token,
    requests,
    issuedSessions,
    close: () => new Promise((resolve) => server.close(resolve)),
  }));
}

function writeApp(app, remote) {
  fs.mkdirSync(path.join(app, "agents/support"), { recursive: true });
  fs.mkdirSync(path.join(app, "app/routes"), { recursive: true });
  fs.mkdirSync(path.join(app, "app/routes/api"), { recursive: true });
  fs.writeFileSync(
    path.join(app, "beater.toml"),
    `[app]
name = "remote-mcp-provider-gate"
port = 3000
`,
  );
  fs.writeFileSync(
    path.join(app, "app/routes/index.tsx"),
    `export default function Page() {
  return <main>remote MCP provider gate</main>;
}
`,
  );
  fs.writeFileSync(
    path.join(app, "app/routes/api/health.ts"),
    `export function GET() {
  return {
    status: 200,
    headers: {"content-type": "application/json"},
    body: JSON.stringify({ok: true, runtime: "beater.js"}),
  };
}
`,
  );
  fs.writeFileSync(
    path.join(app, "agents/support/agent.ts"),
    `import { defineAgent, remoteMcpProvider } from "beater:agent";

export default defineAgent({
  name: "support",
  system: "Expose the remote CRM provider through MCP.",
  tools: [
    remoteMcpProvider("crm", {
      endpoint: "${remote.endpoint}",
      auth: {type: "bearer", env: "REMOTE_MCP_GATE_TOKEN"},
      timeoutMs: 5000,
      retry: {attempts: 2, backoffMs: 1, idempotencyKey: "tool_use_id"},
      session: {scope: "run", cleanup: "always"},
      egress: ["${remote.egress}"],
      idempotent: true,
    }),
  ],
});
`,
  );
}

function devEnv(remote) {
  const env = {
    ...process.env,
    REMOTE_MCP_GATE_TOKEN: remote.token,
    BEATER_MCP_TOKEN: "local-mcp-fixture-token",
  };
  if (process.platform === "darwin" && !env.DYLD_FRAMEWORK_PATH) {
    env.DYLD_FRAMEWORK_PATH = macPythonFrameworkPath;
  }
  delete env.ANTHROPIC_API_KEY;
  delete env.BEATER_LLM_API_KEY;
  delete env.BEATER_LLM_BASE_URL;
  return env;
}

function httpRequest(port, method, urlPath, body, headers = {}) {
  return new Promise((resolve, reject) => {
    const payload = body === undefined ? undefined : JSON.stringify(body);
    const req = http.request(
      {
        hostname: "127.0.0.1",
        port,
        path: urlPath,
        method,
        headers: {
          ...(payload
            ? {
                "content-type": "application/json",
                "content-length": Buffer.byteLength(payload),
              }
            : {}),
          ...headers,
        },
      },
      (res) => {
        let text = "";
        res.setEncoding("utf8");
        res.on("data", (chunk) => (text += chunk));
        res.on("end", () => resolve({ status: res.statusCode, headers: res.headers, text }));
      },
    );
    req.once("error", reject);
    req.setTimeout(5000, () => {
      req.destroy(new Error(`timeout ${method} ${urlPath}`));
    });
    if (payload) req.write(payload);
    req.end();
  });
}

async function mcp(port, id, method, params) {
  const response = await httpRequest(
    port,
    "POST",
    "/mcp",
    { jsonrpc: "2.0", id, method, params: params ?? {} },
    {
      authorization: "Bearer local-mcp-fixture-token",
      "mcp-protocol-version": "2025-11-25",
    },
  );
  if (response.status !== 200) {
    throw new Error(`local /mcp ${method} returned ${response.status}: ${response.text}`);
  }
  const body = JSON.parse(response.text);
  if (body.error) {
    throw new Error(`local /mcp ${method} returned JSON-RPC error: ${response.text}`);
  }
  return body;
}

async function sqliteJson(db, query) {
  const { stdout } = await run("sqlite3", [db, query], { cwd: root });
  return stdout
    .trim()
    .split("\n")
    .filter(Boolean)
    .map((line) => JSON.parse(line));
}

function assertNoSecretLeak(value, secrets) {
  const text = typeof value === "string" ? value : JSON.stringify(value);
  for (const secret of secrets) {
    if (text.includes(secret)) {
      throw new Error("secret leaked into MCP response or journal");
    }
  }
}

async function main() {
  if (!fs.existsSync(beater)) {
    throw new Error(`beater binary not found at ${beater}; run cargo build -p beater-cli first`);
  }
  const workdir = fs.mkdtempSync(path.join(os.tmpdir(), "beater-remote-mcp-provider-gate-"));
  let remote;
  let dev;
  try {
    remote = await startRemoteMcpServer();
    const app = path.join(workdir, "app");
    writeApp(app, remote);
    dev = await startBeaterDev({
      beater,
      app,
      root,
      env: devEnv(remote),
      timeoutMs: 30_000,
    });
    const port = dev.port;

    const unauth = await httpRequest(port, "POST", "/mcp", {
      jsonrpc: "2.0",
      id: "unauth",
      method: "tools/list",
      params: {},
    });
    if (unauth.status !== 401) {
      throw new Error(`expected unauthenticated local /mcp to return 401, got ${unauth.status}`);
    }
    const badAuth = await httpRequest(
      port,
      "POST",
      "/mcp",
      {
        jsonrpc: "2.0",
        id: "bad-auth",
        method: "tools/list",
        params: {},
      },
      { authorization: "Bearer wrong-local-token" },
    );
    if (badAuth.status !== 401) {
      throw new Error(`expected wrong local /mcp bearer token to return 401, got ${badAuth.status}`);
    }

    const init = await mcp(port, "init", "initialize", {});
    if (init.result?.protocolVersion !== "2025-11-25") {
      throw new Error(`unexpected initialize result: ${JSON.stringify(init)}`);
    }
    assertNoSecretLeak(init, [remote.token, "local-mcp-fixture-token"]);

    const list = await mcp(port, "list", "tools/list", {});
    assertNoSecretLeak(list, [remote.token, "local-mcp-fixture-token"]);
    const tool = list.result?.tools?.find((candidate) => candidate.name === "crm.lookup");
    if (!tool) {
      throw new Error(`crm.lookup was not imported from remote provider: ${JSON.stringify(list)}`);
    }
    if (tool.description !== "Look up a CRM contact from a remote MCP provider.") {
      throw new Error(`unexpected imported tool description: ${JSON.stringify(tool)}`);
    }
    if (tool.inputSchema?.properties?.email?.format !== "email") {
      throw new Error(`unexpected imported tool schema: ${JSON.stringify(tool)}`);
    }

    const call = await mcp(port, "call", "tools/call", {
      name: "crm.lookup",
      arguments: { email: "ada@example.test" },
    });
    const text = call.result?.content?.[0]?.text;
    const payload = JSON.parse(text);
    if (
      call.result?.isError !== false ||
      payload.email !== "ada@example.test" ||
      payload.source !== "remote-mcp-provider-gate"
    ) {
      throw new Error(`unexpected tools/call result: ${JSON.stringify(call)}`);
    }
    assertNoSecretLeak(call, [remote.token, "local-mcp-fixture-token"]);

    const discoveryInit = remote.requests.find((request) => request.body?.id === "beater:discover:init");
    const discoveryList = remote.requests.find((request) => request.body?.id === "beater:discover:tools");
    const executeInit = remote.requests.find((request) => request.body?.id === "beater:init");
    const executeCalls = remote.requests.filter((request) => request.body?.method === "tools/call");
    if (executeCalls.length !== 1) {
      throw new Error(`expected exactly one remote tools/call, saw ${executeCalls.length}`);
    }
    const executeCall = executeCalls[0];
    for (const [label, request] of [
      ["discovery initialize", discoveryInit],
      ["discovery tools/list", discoveryList],
      ["execution initialize", executeInit],
      ["execution tools/call", executeCall],
    ]) {
      if (!request) throw new Error(`missing remote ${label} request: ${JSON.stringify(remote.requests)}`);
      if (request.headers.authorization !== `Bearer ${remote.token}`) {
        throw new Error(`remote ${label} did not receive bearer auth`);
      }
    }
    const discoverySession = remote.issuedSessions.find(
      (session) => session.id === "beater:discover:init",
    )?.session;
    const executionSession = remote.issuedSessions.find(
      (session) => session.id === "beater:init",
    )?.session;
    if (!discoverySession || !executionSession || discoverySession === executionSession) {
      throw new Error(`unexpected issued remote sessions: ${JSON.stringify(remote.issuedSessions)}`);
    }
    if (discoveryList.headers["mcp-session-id"] !== discoverySession) {
      throw new Error("remote discovery tools/list did not receive its initialize session id");
    }
    if (executeCall.headers["mcp-session-id"] !== executionSession) {
      throw new Error("remote tools/call did not receive its execution initialize session id");
    }
    const remoteCallId = executeCall.body?.id;
    const idempotencyKey = executeCall.headers["idempotency-key"];
    if (!String(remoteCallId).startsWith("beater:mcp:")) {
      throw new Error(`remote tools/call did not receive synthetic MCP tool id: ${remoteCallId}`);
    }
    if (!String(idempotencyKey).startsWith("beater:") || !String(idempotencyKey).endsWith(remoteCallId)) {
      throw new Error(`remote tools/call did not receive journaled idempotency key: ${idempotencyKey}`);
    }
    if (executeCall.body?.params?.name !== "lookup") {
      throw new Error(
        `remote tools/call used local imported name instead of provider name: ${JSON.stringify(executeCall.body)}`,
      );
    }

    const journal = path.join(app, ".beater/journal.db");
    const rows = await sqliteJson(
      journal,
      `SELECT json_object('run_id', run_id, 'seq', seq, 'kind', kind, 'status', status, 'tool_name', tool_name, 'tool_use_id', tool_use_id, 'request', request, 'result', result) FROM steps WHERE kind='tool_call' AND tool_name='crm.lookup' ORDER BY seq;`,
    );
    if (rows.length !== 1) {
      throw new Error(`expected one journaled crm.lookup tool row, saw ${rows.length}`);
    }
    const row = rows[0];
    if (row.status !== "completed" || row.tool_use_id !== remoteCallId) {
      throw new Error(`unexpected journal row: ${JSON.stringify(row)}`);
    }
    const requestPayload = JSON.parse(row.request);
    if (requestPayload.idempotency_key !== idempotencyKey) {
      throw new Error(`journal idempotency key did not match remote header: ${row.request}`);
    }
    const resultPayload = JSON.parse(row.result);
    const resultContent = JSON.parse(resultPayload.content);
    if (resultContent.email !== "ada@example.test") {
      throw new Error(`unexpected journal result payload: ${row.result}`);
    }
    const runs = await sqliteJson(
      journal,
      `SELECT json_object('id', id, 'agent', agent, 'status', status, 'input', input) FROM runs WHERE id='${row.run_id}';`,
    );
    if (runs.length !== 1 || runs[0].agent !== "mcp" || runs[0].status !== "completed") {
      throw new Error(`unexpected synthetic MCP run: ${JSON.stringify(runs)}`);
    }
    assertNoSecretLeak({ row, runs }, [remote.token, "local-mcp-fixture-token"]);

    console.log(
      `remote MCP provider gate passed: app=${app} local=http://127.0.0.1:${port}/mcp remote=${remote.endpoint}`,
    );
  } finally {
    if (dev) await stopBeaterDev(dev);
    if (remote) await remote.close();
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
  console.error(error.stack ?? error);
  process.exit(1);
});
