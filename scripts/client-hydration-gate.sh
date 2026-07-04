#!/usr/bin/env bash
# Prove Phase C item 2: the page serves a route-scoped client bundle and a real
# browser can hydrate the SSR counter markup.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

APP="${BEATER_APP:-examples/hello}"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
BIN="${BEATER_BIN:-$TARGET_DIR/debug/beater}"
PORT="${BEATER_HYDRATION_PORT:-4180}"
LOG="${BEATER_HYDRATION_LOG:-$TARGET_DIR/client-hydration-gate.log}"
CHROME="${CHROME:-/Applications/Google Chrome.app/Contents/MacOS/Google Chrome}"

if [[ "${BEATER_SKIP_BUILD:-0}" != "1" ]]; then
  cargo build -p beater-cli
elif [[ ! -x "$BIN" ]]; then
  cargo build -p beater-cli
fi

if [[ ! -x "$CHROME" ]]; then
  echo "Chrome executable not found at $CHROME; set CHROME=/path/to/chrome" >&2
  exit 1
fi

mkdir -p "$(dirname "$LOG")"

"$BIN" dev "$APP" --port "$PORT" >"$LOG" 2>&1 &
pid=$!

cleanup() {
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
}
trap cleanup EXIT

python3 - "$PORT" "$LOG" <<'PY'
import socket
import sys
import time

port = int(sys.argv[1])
log = sys.argv[2]
deadline = time.monotonic() + 20
while time.monotonic() < deadline:
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=0.25):
            break
    except OSError:
        time.sleep(0.1)
else:
    print(f"server did not accept connections on {port}; log follows", file=sys.stderr)
    try:
        print(open(log, encoding="utf-8").read(), file=sys.stderr)
    except OSError:
        pass
    sys.exit(1)
PY

BASE_URL="http://127.0.0.1:$PORT" CHROME_PATH="$CHROME" node <<'JS'
const { spawn } = require("node:child_process");
const fs = require("node:fs");
const net = require("node:net");
const os = require("node:os");
const path = require("node:path");

const baseUrl = process.env.BASE_URL;
const chromePath = process.env.CHROME_PATH;

function fail(message) {
  throw new Error(message);
}

async function serverCheck() {
  const home = await fetch(`${baseUrl}/`);
  const html = await home.text();
  if (home.status !== 200) fail(`GET / returned ${home.status}`);
  const csp = home.headers.get("content-security-policy") || "";
  if (!csp.includes("script-src 'self' 'nonce-")) {
    fail(`page CSP does not allow the self-hosted nonce script path: ${csp}`);
  }
  if (csp.includes("script-src 'self' 'unsafe-inline'")) {
    fail(`page CSP should not allow arbitrary inline scripts: ${csp}`);
  }
  if (!html.includes("data-beater-counter")) {
    fail("SSR page did not include the hydration counter marker");
  }
  if (!html.includes('src="/_beater/client.js?route=%2F"')) {
    fail("SSR page did not reference /_beater/client.js for the route");
  }

  const bundle = await fetch(`${baseUrl}/_beater/client.js?route=%2F`);
  const source = await bundle.text();
  if (bundle.status !== 200) fail(`client bundle returned ${bundle.status}`);
  const contentType = bundle.headers.get("content-type") || "";
  if (!contentType.includes("application/javascript")) {
    fail(`client bundle content-type was ${contentType}`);
  }
  if (!source.includes("data-beater-counter")) {
    fail("client bundle did not contain the route counter hydrator");
  }
}

function freePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const port = server.address().port;
      server.close(() => resolve(port));
    });
  });
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function waitForVersion(port, chrome, stderr) {
  const url = `http://127.0.0.1:${port}/json/version`;
  const deadline = Date.now() + 10000;
  while (Date.now() < deadline) {
    if (chrome.exitCode !== null) {
      fail(`Chrome exited before CDP was ready:\n${stderr.join("")}`);
    }
    try {
      const response = await fetch(url);
      if (response.ok) return response.json();
    } catch {
      await sleep(100);
    }
  }
  fail(`Chrome CDP endpoint did not become ready:\n${stderr.join("")}`);
}

class CdpClient {
  constructor(wsUrl) {
    this.ws = new WebSocket(wsUrl);
    this.nextId = 1;
    this.pending = new Map();
    this.waiters = [];
    this.handlers = [];
  }

  async open() {
    await new Promise((resolve, reject) => {
      this.ws.addEventListener("open", resolve, { once: true });
      this.ws.addEventListener("error", reject, { once: true });
    });
    this.ws.addEventListener("message", (event) => this.handleMessage(event.data));
    this.ws.addEventListener("close", () => {
      for (const { reject } of this.pending.values()) {
        reject(new Error("CDP socket closed"));
      }
      this.pending.clear();
    });
  }

  handleMessage(data) {
    const message = JSON.parse(data);
    if (message.id) {
      const pending = this.pending.get(message.id);
      if (!pending) return;
      this.pending.delete(message.id);
      if (message.error) pending.reject(new Error(JSON.stringify(message.error)));
      else pending.resolve(message.result || {});
      return;
    }
    if (!message.method) return;
    for (const handler of this.handlers) {
      if (handler.method === message.method) handler.fn(message);
    }
    for (const waiter of [...this.waiters]) {
      if (waiter.method === message.method && waiter.predicate(message)) {
        this.waiters.splice(this.waiters.indexOf(waiter), 1);
        waiter.resolve(message);
      }
    }
  }

