#!/usr/bin/env node
const { spawn } = require("node:child_process");
const http = require("node:http");
const net = require("node:net");
const path = require("node:path");

let chromium;
try {
  ({ chromium } = require("playwright"));
} catch {
  console.error(
    "Playwright is required. Example:\n" +
      "  tmp=$(mktemp -d)\n" +
      "  npm install --prefix \"$tmp\" playwright\n" +
      "  NODE_PATH=\"$tmp/node_modules\" node scripts/client-hydration-gate.cjs",
  );
  process.exit(2);
}

const root = path.resolve(__dirname, "..");
const beater = process.env.BEATER_BIN ?? path.join(root, "target/debug/beater");
const app = process.env.BEATER_APP ?? path.join(root, "examples/hello");

(async () => {
  const port = Number(process.env.PORT ?? (await freePort()));
  const base = `http://127.0.0.1:${port}`;
  const env = { ...process.env };
  delete env.ANTHROPIC_API_KEY;
  delete env.BEATER_BASE_URL;
  delete env.BEATER_MCP_TOKEN;
  delete env.BEATER_MCP_TRUSTED_ORIGINS;
  const child = spawn(beater, ["dev", app, "--host", "127.0.0.1", "--port", String(port)], {
    cwd: root,
    env,
    stdio: ["ignore", "pipe", "pipe"],
  });
  let output = "";
  child.stdout.on("data", (chunk) => {
    output += chunk;
  });
  child.stderr.on("data", (chunk) => {
    output += chunk;
  });

  try {
    await waitForHttp(`${base}/api/health`);
    const browser = await chromium.launch({ headless: true });
    try {
      const page = await browser.newPage();
      const clientModule = page.waitForResponse(
        (response) =>
          response.url() === `${base}/_beater/client/index.js` && response.status() === 200,
      );
      await page.goto(base, { waitUntil: "networkidle" });
      await clientModule;
      await page.locator("[data-beater-increment]").click();
      const value = await page.locator("[data-beater-increment]").textContent();
      const label = await page.locator("[data-beater-count]").textContent();
      const hydrated = await page
        .locator("[data-beater-counter]")
        .evaluate((node) => node.getAttribute("data-state"));
      if (value !== "1" || hydrated !== "hydrated" || !label?.startsWith("hydrated")) {
        throw new Error(`counter did not hydrate: value=${value} state=${hydrated} label=${label}`);
      }
      console.log(`client hydration passed: ${base} counter incremented to ${value}`);
    } finally {
      await browser.close();
    }
  } catch (error) {
    console.error(output);
    console.error(error);
    process.exitCode = 1;
  } finally {
    child.kill("SIGTERM");
  }
})();

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

async function waitForHttp(url) {
  const started = Date.now();
  let lastError;
  while (Date.now() - started < 10_000) {
    try {
      const status = await statusCode(url);
      if (status >= 200 && status < 500) return;
    } catch (error) {
      lastError = error;
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw lastError ?? new Error(`timed out waiting for ${url}`);
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
