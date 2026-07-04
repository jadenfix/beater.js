// Phase C item 1: streamed React SSR from the embedded isolate — no Node anywhere.

import { Suspense } from "react";

export const agent = {
  title: "beater.js Runtime Console",
  description: "Public, crawl-safe operational home for the hello example app.",
  crawl: true,
};

export function client() {
  const root = document.querySelector("[data-beater-counter]");
  if (!root) return;
  const button = root.querySelector("[data-counter-button]");
  const value = root.querySelector("[data-counter-value]");
  if (!button || !value) return;

  let count = Number(root.getAttribute("data-initial-count") || "0");
  const render = () => {
    value.textContent = String(count);
    button.setAttribute("aria-label", `Hydration counter value ${count}`);
    root.setAttribute("data-count", String(count));
  };

  button.addEventListener("click", () => {
    count += 1;
    render();
  });
  root.setAttribute("data-hydrated", "true");
  window.__beaterHydrationStatus = { route: "/", hydrated: true };
  render();
}

const css = `
:root {
  color-scheme: light;
  --ink: #1d1f1b;
  --muted: #666b5f;
  --line: #d8dcd0;
  --surface: #fbfcf7;
  --surface-2: #f1f4ec;
  --teal: #087f73;
  --green: #2f8f46;
  --amber: #c98612;
  --red: #b84d4d;
  --blue: #2c6fbb;
  --violet: #7357a5;
}

* {
  box-sizing: border-box;
}

html {
  min-height: 100%;
  background: var(--surface-2);
  color: var(--ink);
  font-family:
    Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
  letter-spacing: 0;
}

body {
  min-height: 100%;
  margin: 0;
  background:
    linear-gradient(90deg, rgba(29, 31, 27, 0.035) 1px, transparent 1px),
    linear-gradient(0deg, rgba(29, 31, 27, 0.035) 1px, transparent 1px),
    var(--surface-2);
  background-size: 28px 28px;
}

a {
  color: inherit;
  text-decoration: none;
}

.shell {
  width: min(1440px, 100%);
  margin: 0 auto;
  padding: 18px;
}

.topbar {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  gap: 16px;
  align-items: center;
  min-height: 58px;
  padding: 0 2px 16px;
  border-bottom: 1px solid var(--line);
}

.brand {
  display: flex;
  min-width: 0;
  align-items: center;
  gap: 12px;
}

.mark {
  display: grid;
  width: 38px;
  aspect-ratio: 1;
  flex: 0 0 auto;
  place-items: center;
  border: 1px solid #22251f;
  border-radius: 8px;
  background: #232720;
  color: #f7f8f1;
  font-weight: 800;
}

.brand-title {
  margin: 0;
  font-size: clamp(26px, 4vw, 54px);
  font-weight: 820;
  line-height: 0.95;
}

.brand-copy {
  margin: 4px 0 0;
  color: var(--muted);
  font-size: 14px;
}

.nav {
  display: flex;
  flex-wrap: wrap;
  justify-content: flex-end;
  gap: 8px;
}

.nav a,
.chip {
  display: inline-flex;
  min-height: 34px;
  align-items: center;
  gap: 8px;
  border: 1px solid var(--line);
  border-radius: 8px;
  background: rgba(251, 252, 247, 0.86);
  color: #31362e;
  padding: 7px 10px;
  font-size: 13px;
  font-weight: 650;
  white-space: normal;
}

.status-dot {
  width: 8px;
  aspect-ratio: 1;
  border-radius: 999px;
  background: var(--green);
}

.grid {
  display: grid;
  grid-template-columns: minmax(0, 1.08fr) minmax(340px, 0.92fr);
  gap: 16px;
  padding-top: 16px;
}

.panel {
  border: 1px solid var(--line);
  border-radius: 8px;
  background: rgba(251, 252, 247, 0.92);
  box-shadow: 0 18px 48px rgba(31, 37, 27, 0.08);
}

.overview {
  display: grid;
  min-height: 640px;
  grid-template-rows: auto auto minmax(0, 1fr);
  overflow: hidden;
}

.headline {
  display: grid;
  gap: 20px;
  padding: clamp(24px, 5vw, 52px);
  border-bottom: 1px solid var(--line);
}

.headline h2 {
  max-width: 980px;
  margin: 0;
  font-size: clamp(42px, 8vw, 112px);
  font-weight: 860;
  line-height: 0.92;
}

.headline p {
  max-width: 760px;
  margin: 0;
  color: #4c5348;
  font-size: clamp(17px, 2vw, 22px);
  line-height: 1.45;
}

.counter-panel {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  gap: 14px;
  align-items: center;
  max-width: 680px;
  border: 1px solid #b9c2b0;
  border-radius: 8px;
  background: #fffef8;
  padding: 14px;
}

.counter-copy {
  display: grid;
  min-width: 0;
  gap: 4px;
}

.counter-copy strong {
  color: #242820;
  font-size: 15px;
}

.counter-copy span {
  color: var(--muted);
  font-size: 13px;
  line-height: 1.35;
}

.counter-panel button {
  display: inline-flex;
  min-width: 128px;
  min-height: 42px;
  align-items: center;
  justify-content: center;
  gap: 8px;
  border: 1px solid #096b62;
  border-radius: 8px;
  background: var(--teal);
  color: #f7fffb;
  cursor: pointer;
  font: inherit;
  font-size: 14px;
  font-weight: 780;
}

.counter-panel button:focus-visible {
  outline: 3px solid rgba(8, 127, 115, 0.28);
  outline-offset: 2px;
}

.counter-panel button:hover {
  background: #096b62;
}

.counter-panel button strong {
  min-width: 1ch;
  font-variant-numeric: tabular-nums;
}

.signal-row {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  border-bottom: 1px solid var(--line);
}

.signal {
  min-height: 112px;
  padding: 18px;
  border-right: 1px solid var(--line);
}

.signal:last-child {
  border-right: 0;
}

.signal strong {
  display: block;
  font-size: 24px;
}

.signal span {
  display: block;
  margin-top: 8px;
  color: var(--muted);
  font-size: 13px;
  line-height: 1.35;
}

.console {
  display: grid;
  grid-template-columns: minmax(0, 0.84fr) minmax(280px, 0.56fr);
  min-height: 330px;
}

.topology {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 12px;
  min-height: 360px;
  padding: 22px;
  overflow: hidden;
  border-right: 1px solid var(--line);
  background:
    linear-gradient(90deg, rgba(8, 127, 115, 0.08), transparent 46%),
    linear-gradient(180deg, rgba(201, 134, 18, 0.12), transparent 48%),
    #f9fbf3;
}

.rail {
  grid-column: 1 / -1;
  height: 2px;
  background: #afb9aa;
}

.node {
  position: relative;
  display: grid;
  min-width: 0;
  min-height: 104px;
  gap: 8px;
  align-content: center;
  border: 1px solid #b8c0b1;
  border-radius: 8px;
  background: #fffef8;
  padding: 14px;
}

.node strong {
  font-size: 15px;
}

.node span {
  color: var(--muted);
  font-size: 12px;
  line-height: 1.3;
}

.node-a { border-top: 4px solid var(--blue); }
.node-b { border-top: 4px solid var(--teal); }
.node-c { border-top: 4px solid var(--amber); }
.node-d { grid-column: 2 / 4; border-top: 4px solid var(--violet); }

.run-card {
  grid-column: 1 / -1;
  border: 1px solid #cad0c3;
  border-radius: 8px;
  background: #252921;
  color: #f9fbf3;
  padding: 16px;
}

.run-card code {
  display: block;
  overflow: hidden;
  color: #cdebd4;
  font-family: "SFMono-Regular", Consolas, monospace;
  font-size: 13px;
  line-height: 1.5;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.side-stack {
  display: grid;
  grid-template-rows: repeat(3, 1fr);
}

.lane {
  display: grid;
  gap: 9px;
  align-content: center;
  min-height: 118px;
  padding: 20px;
  border-bottom: 1px solid var(--line);
}

.lane:last-child {
  border-bottom: 0;
}

.lane h3,
.section-title,
.agent h3 {
  margin: 0;
  font-size: 14px;
  font-weight: 820;
  text-transform: uppercase;
}

.lane p {
  margin: 0;
  color: var(--muted);
  font-size: 14px;
  line-height: 1.45;
}

.lane small {
  color: #3f463a;
  font-weight: 720;
}

.agent-panel {
  display: grid;
  gap: 16px;
}

.agent {
  padding: 22px;
}

.agent-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 14px;
  margin-bottom: 18px;
}

.endpoint-list {
  display: grid;
  gap: 8px;
}

.endpoint {
  display: grid;
  grid-template-columns: 72px minmax(0, 1fr) auto;
  gap: 10px;
  align-items: center;
  min-height: 48px;
  border: 1px solid var(--line);
  border-radius: 8px;
  background: #fffef8;
  padding: 8px 10px;
}

.method {
  color: var(--teal);
  font-family: "SFMono-Regular", Consolas, monospace;
  font-size: 12px;
  font-weight: 800;
}

.path {
  min-width: 0;
  overflow: hidden;
  color: #242820;
  font-family: "SFMono-Regular", Consolas, monospace;
  font-size: 13px;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.badge {
  border: 1px solid #cbd3c3;
  border-radius: 999px;
  color: #4d5549;
  padding: 4px 8px;
  font-size: 12px;
  font-weight: 760;
}

.agent-run {
  display: grid;
  gap: 12px;
  padding: 22px;
  background: #252921;
  color: #f9fbf3;
}

.agent-run .section-title {
  color: #edf4e8;
}

.steps {
  display: grid;
  gap: 10px;
}

.step {
  display: grid;
  grid-template-columns: 30px minmax(0, 1fr);
  gap: 10px;
  align-items: start;
}

.step b {
  display: grid;
  width: 26px;
  aspect-ratio: 1;
  place-items: center;
  border: 1px solid #60705e;
  border-radius: 8px;
  color: #cdebd4;
  font-size: 12px;
}

.step span {
  color: #dfe6d8;
  font-size: 14px;
  line-height: 1.35;
}

.footer-grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 16px;
  margin-top: 16px;
}

.mini {
  min-height: 156px;
  padding: 22px;
}

.mini p {
  margin: 10px 0 0;
  color: var(--muted);
  font-size: 14px;
  line-height: 1.45;
}

@media (max-width: 980px) {
  .topbar,
  .grid,
  .console,
  .footer-grid {
    grid-template-columns: 1fr;
  }

  .nav {
    justify-content: flex-start;
  }

  .overview {
    min-height: auto;
  }

  .signal-row {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }

  .signal:nth-child(2) {
    border-right: 0;
  }

  .topology {
    border-right: 0;
    border-bottom: 1px solid var(--line);
  }
}

@media (max-width: 620px) {
  .shell {
    padding: 12px;
  }

  .headline {
    padding: 24px;
  }

  .signal-row {
    grid-template-columns: 1fr;
  }

  .signal {
    border-right: 0;
  }

  .endpoint {
    grid-template-columns: 1fr;
  }

  .counter-panel {
    grid-template-columns: 1fr;
  }

  .counter-panel button {
    width: 100%;
  }

  .topology {
    grid-template-columns: 1fr;
    min-height: auto;
  }

  .node {
    min-height: 96px;
  }

  .node-d,
  .run-card,
  .rail {
    grid-column: 1;
  }
}
`;

