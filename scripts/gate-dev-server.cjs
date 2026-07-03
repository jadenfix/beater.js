const { spawn } = require("node:child_process");
const http = require("node:http");
const net = require("node:net");

const DEFAULT_RUST_LOG = "beater_runtime::server=info,info";

async function startBeaterDev({
  beater,
  app,
  root,
  env,
  choosePort = freePort,
  port,
  timeoutMs = 10_000,
  maxAttempts = 5,
}) {
  const explicitPort = port !== undefined && port !== null;
  if (explicitPort && !validPort(Number(port))) {
    throw new Error(`invalid explicit port: ${port}`);
  }
  let lastError;
  const attempts = explicitPort ? 1 : maxAttempts;
  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    const candidatePort = explicitPort ? Number(port) : await choosePort();
    const server = spawnServer({ beater, app, root, env, port: candidatePort });
    try {
      await waitForFreshServer(server, timeoutMs);
      return server;
    } catch (error) {
      await stopBeaterDev(server);
      error.output = server.output;
      lastError = error;
      if (explicitPort || !isAddressInUse(error, server.output)) {
        throw error;
      }
    }
  }
  throw lastError ?? new Error("beater dev did not start");
}

async function stopBeaterDev(server) {
  const { child } = server;
  if (!child.pid || server.closed || child.exitCode !== null) {
    await waitForChildClose(server);
    return;
  }
  child.kill("SIGTERM");
  await new Promise((resolve) => {
    const timer = setTimeout(() => {
      child.kill("SIGKILL");
      resolve();
    }, 2_000);
    server.closePromise.then(() => {
      clearTimeout(timer);
      resolve();
    });
  });
}

function spawnServer({ beater, app, root, env, port }) {
  const base = `http://127.0.0.1:${port}`;
  const childEnv = {
    ...env,
    RUST_LOG: env.BEATER_GATE_RUST_LOG ?? DEFAULT_RUST_LOG,
  };
  const child = spawn(beater, ["dev", app, "--host", "127.0.0.1", "--port", String(port)], {
    cwd: root,
    env: childEnv,
    stdio: ["ignore", "pipe", "pipe"],
  });
  let resolveClose;
  const closePromise = new Promise((resolve) => {
    resolveClose = resolve;
  });
  const server = {
    base,
    child,
    closed: null,
    closePromise,
    exited: null,
    output: "",
    port,
    spawnError: null,
  };
  child.stdout.on("data", (chunk) => {
    server.output += chunk;
  });
  child.stderr.on("data", (chunk) => {
    server.output += chunk;
  });
  child.once("exit", (code, signal) => {
    server.exited = { code, signal };
  });
  child.once("close", (code, signal) => {
    server.closed = { code, signal };
    server.exited ??= { code, signal };
    resolveClose();
  });
  child.once("error", (error) => {
    server.spawnError = error;
    resolveClose();
  });
  return server;
}

async function waitForFreshServer(server, timeoutMs) {
  const started = Date.now();
  const listening = `beater dev listening on http://127.0.0.1:${server.port}`;
  let lastError;
  while (Date.now() - started < timeoutMs) {
    if (server.spawnError) {
      throw server.spawnError;
    }
    if (server.exited) {
      await waitForChildClose(server);
      throw new Error(
        `beater dev exited before listening on ${server.base} ` +
          `(code=${server.exited.code} signal=${server.exited.signal})`,
      );
    }
    if (server.output.includes(listening)) {
      try {
        const status = await statusCode(`${server.base}/api/health`);
        if (status === 200) return;
      } catch (error) {
        lastError = error;
      }
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw lastError ?? new Error(`timed out waiting for fresh beater dev on ${server.base}`);
}

async function waitForChildClose(server) {
  if (server.closed || server.spawnError) return;
  await server.closePromise;
}

function freePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      const port = typeof address === "object" && address ? address.port : null;
      server.close(() => (port ? resolve(port) : reject(new Error("no port"))));
    });
    server.on("error", reject);
  });
}

function validPort(port) {
  return Number.isInteger(port) && port > 0 && port < 65536;
}

function isAddressInUse(error, output) {
  const text = `${error?.message ?? ""}\n${output ?? ""}`;
  return /Address already in use|address already in use|EADDRINUSE|os error 48|os error 98/.test(
    text,
  );
}

function statusCode(url) {
  return new Promise((resolve, reject) => {
    const req = http.get(url, (res) => {
      res.resume();
      resolve(res.statusCode ?? 0);
    });
    req.on("error", reject);
    req.setTimeout(2000, () => {
      req.destroy(new Error(`timeout GET ${url}`));
    });
  });
}

module.exports = {
  startBeaterDev,
  stopBeaterDev,
};
