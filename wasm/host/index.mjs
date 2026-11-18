// Runs agent-browser.wasm (wasm32-wasip1) in a JavaScript host such as a
// Cloudflare Worker or Node.js.
//
// The module is the upstream CLI. It expects an operating system, so this
// file is a small one: WASI preview 1 over an in-memory file system, plus
// the `agent_browser_host` imports for what WASI lacks (an event loop,
// WebSockets, HTTP, loopback connections and processes). Each
// `agent-browser ...` invocation is a short-lived instance, and the daemon
// it starts is a long-lived instance of the same module, as on a real OS.
//
// The module blocks inside imports, so the engine needs JavaScript Promise
// Integration (`WebAssembly.Suspending`). workerd and Node.js 24 have it.

const ERRNO = {
  SUCCESS: 0, BADF: 8, EXIST: 20, INVAL: 28, ISDIR: 31, NOENT: 44,
  NOSYS: 52, NOTDIR: 54, NOTEMPTY: 55, PIPE: 64,
};
const FILETYPE = { CHARACTER_DEVICE: 2, DIRECTORY: 3, REGULAR_FILE: 4, SOCKET_STREAM: 6 };
const OFLAGS = { CREAT: 1, DIRECTORY: 2, EXCL: 4, TRUNC: 8 };
const FDFLAGS_APPEND = 1;
const EVENT = { TIMER: 1, WEBSOCKET: 2, FETCH: 3, ACCEPT: 4, CONN: 5 };
const ROOT_FD = 3;

const encoder = new TextEncoder();
const decoder = new TextDecoder();

class Exit {
  constructor(code) {
    this.code = code;
  }
}

/** Files shared by every instance. Paths are absolute and normalized. */
export class MemoryFileSystem {
  constructor() {
    /** @type {Map<string, { dir: true } | { dir: false, data: Uint8Array, mtime: number }>} */
    this.nodes = new Map([["/", { dir: true }]]);
  }

  static normalize(path) {
    const parts = [];
    for (const part of path.split("/")) {
      if (part === "" || part === ".") continue;
      if (part === "..") parts.pop();
      else parts.push(part);
    }
    return "/" + parts.join("/");
  }

  static parent(path) {
    return path.slice(0, path.lastIndexOf("/")) || "/";
  }

  mkdirp(path) {
    const parts = MemoryFileSystem.normalize(path).split("/").filter(Boolean);
    let current = "";
    for (const part of parts) {
      current += "/" + part;
      if (!this.nodes.has(current)) this.nodes.set(current, { dir: true });
    }
  }

  /** @param {string} path @param {Uint8Array | string} data */
  writeFile(path, data) {
    path = MemoryFileSystem.normalize(path);
    this.mkdirp(MemoryFileSystem.parent(path));
    const bytes = typeof data === "string" ? encoder.encode(data) : data;
    this.nodes.set(path, { dir: false, data: bytes, mtime: Date.now() });
  }

  /** @returns {Uint8Array | undefined} */
  readFile(path) {
    const node = this.nodes.get(MemoryFileSystem.normalize(path));
    return node && !node.dir ? node.data : undefined;
  }

  remove(path) {
    this.nodes.delete(MemoryFileSystem.normalize(path));
  }

  /** Names directly inside `path`. */
  list(path) {
    const prefix = path === "/" ? "/" : path + "/";
    const names = [];
    for (const key of this.nodes.keys()) {
      if (key !== path && key.startsWith(prefix) && !key.slice(prefix.length).includes("/")) {
        names.push(key.slice(prefix.length));
      }
    }
    return names.sort();
  }

  /** Paths of every file, for copying results out. */
  files() {
    return [...this.nodes].filter(([, node]) => !node.dir).map(([path]) => path);
  }
}

function defaultWebSocket(url, headers) {
  if (headers.length > 0 && typeof WebSocketPair !== "undefined") {
    // Workers: only a fetch upgrade can carry request headers.
    return fetch(url.replace(/^ws/, "http"), {
      headers: { Upgrade: "websocket", ...Object.fromEntries(headers) },
    }).then((response) => {
      if (!response.webSocket) throw new Error(`WebSocket upgrade failed: ${response.status}`);
      response.webSocket.accept();
      return response.webSocket;
    });
  }
  return new WebSocket(url);
}

