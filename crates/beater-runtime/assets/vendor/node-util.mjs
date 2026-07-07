// Minimal deterministic util shim for server-side package compatibility.
// It is local to the isolate: no host environment, process state, warnings, or
// debug configuration are observed.

const customPromisify = Symbol.for("nodejs.util.promisify.custom");
const MAX_INSPECT_DEPTH = 2;
const MAX_INSPECT_ENTRIES = 32;
const MAX_INSPECT_STRING = 200;
const REPLACEMENT = "\uFFFD";

function truncateString(value) {
  const string = String(value);
  return string.length > MAX_INSPECT_STRING ? `${string.slice(0, MAX_INSPECT_STRING)}...` : string;
}

function quoteString(value) {
  return `'${truncateString(value).replace(/\\/g, "\\\\").replace(/'/g, "\\'")}'`;
}

function withRemaining(entries, rendered) {
  const remaining = entries.length - rendered.length;
  return remaining > 0 ? [...rendered, `... ${remaining} more items`] : rendered;
}

function inspectValue(value, seen, depth) {
  if (value === null) return "null";
  if (value === undefined) return "undefined";
  if (typeof value === "string") return quoteString(value);
  if (typeof value === "number" || typeof value === "bigint" || typeof value === "boolean") {
    return String(value);
  }
  if (typeof value === "symbol") return value.toString();
  if (typeof value === "function") {
    return `[Function${value.name ? `: ${value.name}` : ""}]`;
  }
  if (seen.has(value)) return "[Circular]";
  if (depth <= 0) return Array.isArray(value) ? "[Array]" : "[Object]";

    seen.add(value);
  try {
    if (Array.isArray(value)) {
      const entries = [];
      const count = Math.min(value.length, MAX_INSPECT_ENTRIES);
      for (let index = 0; index < count; index += 1) {
        const descriptor = Object.getOwnPropertyDescriptor(value, String(index));
        if (!descriptor) {
          entries.push("<empty>");
        } else if ("value" in descriptor) {
          entries.push(inspectValue(descriptor.value, seen, depth - 1));
        } else {
          entries.push("[Getter]");
        }
      }
      return `[ ${withRemaining(value, entries).join(", ")} ]`;
    }
    if (value instanceof Date) {
      return Number.isNaN(value.getTime()) ? "Invalid Date" : value.toISOString();
    }
    if (value instanceof RegExp) {
      return value.toString();
    }
    if (value instanceof Map) {
      const entries = [];
      for (const [key, entry] of value) {
        if (entries.length >= MAX_INSPECT_ENTRIES) break;
        entries.push(`${inspectValue(key, seen, depth - 1)} => ${inspectValue(entry, seen, depth - 1)}`);
      }
      return `Map(${value.size}) { ${withRemaining({ length: value.size }, entries).join(", ")} }`;
    }
    if (value instanceof Set) {
      const entries = [];
      for (const entry of value) {
        if (entries.length >= MAX_INSPECT_ENTRIES) break;
        entries.push(inspectValue(entry, seen, depth - 1));
      }
      return `Set(${value.size}) { ${withRemaining({ length: value.size }, entries).join(", ")} }`;
    }
    if (ArrayBuffer.isView(value) && !(value instanceof DataView)) {
      const entries = [...value.slice(0, MAX_INSPECT_ENTRIES)].map(String);
      return `${value.constructor.name}(${value.length}) [ ${withRemaining(value, entries).join(", ")} ]`;
    }
    const entries = [];
    let truncated = false;
    for (const key in value) {
      if (!Object.prototype.hasOwnProperty.call(value, key)) continue;
      if (entries.length >= MAX_INSPECT_ENTRIES) {
        truncated = true;
        break;
      }
      const descriptor = Object.getOwnPropertyDescriptor(value, key);
      if (!descriptor || !("value" in descriptor)) {
        entries.push(`${key}: [Getter]`);
      } else {
        entries.push(`${key}: ${inspectValue(descriptor.value, seen, depth - 1)}`);
      }
    }
    if (truncated) entries.push("... more items");
    return `{ ${entries.join(", ")} }`;
  } finally {
    seen.delete(value);
  }
}

export function inspect(value, options = undefined) {
  const depth = typeof options?.depth === "number" ? options.depth : MAX_INSPECT_DEPTH;
  return inspectValue(value, new Set(), depth);
}

function formatJson(value) {
  try {
    const json = JSON.stringify(value);
    return json === undefined ? "undefined" : json;
  } catch {
    return "[Circular]";
  }
}

