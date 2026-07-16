use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

const MAX_AUDIT_EVENTS: usize = 1_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEvent {
    pub timestamp: DateTime<Utc>,
    pub kind: String,
    pub detail: String,
}

#[derive(Debug, Default)]
pub struct AuditLog {
    events: Vec<AuditEvent>,
}

impl AuditLog {
    pub fn append(&mut self, kind: impl Into<String>, detail: impl Into<String>) -> &AuditEvent {
        self.events.push(AuditEvent {
            timestamp: Utc::now(),
            kind: kind.into(),
            detail: detail.into(),
        });
        if self.events.len() > MAX_AUDIT_EVENTS {
            let excess = self.events.len() - MAX_AUDIT_EVENTS;
            self.events.drain(0..excess);
        }
        self.events.last().expect("audit event")
    }

    pub fn events(&self) -> &[AuditEvent] {
        &self.events
    }
}