/**
 * @param {object} options
 * @param {WebAssembly.Module} options.module agent-browser.wasm, compiled.
 * @param {Record<string, string>} [options.env] Environment of every invocation.
 * @param {MemoryFileSystem} [options.fs]
 * @param {(url: string, headers: [string, string][]) => WebSocket | Promise<WebSocket>} [options.webSocket]
 *   Opens the CDP connection. Defaults to `new WebSocket(url)`.
 * @param {typeof fetch} [options.fetch]
 */
export function createAgentBrowser(options) {
  if (typeof WebAssembly.Suspending !== "function") {
    throw new Error("agent-browser.wasm needs JavaScript Promise Integration (WebAssembly.Suspending)");
  }
  const host = new Host(options);
  return {
    fs: host.fs,
    /**
     * Runs `agent-browser <argv>`.
     * @param {string[]} argv
     * @param {{ env?: Record<string, string>, stdin?: string }} [run]
     * @returns {Promise<{ exitCode: number, stdout: string, stderr: string }>}
     */
    async run(argv, run = {}) {
      const process = host.start(["agent-browser", ...argv], { ...host.env, ...run.env }, run.stdin);
      const exitCode = await process.exited;
      return { exitCode, stdout: process.output(1), stderr: process.output(2) };
    },
    /** Stderr and stdout of the daemons, for debugging. */
    daemonLogs() {
      return [...host.exitedDaemons, ...host.processes.values()]
        .filter((process) => process.daemon)
        .map((process) => process.output(2) + process.output(1));
    },
    /** Drops every instance, timer and socket. */
    dispose() {
      for (const process of host.processes.values()) process.release();
      host.processes.clear();
      host.listeners.clear();
    },
  };
}

class Host {
  constructor(options) {
    this.module = options.module;
    this.env = { HOME: "/home", TMPDIR: "/tmp", ...options.env };
    this.fs = options.fs ?? new MemoryFileSystem();
    this.fs.mkdirp(this.env.HOME);
    this.fs.mkdirp(this.env.TMPDIR);
    this.webSocket = options.webSocket ?? defaultWebSocket;
    this.fetch = options.fetch ?? ((...args) => fetch(...args));
    /** @type {Map<number, Process>} */
    this.processes = new Map();
    /** @type {Map<number, Process>} */
    this.listeners = new Map();
    /** @type {Process[]} */
    this.exitedDaemons = [];
    this.nextPid = 1;
    this.nextPort = 49152;
  }

  start(args, env, stdin, daemon = false) {
    const process = new Process(this, this.nextPid++, args, env, stdin, daemon);
    this.processes.set(process.pid, process);
    process.exited = process.run().finally(() => {
      process.release();
      this.processes.delete(process.pid);
      if (daemon) this.exitedDaemons = [...this.exitedDaemons.slice(-4), process];
    });
    return process;
  }
}

class Process {
  constructor(host, pid, args, env, stdin, daemon) {
    this.host = host;
    this.pid = pid;
    this.args = args;
    this.env = env;
    this.daemon = daemon;
    this.alive = true;
    /** @type {Map<number, any>} */
    this.fds = new Map([
      [0, { kind: "stdin", data: encoder.encode(stdin ?? ""), position: 0 }],
      [1, { kind: "output", chunks: [] }],
      [2, { kind: "output", chunks: [] }],
      [ROOT_FD, { kind: "dir", path: "/" }],
    ]);
    this.nextFd = 4;
    /** @type {Uint8Array[]} */
    this.events = [];
    this.currentEvent = new Uint8Array(0);
    this.wake = null;
    this.timers = new Map();
    this.sockets = new Map();
    this.fetches = new Map();
  }

  output(fd) {
    return this.fds.get(fd).chunks.map((chunk) => decoder.decode(chunk)).join("");
  }

  async run() {
    const imports = { wasi_snapshot_preview1: this.wasi(), agent_browser_host: this.hostImports() };
    for (const { module, name, kind } of WebAssembly.Module.imports(this.host.module)) {
      if (kind === "function" && !imports[module]?.[name]) (imports[module] ??= {})[name] = () => ERRNO.NOSYS;
    }
    const instance = new WebAssembly.Instance(this.host.module, imports);
    this.memory = instance.exports.memory;
    try {
      await WebAssembly.promising(instance.exports._start)();
      return 0;
    } catch (error) {
      if (error instanceof Exit) return error.code;
      this.fds.get(2).chunks.push(encoder.encode(`agent-browser.wasm stopped: ${error?.stack ?? error}\n`));
      return 134;
    }
  }

