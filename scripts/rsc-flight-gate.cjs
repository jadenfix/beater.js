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
      "  NODE_PATH=\"$tmp/node_modules\" node scripts/rsc-flight-gate.cjs",
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
      const manifest = page.waitForResponse(
        (response) =>
          response.url() === `${base}/_beater/rsc/manifest.json` && response.status() === 200,
      );
      const flight = page.waitForResponse(
        (response) =>
          response.url() === `${base}/_beater/rsc/index.flight` && response.status() === 200,
      );
      await page.goto(base, { waitUntil: "networkidle" });
      const manifestResponse = await manifest;
      const manifestContentType = manifestResponse.headers()["content-type"] ?? "";
      if (!manifestContentType.includes("application/json")) {
        throw new Error(`RSC manifest content-type was ${manifestContentType}`);
      }
      const manifestPayload = await manifestResponse.json();
      const indexRoute = manifestPayload.routes?.find((route) => route.route === "/");
      if (
        manifestPayload.protocol !== "beater-rsc-manifest" ||
        indexRoute?.flight !== "/_beater/rsc/index.flight" ||
        indexRoute?.client !== "/_beater/client/index.js" ||
        indexRoute?.transport !== "beater-flight"
      ) {
        throw new Error(`RSC manifest did not advertise the index route: ${JSON.stringify(manifestPayload)}`);
      }
      const flightResponse = await flight;
      const contentType = flightResponse.headers()["content-type"] ?? "";
      if (!contentType.includes("text/x-component")) {
        throw new Error(`RSC flight content-type was ${contentType}`);
      }
      await page.locator('[data-beater-rsc-root][data-state="ready"]').waitFor();
      const rscText = await page.locator("[data-beater-rsc-root]").textContent();
      if (
        !rscText?.includes("server component flight") ||
        !rscText.includes("cafe Δ") ||
        !rscText.includes("delayed server fact")
      ) {
        throw new Error(`RSC island did not render expected server text: ${rscText}`);
      }

      await page.locator("[data-beater-increment]").click();
      const value = await page.locator("[data-beater-increment]").textContent();
      if (value !== "1") {
        throw new Error(`client counter did not remain hydrated after RSC render: ${value}`);
      }
      console.log(`RSC flight passed: ${base} advertised manifest, rendered server island, and counter=${value}`);
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
