//! Spawns `pi --mode rpc` and speaks its JSONL protocol: a single writer task
//! drains outgoing command lines to stdin, a reader task frames stdout on `\n`
//! (byte-level, so it never mis-splits on `U+2028`/`U+2029` inside JSON),
//! correlates `response` frames by id, and forwards events / extension-UI
//! requests to the caller.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use futures::StreamExt;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::process::{Child, Command as TokioCommand};
use tokio::sync::{mpsc, oneshot};
use tokio_util::codec::{FramedRead, LinesCodec};

use super::events::{Command, ExtensionUiResponse, Incoming, Response, parse_line};

type Pending = Arc<Mutex<HashMap<String, oneshot::Sender<Response>>>>;

/// Bounded outgoing queue so a slow pi applies backpressure to command writes
/// instead of letting them buffer without limit.
const OUTBOUND_CAPACITY: usize = 128;

/// Handle to a running `pi --mode rpc` process (or an injected transport in tests).
pub struct PiClient {
    lines: mpsc::Sender<String>,
    pending: Pending,
    next_id: AtomicU64,
    _child: Option<Child>,
}

/// Stream of pi events + extension-UI requests (correlated responses removed).
pub type PiIncoming = mpsc::UnboundedReceiver<Incoming>;

impl PiClient {
    pub async fn spawn(
        program: &str,
        args: &[String],
        cwd: &Path,
        env: &[(String, String)],
    ) -> Result<(Self, PiIncoming)> {
        let mut command = TokioCommand::new(program);
        command
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (key, value) in env {
            command.env(key, value);
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("failed to spawn `{program} --mode rpc`"))?;

        let stdin = child.stdin.take().context("pi stdin missing")?;
        let stdout = child.stdout.take().context("pi stdout missing")?;
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut framed = FramedRead::new(stderr, LinesCodec::new());
                while let Some(Ok(line)) = framed.next().await {
                    tracing::debug!(target: "pi", "{line}");
                }
            });
        }

        let (lines, pending, incoming) = Self::start(stdin, stdout);
        Ok((
            Self {
                lines,
                pending,
                next_id: AtomicU64::new(1),
                _child: Some(child),
            },
            incoming,
        ))
    }

    /// Build a client over arbitrary byte streams (used by tests with an
    /// in-memory fake pi).
    #[cfg(test)]
    pub fn from_io<W, R>(stdin: W, stdout: R) -> (Self, PiIncoming)
    where
        W: AsyncWrite + Unpin + Send + 'static,
        R: AsyncRead + Unpin + Send + 'static,
    {
        let (lines, pending, incoming) = Self::start(stdin, stdout);
        (
            Self {
                lines,
                pending,
                next_id: AtomicU64::new(1),
                _child: None,
            },
            incoming,
        )
    }

    fn start<W, R>(mut stdin: W, stdout: R) -> (mpsc::Sender<String>, Pending, PiIncoming)
    where
        W: AsyncWrite + Unpin + Send + 'static,
        R: AsyncRead + Unpin + Send + 'static,
    {
        let (lines_tx, mut lines_rx) = mpsc::channel::<String>(OUTBOUND_CAPACITY);
        tokio::spawn(async move {
            while let Some(line) = lines_rx.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err()
                    || stdin.write_all(b"\n").await.is_err()
                {
                    break;
                }
                let _ = stdin.flush().await;
            }
        });

        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let (evt_tx, evt_rx) = mpsc::unbounded_channel::<Incoming>();
        let pending_reader = pending.clone();
        tokio::spawn(async move {
            let mut framed = FramedRead::new(stdout, LinesCodec::new());
            while let Some(Ok(line)) = framed.next().await {
                match parse_line(&line) {
                    Some(Incoming::Response(response)) => {
                        if let Some(id) = response.id.clone() {
                            let waiter = pending_reader.lock().unwrap().remove(&id);
                            if let Some(waiter) = waiter {
                                let _ = waiter.send(response);
                            }
                        }
                    }
                    Some(other) => {
                        if evt_tx.send(other).is_err() {
                            break;
                        }
                    }
                    None => {}
                }
            }
            // stdout closed: drop pending waiters so in-flight requests fail fast.
            pending_reader.lock().unwrap().clear();
        });

        (lines_tx, pending, evt_rx)
    }

    async fn write(&self, command: &Command) -> Result<()> {
        let line = serde_json::to_string(command)?;
        self.lines.send(line).await.context("pi writer closed")?;
        Ok(())
    }

    /// Fire-and-forget a command (streaming prompts, abort, …).
    pub async fn send(&self, command: Command) -> Result<()> {
        self.write(&command).await
    }

    /// Write an extension-UI response back to pi (permission-gate answers).
    pub async fn respond_ui(&self, response: ExtensionUiResponse) -> Result<()> {
        let line = serde_json::to_string(&response)?;
        self.lines.send(line).await.context("pi writer closed")?;
        Ok(())
    }

    pub fn next_id(&self) -> String {
        let n = self.next_id.fetch_add(1, Ordering::Relaxed);
        format!("acp-{n}")
    }

    /// Send a command carrying `id` and await its correlated response.
    pub async fn request(&self, command: Command, id: &str, timeout: Duration) -> Result<Response> {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id.to_string(), tx);
        self.write(&command).await?;
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(_)) => anyhow::bail!("pi closed before responding to `{id}`"),
            Err(_) => {
                self.pending.lock().unwrap().remove(id);
                anyhow::bail!("pi request `{id}` timed out")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    /// Wire a PiClient to an in-memory fake pi: returns the client, its incoming
    /// stream, and the fake pi's (reader-of-commands, writer-of-events).
    fn wired() -> (
        PiClient,
        PiIncoming,
        BufReader<tokio::io::DuplexStream>,
        tokio::io::DuplexStream,
    ) {
        let (client_w, pi_r) = tokio::io::duplex(4096);
        let (pi_w, client_r) = tokio::io::duplex(4096);
        let (client, incoming) = PiClient::from_io(client_w, client_r);
        (client, incoming, BufReader::new(pi_r), pi_w)
    }

    #[tokio::test]
    async fn correlates_response_by_id() {
        let (client, _incoming, mut pi_in, mut pi_out) = wired();
        let id = client.next_id();
        let req = tokio::spawn(async move {
            client
                .request(
                    Command::GetState {
                        id: Some(id.clone()),
                    },
                    &id,
                    Duration::from_secs(2),
                )
                .await
        });
        // fake pi reads the command, then answers with the same id.
        let mut line = String::new();
        pi_in.read_line(&mut line).await.unwrap();
        assert!(line.contains("\"type\":\"get_state\""));
        let sent_id = serde_json::from_str::<serde_json::Value>(&line).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        pi_out
            .write_all(
                format!(
                    "{{\"type\":\"response\",\"command\":\"get_state\",\"success\":true,\"id\":\"{sent_id}\",\"data\":{{\"thinkingLevel\":\"high\"}}}}\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let resp = req.await.unwrap().unwrap();
        assert!(resp.success);
        assert_eq!(resp.data.unwrap()["thinkingLevel"], "high");
    }

    #[tokio::test]
    async fn forwards_events_and_ui_requests() {
        let (_client, mut incoming, _pi_in, mut pi_out) = wired();
        pi_out
            .write_all(b"{\"type\":\"agent_end\",\"willRetry\":false}\n")
            .await
            .unwrap();
        pi_out
            .write_all(b"{\"type\":\"extension_ui_request\",\"id\":\"1\",\"method\":\"confirm\"}\n")
            .await
            .unwrap();
        assert!(matches!(incoming.recv().await, Some(Incoming::Event(_))));
        assert!(matches!(
            incoming.recv().await,
            Some(Incoming::ExtensionUiRequest(_))
        ));
    }

    #[tokio::test]
    async fn request_fails_fast_when_pi_closes() {
        let (client, _incoming, pi_in, pi_out) = wired();
        drop(pi_out); // pi stdout EOF
        drop(pi_in);
        let id = client.next_id();
        let err = client
            .request(
                Command::GetState {
                    id: Some(id.clone()),
                },
                &id,
                Duration::from_secs(2),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("pi closed"));
    }
}