  /** Called once the instance is gone: closes what it left open. */
  release() {
    this.alive = false;
    for (const timer of this.timers.values()) clearTimeout(timer);
    for (const socket of this.sockets.values()) socket.then?.((ws) => ws.close(), () => {}) ?? socket.close?.();
    for (const controller of this.fetches.values()) controller.abort();
    for (const [port, owner] of this.host.listeners) if (owner === this) this.host.listeners.delete(port);
    for (const [fd, entry] of [...this.fds]) {
      if (entry.kind === "conn") this.closeConn(fd);
      else if (entry.kind === "file") this.closeFile(fd);
    }
  }

  // Memory access. The buffer is replaced when memory grows, so views are
  // made per call.

  view() {
    return new DataView(this.memory.buffer);
  }

  bytes(ptr, len) {
    return new Uint8Array(this.memory.buffer, ptr, len);
  }

  string(ptr, len) {
    return decoder.decode(this.bytes(ptr, len));
  }

  path(dirFd, ptr, len) {
    const dir = this.fds.get(dirFd);
    if (!dir || dir.kind !== "dir") return undefined;
    return MemoryFileSystem.normalize(dir.path + "/" + this.string(ptr, len));
  }

  // Events for the instance's `event_wait`.

  push(kind, id, ...parts) {
    if (!this.alive) return;
    const size = parts.reduce((total, part) => total + part.length, 5);
    const event = new Uint8Array(size);
    event[0] = kind;
    new DataView(event.buffer).setUint32(1, id, true);
    let offset = 5;
    for (const part of parts) {
      event.set(part, offset);
      offset += part.length;
    }
    this.events.push(event);
    this.wake?.();
    this.wake = null;
  }

  closeFile(fd) {
    const entry = this.fds.get(fd);
    this.fds.delete(fd);
    const node = entry?.node;
    if (node?.lockedBy === entry) {
      node.lockedBy = null;
      node.lockWaiters?.splice(0).forEach((resolve) => resolve());
    }
  }

  closeConn(fd) {
    const conn = this.fds.get(fd);
    if (!conn || conn.kind !== "conn") return;
    this.fds.delete(fd);
    const peer = conn.peer;
    if (!peer.closed) {
      conn.closed = true;
      if (peer.async) peer.process.push(EVENT.CONN, peer.fd);
      peer.wake?.();
    }
  }

  writeConn(conn, data) {
    const peer = conn.peer;
    if (peer.closed || !peer.process.alive) return false;
    if (peer.async) peer.process.push(EVENT.CONN, peer.fd, data);
    else {
      peer.received.push(data.slice());
      peer.wake?.();
    }
    return true;
  }

