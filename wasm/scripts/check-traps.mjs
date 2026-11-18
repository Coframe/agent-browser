// Fails when the module still links a std function that traps on WASI.
//
// `std::env::temp_dir` and `std::process::id` panic on wasm32-wasip1, and a
// panic aborts the instance. build.rs rewrites every use in upstream
// sources; this catches a spelling the rewrite missed, or a dependency that
// calls one. It needs the `guard` profile: the release build with symbol
// names kept. A build without link-time optimization keeps both functions
// even when nothing calls them, so it cannot be checked this way.

import { readFileSync } from "node:fs";

const TRAPS = {
  "3std3env8temp_dir": "std::env::temp_dir",
  "3std7process2id": "std::process::id",
};

const path = process.argv[2] ?? "target/wasm32-wasip1/guard/agent-browser.wasm";
const module = readFileSync(path);
if (!module.includes("__rust_alloc") && !module.includes("rust_begin_unwind")) {
  console.error(`${path} has no symbol names. Build with --profile guard.`);
  process.exit(2);
}
const found = Object.entries(TRAPS).filter(([symbol]) => module.includes(symbol));
for (const [, name] of found) {
  console.error(`${path} links ${name}, which traps on WASI. Add a rewrite to EDITS in build.rs.`);
}
if (found.length > 0) process.exit(1);
console.log(`${path}: no std function that traps on WASI is linked.`);
