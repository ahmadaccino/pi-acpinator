#!/usr/bin/env node
"use strict";
// pi-acpinator performance benchmarks. Deterministic (scripted fake pi, no
// model/network) so numbers are reproducible and CI-comparable.
//   node scripts/bench.mjs [path-to-binary]
import { spawn, execSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const BIN = process.argv[2] || join(here, "..", "target", "release", "pi-acpinator");
const FAKE = join(here, "fake-pi.mjs");
const RUNS = 9;
const DELTAS = 20000;

const median = (xs) => xs.slice().sort((a, b) => a - b)[Math.floor(xs.length / 2)];
const rss = (pid) => {
  try { return parseInt(execSync(`ps -o rss= -p ${pid}`).toString().trim(), 10) * 1024; } catch { return 0; }
};

function agent(extraEnv = {}) {
  return spawn(BIN, [], {
    stdio: ["pipe", "pipe", "ignore"],
    env: { ...process.env, RUST_LOG: "off", PI_ACPINATOR_APPROVAL: "off", PI_ACPINATOR_PI_BIN: FAKE, ...extraEnv },
  });
}

function lines(child, onMsg) {
  let buf = "";
  child.stdout.on("data", (d) => {
    buf += d;
    let i;
    while ((i = buf.indexOf("\n")) >= 0) {
      const l = buf.slice(0, i); buf = buf.slice(i + 1);
      if (!l.trim()) continue;
      try { onMsg(JSON.parse(l)); } catch {}
    }
  });
}
const send = (c, o) => c.stdin.write(JSON.stringify(o) + "\n");

function coldStart() {
  return new Promise((res) => {
    const t0 = process.hrtime.bigint();
    const c = agent();
    lines(c, (m) => { if (m.id === 1) { const ms = Number(process.hrtime.bigint() - t0) / 1e6; c.kill(); res(ms); } });
    send(c, { jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: 1 } });
  });
}

function sessionNew() {
  return new Promise((res) => {
    const c = agent();
    let t0;
    lines(c, (m) => {
      if (m.id === 1) { t0 = process.hrtime.bigint(); send(c, { jsonrpc: "2.0", id: 2, method: "session/new", params: { cwd: "/tmp", mcpServers: [], additionalDirectories: [] } }); }
      else if (m.id === 2) { const ms = Number(process.hrtime.bigint() - t0) / 1e6; c.kill(); res(ms); }
    });
    send(c, { jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: 1 } });
  });
}

// One heavy streaming turn: measures TTFT, turn wall time, coalescing ratio,
// chunk throughput, and peak RSS.
function streamTurn() {
  return new Promise((res) => {
    const c = agent({ PI_FAKE_BENCH_DELTAS: String(DELTAS) });
    let sid = null, tStart = 0, tFirst = 0, chunks = 0, peak = 0;
    const sampler = setInterval(() => { const r = rss(c.pid); if (r > peak) peak = r; }, 5);
    lines(c, (m) => {
      if (m.id === 1) send(c, { jsonrpc: "2.0", id: 2, method: "session/new", params: { cwd: "/tmp", mcpServers: [], additionalDirectories: [] } });
      else if (m.id === 2) { sid = m.result.sessionId; tStart = process.hrtime.bigint(); send(c, { jsonrpc: "2.0", id: 3, method: "session/prompt", params: { sessionId: sid, prompt: [{ type: "text", text: "go" }] } }); }
      else if (m.method === "session/update" && m.params?.update?.sessionUpdate === "agent_message_chunk") { if (!tFirst) tFirst = process.hrtime.bigint(); chunks++; }
      else if (m.id === 3 && m.result) {
        clearInterval(sampler);
        const wall = Number(process.hrtime.bigint() - tStart) / 1e6;
        const ttft = Number(tFirst - tStart) / 1e6;
        c.kill();
        res({ wall, ttft, chunks, peak });
      }
    });
    send(c, { jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: 1 } });
  });
}

const starts = [];
for (let i = 0; i < RUNS; i++) starts.push(await coldStart());
const news = [];
for (let i = 0; i < RUNS; i++) news.push(await sessionNew());
const stream = await streamTurn();

const binSize = (() => { try { return execSync(`wc -c < ${BIN}`).toString().trim(); } catch { return "?"; } })();

console.log(`binary:              ${BIN} (${(binSize / 1024 / 1024).toFixed(2)} MiB)`);
console.log(`cold start:          median ${median(starts).toFixed(2)} ms  (min ${Math.min(...starts).toFixed(2)})`);
console.log(`session/new setup:   median ${median(news).toFixed(2)} ms  (spawn pi + handshake)`);
console.log(`stream turn (${DELTAS} deltas):`);
console.log(`  TTFT:              ${stream.ttft.toFixed(2)} ms`);
console.log(`  wall:              ${stream.wall.toFixed(2)} ms  (${(DELTAS / (stream.wall / 1000) / 1000).toFixed(0)}k deltas/s)`);
console.log(`  ACP chunks:        ${stream.chunks}  (coalescing ${(DELTAS / stream.chunks).toFixed(1)}x fewer frames)`);
console.log(`  peak RSS:          ${(stream.peak / 1024 / 1024).toFixed(1)} MiB`);
process.exit(0);
