// Builds dist/package and packs it as @coframe/agent-browser-wasm.
//
//   node scripts/package.mjs [--build <n>]
//
// The version is upstream's with a `-wasm.<n>` suffix, so a consumer can
// tell which agent-browser release it runs.

import { execFileSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("..", import.meta.url));
const repo = join(root, "..");
const wasm = join(root, "target/wasm32-wasip1/release/agent-browser.wasm");
const out = join(root, "dist/package");

const buildFlag = process.argv.indexOf("--build");
const build = buildFlag === -1 ? "0" : process.argv[buildFlag + 1];
const upstream = readFileSync(join(repo, "cli/Cargo.toml"), "utf8").match(/^version = "([^"]+)"/m)[1];
const commit = execFileSync("git", ["rev-parse", "HEAD"], { cwd: repo, encoding: "utf8" }).trim();

if (!existsSync(wasm)) {
  console.error("Build first: cargo build --release --target wasm32-wasip1");
  process.exit(1);
}

function files(dir) {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    return statSync(path).isDirectory() ? files(path) : [path];
  });
}

const skillData = join(repo, "skill-data");
const skills = Object.fromEntries(
  files(skillData).sort().map((path) => [relative(skillData, path).replaceAll("\\", "/"), readFileSync(path, "utf8")]),
);

rmSync(out, { recursive: true, force: true });
mkdirSync(out, { recursive: true });
cpSync(wasm, join(out, "agent-browser.wasm"));
cpSync(join(root, "host/index.mjs"), join(out, "index.mjs"));
cpSync(join(root, "host/index.d.ts"), join(out, "index.d.ts"));
cpSync(join(repo, "LICENSE"), join(out, "LICENSE"));
writeFileSync(join(out, "skills.json"), JSON.stringify(skills));
writeFileSync(join(out, "package.json"), JSON.stringify({
  name: "@coframe/agent-browser-wasm",
  version: `${upstream}-wasm.${build}`,
  description: "agent-browser built for wasm32-wasip1 hosts such as Cloudflare Workers",
  license: "Apache-2.0",
  repository: "github:Coframe/agent-browser",
  type: "module",
  main: "index.mjs",
  types: "index.d.ts",
  exports: {
    ".": { types: "./index.d.ts", default: "./index.mjs" },
    "./agent-browser.wasm": "./agent-browser.wasm",
    "./skills.json": "./skills.json",
  },
  agentBrowser: { version: upstream, commit },
}, null, 2) + "\n");

execFileSync("pnpm", ["pack", "--pack-destination", join(root, "dist")], { cwd: out, stdio: "inherit" });
