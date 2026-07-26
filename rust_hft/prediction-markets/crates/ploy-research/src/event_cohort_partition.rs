use std::collections::BTreeSet;

use ploy_market_data::polymarket_evidence::{
    PolymarketCatalogReceipt, PolymarketCatalogReceiptState, PolymarketReadyEventCatalog,
};
use serde::Serialize;

use crate::{
    prediction_loop::{current_prediction_policy_snapshot_id, validate_sha256_id},
    prediction_loop_fs::{canonical_json_bytes, sha256_hex},
};

pub const EVENT_COHORT_PARTITION_VERSION: &str = "event_cohort_partition.v3";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EventCohortReadyEntry {
    receipt_sha256: String,
    market_id: String,
    reference_path_start_ms: i64,
    reference_path_end_ms: i64,
    settlement_available_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EventCohortPartition {
    schema_version: &'static str,
    ready_entries: Vec<EventCohortReadyEntry>,
    common_time_boundary_ms: i64,
    label_availability_cutoff_ms: i64,
    causal_projection_policy_id: String,
    train_market_ids: Vec<String>,
    crossing_excluded_market_ids: Vec<String>,
    held_out_market_ids: Vec<String>,
    digest: String,
}

#[derive(Serialize)]
struct EventCohortPartitionPayload<'a> {
    schema_version: &'static str,
    ready_entries: &'a [EventCohortReadyEntry],
    common_time_boundary_ms: i64,
    label_availability_cutoff_ms: i64,
    causal_projection_policy_id: &'a str,
    train_market_ids: &'a [String],
    crossing_excluded_market_ids: &'a [String],
    held_out_market_ids: &'a [String],
}

impl EventCohortPartition {
    /// Derive the common partition from #319's complete ordered Ready catalog.
    /// No downstream snapshot enters this identity.
    pub fn from_ready_catalog(
        catalog: &PolymarketReadyEventCatalog,
        common_time_boundary_ms: i64,
    ) -> Result<Self, String> {
        Self::from_ready_entries(
            catalog
                .receipts()
                .filter(|receipt| receipt.state == PolymarketCatalogReceiptState::Ready),
            common_time_boundary_ms,
        )
    }

