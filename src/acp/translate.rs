//! Pure translation helpers: pi tool/event shapes -> ACP schema values.

use std::path::Path;

use agent_client_protocol::schema::v1::{
    ContentBlock, TextContent, ToolCallContent, ToolCallLocation, ToolKind,
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
            let path = Path::new(p);
            let abs = if path.is_absolute() {
                path.to_path_buf()
            } else {
                cwd.join(path)
            };
            out.push(ToolCallLocation::new(abs));
            break;
        }
    }
    out
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
}
