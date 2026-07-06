function createProcessShim() {
  const env = Object.freeze({
    NODE_ENV: "production",
  });
  const versions = Object.freeze({
    node: "0.0.0",
    v8: "embedded",
  });
  const release = Object.freeze({
    name: "beater",
  });
  return Object.freeze({
    env,
    versions,
    release,
    version: "v0.0.0",
    platform: "beater",
    arch: "wasm32",
    browser: false,
    cwd() {
      return "/";
    },
    nextTick(callback, ...args) {
      if (typeof callback !== "function") {
        throw new TypeError("process.nextTick callback must be a function");
      }
      const queue = globalThis.queueMicrotask ?? ((cb) => Promise.resolve().then(cb));
      queue(() => callback(...args));
    },
  });
}

const process = globalThis.process ?? createProcessShim();

if (!globalThis.process) {
  Object.defineProperty(globalThis, "process", {
    value: process,
    writable: false,
    enumerable: false,
    configurable: true,
  });
}

const env = process.env;
const versions = process.versions;
const release = process.release;
const version = process.version;
const platform = process.platform;
const arch = process.arch;
const browser = process.browser;
const cwd = process.cwd.bind(process);
const nextTick = process.nextTick.bind(process);

export {
  arch,
  browser,
  cwd,
  env,
  nextTick,
  platform,
  process,
  release,
  version,
  versions,
};
export default process;
