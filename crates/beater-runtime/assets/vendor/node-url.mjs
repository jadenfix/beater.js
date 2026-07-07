// Minimal deterministic file URL shim for server-side package compatibility.
// It performs string-only URL and file URL operations without reading host cwd,
// filesystem, environment, or platform path settings.

function assertString(value, label) {
  if (typeof value !== "string") {
    throw new TypeError(`${label} must be a string. Received ${typeof value}`);
  }
}

function encodeQuery(value) {
  return encodeURIComponent(String(value)).replace(/%20/g, "+");
}

function decodeQuery(value) {
  return decodeURIComponent(String(value).replace(/\+/g, "%20"));
}

export class URLSearchParams {
  #entries = [];

  constructor(init = "") {
    if (typeof init === "string") {
      const raw = init.startsWith("?") ? init.slice(1) : init;
      if (raw) {
        for (const pair of raw.split("&")) {
          const [name, value = ""] = pair.split("=", 2);
          this.append(decodeQuery(name), decodeQuery(value));
        }
      }
    } else if (Symbol.iterator in Object(init)) {
      for (const [name, value] of init) {
        this.append(name, value);
      }
    } else if (init && typeof init === "object") {
      for (const [name, value] of Object.entries(init)) {
        this.append(name, value);
      }
    }
  }

  append(name, value) {
    this.#entries.push([String(name), String(value)]);
  }

  get(name) {
    const found = this.#entries.find(([key]) => key === String(name));
    return found ? found[1] : null;
  }

  getAll(name) {
    return this.#entries
      .filter(([key]) => key === String(name))
      .map(([, value]) => value);
  }

  has(name) {
    return this.#entries.some(([key]) => key === String(name));
  }

  entries() {
    return this.#entries[Symbol.iterator]();
  }

  [Symbol.iterator]() {
    return this.entries();
  }

  toString() {
    return this.#entries
      .map(([name, value]) => `${encodeQuery(name)}=${encodeQuery(value)}`)
      .join("&");
  }
}

function splitSuffix(input) {
  let rest = input;
  let hash = "";
  const hashIndex = rest.indexOf("#");
  if (hashIndex !== -1) {
    hash = rest.slice(hashIndex);
    rest = rest.slice(0, hashIndex);
  }
  let search = "";
  const searchIndex = rest.indexOf("?");
  if (searchIndex !== -1) {
    search = rest.slice(searchIndex);
    rest = rest.slice(0, searchIndex);
  }
  return { rest, search, hash };
}

function normalizePath(path) {
  const absolute = path.startsWith("/");
  const trailing = path.endsWith("/");
  const output = [];
  for (const part of path.split("/")) {
    if (!part || part === ".") {
      continue;
    }
    if (part === "..") {
      if (output.length > 0) {
        output.pop();
      } else if (!absolute) {
        output.push("..");
      }
      continue;
    }
    output.push(part);
  }
  let normalized = `${absolute ? "/" : ""}${output.join("/")}`;
  if (!normalized) {
    normalized = absolute ? "/" : "";
  }
  if (trailing && normalized !== "/") {
    normalized += "/";
  }
  return normalized || "/";
}

function parseAbsolute(input) {
  const match = /^([A-Za-z][A-Za-z0-9+.-]*:)(.*)$/.exec(input);
  if (!match) {
    return null;
  }
  const protocol = match[1].toLowerCase();
  let rest = match[2];
  let host = "";
  if (rest.startsWith("//")) {
    rest = rest.slice(2);
    const hostEnd = rest.search(/[/?#]/);
    if (hostEnd === -1) {
      host = rest;
      rest = "";
    } else {
      host = rest.slice(0, hostEnd);
      rest = rest.slice(hostEnd);
    }
    host = host.split("@").pop().toLowerCase();
  }
  const { rest: path, search, hash } = splitSuffix(rest);
  return {
    protocol,
    host,
    pathname: path || (protocol === "file:" || host ? "/" : ""),
    search,
    hash,
  };
}

function parseUrl(input, base) {
  const absolute = parseAbsolute(String(input));
  if (absolute) {
    return absolute;
  }
  if (base === undefined) {
    throw new TypeError(`Invalid URL: ${input}`);
  }
  const baseUrl = base instanceof URL ? base : new URL(String(base));
  const { rest: relativePath, search, hash } = splitSuffix(String(input));
  let pathname;
  if (relativePath.startsWith("/")) {
    pathname = normalizePath(relativePath);
  } else {
    const baseDir = baseUrl.pathname.endsWith("/")
      ? baseUrl.pathname
      : baseUrl.pathname.slice(0, baseUrl.pathname.lastIndexOf("/") + 1);
    pathname = normalizePath(`${baseDir}${relativePath}`);
  }
  return {
    protocol: baseUrl.protocol,
    host: baseUrl.host,
    pathname,
    search,
    hash,
  };
}

export class URL {
  constructor(input, base) {
    const parsed = parseUrl(input, base);
    this.protocol = parsed.protocol;
    this.host = parsed.host;
    this.hostname = parsed.host.split(":", 1)[0];
    this.pathname = parsed.pathname;
    this.search = parsed.search;
    this.hash = parsed.hash;
    this.searchParams = new URLSearchParams(this.search);
  }

  get href() {
    const slashes = this.protocol === "file:" || this.host ? "//" : "";
    return `${this.protocol}${slashes}${this.host}${this.pathname}${this.search}${this.hash}`;
  }

  get origin() {
    return this.host ? `${this.protocol}//${this.host}` : "null";
  }

  toString() {
    return this.href;
  }

  toJSON() {
    return this.href;
  }
}

function installGlobal(name, value) {
  if (!globalThis[name]) {
    Object.defineProperty(globalThis, name, {
      value,
      writable: false,
      enumerable: false,
      configurable: true,
    });
  }
}

installGlobal("URL", URL);
installGlobal("URLSearchParams", URLSearchParams);

function toUrl(value) {
  if (value instanceof URL) {
    return value;
  }
  return new URL(String(value));
}

function assertSafeFilePathname(pathname) {
  if (/%2f|%5c/i.test(pathname)) {
    throw new TypeError("File URL path must not include encoded slash characters");
  }
}

export function fileURLToPath(value) {
  const url = toUrl(value);
  if (url.protocol !== "file:") {
    throw new TypeError("fileURLToPath only supports file: URLs");
  }
  if (url.hostname && url.hostname !== "localhost") {
    throw new TypeError("fileURLToPath only supports empty or localhost file URL hosts");
  }
  assertSafeFilePathname(url.pathname);
  return decodeURIComponent(url.pathname);
}

function encodePathSegment(segment) {
  return encodeURIComponent(segment).replace(/[!'()*]/g, (ch) =>
    `%${ch.charCodeAt(0).toString(16).toUpperCase()}`
  );
}

export function pathToFileURL(path) {
  assertString(path, "path");
  if (!path.startsWith("/")) {
    throw new TypeError("pathToFileURL requires an absolute POSIX path");
  }
  const pathname = path.split("/").map(encodePathSegment).join("/");
  return new URL(`file://${pathname}`);
}

const url = {
  URL,
  URLSearchParams,
  fileURLToPath,
  pathToFileURL,
};

export default url;
