use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuditLogEntry {
    pub timestamp: DateTime<Utc>,
    pub method: String,
    pub path: String,
    pub client_addr: Option<String>,
    pub auth_level: String,
    pub required_access: String,
    pub status_code: u16,
    pub outcome: String,
    pub message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::AuditLogEntry;
    use chrono::Utc;
    use serde_json::json;

    #[test]
    fn audit_log_entry_uses_stable_wire_keys() {
        let value = serde_json::to_value(AuditLogEntry {
            timestamp: Utc::now(),
            method: "POST".to_string(),
            path: "/api/deployments/example.paper/control".to_string(),
            client_addr: Some("127.0.0.1:1234".to_string()),
            auth_level: "admin".to_string(),
            required_access: "admin".to_string(),
            status_code: 200,
            outcome: "allowed".to_string(),
            message: Some("deployment paused".to_string()),
        })
        .expect("to_value");

        let object = value.as_object().expect("audit log object");
        assert_eq!(object.len(), 9);
        assert_eq!(object.get("method"), Some(&json!("POST")));
        assert_eq!(
            object.get("path"),
            Some(&json!("/api/deployments/example.paper/control"))
        );
        assert_eq!(object.get("client_addr"), Some(&json!("127.0.0.1:1234")));
        assert_eq!(object.get("auth_level"), Some(&json!("admin")));
        assert_eq!(object.get("required_access"), Some(&json!("admin")));
        assert_eq!(object.get("status_code"), Some(&json!(200)));
        assert_eq!(object.get("outcome"), Some(&json!("allowed")));
        assert_eq!(object.get("message"), Some(&json!("deployment paused")));
    }
}
