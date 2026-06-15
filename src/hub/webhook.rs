//! Fire-and-forget webhook delivery for hub events.
//!
//! Calls `fire` with a fully-constructed [`Payload`] and the hub's configured
//! URL / secret. The function spawns a tokio task and returns immediately —
//! delivery failures are logged but never propagate to the caller.
//!
//! Security: SSRF protection blocks private/loopback/link-local addresses.
//! Reliability: up to 3 attempts with exponential backoff on 5xx / timeout.

use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::Sha256;
use std::net::IpAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

/// Events the hub can deliver to the webhook endpoint.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    HubStart,
    HubStop,
    TaskDone,
    TaskBlocked,
    /// Agent claimed a task (started working on it).
    #[allow(dead_code)]
    TaskClaim,
    AgentCrash,
    /// Agent spawned or restarted.
    #[allow(dead_code)]
    AgentSpawn,
    BudgetWarning,
    /// Agent claimed one or more files.
    FileClaim,
    /// Agent released one or more files (triggers auto-commit if enabled).
    FileRelease,
    /// Fleet-wide pause activated.
    FleetPaused,
    /// Fleet-wide resume activated.
    FleetResumed,
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
    /// For BudgetWarning: USD spent so far.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spent_usd: Option<f64>,
    /// For BudgetWarning: configured max budget.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_usd: Option<f64>,
    /// For BudgetWarning: percentage of budget used (0–100).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_pct: Option<f64>,
    /// For FileClaim/FileRelease: paths involved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paths: Option<Vec<String>>,
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
            spent_usd: None,
            max_usd: None,
            budget_pct: None,
            paths: None,
        }
    }

    pub fn with_budget(mut self, spent: f64, max: f64, pct: f64) -> Self {
        self.spent_usd = Some(spent);
        self.max_usd = Some(max);
        self.budget_pct = Some(pct);
        self
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

    pub fn with_paths(mut self, paths: Vec<String>) -> Self {
        self.paths = Some(paths);
        self
    }
}

/// Return `true` if the IP is loopback, private, link-local, or otherwise
/// reserved — any range an external webhook should never target (SSRF guard).
fn is_private_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()          // 127.0.0.0/8
                || v4.is_private()    // 10/8, 172.16/12, 192.168/16
                || v4.is_link_local() // 169.254.0.0/16 — includes cloud metadata IPs
                || v4.is_unspecified() // 0.0.0.0
                || v4.is_broadcast()   // 255.255.255.255
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()          // ::1
                || v6.is_unspecified() // ::
                // link-local fe80::/10
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                // unique-local fc00::/7
                || (v6.segments()[0] & 0xfe00) == 0xfc00
        }
    }
}

/// Extract just the host portion (no port, no brackets) from an http(s) URL.
fn extract_host(url: &str) -> Option<&str> {
    let rest = url.split("://").nth(1)?;
    let host_and_port = rest.split('/').next()?;
    if host_and_port.starts_with('[') {
        // IPv6 literal: [::1]:8080
        let end = host_and_port.find(']')?;
        Some(&host_and_port[1..end])
    } else {
        Some(host_and_port.split(':').next()?)
    }
}

/// Validate that `url` is safe to deliver to:
/// - Only `http` or `https` schemes
/// - Host must not be a loopback/private/link-local name or IP (SSRF prevention)
fn validate_url(url: &str) -> Result<(), String> {
    let scheme = url.split("://").next().unwrap_or("").to_lowercase();
    match scheme.as_str() {
        "http" | "https" => {}
        _ => {
            return Err(format!(
                "webhook_url has unsupported scheme {scheme:?} — only http/https allowed"
            ))
        }
    }

    let host = extract_host(url).unwrap_or("");
    let host_lower = host.to_lowercase();

    // Block reserved hostnames regardless of what they resolve to.
    if matches!(
        host_lower.as_str(),
        "localhost" | "localhost.localdomain" | "0.0.0.0"
    ) || host_lower.ends_with(".localhost")
    {
        return Err(format!(
            "webhook_url host {host:?} is a reserved name — SSRF protection"
        ));
    }

    // If the host is a literal IP, reject private / reserved ranges.
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_private_ip(&ip) {
            return Err(format!(
                "webhook_url points to a private/reserved IP {host} — SSRF protection"
            ));
        }
    }

    Ok(())
}

/// Compute `X-Agentcom-Signature: sha256=<hex>` for the raw JSON body.
fn hmac_signature(secret: &str, body: &[u8]) -> String {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(body);
    let result = mac.finalize().into_bytes();
    result.iter().map(|b| format!("{b:02x}")).collect()
}

fn hmac_signature_header(secret: &str, body: &[u8]) -> String {
    format!("sha256={}", hmac_signature(secret, body))
}

/// Outcome of a single delivery attempt.
enum Outcome {
    /// 2xx response — done.
    Delivered,
    /// 4xx or other permanent error — log once, do not retry.
    Permanent,
    /// 5xx, timeout, or connection failure — caller should retry.
    Transient(String),
}

