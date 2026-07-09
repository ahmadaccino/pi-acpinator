//! pi-acpinator — an ACP agent that drives `pi --mode rpc`.

mod acp;
mod pi;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use agent_client_protocol::schema::v1::{
    AgentCapabilities, AuthMethod, AuthMethodAgent, AuthenticateRequest, AuthenticateResponse,
    CancelNotification, ContentBlock, ContentChunk, InitializeRequest, InitializeResponse,
    LoadSessionRequest, LoadSessionResponse, NewSessionRequest, NewSessionResponse,
    PermissionOption, PermissionOptionId, PermissionOptionKind, PromptRequest, PromptResponse,
    RequestPermissionOutcome, RequestPermissionRequest, SessionConfigOption, SessionId,
    SessionMode, SessionModeId, SessionModeState, SessionNotification, SessionUpdate,
    SetSessionConfigOptionRequest, SetSessionConfigOptionResponse, SetSessionModeRequest,
    SetSessionModeResponse, StopReason, ToolCall, ToolCallId, ToolCallStatus, ToolCallUpdate,
    ToolCallUpdateFields, ToolKind,
};
use agent_client_protocol::{Agent, Client, ConnectionTo, Dispatch, Stdio};
use tokio::sync::Mutex;

use crate::acp::translate;
use crate::pi::client::{PiClient, PiIncoming};
use crate::pi::events::{Command, Event, ExtensionUiRequest, ExtensionUiResponse, Incoming};

const PI_STATE_TIMEOUT: Duration = Duration::from_secs(5);

const MODEL_CONFIG_ID: &str = "model";

