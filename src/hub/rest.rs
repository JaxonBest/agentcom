//! Minimal HTTP/1.1 JSON API for external dashboards and CI health checks.
//! Acts as a thin proxy to the IPC server — no shared state needed.
//!
//! Endpoints (127.0.0.1 only):
//!   GET /health  — no auth; returns {"status":"ok","uptime_secs":N,"version":"..."}
//!   GET /status  — requires Bearer token; fleet status (agents, cost, open tasks)
//!   GET /tasks   — requires Bearer token; task board (all statuses)
//!   GET /agents  — requires Bearer token; agent list from status
//!
//! The token is the same IPC token used by the hub protocol.

use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

pub async fn serve(port: u16, ipc_port: u16, token: String) {
    let addr = format!("127.0.0.1:{port}");
    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => {
            tracing::info!("REST API listening on http://{addr}");
            l
        }
        Err(e) => {
            tracing::warn!("REST API: failed to bind {addr}: {e}");
            return;
        }
    };
    let started = Instant::now();
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(s) => s,
            Err(_) => break,
        };
        if !peer.ip().is_loopback() {
            continue;
        }
        let token = token.clone();
        let uptime = started.elapsed().as_secs();
        tokio::spawn(async move {
            if let Err(e) = handle_conn(stream, ipc_port, &token, uptime).await {
                tracing::debug!("REST: {peer} error: {e}");
            }
        });
    }
}

async fn handle_conn(
    stream: tokio::net::TcpStream,
    ipc_port: u16,
    token: &str,
    uptime_secs: u64,
) -> anyhow::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    let mut request_line = String::new();
    reader.read_line(&mut request_line).await?;
    let request_line = request_line.trim();

    let mut parts = request_line.splitn(3, ' ');
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("").split('?').next().unwrap_or("");

    // Drain headers, look for Authorization.
    let mut auth_header: Option<String> = None;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        let line = line.trim();
        if line.is_empty() {
            break;
        }
        if let Some(rest) = line.strip_prefix("Authorization:") {
            auth_header = Some(rest.trim().to_string());
        }
    }

    // /health is unauthenticated — useful for load balancers and CI readiness checks.
    if path == "/health" {
        if method != "GET" {
            writer
                .write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\n\r\n")
                .await?;
            return Ok(());
        }
        let version = env!("CARGO_PKG_VERSION");
        let body = format!(
            r#"{{"status":"ok","uptime_secs":{uptime_secs},"version":"{version}"}}"#
        );
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        writer.write_all(header.as_bytes()).await?;
        writer.write_all(body.as_bytes()).await?;
        return Ok(());
    }

    let expected = format!("Bearer {token}");
    if auth_header.as_deref() != Some(&expected) {
        writer
            .write_all(
                b"HTTP/1.1 401 Unauthorized\r\nContent-Type: text/plain\r\nContent-Length: 13\r\n\r\nunauthorized\n",
            )
            .await?;
        return Ok(());
    }

    if method != "GET" {
        writer
            .write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\n\r\n")
            .await?;
        return Ok(());
    }

    // Map REST path → IPC request JSON.
    let ipc_req = match path {
        "/status" => Some(r#"{"cmd":"status"}"#),
        "/tasks" => Some(r#"{"cmd":"task_list","status":null,"search":null,"tag":null}"#),
        "/agents" => Some(r#"{"cmd":"status"}"#),
        _ => None,
    };

    let Some(ipc_req) = ipc_req else {
        writer
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
            .await?;
        return Ok(());
    };

    // Forward to IPC server.
    let ipc_resp = match query_ipc(ipc_port, token, ipc_req).await {
        Ok(r) => r,
        Err(e) => {
            let body = format!("{{\"error\":\"{e}\"}}");
            let header = format!(
                "HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            writer.write_all(header.as_bytes()).await?;
            writer.write_all(body.as_bytes()).await?;
            return Ok(());
        }
    };

    // For /agents, extract agents array from status response.
    let body = if path == "/agents" {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&ipc_resp) {
            serde_json::to_string_pretty(v.get("agents").unwrap_or(&serde_json::Value::Array(vec![])))
                .unwrap_or(ipc_resp)
        } else {
            ipc_resp
        }
    } else {
        ipc_resp
    };

    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    writer.write_all(header.as_bytes()).await?;
    writer.write_all(body.as_bytes()).await?;
    Ok(())
}

async fn query_ipc(port: u16, token: &str, req_json: &str) -> anyhow::Result<String> {
    let mut stream = TcpStream::connect(format!("127.0.0.1:{port}")).await?;

    // Send Hello frame.
    let hello = serde_json::json!({"cmd":"hello","token":token,"identity":"rest-api"});
    stream
        .write_all(format!("{}\n", hello).as_bytes())
        .await?;

    // Send the actual request.
    stream
        .write_all(format!("{req_json}\n").as_bytes())
        .await?;
    stream.flush().await?;

    let mut reader = BufReader::new(stream);
    // Read Hello ack (first line).
    let mut _ack = String::new();
    reader.read_line(&mut _ack).await?;

    // Read the response to our request.
    let mut resp = String::new();
    reader.read_line(&mut resp).await?;
    Ok(resp.trim().to_string())
}
