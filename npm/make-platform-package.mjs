#!/usr/bin/env node
"use strict";
// Builds an @ahmadsandid/pi-acpinator-<platform>-<arch> npm package around a
// prebuilt binary. Run in CI once per target.
//   node npm/make-platform-package.mjs <platform>-<arch> <binary-path> <version>
// e.g. node npm/make-platform-package.mjs darwin-arm64 target/aarch64-apple-darwin/release/pi-acpinator 0.1.0
import { mkdirSync, copyFileSync, writeFileSync, chmodSync } from "node:fs";
import { join } from "node:path";

const [slug, binaryPath, version] = process.argv.slice(2);
if (!slug || !binaryPath || !version) {
  console.error("usage: make-platform-package.mjs <platform>-<arch> <binary-path> <version>");
  process.exit(1);
}

const [platform, arch] = slug.split("-");
const binName = platform === "win32" ? "pi-acpinator.exe" : "pi-acpinator";
const outDir = join("npm", "platform", slug);
const binDir = join(outDir, "bin");
mkdirSync(binDir, { recursive: true });

copyFileSync(binaryPath, join(binDir, binName));
if (platform !== "win32") chmodSync(join(binDir, binName), 0o755);

writeFileSync(
  join(outDir, "package.json"),
  JSON.stringify(
    {
      name: `@ahmadsandid/pi-acpinator-${slug}`,
      version,
      description: `pi-acpinator prebuilt binary for ${platform} ${arch}.`,
      license: "MIT",
      os: [platform],
      cpu: [arch],
      files: ["bin"],
    },
    null,
    2,
  ) + "\n",
);

console.log(`wrote ${outDir}`);
