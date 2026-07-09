//! Typed pi `--mode rpc` protocol: commands we send, events/responses we read.
//! Mirrors the JSONL protocol documented in pi's `docs/rpc.md`.

// Faithful protocol definition: some commands/fields are not wired to a handler
// yet (model selection, images) but are kept so the surface stays complete.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};

/// A command sent to pi on stdin (one JSON object per line).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    Prompt {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        message: String,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        images: Vec<Image>,
        #[serde(rename = "streamingBehavior", skip_serializing_if = "Option::is_none")]
        streaming_behavior: Option<StreamingBehavior>,
    },
    Steer {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        message: String,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        images: Vec<Image>,
    },
    Abort {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    GetState {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    SetModel {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        provider: String,
        #[serde(rename = "modelId")]
        model_id: String,
    },
    SetThinkingLevel {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        level: String,
    },
    GetAvailableModels {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    GetCommands {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StreamingBehavior {
    Steer,
    FollowUp,
}

/// pi `ImageContent`: `{ type:"image", data:<base64>, mimeType }`.
#[derive(Debug, Clone, Serialize)]
pub struct Image {
    #[serde(rename = "type")]
    pub kind: ImageKind,
    pub data: String,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageKind {
    Image,
}

/// An extension-UI response written back to pi (for the permission gate).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename = "extension_ui_response")]
pub struct ExtensionUiResponse {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancelled: Option<bool>,
}

/// A line read from pi stdout, classified. Anything unrecognized is `Other`.
#[derive(Debug, Clone)]
pub enum Incoming {
    Response(Response),
    ExtensionUiRequest(ExtensionUiRequest),
    Event(Event),
    Other,
}

/// A correlated command response (`{type:"response", command, success, id?, ...}`).
#[derive(Debug, Clone, Deserialize)]
pub struct Response {
    pub id: Option<String>,
    pub command: String,
    pub success: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub data: Option<serde_json::Value>,
}

/// An extension-UI request emitted by a pi extension (dialog or fire-and-forget).
#[derive(Debug, Clone, Deserialize)]
pub struct ExtensionUiRequest {
    pub id: String,
    pub method: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub options: Option<Vec<String>>,
}

/// An agent session event streamed on pi stdout. Only the fields we translate
/// are typed; the rest ride along in `extra`.
#[derive(Debug, Clone, Deserialize)]
pub struct Event {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub message: Option<serde_json::Value>,
    #[serde(rename = "assistantMessageEvent", default)]
    pub assistant_message_event: Option<AssistantMessageEvent>,
    #[serde(rename = "toolCallId", default)]
    pub tool_call_id: Option<String>,
    #[serde(rename = "toolName", default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub args: Option<serde_json::Value>,
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    #[serde(rename = "isError", default)]
    pub is_error: Option<bool>,
    #[serde(rename = "willRetry", default)]
    pub will_retry: Option<bool>,
}

/// The streaming delta carried by a `message_update` event.
#[derive(Debug, Clone, Deserialize)]
pub struct AssistantMessageEvent {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub delta: Option<String>,
}

/// Parse one stdout line into a classified [`Incoming`]. Non-object / blank
/// lines and stray output parse to `None` and are ignored by the reader.
pub fn parse_line(line: &str) -> Option<Incoming> {
    let trimmed = line.trim();
    if trimmed.is_empty() || !trimmed.starts_with('{') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let ty = value.get("type").and_then(|v| v.as_str())?;
    match ty {
        "response" => serde_json::from_value(value).ok().map(Incoming::Response),
        "extension_ui_request" => serde_json::from_value(value)
            .ok()
            .map(Incoming::ExtensionUiRequest),
        _ => Some(
            serde_json::from_value::<Event>(value)
                .map(Incoming::Event)
                .unwrap_or(Incoming::Other),
        ),
    }
}

impl Event {
    /// Visible assistant text delta (`assistantMessageEvent.type == "text_delta"`).
    pub fn text_delta(&self) -> Option<&str> {
        let ame = self.assistant_message_event.as_ref()?;
        if ame.kind == "text_delta" {
            ame.delta.as_deref()
        } else {
            None
        }
    }

    /// Reasoning delta (`thinking_delta`).
    pub fn thinking_delta(&self) -> Option<&str> {
        let ame = self.assistant_message_event.as_ref()?;
        if ame.kind == "thinking_delta" {
            ame.delta.as_deref()
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_response_event_and_ui() {
        assert!(matches!(
            parse_line(r#"{"type":"response","command":"get_state","success":true}"#),
            Some(Incoming::Response(_))
        ));
        assert!(matches!(
            parse_line(r#"{"type":"agent_end","willRetry":false}"#),
            Some(Incoming::Event(_))
        ));
        assert!(matches!(
            parse_line(r#"{"type":"extension_ui_request","id":"1","method":"confirm"}"#),
            Some(Incoming::ExtensionUiRequest(_))
        ));
        assert!(parse_line("not json").is_none());
        assert!(parse_line("").is_none());
    }

    #[test]
    fn extracts_text_delta_only() {
        let ev = match parse_line(
            r#"{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"hi"}}"#,
        ) {
            Some(Incoming::Event(e)) => e,
            _ => panic!("expected event"),
        };
        assert_eq!(ev.text_delta(), Some("hi"));
        assert_eq!(ev.thinking_delta(), None);
    }
}
