// Minimal deterministic querystring shim for server-side package compatibility.
// It implements string-only parse/stringify helpers without reading host state.

function stringifyPrimitive(value) {
  if (typeof value === "string") return value;
  if (typeof value === "number" && Number.isFinite(value)) return String(value);
  if (typeof value === "bigint") return String(value);
  if (typeof value === "boolean") return value ? "true" : "false";
  return "";
}

function normalizeOptions(options) {
  return options && typeof options === "object" ? options : {};
}

function isHex(value) {
  return /^[0-9A-Fa-f]$/.test(value);
}

function isContinuation(byte) {
  return (byte & 0xc0) === 0x80;
}

function consumeInvalid(bytes, index, expectedContinuations) {
  let next = index + 1;
  let consumed = 0;
  while (next < bytes.length && consumed < expectedContinuations && isContinuation(bytes[next])) {
    next += 1;
    consumed += 1;
  }
  return next;
}

function decodeBytes(bytes) {
  let output = "";
  for (let index = 0; index < bytes.length; ) {
    const byte = bytes[index];
    if (byte < 0x80) {
      output += String.fromCodePoint(byte);
      index += 1;
    } else if (byte >= 0xc2 && byte <= 0xdf) {
      const byte1 = bytes[index + 1];
      if (index + 1 < bytes.length && isContinuation(byte1)) {
        output += String.fromCodePoint(((byte & 0x1f) << 6) | (byte1 & 0x3f));
        index += 2;
      } else {
        output += "\ufffd";
        index = consumeInvalid(bytes, index, 1);
      }
    } else if (byte >= 0xe0 && byte <= 0xef) {
      const byte1 = bytes[index + 1];
      const byte2 = bytes[index + 2];
      const valid =
        index + 2 < bytes.length &&
        isContinuation(byte1) &&
        isContinuation(byte2) &&
        (byte !== 0xe0 || byte1 >= 0xa0) &&
        (byte !== 0xed || byte1 < 0xa0);
      if (valid) {
        output += String.fromCodePoint(((byte & 0x0f) << 12) | ((byte1 & 0x3f) << 6) | (byte2 & 0x3f));
        index += 3;
      } else {
        output += "\ufffd";
        index = consumeInvalid(bytes, index, 2);
      }
    } else if (byte >= 0xf0 && byte <= 0xf4) {
      const byte1 = bytes[index + 1];
      const byte2 = bytes[index + 2];
      const byte3 = bytes[index + 3];
      const valid =
        index + 3 < bytes.length &&
        isContinuation(byte1) &&
        isContinuation(byte2) &&
        isContinuation(byte3) &&
        (byte !== 0xf0 || byte1 >= 0x90) &&
        (byte !== 0xf4 || byte1 < 0x90);
      if (valid) {
        output += String.fromCodePoint(
          ((byte & 0x07) << 18) | ((byte1 & 0x3f) << 12) | ((byte2 & 0x3f) << 6) | (byte3 & 0x3f)
        );
        index += 4;
      } else {
        output += "\ufffd";
        index = consumeInvalid(bytes, index, 3);
      }
    } else {
      output += "\ufffd";
      index += 1;
    }
  }
  return output;
}

function tolerantPercentDecode(text) {
  let output = "";
  for (let index = 0; index < text.length; ) {
    if (text[index] !== "%" || index + 2 >= text.length || !isHex(text[index + 1]) || !isHex(text[index + 2])) {
      output += text[index];
      index += 1;
      continue;
    }
    const bytes = [];
    while (text[index] === "%" && index + 2 < text.length && isHex(text[index + 1]) && isHex(text[index + 2])) {
      bytes.push(Number.parseInt(text.slice(index + 1, index + 3), 16));
      index += 3;
    }
    output += decodeBytes(bytes);
  }
  return output;
}

export function escape(value) {
  return encodeURIComponent(String(value));
}

export function unescape(value) {
  const text = String(value);
  try {
    return decodeURIComponent(text);
  } catch {
    return tolerantPercentDecode(text);
  }
}

function decodeComponent(value, decode) {
  return decode(String(value).replace(/\+/g, "%20"));
}

export function stringify(object, separator = "&", equals = "=", options = undefined) {
  if (object === null || object === undefined || typeof object !== "object") {
    return "";
  }
  separator = String(separator);
  equals = String(equals);
  const opts = normalizeOptions(options);
  const encodeComponent =
    typeof opts.encodeURIComponent === "function" ? opts.encodeURIComponent : querystring.escape;
  const pairs = [];
  for (const key of Object.keys(object)) {
    const encodedKey = encodeComponent(key);
    const value = object[key];
    if (Array.isArray(value)) {
      for (const entry of value) {
        pairs.push(`${encodedKey}${equals}${encodeComponent(stringifyPrimitive(entry))}`);
      }
    } else {
      pairs.push(`${encodedKey}${equals}${encodeComponent(stringifyPrimitive(value))}`);
    }
  }
  return pairs.join(separator);
}

export function parse(query = "", separator = "&", equals = "=", options = undefined) {
  const result = Object.create(null);
  if (typeof query !== "string" || query.length === 0) {
    return result;
  }
  separator = String(separator);
  equals = String(equals);
  const opts = normalizeOptions(options);
  const decode =
    typeof opts.decodeURIComponent === "function" ? opts.decodeURIComponent : querystring.unescape;
  const maxKeys = opts.maxKeys === 0 ? Number.POSITIVE_INFINITY : Math.max(0, Number(opts.maxKeys ?? 1000));
  if (maxKeys === 0) {
    return result;
  }
  let count = 0;
  let offset = 0;
  while (offset <= query.length) {
    if (count >= maxKeys) {
      break;
    }
    const separatorIndex = separator ? query.indexOf(separator, offset) : -1;
    const end = separatorIndex === -1 ? query.length : separatorIndex;
    const part = query.slice(offset, end);
    offset = separatorIndex === -1 ? query.length + 1 : separatorIndex + separator.length;
    count += 1;
    if (!part) {
      continue;
    }
    const index = part.indexOf(equals);
    const rawKey = index === -1 ? part : part.slice(0, index);
    const rawValue = index === -1 ? "" : part.slice(index + equals.length);
    const key = decodeComponent(rawKey, decode);
    const value = decodeComponent(rawValue, decode);
    if (Object.prototype.hasOwnProperty.call(result, key)) {
      const current = result[key];
      result[key] = Array.isArray(current) ? [...current, value] : [current, value];
    } else {
      result[key] = value;
    }
  }
  return result;
}

export const encode = stringify;
export const decode = parse;

const querystring = {
  decode,
  encode,
  escape,
  parse,
  stringify,
  unescape,
};

export default querystring;
