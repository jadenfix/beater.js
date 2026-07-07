#!/usr/bin/env bash
# Prove the Phase C npm/node-compat wedge: a route can import a real ESM
# package, a leaf CommonJS default export, and server-side Node built-in shims
# from node_modules with bare specifiers, while unsupported CommonJS require()
# fails closed.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if [[ "$(uname -s)" == "Darwin" && -d /Library/Developer/CommandLineTools/Library/Frameworks ]]; then
  export DYLD_FRAMEWORK_PATH="${DYLD_FRAMEWORK_PATH:+$DYLD_FRAMEWORK_PATH:}/Library/Developer/CommandLineTools/Library/Frameworks"
fi

TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
BIN="${BEATER_BIN:-$TARGET_DIR/debug/beater}"
APP="$(mktemp -d "${TMPDIR:-/tmp}/beater-npm-gate.XXXXXX")/zod-app"
PORT="${BEATER_NPM_PORT:-$(python3 - <<'PY'
import socket

with socket.socket() as sock:
    sock.bind(("127.0.0.1", 0))
    print(sock.getsockname()[1])
PY
)}"
LOG="${BEATER_NPM_LOG:-$TARGET_DIR/npm-compat-gate.log}"

cleanup() {
  if [[ -n "${pid:-}" ]]; then
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  fi
  rm -rf "$(dirname "$APP")"
}
trap cleanup EXIT

if [[ "${BEATER_SKIP_BUILD:-0}" != "1" || ! -x "$BIN" ]]; then
  cargo build -p beater-cli
fi

"$BIN" new "$APP" >/dev/null
npm install --prefix "$APP" zod@4.4.3 >/dev/null

mkdir -p "$APP/node_modules/legacy-cjs"
cat >"$APP/node_modules/legacy-cjs/package.json" <<'JSON'
{"name":"legacy-cjs","main":"index.cjs"}
JSON
cat >"$APP/node_modules/legacy-cjs/index.cjs" <<'JS'
module.exports = {
  label: "legacy-cjs",
  double(value) {
    return value * 2;
  },
};
JS

mkdir -p "$APP/node_modules/require-cjs"
cat >"$APP/node_modules/require-cjs/package.json" <<'JSON'
{"name":"require-cjs","main":"index.cjs"}
JSON
cat >"$APP/node_modules/require-cjs/index.cjs" <<'JS'
module.exports = require("fs");
JS

mkdir -p "$APP/node_modules/buffered"
cat >"$APP/node_modules/buffered/package.json" <<'JSON'
{
  "name": "buffered",
  "type": "module",
  "exports": {
    ".": "./index.js"
  }
}
JSON

cat >"$APP/node_modules/buffered/index.js" <<'JS'
import { Buffer } from "node:buffer";

export function encode(value) {
  const buffer = Buffer.from(value, "utf8");
  return {
    text: buffer.toString("utf8"),
    hex: buffer.toString("hex"),
    base64: buffer.toString("base64"),
    bytes: Buffer.byteLength(value, "utf8"),
    isBuffer: Buffer.isBuffer(buffer),
  };
}
JS

mkdir -p "$APP/node_modules/processed"
cat >"$APP/node_modules/processed/package.json" <<'JSON'
{
  "name": "processed",
  "type": "module",
  "exports": {
    ".": "./index.js"
  }
}
JSON

cat >"$APP/node_modules/processed/index.js" <<'JS'
import process, { env, nextTick } from "node:process";

export async function inspect() {
  const order = [];
  nextTick((value) => order.push(value), "tick");
  order.push("sync");
  await Promise.resolve();
  return {
    nodeEnv: env.NODE_ENV,
    secretVisible: env.ANTHROPIC_API_KEY !== undefined,
    frozenEnv: Object.isFrozen(env),
    sameGlobal: globalThis.process === process,
    cwd: process.cwd(),
    version: process.versions.node,
    order,
  };
}
JS

mkdir -p "$APP/node_modules/evented"
cat >"$APP/node_modules/evented/package.json" <<'JSON'
{
  "name": "evented",
  "type": "module",
  "exports": {
    ".": "./index.js"
  }
}
JSON

cat >"$APP/node_modules/evented/index.js" <<'JS'
import EventEmitter, {
  EventEmitter as NamedEventEmitter,
  addAbortListener,
  defaultMaxListeners,
  getEventListeners,
  getMaxListeners,
  listenerCount,
  once,
  on,
  setMaxListeners,
} from "node:events";
import { EventEmitter as BareEventEmitter } from "events";

function rejects(callback) {
  try {
    callback();
    return false;
  } catch {
    return true;
  }
}

