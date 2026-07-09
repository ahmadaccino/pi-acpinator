//! pi-acpinator — an ACP agent that drives `pi --mode rpc`.

mod pi;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol::schema::v1::{
    AgentCapabilities, CancelNotification, ContentBlock, ContentChunk, InitializeRequest,
    InitializeResponse, NewSessionRequest, NewSessionResponse, PromptRequest, PromptResponse,
    SessionId, SessionNotification, SessionUpdate, StopReason,
};
use agent_client_protocol::{Agent, Client, ConnectionTo, Dispatch, Result, Stdio};
use tokio::sync::Mutex;

use crate::pi::client::{PiClient, PiIncoming};
use crate::pi::events::{Command, Incoming};

const PI_STATE_TIMEOUT: Duration = Duration::from_secs(5);

struct Session {
    pi: Arc<PiClient>,
    incoming: Mutex<PiIncoming>,
}

#[derive(Default, Clone)]
struct State {
    sessions: Arc<Mutex<HashMap<SessionId, Arc<Session>>>>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn".into()),
        )
        .init();

    let state = State::default();

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
                    match run_prompt(&state, req, conn).await {
                        Ok(stop) => responder.respond(PromptResponse::new(stop)),
                        Err(err) => responder.respond_with_error(
                            agent_client_protocol::util::internal_error(err.to_string()),
                        ),
                    }
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
            async move |message: Dispatch, cx: ConnectionTo<Client>| {
                message.respond_with_error(
                    agent_client_protocol::util::internal_error("unhandled message"),
                    cx,
                )
            },
            agent_client_protocol::on_receive_dispatch!(),
        )
        .connect_to(Stdio::new())
        .await
}

/// Spawn `pi --mode rpc` for a new ACP session and register it.
async fn start_session(state: &State, cwd: PathBuf) -> anyhow::Result<SessionId> {
    let args = [
        "--mode".to_string(),
        "rpc".to_string(),
        "--no-session".to_string(),
    ];
    let (pi, incoming) = PiClient::spawn("pi", &args, &cwd, &[]).await?;
    // Confirm the process is live and responsive before advertising the session.
    let id = pi.next_id();
    pi.request(Command::GetState { id: Some(id.clone()) }, &id, PI_STATE_TIMEOUT)
        .await?;

    let session_id = SessionId::new(uuid::Uuid::new_v4().to_string());
    state.sessions.lock().await.insert(
        session_id.clone(),
        Arc::new(Session {
            pi: Arc::new(pi),
            incoming: Mutex::new(incoming),
        }),
    );
    Ok(session_id)
}

/// Forward a prompt to pi and stream its output back as ACP session updates,
/// resolving when pi's turn ends.
async fn run_prompt(
    state: &State,
    req: PromptRequest,
    conn: ConnectionTo<Client>,
) -> anyhow::Result<StopReason> {
    let session = state
        .sessions
        .lock()
        .await
        .get(&req.session_id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("unknown session: {}", req.session_id.0))?;

    let message = prompt_text(&req.prompt);
    session.pi.send(Command::Prompt {
        id: None,
        message,
        images: Vec::new(),
        streaming_behavior: None,
    })?;

    let mut incoming = session.incoming.lock().await;
    while let Some(item) = incoming.recv().await {
        let Incoming::Event(event) = item else {
            continue;
        };
        if let Some(delta) = event.text_delta() {
            let _ = conn.send_notification(SessionNotification::new(
                req.session_id.clone(),
                SessionUpdate::AgentMessageChunk(ContentChunk::new(text_block(delta))),
            ));
        } else if event.kind == "agent_end" && !event.will_retry.unwrap_or(false) {
            return Ok(StopReason::EndTurn);
        }
    }
    Ok(StopReason::EndTurn)
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

fn text_block(text: &str) -> ContentBlock {
    ContentBlock::Text(agent_client_protocol::schema::v1::TextContent::new(text))
}
