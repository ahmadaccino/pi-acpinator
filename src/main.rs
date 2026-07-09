//! pi-acpinator — an ACP agent that drives `pi --mode rpc`.

mod acp;
mod pi;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol::schema::v1::{
    AgentCapabilities, CancelNotification, ContentBlock, ContentChunk, InitializeRequest,
    InitializeResponse, NewSessionRequest, NewSessionResponse, PermissionOption,
    PermissionOptionId, PermissionOptionKind, PromptRequest, PromptResponse,
    RequestPermissionOutcome, RequestPermissionRequest, SessionId, SessionNotification,
    SessionUpdate, StopReason, ToolCall, ToolCallId, ToolCallStatus, ToolCallUpdate,
    ToolCallUpdateFields,
};
use agent_client_protocol::{Agent, Client, ConnectionTo, Dispatch, Stdio};
use tokio::sync::Mutex;

use crate::acp::translate;
use crate::pi::client::{PiClient, PiIncoming};
use crate::pi::events::{Command, Event, ExtensionUiRequest, ExtensionUiResponse, Incoming};

const PI_STATE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, PartialEq, Eq)]
enum ApprovalMode {
    Off,
    Mutating,
    All,
}

impl ApprovalMode {
    fn from_env() -> Self {
        match std::env::var("PI_ACPINATOR_APPROVAL")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "off" => Self::Off,
            "all" => Self::All,
            _ => Self::Mutating,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Mutating => "mutating",
            Self::All => "all",
        }
    }
}

#[derive(Clone)]
struct Config {
    approval: ApprovalMode,
    gate_path: Option<Arc<str>>,
}

/// Owns the temp file for the bundled gate extension; removes it on shutdown.
struct GateFile(PathBuf);

impl Drop for GateFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

struct Session {
    pi: Arc<PiClient>,
    incoming: Mutex<PiIncoming>,
    cwd: PathBuf,
}

