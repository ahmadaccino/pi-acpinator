//! Pure translation helpers: pi tool/event shapes -> ACP schema values.

use std::path::Path;

use agent_client_protocol::schema::v1::{
    ContentBlock, ContentChunk, Diff, SessionConfigSelectOption, SessionUpdate, TextContent,
    ToolCall, ToolCallContent, ToolCallId, ToolCallLocation, ToolCallStatus, ToolCallUpdate,
    ToolCallUpdateFields, ToolKind,
};
use serde_json::Value;

const TOOL_OUTPUT_CAP: usize = 8000;

pub fn text_block(text: &str) -> ContentBlock {
    ContentBlock::Text(TextContent::new(text))
}

pub fn tool_kind(name: &str) -> ToolKind {
    let tokens: Vec<&str> = name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    let has = |set: &[&str]| {
        tokens
            .iter()
            .any(|t| set.contains(&t.to_ascii_lowercase().as_str()))
    };
    if has(&[
        "edit", "write", "create", "patch", "apply", "replace", "insert", "append",
    ]) {
        ToolKind::Edit
    } else if has(&["delete", "rm", "remove", "unlink"]) {
        ToolKind::Delete
    } else if has(&["move", "rename", "mv"]) {
        ToolKind::Move
    } else if has(&["read", "cat", "view", "open"]) {
        ToolKind::Read
    } else if has(&["grep", "glob", "find", "search", "ls", "list", "rg"]) {
        ToolKind::Search
    } else if has(&[
        "bash", "shell", "sh", "exec", "execute", "run", "terminal", "command", "cmd",
    ]) {
        ToolKind::Execute
    } else if has(&["fetch", "http", "web", "url", "curl", "download"]) {
        ToolKind::Fetch
    } else if has(&["think", "reason", "reasoning"]) {
        ToolKind::Think
    } else {
        ToolKind::Other
    }
}

pub fn tool_call_title(name: &str, args: Option<&Value>) -> String {
    let detail = args.and_then(|a| {
        for key in [
            "command",
            "cmd",
            "file_path",
            "filePath",
            "path",
            "file",
            "pattern",
            "query",
            "url",
        ] {
            if let Some(s) = a.get(key).and_then(Value::as_str) {
                return Some(s.to_string());
            }
        }
        None
    });
    match detail {
        Some(d) => {
            let d = d.trim();
            let d = if d.chars().count() > 72 {
                format!("{}…", d.chars().take(72).collect::<String>())
            } else {
                d.to_string()
            };
            format!("{name}: {d}")
        }
        None => name.to_string(),
    }
}