  hostImports() {
    const host = this.host;
    return {
      event_wait: new WebAssembly.Suspending(async () => {
        while (this.events.length === 0) {
          await new Promise((resolve) => (this.wake = resolve));
        }
        this.currentEvent = this.events.shift();
        return this.currentEvent.length;
      }),
      event_read: (ptr) => this.bytes(ptr, this.currentEvent.length).set(this.currentEvent),

      timer_set: (id, ms) => {
        this.timers.set(id, setTimeout(() => {
          this.timers.delete(id);
          this.push(EVENT.TIMER, id);
        }, ms));
      },
      timer_clear: (id) => {
        clearTimeout(this.timers.get(id));
        this.timers.delete(id);
      },

      ws_open: (id, urlPtr, urlLen, headersPtr, headersLen) => {
        const url = this.string(urlPtr, urlLen);
        const headers = JSON.parse(this.string(headersPtr, headersLen));
        const event = (kind, text = "") => this.push(EVENT.WEBSOCKET, id, [kind], encoder.encode(text));
        const opening = Promise.resolve()
          .then(() => host.webSocket(url, headers))
          .then((ws) => {
            ws.addEventListener("message", ({ data }) => {
              event(1, typeof data === "string" ? data : decoder.decode(data));
            });
            ws.addEventListener("close", ({ reason }) => event(2, reason));
            ws.addEventListener("error", (e) => event(3, e.message ?? "WebSocket error"));
            if (ws.readyState === 1) event(0);
            else ws.addEventListener("open", () => event(0));
            this.sockets.set(id, ws);
            return ws;
          });
        opening.catch((error) => event(3, String(error?.message ?? error)));
        this.sockets.set(id, opening);
      },
      ws_send: (id, ptr, len) => {
        const ws = this.sockets.get(id);
        if (!ws || ws.readyState !== 1) return 1;
        ws.send(this.string(ptr, len));
        return 0;
      },
      ws_close: (id) => {
        const ws = this.sockets.get(id);
        this.sockets.delete(id);
        if (ws?.then) ws.then((opened) => opened.close(), () => {});
        else ws?.close();
      },

      fetch_start: (id, requestPtr, requestLen, bodyPtr, bodyLen) => {
        const request = JSON.parse(this.string(requestPtr, requestLen));
        const body = bodyLen > 0 ? this.bytes(bodyPtr, bodyLen).slice() : undefined;
        const controller = new AbortController();
        this.fetches.set(id, controller);
        const timer = request.timeout_ms == null ? undefined
          : setTimeout(() => controller.abort("timeout"), request.timeout_ms);
        const done = (status, head, payload = new Uint8Array(0)) => {
          clearTimeout(timer);
          if (!this.fetches.delete(id)) return;
          const headBytes = encoder.encode(JSON.stringify(head));
          const prefix = new Uint8Array(8);
          new DataView(prefix.buffer).setUint32(0, status, true);
          new DataView(prefix.buffer).setUint32(4, headBytes.length, true);
          this.push(EVENT.FETCH, id, prefix, headBytes, payload);
        };
        host.fetch(request.url, {
          method: request.method,
          headers: request.headers,
          body,
          signal: controller.signal,
        }).then(async (response) => {
          const payload = new Uint8Array(await response.arrayBuffer());
          done(response.status, { url: response.url || request.url, headers: [...response.headers] }, payload);
        }).catch((error) => {
          done(0, { message: String(error?.message ?? error), timeout: controller.signal.reason === "timeout" });
        });
      },
      fetch_abort: (id) => {
        this.fetches.get(id)?.abort();
        this.fetches.delete(id);
      },

      tcp_listen: (port) => {
        if (port === 0) {
          while (host.listeners.has(host.nextPort)) host.nextPort++;
          port = host.nextPort++;
        }
        if (host.listeners.has(port)) return -1;
        host.listeners.set(port, this);
        return port;
      },
      tcp_unlisten: (port) => {
        if (host.listeners.get(port) === this) host.listeners.delete(port);
      },
      tcp_connect: (port) => {
        const listener = host.listeners.get(port);
        if (!listener?.alive) return -1;
        const near = { kind: "conn", process: this, fd: this.nextFd++, async: false, received: [], closed: false };
        const far = { kind: "conn", process: listener, fd: listener.nextFd++, async: true, closed: false };
        near.peer = far;
        far.peer = near;
        this.fds.set(near.fd, near);
        listener.fds.set(far.fd, far);
        const fd = new Uint8Array(4);
        new DataView(fd.buffer).setUint32(0, far.fd, true);
        listener.push(EVENT.ACCEPT, port, fd);
        return near.fd;
      },
      conn_write: (fd, ptr, len) => {
        const conn = this.fds.get(fd);
        return conn?.kind === "conn" && this.writeConn(conn, this.bytes(ptr, len).slice()) ? 0 : 1;
      },
      conn_close: (fd) => this.closeConn(fd),

      // An exclusive lock on an open file, held until the fd is closed.
      file_lock: new WebAssembly.Suspending(async (fd) => {
        const entry = this.fds.get(fd);
        if (entry?.kind !== "file") return 1;
        const node = entry.node;
        while (node.lockedBy && node.lockedBy !== entry) {
          await new Promise((resolve) => (node.lockWaiters ??= []).push(resolve));
        }
        node.lockedBy = entry;
        return 0;
      }),

      process_id: () => this.pid,
      process_alive: (pid) => (host.processes.get(pid)?.alive ? 1 : 0),
      spawn: (ptr, len) => {
        const [args, env] = JSON.parse(this.string(ptr, len));
        const child = host.start(args, { ...this.env, ...Object.fromEntries(env) }, "", true);
        child.exited.catch(() => {});
        return child.pid;
      },
    };
  }