    fn from_ready_entries<'a>(
        ready_entries: impl IntoIterator<Item = &'a PolymarketCatalogReceipt>,
        common_time_boundary_ms: i64,
    ) -> Result<Self, String> {
        if common_time_boundary_ms <= 0 {
            return Err(
                "common time boundary must be a positive Unix millisecond timestamp".into(),
            );
        }
        let causal_projection_policy_id = current_prediction_policy_snapshot_id();
        validate_sha256_id(
            &causal_projection_policy_id,
            "causal projection policy identity",
        )?;

        let ready_entries = ready_entries
            .into_iter()
            .map(EventCohortReadyEntry::from_receipt)
            .collect::<Result<Vec<_>, _>>()?;

        let mut receipt_ids = BTreeSet::new();
        let mut market_ids = BTreeSet::new();
        let mut train_market_ids = Vec::new();
        let mut crossing_excluded_market_ids = Vec::new();
        let mut held_out_market_ids = Vec::new();
        for entry in &ready_entries {
            if !receipt_ids.insert(&entry.receipt_sha256) {
                return Err(format!(
                    "ready catalog contains duplicate receipt_sha256 {}",
                    entry.receipt_sha256
                ));
            }
            if !market_ids.insert(&entry.market_id) {
                return Err(format!(
                    "ready catalog contains duplicate market_id {}",
                    entry.market_id
                ));
            }

            if entry.reference_path_end_ms < common_time_boundary_ms {
                train_market_ids.push(entry.market_id.clone());
            } else if entry.reference_path_start_ms >= common_time_boundary_ms {
                held_out_market_ids.push(entry.market_id.clone());
            } else {
                crossing_excluded_market_ids.push(entry.market_id.clone());
            }
        }
        let label_availability_cutoff_ms = ready_entries
            .iter()
            .filter(|entry| entry.reference_path_start_ms >= common_time_boundary_ms)
            .map(|entry| entry.reference_path_start_ms)
            .min()
            .map(|decision| decision.min(common_time_boundary_ms))
            .unwrap_or(common_time_boundary_ms);
        for entry in &ready_entries {
            if entry.reference_path_end_ms < common_time_boundary_ms
                && entry.settlement_available_at_ms > label_availability_cutoff_ms
            {
                return Err(format!(
                    "ready market {} settlement label unavailable by cutoff {}",
                    entry.market_id, label_availability_cutoff_ms
                ));
            }
        }

        let payload = EventCohortPartitionPayload {
            schema_version: EVENT_COHORT_PARTITION_VERSION,
            ready_entries: &ready_entries,
            common_time_boundary_ms,
            label_availability_cutoff_ms,
            causal_projection_policy_id: &causal_projection_policy_id,
            train_market_ids: &train_market_ids,
            crossing_excluded_market_ids: &crossing_excluded_market_ids,
            held_out_market_ids: &held_out_market_ids,
        };
        let digest = format!("sha256:{}", sha256_hex(&canonical_json_bytes(&payload)?));

        Ok(Self {
            schema_version: EVENT_COHORT_PARTITION_VERSION,
            ready_entries,
            common_time_boundary_ms,
            label_availability_cutoff_ms,
            causal_projection_policy_id,
            train_market_ids,
            crossing_excluded_market_ids,
            held_out_market_ids,
            digest,
        })
    }

    pub fn common_time_boundary_ms(&self) -> i64 {
        self.common_time_boundary_ms
    }

    pub fn ready_entries(&self) -> &[EventCohortReadyEntry] {
        &self.ready_entries
    }

    pub fn label_availability_cutoff_ms(&self) -> i64 {
        self.label_availability_cutoff_ms
    }

    pub fn causal_projection_policy_id(&self) -> &str {
        &self.causal_projection_policy_id
    }

    pub fn train_market_ids(&self) -> &[String] {
        &self.train_market_ids
    }

    pub fn crossing_excluded_market_ids(&self) -> &[String] {
        &self.crossing_excluded_market_ids
    }

    pub fn held_out_market_ids(&self) -> &[String] {
        &self.held_out_market_ids
    }

    pub fn digest(&self) -> &str {
        &self.digest
    }
}

impl EventCohortReadyEntry {
    pub fn receipt_sha256(&self) -> &str {
        &self.receipt_sha256
    }

    pub fn market_id(&self) -> &str {
        &self.market_id
    }

    pub fn reference_path_start_ms(&self) -> i64 {
        self.reference_path_start_ms
    }

    pub fn reference_path_end_ms(&self) -> i64 {
        self.reference_path_end_ms
    }

    pub fn settlement_available_at_ms(&self) -> i64 {
        self.settlement_available_at_ms
    }

    fn from_receipt(receipt: &PolymarketCatalogReceipt) -> Result<Self, String> {
        if receipt.state != PolymarketCatalogReceiptState::Ready {
            return Err(format!(
                "catalog receipt {} is not Ready",
                receipt.receipt_sha256
            ));
        }
        validate_lower_sha256(&receipt.receipt_sha256, "catalog receipt_sha256")?;
        validate_market_id(&receipt.market_id)?;
        let reference_path_start_ms = receipt
            .event_start
            .ok_or_else(|| format!("ready market {} is missing event_start", receipt.market_id))?
            .timestamp_millis();
        let reference_path_end_ms = receipt
            .event_end
            .ok_or_else(|| format!("ready market {} is missing event_end", receipt.market_id))?
            .timestamp_millis();
        if reference_path_start_ms >= reference_path_end_ms {
            return Err(format!(
                "ready market {} has an invalid reference path",
                receipt.market_id
            ));
        }
        let settlement_available_at_ms = receipt
            .availability
            .as_ref()
            .and_then(|availability| availability.settlement)
            .ok_or_else(|| {
                format!(
                    "ready market {} is missing settlement label availability",
                    receipt.market_id
                )
            })?
            .timestamp_millis();
        if settlement_available_at_ms < reference_path_end_ms {
            return Err(format!(
                "ready market {} settlement label predates event end",
                receipt.market_id
            ));
        }
        Ok(Self {
            receipt_sha256: receipt.receipt_sha256.clone(),
            market_id: receipt.market_id.clone(),
            reference_path_start_ms,
            reference_path_end_ms,
            settlement_available_at_ms,
        })
    }
}