type EndpointRow = readonly [method: string, path: string, badge: string];

const endpoints: readonly EndpointRow[] = [
  ["GET", "/api/health", "V8"],
  ["POST", "/mcp", "MCP"],
  ["GET", "/llms.txt", "crawl"],
  ["GET", "/.well-known/beater.json", "manifest"],
] as const;

function Endpoint({ method, path, badge }: { method: string; path: string; badge: string }) {
  return (
    <a className="endpoint" href={method === "GET" ? path : "/mcp"}>
      <span className="method">{method}</span>
      <span className="path">{path}</span>
      <span className="badge">{badge}</span>
    </a>
  );
}

function Signal({ value, label }: { value: string; label: string }) {
  return (
    <div className="signal">
      <strong>{value}</strong>
      <span>{label}</span>
    </div>
  );
}

function Lane({ title, body, meta }: { title: string; body: string; meta: string }) {
  return (
    <div className="lane">
      <small>{meta}</small>
      <h3>{title}</h3>
      <p>{body}</p>
    </div>
  );
}

function HydrationCounter() {
  return (
    <div className="counter-panel" data-beater-counter data-initial-count="0" data-count="0">
      <span className="counter-copy">
        <strong>Client hydration probe</strong>
        <span>Server markup upgrades into a route-scoped browser bundle.</span>
      </span>
      <button type="button" data-counter-button aria-label="Hydration counter value 0">
        Count <strong data-counter-value>0</strong>
      </button>
    </div>
  );
}

