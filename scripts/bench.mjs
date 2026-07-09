#!/usr/bin/env node
// Measures pi-acpinator cold-start latency and resident memory.
// Usage: node scripts/bench.mjs [path-to-binary]
import { spawn, execSync } from "node:child_process";

const BIN = process.argv[2] || "target/release/pi-acpinator";
const RUNS = 7;

function rss(pid) {
  try {
    return parseInt(execSync(`ps -o rss= -p ${pid}`).toString().trim(), 10) * 1024;
  } catch {
    return null;
  }
}

function coldStart() {
  return new Promise((resolve) => {
    const t0 = process.hrtime.bigint();
    const child = spawn(BIN, [], { stdio: ["pipe", "pipe", "ignore"], env: { ...process.env, RUST_LOG: "off" } });
    let buf = "";
    child.stdout.on("data", (d) => {
      buf += d;
      const i = buf.indexOf("\n");
      if (i < 0) return;
      const ms = Number(process.hrtime.bigint() - t0) / 1e6;
      child.kill();
      resolve(ms);
    });
    child.stdin.write(JSON.stringify({ jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: 1 } }) + "\n");
  });
}

function idleRss() {
  return new Promise((resolve) => {
    const child = spawn(BIN, [], { stdio: ["pipe", "pipe", "ignore"], env: { ...process.env, RUST_LOG: "off", PI_ACPINATOR_APPROVAL: "off" } });
    let buf = "";
    child.stdout.on("data", (d) => {
      buf += d;
      let i;
      while ((i = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, i);
        buf = buf.slice(i + 1);
        let m;
        try { m = JSON.parse(line); } catch { continue; }
        if (m.id === 1) child.stdin.write(JSON.stringify({ jsonrpc: "2.0", id: 2, method: "session/new", params: { cwd: "/tmp", mcpServers: [], additionalDirectories: [] } }) + "\n");
        else if (m.id === 2) setTimeout(() => { const r = rss(child.pid); child.kill(); resolve(r); }, 600);
      }
    });
    child.stdin.write(JSON.stringify({ jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: 1 } }) + "\n");
  });
}

const median = (xs) => xs.slice().sort((a, b) => a - b)[Math.floor(xs.length / 2)];

const starts = [];
for (let i = 0; i < RUNS; i++) starts.push(await coldStart());
const bridgeRss = await idleRss();

console.log(`binary:        ${BIN}`);
console.log(`cold start:    median ${median(starts).toFixed(2)} ms  (min ${Math.min(...starts).toFixed(2)}, max ${Math.max(...starts).toFixed(2)})`);
console.log(`idle bridge RSS (with one pi session): ${(bridgeRss / 1024 / 1024).toFixed(1)} MiB`);
console.log(`note: the pi child runs as a separate process with its own RSS.`);
process.exit(0);