export async function inspectEvents() {
  const emitter = new EventEmitter();
  const seen = [];
  const listener = (value) => seen.push(`on:${value}`);
  emitter.on("data", listener);
  emitter.once("data", (value) => seen.push(`once:${value}`));
  const before = listenerCount(emitter, "data");
  emitter.emit("data", "a");
  emitter.emit("data", "b");
  emitter.off("data", listener);

  const readyPromise = once(emitter, "ready");
  emitter.emit("ready", "ok", 7);
  const ready = await readyPromise;

  const ordered = [];
  emitter.on("order", () => ordered.push("last"));
  emitter.prependListener("order", () => ordered.push("first"));
  emitter.emit("order");

  const raw = new EventEmitter();
  function original() {}
  raw.once("x", original);
  const rawHasWrapper = raw.rawListeners("x")[0] !== raw.listeners("x")[0];
  const listenerCopy = getEventListeners(raw, "x");
  raw.removeAllListeners("x");

  const duplicateOrder = [];
  function duplicate() {
    duplicateOrder.push("duplicate");
  }
  emitter.on("duplicate", () => duplicateOrder.push("head"));
  emitter.on("duplicate", duplicate);
  emitter.on("duplicate", () => duplicateOrder.push("middle"));
  emitter.on("duplicate", duplicate);
  emitter.removeListener("duplicate", duplicate);
  emitter.emit("duplicate");
  emitter.removeAllListeners("duplicate");

  const maxBefore = getMaxListeners(emitter);
  setMaxListeners(3, emitter);
  const maxAfter = emitter.getMaxListeners();

  let errorMessage = "";
  try {
    new EventEmitter().emit("error", new Error("boom"));
  } catch (error) {
    errorMessage = error.message;
  }

  const abortSignalLike = {
    addEventListener() {},
    removeEventListener() {},
  };

  return {
    addAbortRejected: rejects(() => addAbortListener(abortSignalLike, () => {})),
    asyncIteratorRejected: rejects(() => on(emitter, "data")),
    bareIsNamed: BareEventEmitter === NamedEventEmitter,
    before,
    defaultHasStatics:
      EventEmitter.EventEmitter === NamedEventEmitter &&
      EventEmitter.once === once &&
      EventEmitter.listenerCount === listenerCount,
    defaultIsNamed: EventEmitter === NamedEventEmitter,
    defaultMaxListeners,
    duplicateOrder,
    errorMessage,
    eventNames: emitter.eventNames(),
    listenerCopyLength: listenerCopy.length,
    maxAfter,
    maxBefore,
    missingEmit: emitter.emit("missing"),
    ordered,
    rawAfterRemove: raw.listenerCount("x"),
    rawHasWrapper,
    ready,
    remainingData: emitter.listenerCount("data"),
    seen,
  };
}
JS

mkdir -p "$APP/node_modules/utiled"
cat >"$APP/node_modules/utiled/package.json" <<'JSON'
{
  "name": "utiled",
  "type": "module",
  "exports": {
    ".": "./index.js"
  }
}
JSON

cat >"$APP/node_modules/utiled/index.js" <<'JS'
import util, {
  TextDecoder,
  TextEncoder,
  callbackify,
  debuglog,
  deprecate,
  format,
  inherits,
  inspect,
  isDeepStrictEqual,
  promisify,
  types,
} from "node:util";
import bareUtil from "util";
import utilTypesDefault, { isDataView, isTypedArray } from "node:util/types";

function Parent(name) {
  this.name = name;
}
Parent.prototype.describe = function describe() {
  return `parent:${this.name}`;
};

function Child(name) {
  Parent.call(this, name);
}
inherits(Child, Parent);

function callbackAdd(left, right, callback) {
  callback(null, left + right);
}

const receiver = {
  offset: 4,
  add(value, callback) {
    callback(null, this.offset + value);
  },
};

function customOriginal() {}
customOriginal[promisify.custom] = () => Promise.resolve("custom-result");

export async function inspectUtil() {
  const child = new Child("beater");
  const encoded = new TextEncoder().encode("beater");
  const decoded = new TextDecoder().decode(encoded);
  const callbackified = callbackify(async (value) => value.toUpperCase());
  const callbackOrder = [];
  const callbackValue = await new Promise((resolve, reject) => {
    callbackified("ok", (error, value) => {
      callbackOrder.push("callback");
      if (error) reject(error);
      else resolve(value);
    });
    callbackOrder.push("after-call");
  });
  const callbackFalsyReason = await new Promise((resolve) => {
    callbackify(async () => Promise.reject(null))((error) => {
      resolve(
        error.message === "Promise was rejected with a falsy value" &&
          Object.prototype.hasOwnProperty.call(error, "reason") &&
          error.reason === null
      );
    });
  });
  const circular = {};
  circular.self = circular;
  const getterObject = {};
  Object.defineProperty(getterObject, "value", {
    enumerable: true,
    get() {
      throw new Error("getter should not run");
    },
  });
  const getterArray = [];
  Object.defineProperty(getterArray, "0", {
    enumerable: true,
    get() {
      throw new Error("array getter should not run");
    },
  });
  const wideObject = {};
  const wideMap = new Map();
  const wideSet = new Set();
  for (let index = 0; index < 35; index += 1) {
    wideObject[`k${index}`] = index;
    wideMap.set(`k${index}`, index);
    wideSet.add(index);
  }
  const deprecated = deprecate((value) => `dep:${value}`)("x");
  const debug = debuglog("beater");
  const viewBytes = new Uint8Array([88, 98, 101, 97, 116, 101, 114, 89]);
  const view = new DataView(viewBytes.buffer, 1, 6);

  return {
    bareDefaultMatches: bareUtil.format === format,
    callbackFalsyReason,
    callbackified: callbackValue,
    customPromisify: await promisify(customOriginal)(),
    decoded,
    decodedMalformed: new TextDecoder().decode(new Uint8Array([0xff])),
    decodedOutOfRangeRejected: new TextDecoder()
      .decode(new Uint8Array([0xf4, 0x90, 0x80, 0x80]))
      .includes("\ufffd"),
    decodedView: new TextDecoder().decode(view),
    deepEqual: isDeepStrictEqual({ a: [1, "x"] }, { a: [1, "x"] }),
    deepArrayBufferFalse: !isDeepStrictEqual(new Uint8Array([1]).buffer, new Uint8Array([2]).buffer),
    deepDateFalse: !isDeepStrictEqual(new Date(0), new Date(1)),
    deepMapFalse: !isDeepStrictEqual(new Map([["a", 1]]), new Map([["a", 2]])),
    deepRegExpFalse: !isDeepStrictEqual(/a/g, /a/i),
    deprecated,
    debugEnabled: debug.enabled,
    encoded: [...encoded],
    encoderIsGlobal: globalThis.TextEncoder === undefined || TextEncoder === globalThis.TextEncoder,
    decoderIsGlobal: globalThis.TextDecoder === undefined || TextDecoder === globalThis.TextDecoder,
    format: format("id:%s count:%d data:%j %%", "beater", 7, { ok: true }),
    inherits: child instanceof Parent && child.describe(),
    inspect: inspect({ alpha: 1, beta: ["x"] }),
    inspectCircular: inspect(circular),
    inspectGetterArray: inspect(getterArray),
    inspectGetter: inspect(getterObject),
    inspectWideMapBounded: inspect(wideMap).includes("... 3 more items"),
    inspectWideBounded: inspect(wideObject).includes("... more items"),
    inspectWideSetBounded: inspect(wideSet).includes("... 3 more items"),
    promisified: await promisify(callbackAdd)(2, 5),
    promisifiedReceiver: await promisify(receiver.add).call(receiver, 3),
    sameDefault: util.promisify === promisify && util.types === types,
    callbackOrder,
    types: {
      arrayBuffer: types.isArrayBuffer(new ArrayBuffer(2)),
      dataView: types.isDataView(new DataView(new ArrayBuffer(2))),
      date: types.isDate(new Date(0)),
      map: types.isMap(new Map()),
      nativeError: types.isNativeError(new Error("x")),
      promise: types.isPromise(Promise.resolve(1)),
      set: types.isSet(new Set()),
      thenable: types.isPromise({ then() {} }),
      typedArray: types.isTypedArray(new Uint8Array([1])),
      uint8Array: types.isUint8Array(new Uint8Array([1])),
    },
    utilTypes: {
      defaultMatches: utilTypesDefault === types,
      dataView: isDataView(new DataView(new ArrayBuffer(2))),
      typedArray: isTypedArray(new Uint8Array([1])),
    },
  };
}
JS

