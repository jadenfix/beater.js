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
    globalThis.TextEncoder = class TextEncoder {
      get encoding() {
        return "utf-8";
      }
      encode(input = "") {
        const out = [];
        for (let i = 0; i < input.length; i++) {
          let c = input.codePointAt(i);
          if (c > 0xffff) i++;
          if (c < 0x80) out.push(c);
          else if (c < 0x800) out.push(0xc0 | (c >> 6), 0x80 | (c & 63));
          else if (c < 0x10000)
            out.push(0xe0 | (c >> 12), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63));
          else
            out.push(
              0xf0 | (c >> 18),
              0x80 | ((c >> 12) & 63),
              0x80 | ((c >> 6) & 63),
              0x80 | (c & 63),
            );
        }
        return new Uint8Array(out);
      }
      encodeInto(src, dest) {
        const bytes = this.encode(src);
        const written = Math.min(bytes.length, dest.length);
        dest.set(bytes.subarray(0, written));
        return { read: src.length, written };
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

  const pageStreams = new Map();

  function streamChunkBytes(value) {
    if (value == null) return new Uint8Array();
    if (value instanceof Uint8Array) return value;
    if (ArrayBuffer.isView(value)) {
      return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
    }
    if (value instanceof ArrayBuffer) return new Uint8Array(value);
    return new TextEncoder().encode(String(value));
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
      if (pageStreams.get(stream_id) === reader) {
        ops.op_beater_stream_error(stream_id, fmt(error));
      }
    } finally {
      if (pageStreams.get(stream_id) === reader) {
        pageStreams.delete(stream_id);
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
    const renderOptions = {
      onError(error) {
        if (!isExpectedRenderAbort(error)) console.error(error);
      },
    };
    if (typeof request.scriptNonce === "string" && request.scriptNonce !== "") {
      renderOptions.nonce = request.scriptNonce;
    }
    const stream = await ReactDOMServer.renderToReadableStream(
      React.createElement(Component, { request }),
      renderOptions,
    );
    const reader = stream.getReader();
    pageStreams.set(stream_id, reader);
    pumpPageStream(stream_id, reader);
    return {
      status: 200,
      headers: { "content-type": "text/html; charset=utf-8" },
    };
  };

  globalThis.__beaterCancelPageStream = async (stream_id) => {
    const reader = pageStreams.get(stream_id);
    pageStreams.delete(stream_id);
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

  // Client bundle dispatch: a page route may export `client` as either a
  // function or a JavaScript string. This keeps the first hydration slice
  // route-scoped without introducing npm resolution or a persistent bundler.
  globalThis.__beaterClientBundle = async (specifier, routePath) => {
    const mod = await import(specifier);
    const client = mod.client;
    if (client == null) return null;
    let source;
    if (typeof client === "function") {
      source = `(${client.toString()})();`;
    } else if (typeof client === "string") {
      source = client;
    } else {
      throw new Error(`route client export must be a function or string: ${specifier}`);
    }
    return [
      `// beater.js route client bundle`,
      `// route: ${routePath}`,
      `(() => {`,
      `"use strict";`,
      source,
      `})();`,
      ``,
    ].join("\n");
  };

  // Route dispatch: called from Rust with (specifier, method, request).
  // API routes export per-method handlers (GET, POST, ...) or a default.
  globalThis.__beaterDispatch = async (specifier, method, request) => {
    const mod = await import(specifier);
    const handler = mod[method] ?? mod.default;
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
