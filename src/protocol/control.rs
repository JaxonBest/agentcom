//! Control-protocol requests (the side channel the official SDKs use to
//! interrupt a running turn).
//!
//! The interrupt subtype string is centralized here with an env override so
//! a CLI rename can be fixed at runtime without recompiling:
//! `AGENTCOM_INTERRUPT_SUBTYPE=new_name agentcom up`.

use serde_json::json;

pub const INTERRUPT_SUBTYPE_DEFAULT: &str = "interrupt";

pub fn interrupt_subtype() -> String {
    std::env::var("AGENTCOM_INTERRUPT_SUBTYPE")
        .unwrap_or_else(|_| INTERRUPT_SUBTYPE_DEFAULT.to_string())
}

pub fn interrupt_request(request_id: &str) -> String {
    json!({
        "type": "control_request",
        "request_id": request_id,
        "request": { "subtype": interrupt_subtype() }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn interrupt_request_shape() {
        let line = super::interrupt_request("req-1");
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["type"], "control_request");
        assert_eq!(v["request_id"], "req-1");
        assert_eq!(v["request"]["subtype"], "interrupt");
    }
}
