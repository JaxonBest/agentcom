//! Client side of the IPC protocol, used by every `agentcom` subcommand
//! that talks to a running hub.
//!
//! Discovery order:
//! 1. `AGENTCOM_PORT` / `AGENTCOM_TOKEN` env vars — present inside agent
//!    child processes (and their Bash subprocesses), so agents need zero
//!    configuration and self-identify via `AGENTCOM_AGENT`.
//! 2. `hub.json` in the project data dir, located by walking up from the
//!    current directory to find `agentcom.toml`.

use super::{HubInfo, Request, Response};
use anyhow::{bail, Context, Result};
use std::path::Path;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

pub struct Client {
    reader: BufReader<tokio::net::tcp::OwnedReadHalf>,
    writer: tokio::net::tcp::OwnedWriteHalf,
}

/// Identity for a non-agent peer (human terminal, TUI, or CLI one-shot).
///
/// A stable id can be pinned via `AGENTCOM_SESSION` so a shell keeps a usable,
/// persistent inbox across invocations (otherwise each one-shot would mint a
/// fresh id, read from the cursor head, and see an empty inbox). Without it we
/// mint a per-process `human:<uuid>`.
fn human_identity() -> String {
    if let Ok(s) = std::env::var("AGENTCOM_SESSION") {
        let s = s.trim();
        if !s.is_empty() {
            return if s.starts_with("human") {
                s.to_string()
            } else {
                format!("human:{s}")
            };
        }
    }
    format!("human:{}", uuid::Uuid::new_v4())
}

pub fn discover() -> Result<(u16, String, String)> {
    if let (Ok(port), Ok(token)) = (
        std::env::var("AGENTCOM_PORT"),
        std::env::var("AGENTCOM_TOKEN"),
    ) {
        let identity = std::env::var("AGENTCOM_AGENT").unwrap_or_else(|_| human_identity());
        return Ok((
            port.parse().context("AGENTCOM_PORT is not a port")?,
            token,
            identity,
        ));
    }

    let cwd = std::env::current_dir()?;
    let root = crate::paths::find_project_root(&cwd).context(
        "no agentcom.toml found here or in any parent directory (and no AGENTCOM_PORT env)",
    )?;
    let hub_path = crate::paths::hub_json_path(&root)?;
    let info = read_hub_json(&hub_path)?;
    let identity = std::env::var("AGENTCOM_AGENT").unwrap_or_else(|_| human_identity());
    Ok((info.port, info.token, identity))
}

pub fn read_hub_json(path: &Path) -> Result<HubInfo> {
    let text = std::fs::read_to_string(path).with_context(|| {
        format!(
            "hub is not running (no {} — start it with `agentcom up`)",
            path.display()
        )
    })?;
    let info: HubInfo = serde_json::from_str(&text).context("hub.json is corrupt")?;
    Ok(info)
}

impl Client {
    pub async fn connect() -> Result<Self> {
        let (port, token, identity) = discover()?;
        Self::connect_to(port, &token, &identity).await
    }

    pub async fn connect_to(port: u16, token: &str, identity: &str) -> Result<Self> {
        let stream = TcpStream::connect(("127.0.0.1", port))
            .await
            .context("hub is not running (connection refused) — start it with `agentcom up`")?;
        let (read_half, writer) = stream.into_split();
        let mut client = Self {
            reader: BufReader::new(read_half),
            writer,
        };
        // CLI one-shots and the Client::connect path register as "cli";
        // agents (identity not starting with "human") send no kind.
        let kind = identity
            .starts_with("human")
            .then(|| "cli".to_string());
        let resp = client
            .request(&Request::Hello {
                token: token.to_string(),
                identity: identity.to_string(),
                kind,
                label: None,
            })
            .await?;
        match resp {
            // HelloOk is the new reply; plain Ok is accepted for a legacy hub.
            Response::Ok { .. } | Response::HelloOk { .. } => Ok(client),
            Response::Err { message } => bail!("hub rejected connection: {message}"),
            other => bail!("unexpected hello response: {other:?}"),
        }
    }

    pub async fn request(&mut self, req: &Request) -> Result<Response> {
        self.send(req).await?;
        self.next_response()
            .await?
            .context("hub closed the connection")
    }

    pub async fn send(&mut self, req: &Request) -> Result<()> {
        let mut line = serde_json::to_string(req)?;
        line.push('\n');
        self.writer.write_all(line.as_bytes()).await?;
        Ok(())
    }

    pub async fn next_response(&mut self) -> Result<Option<Response>> {
        let mut line = String::new();
        loop {
            line.clear();
            let n = self.reader.read_line(&mut line).await?;
            if n == 0 {
                return Ok(None);
            }
            if line.trim().is_empty() {
                continue;
            }
            return Ok(Some(serde_json::from_str(line.trim())?));
        }
    }
}