/// Extract human-readable text from a pi tool result / partial payload.
pub fn tool_result_text(value: &Value) -> Option<String> {
    let text = match value {
        Value::Null => return None,
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(tool_result_text)
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(map) => {
            if let Some(content) = map.get("content") {
                tool_result_text(content).unwrap_or_default()
            } else if let Some(out) = map
                .get("stdout")
                .or_else(|| map.get("output"))
                .or_else(|| map.get("text"))
                .or_else(|| map.get("result"))
                .or_else(|| map.get("message"))
            {
                tool_result_text(out).unwrap_or_default()
            } else {
                return None;
            }
        }
        other => other.to_string(),
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.len() > TOOL_OUTPUT_CAP {
        let mut cut = TOOL_OUTPUT_CAP;
        while !trimmed.is_char_boundary(cut) {
            cut -= 1;
        }
        Some(format!("{}…", &trimmed[..cut]))
    } else {
        Some(trimmed.to_string())
    }
}

pub fn tool_content(value: &Value) -> Vec<ToolCallContent> {
    match tool_result_text(value) {
        Some(text) => vec![ToolCallContent::from(text_block(&text))],
        None => Vec::new(),
    }
}

/// File locations referenced by a tool's args, resolved against the session cwd.
pub fn tool_locations(args: Option<&Value>, cwd: &Path) -> Vec<ToolCallLocation> {
    let Some(args) = args else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for key in ["file_path", "filePath", "path", "file"] {
        if let Some(p) = args.get(key).and_then(Value::as_str) {
            out.push(ToolCallLocation::new(abs_path(p, cwd)));
            break;
        }
    }
    out
}

fn abs_path(path: &str, cwd: &Path) -> std::path::PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

/// Build a structured diff for an edit/write tool from its start args, so the
/// client renders the change instead of a plain "wrote N bytes" string.
pub fn edit_diff(name: &str, args: Option<&Value>, cwd: &Path) -> Option<ToolCallContent> {
    if !matches!(tool_kind(name), ToolKind::Edit) {
        return None;
    }
    let args = args?;
    let path = ["path", "file_path", "filePath", "file"]
        .iter()
        .find_map(|k| args.get(*k).and_then(Value::as_str))?;
    let abs = abs_path(path, cwd);

    // write: full new content, no prior text.
    if let Some(content) = args.get("content").and_then(Value::as_str) {
        return Some(ToolCallContent::Diff(Diff::new(abs, content)));
    }
    // edit: join the replacement fragments into old/new text.
    if let Some(edits) = args.get("edits").and_then(Value::as_array) {
        let mut old = String::new();
        let mut new = String::new();
        for edit in edits {
            if let Some(o) = edit.get("oldText").and_then(Value::as_str) {
                if !old.is_empty() {
                    old.push('\n');
                }
                old.push_str(o);
            }
            if let Some(n) = edit.get("newText").and_then(Value::as_str) {
                if !new.is_empty() {
                    new.push('\n');
                }
                new.push_str(n);
            }
        }
        if old.is_empty() && new.is_empty() {
            return None;
        }
        return Some(ToolCallContent::Diff(
            Diff::new(abs, new).old_text(Some(old)),
        ));
    }
    None
}

/// Map pi's available models to ACP select options (value = "<provider>/<id>").
pub fn model_options(models: &[Value]) -> Vec<SessionConfigSelectOption> {
    models
        .iter()
        .filter_map(|m| {
            let id = m.get("id")?.as_str()?;
            let provider = m.get("provider").and_then(Value::as_str).unwrap_or("");
            let name = m.get("name").and_then(Value::as_str).unwrap_or(id);
            Some(SessionConfigSelectOption::new(
                model_value(provider, id),
                if provider.is_empty() {
                    name.to_string()
                } else {
                    format!("{name} ({provider})")
                },
            ))
        })
        .collect()
}

/// The "<provider>/<id>" value for pi's current model in a get_state payload.
pub fn current_model_value(state: &Value) -> Option<String> {
    let model = state.get("model")?;
    let id = model.get("id")?.as_str()?;
    let provider = model.get("provider").and_then(Value::as_str).unwrap_or("");
    Some(model_value(provider, id))
}

fn model_value(provider: &str, id: &str) -> String {
    if provider.is_empty() {
        id.to_string()
    } else {
        format!("{provider}/{id}")
    }
}

/// Split a "<provider>/<id>" model value back into (provider, modelId). Splits
/// on the first slash, so model ids that themselves contain slashes survive.
pub fn split_model_value(value: &str) -> Option<(String, String)> {
    let (provider, id) = value.split_once('/')?;
    if provider.is_empty() || id.is_empty() {
        return None;
    }
    Some((provider.to_string(), id.to_string()))
}

/// Replay a pi message history (from `get_messages`) as ACP session updates.
pub fn history_updates(messages: &[Value], cwd: &Path) -> Vec<SessionUpdate> {
    let mut out = Vec::new();
    for msg in messages {
        match msg.get("role").and_then(Value::as_str).unwrap_or("") {
            "user" => {
                if let Some(text) = joined_text(msg.get("content")) {
                    out.push(SessionUpdate::UserMessageChunk(ContentChunk::new(
                        text_block(&text),
                    )));
                }
            }
            "assistant" => {
                let Some(parts) = msg.get("content").and_then(Value::as_array) else {
                    continue;
                };
                for part in parts {
                    match part.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            if let Some(t) = part.get("text").and_then(Value::as_str) {
                                out.push(SessionUpdate::AgentMessageChunk(ContentChunk::new(
                                    text_block(t),
                                )));
                            }
                        }
                        Some("toolCall") => {
                            if let Some(id) = part.get("id").and_then(Value::as_str) {
                                let name =
                                    part.get("name").and_then(Value::as_str).unwrap_or("tool");
                                let args = part.get("arguments");
                                out.push(SessionUpdate::ToolCall(
                                    ToolCall::new(ToolCallId::new(id), tool_call_title(name, args))
                                        .kind(tool_kind(name))
                                        .status(ToolCallStatus::InProgress)
                                        .raw_input(args.cloned().unwrap_or(Value::Null))
                                        .locations(tool_locations(args, cwd)),
                                ));
                            }
                        }
                        _ => {}
                    }
                }
            }
            "toolResult" => {
                if let Some(id) = msg.get("toolCallId").and_then(Value::as_str) {
                    let status = if msg.get("isError").and_then(Value::as_bool).unwrap_or(false) {
                        ToolCallStatus::Failed
                    } else {
                        ToolCallStatus::Completed
                    };
                    let content = msg.get("content").map(tool_content).unwrap_or_default();
                    out.push(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                        ToolCallId::new(id),
                        ToolCallUpdateFields::new().status(status).content(content),
                    )));
                }
            }
            _ => {}
        }
    }
    out
}

