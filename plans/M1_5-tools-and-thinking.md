# M1.5 — Tool-call mapping + reasoning streaming

Extend `run_prompt` (src/main.rs) to translate pi tool execution + thinking into ACP session
updates, so ACP clients (Zed) render tool calls, progress, results, diffs, and thoughts.

## pi events (from pi stdout, already typed in `events.rs::Event`)
- `tool_execution_start` — `toolCallId`, `toolName`, `args` (object).
- `tool_execution_update` — `toolCallId`, `toolName`, `args`, `partialResult` (accumulated).
- `tool_execution_end` — `toolCallId`, `toolName`, `result`, `isError`.
- `message_update` with `assistantMessageEvent.type == "thinking_delta"` (use `Event::thinking_delta()`).

## ACP types (schema::v1)
- `SessionUpdate::AgentThoughtChunk(ContentChunk)` — thinking stream (mirror the existing
  AgentMessageChunk path).
- `SessionUpdate::ToolCall(ToolCall)` — on start.
  `ToolCall::new(tool_call_id, title)` then `.kind(ToolKind)` `.status(ToolCallStatus)`
  `.raw_input(Some(args))` `.content(Vec<ToolCallContent>)` `.locations(Vec<ToolCallLocation>)`.
- `SessionUpdate::ToolCallUpdate(ToolCallUpdate)` — on update/end.
  `ToolCallUpdate::new(tool_call_id, ToolCallUpdateFields::new().status(..).content(..)...)`.
- `ToolKind { Read, Edit, Delete, Move, Search, Execute, Think, Fetch, SwitchMode, Other }`.
- `ToolCallStatus { Pending, InProgress, Completed, Failed }`.
- `ToolCallContent::Content(Content::new(ContentBlock))` for text output;
  `ToolCallContent::Diff(Diff::new(path, new_text))` for edits (with old text if inferable).
- `ToolCallLocation::new(path)` — resolve relative paths against session cwd (store cwd on `Session`).
- `ToolCallId::new(pi.toolCallId)` — reuse pi's id verbatim for correlation.

## Implementation
1. **Store cwd** on `Session` (add `cwd: PathBuf`) for location resolution; set it in `start_session`.
2. In `run_prompt`'s event loop, extend the `match event.kind`:
   - `"tool_execution_start"` → build `ToolCall` (map toolName→ToolKind via a small helper
     `tool_kind(name)`: bash/shell/execute→Execute, read→Read, edit/write/apply_patch→Edit,
     grep/find/search→Search, fetch/web→Fetch, else→Other), title from toolName + a short arg
     summary, `status: InProgress`, `raw_input: args`, locations from `args.file_path`/`path`.
     Emit `SessionUpdate::ToolCall`.
   - `"tool_execution_update"` → `ToolCallUpdate` with `status: InProgress` and content from
     `partialResult` text (extract text via a `tool_result_text(value)` helper handling
     string / `content[]` / `stdout` / `output` / `text`).
   - `"tool_execution_end"` → `ToolCallUpdate` with `status: isError ? Failed : Completed`,
     content from `result`, and for edit/write a `Diff` when the result/args expose old/new text.
   - `thinking_delta` → `SessionUpdate::AgentThoughtChunk(ContentChunk::new(text_block(delta)))`.
3. Extract helpers into `src/acp/translate.rs` (new module): `tool_kind`, `tool_result_text`,
   `tool_call_title`, `content_text_block`. Keep them **pure** + unit-tested.

## Edge cases
- Unknown/custom/MCP tool names → `ToolKind::Other` (never guess-map).
- Large tool output: pass through as text (pi already truncates; don't re-buffer unbounded).
- `partialResult` is cumulative in pi — send it as a replace (ACP `tool_call_update` content
  replaces), don't diff-append.
- Interleaving: assistant `text_delta` and tool events share the pi stream; forward in arrival
  order (single loop already does this).

## Tests
- `translate.rs` unit: tool_kind mapping (incl. exact-token matching so `"recommend"` ≠ Execute),
  tool_result_text over the shapes, title formatting.
- Component (see TESTING.md): scripted fake pi emits start→update→end + thinking_delta; assert the
  emitted ACP `SessionUpdate` sequence.

## Verify
`cargo test`; live smoke with a prompt that forces a tool (e.g. "run the bash command: echo hi")
and confirm the client receives `tool_call` + `tool_call_update(Completed)` frames and a thought
chunk when the model reasons.

## Effort: ~0.5–1 day.
