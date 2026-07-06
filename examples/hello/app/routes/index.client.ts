import { clientBundleMarker, clientCounterLabel } from "../client/counter";

const root = document.querySelector<HTMLElement>("[data-beater-counter]");
const rscRoot = document.querySelector<HTMLElement>("[data-beater-rsc-root]");
const runEventsRoot = document.querySelector<HTMLElement>("[data-beater-run-events]");

if (root) {
  const button = root.querySelector<HTMLButtonElement>("[data-beater-increment]");
  const label = root.querySelector<HTMLElement>("[data-beater-count]");
  let count = Number(root.dataset.count ?? "0");

  const render = () => {
    root.dataset.state = "hydrated";
    root.dataset.bundle = clientBundleMarker;
    root.dataset.count = String(count);
    if (button) {
      button.textContent = String(count);
      button.setAttribute("aria-label", `Client counter value ${count}`);
    }
    if (label) {
      label.textContent = clientCounterLabel(count);
    }
  };

  button?.addEventListener("click", () => {
    count += 1;
    render();
  });

  render();
}

if (rscRoot) {
  type FlightState = {
    began: boolean;
    ended: boolean;
    html: string;
    decoder: TextDecoder;
  };

  const applyFrame = (line: string, state: FlightState) => {
    const kind = line[0];
    const payload = JSON.parse(line.slice(1));
    if (kind === "B") {
      state.began = payload.protocol === "beater-flight";
    } else if (kind === "H") {
      if (!Array.isArray(payload)) throw new Error("RSC HTML frame must contain bytes");
      state.html += state.decoder.decode(new Uint8Array(payload), { stream: true });
    } else if (kind === "E") {
      if (!payload.ok) throw new Error(payload.error ?? "RSC flight failed");
      state.html += state.decoder.decode();
      state.ended = true;
    }
  };

  const renderFlight = async () => {
    const response = await fetch("/_beater/rsc/index.flight", {
      headers: { accept: "text/x-component" },
    });
    if (!response.ok) throw new Error(`RSC flight returned ${response.status}`);
    if (!response.body) throw new Error("RSC flight response did not include a stream");

    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    const state: FlightState = { began: false, ended: false, html: "", decoder: new TextDecoder() };
    let buffer = "";
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });
      let newline = buffer.indexOf("\n");
      while (newline !== -1) {
        const line = buffer.slice(0, newline);
        buffer = buffer.slice(newline + 1);
        if (line) applyFrame(line, state);
        newline = buffer.indexOf("\n");
      }
    }
    buffer += decoder.decode();
    if (buffer) applyFrame(buffer, state);
    if (!state.began || !state.ended) throw new Error("RSC flight ended without complete framing");

    rscRoot.innerHTML = state.html;
    rscRoot.dataset.state = "ready";
  };

  renderFlight().catch((error) => {
    rscRoot.dataset.state = "error";
    rscRoot.textContent = error instanceof Error ? error.message : String(error);
  });
}

if (runEventsRoot) {
  type RunSummary = {
    id: string;
    agent: string;
    status: string;
    input: string;
    steps: number;
  };

  const form = runEventsRoot.querySelector<HTMLFormElement>("[data-run-events-form]");
  const input = runEventsRoot.querySelector<HTMLInputElement>("[data-run-id-input]");
  const status = runEventsRoot.querySelector<HTMLElement>("[data-run-events-status]");
  const log = runEventsRoot.querySelector<HTMLElement>("[data-run-events-log]");
  const history = runEventsRoot.querySelector<HTMLElement>("[data-run-history]");
  let source: EventSource | null = null;

  const setStatus = (value: string, state: string) => {
    runEventsRoot.dataset.state = state;
    if (status) status.textContent = value;
  };

  const appendLine = (value: string) => {
    if (!log) return;
    const line = document.createElement("div");
    line.textContent = value;
    log.append(line);
    log.scrollTop = log.scrollHeight;
  };

  const openRun = async (runId: string) => {
    source?.close();
    if (log) log.textContent = "";
    if (input) input.value = runId;
    setStatus("loading run", "connecting");

    const detail = await fetch(`/_beater/agent/runs/${encodeURIComponent(runId)}`, {
      headers: { accept: "application/json" },
    });
    if (!detail.ok) throw new Error(`run detail returned ${detail.status}`);
    const body = await detail.json();
    appendLine(`${body.run.agent} · ${body.run.status} · ${body.steps.length} steps`);
    for (const step of body.steps.slice(-3)) {
      appendLine(`#${step.seq} ${step.kind} ${step.status} partials=${step.partials}`);
    }

    setStatus("connecting", "connecting");
    source = new EventSource(`/_beater/agent/runs/${encodeURIComponent(runId)}/events`);
    source.addEventListener("open", () => setStatus("streaming", "streaming"));
    source.addEventListener("llm_partial", (event) => partialText(event as MessageEvent<string>));
    source.addEventListener("done", () => {
      setStatus("complete", "done");
      source?.close();
      source = null;
    });
    source.addEventListener("error", () => {
      setStatus("stream unavailable", "error");
      source?.close();
      source = null;
    });
  };

  const renderHistory = (runs: RunSummary[]) => {
    if (!history) return;
    history.textContent = "";
    if (runs.length === 0) {
      const empty = document.createElement("div");
      empty.className = "run-history-meta";
      empty.textContent = "no runs yet";
      history.append(empty);
      return;
    }
    for (const run of runs.slice(0, 4)) {
      const button = document.createElement("button");
      button.className = "run-history-item";
      button.type = "button";
      button.dataset.runId = run.id;

      const id = document.createElement("span");
      id.className = "run-history-id";
      id.textContent = run.id;

      const meta = document.createElement("span");
      meta.className = "run-history-meta";
      meta.textContent = `${run.agent} · ${run.status} · ${run.steps}`;

      button.append(id, meta);
      button.addEventListener("click", () => {
        openRun(run.id).catch((error) => {
          setStatus("run unavailable", "error");
          appendLine(error instanceof Error ? error.message : String(error));
        });
      });
      history.append(button);
    }
  };

  const partialText = (event: MessageEvent<string>) => {
    const parsed = JSON.parse(event.data);
    const payload = parsed.payload?.data ?? parsed.payload;
    const text = payload?.delta?.text ?? payload?.text;
    appendLine(typeof text === "string" ? text : `${parsed.kind} #${parsed.ordinal}`);
  };

  form?.addEventListener("submit", (event) => {
    event.preventDefault();
    const runId = input?.value.trim();
    if (!runId) {
      setStatus("enter a run id", "idle");
      return;
    }

    openRun(runId).catch((error) => {
      setStatus("run unavailable", "error");
      appendLine(error instanceof Error ? error.message : String(error));
    });
  });

  fetch("/_beater/agent/runs", { headers: { accept: "application/json" } })
    .then((response) => {
      if (!response.ok) throw new Error(`runs returned ${response.status}`);
      return response.json();
    })
    .then((body: { runs: RunSummary[] }) => {
      renderHistory(body.runs ?? []);
      setStatus((body.runs ?? []).length ? "ready" : "no runs", "idle");
    })
    .catch((error) => {
      setStatus("history unavailable", "error");
      appendLine(error instanceof Error ? error.message : String(error));
    });
}