mkdir -p "$APP/node_modules/asserted"
cat >"$APP/node_modules/asserted/package.json" <<'JSON'
{
  "name": "asserted",
  "type": "module",
  "exports": {
    ".": "./index.js"
  }
}
JSON

cat >"$APP/node_modules/asserted/index.js" <<'JS'
import assert, {
  deepEqual,
  AssertionError,
  deepStrictEqual,
  doesNotMatch,
  doesNotReject,
  doesNotThrow,
  equal,
  fail,
  ifError,
  match,
  notDeepStrictEqual,
  notDeepEqual,
  notEqual,
  notStrictEqual,
  ok,
  rejects,
  strict,
  strictEqual,
  throws,
} from "node:assert";
import strictAssert from "node:assert/strict";
import bareAssert from "assert";
import bareStrict from "assert/strict";

function capture(callback) {
  try {
    callback();
    return null;
  } catch (error) {
    return {
      actual: error.actual === undefined ? null : error.actual,
      code: error.code,
      expected: error.expected === undefined ? null : error.expected,
      generatedMessage: error.generatedMessage,
      message: error.message,
      name: error.name,
      operator: error.operator,
    };
  }
}

async function captureAsync(callback) {
  try {
    await callback();
    return null;
  } catch (error) {
    return {
      actualName: error.actual?.name ?? null,
      code: error.code,
      expectedName: error.expected?.name ?? null,
      generatedMessage: error.generatedMessage,
      name: error.name,
      operator: error.operator,
    };
  }
}

export async function inspectAssert() {
  assert(true);
  ok(1);
  equal("7", 7);
  notEqual(7, 8);
  strictEqual(7, 7);
  notStrictEqual(7, "7");
  deepStrictEqual({ a: [1] }, { a: [1] });
  notDeepStrictEqual({ a: 1 }, { a: 2 });
  deepEqual({ a: [1] }, { a: ["1"] });
  assert.deepEqual(1, "1");
  notDeepEqual({ a: 1 }, { a: 2 });
  throws(() => {
    throw new TypeError("bad");
  }, TypeError);
  doesNotThrow(() => ok(true));
  await rejects(Promise.reject(new Error("no")), /no/);
  await rejects(async () => {
    throw new TypeError("async bad");
  }, TypeError);
  await doesNotReject(Promise.resolve("ok"));
  match("beater", /beat/);
  doesNotMatch("beater", /node/);
  ifError(null);

  const custom = new AssertionError({
    actual: 1,
    expected: 2,
    message: "m",
    operator: "===",
  });
  const strictFailure = capture(() => strictEqual(1, 2));
  const failFailure = capture(() => fail("manual"));
  const ifErrorFailure = capture(() => ifError(new Error("err")));
  const matchTypeFailure = capture(() => match(123, /123/));
  const doesNotMatchTypeFailure = capture(() => doesNotMatch(123, /123/));
  const throwsMismatch = capture(() =>
    throws(() => {
      throw new TypeError("bad");
    }, RangeError)
  );
  const rejectsMismatch = await captureAsync(() => rejects(Promise.reject(new TypeError("bad")), RangeError));

  return {
    assertionError: {
      actual: custom.actual,
      code: custom.code,
      expected: custom.expected,
      generatedMessage: custom.generatedMessage,
      message: custom.message,
      name: custom.name,
      operator: custom.operator,
    },
    assertStatics:
      assert.AssertionError === AssertionError &&
      assert.deepStrictEqual === deepStrictEqual &&
      assert.strict === strict,
    bareDefaultMatches: bareAssert === assert,
    bareStrictDefaultMatches: bareStrict === strictAssert,
    defaultHasOk: assert.ok === ok,
    doesNotMatchTypeFailure: {
      message: doesNotMatchTypeFailure.message,
      name: doesNotMatchTypeFailure.name,
    },
    failFailure,
    ifErrorFailure: {
      actualName: ifErrorFailure.actual.name,
      code: ifErrorFailure.code,
      message: ifErrorFailure.message,
      name: ifErrorFailure.name,
      operator: ifErrorFailure.operator,
    },
    legacyDeepEqual: true,
    matchTypeFailure: {
      message: matchTypeFailure.message,
      name: matchTypeFailure.name,
    },
    rejectsMismatch,
    strictAliases:
      strict.equal === strict.strictEqual &&
      strict.equal === strictEqual &&
      strict.notEqual === notStrictEqual &&
      strict.deepEqual === deepStrictEqual &&
      strict.notDeepEqual === notDeepStrictEqual &&
      strictAssert.equal === strictEqual,
    strictFailure,
    throwsMismatch: {
      actualName: throwsMismatch.actual.name,
      code: throwsMismatch.code,
      expectedName: throwsMismatch.expected.name,
      generatedMessage: throwsMismatch.generatedMessage,
      name: throwsMismatch.name,
      operator: throwsMismatch.operator,
    },
  };
}
JS