export function format(first, ...args) {
  if (typeof first !== "string") {
    return [first, ...args].map((value) => inspect(value)).join(" ");
  }
  let index = 0;
  const formatted = first.replace(/%[sdifjoO%]/g, (token) => {
    if (token === "%%") return "%";
    if (index >= args.length) return token;
    const value = args[index++];
    switch (token) {
      case "%s":
        return String(value);
      case "%d":
      case "%i":
        return Number.parseInt(value, 10).toString();
      case "%f":
        return Number.parseFloat(value).toString();
      case "%j":
        return formatJson(value);
      case "%o":
      case "%O":
        return inspect(value);
      default:
        return token;
    }
  });
  if (index >= args.length) return formatted;
  return `${formatted} ${args.slice(index).map((value) => inspect(value)).join(" ")}`;
}

export function inherits(ctor, superCtor) {
  if (typeof ctor !== "function" || typeof superCtor !== "function") {
    throw new TypeError("util.inherits requires constructor functions");
  }
  ctor.super_ = superCtor;
  ctor.prototype = Object.create(superCtor.prototype, {
    constructor: {
      value: ctor,
      enumerable: false,
      writable: true,
      configurable: true,
    },
  });
  Object.setPrototypeOf(ctor, superCtor);
}

export function promisify(original) {
  if (typeof original !== "function") {
    throw new TypeError("util.promisify requires a function");
  }
  const custom = original[customPromisify];
  if (custom !== undefined) {
    if (typeof custom !== "function") {
      throw new TypeError("util.promisify.custom must be a function");
    }
    return custom;
  }
  function promisified(...args) {
    return new Promise((resolve, reject) => {
      original.call(this, ...args, (error, value) => {
        if (error) {
          reject(error);
        } else {
          resolve(value);
        }
      });
    });
  }
  Object.defineProperty(promisified, customPromisify, {
    value: promisified,
    enumerable: false,
  });
  return promisified;
}

Object.defineProperty(promisify, "custom", {
  value: customPromisify,
  enumerable: false,
});

export function callbackify(original) {
  if (typeof original !== "function") {
    throw new TypeError("util.callbackify requires a function");
  }
  return function callbackified(...args) {
    const callback = args.pop();
    if (typeof callback !== "function") {
      throw new TypeError("last argument must be a callback");
    }
    Promise.resolve()
      .then(() => original.apply(this, args))
      .then(
        (value) => queueMicrotask(() => callback(null, value)),
        (reason) => {
          let error = reason;
          if (!error) {
            error = new Error("Promise was rejected with a falsy value");
            error.reason = reason;
          }
          queueMicrotask(() => callback(error));
        }
      );
  };
}

function bytesFor(value) {
  if (value instanceof ArrayBuffer) {
    return new Uint8Array(value);
  }
  if (ArrayBuffer.isView(value)) {
    return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
  }
  return new Uint8Array();
}

function equalBytes(left, right) {
  const leftBytes = bytesFor(left);
  const rightBytes = bytesFor(right);
  return leftBytes.length === rightBytes.length && leftBytes.every((byte, index) => byte === rightBytes[index]);
}

function ownKeysEqual(left, right) {
  const leftKeys = Object.keys(left).sort();
  const rightKeys = Object.keys(right).sort();
  return leftKeys.length === rightKeys.length && leftKeys.every((key, index) => key === rightKeys[index]);
}

export function isDeepStrictEqual(left, right, seen = new Map()) {
  if (Object.is(left, right)) return true;
  if (typeof left !== "object" || left === null || typeof right !== "object" || right === null) {
    return false;
  }
  if (left.constructor !== right.constructor) return false;
  if (seen.get(left) === right) return true;
  seen.set(left, right);
  if (left instanceof Date) return left.getTime() === right.getTime();
  if (left instanceof RegExp) return left.source === right.source && left.flags === right.flags;
  if (left instanceof Error) return left.name === right.name && left.message === right.message;
  if (left instanceof ArrayBuffer || ArrayBuffer.isView(left)) return equalBytes(left, right);
  if (left instanceof Map) {
    if (left.size !== right.size) return false;
    const leftEntries = [...left.entries()];
    const rightEntries = [...right.entries()];
    return leftEntries.every(([leftKey, leftValue], index) => {
      const [rightKey, rightValue] = rightEntries[index];
      return isDeepStrictEqual(leftKey, rightKey, seen) && isDeepStrictEqual(leftValue, rightValue, seen);
    });
  }
  if (left instanceof Set) {
    if (left.size !== right.size) return false;
    const unmatched = [...right.values()];
    return [...left.values()].every((leftValue) => {
      const index = unmatched.findIndex((rightValue) => isDeepStrictEqual(leftValue, rightValue, seen));
      if (index === -1) return false;
      unmatched.splice(index, 1);
      return true;
    });
  }
  if (Array.isArray(left)) {
    return left.length === right.length && left.every((value, index) => isDeepStrictEqual(value, right[index], seen));
  }
  if (!ownKeysEqual(left, right)) return false;
  return Object.keys(left).every((key) => isDeepStrictEqual(left[key], right[key], seen));
}

export function deprecate(fn) {
  if (typeof fn !== "function") {
    throw new TypeError("util.deprecate requires a function");
  }
  return function deprecated(...args) {
    return fn.apply(this, args);
  };
}

