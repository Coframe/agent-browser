import { spawn } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { createServer } from "node:http";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

export const root = fileURLToPath(new URL("..", import.meta.url));

/** The module under test: `AGENT_BROWSER_WASM`, else the newest local build. */
export function wasmPath() {
  const candidates = [
    process.env.AGENT_BROWSER_WASM,
    join(root, "target/wasm32-wasip1/release/agent-browser.wasm"),
    join(root, "target/wasm32-wasip1/fast/agent-browser.wasm"),
  ].filter((path) => path && existsSync(path));
  if (candidates.length === 0) throw new Error("Build first: cargo build --release --target wasm32-wasip1");
  return candidates[0];
}

/** The native binary to compare with, when `AGENT_BROWSER_NATIVE` names one. */
export function nativePath() {
  return process.env.AGENT_BROWSER_NATIVE;
}

function chromePath() {
  const cache = join(homedir(), ".cache/ms-playwright");
  const playwright = existsSync(cache)
    ? readdirSync(cache).filter((name) => /^chromium-\d+$/.test(name)).sort().reverse()
        .map((name) => join(cache, name, "chrome-linux64/chrome"))
    : [];
  const candidates = [
    process.env.CHROME_PATH,
    "/usr/bin/google-chrome",
    "/usr/bin/chromium",
    "/usr/bin/chromium-browser",
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    ...playwright,
  ];
  const found = candidates.find((path) => path && existsSync(path));
  if (!found) throw new Error("Chrome not found. Set CHROME_PATH.");
  return found;
}

/** Starts headless Chrome and returns its CDP WebSocket URL. */
export async function startChrome() {
  const profile = mkdtempSync(join(tmpdir(), "agent-browser-wasm-chrome-"));
  const chrome = spawn(chromePath(), [
    "--headless=new", "--no-sandbox", "--remote-debugging-port=0",
    `--user-data-dir=${profile}`, "--window-size=1280,720", "about:blank",
  ], { stdio: ["ignore", "ignore", "pipe"] });
  const cdp = await new Promise((resolve, reject) => {
    let stderr = "";
    chrome.stderr.on("data", (chunk) => {
      stderr += chunk;
      const match = stderr.match(/DevTools listening on (ws:\/\/\S+)/);
      if (match) resolve(match[1]);
    });
    chrome.on("exit", () => reject(new Error(`Chrome exited: ${stderr}`)));
  });
  return {
    cdp,
    stop() {
      chrome.kill();
      rmSync(profile, { recursive: true, force: true, maxRetries: 5 });
    },
  };
}

/** Serves the fixture pages on a loopback port. */
export async function startSite() {
  const rows = Array.from({ length: 200 }, (_, i) => `<p id="row${i}">Row ${i}</p>`).join("");
  const long = `<!doctype html><html><head><meta charset="utf-8"><title>Fixture Long</title></head>`
    + `<body><h1>Long</h1>${rows}<button id="bottom">Bottom button</button></body></html>`;
  const server = createServer((request, response) => {
    const name = request.url.split("?")[0];
    const file = join(root, "test/site", name);
    if (name === "/long.html") response.end(long);
    else if (existsSync(file)) response.end(readFileSync(file));
    else {
      response.statusCode = 404;
      response.end("not found");
    }
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  return { url: `http://127.0.0.1:${server.address().port}`, stop: () => server.close() };
}

/** The command script as argv arrays. */
export function commands(site) {
  return readFileSync(join(root, "test/commands.txt"), "utf8")
    .split("\n")
    .filter((line) => line.trim() && !line.startsWith("#"))
    .map((line) => line.replaceAll("{site}", site))
    .map((line) => line.match(/"[^"]*"|\S+/g).map((arg) => arg.replace(/^"|"$/g, "")));
}

function run(binary, argv, options) {
  return new Promise((resolve) => {
    const child = spawn(binary, argv, { ...options, stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => (stdout += chunk));
    child.stderr.on("data", (chunk) => (stderr += chunk));
    child.on("exit", (exitCode) => resolve({ exitCode, stdout, stderr }));
  });
}

/**
 * Runs the script through the native binary in a scratch home directory.
 * It must not block the event loop: the fixture site is served by this
 * process.
 */
export async function runNative(binary, cdp, script) {
  const home = mkdtempSync(join(tmpdir(), "agent-browser-wasm-native-"));
  const env = {
    PATH: process.env.PATH, HOME: home, TMPDIR: home, NO_COLOR: "1",
    AGENT_BROWSER_CDP: cdp, AGENT_BROWSER_SOCKET_DIR: join(home, ".agent-browser"),
  };
  const results = [];
  for (const argv of script) results.push(await run(binary, argv, { env, cwd: home }));
  await run(binary, ["close", "--all"], { env, cwd: home });
  rmSync(home, { recursive: true, force: true, maxRetries: 5 });
  return { results, home };
}

/** Removes what legitimately differs between two runs of the script. */
export function normalize(text, { site, home }) {
  return text
    .replaceAll(site, "{site}")
    .replaceAll(home, "{home}")
    .replace(/127\.0\.0\.1:\d+/g, "{host}")
    .replace(/screenshot-\d+\.png/g, "screenshot-N.png")
    .replace(/"launchHash":\d+/g, '"launchHash":N');
}