mkdir -p "$APP/node_modules/queried"
cat >"$APP/node_modules/queried/package.json" <<'JSON'
{
  "name": "queried",
  "type": "module",
  "exports": {
    ".": "./index.js"
  }
}
JSON

cat >"$APP/node_modules/queried/index.js" <<'JS'
import querystring, {
  decode,
  encode,
  escape,
  parse,
  stringify,
  unescape,
} from "node:querystring";
import bareQuerystring from "querystring";

export function inspectQuerystring() {
  const parsed = parse("a=1&a=2&space=hello+world&encoded=%23tag&empty");
  const custom = parse("name:beater;kind:agent", ";", ":");
  const limited = parse("x=1&y=2&z=3", "&", "=", { maxKeys: 2 });
  const emptySegments = parse("&a=1&&b=2&");
  const emptySegmentsLimited = parse("&&x=1&&y=2", "&", "=", { maxKeys: 1 });
  const separatorHeavy = parse(`${"&".repeat(5000)}last=1&ignored=2`, "&", "=", { maxKeys: 1 });
  const unlimited = parse("k=1&k=2&k=3", "&", "=", { maxKeys: 0 });
  const passthroughMalformed = unescape("%E0%A4%A");
  const mixedMalformed = unescape("ok%21%E0%A4%A%23tail");
  const originalEscape = querystring.escape;
  const originalUnescape = querystring.unescape;
  querystring.escape = (value) => `enc<${String(value)}>`;
  querystring.unescape = (value) => `dec<${String(value)}>`;
  const overridden = {
    parsed: parse("name=beater"),
    stringified: stringify({ name: "beater" }),
  };
  querystring.escape = originalEscape;
  querystring.unescape = originalUnescape;
  return {
    bareDefaultMatches: bareQuerystring.parse === parse,
    custom,
    defaultStatics:
      querystring.stringify === stringify &&
      querystring.parse === parse &&
      querystring.encode === encode &&
      querystring.decode === decode,
    escaped: escape("a b+c&d"),
    emptySegments,
    emptySegmentsLimited,
    limited,
    mixedMalformed,
    overridden,
    passthroughMalformed,
    parsed,
    separatorHeavy,
    stringified: stringify({
      a: ["1", "2"],
      bool: true,
      count: 7,
      empty: "",
      none: null,
      space: "hello world",
    }),
    unescaped: unescape("hello%20world%21"),
    unlimited,
  };
}
JS

mkdir -p "$APP/node_modules/pathed"
cat >"$APP/node_modules/pathed/package.json" <<'JSON'
{
  "name": "pathed",
  "type": "module",
  "exports": {
    ".": "./index.js"
  }
}
JSON

cat >"$APP/node_modules/pathed/index.js" <<'JS'
import path, {
  basename,
  dirname,
  extname,
  isAbsolute,
  join,
  normalize,
  relative,
  resolve,
  sep,
} from "node:path";
import { sep as bareSep } from "path";

export function inspectPath() {
  return {
    basename: basename("/tmp/beater/app.ts"),
    dirname: dirname("/tmp/beater/app.ts"),
    extname: extname("index.client.tsx"),
    joined: join("/tmp", "beater", "..", "app", "route.ts"),
    normalized: normalize("/tmp//beater/./routes/../index.ts"),
    relative: relative("/tmp/beater/app", "/tmp/beater/app/routes/index.ts"),
    resolved: resolve("app", "routes", "../index.ts"),
    parsedFile: path.parse("index.ts"),
    parsedParent: path.parse("../index.ts"),
    formatted: path.format({ dir: "agents", name: "support", ext: ".ts" }),
    dotDotExt: extname(".."),
    absolute: isAbsolute("/tmp"),
    relativeIsAbsolute: isAbsolute("tmp"),
    defaultJoin: path.join("agents", "support"),
    posixSep: path.posix.sep,
    bareSep,
    sep,
  };
}
JS

mkdir -p "$APP/node_modules/osed"
cat >"$APP/node_modules/osed/package.json" <<'JSON'
{
  "name": "osed",
  "type": "module",
  "exports": {
    ".": "./index.js"
  }
}
JSON

