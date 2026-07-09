#!/usr/bin/env node

// A scripted fake `pi --mode rpc` for deterministic component tests: no model,
// no network. Reads JSONL commands on stdin, emits canned responses/events.
let buf = "";
const write = (o) => process.stdout.write(JSON.stringify(o) + "\n");
const reply = (cmd, id, data) =>
	write({ type: "response", command: cmd, success: true, id, data });
const replyError = (cmd, id, error) =>
	write({ type: "response", command: cmd, success: false, id, error });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const MODELS = [
	{ id: "m1", name: "Model One", provider: "prov" },
	{ id: "m2", name: "Model Two", provider: "prov" },
];
const HISTORY = [
	{ role: "user", content: [{ type: "text", text: "prior question" }] },
	{
		role: "assistant",
		content: [
			{
				type: "toolCall",
				id: "h1",
				name: "read",
				arguments: { path: "a.txt" },
			},
		],
	},
	{
		role: "toolResult",
		toolCallId: "h1",
		toolName: "read",
		content: [{ type: "text", text: "file body" }],
		isError: false,
	},
	{ role: "assistant", content: [{ type: "text", text: "prior answer" }] },
];

let serializedPromptActive = false;
let serializedPromptCount = 0;
const uiWaiters = new Map();

async function runSerializedPrompt() {
	const n = ++serializedPromptCount;
	if (serializedPromptActive) {
		write({
			type: "message_update",
			assistantMessageEvent: { type: "text_delta", delta: "OVERLAP" },
		});
		write({ type: "agent_end", willRetry: false });
		return;
	}
	serializedPromptActive = true;
	await sleep(parseInt(process.env.PI_FAKE_SERIAL_PROMPT_MS || "25", 10));
	write({
		type: "message_update",
		assistantMessageEvent: { type: "text_delta", delta: `serialized-${n}` },
	});
	write({ type: "agent_end", willRetry: false });
	serializedPromptActive = false;
}

function waitForUi(id, timeoutMs) {
	return new Promise((resolve) => {
		const timeout = setTimeout(() => {
			if (uiWaiters.delete(id)) resolve(null);
		}, timeoutMs);
		uiWaiters.set(id, (value) => {
			clearTimeout(timeout);
			resolve(value);
		});
	});
}

async function runPermissionPrompt() {
	const id = `perm-${Date.now()}`;
	write({
		type: "extension_ui_request",
		id,
		method: "confirm",
		title: "Allow bash?",
		message: "bash: true",
	});
	const response = await waitForUi(id, 1000);
	write({
		type: "message_update",
		assistantMessageEvent: {
			type: "text_delta",
			delta: response?.confirmed ? "allowed" : "denied",
		},
	});
	write({ type: "agent_end", willRetry: false });
}

async function runPrompt(message = "") {
	const bench = parseInt(process.env.PI_FAKE_BENCH_DELTAS || "0", 10);
	if (bench > 0) {
		// high-throughput mode: emit `bench` tiny text deltas as fast as possible.
		for (let n = 0; n < bench; n++) {
			write({
				type: "message_update",
				assistantMessageEvent: { type: "text_delta", delta: "tok " },
			});
		}
		write({ type: "agent_end", willRetry: false });
		return;
	}
	if (message === "serialize-check") {
		await runSerializedPrompt();
		return;
	}
	if (message === "permission-timeout") {
		await runPermissionPrompt();
		return;
	}
	if (message === "exit-early") {
		process.exit(0);
	}

	await sleep(5);
	// contiguous thinking burst
	write({
		type: "message_update",
		assistantMessageEvent: { type: "thinking_delta", delta: "pon" },
	});
	write({
		type: "message_update",
		assistantMessageEvent: { type: "thinking_delta", delta: "dering" },
	});
	// contiguous text burst -> should coalesce to "Hello"
	write({
		type: "message_update",
		assistantMessageEvent: { type: "text_delta", delta: "Hel" },
	});
	write({
		type: "message_update",
		assistantMessageEvent: { type: "text_delta", delta: "lo" },
	});
	await sleep(5);
	write({
		type: "tool_execution_start",
		toolCallId: "tc1",
		toolName: "bash",
		args: { command: "echo hi" },
	});
	write({
		type: "tool_execution_end",
		toolCallId: "tc1",
		toolName: "bash",
		result: { content: [{ type: "text", text: "hi" }] },
		isError: false,
	});
	await sleep(5);
	// an edit tool -> should surface as a structured diff
	write({
		type: "tool_execution_start",
		toolCallId: "tc2",
		toolName: "edit",
		args: { path: "/tmp/x.txt", edits: [{ oldText: "foo", newText: "bar" }] },
	});
	write({
		type: "tool_execution_end",
		toolCallId: "tc2",
		toolName: "edit",
		result: {
			content: [{ type: "text", text: "Successfully replaced 1 block(s)" }],
		},
		isError: false,
	});
	await sleep(5);
	write({
		type: "message_update",
		assistantMessageEvent: { type: "text_delta", delta: " world" },
	});
	write({ type: "agent_end", willRetry: false });
}

process.stdin.on("data", (d) => {
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
		switch (m.type) {
			case "get_state":
				reply("get_state", m.id, {
					thinkingLevel: "low",
					model: MODELS[0],
					sessionId: "fake",
					sessionFile: "/tmp/fake.jsonl",
					isStreaming: false,
				});
				break;
			case "get_available_models":
				reply("get_available_models", m.id, { models: MODELS });
				break;
			case "get_commands":
				reply("get_commands", m.id, { commands: [] });
				break;
			case "get_messages":
				reply("get_messages", m.id, { messages: HISTORY });
				break;
			case "set_model":
				if (m.modelId === "bad")
					replyError("set_model", m.id, "no API key for that model");
				else
					reply("set_model", m.id, {
						model: { id: m.modelId, provider: m.provider },
					});
				break;
			case "set_thinking_level":
				if (m.level === "xhigh")
					replyError("set_thinking_level", m.id, "unsupported test level");
				else reply("set_thinking_level", m.id, { thinkingLevel: m.level });
				break;
			case "prompt":
				runPrompt(m.message);
				break;
			case "abort":
				write({ type: "agent_end", willRetry: false });
				break;
			case "extension_ui_response": {
				const waiter = uiWaiters.get(m.id);
				if (waiter) {
					uiWaiters.delete(m.id);
					waiter(m);
				}
				break;
			}
			default:
				if (m.id) reply(m.type, m.id, {});
		}
	}
});