  on(method, fn) {
    this.handlers.push({ method, fn });
  }

  waitFor(method, predicate = () => true, timeoutMs = 5000) {
    return new Promise((resolve, reject) => {
      const waiter = { method, predicate, resolve, reject };
      const timer = setTimeout(() => {
        const index = this.waiters.indexOf(waiter);
        if (index !== -1) this.waiters.splice(index, 1);
        reject(new Error(`timed out waiting for ${method}`));
      }, timeoutMs);
      waiter.resolve = (message) => {
        clearTimeout(timer);
        resolve(message);
      };
      this.waiters.push(waiter);
    });
  }

  send(method, params = {}, sessionId = undefined) {
    const id = this.nextId++;
    const message = { id, method, params };
    if (sessionId) message.sessionId = sessionId;
    this.ws.send(JSON.stringify(message));
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
    });
  }

  close() {
    this.ws.close();
  }
}

async function runBrowserCheck() {
  const cdpPort = await freePort();
  const userDataDir = fs.mkdtempSync(path.join(os.tmpdir(), "beater-hydration-chrome-"));
  const stderr = [];
  const chrome = spawn(chromePath, [
    "--headless=new",
    "--disable-background-networking",
    "--disable-gpu",
    "--no-default-browser-check",
    "--no-first-run",
    `--remote-debugging-port=${cdpPort}`,
    `--user-data-dir=${userDataDir}`,
    "about:blank",
  ], { stdio: ["ignore", "ignore", "pipe"] });
  chrome.stderr.on("data", (chunk) => stderr.push(String(chunk)));

  let cdp;
  try {
    const version = await waitForVersion(cdpPort, chrome, stderr);
    cdp = new CdpClient(version.webSocketDebuggerUrl);
    await cdp.open();

    const cspMessages = [];
    cdp.on("Runtime.consoleAPICalled", (message) => {
      const text = (message.params.args || []).map((arg) => arg.value || arg.description || "").join(" ");
      if (/Content Security Policy|Refused to execute/i.test(text)) cspMessages.push(text);
    });
    cdp.on("Log.entryAdded", (message) => {
      const text = message.params.entry?.text || "";
      if (/Content Security Policy|Refused to execute/i.test(text)) cspMessages.push(text);
    });

    const target = await cdp.send("Target.createTarget", { url: "about:blank" });
    const attached = await cdp.send("Target.attachToTarget", {
      targetId: target.targetId,
      flatten: true,
    });
    const sessionId = attached.sessionId;
    await cdp.send("Runtime.enable", {}, sessionId);
    await cdp.send("Log.enable", {}, sessionId);
    await cdp.send("Page.enable", {}, sessionId);

    const load = cdp.waitFor("Page.loadEventFired", (message) => message.sessionId === sessionId, 10000);
    await cdp.send("Page.navigate", { url: `${baseUrl}/` }, sessionId);
    await load;

    async function evaluate(expression) {
      const result = await cdp.send("Runtime.evaluate", {
        expression,
        awaitPromise: true,
        returnByValue: true,
      }, sessionId);
      if (result.exceptionDetails) {
        fail(`browser evaluation failed: ${JSON.stringify(result.exceptionDetails)}`);
      }
      return result.result?.value;
    }

    async function waitForExpression(expression, timeoutMs = 5000) {
      const deadline = Date.now() + timeoutMs;
      while (Date.now() < deadline) {
        if (await evaluate(expression)) return;
        await sleep(50);
      }
      fail(`browser timed out waiting for ${expression}`);
    }

    await waitForExpression(`!!document.querySelector('[data-beater-counter][data-hydrated="true"]')`);
    await waitForExpression(`!!document.querySelector('[data-stream-marker="delayed"]')`);

    const before = await evaluate(`document.querySelector('[data-counter-value]').textContent`);
    const first = await evaluate(`(() => {
      document.querySelector('[data-counter-button]').click();
      return document.querySelector('[data-counter-value]').textContent;
    })()`);
    const second = await evaluate(`(() => {
      document.querySelector('[data-counter-button]').click();
      return document.querySelector('[data-counter-value]').textContent;
    })()`);
    const status = await evaluate(`window.__beaterHydrationStatus?.hydrated === true`);

    if (before !== "0" || first !== "1" || second !== "2") {
      fail(`counter did not increment through browser clicks: before=${before} first=${first} second=${second}`);
    }
    if (!status) fail("client bundle did not publish the hydration status marker");
    if (cspMessages.length > 0) {
      fail(`browser reported CSP execution errors:\n${cspMessages.join("\n")}`);
    }

    console.log(`client hydration gate passed: before=${before} after=${second}`);
  } finally {
    if (cdp) {
      try {
        await cdp.send("Browser.close");
      } catch {}
      cdp.close();
    }
    chrome.kill("SIGTERM");
    await new Promise((resolve) => chrome.once("exit", resolve)).catch(() => {});
    fs.rmSync(userDataDir, { recursive: true, force: true });
  }
}

(async () => {
  await serverCheck();
  await runBrowserCheck();
})().catch((error) => {
  console.error(error.stack || String(error));
  process.exit(1);
});
JS