async fn attempt_delivery(
    client: &reqwest::Client,
    url: &str,
    body: &[u8],
    secret: &Option<String>,
    delivery_id: &str,
    event_name: &str,
) -> Outcome {
    let mut req = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("User-Agent", "agentcom-hub")
        .header("X-Agentcom-Delivery", delivery_id)
        .header("X-Agentcom-Event", event_name);

    if let Some(s) = secret {
        req = req.header("X-Agentcom-Signature", hmac_signature_header(s, body));
    }

    match req.body(body.to_vec()).send().await {
        Ok(resp) if resp.status().is_success() => Outcome::Delivered,
        Ok(resp) if resp.status().is_server_error() => {
            Outcome::Transient(format!("server error {}", resp.status().as_u16()))
        }
        Ok(resp) => {
            tracing::warn!(
                "webhook: endpoint returned {} for {} (not retrying)",
                resp.status().as_u16(),
                url
            );
            Outcome::Permanent
        }
        Err(e) if e.is_timeout() || e.is_connect() => {
            Outcome::Transient(format!("transient: {e}"))
        }
        Err(e) => {
            tracing::warn!("webhook: permanent error delivering to {url}: {e}");
            Outcome::Permanent
        }
    }
}

/// Spawn a tokio task that POSTs `payload` to `url`. Returns immediately.
///
/// Delivery is retried up to 3 times with exponential backoff (0s, 1s, 5s)
/// on 5xx responses and connection/timeout failures. 4xx and other permanent
/// failures are logged once without retry. All failures are non-fatal.
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

        let event_name = serde_json::to_value(&payload.event)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| "unknown".into());

        let delivery_id = Uuid::new_v4().to_string();

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_default();

        // Attempt 1: immediate; Attempt 2: after 1s; Attempt 3: after 5s.
        let delays = [Duration::ZERO, Duration::from_secs(1), Duration::from_secs(5)];

        for (attempt, &delay) in delays.iter().enumerate() {
            if delay > Duration::ZERO {
                tokio::time::sleep(delay).await;
            }

            match attempt_delivery(&client, &url, &body, &secret, &delivery_id, &event_name).await
            {
                Outcome::Delivered => {
                    if attempt > 0 {
                        tracing::debug!(
                            "webhook delivered on attempt {}: {} [{}]",
                            attempt + 1,
                            url,
                            delivery_id
                        );
                    } else {
                        tracing::debug!("webhook delivered: {} [{}]", url, delivery_id);
                    }
                    return;
                }
                Outcome::Permanent => return,
                Outcome::Transient(reason) if attempt + 1 < delays.len() => {
                    tracing::debug!(
                        "webhook attempt {} failed ({reason}), retrying: {}",
                        attempt + 1,
                        url
                    );
                }
                Outcome::Transient(reason) => {
                    tracing::warn!(
                        "webhook: all {} attempts failed to {} [{}]: {reason}",
                        delays.len(),
                        url,
                        delivery_id
                    );
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_url_accepts_public_https() {
        assert!(validate_url("https://example.com/webhook").is_ok());
        assert!(validate_url("https://hooks.slack.com/services/T0/B0/xxx").is_ok());
        assert!(validate_url("http://my-server.example.com:9000/hook").is_ok());
    }

    #[test]
    fn validate_url_rejects_bad_schemes() {
        assert!(validate_url("file:///etc/passwd").is_err());
        assert!(validate_url("ftp://example.com").is_err());
        assert!(validate_url("data:text/plain,hello").is_err());
    }

    #[test]
    fn validate_url_rejects_localhost_names() {
        assert!(validate_url("http://localhost/hook").is_err());
        assert!(validate_url("http://localhost:8080/hook").is_err());
        assert!(validate_url("http://localhost.localdomain/hook").is_err());
        assert!(validate_url("http://sub.localhost/hook").is_err());
        assert!(validate_url("http://0.0.0.0/hook").is_err());
    }

    #[test]
    fn validate_url_rejects_private_ips() {
        assert!(validate_url("http://127.0.0.1/hook").is_err());
        assert!(validate_url("http://10.0.0.1/hook").is_err());
        assert!(validate_url("http://192.168.1.1/hook").is_err());
        assert!(validate_url("http://172.16.0.1/hook").is_err());
        // Cloud metadata endpoint
        assert!(validate_url("http://169.254.169.254/latest/meta-data/").is_err());
        // IPv6 loopback
        assert!(validate_url("http://[::1]/hook").is_err());
        // IPv6 link-local
        assert!(validate_url("http://[fe80::1]/hook").is_err());
    }

    #[test]
    fn hmac_signature_is_deterministic() {
        let body = b"hello world";
        let s1 = hmac_signature_header("secret", body);
        let s2 = hmac_signature_header("secret", body);
        assert_eq!(s1, s2);
        assert!(s1.starts_with("sha256="));
        assert_eq!(s1.len(), 7 + 64); // "sha256=" + 64 hex chars
    }
}
