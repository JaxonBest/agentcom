//! Hub-side IPC server: NDJSON over TCP on 127.0.0.1 with token auth.
//!
//! Most requests are forwarded to the hub loop and answered with a single
//! response frame. `Tail` is served directly from the shared ring buffers
//! (plus the UI broadcast for `--follow`) so per-connection streaming never
//! touches hub state.

use super::{HubInfo, Request, Response};
use crate::hub::events::{IpcMsg, UiEvent};
use crate::tui::ringbuf::SharedRingBuf;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, oneshot};

/// Agent name -> live output buffer; the hub registers buffers at spawn.
pub type Buffers = Arc<RwLock<HashMap<String, SharedRingBuf>>>;

pub struct IpcServer {
    pub info: HubInfo,
    listener: TcpListener,
    token: String,
    hub_tx: mpsc::Sender<IpcMsg>,
    buffers: Buffers,
    ui_tx: broadcast::Sender<UiEvent>,
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
        })
    }

    /// Persist hub.json so human terminals can discover us.
    pub fn write_hub_json(&self, path: &std::path::Path) -> Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(&self.info)?)?;
        Ok(())
    }

    pub async fn run(self) {
        loop {
            match self.listener.accept().await {
                Ok((stream, _addr)) => {
                    let token = self.token.clone();
                    let hub_tx = self.hub_tx.clone();
                    let buffers = self.buffers.clone();
                    let ui_rx = self.ui_tx.subscribe();
                    tokio::spawn(async move {
                        if let Err(e) = handle_conn(stream, token, hub_tx, buffers, ui_rx).await {
                            tracing::debug!(error = %e, "ipc connection ended with error");
                        }
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
    let identity = match lines.next_line().await? {
        Some(line) => match serde_json::from_str::<Request>(&line) {
            Ok(Request::Hello {
                token: t,
                identity,
            }) if t == token => identity,
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

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let req: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                write_frame(&mut write_half, &Response::err(format!("bad request: {e}")))
                    .await?;
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

async fn write_frame(
    w: &mut (impl AsyncWriteExt + Unpin),
    resp: &Response,
) -> std::io::Result<()> {
    let mut line = serde_json::to_string(resp).expect("response serializes");
    line.push('\n');
    w.write_all(line.as_bytes()).await
}