#[derive(Clone)]
struct State {
    sessions: Arc<Mutex<HashMap<SessionId, Arc<Session>>>>,
    config: Config,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn".into()),
        )
        .init();

    let approval = ApprovalMode::from_env();
    let gate = if approval == ApprovalMode::Off {
        None
    } else {
        let path = std::env::temp_dir().join(format!("pi-acpinator-gate-{}.ts", uuid::Uuid::new_v4()));
        std::fs::write(&path, include_str!("../assets/permission-gate.ts"))?;
        Some(GateFile(path))
    };
    let config = Config {
        approval,
        gate_path: gate
            .as_ref()
            .map(|g| Arc::from(g.0.to_string_lossy().as_ref())),
    };

    let state = State {
        sessions: Arc::new(Mutex::new(HashMap::new())),
        config,
    };

    Agent
        .builder()
        .name("pi-acpinator")
        .on_receive_request(
            async move |req: InitializeRequest, responder, _conn| {
                responder.respond(
                    InitializeResponse::new(req.protocol_version)
                        .agent_capabilities(AgentCapabilities::new()),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let state = state.clone();
                async move |req: NewSessionRequest, responder, _conn: ConnectionTo<Client>| {
                    match start_session(&state, req.cwd).await {
                        Ok(session_id) => responder.respond(NewSessionResponse::new(session_id)),
                        Err(err) => responder.respond_with_error(
                            agent_client_protocol::util::internal_error(err.to_string()),
                        ),
                    }
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let state = state.clone();
                async move |req: PromptRequest, responder, conn: ConnectionTo<Client>| {
                    let session = state.sessions.lock().await.get(&req.session_id).cloned();
                    let Some(session) = session else {
                        return responder.respond_with_error(
                            agent_client_protocol::util::internal_error(format!(
                                "unknown session: {}",
                                req.session_id.0
                            )),
                        );
                    };
                    let task_conn = conn.clone();
                    conn.spawn(async move {
                        let stop = run_prompt(session, req, task_conn).await;
                        let _ = match stop {
                            Ok(reason) => responder.respond(PromptResponse::new(reason)),
                            Err(err) => responder.respond_with_error(
                                agent_client_protocol::util::internal_error(err.to_string()),
                            ),
                        };
                        Ok(())
                    })
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            {
                let state = state.clone();
                async move |note: CancelNotification, _conn: ConnectionTo<Client>| {
                    if let Some(session) = state.sessions.lock().await.get(&note.session_id) {
                        let _ = session.pi.send(Command::Abort { id: None });
                    }
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_dispatch(
            async move |message: Dispatch, cx: ConnectionTo<Client>| match message {
                Dispatch::Response(result, router) => router.respond_with_result(result),
                other => other.respond_with_error(
                    agent_client_protocol::util::internal_error("unhandled message"),
                    cx,
                ),
            },
            agent_client_protocol::on_receive_dispatch!(),
        )
        .connect_to(Stdio::new())
        .await
        .map_err(|err| anyhow::anyhow!("{err}"))?;

    drop(gate);
    Ok(())
}

/// Spawn `pi --mode rpc` for a new ACP session and register it.
async fn start_session(state: &State, cwd: PathBuf) -> anyhow::Result<SessionId> {
    let mut args = vec![
        "--mode".to_string(),
        "rpc".to_string(),
        "--no-session".to_string(),
    ];
    let mut env = Vec::new();
    if let Some(gate_path) = &state.config.gate_path {
        args.push("--extension".to_string());
        args.push(gate_path.to_string());
        env.push((
            "PI_ACP_APPROVAL_MODE".to_string(),
            state.config.approval.as_str().to_string(),
        ));
    }

    let (pi, incoming) = PiClient::spawn("pi", &args, &cwd, &env).await?;
    let id = pi.next_id();
    pi.request(Command::GetState { id: Some(id.clone()) }, &id, PI_STATE_TIMEOUT)
        .await?;

    if state.config.gate_path.is_some() {
        let id = pi.next_id();
        let resp = pi
            .request(Command::GetCommands { id: Some(id.clone()) }, &id, PI_STATE_TIMEOUT)
            .await?;
        let loaded = resp
            .data
            .map(|d| d.to_string().contains("acp-permission-gate"))
            .unwrap_or(false);
        if !loaded {
            anyhow::bail!("permission gate extension failed to load");
        }
    }

    let session_id = SessionId::new(uuid::Uuid::new_v4().to_string());
    state.sessions.lock().await.insert(
        session_id.clone(),
        Arc::new(Session {
            pi: Arc::new(pi),
            incoming: Mutex::new(incoming),
            cwd: cwd.to_path_buf(),
        }),
    );
    Ok(session_id)
}

/// Forward a prompt to pi and stream its output back as ACP session updates,
/// bridging permission requests, until pi's turn ends.
async fn run_prompt(
    session: Arc<Session>,
    req: PromptRequest,
    conn: ConnectionTo<Client>,
) -> anyhow::Result<StopReason> {
    let session_id = req.session_id.clone();
    session.pi.send(Command::Prompt {
        id: None,
        message: prompt_text(&req.prompt),
        images: Vec::new(),
        streaming_behavior: None,
    })?;

    let mut incoming = session.incoming.lock().await;
    while let Some(item) = incoming.recv().await {
        match item {
            Incoming::Event(event) => {
                for update in translate_event(&event, &session.cwd) {
                    let _ = conn
                        .send_notification(SessionNotification::new(session_id.clone(), update));
                }
                if event.kind == "agent_end" && !event.will_retry.unwrap_or(false) {
                    return Ok(StopReason::EndTurn);
                }
            }
            Incoming::ExtensionUiRequest(ui) => {
                let response = handle_ui_request(&conn, &session_id, &ui).await;
                let _ = session.pi.respond_ui(response);
            }
            _ => {}
        }
    }
    Ok(StopReason::EndTurn)
}

/// Translate a pi extension-UI request into an ACP permission decision (or
/// cancel unsupported dialogs so pi never hangs waiting on us).
async fn handle_ui_request(
    conn: &ConnectionTo<Client>,
    session_id: &SessionId,
    ui: &ExtensionUiRequest,
) -> ExtensionUiResponse {
    if ui.method != "confirm" {
        return ExtensionUiResponse {
            id: ui.id.clone(),
            confirmed: None,
            value: None,
            cancelled: Some(true),
        };
    }
    ExtensionUiResponse {
        id: ui.id.clone(),
        confirmed: Some(request_permission(conn, session_id, ui).await),
        value: None,
        cancelled: None,
    }
}

/// Ask the ACP client to approve a tool via `session/request_permission`.
async fn request_permission(
    conn: &ConnectionTo<Client>,
    session_id: &SessionId,
    ui: &ExtensionUiRequest,
) -> bool {
    let title = ui
        .title
        .clone()
        .or_else(|| ui.message.clone())
        .unwrap_or_else(|| "Allow tool?".to_string());
    let tool_call =
        ToolCallUpdate::new(ToolCallId::new(ui.id.as_str()), ToolCallUpdateFields::new().title(title));
    let options = vec![
        PermissionOption::new(
            PermissionOptionId::new("allow"),
            "Allow",
            PermissionOptionKind::AllowOnce,
        ),
        PermissionOption::new(
            PermissionOptionId::new("reject"),
            "Reject",
            PermissionOptionKind::RejectOnce,
        ),
    ];
    let request = RequestPermissionRequest::new(session_id.clone(), tool_call, options);
    match conn.send_request(request).block_task().await {
        Ok(response) => matches!(
            response.outcome,
            RequestPermissionOutcome::Selected(sel) if sel.option_id.0.starts_with("allow")
        ),
        Err(err) => {
            tracing::debug!(%err, "permission request failed");
            false
        }
    }
}

/// Translate a single pi event into zero or more ACP session updates.
fn translate_event(event: &Event, cwd: &Path) -> Vec<SessionUpdate> {
    if let Some(delta) = event.text_delta() {
        return vec![SessionUpdate::AgentMessageChunk(ContentChunk::new(
            translate::text_block(delta),
        ))];
    }
    if let Some(delta) = event.thinking_delta() {
        return vec![SessionUpdate::AgentThoughtChunk(ContentChunk::new(
            translate::text_block(delta),
        ))];
    }
    let Some(id) = event.tool_call_id.as_deref() else {
        return Vec::new();
    };
    let call_id = ToolCallId::new(id);
    match event.kind.as_str() {
        "tool_execution_start" => {
            let name = event.tool_name.as_deref().unwrap_or("tool");
            vec![SessionUpdate::ToolCall(
                ToolCall::new(call_id, translate::tool_call_title(name, event.args.as_ref()))
                    .kind(translate::tool_kind(name))
                    .status(ToolCallStatus::InProgress)
                    .raw_input(event.args.clone().unwrap_or(serde_json::Value::Null))
                    .locations(translate::tool_locations(event.args.as_ref(), cwd)),
            )]
        }
        "tool_execution_update" => {
            let content = event.result.as_ref().map(translate::tool_content).unwrap_or_default();
            vec![SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                call_id,
                ToolCallUpdateFields::new()
                    .status(ToolCallStatus::InProgress)
                    .content(content),
            ))]
        }
        "tool_execution_end" => {
            let status = if event.is_error.unwrap_or(false) {
                ToolCallStatus::Failed
            } else {
                ToolCallStatus::Completed
            };
            let content = event.result.as_ref().map(translate::tool_content).unwrap_or_default();
            vec![SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                call_id,
                ToolCallUpdateFields::new().status(status).content(content),
            ))]
        }
        _ => Vec::new(),
    }
}

/// Concatenate the text content blocks of an ACP prompt into a plain message.
fn prompt_text(blocks: &[ContentBlock]) -> String {
    let mut out = String::new();
    for block in blocks {
        if let ContentBlock::Text(text) = block {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&text.text);
        }
    }
    out
}
