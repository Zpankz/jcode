//! Long-lived `just-bash` sidecar process and its NDJSON-over-stdio client.
//!
//! Mirrors the MCP stdio client transport (`jcode-base/src/mcp/client.rs`): a
//! single `node server.mjs` child per session, a writer task draining a channel
//! into the child's stdin, and a reader task routing each NDJSON response to the
//! matching pending request by `id`. One sidecar owns one `Bash` instance, so
//! the virtual filesystem persists across requests for the session's lifetime.

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, mpsc, oneshot};

/// Default per-exec timeout enforced on the Rust side (the sidecar enforces its
/// own AbortSignal too). Generous because builds/data jobs can be slow.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// A response line from the sidecar.
#[derive(Debug, Deserialize)]
struct SidecarResponse {
    id: Option<String>,
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    stdout: Option<String>,
    #[serde(default)]
    stderr: Option<String>,
    #[serde(default, rename = "exitCode")]
    exit_code: Option<i64>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    version: Option<String>,
}

/// Result of an `exec` op.
#[derive(Debug, Clone)]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i64,
}

type Pending = Arc<Mutex<HashMap<String, oneshot::Sender<SidecarResponse>>>>;

/// A connected sidecar.
pub struct SidecarClient {
    request_id: AtomicU64,
    pending: Pending,
    writer_tx: mpsc::Sender<String>,
    child: Mutex<Child>,
}

impl SidecarClient {
    /// Spawn `node <server_mjs>` with the given environment and wire up the
    /// transport. `node_bin` is the resolved node executable.
    pub async fn spawn(
        node_bin: &str,
        server_mjs: &Path,
        env: &[(String, String)],
    ) -> Result<Self> {
        let mut command = Command::new(node_bin);
        command
            .arg(server_mjs)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (k, v) in env {
            command.env(k, v);
        }
        let mut child = command.spawn().with_context(|| {
            format!("spawn jsbash sidecar: {node_bin} {}", server_mjs.display())
        })?;

        let stdin = child.stdin.take().context("sidecar has no stdin")?;
        let stdout = child.stdout.take().context("sidecar has no stdout")?;
        let stderr = child.stderr.take().context("sidecar has no stderr")?;

        // Stderr -> logs (never fatal).
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) => {
                        let t = line.trim();
                        if !t.is_empty() {
                            crate::logging::warn(&format!("jsbash sidecar stderr: {t}"));
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let (writer_tx, mut writer_rx) = mpsc::channel::<String>(32);

        // Writer task.
        let mut stdin = stdin;
        tokio::spawn(async move {
            while let Some(msg) = writer_rx.recv().await {
                if stdin.write_all(msg.as_bytes()).await.is_err() {
                    break;
                }
                if stdin.flush().await.is_err() {
                    break;
                }
            }
        });

        // Reader task: route responses by id.
        let pending_reader = Arc::clone(&pending);
        let mut reader = BufReader::new(stdout);
        tokio::spawn(async move {
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        match serde_json::from_str::<SidecarResponse>(trimmed) {
                            Ok(resp) => {
                                if let Some(id) = resp.id.clone() {
                                    let mut p = pending_reader.lock().await;
                                    if let Some(tx) = p.remove(&id) {
                                        let _ = tx.send(resp);
                                    }
                                }
                            }
                            Err(e) => {
                                crate::logging::debug(&format!(
                                    "jsbash sidecar non-JSON line ({e}): {trimmed}"
                                ));
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            request_id: AtomicU64::new(1),
            pending,
            writer_tx,
            child: Mutex::new(child),
        })
    }

    /// Send a request and await the matching response (by id), with a timeout.
    async fn request(&self, mut payload: Value) -> Result<SidecarResponse> {
        let id = self.request_id.fetch_add(1, Ordering::SeqCst).to_string();
        if let Value::Object(map) = &mut payload {
            map.insert("id".to_string(), json!(id));
        }
        let (tx, rx) = oneshot::channel();
        {
            let mut p = self.pending.lock().await;
            p.insert(id.clone(), tx);
        }
        let msg = serde_json::to_string(&payload)? + "\n";
        self.writer_tx
            .send(msg)
            .await
            .map_err(|_| anyhow!("jsbash sidecar writer closed"))?;

        match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
            Ok(Ok(resp)) => Ok(resp),
            Ok(Err(_)) => Err(anyhow!("jsbash sidecar dropped the response channel")),
            Err(_) => {
                // Clean up the pending entry on timeout.
                let mut p = self.pending.lock().await;
                p.remove(&id);
                Err(anyhow!("jsbash sidecar request timed out"))
            }
        }
    }

    /// Probe the sidecar (and just-bash availability).
    pub async fn ready(&self) -> Result<String> {
        let resp = self.request(json!({"op": "ready"})).await?;
        if resp.ok {
            Ok(resp.version.unwrap_or_else(|| "just-bash".to_string()))
        } else {
            Err(anyhow!(
                resp.error.unwrap_or_else(|| "sidecar not ready".into())
            ))
        }
    }

    pub async fn exec(
        &self,
        script: &str,
        stdin: Option<&str>,
        cwd: Option<&str>,
        timeout_ms: Option<u64>,
    ) -> Result<ExecResult> {
        let mut payload = json!({"op": "exec", "script": script});
        if let Value::Object(map) = &mut payload {
            if let Some(s) = stdin {
                map.insert("stdin".into(), json!(s));
            }
            if let Some(c) = cwd {
                map.insert("cwd".into(), json!(c));
            }
            if let Some(t) = timeout_ms {
                map.insert("timeoutMs".into(), json!(t));
            }
        }
        let resp = self.request(payload).await?;
        if !resp.ok {
            return Err(anyhow!(resp.error.unwrap_or_else(|| "exec failed".into())));
        }
        Ok(ExecResult {
            stdout: resp.stdout.unwrap_or_default(),
            stderr: resp.stderr.unwrap_or_default(),
            exit_code: resp.exit_code.unwrap_or(0),
        })
    }

    pub async fn write_file(&self, path: &str, content: &str) -> Result<()> {
        let resp = self
            .request(json!({"op": "write", "path": path, "content": content}))
            .await?;
        if resp.ok {
            Ok(())
        } else {
            Err(anyhow!(resp.error.unwrap_or_else(|| "write failed".into())))
        }
    }

    pub async fn read_file(&self, path: &str) -> Result<String> {
        let resp = self.request(json!({"op": "read", "path": path})).await?;
        if resp.ok {
            Ok(resp.content.unwrap_or_default())
        } else {
            Err(anyhow!(resp.error.unwrap_or_else(|| "read failed".into())))
        }
    }

    pub async fn ls(&self, path: Option<&str>) -> Result<String> {
        let mut payload = json!({"op": "ls"});
        if let (Value::Object(map), Some(p)) = (&mut payload, path) {
            map.insert("path".into(), json!(p));
        }
        let resp = self.request(payload).await?;
        if resp.ok {
            Ok(resp.stdout.unwrap_or_default())
        } else {
            Err(anyhow!(resp.error.unwrap_or_else(|| "ls failed".into())))
        }
    }

    pub async fn reset(&self) -> Result<()> {
        let resp = self.request(json!({"op": "reset"})).await?;
        if resp.ok {
            Ok(())
        } else {
            Err(anyhow!(resp.error.unwrap_or_else(|| "reset failed".into())))
        }
    }

    /// Whether the child is still alive.
    pub async fn is_running(&self) -> bool {
        let mut child = self.child.lock().await;
        matches!(child.try_wait(), Ok(None))
    }
}
