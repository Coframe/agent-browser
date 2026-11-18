// End-to-end tests against a real headless Chrome.
//
// One command script runs three ways: the wasm module in Node.js, the wasm
// module in workerd (the Cloudflare Workers runtime), and the native binary
// when AGENT_BROWSER_NATIVE names one. The outputs must match.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { after, before, test } from "node:test";

import { createAgentBrowser } from "../host/index.mjs";
import { commands, nativePath, normalize, root, runNative, startChrome, startSite, wasmPath } from "./helpers.mjs";

const HOME = "/home";
const CWD = "/work";
let site;
let script;
let module;
let skills;
/** @type {{ exitCode: number, stdout: string, stderr: string }[]} */
let nodeResults;
let nodeFiles;

function byCommand(results, context) {
  return results.map((result, index) => ({
    command: script[index].join(" ").replaceAll(site.url, "{site}"),
    exitCode: result.exitCode,
    stdout: normalize(result.stdout, context),
    stderr: normalize(result.stderr, context),
  }));
}

/** Results keyed by command line. A repeated command keeps its first run. */
function firstByCommand(results) {
  const output = {};
  for (const result of byCommand(results, { site: site.url, home: HOME })) {
    output[result.command] ??= result;
  }
  return output;
}

before(async () => {
  site = await startSite();
  script = commands(site.url);
  module = new WebAssembly.Module(readFileSync(wasmPath()));
  skills = Object.fromEntries(
    ["core/SKILL.md"].map((path) => [path, readFileSync(join(root, "../skill-data", path), "utf8")]),
  );

  const chrome = await startChrome();
  try {
    const browser = createAgentBrowser({ module, skills, env: { AGENT_BROWSER_CDP: chrome.cdp, NO_COLOR: "1" } });
    nodeResults = [];
    for (const argv of script) nodeResults.push(await browser.run(argv, { cwd: CWD }));
    nodeFiles = new Map(browser.fs.drainChanges().map((path) => [path, browser.fs.readFile(path)]));
    browser.dispose();
  } finally {
    chrome.stop();
  }
});

after(() => site?.stop());

test("drives a page in Node.js", () => {
  const output = firstByCommand(nodeResults);
  const stdout = (command) => output[command].stdout.trim();

  assert.equal(output["open {site}/index.html"].exitCode, 0);
  assert.match(stdout("open {site}/index.html"), /Fixture Home/);
  assert.match(stdout("snapshot -i"), /button "Increment" \[ref=e2\]/);
  assert.equal(stdout("get text #count"), "1");
  assert.equal(stdout("get text #box"), "hovered");
  assert.equal(stdout("get title"), "Fixture Form");
  assert.equal(stdout("get text #result"), "Submitted: Ada Lovelace / pro / tos");
  assert.equal(stdout("is checked #tos"), "true");
  assert.equal(stdout(`eval innerWidth + 'x' + innerHeight`), '"390x844"');
  assert.equal(stdout("eval navigator.userAgent.includes('iPhone')"), "true");
  assert.equal(stdout("eval scrollY"), "800");
  assert.equal(stdout("cookies"), "fixture=yes");
  assert.equal(stdout("console"), "[log] fixture loaded");
  assert.match(stdout("tab list"), /\[t2\]/);
  assert.equal(stdout("wait --text Row 199"), "Row 199");
});

test("reports failures like the CLI", () => {
  const output = firstByCommand(nodeResults);

  assert.equal(output["bogus-command"].exitCode, 1);
  assert.match(output["bogus-command"].stderr, /Unknown command: bogus-command/);
  assert.equal(output["click @e999"].exitCode, 1);
  assert.match(output["click @e999"].stderr, /Unknown ref: e999/);
});

test("writes artifacts to the file system, relative to cwd", () => {
  const png = nodeFiles.get(`${CWD}/shot.png`);
  assert.deepEqual([...png.subarray(0, 8)], [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
  assert.equal(new TextDecoder().decode(nodeFiles.get(`${CWD}/page.pdf`).subarray(0, 5)), "%PDF-");
  assert.ok(JSON.parse(new TextDecoder().decode(nodeFiles.get(`${CWD}/state.json`))).cookies);
  const full = [...nodeFiles.keys()].find((path) => /screenshots\/screenshot-\d+\.png$/.test(path));
  assert.ok(full, "the default screenshot path is under the home directory");
});

test("runs a daemon per session", () => {
  const output = firstByCommand(nodeResults);

  assert.equal(output["get title --session other"].exitCode, 0);
  assert.match(output["session list"].stdout, /default/);
  assert.match(output["session list"].stdout, /other/);
});

test("serves bundled skills", async () => {
  const browser = createAgentBrowser({ module, skills });
  const result = await browser.run(["skills", "get", "core"]);
  browser.dispose();

  assert.equal(result.exitCode, 0, result.stderr);
  assert.equal(result.stdout.trim(), skills["core/SKILL.md"].trim());
});

async function startWorker() {
  const { Miniflare } = await import("miniflare");
  return new Miniflare({
    compatibilityDate: "2026-05-01",
    durableObjects: { BROWSER: "BrowserSession" },
    modules: [
      { type: "ESModule", path: "worker.mjs", contents: readFileSync(join(root, "test/worker.mjs"), "utf8") },
      { type: "ESModule", path: "host.mjs", contents: readFileSync(join(root, "host/index.mjs"), "utf8") },
      { type: "CompiledWasm", path: "agent-browser.wasm", contents: readFileSync(wasmPath()) },
      { type: "Text", path: "skills.json", contents: JSON.stringify(skills) },
    ],
  });
}

async function post(miniflare, body) {
  const response = await miniflare.dispatchFetch("http://worker/", { method: "POST", body: JSON.stringify(body) });
  assert.equal(response.status, 200, await response.clone().text());
  return response.json();
}

test("runs in workerd with the same output as in Node.js", async () => {
  const miniflare = await startWorker();
  const chrome = await startChrome();
  try {
    const { results, files } = await post(miniflare, { cdp: chrome.cdp, script, cwd: CWD });
    const context = { site: site.url, home: HOME };

    assert.deepEqual(byCommand(results, context), byCommand(nodeResults, context));
    assert.ok(files.some(([path, size]) => path === `${CWD}/shot.png` && size > 1000));
  } finally {
    chrome.stop();
    await miniflare.dispose();
  }
});

test("keeps a session in a Durable Object, one command per request", async () => {
  const miniflare = await startWorker();
  const chrome = await startChrome();
  try {
    const results = [];
    const files = [];
    for (const argv of script) {
      const response = await post(miniflare, { cdp: chrome.cdp, argv, cwd: CWD });
      results.push(response.result);
      files.push(...response.files);
    }
    const context = { site: site.url, home: HOME };

    assert.deepEqual(byCommand(results, context), byCommand(nodeResults, context));
    assert.ok(files.some(([path, size]) => path === `${CWD}/shot.png` && size > 1000));
  } finally {
    chrome.stop();
    await miniflare.dispose();
  }
});

test("matches the native binary", { skip: nativePath() ? false : "set AGENT_BROWSER_NATIVE to compare" }, async () => {
  const chrome = await startChrome();
  try {
    const native = await runNative(nativePath(), chrome.cdp, script);
    const expected = byCommand(native.results, { site: site.url, home: native.home });
    const actual = byCommand(nodeResults, { site: site.url, home: HOME })
      // The wasm run used cwd /work inside its own file system.
      .map((result) => ({ ...result, stdout: result.stdout.replaceAll(CWD, "{home}") }));

    assert.deepEqual(actual, expected);
  } finally {
    chrome.stop();
  }
});
