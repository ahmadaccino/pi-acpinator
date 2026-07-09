//! Spawns `pi --mode rpc` and speaks its JSONL protocol: a single writer task
//! drains outgoing command lines to stdin, a reader task frames stdout on `\n`
//! (byte-level, so it never mis-splits on `U+2028`/`U+2029` inside JSON),
//! correlates `response` frames by id, and forwards events / extension-UI
//! requests to the caller.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use futures::StreamExt;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command as TokioCommand};
use tokio::sync::{mpsc, oneshot};
use tokio_util::codec::{FramedRead, LinesCodec};

use super::events::{parse_line, Command, ExtensionUiResponse, Incoming, Response};

type Pending = Arc<Mutex<HashMap<String, oneshot::Sender<Response>>>>;

/// Handle to a running `pi --mode rpc` process.
pub struct PiClient {
    lines: mpsc::UnboundedSender<String>,
    pending: Pending,
    next_id: std::sync::atomic::AtomicU64,
    _child: Child,
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

        let mut stdin = child.stdin.take().context("pi stdin missing")?;
        let stdout = child.stdout.take().context("pi stdout missing")?;
        let stderr = child.stderr.take();

        let (lines_tx, mut lines_rx) = mpsc::unbounded_channel::<String>();
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
                    Some(Incoming::Response(response)) => match response.id.clone() {
                        Some(id) => {
                            let waiter = pending_reader.lock().unwrap().remove(&id);
                            if let Some(waiter) = waiter {
                                let _ = waiter.send(response);
                            }
                        }
                        None => {}
                    },
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

        if let Some(stderr) = stderr {
            tokio::spawn(async move {
                let mut framed = FramedRead::new(stderr, LinesCodec::new());
                while let Some(Ok(line)) = framed.next().await {
                    tracing::debug!(target: "pi", "{line}");
                }
            });
        }

        Ok((
            Self {
                lines: lines_tx,
                pending,
                next_id: std::sync::atomic::AtomicU64::new(1),
                _child: child,
            },
            evt_rx,
        ))
    }

    fn write(&self, command: &Command) -> Result<()> {
        let line = serde_json::to_string(command)?;
        self.lines.send(line).context("pi writer closed")?;
        Ok(())
    }

    /// Fire-and-forget a command (used for streaming prompts, abort, …).
    pub fn send(&self, command: Command) -> Result<()> {
        self.write(&command)
    }

    /// Write an extension-UI response back to pi (permission-gate answers).
    pub fn respond_ui(&self, response: ExtensionUiResponse) -> Result<()> {
        let line = serde_json::to_string(&response)?;
        self.lines.send(line).context("pi writer closed")?;
        Ok(())
    }

    pub fn next_id(&self) -> String {
        let n = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        format!("acp-{n}")
    }

    /// Send a command carrying `id` and await its correlated response.
    pub async fn request(&self, command: Command, id: &str, timeout: Duration) -> Result<Response> {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id.to_string(), tx);
        self.write(&command)?;
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
