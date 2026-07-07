// Minimal promises timer shim layered over the deterministic timer wrapper.

import {
  clearImmediate,
  clearInterval,
  clearTimeout,
  setImmediate as callbackSetImmediate,
  setInterval as callbackSetInterval,
  setTimeout as callbackSetTimeout,
} from "node:timers";

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

export function setTimeout(delay = 1, value = undefined, options = undefined) {
  const opts = normalizeOptions(options);
  return new Promise((resolve, reject) => {
    let unwatch = () => {};
    const handle = callbackSetTimeout(() => {
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

export function setImmediate(value = undefined, options = undefined) {
  const opts = normalizeOptions(options);
  return new Promise((resolve, reject) => {
    let unwatch = () => {};
    const handle = callbackSetImmediate(() => {
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

function finishWaiters(waiters, result) {
  while (waiters.length > 0) {
    waiters.shift().resolve(result);
  }
}

function rejectWaiters(waiters, error) {
  while (waiters.length > 0) {
    waiters.shift().reject(error);
  }
}

export function setInterval(delay = 1, value = undefined, options = undefined) {
  const opts = normalizeOptions(options);
  const waiters = [];
  let pending = false;
  let done = false;
  let error = null;
  let unwatch = () => {};

  const handle = callbackSetInterval(() => {
    if (done) {
      return;
    }
    if (waiters.length > 0) {
      waiters.shift().resolve({ value, done: false });
    } else {
      pending = true;
    }
  }, normalizeDelay(delay));
  if (opts.ref === false) {
    handle.unref();
  }

  const cleanup = () => {
    done = true;
    pending = false;
    clearInterval(handle);
    unwatch();
  };
  unwatch = watchAbort(
    opts.signal,
    (abort) => {
      error = abort;
      cleanup();
      rejectWaiters(waiters, abort);
    },
    cleanup
  );

  return {
    [Symbol.asyncIterator]() {
      return this;
    },
    next() {
      if (error) {
        return Promise.reject(error);
      }
      if (done) {
        return Promise.resolve({ value: undefined, done: true });
      }
      if (pending) {
        pending = false;
        return Promise.resolve({ value, done: false });
      }
      return new Promise((resolve, reject) => {
        waiters.push({ resolve, reject });
      });
    },
    return() {
      cleanup();
      finishWaiters(waiters, { value: undefined, done: true });
      return Promise.resolve({ value: undefined, done: true });
    },
  };
}

function schedulerWait(delay = 1, options = undefined) {
  return setTimeout(delay, undefined, options);
}

export const scheduler = {
  wait: schedulerWait,
  yield() {
    return setImmediate();
  },
};

const timersPromises = {
  scheduler,
  setImmediate,
  setInterval,
  setTimeout,
};

export default timersPromises;
