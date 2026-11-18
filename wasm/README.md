# agent-browser for WebAssembly hosts

This directory builds agent-browser for `wasm32-wasip1` so it runs where there is no operating system to run the native binary, such as a Cloudflare Worker or Durable Object. The browser stays remote: the module connects to it over the Chrome DevTools Protocol.

It is the upstream CLI and daemon, compiled from the sources in `../cli`. Nothing in `../cli` is modified, so merging upstream cannot conflict with this directory.

## Use

```js
import module from "@coframe/agent-browser-wasm/agent-browser.wasm";
import skills from "@coframe/agent-browser-wasm/skills.json";
import { createAgentBrowser } from "@coframe/agent-browser-wasm";

const browser = createAgentBrowser({
  module,
  skills,
  env: { AGENT_BROWSER_CDP: "wss://browser.example/devtools/browser/..." },
});

const { exitCode, stdout, stderr } = await browser.run(["snapshot", "-i"]);
```

`run` takes the arguments of one `agent-browser` invocation and returns what the CLI printed. Files the module writes, such as screenshots, land in `browser.fs`, an in-memory file system. `browser.fs.drainChanges()` lists what was written since the last call.

The engine must support JavaScript Promise Integration (`WebAssembly.Suspending`). workerd and Node.js 24 do.

## How it works

<table>
<tr><th>Piece</th><th>What it does</th></tr>
<tr><td><code>build.rs</code></td><td>Copies <code>../cli/src</code> into the build directory and applies the edits in its <code>EDITS</code> table. The copy is the crate root.</td></tr>
<tr><td><code>shims/</code></td><td>Crates that stand in for <code>tokio</code>, <code>tokio-tungstenite</code>, <code>reqwest</code>, <code>rustls</code>, <code>rustls-native-certs</code> and <code>socket2</code> under the same names, built on <code>shims/host</code>.</td></tr>
<tr><td><code>shims/host</code></td><td>The executor, timers, WebSockets, HTTP, loopback connections and processes, as imports that the JavaScript host supplies.</td></tr>
<tr><td><code>host/index.mjs</code></td><td>The JavaScript host: WASI preview 1 over an in-memory file system, and the imports above.</td></tr>
</table>

The process model is upstream's. Each invocation is a short-lived instance of the module. The daemon is a long-lived instance that the first invocation of a session starts, and the two talk over a loopback connection that the host carries. Commands that need a local operating system do not work. Without a CDP endpoint, `open` exits 1 with the CLI's "Chrome not found" error. `install` panics in upstream code for an unsupported platform, which aborts the instance: `run` returns exit code 134 with the panic message on stderr. Other native-only commands have not been surveyed.

## Build and test

```bash
rustup target add wasm32-wasip1
cd wasm
pnpm install
cargo build --release --target wasm32-wasip1
pnpm test
```

The tests need Chrome. Set `CHROME_PATH` when it is not in a standard location. They run one command script in Node.js and in workerd, and compare both with the native binary when `AGENT_BROWSER_NATIVE` names one:

```bash
(cd ../cli && cargo build)
AGENT_BROWSER_NATIVE=$PWD/../cli/target/debug/agent-browser pnpm test
```

`pnpm run package` writes the npm tarball to `dist/`.

`cargo build --profile fast --target wasm32-wasip1` builds in about a minute for local iteration. Do not ship that build.

## Sync with upstream

1. Merge `upstream/main`. This directory is the only difference from upstream, so the merge is clean.
2. Build. The build stops with a message when upstream changed something this directory depends on:
   - An edit in `build.rs` no longer matches. The message names the file and the text. Update the entry in `EDITS`.
   - `cli/Cargo.toml` gained a dependency. Add it to `Cargo.toml`, or add a shim under `shims/` when the crate needs an operating system.
   - Upstream calls a function that a shim does not have yet. The compiler names it. Add it to the shim.
3. Run the trap check and the tests, with the native comparison:

   ```bash
   cargo build --profile guard --target wasm32-wasip1
   pnpm run check:traps
   ```

4. Review new `#[cfg(windows)]` code in `../cli/src`. A function with one `cfg(unix)` body and one `cfg(windows)` body compiles to an empty function here, which fails at run time, not at build time. `EDITS` widens the Windows arm to every non-Unix target where the arm only needs loopback TCP and files.

## What the edits are

`EDITS` in `build.rs` is the full list. There are three kinds:

- Calls that the Rust standard library does not support on WASI (`std::env::temp_dir`, `std::process::id`, `std::env::home_dir`, `File::lock`, `current_exe`) go to `shims/host`. `scripts/check-traps.mjs` checks the `guard` build for the two that abort the instance.
- The daemon endpoint. Windows reaches the daemon over loopback TCP and a `.port` file. Those arms are widened from `cfg(windows)` to `cfg(not(unix))`.
- Functions with a Unix arm and a Windows arm get a third arm, for example starting the daemon as a second instance.
