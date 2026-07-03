#!/usr/bin/env node
const path = require("node:path");
const { startBeaterDev, stopBeaterDev } = require("./gate-dev-server.cjs");

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
  const env = { ...process.env };
  delete env.ANTHROPIC_API_KEY;
  delete env.BEATER_BASE_URL;
  delete env.BEATER_MCP_TOKEN;
  delete env.BEATER_MCP_TRUSTED_ORIGINS;
  let server;
  try {
    server = await startBeaterDev({
      app,
      beater,
      env,
      port: process.env.PORT ? Number(process.env.PORT) : undefined,
      root,
    });
    const { base } = server;
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
    console.error(server?.output ?? error.output ?? "");
    console.error(error);
    process.exitCode = 1;
  } finally {
    if (server) await stopBeaterDev(server);
  }
})();
