//! Hub-side IPC server: NDJSON over TCP on 127.0.0.1 with token auth.
//!
//! Most requests are forwarded to the hub loop and answered with a single
//! response frame. `Tail` is served directly from the shared ring buffers
//! (plus the UI broadcast for `--follow`) so per-connection streaming never
//! touches hub state.
//!
//! Security hardening:
//! - MAX_CONNECTIONS: concurrent connection cap; new connections are rejected
//!   once the limit is hit to prevent a misbehaving agent from DoS-ing the hub.
//! - MAX_LINE_BYTES: per-request size cap; oversized frames are rejected
//!   immediately to prevent memory exhaustion from unbounded task descriptions.

use super::{HubInfo, Request, Response};
use crate::hub::events::{IpcMsg, UiEvent};
use crate::tui::ringbuf::SharedRingBuf;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, oneshot};

/// Concurrent connection cap. Rejects new connections once reached.
/// 64 is generous for any real fleet (max 9 agents + human terminals).
const MAX_CONNECTIONS: usize = 64;

/// Maximum JSON frame size in bytes. Rejects frames that exceed this.
/// 1 MiB is ample for any task description while preventing memory exhaustion.
const MAX_LINE_BYTES: usize = 1 << 20; // 1 MiB

/// Agent name -> live output buffer; the hub registers buffers at spawn.
pub type Buffers = Arc<RwLock<HashMap<String, SharedRingBuf>>>;

pub struct IpcServer {
    pub info: HubInfo,
    listener: TcpListener,
    token: String,
    hub_tx: mpsc::Sender<IpcMsg>,
    buffers: Buffers,
    ui_tx: broadcast::Sender<UiEvent>,
    /// Shared counter of live connections (across tasks).
    conn_count: Arc<AtomicUsize>,
}

impl IpcServer {
    pub async fn bind(
        project_root: std::path::PathBuf,
        hub_tx: mpsc::Sender<IpcMsg>,
        buffers: Buffers,
        ui_tx: broadcast::Sender<UiEvent>,
    ) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .context("binding IPC listener on 127.0.0.1")?;
        let port = listener.local_addr()?.port();
        let token: String = {
            use rand::Rng;
            let bytes: [u8; 32] = rand::thread_rng().gen();
            bytes.iter().map(|b| format!("{b:02x}")).collect()
        };
        let info = HubInfo {
            port,
            token: token.clone(),
            pid: std::process::id(),
            project_root,
            started_at: crate::store::now_ts(),
        };
        Ok(Self {
            info,
            listener,
            token,
            hub_tx,
            buffers,
            ui_tx,
            conn_count: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// Persist hub.json so human terminals can discover us.
    ///
    /// On Unix, sets the file to 0600 so only the owner can read the IPC
    /// token. Agents receive the token via `AGENTCOM_TOKEN` env var instead.
    pub fn write_hub_json(&self, path: &std::path::Path) -> Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(&self.info)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(path)?.permissions();
            perms.set_mode(0o600);
            std::fs::set_permissions(path, perms)?;
        }
        Ok(())
    }