export function debuglog() {
  const logger = function noopDebuglog() {};
  logger.enabled = false;
  return logger;
}

export const types = Object.freeze({
  isAnyArrayBuffer(value) {
    return (
      value instanceof ArrayBuffer ||
      (typeof SharedArrayBuffer !== "undefined" && value instanceof SharedArrayBuffer)
    );
  },
  isArrayBuffer(value) {
    return value instanceof ArrayBuffer;
  },
  isArrayBufferView(value) {
    return ArrayBuffer.isView(value);
  },
  isDate(value) {
    return value instanceof Date;
  },
  isDataView(value) {
    return value instanceof DataView;
  },
  isMap(value) {
    return value instanceof Map;
  },
  isNativeError(value) {
    return value instanceof Error;
  },
  isPromise(value) {
    return value instanceof Promise;
  },
  isRegExp(value) {
    return value instanceof RegExp;
  },
  isSet(value) {
    return value instanceof Set;
  },
  isTypedArray(value) {
    return ArrayBuffer.isView(value) && !(value instanceof DataView);
  },
  isUint8Array(value) {
    return value instanceof Uint8Array;
  },
});

function encodeUtf8(input) {
  const bytes = [];
  for (let index = 0; index < input.length; index += 1) {
    let codePoint = input.codePointAt(index);
    if (codePoint > 0xffff) index += 1;
    if (codePoint <= 0x7f) {
      bytes.push(codePoint);
    } else if (codePoint <= 0x7ff) {
      bytes.push(0xc0 | (codePoint >> 6), 0x80 | (codePoint & 0x3f));
    } else if (codePoint <= 0xffff) {
      bytes.push(0xe0 | (codePoint >> 12), 0x80 | ((codePoint >> 6) & 0x3f), 0x80 | (codePoint & 0x3f));
    } else {
      bytes.push(
        0xf0 | (codePoint >> 18),
        0x80 | ((codePoint >> 12) & 0x3f),
        0x80 | ((codePoint >> 6) & 0x3f),
        0x80 | (codePoint & 0x3f)
      );
    }
  }
  return new Uint8Array(bytes);
}

function decodeUtf8(input) {
  const bytes = input instanceof Uint8Array ? input : bytesFor(input);
  let out = "";
  for (let index = 0; index < bytes.length; index += 1) {
    const first = bytes[index];
    if (first < 0x80) {
      out += String.fromCodePoint(first);
    } else if (first >= 0xc2 && first < 0xe0) {
      const second = bytes[index + 1];
      if ((second & 0xc0) !== 0x80) {
        out += REPLACEMENT;
        continue;
      }
      index += 1;
      out += String.fromCodePoint(((first & 0x1f) << 6) | (second & 0x3f));
    } else if (first >= 0xe0 && first < 0xf0) {
      const second = bytes[index + 1];
      const third = bytes[index + 2];
      if ((second & 0xc0) !== 0x80 || (third & 0xc0) !== 0x80) {
        out += REPLACEMENT;
        continue;
      }
      const codePoint = ((first & 0x0f) << 12) | ((second & 0x3f) << 6) | (third & 0x3f);
      if (codePoint < 0x800 || (codePoint >= 0xd800 && codePoint <= 0xdfff)) {
        out += REPLACEMENT;
        continue;
      }
      index += 2;
      out += String.fromCodePoint(codePoint);
    } else if (first >= 0xf0 && first <= 0xf4) {
      const second = bytes[index + 1];
      const third = bytes[index + 2];
      const fourth = bytes[index + 3];
      if ((second & 0xc0) !== 0x80 || (third & 0xc0) !== 0x80 || (fourth & 0xc0) !== 0x80) {
        out += REPLACEMENT;
        continue;
      }
      const codePoint =
        ((first & 0x07) << 18) |
        ((second & 0x3f) << 12) |
        ((third & 0x3f) << 6) |
        (fourth & 0x3f);
      if (codePoint < 0x10000 || codePoint > 0x10ffff) {
        out += REPLACEMENT;
        continue;
      }
      index += 3;
      out += String.fromCodePoint(codePoint);
    } else {
      out += REPLACEMENT;
    }
  }
  return out;
}

class BeaterTextEncoder {
  get encoding() {
    return "utf-8";
  }

  encode(input = "") {
    return encodeUtf8(String(input));
  }
}

class BeaterTextDecoder {
  constructor(label = "utf-8") {
    if (String(label).toLowerCase() !== "utf-8") {
      throw new RangeError("only utf-8 TextDecoder is supported by beater.js util");
    }
  }

  get encoding() {
    return "utf-8";
  }

  decode(input = new Uint8Array()) {
    return decodeUtf8(input);
  }
}

export const TextEncoder = globalThis.TextEncoder ?? BeaterTextEncoder;
export const TextDecoder = globalThis.TextDecoder ?? BeaterTextDecoder;

const util = {
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
};

export default util;
