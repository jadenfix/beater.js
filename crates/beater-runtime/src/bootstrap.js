// beater.js isolate bootstrap: minimal web-ish shims + the dispatch glue.
// deno_core is not Deno — nothing exists here except what we add.
((core) => {
  const ops = core.ops;

  function fmt(x) {
    if (typeof x === "string") return x;
    if (x instanceof Error) return x.stack ?? String(x);
    try {
      return JSON.stringify(x);
    } catch {
      return String(x);
    }
  }

  globalThis.console = {
    log: (...args) => core.print(args.map(fmt).join(" ") + "\n", false),
    info: (...args) => core.print(args.map(fmt).join(" ") + "\n", false),
    warn: (...args) => core.print(args.map(fmt).join(" ") + "\n", true),
    error: (...args) => core.print(args.map(fmt).join(" ") + "\n", true),
    debug: (...args) => core.print(args.map(fmt).join(" ") + "\n", false),
  };

  let nextTimerId = 1;
  const cancelledTimers = new Set();
  globalThis.setTimeout = (cb, ms = 0, ...args) => {
    const id = nextTimerId++;
    ops.op_beater_sleep(ms).then(() => {
      if (!cancelledTimers.delete(id)) cb(...args);
    });
    return id;
  };
  globalThis.clearTimeout = (id) => {
    cancelledTimers.add(id);
  };
  globalThis.setInterval = (cb, ms = 0, ...args) => {
    const id = nextTimerId++;
    (async () => {
      while (true) {
        await ops.op_beater_sleep(ms);
        if (cancelledTimers.has(id)) {
          cancelledTimers.delete(id);
          return;
        }
        cb(...args);
      }
    })();
    return id;
  };
  globalThis.clearInterval = globalThis.clearTimeout;

  globalThis.queueMicrotask ??= (cb) => {
    Promise.resolve().then(cb);
  };

  globalThis.performance ??= { now: () => Date.now() };

  // Minimal UTF-8 TextEncoder — enough for React SSR and stream chunk
  // encoding. Full web API fidelity comes with the npm-compat era.
  if (!globalThis.TextEncoder) {
    const utf8Bytes = (c) => {
      if (c < 0x80) return [c];
      if (c < 0x800) return [0xc0 | (c >> 6), 0x80 | (c & 63)];
      if (c < 0x10000)
        return [0xe0 | (c >> 12), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63)];
      return [
        0xf0 | (c >> 18),
        0x80 | ((c >> 12) & 63),
        0x80 | ((c >> 6) & 63),
        0x80 | (c & 63),
      ];
    };
    globalThis.TextEncoder = class TextEncoder {
      get encoding() {
        return "utf-8";
      }
      encode(input = "") {
        const out = [];
        for (let i = 0; i < input.length; i++) {
          let c = input.codePointAt(i);
          if (c > 0xffff) i++;
          out.push(...utf8Bytes(c));
        }
        return new Uint8Array(out);
      }
      encodeInto(src, dest) {
        let read = 0;
        let written = 0;
        for (let i = 0; i < src.length; ) {
          const c = src.codePointAt(i);
          const units = c > 0xffff ? 2 : 1;
          const bytes = utf8Bytes(c);
          if (written + bytes.length > dest.length) break;
          dest.set(bytes, written);
          written += bytes.length;
          i += units;
          read = i;
        }
        return { read, written };
      }
    };
  }
  if (!globalThis.ReadableStream) {
    globalThis.ReadableStream = class ReadableStream {
      constructor(source = {}) {
        this._source = source;
        this._queue = [];
        this._pending = [];
        this._closed = false;
        this._error = null;
        this._locked = false;
        this._pulling = false;
        this._controller = {
          get desiredSize() {
            return 1;
          },
          enqueue: (chunk) => {
            if (this._closed || this._error) return;
            const pending = this._pending.shift();
            if (pending) pending.resolve({ done: false, value: chunk });
            else this._queue.push(chunk);
          },
          close: () => {
            if (this._closed) return;
            this._closed = true;
            for (const pending of this._pending.splice(0)) {
              pending.resolve({ done: true, value: undefined });
            }
          },
          error: (error) => {
            if (this._error) return;
            this._error = error ?? new Error("ReadableStream errored");
            for (const pending of this._pending.splice(0)) {
              pending.reject(this._error);
            }
          },
        };
        Promise.resolve()
          .then(() => this._source.start?.(this._controller))
          .catch((error) => this._controller.error(error));
      }

      getReader() {
        if (this._locked) throw new TypeError("ReadableStream is locked");
        this._locked = true;
        return {
          read: () => this._read(),
          cancel: (reason) => this.cancel(reason),
          releaseLock: () => {
            this._locked = false;
          },
        };
      }

      cancel(reason) {
        this._queue.length = 0;
        this._controller.close();
        return Promise.resolve(this._source.cancel?.(reason));
      }

      _read() {
        if (this._queue.length) {
          return Promise.resolve({ done: false, value: this._queue.shift() });
        }
        if (this._error) return Promise.reject(this._error);
        if (this._closed) {
          return Promise.resolve({ done: true, value: undefined });
        }
        const promise = new Promise((resolve, reject) => {
          this._pending.push({ resolve, reject });
        });
        this._pull();
        return promise;
      }

      _pull() {
        if (this._pulling || typeof this._source.pull !== "function") return;
        this._pulling = true;
        Promise.resolve()
          .then(() => this._source.pull(this._controller))
          .catch((error) => this._controller.error(error))
          .finally(() => {
            this._pulling = false;
          });
      }
    };
  }

  const activeStreams = new Map();

  function streamChunkBytes(value) {
    if (value == null) return new Uint8Array();
    if (value instanceof Uint8Array) return value;
    if (ArrayBuffer.isView(value)) {
      return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
    }
    if (value instanceof ArrayBuffer) return new Uint8Array(value);
    return new TextEncoder().encode(String(value));
  }

  const flightEncoder = new TextEncoder();

  function flightFrame(kind, payload) {
    return flightEncoder.encode(`${kind}${JSON.stringify(payload)}\n`);
  }

  function writeFlightFrame(stream_id, kind, payload) {
    return ops.op_beater_stream_chunk(stream_id, flightFrame(kind, payload));
  }

  async function cancelReader(reader) {
    try {
      await reader?.cancel?.();
    } catch {
      // Expected when the stream is already closing because the HTTP side went away.
    }
  }

  function releaseReader(reader) {
    try {
      reader?.releaseLock?.();
    } catch {
      // A canceled React stream may already have released or rejected pending reads.
    }
  }

  function isExpectedRenderAbort(error) {
    return fmt(error).includes("The render was aborted by the server");
  }

  async function pumpPageStream(stream_id, reader) {
    try {
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        if (!ops.op_beater_stream_chunk(stream_id, streamChunkBytes(value))) {
          await cancelReader(reader);
          return;
        }
      }
      ops.op_beater_stream_end(stream_id);
    } catch (error) {
      if (activeStreams.get(stream_id) === reader) {
        ops.op_beater_stream_error(stream_id, fmt(error));
      }
    } finally {
      if (activeStreams.get(stream_id) === reader) {
        activeStreams.delete(stream_id);
      }
      releaseReader(reader);
    }
  }

  async function pumpRscFlightStream(stream_id, reader) {
    try {
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        const bytes = Array.from(streamChunkBytes(value));
        if (bytes.length && !writeFlightFrame(stream_id, "H", bytes)) {
          await cancelReader(reader);
          return;
        }
      }
      writeFlightFrame(stream_id, "E", { ok: true });
      ops.op_beater_stream_end(stream_id);
    } catch (error) {
      if (activeStreams.get(stream_id) === reader) {
        writeFlightFrame(stream_id, "E", { ok: false, error: fmt(error) });
        ops.op_beater_stream_end(stream_id);
      }
    } finally {
      if (activeStreams.get(stream_id) === reader) {
        activeStreams.delete(stream_id);
      }
      releaseReader(reader);
    }
  }

  // Page SSR: render the default-export component as a React stream. Rust gets
  // headers when the shell is ready; the isolate keeps pumping chunks without
  // monopolizing the worker request loop.
  globalThis.__beaterPreparePageStream = async (specifier, request, stream_id) => {
    const [mod, React, ReactDOMServer] = await Promise.all([
      import(specifier),
      import("react"),
      import("react-dom/server"),
    ]);
    const Component = mod.default;
    if (typeof Component !== "function") {
      throw new Error(`page route must export a default component: ${specifier}`);
    }
    if (typeof ReactDOMServer.renderToReadableStream !== "function") {
      throw new Error("react-dom/server renderToReadableStream is unavailable");
    }
    const stream = await ReactDOMServer.renderToReadableStream(
      React.createElement(Component, { request }),
      {
        onError(error) {
          if (!isExpectedRenderAbort(error)) console.error(error);
        },
      },
    );
    const reader = stream.getReader();
    activeStreams.set(stream_id, reader);
    pumpPageStream(stream_id, reader);
    return {
      status: 200,
      headers: { "content-type": "text/html; charset=utf-8" },
    };
  };

  globalThis.__beaterPrepareRscFlightStream = async (specifier, request, stream_id) => {
    const [mod, React, ReactDOMServer] = await Promise.all([
      import(specifier),
      import("react"),
      import("react-dom/server"),
    ]);
    const Component = mod.default;
    if (typeof Component !== "function") {
      throw new Error(`RSC route must export a default component: ${specifier}`);
    }
    if (typeof ReactDOMServer.renderToReadableStream !== "function") {
      throw new Error("react-dom/server renderToReadableStream is unavailable");
    }
    writeFlightFrame(stream_id, "B", {
      protocol: "beater-flight",
      version: 0,
    });
    const stream = await ReactDOMServer.renderToReadableStream(
      React.createElement(Component, { request }),
      {
        onError(error) {
          if (!isExpectedRenderAbort(error)) console.error(error);
        },
      },
    );
    const reader = stream.getReader();
    activeStreams.set(stream_id, reader);
    pumpRscFlightStream(stream_id, reader);
    return {
      status: 200,
      headers: {
        "content-type": "text/x-component; charset=utf-8",
        "cache-control": "no-store",
      },
    };
  };

  globalThis.__beaterCancelPageStream = async (stream_id) => {
    const reader = activeStreams.get(stream_id);
    activeStreams.delete(stream_id);
    await cancelReader(reader);
    releaseReader(reader);
    ops.op_beater_stream_end(stream_id);
    return null;
  };

  // Agent Access Layer: read a route module's optional `agent` metadata
  // export ({title, description, crawl}) for llms.txt / sitemap generation.
  globalThis.__beaterRouteMeta = async (specifier) => {
    const mod = await import(specifier);
    const meta = mod.agent;
    if (!meta || typeof meta !== "object") return null;
    return {
      title: typeof meta.title === "string" ? meta.title : null,
      description: typeof meta.description === "string" ? meta.description : null,
      crawl: meta.crawl !== false,
    };
  };

  // Route dispatch: called from Rust with (specifier, method, request).
  // API routes export per-method handlers (GET, POST, ...) or a default.
  globalThis.__beaterDispatch = async (specifier, method, request) => {
    const mod = await import(specifier);
    const fallbackGet = method === "HEAD" ? mod.GET : undefined;
    const handler = mod[method] ?? fallbackGet ?? mod.default;
    if (typeof handler !== "function") {
      throw new Error(
        `route module does not export a ${method} handler or default: ${specifier}`,
      );
    }
    const resp = await handler(request);
    if (
      resp === null ||
      typeof resp !== "object" ||
      typeof resp.status !== "number"
    ) {
      throw new Error(
        `route handler must return { status, headers?, body? }, got: ${fmt(resp)}`,
      );
    }
    let bodyChunks = [];
    if (resp.body_chunks !== undefined) {
      if (
        !Array.isArray(resp.body_chunks) ||
        !resp.body_chunks.every((chunk) => typeof chunk === "string")
      ) {
        throw new Error("route handler body_chunks must be an array of strings");
      }
      bodyChunks = resp.body_chunks;
    }
    return {
      status: resp.status,
      headers: resp.headers ?? {},
      body: resp.body == null ? "" : String(resp.body),
      body_chunks: bodyChunks,
    };
  };
})(Deno.core);