  wasi() {
    const fs = this.host.fs;
    const strings = (values) => values.map((value) => encoder.encode(value + "\0"));
    const writeStrings = (values, pointersPtr, bufferPtr) => {
      const view = this.view();
      for (const value of strings(values)) {
        view.setUint32(pointersPtr, bufferPtr, true);
        this.bytes(bufferPtr, value.length).set(value);
        pointersPtr += 4;
        bufferPtr += value.length;
      }
      return ERRNO.SUCCESS;
    };
    const sizes = (values, countPtr, sizePtr) => {
      const view = this.view();
      view.setUint32(countPtr, values.length, true);
      view.setUint32(sizePtr, strings(values).reduce((total, value) => total + value.length, 0), true);
      return ERRNO.SUCCESS;
    };
    const environ = () => Object.entries(this.env).map(([key, value]) => `${key}=${value}`);
    const iovecs = (ptr, count) => {
      const view = this.view();
      const result = [];
      for (let i = 0; i < count; i++) {
        result.push([view.getUint32(ptr + i * 8, true), view.getUint32(ptr + i * 8 + 4, true)]);
      }
      return result;
    };
    const filestat = (ptr, node) => {
      const view = this.view();
      this.bytes(ptr, 64).fill(0);
      view.setUint8(ptr + 16, node.dir ? FILETYPE.DIRECTORY : FILETYPE.REGULAR_FILE);
      view.setBigUint64(ptr + 24, 1n, true);
      view.setBigUint64(ptr + 32, BigInt(node.dir ? 0 : node.data.length), true);
      const time = BigInt(Math.round(node.mtime ?? 0)) * 1_000_000n;
      view.setBigUint64(ptr + 40, time, true);
      view.setBigUint64(ptr + 48, time, true);
      view.setBigUint64(ptr + 56, time, true);
      return ERRNO.SUCCESS;
    };
    const fill = (ptr, len, data) => {
      const count = Math.min(len, data.length);
      this.bytes(ptr, count).set(data.subarray(0, count));
      return count;
    };

    return {
      args_get: (pointers, buffer) => writeStrings(this.args, pointers, buffer),
      args_sizes_get: (count, size) => sizes(this.args, count, size),
      environ_get: (pointers, buffer) => writeStrings(environ(), pointers, buffer),
      environ_sizes_get: (count, size) => sizes(environ(), count, size),

      clock_time_get: (id, _precision, ptr) => {
        const ms = id === 0 ? Date.now() : performance.now();
        this.view().setBigUint64(ptr, BigInt(Math.round(ms * 1e6)), true);
        return ERRNO.SUCCESS;
      },
      random_get: (ptr, len) => {
        for (let offset = 0; offset < len; offset += 65536) {
          crypto.getRandomValues(this.bytes(ptr + offset, Math.min(65536, len - offset)));
        }
        return ERRNO.SUCCESS;
      },
      sched_yield: () => ERRNO.SUCCESS,
      proc_exit: (code) => {
        throw new Exit(code);
      },
      // Only clock subscriptions occur: this is `std::thread::sleep`.
      poll_oneoff: new WebAssembly.Suspending(async (inPtr, outPtr, count, countPtr) => {
        const view = this.view();
        let wait = 0;
        for (let i = 0; i < count; i++) {
          const base = inPtr + i * 48;
          if (view.getUint8(base + 8) === 0) {
            wait = Math.max(wait, Number(view.getBigUint64(base + 24, true)) / 1e6);
          }
        }
        await new Promise((resolve) => setTimeout(resolve, wait));
        const after = this.view();
        for (let i = 0; i < count; i++) {
          this.bytes(outPtr + i * 32, 32).fill(0);
          after.setBigUint64(outPtr + i * 32, after.getBigUint64(inPtr + i * 48, true), true);
          after.setUint8(outPtr + i * 32 + 10, after.getUint8(inPtr + i * 48 + 8));
        }
        after.setUint32(countPtr, count, true);
        return ERRNO.SUCCESS;
      }),

      fd_prestat_get: (fd, ptr) => {
        if (fd !== ROOT_FD) return ERRNO.BADF;
        const view = this.view();
        view.setUint8(ptr, 0);
        view.setUint32(ptr + 4, 1, true);
        return ERRNO.SUCCESS;
      },
      fd_prestat_dir_name: (fd, ptr) => {
        if (fd !== ROOT_FD) return ERRNO.BADF;
        this.bytes(ptr, 1).set(encoder.encode("/"));
        return ERRNO.SUCCESS;
      },
      fd_fdstat_get: (fd, ptr) => {
        const entry = this.fds.get(fd);
        if (!entry) return ERRNO.BADF;
        const type = { dir: FILETYPE.DIRECTORY, file: FILETYPE.REGULAR_FILE, conn: FILETYPE.SOCKET_STREAM }[entry.kind]
          ?? FILETYPE.CHARACTER_DEVICE;
        const view = this.view();
        this.bytes(ptr, 24).fill(0);
        view.setUint8(ptr, type);
        view.setUint16(ptr + 2, entry.append ? FDFLAGS_APPEND : 0, true);
        view.setBigUint64(ptr + 8, 0xffffffffffffffffn, true);
        view.setBigUint64(ptr + 16, 0xffffffffffffffffn, true);
        return ERRNO.SUCCESS;
      },
      fd_filestat_get: (fd, ptr) => {
        const entry = this.fds.get(fd);
        if (!entry) return ERRNO.BADF;
        if (entry.kind === "dir") return filestat(ptr, { dir: true });
        if (entry.kind === "file") return filestat(ptr, entry.node);
        return filestat(ptr, { dir: false, data: new Uint8Array(0) });
      },
      fd_close: (fd) => {
        const entry = this.fds.get(fd);
        if (!entry) return ERRNO.BADF;
        if (entry.kind === "conn") this.closeConn(fd);
        else this.closeFile(fd);
        return ERRNO.SUCCESS;
      },
      fd_sync: () => ERRNO.SUCCESS,
      fd_seek: (fd, offset, whence, ptr) => {
        const entry = this.fds.get(fd);
        if (entry?.kind !== "file") return ERRNO.BADF;
        const base = [0, entry.position, entry.node.data.length][whence];
        entry.position = Math.max(0, base + Number(offset));
        this.view().setBigUint64(ptr, BigInt(entry.position), true);
        return ERRNO.SUCCESS;
      },
      fd_read: new WebAssembly.Suspending(async (fd, iovsPtr, iovsLen, readPtr) => {
        const entry = this.fds.get(fd);
        if (!entry) return ERRNO.BADF;
        let source;
        if (entry.kind === "file") source = entry.node.data.subarray(entry.position);
        else if (entry.kind === "stdin") source = entry.data.subarray(entry.position);
        else if (entry.kind === "conn") {
          // The blocking end of a loopback connection.
          while (entry.received.length === 0 && !entry.peer.closed && entry.peer.process.alive) {
            await new Promise((resolve) => (entry.wake = resolve));
          }
          source = entry.received[0] ?? new Uint8Array(0);
        } else return ERRNO.BADF;
        let read = 0;
        for (const [ptr, len] of iovecs(iovsPtr, iovsLen)) {
          read += fill(ptr, len, source.subarray(read));
          if (read === source.length) break;
        }
        if (entry.kind === "conn") {
          if (read === source.length) entry.received.shift();
          else entry.received[0] = source.subarray(read);
        } else entry.position += read;
        this.view().setUint32(readPtr, read, true);
        return ERRNO.SUCCESS;
      }),
      fd_write: (fd, iovsPtr, iovsLen, writtenPtr) => {
        const entry = this.fds.get(fd);
        if (!entry) return ERRNO.BADF;
        const parts = iovecs(iovsPtr, iovsLen).map(([ptr, len]) => this.bytes(ptr, len).slice());
        const size = parts.reduce((total, part) => total + part.length, 0);
        const data = new Uint8Array(size);
        parts.reduce((offset, part) => (data.set(part, offset), offset + part.length), 0);
        if (entry.kind === "output") entry.chunks.push(data);
        else if (entry.kind === "conn") {
          if (!this.writeConn(entry, data)) return ERRNO.PIPE;
        } else if (entry.kind === "file") {
          const node = entry.node;
          const at = entry.append ? node.data.length : entry.position;
          if (at + size > node.data.length) {
            const grown = new Uint8Array(at + size);
            grown.set(node.data);
            node.data = grown;
          }
          node.data.set(data, at);
          node.mtime = Date.now();
          entry.position = at + size;
        } else return ERRNO.BADF;
        this.view().setUint32(writtenPtr, size, true);
        return ERRNO.SUCCESS;
      },
      fd_readdir: (fd, ptr, len, cookie, usedPtr) => {
        const entry = this.fds.get(fd);
        if (entry?.kind !== "dir") return ERRNO.BADF;
        const names = fs.list(entry.path);
        let used = 0;
        for (let index = Number(cookie); index < names.length && used < len; index++) {
          const name = encoder.encode(names[index]);
          const child = fs.nodes.get((entry.path === "/" ? "" : entry.path) + "/" + names[index]);
          const record = new Uint8Array(24 + name.length);
          const view = new DataView(record.buffer);
          view.setBigUint64(0, BigInt(index + 1), true);
          view.setUint32(16, name.length, true);
          view.setUint8(20, child?.dir ? FILETYPE.DIRECTORY : FILETYPE.REGULAR_FILE);
          record.set(name, 24);
          used += fill(ptr + used, len - used, record);
        }
        this.view().setUint32(usedPtr, used, true);
        return ERRNO.SUCCESS;
      },

      path_open: (dirFd, _dirFlags, pathPtr, pathLen, oflags, _rights, _inheriting, fdflags, fdPtr) => {
        const path = this.path(dirFd, pathPtr, pathLen);
        if (path === undefined) return ERRNO.BADF;
        let node = fs.nodes.get(path);
        if (node && oflags & OFLAGS.EXCL) return ERRNO.EXIST;
        if (!node) {
          if (!(oflags & OFLAGS.CREAT)) return ERRNO.NOENT;
          if (!fs.nodes.get(MemoryFileSystem.parent(path))?.dir) return ERRNO.NOENT;
          node = { dir: false, data: new Uint8Array(0), mtime: Date.now() };
          fs.nodes.set(path, node);
        }
        if (oflags & OFLAGS.DIRECTORY && !node.dir) return ERRNO.NOTDIR;
        if (!node.dir && oflags & OFLAGS.TRUNC) node.data = new Uint8Array(0);
        const fd = this.nextFd++;
        this.fds.set(fd, node.dir
          ? { kind: "dir", path }
          : { kind: "file", node, position: 0, append: Boolean(fdflags & FDFLAGS_APPEND) });
        this.view().setUint32(fdPtr, fd, true);
        return ERRNO.SUCCESS;
      },
      path_filestat_get: (dirFd, _flags, pathPtr, pathLen, ptr) => {
        const path = this.path(dirFd, pathPtr, pathLen);
        if (path === undefined) return ERRNO.BADF;
        const node = fs.nodes.get(path);
        return node ? filestat(ptr, node) : ERRNO.NOENT;
      },
      path_create_directory: (dirFd, pathPtr, pathLen) => {
        const path = this.path(dirFd, pathPtr, pathLen);
        if (path === undefined) return ERRNO.BADF;
        if (fs.nodes.has(path)) return ERRNO.EXIST;
        if (!fs.nodes.get(MemoryFileSystem.parent(path))?.dir) return ERRNO.NOENT;
        fs.nodes.set(path, { dir: true });
        return ERRNO.SUCCESS;
      },
      path_remove_directory: (dirFd, pathPtr, pathLen) => {
        const path = this.path(dirFd, pathPtr, pathLen);
        if (path === undefined) return ERRNO.BADF;
        const node = fs.nodes.get(path);
        if (!node) return ERRNO.NOENT;
        if (!node.dir) return ERRNO.NOTDIR;
        if (fs.list(path).length > 0) return ERRNO.NOTEMPTY;
        fs.nodes.delete(path);
        return ERRNO.SUCCESS;
      },
      path_unlink_file: (dirFd, pathPtr, pathLen) => {
        const path = this.path(dirFd, pathPtr, pathLen);
        if (path === undefined) return ERRNO.BADF;
        const node = fs.nodes.get(path);
        if (!node) return ERRNO.NOENT;
        if (node.dir) return ERRNO.ISDIR;
        fs.nodes.delete(path);
        return ERRNO.SUCCESS;
      },
      path_rename: (oldFd, oldPtr, oldLen, newFd, newPtr, newLen) => {
        const from = this.path(oldFd, oldPtr, oldLen);
        const to = this.path(newFd, newPtr, newLen);
        if (from === undefined || to === undefined) return ERRNO.BADF;
        if (!fs.nodes.has(from)) return ERRNO.NOENT;
        for (const [path, node] of [...fs.nodes]) {
          if (path === from || path.startsWith(from + "/")) {
            fs.nodes.delete(path);
            fs.nodes.set(to + path.slice(from.length), node);
          }
        }
        return ERRNO.SUCCESS;
      },
      // There are no symlinks.
      path_readlink: () => ERRNO.INVAL,
    };
  }
}
