#!/usr/bin/env node
"use strict";
// Deterministic component test: drives the real pi-acpinator binary over ACP
// while pointing it at a scripted fake pi (no model, no network).
//   node scripts/component-test.mjs [path-to-binary]
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const BIN = process.argv[2] || join(here, "..", "target", "debug", "pi-acpinator");
const FAKE = join(here, "fake-pi.mjs");

const child = spawn(BIN, [], {
  stdio: ["pipe", "pipe", "inherit"],
  env: { ...process.env, RUST_LOG: "error", PI_ACPINATOR_APPROVAL: "off", PI_ACPINATOR_PI_BIN: FAKE },
});

let buf = "";
const send = (o) => child.stdin.write(JSON.stringify(o) + "\n");
const results = [];
const check = (name, cond) => {
  results.push([name, cond]);
  console.log(`${cond ? "ok  " : "FAIL"} ${name}`);
};

let sid = null;
let text = "";
let thoughts = "";
const tools = [];
let histText = "";
let loadDone = false;

function finish() {
  const failed = results.filter(([, c]) => !c);
  console.log(`\n${results.length - failed.length}/${results.length} checks passed`);
  child.kill();
  process.exit(failed.length ? 1 : 0);
}

child.stdout.on("data", (d) => {
  buf += d;
  let i;
  while ((i = buf.indexOf("\n")) >= 0) {
    const line = buf.slice(0, i);
    buf = buf.slice(i + 1);
    if (!line.trim()) continue;
    let m;
    try { m = JSON.parse(line); } catch { continue; }

    if (m.id === 1 && m.result) {
      check("initialize advertises loadSession", m.result.agentCapabilities?.loadSession === true);
      check("initialize advertises auth method", (m.result.authMethods || []).length > 0);
      send({ jsonrpc: "2.0", id: 2, method: "session/new", params: { cwd: "/tmp", mcpServers: [], additionalDirectories: [] } });
    } else if (m.id === 2 && m.result) {
      sid = m.result.sessionId;
      check("session/new returns 6 thinking modes", m.result.modes?.availableModes?.length === 6);
      const co = m.result.configOptions?.[0];
      const opts = co?.select?.options || co?.options || [];
      check("session/new returns model config option", co?.id === "model" && opts.length === 2);
      send({ jsonrpc: "2.0", id: 3, method: "session/prompt", params: { sessionId: sid, prompt: [{ type: "text", text: "go" }] } });
    } else if (m.method === "session/update" && m.params?.sessionId === sid) {
      const u = m.params.update;
      const k = u.sessionUpdate;
      if (loadDone) {
        if (k === "user_message_chunk") histText += "U";
        else if (k === "agent_message_chunk") histText += "A";
        else if (k === "tool_call") histText += "T";
      } else if (k === "agent_message_chunk" && u.content?.text) text += u.content.text;
      else if (k === "agent_thought_chunk" && u.content?.text) thoughts += u.content.text;
      else if (k === "tool_call") { tools.push(["call", u.kind, u.title]); if ((u.content || []).some((c) => c.type === "diff")) tools.push(["diff", u.content.find((c) => c.type === "diff").newText]); }
      else if (k === "tool_call_update") tools.push(["update", u.status]);
    } else if (m.id === 3 && m.result) {
      check("prompt streamed coalesced text", text === "Hello world");
      check("prompt streamed thoughts", thoughts === "pondering");
      check("tool_call emitted as execute", tools.some((t) => t[0] === "call" && t[1] === "execute"));
      check("tool_call completed", tools.some((t) => t[0] === "update" && t[1] === "completed"));
      check("edit tool surfaces a diff", tools.some((t) => t[0] === "diff" && t[1] === "bar"));
      check("prompt stopReason end_turn", m.result.stopReason === "end_turn");
      send({ jsonrpc: "2.0", id: 4, method: "session/set_mode", params: { sessionId: sid, modeId: "high" } });
    } else if (m.id === 4) {
      check("session/set_mode ok", !!m.result);
      const cur = "prov/m1";
      send({ jsonrpc: "2.0", id: 5, method: "session/set_config_option", params: { sessionId: sid, configId: "model", value: cur } });
    } else if (m.id === 5) {
      check("session/set_config_option ok", !!m.result && !m.error);
      send({ jsonrpc: "2.0", id: 6, method: "session/set_config_option", params: { sessionId: sid, configId: "model", value: "prov/bad" } });
    } else if (m.id === 6) {
      check("set_config_option rejects bad model with error", !!m.error);
      loadDone = true;
      send({ jsonrpc: "2.0", id: 7, method: "session/load", params: { sessionId: sid, cwd: "/tmp", mcpServers: [] } });
    } else if (m.id === 7 && m.result) {
      check("session/load replays history (user+assistant+tool)", histText.includes("U") && histText.includes("A") && histText.includes("T"));
      finish();
    } else if ((m.id === 3 || m.id === 7) && m.error) {
      check(`request ${m.id} no error`, false);
      finish();
    }
  }
});

setTimeout(() => { check("completed before timeout", false); finish(); }, 20000);
send({ jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: 1 } });
