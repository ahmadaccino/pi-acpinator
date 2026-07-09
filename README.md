# pi-acpinator

A fast, tiny [ACP](https://agentclientprotocol.com) (Agent Client Protocol) adapter for the
[`pi`](https://github.com/earendil-works/pi) coding agent, written in Rust.

It speaks ACP JSON-RPC 2.0 over stdio to any ACP client (e.g. the Zed editor) and drives
`pi --mode rpc` underneath. Because it spawns `pi` rather than embedding a JS runtime, the
adapter is a small native binary — low memory, fast startup.

## Status

Early. Working today:

- `initialize` handshake
- `session/new` — spawns and supervises a `pi --mode rpc` process per session
- `session/prompt` — forwards the prompt and streams pi's assistant output back as ACP
  `agent_message_chunk` updates; resolves on turn end
- `session/cancel` — aborts the running turn

In progress (see `PLAN.md`): reasoning/thought streaming, tool-call mapping
(`tool_call`/`tool_call_update`) with structured diffs, `session/request_permission` via a
bundled pi permission-gate extension, model/thinking config options, `session/load`,
delta coalescing, and the benchmark harness.

## Build

```bash
cargo build --release
```

## Use with an ACP client (Zed)

```json
"agent_servers": {
  "pi": {
    "type": "custom",
    "command": "/path/to/pi-acpinator/target/release/pi-acpinator",
    "args": [],
    "env": {}
  }
}
```

Requires `pi` on `PATH` (`npm install -g @earendil-works/pi-coding-agent`), configured with
your model provider / API key.

## Develop

```bash
cargo test          # unit tests (JSONL framing, event classification)
RUST_LOG=debug cargo run
```

## License

MIT