fn validate_lower_sha256(value: &str, field: &str) -> Result<(), String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(format!(
            "{field} must be 64 lowercase hexadecimal characters"
        ));
    }
    Ok(())
}

fn validate_market_id(market_id: &str) -> Result<(), String> {
    if market_id.trim().is_empty() || market_id.trim() != market_id {
        return Err("ready catalog market_id must be a trimmed non-empty string".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use ploy_market_data::polymarket_evidence::{
        PolymarketCatalogReceipt, PolymarketCatalogReceiptState, PolymarketCatalogVerifier,
        PolymarketEvidenceAvailability, PolymarketReadyEventCatalog, PolymarketResearchTask,
    };

    use super::EventCohortPartition;

    fn ready_receipt(
        receipt_sha256: char,
        market_id: &str,
        reference_path_start_ms: i64,
        reference_path_end_ms: i64,
    ) -> PolymarketCatalogReceipt {
        PolymarketCatalogReceipt {
            receipt_sha256: receipt_sha256.to_string().repeat(64),
            market_id: market_id.to_owned(),
            content_sha256: "b".repeat(64),
            manifest_sha256: "c".repeat(64),
            qualification_sha256: "d".repeat(64),
            success_sha256: Some("e".repeat(64)),
            verifier: PolymarketCatalogVerifier::new(
                "f".repeat(64),
                "a".repeat(64),
                "b".repeat(64),
                "c".repeat(64),
            )
            .unwrap(),
            event_start: Utc.timestamp_millis_opt(reference_path_start_ms).single(),
            event_end: Utc.timestamp_millis_opt(reference_path_end_ms).single(),
            up_token_id: Some("up".to_owned()),
            down_token_id: Some("down".to_owned()),
            sequence: None,
            coverage: None,
            trade_completion: None,
            availability: Some(PolymarketEvidenceAvailability {
                contract: None,
                books: None,
                references: None,
                trades: None,
                settlement: Utc.timestamp_millis_opt(reference_path_end_ms + 1).single(),
            }),
            state: PolymarketCatalogReceiptState::Ready,
            reasons: Vec::new(),
            supported_tasks: vec![PolymarketResearchTask::Btc5mBacktest],
        }
    }

    #[test]
    fn ready_catalog_assigns_each_market_once_and_excludes_crossing_reference_paths() {
        let held_out = ready_receipt('a', "held-out", 2_000, 2_500);
        let crossing = ready_receipt('b', "crossing", 900, 1_100);
        let train = ready_receipt('c', "train", 100, 999);

        let partition =
            EventCohortPartition::from_ready_entries([&held_out, &crossing, &train], 1_000)
                .unwrap();

        assert_eq!(partition.train_market_ids(), ["train"]);
        assert_eq!(partition.crossing_excluded_market_ids(), ["crossing"]);
        assert_eq!(partition.held_out_market_ids(), ["held-out"]);
    }

    #[test]
    fn digest_binds_catalog_identity_and_reference_path_in_catalog_order() {
        let first = ready_receipt('a', "first", 100, 999);
        let second = ready_receipt('b', "second", 2_000, 2_500);
        let partition = EventCohortPartition::from_ready_entries([&first, &second], 1_000).unwrap();
        let reordered = EventCohortPartition::from_ready_entries([&second, &first], 1_000).unwrap();

        let different_identity = ready_receipt('c', "first", 100, 999);
        let changed_identity =
            EventCohortPartition::from_ready_entries([&different_identity, &second], 1_000)
                .unwrap();
        let changed_window = ready_receipt('a', "first", 100, 998);
        let changed_path =
            EventCohortPartition::from_ready_entries([&changed_window, &second], 1_000).unwrap();
        let mut changed_availability = ready_receipt('a', "first", 100, 999);
        changed_availability
            .availability
            .as_mut()
            .unwrap()
            .settlement = Utc.timestamp_millis_opt(999).single();
        let changed_label_availability =
            EventCohortPartition::from_ready_entries([&changed_availability, &second], 1_000)
                .unwrap();
        let missing_member = EventCohortPartition::from_ready_entries([&first], 1_000).unwrap();

        assert_ne!(partition.digest(), reordered.digest());
        assert_ne!(partition.digest(), changed_identity.digest());
        assert_ne!(partition.digest(), changed_path.digest());
        assert_ne!(partition.digest(), changed_label_availability.digest());
        assert_ne!(partition.digest(), missing_member.digest());
    }

    #[test]
    fn duplicate_market_and_missing_window_fail_closed() {
        let first = ready_receipt('a', "same-market", 100, 999);
        let duplicate = ready_receipt('b', "same-market", 2_000, 2_500);
        let duplicate_error = EventCohortPartition::from_ready_entries([&first, &duplicate], 1_000)
            .expect_err("one market cannot be assigned to more than one cohort");
        assert!(duplicate_error.contains("duplicate market_id same-market"));

        let mut missing_window = ready_receipt('c', "missing-window", 2_000, 2_500);
        missing_window.event_end = None;
        let missing_window_error =
            EventCohortPartition::from_ready_entries([&missing_window], 1_000)
                .expect_err("catalog entry without its reference path is not usable");
        assert!(missing_window_error.contains("missing event_end"));
    }

    #[test]
    fn boundary_touching_path_is_excluded_and_artifact_has_no_snapshot_identity() {
        let ends_at_boundary = ready_receipt('a', "ends-at-boundary", 500, 1_000);
        let starts_at_boundary = ready_receipt('b', "starts-at-boundary", 1_000, 1_500);
        let partition = EventCohortPartition::from_ready_entries(
            [&ends_at_boundary, &starts_at_boundary],
            1_000,
        )
        .unwrap();

        assert_eq!(
            partition.crossing_excluded_market_ids(),
            ["ends-at-boundary"]
        );
        assert_eq!(partition.held_out_market_ids(), ["starts-at-boundary"]);
        assert!(serde_json::to_value(&partition)
            .unwrap()
            .get("authenticated_snapshot_digest")
            .is_none());
    }

    #[test]
    fn public_catalog_seam_is_task_agnostic_and_uses_the_complete_ready_projection() {
        let catalog = PolymarketReadyEventCatalog::default();

        let partition = EventCohortPartition::from_ready_catalog(&catalog, 1_000).unwrap();

        assert!(partition.ready_entries().is_empty());
        assert_eq!(partition.common_time_boundary_ms(), 1_000);
    }

    #[test]
    fn training_label_unavailable_by_the_cutoff_rejects_the_partition() {
        let mut train = ready_receipt('a', "late-label", 100, 999);
        train.availability = Some(PolymarketEvidenceAvailability {
            contract: None,
            books: None,
            references: None,
            trades: None,
            settlement: Utc.timestamp_millis_opt(1_001).single(),
        });

        let error = EventCohortPartition::from_ready_entries([&train], 1_000)
            .expect_err("a training label observed after the cutoff must fail closed");

        assert!(error.contains("settlement label unavailable by cutoff"));
    }

    #[test]
    fn artifact_serialization_binds_the_causal_projection_policy_identity() {
        let train = ready_receipt('a', "train", 100, 999);
        let partition = EventCohortPartition::from_ready_entries([&train], 1_000).unwrap();

        assert!(serde_json::to_value(&partition)
            .unwrap()
            .get("causal_projection_policy_id")
            .is_some());
        assert_eq!(
            partition.causal_projection_policy_id(),
            crate::prediction_loop::current_prediction_policy_snapshot_id()
        );
    }
}