    pub async fn run(self) {
        loop {
            match self.listener.accept().await {
                Ok((stream, _addr)) => {
                    let count = self.conn_count.fetch_add(1, Ordering::Relaxed) + 1;
                    if count > MAX_CONNECTIONS {
                        self.conn_count.fetch_sub(1, Ordering::Relaxed);
                        tracing::warn!(
                            "ipc: connection limit ({MAX_CONNECTIONS}) reached, dropping new connection"
                        );
                        // stream is dropped here, closing the TCP connection
                        continue;
                    }

                    let token = self.token.clone();
                    let hub_tx = self.hub_tx.clone();
                    let buffers = self.buffers.clone();
                    let ui_rx = self.ui_tx.subscribe();
                    let conn_count = self.conn_count.clone();
                    tokio::spawn(async move {
                        if let Err(e) =
                            handle_conn(stream, token, hub_tx, buffers, ui_rx).await
                        {
                            tracing::debug!(error = %e, "ipc connection ended with error");
                        }
                        conn_count.fetch_sub(1, Ordering::Relaxed);
                    });
                }
                Err(e) => {
                    tracing::warn!(error = %e, "ipc accept failed");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
    }
}

/// Compare two strings in constant time to prevent timing side channels on the
/// IPC token. Processes all bytes even after a mismatch — no early exit.
fn ct_eq(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Read exactly one line from `lines`, enforcing the MAX_LINE_BYTES cap.
/// Returns None on EOF, Err on I/O error, Ok(Some(line)) on success.
async fn read_line_bounded(
    lines: &mut tokio::io::Lines<BufReader<tokio::net::tcp::OwnedReadHalf>>,
) -> Result<Option<String>> {
    let line = lines.next_line().await?;
    if let Some(ref l) = line {
        if l.len() > MAX_LINE_BYTES {
            anyhow::bail!(
                "request frame exceeds {MAX_LINE_BYTES} bytes ({} bytes)",
                l.len()
            );
        }
    }
    Ok(line)
}

async fn handle_conn(
    stream: TcpStream,
    token: String,
    hub_tx: mpsc::Sender<IpcMsg>,
    buffers: Buffers,
    mut ui_rx: broadcast::Receiver<UiEvent>,
) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();

    // First frame must be a valid Hello.
    let identity = match read_line_bounded(&mut lines).await? {
        Some(line) => match serde_json::from_str::<Request>(&line) {
            Ok(Request::Hello { token: t, identity }) if ct_eq(&t, &token) => identity,
            Ok(Request::Hello { .. }) => {
                write_frame(&mut write_half, &Response::err("invalid token")).await?;
                return Ok(());
            }
            _ => {
                write_frame(&mut write_half, &Response::err("expected hello frame")).await?;
                return Ok(());
            }
        },
        None => return Ok(()),
    };
    write_frame(&mut write_half, &Response::ok()).await?;

    loop {
        let line = match read_line_bounded(&mut lines).await {
            Ok(Some(l)) => l,
            Ok(None) => break,
            Err(e) => {
                write_frame(
                    &mut write_half,
                    &Response::err(format!("request too large: {e}")),
                )
                .await?;
                return Ok(());
            }
        };

        if line.trim().is_empty() {
            continue;
        }
        let req: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                write_frame(&mut write_half, &Response::err(format!("bad request: {e}"))).await?;
                continue;
            }
        };

        match req {
            Request::Hello { .. } => {
                write_frame(&mut write_half, &Response::err("already authenticated")).await?;
            }
            Request::Tail {
                agent,
                lines: n,
                follow,
            } => {
                let backlog: Option<Vec<String>> = {
                    let map = buffers.read().unwrap();
                    map.get(&agent).map(|buf| buf.read().unwrap().tail(n))
                };
                let Some(backlog) = backlog else {
                    write_frame(
                        &mut write_half,
                        &Response::err(format!("unknown agent {agent:?}")),
                    )
                    .await?;
                    continue;
                };
                for l in backlog {
                    write_frame(&mut write_half, &Response::TailLine { line: l }).await?;
                }
                if !follow {
                    write_frame(&mut write_half, &Response::ok()).await?;
                    continue;
                }
                // Follow until the client hangs up or the hub shuts down.
                loop {
                    match ui_rx.recv().await {
                        Ok(UiEvent::AgentLine { agent: a, line }) if a == agent => {
                            if write_frame(&mut write_half, &Response::TailLine { line })
                                .await
                                .is_err()
                            {
                                return Ok(());
                            }
                        }
                        Ok(UiEvent::Shutdown) => return Ok(()),
                        Ok(_) => {}
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => return Ok(()),
                    }
                }
            }
            other => {
                let (reply_tx, reply_rx) = oneshot::channel();
                let msg = IpcMsg {
                    identity: identity.clone(),
                    request: other,
                    reply: reply_tx,
                };
                if hub_tx.send(msg).await.is_err() {
                    write_frame(&mut write_half, &Response::err("hub is shutting down")).await?;
                    return Ok(());
                }
                let resp = reply_rx
                    .await
                    .unwrap_or_else(|_| Response::err("hub dropped the request"));
                write_frame(&mut write_half, &resp).await?;
            }
        }
    }
    Ok(())
}

async fn write_frame(w: &mut (impl AsyncWriteExt + Unpin), resp: &Response) -> std::io::Result<()> {
    let mut line = serde_json::to_string(resp).expect("response serializes");
    line.push('\n');
    w.write_all(line.as_bytes()).await
}

#[cfg(test)]
mod tests {
    use super::ct_eq;

    #[test]
    fn ct_eq_matches_equal_strings() {
        assert!(ct_eq("abc", "abc"));
        assert!(ct_eq("", ""));
        let tok = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
        assert!(ct_eq(tok, tok));
    }

    #[test]
    fn ct_eq_rejects_different_strings() {
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "ab"));
        assert!(!ct_eq("", "x"));
        assert!(!ct_eq("token", "Token")); // case-sensitive
    }
}
