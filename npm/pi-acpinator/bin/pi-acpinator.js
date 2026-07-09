#!/usr/bin/env node
"use strict";
// Resolves the prebuilt binary for this platform (shipped as an optional
// @ahmadsandid/pi-acpinator-<platform>-<arch> package) and execs it, forwarding
// stdio so the ACP JSON-RPC stream passes through untouched.
const { spawnSync } = require("node:child_process");

const platform = process.platform;
const arch = process.arch;
const pkg = `@ahmadsandid/pi-acpinator-${platform}-${arch}`;
const binName = platform === "win32" ? "pi-acpinator.exe" : "pi-acpinator";

let binPath;
try {
  binPath = require.resolve(`${pkg}/bin/${binName}`);
} catch {
  console.error(`pi-acpinator: no prebuilt binary for ${platform}-${arch} (missing ${pkg}).`);
  console.error(`Build from source instead: cargo install pi-acpinator`);
  process.exit(1);
}

const result = spawnSync(binPath, process.argv.slice(2), { stdio: "inherit" });
if (result.error) {
  console.error(result.error);
  process.exit(1);
}
process.exit(result.status ?? 0);