fn joined_text(content: Option<&Value>) -> Option<String> {
    let parts = content?.as_array()?;
    let text: String = parts
        .iter()
        .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|p| p.get("text").and_then(Value::as_str))
        .collect();
    if text.is_empty() { None } else { Some(text) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_tool_kinds_by_token() {
        assert!(matches!(tool_kind("read_file"), ToolKind::Read));
        assert!(matches!(tool_kind("apply_patch"), ToolKind::Edit));
        assert!(matches!(tool_kind("bash"), ToolKind::Execute));
        assert!(matches!(tool_kind("grep"), ToolKind::Search));
        assert!(matches!(tool_kind("web_fetch"), ToolKind::Fetch));
        assert!(matches!(tool_kind("mcp__custom__thing"), ToolKind::Other));
        // token-exact: "recommend" must not match "command"
        assert!(matches!(tool_kind("recommend"), ToolKind::Other));
    }

    #[test]
    fn extracts_result_text_shapes() {
        assert_eq!(
            tool_result_text(&serde_json::json!("hi")).as_deref(),
            Some("hi")
        );
        assert_eq!(
            tool_result_text(&serde_json::json!({"stdout": "out"})).as_deref(),
            Some("out")
        );
        assert_eq!(
            tool_result_text(&serde_json::json!({"content": [{"type":"text","text":"a"},{"type":"text","text":"b"}]}))
                .as_deref(),
            Some("a\nb")
        );
        assert_eq!(tool_result_text(&serde_json::json!({})), None);
        assert_eq!(tool_result_text(&Value::Null), None);
    }

    #[test]
    fn title_includes_short_detail() {
        assert_eq!(
            tool_call_title("bash", Some(&serde_json::json!({"command": "echo hi"}))),
            "bash: echo hi"
        );
        assert_eq!(tool_call_title("read", None), "read");
    }

    #[test]
    fn locations_resolve_relative_to_cwd() {
        let locs = tool_locations(
            Some(&serde_json::json!({"path": "src/x.rs"})),
            Path::new("/work"),
        );
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].path, Path::new("/work/src/x.rs"));
    }

    #[test]
    fn model_value_roundtrips_through_split() {
        let opts = model_options(&[serde_json::json!({
            "id": "anthropic/claude-3.5",
            "name": "Claude",
            "provider": "openrouter"
        })]);
        assert_eq!(opts.len(), 1);
        let (provider, id) = split_model_value(opts[0].value.0.as_ref()).unwrap();
        assert_eq!(provider, "openrouter");
        assert_eq!(id, "anthropic/claude-3.5");
    }

    #[test]
    fn builds_diff_for_write_and_edit() {
        let w = edit_diff(
            "write",
            Some(&serde_json::json!({"path": "a.txt", "content": "hi"})),
            Path::new("/w"),
        );
        match w {
            Some(ToolCallContent::Diff(d)) => {
                assert_eq!(d.path, Path::new("/w/a.txt"));
                assert_eq!(d.new_text, "hi");
                assert!(d.old_text.is_none());
            }
            _ => panic!("expected write diff"),
        }
        let e = edit_diff(
            "edit",
            Some(
                &serde_json::json!({"path": "/x/b.txt", "edits": [{"oldText": "a", "newText": "b"}]}),
            ),
            Path::new("/w"),
        );
        match e {
            Some(ToolCallContent::Diff(d)) => {
                assert_eq!(d.path, Path::new("/x/b.txt"));
                assert_eq!(d.old_text.as_deref(), Some("a"));
                assert_eq!(d.new_text, "b");
            }
            _ => panic!("expected edit diff"),
        }
        assert!(
            edit_diff(
                "bash",
                Some(&serde_json::json!({"command": "ls"})),
                Path::new("/w")
            )
            .is_none()
        );
    }

    #[test]
    fn replays_history_in_order() {
        let msgs = vec![
            serde_json::json!({"role":"user","content":[{"type":"text","text":"hi"}]}),
            serde_json::json!({"role":"assistant","content":[
                {"type":"toolCall","id":"t1","name":"bash","arguments":{"command":"ls"}}
            ]}),
            serde_json::json!({"role":"toolResult","toolCallId":"t1","toolName":"bash",
                "content":[{"type":"text","text":"out"}],"isError":false}),
            serde_json::json!({"role":"assistant","content":[{"type":"text","text":"done"}]}),
        ];
        let updates = history_updates(&msgs, Path::new("/w"));
        let kinds: Vec<&str> = updates
            .iter()
            .map(|u| match u {
                SessionUpdate::UserMessageChunk(_) => "user",
                SessionUpdate::AgentMessageChunk(_) => "msg",
                SessionUpdate::ToolCall(_) => "tool",
                SessionUpdate::ToolCallUpdate(_) => "tool_update",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["user", "tool", "tool_update", "msg"]);
    }
}
