#!/usr/bin/env node

// Deterministic component test: drives the real pi-acpinator binary over ACP
// while pointing it at a scripted fake pi (no model, no network).
//   node scripts/component-test.mjs [path-to-binary]
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const BIN =
	process.argv[2] || join(here, "..", "target", "debug", "pi-acpinator");
const FAKE = join(here, "fake-pi.mjs");

const child = spawn(BIN, [], {
	stdio: ["pipe", "pipe", "inherit"],
	env: {
		...process.env,
		RUST_LOG: "error",
		PI_ACPINATOR_APPROVAL: "off",
		PI_ACPINATOR_PI_BIN: FAKE,
		PI_ACPINATOR_PERMISSION_TIMEOUT_MS: "30",
	},
});

let buf = "";
const send = (o) => child.stdin.write(JSON.stringify(o) + "\n");
const prompt = (id, text) =>
	send({
		jsonrpc: "2.0",
		id,
		method: "session/prompt",
		params: { sessionId: sid, prompt: [{ type: "text", text }] },
	});
const results = [];
const check = (name, cond) => {
	results.push([name, cond]);
	console.log(`${cond ? "ok  " : "FAIL"} ${name}`);
};

let sid = null;
let stage = "normal";
let text = "";
let thoughts = "";
const tools = [];
let histText = "";
let concurrentText = "";
let permissionText = "";
const concurrentDone = new Set();

function finish() {
	const failed = results.filter(([, c]) => !c);
	console.log(
		`\n${results.length - failed.length}/${results.length} checks passed`,
	);
	child.kill();
	process.exit(failed.length ? 1 : 0);
}

function collectUpdate(update) {
	const k = update.sessionUpdate;
	if (stage === "load" || stage === "cleanup-load") {
		if (k === "user_message_chunk") histText += "U";
		else if (k === "agent_message_chunk") histText += "A";
		else if (k === "tool_call") histText += "T";
		return;
	}
	if (stage === "concurrent") {
		if (k === "agent_message_chunk" && update.content?.text)
			concurrentText += update.content.text;
		return;
	}
	if (stage === "permission") {
		if (k === "agent_message_chunk" && update.content?.text)
			permissionText += update.content.text;
		return;
	}
	if (k === "agent_message_chunk" && update.content?.text)
		text += update.content.text;
	else if (k === "agent_thought_chunk" && update.content?.text)
		thoughts += update.content.text;
	else if (k === "tool_call") {
		tools.push(["call", update.kind, update.title]);
		if ((update.content || []).some((c) => c.type === "diff"))
			tools.push([
				"diff",
				update.content.find((c) => c.type === "diff").newText,
			]);
	} else if (k === "tool_call_update") tools.push(["update", update.status]);
}