cat >"$APP/node_modules/osed/index.js" <<'JS'
import os, {
  arch,
  availableParallelism,
  cpus,
  devNull,
  EOL,
  freemem,
  homedir,
  hostname,
  loadavg,
  networkInterfaces,
  platform,
  tmpdir,
  totalmem,
  userInfo,
} from "node:os";
import { platform as barePlatform } from "os";

function rejects(callback) {
  try {
    callback();
    return false;
  } catch {
    return true;
  }
}

export function inspectOs() {
  return {
    arch: arch(),
    availableParallelism: availableParallelism(),
    barePlatform: barePlatform(),
    cpus: cpus(),
    defaultPlatform: os.platform(),
    devNull,
    eol: EOL,
    freemem: freemem(),
    homedir: homedir(),
    hostname: hostname(),
    loadavg: loadavg(),
    networkInterfaces: networkInterfaces(),
    platform: platform(),
    priority: os.getPriority(),
    setPriorityRejected: rejects(() => os.setPriority(10)),
    tmpdir: tmpdir(),
    totalmem: totalmem(),
    type: os.type(),
    uptime: os.uptime(),
    userInfo: userInfo(),
    version: os.version(),
  };
}
JS

mkdir -p "$APP/node_modules/urled"
cat >"$APP/node_modules/urled/package.json" <<'JSON'
{
  "name": "urled",
  "type": "module",
  "exports": {
    ".": "./index.js"
  }
}
JSON

cat >"$APP/node_modules/urled/index.js" <<'JS'
import url, {
  fileURLToPath,
  pathToFileURL,
  URL,
  URLSearchParams,
} from "node:url";
import { fileURLToPath as bareFileURLToPath } from "url";

function rejects(callback) {
  try {
    callback();
    return false;
  } catch {
    return true;
  }
}

export function inspectUrl() {
  const parsed = new URL("../api/health?q=beater#frag", "https://example.test/app/routes/");
  const params = new URLSearchParams([
    ["q", "beater js"],
    ["q", "agent"],
  ]);
  const nullParams = new URLSearchParams(null);
  const mutable = new URL("https://idp.example/cb");
  mutable.searchParams.append("state", "a=b=");
  return {
    filePath: fileURLToPath("file:///tmp/beater/routes/index.ts"),
    localhostPath: fileURLToPath(new URL("file://localhost/tmp/beater/routes/index.ts")),
    bareFilePath: bareFileURLToPath("file:///tmp/beater/bare.ts"),
    fileHref: pathToFileURL("/tmp/beater/space name.ts").href,
    encodedHref: pathToFileURL("/tmp/beater/hash#query?.ts").href,
    defaultHref: url.pathToFileURL("/tmp/beater/default.ts").href,
    parsedHref: parsed.href,
    paramsAll: params.getAll("q"),
    stringParam: new URLSearchParams("sig=a=b=&empty").get("sig"),
    nullParams: nullParams.toString(),
    mutableHref: mutable.href,
    queryOnlyHref: new URL("?page=2", "https://example.test/app/items?old=1#top").href,
    hashOnlyHref: new URL("#next", "https://example.test/app/items?old=1#top").href,
    importedGlobal: URL === globalThis.URL,
    paramsGlobal: URLSearchParams === globalThis.URLSearchParams,
    importMetaPath: fileURLToPath(import.meta.url).endsWith("/node_modules/urled/index.js"),
    badHostRejected: rejects(() => fileURLToPath("file://evil.test/tmp/beater.ts")),
    encodedSlashRejected: rejects(() => fileURLToPath("file:///tmp/a%2Fb.ts")),
    relativePathRejected: rejects(() => pathToFileURL("tmp/beater.ts")),
  };
}
JS

cat >"$APP/app/routes/api/zod.ts" <<'TS'
import { z } from "zod";

const Payload = z.object({ value: z.string().min(3) });

export function GET() {
  const parsed = Payload.parse({ value: "beater" });
  return {
    status: 200,
    headers: { "content-type": "application/json; charset=utf-8" },
    body: JSON.stringify({ ok: true, value: parsed.value }),
  };
}
TS

cat >"$APP/app/routes/api/cjs.ts" <<'TS'
import legacy from "legacy-cjs";

export function GET() {
  return {
    status: 200,
    headers: { "content-type": "application/json; charset=utf-8" },
    body: JSON.stringify({
      label: legacy.label,
      doubled: legacy.double(21),
    }),
  };
}
TS

cat >"$APP/app/routes/api/cjs-require.ts" <<'TS'
import blocked from "require-cjs";

export function GET() {
  return {
    status: 200,
    headers: { "content-type": "text/plain; charset=utf-8" },
    body: String(blocked),
  };
}
TS

cat >"$APP/app/routes/api/evented.ts" <<'TS'
import { inspectEvents } from "evented";

export async function GET() {
  return {
    status: 200,
    headers: { "content-type": "application/json; charset=utf-8" },
    body: JSON.stringify(await inspectEvents()),
  };
}
TS

cat >"$APP/app/routes/api/utiled.ts" <<'TS'
import { inspectUtil } from "utiled";

export async function GET() {
  return {
    status: 200,
    headers: { "content-type": "application/json; charset=utf-8" },
    body: JSON.stringify(await inspectUtil()),
  };
}
TS

cat >"$APP/app/routes/api/asserted.ts" <<'TS'
import { inspectAssert } from "asserted";

export async function GET() {
  return {
    status: 200,
    headers: { "content-type": "application/json; charset=utf-8" },
    body: JSON.stringify(await inspectAssert()),
  };
}
TS

