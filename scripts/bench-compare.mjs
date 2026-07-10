#!/usr/bin/env node
"use strict";
// Head-to-head benchmark vs other pi ACP adapters. Measures each adapter's own
// footprint at the ACP `initialize` stage (before any pi session is spawned),
// which isolates the adapter's overhead — the thing that actually differs.
//
// Reproducible: installs the competitor(s) into a temp dir, then benchmarks
// them against the local release binary. Run:  node scripts/bench-compare.mjs
import { spawn, execSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";

const here = dirname(fileURLToPath(import.meta.url));
const BIN = process.argv[2] || join(here, "..", "target", "release", "pi-acpinator");
const RUNS = 9;
const median = (xs) => xs.slice().sort((a, b) => a - b)[Math.floor(xs.length / 2)];

// Install competitors into a throwaway dir so the comparison is reproducible.
const work = mkdtempSync(join(tmpdir(), "pi-acp-bench-"));
writeFileSync(join(work, "package.json"), JSON.stringify({ name: "b", private: true, version: "1.0.0" }));
console.error("installing competitors (pi-acp)…");
execSync("npm i pi-acp@latest", { cwd: work, stdio: "ignore" });

const targets = [
  { name: "pi-acpinator (Rust, spawns pi)", cmd: BIN, args: [] },
  { name: "pi-acp / svkozak (Node, spawns pi)", cmd: "node", args: [join(work, "node_modules/pi-acp/dist/index.js")] },
  // @victor-software-house/pi-acp and @harms-haus/pi-acp embed the pi SDK
  // in-process (the whole Node pi runtime lives inside the adapter); victor also
  // requires a background daemon and does not run standalone.
];

function rssTreeBytes(root) {
  const rows = execSync("ps -Ao pid=,ppid=,rss=").toString().trim().split("\n").map((l) => l.trim().split(/\s+/).map(Number));
  const kids = {}, rss = {};
  for (const [pid, ppid, r] of rows) { (kids[ppid] ||= []).push(pid); rss[pid] = r; }
  let total = 0; const stack = [root];
  while (stack.length) { const p = stack.pop(); total += rss[p] || 0; for (const c of kids[p] || []) stack.push(c); }
  return total * 1024;
}

function once(cmd, args) {
  return new Promise((res) => {
    const t0 = process.hrtime.bigint();
    const child = spawn(cmd, args, { stdio: ["pipe", "pipe", "ignore"], env: { ...process.env, RUST_LOG: "off" } });
    let buf = "", done = false;
    const finish = (v) => { if (!done) { done = true; try { child.kill("SIGKILL"); } catch {} res(v); } };
    child.on("error", () => finish(null));
    child.stdout.on("data", (d) => {
      buf += d; let i;
      while ((i = buf.indexOf("\n")) >= 0) {
        const l = buf.slice(0, i); buf = buf.slice(i + 1);
        let m; try { m = JSON.parse(l); } catch { continue; }
        if (m.id === 1 && (m.result || m.error)) finish({ ms: Number(process.hrtime.bigint() - t0) / 1e6, rss: rssTreeBytes(child.pid) });
      }
    });
    child.stdin.write(JSON.stringify({ jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: 1, clientCapabilities: {} } }) + "\n");
    setTimeout(() => finish(null), 15000);
  });
}

for (const t of targets) {
  const starts = [], mems = [];
  for (let i = 0; i < RUNS; i++) { const r = await once(t.cmd, t.args); if (r) { starts.push(r.ms); mems.push(r.rss); } await new Promise((r) => setTimeout(r, 150)); }
  if (!starts.length) { console.log(`${t.name}: did not respond to initialize`); continue; }
  console.log(`${t.name}\n  cold start: ${median(starts).toFixed(1)} ms   RSS: ${(median(mems) / 1024 / 1024).toFixed(1)} MiB`);
}

// @victor-software-house/pi-acp: Bun runtime + persistent daemon that embeds the
// pi SDK. Measured separately: idle daemon RSS + thin-client cold start (warm daemon).
let bun = null;
try { bun = execSync("command -v bun").toString().trim(); } catch {}
if (bun) {
  try {
    console.error("installing @victor-software-house/pi-acp\u2026");
    execSync("npm i @victor-software-house/pi-acp@latest", { cwd: work, stdio: "ignore" });
    const entry = join(work, "node_modules/@victor-software-house/pi-acp/dist/index.mjs");
    execSync("pkill -f 'index.mjs --daemon' || true", { stdio: "ignore" });
    const daemon = spawn(bun, [entry, "--daemon"], { cwd: work, stdio: "ignore", detached: true });
    await new Promise((r) => setTimeout(r, 8000));
    const dpid = Number(execSync("pgrep -f 'index.mjs --daemon' | head -1").toString().trim());
    const idle = rssTreeBytes(dpid);
    const cs = [];
    for (let i = 0; i < 5; i++) {
      const r = await once(bun, [entry]);
      if (r) cs.push(r.ms);
      await new Promise((r) => setTimeout(r, 250));
    }
    execSync("pkill -f 'index.mjs --daemon' || true", { stdio: "ignore" });
    try { daemon.kill("SIGKILL"); } catch {}
    console.log(`@victor-software-house/pi-acp (Bun daemon, embeds SDK)\n  cold start (warm daemon): ${cs.length ? median(cs).toFixed(1) : "n/a"} ms   daemon RSS (idle): ${(idle / 1024 / 1024).toFixed(1)} MiB`);
  } catch (e) {
    console.log(`@victor-software-house/pi-acp: measurement failed (${e.message})`);
  }
} else {
  console.log("@victor-software-house/pi-acp: skipped (needs the Bun runtime)");
}
process.exit(0);
