// Minimal timer shim for server-side package compatibility.
// It wraps isolate-local timers and exposes Node-style clear/ref/unref handles
// without touching host filesystem, process state, or network resources.

import { promisify } from "node:util";

const nativeSetTimeout = globalThis.setTimeout?.bind(globalThis);
const nativeClearTimeout = globalThis.clearTimeout?.bind(globalThis);
const nativeSetInterval = globalThis.setInterval?.bind(globalThis);
const nativeClearInterval = globalThis.clearInterval?.bind(globalThis);
const promisifyCustom = promisify.custom;

function assertTimerSupport(name, value) {
  if (typeof value !== "function") {
    throw new Error(`node:timers ${name} is not available in this isolate`);
  }
}

function assertCallback(callback, name) {
  if (typeof callback !== "function") {
    throw new TypeError(`${name} callback must be a function`);
  }
}

function normalizeDelay(delay) {
  const number = Number(delay);
  if (!Number.isFinite(number) || number < 0) {
    return 0;
  }
  return Math.trunc(number);
}

function normalizeOptions(options) {
  return options && typeof options === "object" ? options : {};
}

function abortError() {
  const error = new Error("The operation was aborted");
  error.name = "AbortError";
  error.code = "ABORT_ERR";
  return error;
}

function watchAbort(signal, reject, cleanup) {
  if (!signal) {
    return () => {};
  }
  if (signal.aborted) {
    cleanup();
    reject(abortError());
    return () => {};
  }
  if (typeof signal.addEventListener !== "function") {
    return () => {};
  }
  const onAbort = () => {
    cleanup();
    reject(abortError());
  };
  signal.addEventListener("abort", onAbort, { once: true });
  return () => signal.removeEventListener?.("abort", onAbort);
}

class TimerHandle {
  constructor(nativeHandle, clearNative) {
    this._nativeHandle = nativeHandle;
    this._clearNative = clearNative;
    this._active = true;
    this._ref = true;
  }

  _clear() {
    if (this._active) {
      this._active = false;
      this._clearNative(this._nativeHandle);
    }
  }

  ref() {
    this._ref = true;
    return this;
  }

  unref() {
    this._ref = false;
    return this;
  }

  hasRef() {
    return this._ref;
  }

  close() {
    this._clear();
    return this;
  }
}

function clearHandle(handle, clearNative) {
  if (handle && typeof handle._clear === "function") {
    handle._clear();
  } else if (handle !== undefined && handle !== null) {
    clearNative(handle);
  }
}

export function setTimeout(callback, delay = 1, ...args) {
  assertTimerSupport("setTimeout", nativeSetTimeout);
  assertTimerSupport("clearTimeout", nativeClearTimeout);
  assertCallback(callback, "setTimeout");
  let handle;
  const nativeHandle = nativeSetTimeout(() => {
    if (!handle._active) {
      return;
    }
    handle._active = false;
    callback(...args);
  }, normalizeDelay(delay));
  handle = new TimerHandle(nativeHandle, nativeClearTimeout);
  return handle;
}

export function clearTimeout(handle) {
  assertTimerSupport("clearTimeout", nativeClearTimeout);
  clearHandle(handle, nativeClearTimeout);
}

export function setInterval(callback, delay = 1, ...args) {
  assertTimerSupport("setInterval", nativeSetInterval);
  assertTimerSupport("clearInterval", nativeClearInterval);
  assertCallback(callback, "setInterval");
  let handle;
  const nativeHandle = nativeSetInterval(() => {
    if (handle._active) {
      callback(...args);
    }
  }, normalizeDelay(delay));
  handle = new TimerHandle(nativeHandle, nativeClearInterval);
  return handle;
}

export function clearInterval(handle) {
  assertTimerSupport("clearInterval", nativeClearInterval);
  clearHandle(handle, nativeClearInterval);
}

export function setImmediate(callback, ...args) {
  assertTimerSupport("setTimeout", nativeSetTimeout);
  assertTimerSupport("clearTimeout", nativeClearTimeout);
  assertCallback(callback, "setImmediate");
  return setTimeout(callback, 0, ...args);
}

export function clearImmediate(handle) {
  clearTimeout(handle);
}

function promisifiedSetTimeout(delay = 1, value = undefined, options = undefined) {
  const opts = normalizeOptions(options);
  return new Promise((resolve, reject) => {
    let unwatch = () => {};
    const handle = setTimeout(() => {
      unwatch();
      resolve(value);
    }, normalizeDelay(delay));
    if (opts.ref === false) {
      handle.unref();
    }
    const cleanup = () => clearTimeout(handle);
    unwatch = watchAbort(opts.signal, reject, cleanup);
  });
}

function promisifiedSetImmediate(value = undefined, options = undefined) {
  const opts = normalizeOptions(options);
  return new Promise((resolve, reject) => {
    let unwatch = () => {};
    const handle = setImmediate(() => {
      unwatch();
      resolve(value);
    });
    if (opts.ref === false) {
      handle.unref();
    }
    const cleanup = () => clearImmediate(handle);
    unwatch = watchAbort(opts.signal, reject, cleanup);
  });
}

Object.defineProperty(setTimeout, promisifyCustom, {
  value: promisifiedSetTimeout,
  enumerable: false,
});

Object.defineProperty(setImmediate, promisifyCustom, {
  value: promisifiedSetImmediate,
  enumerable: false,
});

export function active(handle) {
  return handle?.ref?.();
}

export function unenroll(handle) {
  return clearTimeout(handle);
}

export function enroll() {
  throw new Error("timers.enroll is not supported by beater.js");
}

const timers = {
  active,
  clearImmediate,
  clearInterval,
  clearTimeout,
  enroll,
  setImmediate,
  setInterval,
  setTimeout,
  unenroll,
};

export default timers;