cat >"$APP/app/routes/api/queried.ts" <<'TS'
import { inspectQuerystring } from "queried";

export function GET() {
  return {
    status: 200,
    headers: { "content-type": "application/json; charset=utf-8" },
    body: JSON.stringify(inspectQuerystring()),
  };
}
TS

cat >"$APP/app/routes/api/pathed.ts" <<'TS'
import { inspectPath } from "pathed";

export function GET() {
  return {
    status: 200,
    headers: { "content-type": "application/json; charset=utf-8" },
    body: JSON.stringify(inspectPath()),
  };
}
TS

cat >"$APP/app/routes/api/osed.ts" <<'TS'
import { inspectOs } from "osed";

export function GET() {
  return {
    status: 200,
    headers: { "content-type": "application/json; charset=utf-8" },
    body: JSON.stringify(inspectOs()),
  };
}
TS

cat >"$APP/app/routes/api/urled.ts" <<'TS'
import { inspectUrl } from "urled";

export function GET() {
  return {
    status: 200,
    headers: { "content-type": "application/json; charset=utf-8" },
    body: JSON.stringify(inspectUrl()),
  };
}
TS

cat >"$APP/app/routes/api/buffered.ts" <<'TS'
import { encode } from "buffered";

export function GET() {
  return {
    status: 200,
    headers: { "content-type": "application/json; charset=utf-8" },
    body: JSON.stringify(encode("beater")),
  };
}
TS

cat >"$APP/app/routes/api/processed.ts" <<'TS'
import { inspect } from "processed";

export async function GET() {
  return {
    status: 200,
    headers: { "content-type": "application/json; charset=utf-8" },
    body: JSON.stringify(await inspect()),
  };
}
TS

mkdir -p "$(dirname "$LOG")"
(
  unset ANTHROPIC_API_KEY
  unset BEATER_BASE_URL
  unset BEATER_MCP_TOKEN
  unset BEATER_MCP_TRUSTED_ORIGINS
  export HOME=/host/home/leak
  export HOSTNAME=host-name-leak
  export USER=host-user-leak
  "$BIN" dev "$APP" --host 127.0.0.1 --port "$PORT"
) >"$LOG" 2>&1 &
pid=$!

python3 - "$PORT" "$LOG" <<'PY'
import http.client
import json
import sys
import time

port = int(sys.argv[1])
log = sys.argv[2]
deadline = time.monotonic() + 20

while True:
    try:
        conn = http.client.HTTPConnection("127.0.0.1", port, timeout=0.5)
        conn.request("GET", "/api/health")
        response = conn.getresponse()
        response.read()
        conn.close()
        if response.status == 200:
            break
    except OSError:
        pass
    if time.monotonic() > deadline:
        print(f"server did not become ready on {port}; log follows", file=sys.stderr)
        try:
            print(open(log, encoding="utf-8").read(), file=sys.stderr)
        except OSError:
            pass
        sys.exit(1)
    time.sleep(0.1)

conn = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
conn.request("GET", "/api/zod")
response = conn.getresponse()
body = response.read().decode("utf-8")
conn.close()
if response.status != 200:
    sys.exit(f"expected 200 from /api/zod, got {response.status}: {body}")
zod_payload = json.loads(body)
if zod_payload != {"ok": True, "value": "beater"}:
    sys.exit(f"unexpected /api/zod payload: {zod_payload!r}")

conn = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
conn.request("GET", "/api/cjs")
response = conn.getresponse()
body = response.read().decode("utf-8")
conn.close()
if response.status != 200:
    sys.exit(f"expected 200 from /api/cjs, got {response.status}: {body}")
cjs_payload = json.loads(body)
if cjs_payload != {"label": "legacy-cjs", "doubled": 42}:
    sys.exit(f"unexpected /api/cjs payload: {cjs_payload!r}")

conn = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
conn.request("GET", "/api/evented")
response = conn.getresponse()
body = response.read().decode("utf-8")
conn.close()
if response.status != 200:
    sys.exit(f"expected 200 from /api/evented, got {response.status}: {body}")
events_payload = json.loads(body)
expected_events = {
    "addAbortRejected": True,
    "asyncIteratorRejected": True,
    "bareIsNamed": True,
    "before": 2,
    "defaultHasStatics": True,
    "defaultIsNamed": True,
    "defaultMaxListeners": 10,
    "duplicateOrder": ["head", "duplicate", "middle"],
    "errorMessage": "boom",
    "eventNames": ["order"],
    "listenerCopyLength": 1,
    "maxAfter": 3,
    "maxBefore": 10,
    "missingEmit": False,
    "ordered": ["first", "last"],
    "rawAfterRemove": 0,
    "rawHasWrapper": True,
    "ready": ["ok", 7],
    "remainingData": 0,
    "seen": ["on:a", "once:a", "on:b"],
}
if events_payload != expected_events:
    sys.exit(f"unexpected /api/evented payload: {events_payload!r}")

conn = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
conn.request("GET", "/api/utiled")
response = conn.getresponse()
body = response.read().decode("utf-8")
conn.close()
if response.status != 200:
    sys.exit(f"expected 200 from /api/utiled, got {response.status}: {body}")
