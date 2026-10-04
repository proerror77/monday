//! Contract for a future Codex app-server Session transport, not a second agent
//! runner. No model calls, daemon startup or implicit workspace access here.
use crate::{sha256, valid_digest};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: &str = "codex-app-server/0.159.2";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    NotSent,
    Rejected,
    Accepted,
    Unknown,
}
impl Delivery {
    pub fn may_resubmit(self) -> bool {
        matches!(self, Self::NotSent | Self::Rejected)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum RpcId {
    Number(i64),
    String(String),
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalKind {
    Command,
    FileChange,
    UserInput,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PendingApproval {
    pub process_generation: String,
    pub original_rpc_id: RpcId,
    pub kind: ApprovalKind,
}
impl PendingApproval {
    /// A restarted child may reuse RPC IDs. Replies must remain bound to the
    /// original process generation; user-input and approval schemas differ.
    pub fn reply(
        &self,
        generation: &str,
        id: &RpcId,
        kind: ApprovalKind,
        response: serde_json::Value,
    ) -> Result<serde_json::Value> {
        ensure!(
            valid_digest(generation)
                && generation == self.process_generation
                && id == &self.original_rpc_id
                && kind == self.kind,
            "stale/foreign approval response"
        );
        let object = response
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("invalid approval response"))?;
        match self.kind {
            ApprovalKind::Command | ApprovalKind::FileChange => {
                ensure!(
                    object.len() == 1
                        && matches!(
                            object.get("decision").and_then(|v| v.as_str()),
                            Some("accept" | "decline" | "cancel")
                        ),
                    "unsupported approval decision schema"
                );
            }
            ApprovalKind::UserInput => {
                ensure!(object.len() == 1, "unexpected user response fields");
                let answers = object
                    .get("answers")
                    .and_then(|v| v.as_object())
                    .ok_or_else(|| anyhow::anyhow!("missing user answers"))?;
                ensure!(
                    !answers.is_empty()
                        && answers.len() <= 16
                        && answers.iter().all(|(id, value)| !id.is_empty()
                            && id.len() <= 128
                            && value.as_object().is_some_and(|o| o.len() == 1
                                && o.get("answers")
                                    .and_then(|v| v.as_array())
                                    .is_some_and(|v| v.len() <= 16
                                        && v.iter().all(|s| s
                                            .as_str()
                                            .is_some_and(|s| s.len() <= 4096))))),
                    "invalid user answers schema"
                );
            }
        }
        Ok(serde_json::json!({"id":self.original_rpc_id,"result":response}))
    }
}
/// PG thread metadata cannot replace the provider's native thread state. The
/// transport must read back the persisted native-state manifest before resume.
pub fn admit_resume(
    snapshot: &crate::research::SessionSnapshot,
    expected_snapshot: &str,
    native_manifest: Option<&[u8]>,
) -> Result<()> {
    ensure!(
        snapshot.id()? == expected_snapshot,
        "session snapshot identity changed"
    );
    let native = native_manifest
        .ok_or_else(|| anyhow::anyhow!("native session state missing; resume blocked"))?;
    ensure!(
        sha256(native) == snapshot.native_state_manifest_sha256,
        "native session state changed"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_delivery_requires_reconciliation_and_old_process_cannot_approve() {
        assert!(Delivery::NotSent.may_resubmit());
        assert!(Delivery::Rejected.may_resubmit());
        assert!(!Delivery::Accepted.may_resubmit());
        assert!(!Delivery::Unknown.may_resubmit());
        let pending = PendingApproval {
            process_generation: "a".repeat(64),
            original_rpc_id: RpcId::Number(7),
            kind: ApprovalKind::Command,
        };
        let decision = serde_json::json!({"decision":"decline"});
        assert!(pending
            .reply(
                &"b".repeat(64),
                &RpcId::Number(7),
                ApprovalKind::Command,
                decision.clone()
            )
            .is_err());
        assert!(pending
            .reply(
                &"a".repeat(64),
                &RpcId::Number(8),
                ApprovalKind::Command,
                decision.clone()
            )
            .is_err());
        assert!(pending
            .reply(
                &"a".repeat(64),
                &RpcId::Number(7),
                ApprovalKind::UserInput,
                decision.clone()
            )
            .is_err());
        assert_eq!(
            pending
                .reply(
                    &"a".repeat(64),
                    &RpcId::Number(7),
                    ApprovalKind::Command,
                    decision
                )
                .unwrap()["id"],
            7
        );
    }
    #[test]
    fn resume_requires_native_state_not_only_thread_id() {
        let native = b"fixture native-state manifest";
        let snapshot = crate::research::SessionSnapshot {
            session_sha256: "a".repeat(64),
            parent_snapshot_sha256: None,
            code_commit: "b".repeat(40),
            workspace_manifest_sha256: "c".repeat(64),
            transcript_manifest_sha256: "d".repeat(64),
            native_state_manifest_sha256: sha256(native),
        };
        let id = snapshot.id().unwrap();
        assert!(admit_resume(&snapshot, &id, None).is_err());
        assert!(admit_resume(&snapshot, &id, Some(b"different")).is_err());
        admit_resume(&snapshot, &id, Some(native)).unwrap();
    }
}