type PageRequest = {
  id: string;
  path: string;
  scriptNonce: string | null;
};

type DelayRecord = {
  ready: boolean;
  promise: Promise<void>;
};

const delayedByRequest = new Map<string, DelayRecord>();

function waitForDelayedSubtree(requestId: string) {
  let record = delayedByRequest.get(requestId);
  if (!record) {
    record = {
      ready: false,
      promise: Promise.resolve(),
    };
    record.promise = new Promise((resolve) => {
      setTimeout(() => {
        record!.ready = true;
        resolve();
      }, 450);
    });
    delayedByRequest.set(requestId, record);
  }
  if (!record.ready) throw record.promise;
}

function DelayedStreamingSubtree({ requestId }: { requestId: string }) {
  waitForDelayedSubtree(requestId);
  delayedByRequest.delete(requestId);
  return (
    <p id="stream-delayed" data-stream-marker="delayed">
      Suspense-delayed subtree flushed after the shell.
    </p>
  );
}

export default function Home({ request }: { request: PageRequest }) {
  return (
    <html lang="en">
      <head>
        <title>beater.js — runtime console</title>
        <meta name="viewport" content="width=device-width, initial-scale=1" />
        <meta name="theme-color" content="#f1f4ec" />
        <style>{css}</style>
      </head>
      <body>
        <main className="shell">
          <header className="topbar">
            <div className="brand">
              <div className="mark">b</div>
              <div>
                <h1 className="brand-title">beater.js</h1>
                <p className="brand-copy">Rendered server-side at {request.path}</p>
              </div>
            </div>
            <nav className="nav" aria-label="Runtime links">
              <a href="/api/health"><span className="status-dot" />Health</a>
              <a href="/mcp">MCP</a>
              <a href="/llms.txt">llms.txt</a>
              <a href="/.well-known/beater.json">Manifest</a>
            </nav>
          </header>

          <section className="grid" aria-label="Runtime overview">
            <div className="panel overview">
              <div className="headline">
                <span className="chip"><span className="status-dot" />One process · three runtimes · agent-ready</span>
                <h2>Build the web UI and the agent loop in one place.</h2>
                <p>
                  TypeScript routes, React SSR, durable Rust agent runs, Python tools,
                  and MCP discovery are served from the same local runtime.
                </p>
                <HydrationCounter />
                <Suspense
                  fallback={
                    <p id="stream-shell" data-stream-marker="shell">
                      Streaming shell flushed before the delayed subtree.
                    </p>
                  }
                >
                  <DelayedStreamingSubtree requestId={request.id} />
                </Suspense>
              </div>

              <div className="signal-row">
                <Signal value="V8" label="TS and React routes execute in an embedded isolate." />
                <Signal value="Rust" label="Agent runs journal every step for durable resume." />
                <Signal value="CPython" label="Tools call into the ML ecosystem without a sidecar." />
                <Signal value="MCP" label="Agents discover tools and crawl surfaces automatically." />
              </div>

              <div className="console">
                <div className="topology" aria-label="Runtime topology">
                  <div className="rail" />
                  <div className="node node-a">
                    <strong>Route table</strong>
                    <span>file paths become HTTP and crawlable surfaces</span>
                  </div>
                  <div className="node node-b">
                    <strong>V8 isolate</strong>
                    <span>hot-reloaded TypeScript and React SSR</span>
                  </div>
                  <div className="node node-c">
                    <strong>Agent journal</strong>
                    <span>runs rebuild state from committed journal steps</span>
                  </div>
                  <div className="node node-d">
                    <strong>Python tools</strong>
                    <span>registered capabilities exposed through MCP</span>
                  </div>
                  <div className="run-card">
                    <code>$ beater dev examples/hello</code>
                    <code>GET /api/health → {"{ ok: true }"}</code>
                    <code>POST /mcp → tools/list</code>
                  </div>
                </div>

                <div className="side-stack">
                  <Lane
                    meta="web lane"
                    title="SSR without Node"
                    body="The page you are reading was rendered by React inside the embedded runtime."
                  />
                  <Lane
                    meta="agent lane"
                    title="Crash-aware runs"
                    body="Every model turn and tool call is recorded before the next step starts."
                  />
                  <Lane
                    meta="tool lane"
                    title="ML-native tools"
                    body="Python functions run alongside the web app while Rust owns orchestration."
                  />
                </div>
              </div>
            </div>

            <aside className="agent-panel" aria-label="Agent access panel">
              <section className="panel agent">
                <div className="agent-head">
                  <h3>Agent surfaces</h3>
                  <span className="chip"><span className="status-dot" />online</span>
                </div>
                <div className="endpoint-list">
                  {endpoints.map(([method, path, badge]) => (
                    <Endpoint key={path} method={method} path={path} badge={badge} />
                  ))}
                </div>
              </section>

              <section className="panel agent-run">
                <h3 className="section-title">Support agent run</h3>
                <div className="steps">
                  <div className="step">
                    <b>1</b>
                    <span>Prompt is converted into a durable run with a journal id.</span>
                  </div>
                  <div className="step">
                    <b>2</b>
                    <span>Tool calls are written before execution, then completed or parked.</span>
                  </div>
                  <div className="step">
                    <b>3</b>
                    <span>Resume rebuilds the transcript from completed journal steps.</span>
                  </div>
                </div>
              </section>

              <section className="panel mini">
                <h3 className="section-title">Current app</h3>
                <p>
                  `examples/hello` is intentionally small: one React route, one health API,
                  one support agent, and Python tools that prove the polyglot loop.
                </p>
              </section>
            </aside>
          </section>

          <section className="footer-grid" aria-label="Runtime details">
            <div className="panel mini">
              <h3 className="section-title">No plugin maze</h3>
              <p>
                The runtime already owns routes, agent metadata, tool schemas, crawl files,
                and MCP. The app surface stays coherent because those outputs share one source.
              </p>
            </div>
            <div className="panel mini">
              <h3 className="section-title">Cloud neutral by default</h3>
              <p>
                `beater dev` is a local binary with embedded runtimes. The deployment target is
                a single host process, not a bundle of queues and sidecars.
              </p>
            </div>
          </section>
        </main>
        <script
          type="module"
          src={`/_beater/client.js?route=${encodeURIComponent(request.path)}`}
          nonce={request.scriptNonce ?? undefined}
        />
      </body>
    </html>
  );
}
