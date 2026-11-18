/** Files shared by every instance of the module. Paths are absolute. */
export class MemoryFileSystem {
  writeFile(path: string, data: Uint8Array | string): void;
  readFile(path: string): Uint8Array | undefined;
  exists(path: string): boolean;
  remove(path: string): void;
  mkdirp(path: string): void;
  /** Names directly inside a directory. */
  list(path: string): string[];
  /** Paths of every file. */
  files(): string[];
  /**
   * Files the module wrote since the last call, for copying results such as
   * screenshots into the embedder's own storage.
   */
  drainChanges(): string[];
}

export interface AgentBrowserOptions {
  /** `agent-browser.wasm`, compiled. */
  module: WebAssembly.Module;
  /** Environment of every invocation, for example `AGENT_BROWSER_CDP`. */
  env?: Record<string, string>;
  fs?: MemoryFileSystem;
  /** The `skills.json` shipped with the module. Makes `agent-browser skills` work. */
  skills?: Record<string, string>;
  /** Opens the CDP connection. Defaults to `new WebSocket(url)`. */
  webSocket?: (url: string, headers: [string, string][]) => WebSocket | Promise<WebSocket>;
  fetch?: typeof fetch;
}

export interface RunOptions {
  env?: Record<string, string>;
  /** Working directory for relative paths. Defaults to `/`. */
  cwd?: string;
  stdin?: string;
}

export interface RunResult {
  exitCode: number;
  stdout: string;
  stderr: string;
}

export interface AgentBrowser {
  fs: MemoryFileSystem;
  /** Runs `agent-browser <argv>`. */
  run(argv: string[], options?: RunOptions): Promise<RunResult>;
  /** Output of the daemons, for debugging. */
  daemonLogs(): string[];
  /** Drops every instance, timer and socket. */
  dispose(): void;
}

/** Needs JavaScript Promise Integration: workerd, or Node.js 24 and later. */
export function createAgentBrowser(options: AgentBrowserOptions): AgentBrowser;