util_payload = json.loads(body)
expected_util = {
    "bareDefaultMatches": True,
    "callbackFalsyReason": True,
    "callbackOrder": ["after-call", "callback"],
    "callbackified": "OK",
    "customPromisify": "custom-result",
    "decoderIsGlobal": True,
    "decoded": "beater",
    "decodedMalformed": "\ufffd",
    "decodedOutOfRangeRejected": True,
    "decodedView": "beater",
    "deepArrayBufferFalse": True,
    "deepEqual": True,
    "deepDateFalse": True,
    "deepMapFalse": True,
    "deepRegExpFalse": True,
    "deprecated": "dep:x",
    "debugEnabled": False,
    "encoded": [98, 101, 97, 116, 101, 114],
    "encoderIsGlobal": True,
    "format": "id:beater count:7 data:{\"ok\":true} %",
    "inherits": "parent:beater",
    "inspect": "{ alpha: 1, beta: [ 'x' ] }",
    "inspectCircular": "{ self: [Circular] }",
    "inspectGetterArray": "[ [Getter] ]",
    "inspectGetter": "{ value: [Getter] }",
    "inspectWideMapBounded": True,
    "inspectWideBounded": True,
    "inspectWideSetBounded": True,
    "promisified": 7,
    "promisifiedReceiver": 7,
    "sameDefault": True,
    "types": {
        "arrayBuffer": True,
        "dataView": True,
        "date": True,
        "map": True,
        "nativeError": True,
        "promise": True,
        "set": True,
        "thenable": False,
        "typedArray": True,
        "uint8Array": True,
    },
    "utilTypes": {
        "dataView": True,
        "defaultMatches": True,
        "typedArray": True,
    },
}
if util_payload != expected_util:
    sys.exit(f"unexpected /api/utiled payload: {util_payload!r}")

conn = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
conn.request("GET", "/api/asserted")
response = conn.getresponse()
body = response.read().decode("utf-8")
conn.close()
if response.status != 200:
    sys.exit(f"expected 200 from /api/asserted, got {response.status}: {body}")
assert_payload = json.loads(body)
expected_assert = {
    "assertionError": {
        "actual": 1,
        "code": "ERR_ASSERTION",
        "expected": 2,
        "generatedMessage": False,
        "message": "m",
        "name": "AssertionError",
        "operator": "===",
    },
    "assertStatics": True,
    "bareDefaultMatches": True,
    "bareStrictDefaultMatches": True,
    "defaultHasOk": True,
    "doesNotMatchTypeFailure": {
        "message": "assert.doesNotMatch requires a string input",
        "name": "TypeError",
    },
    "failFailure": {
        "actual": None,
        "code": "ERR_ASSERTION",
        "expected": None,
        "generatedMessage": False,
        "message": "manual",
        "name": "AssertionError",
        "operator": "fail",
    },
    "ifErrorFailure": {
        "actualName": "Error",
        "code": "ERR_ASSERTION",
        "message": "ifError got unwanted exception: err",
        "name": "AssertionError",
        "operator": "ifError",
    },
    "legacyDeepEqual": True,
    "matchTypeFailure": {
        "message": "assert.match requires a string input",
        "name": "TypeError",
    },
    "rejectsMismatch": {
        "actualName": "TypeError",
        "code": "ERR_ASSERTION",
        "expectedName": "RangeError",
        "generatedMessage": True,
        "name": "AssertionError",
        "operator": "rejects",
    },
    "strictAliases": True,
    "strictFailure": {
        "actual": 1,
        "code": "ERR_ASSERTION",
        "expected": 2,
        "generatedMessage": True,
        "message": "1 strictEqual 2",
        "name": "AssertionError",
        "operator": "strictEqual",
    },
    "throwsMismatch": {
        "actualName": "TypeError",
        "code": "ERR_ASSERTION",
        "expectedName": "RangeError",
        "generatedMessage": True,
        "name": "AssertionError",
        "operator": "throws",
    },
}
if assert_payload != expected_assert:
    sys.exit(f"unexpected /api/asserted payload: {assert_payload!r}")

conn = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
conn.request("GET", "/api/queried")
response = conn.getresponse()
body = response.read().decode("utf-8")
conn.close()
if response.status != 200:
    sys.exit(f"expected 200 from /api/queried, got {response.status}: {body}")
query_payload = json.loads(body)
expected_query = {
    "bareDefaultMatches": True,
    "custom": {
        "kind": "agent",
        "name": "beater",
    },
    "defaultStatics": True,
    "escaped": "a%20b%2Bc%26d",
    "emptySegments": {
        "a": "1",
        "b": "2",
    },
    "emptySegmentsLimited": {},
    "limited": {
        "x": "1",
        "y": "2",
    },
    "mixedMalformed": "ok!\ufffd%A#tail",
    "overridden": {
        "parsed": {
            "dec<name>": "dec<beater>",
        },
        "stringified": "enc<name>=enc<beater>",
    },
    "passthroughMalformed": "\ufffd%A",
    "parsed": {
        "a": ["1", "2"],
        "encoded": "#tag",
        "empty": "",
        "space": "hello world",
    },
    "separatorHeavy": {},
    "stringified": "a=1&a=2&bool=true&count=7&empty=&none=&space=hello%20world",
    "unescaped": "hello world!",
    "unlimited": {
        "k": ["1", "2", "3"],
    },
}
if query_payload != expected_query:
    sys.exit(f"unexpected /api/queried payload: {query_payload!r}")

conn = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
conn.request("GET", "/api/pathed")
response = conn.getresponse()
body = response.read().decode("utf-8")
conn.close()
if response.status != 200:
    sys.exit(f"expected 200 from /api/pathed, got {response.status}: {body}")
