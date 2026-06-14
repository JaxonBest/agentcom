//! Fire-and-forget webhook delivery for hub events.
//!
//! Call `fire` with a fully-constructed [`Payload`] and the hub's configured
//! URL / secret. The function spawns a tokio task and returns immediately —
//! delivery failures are logged but never propagate to the caller.

use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::Sha256;
use std::time::{SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

/// Events the hub can deliver to the webhook endpoint.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    HubStart,
    HubStop,
    TaskDone,
    TaskBlocked,
    AgentCrash,
}

/// JSON body sent to the webhook endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct Payload {
    pub event: Event,
    pub timestamp: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_title: Option<String>,
}

impl Payload {
    pub fn new(event: Event) -> Self {
        Self {
            event,
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            agent: None,
            task_id: None,
            task_title: None,
        }
    }

    pub fn with_agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = Some(agent.into());
        self
    }

    pub fn with_task(mut self, id: i64, title: impl Into<String>) -> Self {
        self.task_id = Some(id);
        self.task_title = Some(title.into());
        self
    }
}

/// Validate that `url` has an http or https scheme and is not a file/data/etc
/// URI. Returns an error string suitable for logging.
fn validate_url(url: &str) -> Result<(), String> {
    let scheme = url.split("://").next().unwrap_or("").to_lowercase();
    match scheme.as_str() {
        "http" | "https" => Ok(()),
        _ => Err(format!(
            "webhook_url has unsupported scheme {scheme:?} — only http/https allowed"
        )),
    }
}

/// Compute `X-Agentcom-Signature: sha256=<hex>` for the raw JSON body.
fn hmac_signature(secret: &str, body: &[u8]) -> String {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(body);
    let result = mac.finalize().into_bytes();
    let hex: String = result.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256={hex}")
}

/// Spawn a tokio task that POSTs `payload` to `url`. Returns immediately.
/// Delivery failures are only logged — they never surface to the caller.
pub fn fire(url: String, secret: Option<String>, payload: Payload) {
    tokio::spawn(async move {
        if let Err(e) = validate_url(&url) {
            tracing::warn!("webhook skipped: {e}");
            return;
        }

        let body = match serde_json::to_vec(&payload) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("webhook: failed to serialize payload: {e}");
                return;
            }
        };

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default();

        let mut req = client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("User-Agent", "agentcom-hub");

        if let Some(ref s) = secret {
            req = req.header("X-Agentcom-Signature", hmac_signature(s, &body));
        }

        let req = req.body(body);

        match req.send().await {
            Ok(resp) if resp.status().is_success() => {
                tracing::debug!(
                    "webhook delivered: {} -> {}",
                    url,
                    resp.status().as_u16()
                );
            }
            Ok(resp) => {
                tracing::warn!(
                    "webhook: endpoint returned {}: {}",
                    resp.status().as_u16(),
                    url
                );
            }
            Err(e) => {
                tracing::warn!("webhook: delivery failed to {url}: {e}");
            }
        }
    });
}