child.stdout.on("data", (d) => {
	buf += d;
	let i;
	while ((i = buf.indexOf("\n")) >= 0) {
		const line = buf.slice(0, i);
		buf = buf.slice(i + 1);
		if (!line.trim()) continue;
		let m;
		try {
			m = JSON.parse(line);
		} catch {
			continue;
		}

		if (m.id === 1 && m.result) {
			check(
				"initialize advertises loadSession",
				m.result.agentCapabilities?.loadSession === true,
			);
			check(
				"initialize advertises no auth (pi keys are external)",
				(m.result.authMethods || []).length === 0,
			);
			send({
				jsonrpc: "2.0",
				id: 2,
				method: "session/new",
				params: { cwd: "/tmp", mcpServers: [], additionalDirectories: [] },
			});
		} else if (m.id === 2 && m.result) {
			sid = m.result.sessionId;
			check(
				"session/new returns 6 thinking modes",
				m.result.modes?.availableModes?.length === 6,
			);
			const co = m.result.configOptions?.[0];
			const opts = co?.select?.options || co?.options || [];
			check(
				"session/new returns model config option",
				co?.id === "model" && opts.length === 2,
			);
			prompt(3, "go");
		} else if (m.method === "session/update" && m.params?.sessionId === sid) {
			collectUpdate(m.params.update);
		} else if (m.id === 3 && m.result) {
			check("prompt streamed coalesced text", text === "Hello world");
			check("prompt streamed thoughts", thoughts === "pondering");
			check(
				"tool_call emitted as execute",
				tools.some((t) => t[0] === "call" && t[1] === "execute"),
			);
			check(
				"tool_call completed",
				tools.some((t) => t[0] === "update" && t[1] === "completed"),
			);
			check(
				"edit tool surfaces a diff",
				tools.some((t) => t[0] === "diff" && t[1] === "bar"),
			);
			check("prompt stopReason end_turn", m.result.stopReason === "end_turn");
			send({
				jsonrpc: "2.0",
				id: 4,
				method: "session/set_mode",
				params: { sessionId: sid, modeId: "high" },
			});
		} else if (m.id === 4) {
			check("session/set_mode ok", !!m.result && !m.error);
			const cur = "prov/m1";
			send({
				jsonrpc: "2.0",
				id: 5,
				method: "session/set_config_option",
				params: { sessionId: sid, configId: "model", value: cur },
			});
		} else if (m.id === 5) {
			check("session/set_config_option ok", !!m.result && !m.error);
			send({
				jsonrpc: "2.0",
				id: 6,
				method: "session/set_config_option",
				params: { sessionId: sid, configId: "model", value: "prov/bad" },
			});
		} else if (m.id === 6) {
			check("set_config_option rejects bad model with error", !!m.error);
			send({
				jsonrpc: "2.0",
				id: 7,
				method: "session/set_mode",
				params: { sessionId: sid, modeId: "xhigh" },
			});
		} else if (m.id === 7) {
			check("session/set_mode surfaces pi rejection", !!m.error);
			stage = "load";
			histText = "";
			send({
				jsonrpc: "2.0",
				id: 8,
				method: "session/load",
				params: { sessionId: sid, cwd: "/tmp", mcpServers: [] },
			});
		} else if (m.id === 8 && m.result) {
			check(
				"session/load replays history (user+assistant+tool)",
				histText.includes("U") &&
					histText.includes("A") &&
					histText.includes("T"),
			);
			stage = "concurrent";
			prompt(9, "serialize-check");
			prompt(10, "serialize-check");
		} else if ((m.id === 9 || m.id === 10) && m.result) {
			concurrentDone.add(m.id);
			if (concurrentDone.size === 2) {
				check(
					"concurrent prompts are serialized",
					concurrentText === "serialized-1serialized-2",
				);
				check(
					"concurrent prompts do not overlap",
					!concurrentText.includes("OVERLAP"),
				);
				stage = "permission";
				prompt(11, "permission-timeout");
			}
		} else if (m.id === 11 && m.result) {
			check(
				"permission request times out as reject",
				permissionText === "denied",
			);
			check(
				"permission timeout turn completes",
				m.result.stopReason === "end_turn",
			);
			stage = "exit";
			prompt(12, "exit-early");
		} else if (m.id === 12 && m.error) {
			check("dead pi prompt surfaces error", !!m.error);
			stage = "cleanup-load";
			histText = "";
			send({
				jsonrpc: "2.0",
				id: 13,
				method: "session/load",
				params: { sessionId: sid, cwd: "/tmp", mcpServers: [] },
			});
		} else if (m.id === 13 && m.result) {
			check(
				"session/load respawns after dead pi",
				histText.includes("U") &&
					histText.includes("A") &&
					histText.includes("T"),
			);
			finish();
		} else if ([3, 8, 9, 10, 11, 13].includes(m.id) && m.error) {
			check(`request ${m.id} no error`, false);
			finish();
		}
	}
});

setTimeout(() => {
	check("completed before timeout", false);
	finish();
}, 20000);
send({
	jsonrpc: "2.0",
	id: 1,
	method: "initialize",
	params: { protocolVersion: 1 },
});
