//! Append-only approval revocation evidence and the effective approval view.

use super::{
    append_journal, authentication_tag, database_error, decode_authenticated, encoded,
    read_json_row_with_hash, require_text, AlphaStore, ApprovalRecord, StoreError,
};
use alpha_domain::canonical_json_hash;
use chrono::{DateTime, Utc};
use duckdb::params;
use serde::{Deserialize, Serialize};

const SCHEMA: &str = "monday.approval_revocation.v1";
const AUTH_DOMAIN: &str = "approval-revocation";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRevocationV1 {
    pub schema_version: String,
    pub approval_id: String,
    pub approval_content_sha256: String,
    pub revoked_at: DateTime<Utc>,
    pub revoked_by: String,
    pub reason: String,
}

impl ApprovalRevocationV1 {
    pub fn content_hash(&self) -> Result<String, StoreError> {
        canonical_json_hash(self).map_err(|error| StoreError::Domain(error.to_string()))
    }

    pub fn revocation_id(&self) -> Result<String, StoreError> {
        Ok(format!("approval-revocation-{}", self.content_hash()?))
    }

    fn apply_to(
        &self,
        mut approval: ApprovalRecord,
        hash: &str,
    ) -> Result<ApprovalRecord, StoreError> {
        require_text(&self.revoked_by)?;
        require_text(&self.reason)?;
        if self.schema_version != SCHEMA
            || self.approval_id != approval.approval_id
            || self.approval_content_sha256 != hash
            || approval.revoked_at.is_some()
        {
            return Err(StoreError::Domain(
                "approval revocation binding is invalid".into(),
            ));
        }
        approval.revoked_at = Some(self.revoked_at);
        approval.revoked_by = Some(self.revoked_by.clone());
        approval.revocation_reason = Some(self.reason.clone());
        approval.validate()?;
        Ok(approval)
    }
}

impl AlphaStore {
    /// Original stored evidence, without overlaying a later revocation. Existing
    /// pre-migration rows are returned unchanged as historical snapshots.
    pub fn get_approval_evidence(
        &self,
        approval_id: &str,
    ) -> Result<(ApprovalRecord, String), StoreError> {
        read_json_row_with_hash(
            &self.connection,
            "SELECT payload_json, content_hash FROM approvals WHERE approval_id = ?",
            approval_id,
        )
    }