path_payload = json.loads(body)
expected_path = {
    "basename": "app.ts",
    "dirname": "/tmp/beater",
    "extname": ".tsx",
    "joined": "/tmp/app/route.ts",
    "normalized": "/tmp/beater/index.ts",
    "relative": "routes/index.ts",
    "resolved": "/app/index.ts",
    "parsedFile": {
        "root": "",
        "dir": "",
        "base": "index.ts",
        "ext": ".ts",
        "name": "index",
    },
    "parsedParent": {
        "root": "",
        "dir": "..",
        "base": "index.ts",
        "ext": ".ts",
        "name": "index",
    },
    "formatted": "agents/support.ts",
    "dotDotExt": "",
    "absolute": True,
    "relativeIsAbsolute": False,
    "defaultJoin": "agents/support",
    "posixSep": "/",
    "bareSep": "/",
    "sep": "/",
}
if path_payload != expected_path:
    sys.exit(f"unexpected /api/pathed payload: {path_payload!r}")

conn = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
conn.request("GET", "/api/osed")
response = conn.getresponse()
body = response.read().decode("utf-8")
conn.close()
if response.status != 200:
    sys.exit(f"expected 200 from /api/osed, got {response.status}: {body}")
os_payload = json.loads(body)
expected_os = {
    "arch": "wasm32",
    "availableParallelism": 1,
    "barePlatform": "beater",
    "cpus": [],
    "defaultPlatform": "beater",
    "devNull": "/dev/null",
    "eol": "\n",
    "freemem": 0,
    "homedir": "/",
    "hostname": "localhost",
    "loadavg": [0, 0, 0],
    "networkInterfaces": {},
    "platform": "beater",
    "priority": 0,
    "setPriorityRejected": True,
    "tmpdir": "/tmp",
    "totalmem": 0,
    "type": "Beater",
    "uptime": 0,
    "userInfo": {
        "uid": -1,
        "gid": -1,
        "username": "beater",
        "homedir": "/",
        "shell": None,
    },
    "version": "0.0.0",
}
if os_payload != expected_os:
    sys.exit(f"unexpected /api/osed payload: {os_payload!r}")

conn = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
conn.request("GET", "/api/urled")
response = conn.getresponse()
body = response.read().decode("utf-8")
conn.close()
if response.status != 200:
    sys.exit(f"expected 200 from /api/urled, got {response.status}: {body}")
url_payload = json.loads(body)
expected_url = {
    "filePath": "/tmp/beater/routes/index.ts",
    "localhostPath": "/tmp/beater/routes/index.ts",
    "bareFilePath": "/tmp/beater/bare.ts",
    "fileHref": "file:///tmp/beater/space%20name.ts",
    "encodedHref": "file:///tmp/beater/hash%23query%3F.ts",
    "defaultHref": "file:///tmp/beater/default.ts",
    "parsedHref": "https://example.test/app/api/health?q=beater#frag",
    "paramsAll": ["beater js", "agent"],
    "stringParam": "a=b=",
    "nullParams": "",
    "mutableHref": "https://idp.example/cb?state=a%3Db%3D",
    "queryOnlyHref": "https://example.test/app/items?page=2",
    "hashOnlyHref": "https://example.test/app/items?old=1#next",
    "importedGlobal": True,
    "paramsGlobal": True,
    "importMetaPath": True,
    "badHostRejected": True,
    "encodedSlashRejected": True,
    "relativePathRejected": True,
}
if url_payload != expected_url:
    sys.exit(f"unexpected /api/urled payload: {url_payload!r}")

conn = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
conn.request("GET", "/api/buffered")
response = conn.getresponse()
body = response.read().decode("utf-8")
conn.close()
if response.status != 200:
    sys.exit(f"expected 200 from /api/buffered, got {response.status}: {body}")
buffer_payload = json.loads(body)
expected_buffer = {
    "text": "beater",
    "hex": "626561746572",
    "base64": "YmVhdGVy",
    "bytes": 6,
    "isBuffer": True,
}
if buffer_payload != expected_buffer:
    sys.exit(f"unexpected /api/buffered payload: {buffer_payload!r}")

conn = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
conn.request("GET", "/api/processed")
response = conn.getresponse()
body = response.read().decode("utf-8")
conn.close()
if response.status != 200:
    sys.exit(f"expected 200 from /api/processed, got {response.status}: {body}")
process_payload = json.loads(body)
expected_process = {
    "nodeEnv": "production",
    "secretVisible": False,
    "frozenEnv": True,
    "sameGlobal": True,
    "cwd": "/",
    "version": "0.0.0",
    "order": ["sync", "tick"],
}
if process_payload != expected_process:
    sys.exit(f"unexpected /api/processed payload: {process_payload!r}")

conn = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
conn.request("GET", "/api/cjs-require")
response = conn.getresponse()
body = response.read().decode("utf-8")
conn.close()
if response.status < 500:
    sys.exit(f"expected /api/cjs-require to fail closed, got {response.status}: {body}")
if "CommonJS require" not in body:
    sys.exit(f"expected /api/cjs-require failure to mention CommonJS require, got: {body}")
print(
    "npm compat passed: "
    f"zod import returned {zod_payload['value']}; "
    f"cjs doubled {cjs_payload['doubled']}; "
    f"events seen {','.join(events_payload['seen'])}; "
    f"util format {util_payload['format']}; "
    f"assert strict {assert_payload['strictFailure']['operator']}; "
    f"query {query_payload['stringified']}; "
    f"path resolved {path_payload['resolved']}; "
    f"os platform {os_payload['platform']}; "
    f"url file {url_payload['filePath']}; "
    f"buffer base64 {buffer_payload['base64']}; "
    f"process env {process_payload['nodeEnv']}; "
    "require failed closed"
)
PY
