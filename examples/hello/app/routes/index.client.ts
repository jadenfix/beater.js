const root = document.querySelector<HTMLElement>("[data-beater-counter]");
const rscRoot = document.querySelector<HTMLElement>("[data-beater-rsc-root]");

if (root) {
  const button = root.querySelector<HTMLButtonElement>("[data-beater-increment]");
  const label = root.querySelector<HTMLElement>("[data-beater-count]");
  let count = Number(root.dataset.count ?? "0");

  const render = () => {
    root.dataset.state = "hydrated";
    root.dataset.count = String(count);
    if (button) {
      button.textContent = String(count);
      button.setAttribute("aria-label", `Client counter value ${count}`);
    }
    if (label) {
      label.textContent = `hydrated · ${count}`;
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