    pub fn get_approval_revocation(
        &self,
        approval_id: &str,
    ) -> Result<Option<ApprovalRevocationV1>, StoreError> {
        let row = self.connection.query_row(
            "SELECT revocation_id, payload_json, content_hash, auth_tag FROM approval_revocations WHERE approval_id = ?",
            params![approval_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?)),
        );
        let (id, json, hash, auth_tag) = match row {
            Ok(row) => row,
            Err(duckdb::Error::QueryReturnedNoRows) => return Ok(None),
            Err(error) => return Err(database_error(error)),
        };
        let event: ApprovalRevocationV1 = decode_authenticated(
            &self.integrity_key,
            AUTH_DOMAIN,
            &id,
            &json,
            &hash,
            &auth_tag,
        )?;
        if event.revocation_id()? != id {
            return Err(StoreError::ContentHashMismatch);
        }
        let (approval, approval_hash) = self.get_approval_evidence(approval_id)?;
        event.apply_to(approval, &approval_hash)?;
        Ok(Some(event))
    }

    /// All authorization consumers use this effective view, never raw SQL rows.
    pub fn get_approval(&self, approval_id: &str) -> Result<ApprovalRecord, StoreError> {
        let (approval, hash) = self.get_approval_evidence(approval_id)?;
        if let Some(revocation) = self.get_approval_revocation(approval_id)? {
            return revocation.apply_to(approval, &hash);
        }
        if approval.revoked_at.is_none() {
            let recorded: bool = self.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM run_journal WHERE event_kind = 'approval_revoked' AND record_id = ?)",
                params![approval_id], |row| row.get(0),
            ).map_err(database_error)?;
            if recorded {
                return Err(StoreError::Domain(
                    "recorded approval revocation evidence is missing".into(),
                ));
            }
        }
        Ok(approval)
    }

    pub fn revoke_approval(
        &mut self,
        approval_id: &str,
        revoked_by: &str,
        reason: &str,
        at: DateTime<Utc>,
    ) -> Result<ApprovalRecord, StoreError> {
        let (_, original_hash) = self.get_approval_evidence(approval_id)?;
        self.append_approval_revocation(&ApprovalRevocationV1 {
            schema_version: SCHEMA.into(),
            approval_id: approval_id.into(),
            approval_content_sha256: original_hash,
            revoked_at: at,
            revoked_by: revoked_by.into(),
            reason: reason.into(),
        })
    }

    /// Append a reviewed revocation or replay identical evidence. A conflicting
    /// second event is rejected, while original approval bytes stay unchanged.
    pub fn append_approval_revocation(
        &mut self,
        event: &ApprovalRevocationV1,
    ) -> Result<ApprovalRecord, StoreError> {
        let (approval, original_hash) = self.get_approval_evidence(&event.approval_id)?;
        let effective = event.apply_to(approval, &original_hash)?;
        if let Some(existing) = self.get_approval_revocation(&event.approval_id)? {
            if existing == *event {
                return Ok(effective);
            }
            return Err(StoreError::Domain(
                "approval already has a different revocation".into(),
            ));
        }
        // Also reject a missing revocation after a recorded event; do not repair
        // damaged history by inventing a new revocation with different bytes.
        self.get_approval(&event.approval_id)?;
        let id = event.revocation_id()?;
        let (json, hash) = encoded(event)?;
        let auth_tag = authentication_tag(&self.integrity_key, AUTH_DOMAIN, &id, &json)?;
        let tx = self.connection.transaction().map_err(database_error)?;
        tx.execute(
            "INSERT INTO approval_revocations VALUES (?, ?, ?, ?, ?)",
            params![event.approval_id, id, json, hash, auth_tag],
        )
        .map_err(database_error)?;
        append_journal(
            &tx,
            None,
            "approval_revoked",
            &event.approval_id,
            &hash,
            event.revoked_at,
        )?;
        tx.commit().map_err(database_error)?;
        Ok(effective)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeDelta;

    fn approval() -> ApprovalRecord {
        let now = DateTime::parse_from_rfc3339("2026-09-05T00:00:00Z")
            .unwrap()
            .to_utc();
        ApprovalRecord {
            approval_id: "approval-a".into(),
            approval_class: "campaign_root".into(),
            subject_id: "root-a".into(),
            payload: serde_json::json!({"grant_sha256":"a".repeat(64)}),
            signer_id: Some("operator-a".into()),
            valid_from: Some(now),
            expires_at: Some(now + TimeDelta::hours(1)),
            revoked_at: None,
            revoked_by: None,
            revocation_reason: None,
            created_at: now,
        }
    }

    #[test]
    fn revocation_is_append_only_idempotent_and_replayable() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        let approval = approval();
        store.record_approval(&approval).unwrap();
        let original = store.get_approval_evidence(&approval.approval_id).unwrap();
        let at = approval.created_at + TimeDelta::seconds(10);
        let effective = store
            .revoke_approval(&approval.approval_id, "operator-b", "cancelled", at)
            .unwrap();
        let event = store
            .get_approval_revocation(&approval.approval_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            original,
            store.get_approval_evidence(&approval.approval_id).unwrap()
        );
        assert_eq!(event.approval_content_sha256, original.1);
        assert!(effective.is_active_at(at - TimeDelta::seconds(1)));
        assert!(!effective.is_active_at(at));
        assert_eq!(store.append_approval_revocation(&event).unwrap(), effective);
        let count: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM run_journal WHERE event_kind = 'approval_revoked'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        let mut rebuilt = AlphaStore::open_in_memory().unwrap();
        rebuilt.record_approval(&approval).unwrap();
        // The caller has authenticated these exported source bytes; the new
        // store authenticates its own local copy with its own integrity key.
        rebuilt.append_approval_revocation(&event).unwrap();
        assert_eq!(
            rebuilt.get_approval(&approval.approval_id).unwrap(),
            effective
        );
        assert_eq!(
            rebuilt
                .get_approval_evidence(&approval.approval_id)
                .unwrap(),
            original
        );
    }

    #[test]
    fn revocation_survives_reopen_without_rewriting_approval() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "monday-approval-reopen-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&dir).unwrap();
        let db = dir.join("control.duckdb");
        let approval = approval();
        let original;
        {
            let mut store = AlphaStore::open(&db).unwrap();
            store.record_approval(&approval).unwrap();
            original = store.get_approval_evidence(&approval.approval_id).unwrap();
            store
                .revoke_approval(
                    &approval.approval_id,
                    "operator-b",
                    "cancelled",
                    approval.created_at,
                )
                .unwrap();
        }
        let store = AlphaStore::open(&db).unwrap();
        assert_eq!(
            store.get_approval_evidence(&approval.approval_id).unwrap(),
            original
        );
        assert!(!store
            .get_approval(&approval.approval_id)
            .unwrap()
            .is_active_at(approval.created_at));
        assert!(store
            .get_approval_revocation(&approval.approval_id)
            .unwrap()
            .is_some());
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn conflicting_or_backdated_revocation_is_rejected() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        let approval = approval();
        store.record_approval(&approval).unwrap();
        assert!(store
            .revoke_approval(
                &approval.approval_id,
                "operator-b",
                "cancelled",
                approval.created_at - TimeDelta::seconds(1)
            )
            .is_err());
        assert!(store
            .get_approval_revocation(&approval.approval_id)
            .unwrap()
            .is_none());
        store
            .revoke_approval(
                &approval.approval_id,
                "operator-b",
                "cancelled",
                approval.created_at,
            )
            .unwrap();
        let original = store
            .get_approval_revocation(&approval.approval_id)
            .unwrap();
        assert!(store
            .revoke_approval(
                &approval.approval_id,
                "operator-c",
                "different reason",
                approval.created_at
            )
            .is_err());
        assert_eq!(
            store
                .get_approval_revocation(&approval.approval_id)
                .unwrap(),
            original
        );
    }

    #[test]
    fn changed_revocation_cannot_be_hidden_by_rehashing_json() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        let approval = approval();
        store.record_approval(&approval).unwrap();
        store
            .revoke_approval(
                &approval.approval_id,
                "operator-b",
                "cancelled",
                approval.created_at,
            )
            .unwrap();
        let mut event = store
            .get_approval_revocation(&approval.approval_id)
            .unwrap()
            .unwrap();
        event.revoked_at += TimeDelta::hours(1);
        let (json, hash) = encoded(&event).unwrap();
        store.connection.execute("UPDATE approval_revocations SET payload_json = ?, content_hash = ? WHERE approval_id = ?", params![json, hash, approval.approval_id]).unwrap();
        assert!(matches!(
            store.get_approval(&approval.approval_id),
            Err(StoreError::AuthenticityMismatch)
        ));
    }

    #[test]
    fn missing_revocation_evidence_cannot_reactivate_approval() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        let approval = approval();
        store.record_approval(&approval).unwrap();
        store
            .revoke_approval(
                &approval.approval_id,
                "operator-b",
                "cancelled",
                approval.created_at,
            )
            .unwrap();
        // Corruption fixture: production has no revocation DELETE path.
        store
            .connection
            .execute(
                "DELETE FROM approval_revocations WHERE approval_id = ?",
                params![approval.approval_id],
            )
            .unwrap();
        assert!(store.get_approval(&approval.approval_id).is_err());
        assert!(store
            .revoke_approval(
                &approval.approval_id,
                "operator-b",
                "replacement",
                approval.created_at
            )
            .is_err());
    }

    #[test]
    fn legacy_revoked_snapshot_remains_read_only() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        let mut approval = approval();
        store.record_approval(&approval).unwrap();
        approval.revoked_at = Some(approval.created_at);
        approval.revoked_by = Some("legacy-operator".into());
        approval.revocation_reason = Some("legacy snapshot".into());
        let (json, hash) = encoded(&approval).unwrap();
        // Simulate an already-applied old database, not a new production write.
        store
            .connection
            .execute(
                "UPDATE approvals SET payload_json = ?, content_hash = ? WHERE approval_id = ?",
                params![json, hash, approval.approval_id],
            )
            .unwrap();
        assert_eq!(store.get_approval(&approval.approval_id).unwrap(), approval);
        assert!(!store
            .get_approval(&approval.approval_id)
            .unwrap()
            .is_active_at(approval.created_at));
        assert!(store
            .revoke_approval(
                &approval.approval_id,
                "operator-b",
                "replacement",
                approval.created_at
            )
            .is_err());
        assert!(store
            .get_approval_revocation(&approval.approval_id)
            .unwrap()
            .is_none());
        assert_eq!(
            store
                .get_approval_evidence(&approval.approval_id)
                .unwrap()
                .0,
            approval
        );
    }
}