const THINKING_LEVELS: [&str; 6] = ["off", "minimal", "low", "medium", "high", "xhigh"];

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
    aborted: AtomicBool,
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
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();

    let approval = ApprovalMode::from_env();
    let gate = if approval == ApprovalMode::Off {
        None
    } else {
        let path =
            std::env::temp_dir().join(format!("pi-acpinator-gate-{}.ts", uuid::Uuid::new_v4()));
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
                        .agent_capabilities(AgentCapabilities::new().load_session(true))
                        .auth_methods(vec![AuthMethod::Agent(AuthMethodAgent::new(
                            "pi",
                            "pi (model provider keys configured via the pi CLI)",
                        ))]),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |_req: AuthenticateRequest, responder, _conn: ConnectionTo<Client>| {
                responder.respond(AuthenticateResponse::new())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let state = state.clone();
                async move |req: NewSessionRequest, responder, _conn: ConnectionTo<Client>| {
                    match start_session(&state, req.cwd).await {
                        Ok((session_id, setup)) => responder.respond(
                            NewSessionResponse::new(session_id)
                                .modes(Some(setup.modes))
                                .config_options(Some(setup.config_options)),
                        ),
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
                async move |req: LoadSessionRequest, responder, conn: ConnectionTo<Client>| {
                    match load_session(&state, &req, &conn).await {
                        Ok(setup) => responder.respond(
                            LoadSessionResponse::new()
                                .modes(Some(setup.modes))
                                .config_options(Some(setup.config_options)),
                        ),
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
                        session.aborted.store(true, Ordering::SeqCst);
                        let _ = session.pi.send(Command::Abort { id: None }).await;
                    }
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            {
                let state = state.clone();
                async move |req: SetSessionModeRequest, responder, _conn: ConnectionTo<Client>| {
                    let session = state.sessions.lock().await.get(&req.session_id).cloned();
                    match session {
                        Some(session) => {
                            let _ = session
                                .pi
                                .send(Command::SetThinkingLevel {
                                    id: None,
                                    level: req.mode_id.0.to_string(),
                                })
                                .await;
                            responder.respond(SetSessionModeResponse::new())
                        }
                        None => responder.respond_with_error(
                            agent_client_protocol::util::internal_error(format!(
                                "unknown session: {}",
                                req.session_id.0
                            )),
                        ),
                    }
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let state = state.clone();
                async move |req: SetSessionConfigOptionRequest,
                            responder,
                            _conn: ConnectionTo<Client>| {
                    match set_config_option(&state, &req).await {
                        Ok(options) => {
                            responder.respond(SetSessionConfigOptionResponse::new(options))
                        }
                        Err(err) => responder.respond_with_error(
                            agent_client_protocol::util::internal_error(err.to_string()),
                        ),
                    }
                }
            },
            agent_client_protocol::on_receive_request!(),
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

/// Modes + config options advertised to the client for a session.
struct SessionSetup {
    modes: SessionModeState,
    config_options: Vec<SessionConfigOption>,
}

/// Spawn `pi --mode rpc` bound to `session_id`, run the handshake, and build the
/// modes + config options. Shared by session/new and session/load.
async fn spawn_pi(
    config: &Config,
    cwd: &Path,
    session_id: &str,
) -> anyhow::Result<(PiClient, PiIncoming, SessionSetup)> {
    let mut args = vec![
        "--mode".to_string(),
        "rpc".to_string(),
        "--session-id".to_string(),
        session_id.to_string(),
    ];
    let mut env = Vec::new();
    if let Some(gate_path) = &config.gate_path {
        args.push("--extension".to_string());
        args.push(gate_path.to_string());
        env.push((
            "PI_ACP_APPROVAL_MODE".to_string(),
            config.approval.as_str().to_string(),
        ));
    }

    let program = std::env::var("PI_ACPINATOR_PI_BIN").unwrap_or_else(|_| "pi".to_string());
    let (pi, incoming) = PiClient::spawn(&program, &args, cwd, &env).await?;

    let id = pi.next_id();
    let state = pi
        .request(
            Command::GetState {
                id: Some(id.clone()),
            },
            &id,
            PI_STATE_TIMEOUT,
        )
        .await?
        .data
        .unwrap_or(serde_json::Value::Null);
    let current_level = state
        .get("thinkingLevel")
        .and_then(|v| v.as_str())
        .unwrap_or("medium")
        .to_string();

    if config.gate_path.is_some() {
        let id = pi.next_id();
        let resp = pi
            .request(
                Command::GetCommands {
                    id: Some(id.clone()),
                },
                &id,
                PI_STATE_TIMEOUT,
            )
            .await?;
        let loaded = resp
            .data
            .map(|d| d.to_string().contains("acp-permission-gate"))
            .unwrap_or(false);
        if !loaded {
            anyhow::bail!("permission gate extension failed to load");
        }
    }

    let id = pi.next_id();
    let models = pi
        .request(
            Command::GetAvailableModels {
                id: Some(id.clone()),
            },
            &id,
            PI_STATE_TIMEOUT,
        )
        .await?
        .data
        .and_then(|d| d.get("models").and_then(|m| m.as_array().cloned()))
        .unwrap_or_default();

    let setup = SessionSetup {
        modes: thinking_modes(&current_level),
        config_options: vec![model_config_option(
            &models,
            translate::current_model_value(&state),
        )],
    };
    Ok((pi, incoming, setup))
}

/// Spawn `pi --mode rpc` for a new ACP session and register it.
async fn start_session(state: &State, cwd: PathBuf) -> anyhow::Result<(SessionId, SessionSetup)> {
    let session_id = uuid::Uuid::new_v4().to_string();
    let (pi, incoming, setup) = spawn_pi(&state.config, &cwd, &session_id).await?;
    let session_id = SessionId::new(session_id);
    state.sessions.lock().await.insert(
        session_id.clone(),
        Arc::new(Session {
            pi: Arc::new(pi),
            incoming: Mutex::new(incoming),
            cwd: cwd.clone(),
            aborted: AtomicBool::new(false),
        }),
    );
    Ok((session_id, setup))
}

/// Resume a persisted pi session, replay its history to the client, and register it.
async fn load_session(
    state: &State,
    req: &LoadSessionRequest,
    conn: &ConnectionTo<Client>,
) -> anyhow::Result<SessionSetup> {
    let (pi, incoming, setup) =
        spawn_pi(&state.config, &req.cwd, req.session_id.0.as_ref()).await?;

    let id = pi.next_id();
    let messages = pi
        .request(
            Command::GetMessages {
                id: Some(id.clone()),
            },
            &id,
            PI_STATE_TIMEOUT,
        )
        .await?
        .data
        .and_then(|d| d.get("messages").and_then(|m| m.as_array().cloned()))
        .unwrap_or_default();
    for update in translate::history_updates(&messages, &req.cwd) {
        let _ = conn.send_notification(SessionNotification::new(req.session_id.clone(), update));
    }

    state.sessions.lock().await.insert(
        req.session_id.clone(),
        Arc::new(Session {
            pi: Arc::new(pi),
            incoming: Mutex::new(incoming),
            cwd: req.cwd.clone(),
            aborted: AtomicBool::new(false),
        }),
    );
    Ok(setup)
}

/// Apply a `session/set_config_option` (currently: the model selector). Returns
/// the refreshed config options reflecting the new selection.
async fn set_config_option(
    state: &State,
    req: &SetSessionConfigOptionRequest,
) -> anyhow::Result<Vec<SessionConfigOption>> {
    if req.config_id.0.as_ref() != MODEL_CONFIG_ID {
        anyhow::bail!("unknown config option: {}", req.config_id.0);
    }
    let value = req
        .value
        .as_value_id()
        .map(|v| v.0.to_string())
        .ok_or_else(|| anyhow::anyhow!("model config expects a value id"))?;
    let (provider, model_id) = translate::split_model_value(&value)
        .ok_or_else(|| anyhow::anyhow!("invalid model value: {value}"))?;
    let session = state
        .sessions
        .lock()
        .await
        .get(&req.session_id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("unknown session: {}", req.session_id.0))?;
    session
        .pi
        .send(Command::SetModel {
            id: None,
            provider,
            model_id,
        })
        .await?;

    let id = session.pi.next_id();
    let models = session
        .pi
        .request(
            Command::GetAvailableModels {
                id: Some(id.clone()),
            },
            &id,
            PI_STATE_TIMEOUT,
        )
        .await?
        .data
        .and_then(|d| d.get("models").and_then(|m| m.as_array().cloned()))
        .unwrap_or_default();
    Ok(vec![model_config_option(&models, Some(value))])
}

/// Build the model selector config option from pi's available models.
fn model_config_option(
    models: &[serde_json::Value],
    current: Option<String>,
) -> SessionConfigOption {
    let current = current.unwrap_or_default();
    SessionConfigOption::select(
        MODEL_CONFIG_ID,
        "Model",
        current,
        translate::model_options(models),
    )
    .description(Some("The model pi uses for this session".to_string()))
}

/// Advertise pi's thinking levels as ACP session modes.
fn thinking_modes(current: &str) -> SessionModeState {
    let modes = THINKING_LEVELS
        .iter()
        .map(|level| {
            let name = format!("{}{}", level[..1].to_uppercase(), &level[1..]);
            SessionMode::new(SessionModeId::new(*level), name)
                .description(Some(format!("Thinking level: {level}")))
        })
        .collect();
    let current = if THINKING_LEVELS.contains(&current) {
        current
    } else {
        "medium"
    };
    SessionModeState::new(SessionModeId::new(current), modes)
}

/// Forward a prompt to pi and stream its output back as ACP session updates,
/// bridging permission requests, until pi's turn ends.
async fn run_prompt(
    session: Arc<Session>,
    req: PromptRequest,
    conn: ConnectionTo<Client>,
) -> anyhow::Result<StopReason> {
    let session_id = req.session_id.clone();
    session.aborted.store(false, Ordering::SeqCst);
    session
        .pi
        .send(Command::Prompt {
            id: None,
            message: prompt_text(&req.prompt),
            images: Vec::new(),
            streaming_behavior: None,
        })
        .await?;

    let mut incoming = session.incoming.lock().await;
    let mut coalescer = Coalescer::default();
    while let Some(first) = incoming.recv().await {
        // Drain the burst already queued, coalescing contiguous text/thought
        // deltas into one chunk without adding latency.
        let mut item = Some(first);
        while let Some(current) = item.take() {
            match current {
                Incoming::Event(event) => {
                    if let Some((stream, delta)) = stream_delta(&event) {
                        if let Some(update) = coalescer.push(stream, delta) {
                            let _ = conn.send_notification(SessionNotification::new(
                                session_id.clone(),
                                update,
                            ));
                        }
                    } else {
                        flush(&mut coalescer, &conn, &session_id);
                        for update in tool_updates(&event, &session.cwd) {
                            let _ = conn.send_notification(SessionNotification::new(
                                session_id.clone(),
                                update,
                            ));
                        }
                        if event.kind == "agent_end" && !event.will_retry.unwrap_or(false) {
                            return Ok(if session.aborted.load(Ordering::SeqCst) {
                                StopReason::Cancelled
                            } else {
                                StopReason::EndTurn
                            });
                        }
                    }
                }
                Incoming::ExtensionUiRequest(ui) => {
                    flush(&mut coalescer, &conn, &session_id);
                    let response = handle_ui_request(&conn, &session_id, &ui).await;
                    let _ = session.pi.respond_ui(response).await;
                }
                _ => {}
            }
            item = incoming.try_recv().ok();
        }
        flush(&mut coalescer, &conn, &session_id);
    }
    // pi closed its stream before a terminal agent_end: surface a failure
    // instead of a false EndTurn (unless we asked it to abort).
    flush(&mut coalescer, &conn, &session_id);
    if session.aborted.load(Ordering::SeqCst) {
        return Ok(StopReason::Cancelled);
    }
    anyhow::bail!("pi ended the stream before completing the turn")
}

/// Coalesces contiguous assistant text / thought deltas into single chunks.
#[derive(Default)]
struct Coalescer {
    kind: Option<Stream>,
    buf: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stream {
    Message,
    Thought,
}

impl Coalescer {
    /// Buffer a delta; if it switches stream kind, return the flush of the
    /// previous kind so callers can emit it first.
    fn push(&mut self, kind: Stream, delta: &str) -> Option<SessionUpdate> {
        let flushed = if self.kind.is_some() && self.kind != Some(kind) {
            self.take()
        } else {
            None
        };
        self.kind = Some(kind);
        self.buf.push_str(delta);
        flushed
    }

    fn take(&mut self) -> Option<SessionUpdate> {
        let kind = self.kind.take()?;
        if self.buf.is_empty() {
            return None;
        }
        let text = std::mem::take(&mut self.buf);
        let chunk = ContentChunk::new(translate::text_block(&text));
        Some(match kind {
            Stream::Message => SessionUpdate::AgentMessageChunk(chunk),
            Stream::Thought => SessionUpdate::AgentThoughtChunk(chunk),
        })
    }
}

fn flush(coalescer: &mut Coalescer, conn: &ConnectionTo<Client>, session_id: &SessionId) {
    if let Some(update) = coalescer.take() {
        let _ = conn.send_notification(SessionNotification::new(session_id.clone(), update));
    }
}

fn stream_delta(event: &Event) -> Option<(Stream, &str)> {
    if let Some(delta) = event.text_delta() {
        Some((Stream::Message, delta))
    } else {
        event.thinking_delta().map(|delta| (Stream::Thought, delta))
    }
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
    let tool_call = ToolCallUpdate::new(
        ToolCallId::new(ui.id.as_str()),
        ToolCallUpdateFields::new().title(title),
    );
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

/// Translate a pi tool-execution event into ACP tool-call updates.
fn tool_updates(event: &Event, cwd: &Path) -> Vec<SessionUpdate> {
    let Some(id) = event.tool_call_id.as_deref() else {
        return Vec::new();
    };
    let call_id = ToolCallId::new(id);
    let name = event.tool_name.as_deref().unwrap_or("tool");
    match event.kind.as_str() {
        "tool_execution_start" => {
            let content: Vec<_> = translate::edit_diff(name, event.args.as_ref(), cwd)
                .into_iter()
                .collect();
            vec![SessionUpdate::ToolCall(
                ToolCall::new(
                    call_id,
                    translate::tool_call_title(name, event.args.as_ref()),
                )
                .kind(translate::tool_kind(name))
                .status(ToolCallStatus::InProgress)
                .raw_input(event.args.clone().unwrap_or(serde_json::Value::Null))
                .locations(translate::tool_locations(event.args.as_ref(), cwd))
                .content(content),
            )]
        }
        "tool_execution_update" => {
            let content = event
                .result
                .as_ref()
                .map(translate::tool_content)
                .unwrap_or_default();
            vec![SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                call_id,
                ToolCallUpdateFields::new()
                    .status(ToolCallStatus::InProgress)
                    .content(content),
            ))]
        }
        "tool_execution_end" => {
            let is_error = event.is_error.unwrap_or(false);
            let status = if is_error {
                ToolCallStatus::Failed
            } else {
                ToolCallStatus::Completed
            };
            let mut fields = ToolCallUpdateFields::new().status(status);
            // For a successful edit, keep the diff emitted at start; otherwise
            // attach the tool's text result (or error).
            if is_error || !matches!(translate::tool_kind(name), ToolKind::Edit) {
                let content = event
                    .result
                    .as_ref()
                    .map(translate::tool_content)
                    .unwrap_or_default();
                fields = fields.content(content);
            }
            vec![SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                call_id, fields,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(update: &SessionUpdate) -> (&'static str, String) {
        match update {
            SessionUpdate::AgentMessageChunk(c) => ("msg", chunk_text(c)),
            SessionUpdate::AgentThoughtChunk(c) => ("thought", chunk_text(c)),
            _ => ("other", String::new()),
        }
    }

    fn chunk_text(chunk: &ContentChunk) -> String {
        match &chunk.content {
            ContentBlock::Text(t) => t.text.clone(),
            _ => String::new(),
        }
    }

    #[test]
    fn coalesces_same_kind_and_flushes_on_switch() {
        let mut c = Coalescer::default();
        assert!(c.push(Stream::Message, "he").is_none());
        assert!(c.push(Stream::Message, "llo").is_none());
        // switching to thought flushes the buffered message
        let flushed = c.push(Stream::Thought, "hmm").expect("flush on switch");
        assert_eq!(text_of(&flushed), ("msg", "hello".to_string()));
        // remaining thought comes out on take
        let rest = c.take().expect("thought");
        assert_eq!(text_of(&rest), ("thought", "hmm".to_string()));
        assert!(c.take().is_none());
    }

    #[test]
    fn coalesces_a_pure_burst_into_one_chunk() {
        let mut c = Coalescer::default();
        for part in ["a", "b", "c", "d"] {
            assert!(c.push(Stream::Message, part).is_none());
        }
        let joined = c.take().expect("one chunk");
        assert_eq!(text_of(&joined), ("msg", "abcd".to_string()));
        assert!(c.take().is_none());
    }
}
