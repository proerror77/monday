use std::{collections::BTreeSet, path::Path};

use ploy_market_data::polymarket_evidence::{
    PolymarketCatalogReceipt, PolymarketCatalogReceiptState, PolymarketReadyEventCatalog,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    prediction_loop::{current_prediction_policy_snapshot_id, validate_sha256_id},
    prediction_loop_fs::{
        canonical_json_bytes, read_verified_artifact_bounded, relative_path, sha256_hex,
        write_content_addressed_json, ArtifactRef,
    },
};

pub const EVENT_COHORT_PARTITION_VERSION: &str = "event_cohort_partition.v3";
pub const CATALOG_PARTITION_ARTIFACT_VERSION: &str = "catalog_partition_artifact.v1";
const MAX_CATALOG_PARTITION_ARTIFACT_BYTES: usize = 8 * 1024 * 1024;
const MAX_CATALOG_PARTITION_ARTIFACT_PATH_BYTES: usize = 1_024;
const MAX_READY_CATALOG_ENTRIES: usize = 512;
const MAX_READY_CATALOG_TEXT_BYTES: usize = 2_048;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventCohortReadyEntry {
    receipt_sha256: String,
    market_id: String,
    reference_path_start_ms: i64,
    reference_path_end_ms: i64,
    settlement_available_at_ms: i64,
}

/// Immutable path-and-digest reference to the canonical artifact. The path is
/// only a locator; readback verifies that its filename and bytes match this ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogPartitionArtifactRef {
    path: String,
    artifact_sha256: String,
    payload_sha256: String,
}

impl CatalogPartitionArtifactRef {
    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn artifact_sha256(&self) -> &str {
        &self.artifact_sha256
    }

    pub fn payload_sha256(&self) -> &str {
        &self.payload_sha256
    }
}

/// The only successful #365 readback. Its fields have already passed fresh
/// canonical byte, receipt, partition, policy, and membership validation.
#[derive(Debug)]
pub struct ValidatedCatalogPartitionArtifact {
    catalog: PolymarketReadyEventCatalog,
    partition: EventCohortPartition,
}

impl ValidatedCatalogPartitionArtifact {
    pub fn catalog(&self) -> &PolymarketReadyEventCatalog {
        &self.catalog
    }

    pub fn partition(&self) -> &EventCohortPartition {
        &self.partition
    }
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

#[derive(Serialize)]
struct CatalogPartitionArtifactPayload<'a> {
    schema_version: &'static str,
    policy_snapshot_id: &'a str,
    catalog_receipts: Vec<&'a PolymarketCatalogReceipt>,
    partition: PersistedEventCohortPartition,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogPartitionArtifactEnvelope {
    schema_version: String,
    payload_sha256: String,
    payload: PersistedCatalogPartitionArtifactPayload,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedCatalogPartitionArtifactPayload {
    schema_version: String,
    policy_snapshot_id: String,
    catalog_receipts: Vec<Value>,
    partition: PersistedEventCohortPartition,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedEventCohortPartition {
    schema_version: String,
    ready_entries: Vec<EventCohortReadyEntry>,
    common_time_boundary_ms: i64,
    label_availability_cutoff_ms: i64,
    causal_projection_policy_id: String,
    train_market_ids: Vec<String>,
    crossing_excluded_market_ids: Vec<String>,
    held_out_market_ids: Vec<String>,
    digest: String,
}

impl From<&EventCohortPartition> for PersistedEventCohortPartition {
    fn from(value: &EventCohortPartition) -> Self {
        Self {
            schema_version: value.schema_version.to_string(),
            ready_entries: value.ready_entries.clone(),
            common_time_boundary_ms: value.common_time_boundary_ms,
            label_availability_cutoff_ms: value.label_availability_cutoff_ms,
            causal_projection_policy_id: value.causal_projection_policy_id.clone(),
            train_market_ids: value.train_market_ids.clone(),
            crossing_excluded_market_ids: value.crossing_excluded_market_ids.clone(),
            held_out_market_ids: value.held_out_market_ids.clone(),
            digest: value.digest.clone(),
        }
    }
}

impl PersistedEventCohortPartition {
    fn into_partition(self) -> Result<EventCohortPartition, String> {
        if self.schema_version != EVENT_COHORT_PARTITION_VERSION {
            return Err(format!(
                "unsupported event cohort partition schema {}",
                self.schema_version
            ));
        }
        if self.common_time_boundary_ms <= 0 {
            return Err("common time boundary must be positive".into());
        }
        validate_sha256_id(
            &self.causal_projection_policy_id,
            "causal projection policy identity",
        )?;
        if self.causal_projection_policy_id != current_prediction_policy_snapshot_id() {
            return Err("persisted partition has stale causal projection policy identity".into());
        }
        validate_sha256_id(&self.digest, "event cohort partition digest")?;
        let mut receipt_ids = BTreeSet::new();
        let mut market_ids = BTreeSet::new();
        let mut expected_train = Vec::new();
        let mut expected_crossing = Vec::new();
        let mut expected_held_out = Vec::new();
        for entry in &self.ready_entries {
            validate_lower_sha256(&entry.receipt_sha256, "catalog receipt_sha256")?;
            validate_market_id(&entry.market_id)?;
            if entry.reference_path_start_ms >= entry.reference_path_end_ms {
                return Err(format!(
                    "ready market {} has an invalid reference path",
                    entry.market_id
                ));
            }
            if entry.settlement_available_at_ms < entry.reference_path_end_ms {
                return Err(format!(
                    "ready market {} settlement label predates event end",
                    entry.market_id
                ));
            }
            if !receipt_ids.insert(&entry.receipt_sha256) {
                return Err(format!(
                    "persisted partition contains duplicate receipt_sha256 {}",
                    entry.receipt_sha256
                ));
            }
            if !market_ids.insert(&entry.market_id) {
                return Err(format!(
                    "persisted partition contains duplicate market_id {}",
                    entry.market_id
                ));
            }
            if entry.reference_path_end_ms < self.common_time_boundary_ms {
                expected_train.push(entry.market_id.clone());
            } else if entry.reference_path_start_ms >= self.common_time_boundary_ms {
                expected_held_out.push(entry.market_id.clone());
            } else {
                expected_crossing.push(entry.market_id.clone());
            }
        }
        if self.train_market_ids != expected_train
            || self.crossing_excluded_market_ids != expected_crossing
            || self.held_out_market_ids != expected_held_out
        {
            return Err("persisted partition assignments do not match its ready entries".into());
        }
        let expected_label_cutoff = self
            .ready_entries
            .iter()
            .filter(|entry| entry.reference_path_start_ms >= self.common_time_boundary_ms)
            .map(|entry| entry.reference_path_start_ms)
            .min()
            .unwrap_or(self.common_time_boundary_ms);
        if self.label_availability_cutoff_ms != expected_label_cutoff {
            return Err("persisted partition label availability cutoff is invalid".into());
        }
        if self.ready_entries.iter().any(|entry| {
            entry.reference_path_end_ms < self.common_time_boundary_ms
                && entry.settlement_available_at_ms >= expected_label_cutoff
        }) {
            return Err(
                "persisted partition includes a training label unavailable by cutoff".into(),
            );
        }
        let payload = EventCohortPartitionPayload {
            schema_version: EVENT_COHORT_PARTITION_VERSION,
            ready_entries: &self.ready_entries,
            common_time_boundary_ms: self.common_time_boundary_ms,
            label_availability_cutoff_ms: self.label_availability_cutoff_ms,
            causal_projection_policy_id: &self.causal_projection_policy_id,
            train_market_ids: &self.train_market_ids,
            crossing_excluded_market_ids: &self.crossing_excluded_market_ids,
            held_out_market_ids: &self.held_out_market_ids,
        };
        let digest = format!("sha256:{}", sha256_hex(&canonical_json_bytes(&payload)?));
        if digest != self.digest {
            return Err("persisted partition digest does not match canonical content".into());
        }
        Ok(EventCohortPartition {
            schema_version: EVENT_COHORT_PARTITION_VERSION,
            ready_entries: self.ready_entries,
            common_time_boundary_ms: self.common_time_boundary_ms,
            label_availability_cutoff_ms: self.label_availability_cutoff_ms,
            causal_projection_policy_id: self.causal_projection_policy_id,
            train_market_ids: self.train_market_ids,
            crossing_excluded_market_ids: self.crossing_excluded_market_ids,
            held_out_market_ids: self.held_out_market_ids,
            digest: self.digest,
        })
    }
}

/// Persist #319's already-authenticated Ready catalog and #322's already
/// derived partition. This never constructs or re-splits a partition.
pub fn write_catalog_partition_artifact(
    output_root: &Path,
    directory: &Path,
    catalog: &PolymarketReadyEventCatalog,
    partition: &EventCohortPartition,
) -> Result<CatalogPartitionArtifactRef, String> {
    validate_catalog_partition_write_bounds(catalog, partition)?;
    validate_catalog_partition_membership(catalog, partition)?;
    let payload = CatalogPartitionArtifactPayload {
        schema_version: CATALOG_PARTITION_ARTIFACT_VERSION,
        policy_snapshot_id: partition.causal_projection_policy_id(),
        catalog_receipts: catalog
            .receipts()
            .filter(|receipt| receipt.state == PolymarketCatalogReceiptState::Ready)
            .collect(),
        partition: PersistedEventCohortPartition::from(partition),
    };
    let payload_sha256 = format!("sha256:{}", sha256_hex(&canonical_json_bytes(&payload)?));
    let envelope = CatalogPartitionArtifactEnvelope {
        schema_version: CATALOG_PARTITION_ARTIFACT_VERSION.to_string(),
        payload_sha256: payload_sha256.clone(),
        payload: PersistedCatalogPartitionArtifactPayload {
            schema_version: payload.schema_version.to_string(),
            policy_snapshot_id: payload.policy_snapshot_id.to_string(),
            catalog_receipts: payload
                .catalog_receipts
                .into_iter()
                .map(serde_json::to_value)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("serialize catalog receipt: {error}"))?,
            partition: payload.partition,
        },
    };
    let bytes = canonical_json_bytes(&envelope)?;
    if bytes.len() > MAX_CATALOG_PARTITION_ARTIFACT_BYTES {
        return Err(format!(
            "catalog partition artifact exceeds {MAX_CATALOG_PARTITION_ARTIFACT_BYTES} bytes"
        ));
    }
    let expected_path = relative_path(
        output_root,
        &directory.join(format!("catalog-partition-{}.json", sha256_hex(&bytes))),
    )?;
    validate_catalog_partition_artifact_path(&expected_path)?;
    let artifact =
        write_content_addressed_json(output_root, directory, "catalog-partition", &envelope)?;
    debug_assert_eq!(artifact.path, expected_path);
    Ok(CatalogPartitionArtifactRef {
        path: artifact.path,
        artifact_sha256: format!("sha256:{}", artifact.sha256),
        payload_sha256,
    })
}

/// Freshly verify a bounded, canonical artifact before returning usable inputs.
pub fn read_catalog_partition_artifact(
    output_root: &Path,
    artifact: &CatalogPartitionArtifactRef,
) -> Result<ValidatedCatalogPartitionArtifact, String> {
    validate_catalog_partition_artifact_path(&artifact.path)?;
    validate_sha256_id(
        &artifact.artifact_sha256,
        "catalog partition artifact identity",
    )?;
    validate_sha256_id(
        &artifact.payload_sha256,
        "catalog partition payload identity",
    )?;
    let raw_digest = artifact
        .artifact_sha256
        .strip_prefix("sha256:")
        .expect("validate_sha256_id accepts only sha256 IDs");
    let bytes = read_verified_artifact_bounded(
        output_root,
        &ArtifactRef {
            path: artifact.path.clone(),
            sha256: raw_digest.to_string(),
        },
        MAX_CATALOG_PARTITION_ARTIFACT_BYTES,
    )?;
    let envelope: CatalogPartitionArtifactEnvelope = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse catalog partition artifact: {error}"))?;
    if canonical_json_bytes(&envelope)? != bytes {
        return Err("catalog partition artifact is not canonical JSON".into());
    }
    if envelope.schema_version != CATALOG_PARTITION_ARTIFACT_VERSION
        || envelope.payload.schema_version != CATALOG_PARTITION_ARTIFACT_VERSION
    {
        return Err("unsupported catalog partition artifact schema".into());
    }
    if envelope.payload_sha256 != artifact.payload_sha256 {
        return Err("catalog partition artifact payload identity mismatches reference".into());
    }
    let expected_payload_sha256 = format!(
        "sha256:{}",
        sha256_hex(&canonical_json_bytes(&envelope.payload)?)
    );
    if expected_payload_sha256 != envelope.payload_sha256 {
        return Err(
            "catalog partition artifact payload digest does not match canonical content".into(),
        );
    }
    let PersistedCatalogPartitionArtifactPayload {
        schema_version: _,
        policy_snapshot_id,
        catalog_receipts,
        partition,
    } = envelope.payload;
    if catalog_receipts.len() > MAX_READY_CATALOG_ENTRIES {
        return Err("persisted ready catalog exceeds the bounded entry count".into());
    }
    if partition.ready_entries.len() > MAX_READY_CATALOG_ENTRIES {
        return Err("event cohort partition exceeds the bounded Ready entry count".into());
    }
    let catalog = PolymarketReadyEventCatalog::from_persisted_ready_receipts(catalog_receipts)
        .map_err(|error| format!("validate persisted ready catalog: {error}"))?;
    let partition = partition.into_partition()?;
    validate_catalog_partition_write_bounds(&catalog, &partition)?;
    if policy_snapshot_id != partition.causal_projection_policy_id() {
        return Err("artifact policy identity does not match partition".into());
    }
    if policy_snapshot_id != current_prediction_policy_snapshot_id() {
        return Err("catalog partition artifact has stale policy identity".into());
    }
    validate_catalog_partition_membership(&catalog, &partition)?;
    Ok(ValidatedCatalogPartitionArtifact { catalog, partition })
}

fn validate_catalog_partition_artifact_path(path: &str) -> Result<(), String> {
    if path.len() > MAX_CATALOG_PARTITION_ARTIFACT_PATH_BYTES {
        return Err("catalog partition artifact path exceeds the bounded length".into());
    }
    Ok(())
}

fn validate_catalog_partition_membership(
    catalog: &PolymarketReadyEventCatalog,
    partition: &EventCohortPartition,
) -> Result<(), String> {
    if partition.causal_projection_policy_id() != current_prediction_policy_snapshot_id() {
        return Err("partition has stale causal projection policy identity".into());
    }
    let ready = catalog
        .receipts()
        .filter(|receipt| receipt.state == PolymarketCatalogReceiptState::Ready)
        .collect::<Vec<_>>();
    if ready.len() != partition.ready_entries.len() {
        return Err("partition does not contain every Ready catalog receipt".into());
    }
    for (receipt, entry) in ready.into_iter().zip(&partition.ready_entries) {
        let settlement = receipt
            .availability
            .as_ref()
            .and_then(|availability| availability.settlement)
            .ok_or_else(|| {
                format!(
                    "ready market {} is missing settlement availability",
                    receipt.market_id
                )
            })?;
        if receipt.receipt_sha256 != entry.receipt_sha256
            || receipt.market_id != entry.market_id
            || receipt.event_start.map(|value| value.timestamp_millis())
                != Some(entry.reference_path_start_ms)
            || receipt.event_end.map(|value| value.timestamp_millis())
                != Some(entry.reference_path_end_ms)
            || settlement.timestamp_millis() != entry.settlement_available_at_ms
        {
            return Err(format!(
                "partition entry does not match Ready catalog receipt {}",
                receipt.market_id
            ));
        }
    }
    Ok(())
}

fn validate_catalog_partition_write_bounds(
    catalog: &PolymarketReadyEventCatalog,
    partition: &EventCohortPartition,
) -> Result<(), String> {
    catalog
        .validate_ready_artifact_bounds(MAX_READY_CATALOG_ENTRIES, MAX_READY_CATALOG_TEXT_BYTES)
        .map_err(|error| format!("validate ready catalog artifact bounds: {error}"))?;
    if partition.ready_entries.len() > MAX_READY_CATALOG_ENTRIES {
        return Err("event cohort partition exceeds the bounded Ready entry count".into());
    }
    Ok(())
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
            .unwrap_or(common_time_boundary_ms);
        for entry in &ready_entries {
            if entry.reference_path_end_ms < common_time_boundary_ms
                && entry.settlement_available_at_ms >= label_availability_cutoff_ms
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

    use crate::prediction_loop_fs::{
        canonical_json_bytes, sha256_hex, write_content_addressed_json,
    };

    use super::{
        read_catalog_partition_artifact, write_catalog_partition_artifact,
        CatalogPartitionArtifactEnvelope, CatalogPartitionArtifactRef, EventCohortPartition,
        EventCohortPartitionPayload, EventCohortReadyEntry, EVENT_COHORT_PARTITION_VERSION,
        MAX_READY_CATALOG_ENTRIES,
    };

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
                settlement: Utc.timestamp_millis_opt(reference_path_end_ms).single(),
            }),
            state: PolymarketCatalogReceiptState::Ready,
            reasons: Vec::new(),
            supported_tasks: vec![PolymarketResearchTask::Btc5mBacktest],
        }
    }

    fn persisted_ready_catalog_fixture() -> PolymarketReadyEventCatalog {
        let receipt = serde_json::from_str(r#"{
            "availability":{"books":"2026-07-17T05:30:02Z","contract":"2026-07-17T05:29:59Z","references":"2026-07-17T05:29:57Z","settlement":"2026-07-17T05:35:02Z","trades":"2026-07-17T05:30:05Z"},
            "content_sha256":"7dc38e6a4930c7ec840787fe24eb9256bc297c7243e5fe38177aff4bf2a6fd8c",
            "coverage":{"down_book":1,"reference":1,"settlement":1,"trades":1,"up_book":1},
            "down_token_id":"down-token","event_end":"2026-07-17T05:35:00Z","event_start":"2026-07-17T05:30:00Z",
            "manifest_sha256":"9fd05772d126b0c7e6f1fbe68595637562166051784f8bc5e0a5b6e8e9b8aef4","market_id":"market-1",
            "qualification_sha256":"52233ca3d83364aaeccb3460a4716a095ab645422773751c0a348b111d9db4ce","reasons":[],
            "receipt_sha256":"ccc6bdb04b333e261629f34ca0df72cc6ea09b14a27f925c8a7e8c035a3bfa67",
            "sequence":{"end":7,"gaps":0,"start":1},"state":"ready",
            "success_sha256":"9c483a286640fbc5e213f782ad33a31d83d4ba4c66868b62a8f5fcc39e1e27c0",
            "supported_tasks":["btc5m_backtest"],
            "trade_completion":{"trade_count":1,"trade_record_ids_sha256":"984aa561be4dd28b3c6638ed6d3369837e828613e8677a1c5a469ef3866f1c5b"},
            "up_token_id":"up-token",
            "verifier":{"binary_sha256":"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee","configuration_sha256":"ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff","policy_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","source_sha256":"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"}
        }"#).expect("parse verified #319 receipt fixture");
        PolymarketReadyEventCatalog::from_persisted_ready_receipts(vec![receipt])
            .expect("fixture is a verified Ready catalog receipt")
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
            .settlement = Utc.timestamp_millis_opt(1_000).single();
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
    fn persisted_catalog_partition_artifact_round_trips_only_after_content_addressed_readback() {
        let root = tempfile::tempdir().unwrap();
        let catalog = PolymarketReadyEventCatalog::default();
        let partition = EventCohortPartition::from_ready_catalog(&catalog, 1_000).unwrap();

        let artifact = write_catalog_partition_artifact(
            root.path(),
            &root.path().join("evidence"),
            &catalog,
            &partition,
        )
        .expect("persist canonical catalog and partition");
        let restored = read_catalog_partition_artifact(root.path(), &artifact)
            .expect("fresh readback must validate the persisted artifact");

        assert_eq!(restored.partition().digest(), partition.digest());
        assert_eq!(restored.catalog().receipts().count(), 0);
    }

    #[test]
    fn persisted_catalog_partition_artifact_round_trips_a_verified_ready_receipt() {
        let root = tempfile::tempdir().unwrap();
        let catalog = persisted_ready_catalog_fixture();
        let end = catalog
            .receipts()
            .next()
            .unwrap()
            .event_end
            .unwrap()
            .timestamp_millis();
        let partition = EventCohortPartition::from_ready_catalog(&catalog, end + 3_000).unwrap();
        let artifact = write_catalog_partition_artifact(
            root.path(),
            &root.path().join("evidence"),
            &catalog,
            &partition,
        )
        .unwrap();

        let restored = read_catalog_partition_artifact(root.path(), &artifact).unwrap();
        let receipt = restored.catalog().receipts().next().unwrap();
        assert_eq!(
            receipt.receipt_sha256,
            "ccc6bdb04b333e261629f34ca0df72cc6ea09b14a27f925c8a7e8c035a3bfa67"
        );
        assert_eq!(receipt.market_id, "market-1");
        assert_eq!(
            restored.partition().ready_entries()[0].receipt_sha256(),
            receipt.receipt_sha256
        );
        assert_eq!(restored.partition().digest(), partition.digest());
    }

    #[test]
    fn persisted_catalog_partition_artifact_rejects_corruption_mutable_paths_and_missing_partition()
    {
        let root = tempfile::tempdir().unwrap();
        let catalog = PolymarketReadyEventCatalog::default();
        let partition = EventCohortPartition::from_ready_catalog(&catalog, 1_000).unwrap();
        let artifact = write_catalog_partition_artifact(
            root.path(),
            &root.path().join("evidence"),
            &catalog,
            &partition,
        )
        .unwrap();

        let mut mutable_path = artifact.clone();
        mutable_path.path = "mutable.json".to_string();
        assert!(read_catalog_partition_artifact(root.path(), &mutable_path).is_err());

        let mut missing_partition: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.path().join(artifact.path())).unwrap())
                .unwrap();
        missing_partition["payload"]
            .as_object_mut()
            .unwrap()
            .remove("partition");
        let missing = write_content_addressed_json(
            root.path(),
            &root.path().join("evidence"),
            "catalog-partition",
            &missing_partition,
        )
        .unwrap();
        let missing_ref = CatalogPartitionArtifactRef {
            path: missing.path,
            artifact_sha256: format!("sha256:{}", missing.sha256),
            payload_sha256: artifact.payload_sha256.clone(),
        };
        assert!(read_catalog_partition_artifact(root.path(), &missing_ref)
            .expect_err("missing partition must fail closed")
            .contains("missing field `partition`"));

        std::fs::write(root.path().join(artifact.path()), b"corrupt").unwrap();
        assert!(read_catalog_partition_artifact(root.path(), &artifact)
            .expect_err("tampered bytes must fail before parse")
            .contains("hash mismatch"));
    }

    #[test]
    fn persisted_catalog_partition_artifact_rejects_catalog_partition_membership_mismatch() {
        let root = tempfile::tempdir().unwrap();
        let catalog = PolymarketReadyEventCatalog::default();
        let partition = EventCohortPartition::from_ready_catalog(&catalog, 1_000).unwrap();
        let artifact = write_catalog_partition_artifact(
            root.path(),
            &root.path().join("evidence"),
            &catalog,
            &partition,
        )
        .unwrap();
        let mut envelope: CatalogPartitionArtifactEnvelope =
            serde_json::from_slice(&std::fs::read(root.path().join(artifact.path())).unwrap())
                .unwrap();
        let persisted = &mut envelope.payload.partition;
        persisted.ready_entries.push(EventCohortReadyEntry {
            receipt_sha256: "a".repeat(64),
            market_id: "missing-from-catalog".to_string(),
            reference_path_start_ms: 2_000,
            reference_path_end_ms: 2_500,
            settlement_available_at_ms: 2_500,
        });
        persisted.label_availability_cutoff_ms = 2_000;
        persisted.held_out_market_ids = vec!["missing-from-catalog".to_string()];
        let partition_payload = EventCohortPartitionPayload {
            schema_version: EVENT_COHORT_PARTITION_VERSION,
            ready_entries: &persisted.ready_entries,
            common_time_boundary_ms: persisted.common_time_boundary_ms,
            label_availability_cutoff_ms: persisted.label_availability_cutoff_ms,
            causal_projection_policy_id: &persisted.causal_projection_policy_id,
            train_market_ids: &persisted.train_market_ids,
            crossing_excluded_market_ids: &persisted.crossing_excluded_market_ids,
            held_out_market_ids: &persisted.held_out_market_ids,
        };
        persisted.digest = format!(
            "sha256:{}",
            sha256_hex(&canonical_json_bytes(&partition_payload).unwrap())
        );
        envelope.payload_sha256 = format!(
            "sha256:{}",
            sha256_hex(&canonical_json_bytes(&envelope.payload).unwrap())
        );
        let mismatched = write_content_addressed_json(
            root.path(),
            &root.path().join("evidence"),
            "catalog-partition",
            &envelope,
        )
        .unwrap();
        let mismatched_ref = CatalogPartitionArtifactRef {
            path: mismatched.path,
            artifact_sha256: format!("sha256:{}", mismatched.sha256),
            payload_sha256: envelope.payload_sha256,
        };
        assert!(
            read_catalog_partition_artifact(root.path(), &mismatched_ref)
                .expect_err("partition cannot add a receipt absent from the catalog")
                .contains("does not contain every Ready catalog receipt")
        );
    }

    #[test]
    fn persisted_catalog_partition_artifact_rejects_correctly_hashed_oversized_entries() {
        let root = tempfile::tempdir().unwrap();
        let catalog = persisted_ready_catalog_fixture();
        let end = catalog
            .receipts()
            .next()
            .unwrap()
            .event_end
            .unwrap()
            .timestamp_millis();
        let partition = EventCohortPartition::from_ready_catalog(&catalog, end + 3_000).unwrap();
        let artifact = write_catalog_partition_artifact(
            root.path(),
            &root.path().join("evidence"),
            &catalog,
            &partition,
        )
        .unwrap();
        let envelope_bytes = std::fs::read(root.path().join(artifact.path())).unwrap();

        let mut oversized_catalog: CatalogPartitionArtifactEnvelope =
            serde_json::from_slice(&envelope_bytes).unwrap();
        let receipt = oversized_catalog.payload.catalog_receipts[0].clone();
        oversized_catalog.payload.catalog_receipts = vec![receipt; MAX_READY_CATALOG_ENTRIES + 1];
        oversized_catalog.payload_sha256 = format!(
            "sha256:{}",
            sha256_hex(&canonical_json_bytes(&oversized_catalog.payload).unwrap())
        );
        let oversized_catalog_ref = write_content_addressed_json(
            root.path(),
            &root.path().join("evidence"),
            "catalog-partition",
            &oversized_catalog,
        )
        .unwrap();
        let oversized_catalog_ref = CatalogPartitionArtifactRef {
            path: oversized_catalog_ref.path,
            artifact_sha256: format!("sha256:{}", oversized_catalog_ref.sha256),
            payload_sha256: oversized_catalog.payload_sha256,
        };
        assert!(
            read_catalog_partition_artifact(root.path(), &oversized_catalog_ref)
                .expect_err("an oversized canonical catalog must fail before receipt admission")
                .contains("persisted ready catalog exceeds the bounded entry count")
        );

        let mut oversized_partition: CatalogPartitionArtifactEnvelope =
            serde_json::from_slice(&envelope_bytes).unwrap();
        let entry = oversized_partition.payload.partition.ready_entries[0].clone();
        oversized_partition.payload.partition.ready_entries =
            vec![entry; MAX_READY_CATALOG_ENTRIES + 1];
        let persisted = &mut oversized_partition.payload.partition;
        let partition_payload = EventCohortPartitionPayload {
            schema_version: EVENT_COHORT_PARTITION_VERSION,
            ready_entries: &persisted.ready_entries,
            common_time_boundary_ms: persisted.common_time_boundary_ms,
            label_availability_cutoff_ms: persisted.label_availability_cutoff_ms,
            causal_projection_policy_id: &persisted.causal_projection_policy_id,
            train_market_ids: &persisted.train_market_ids,
            crossing_excluded_market_ids: &persisted.crossing_excluded_market_ids,
            held_out_market_ids: &persisted.held_out_market_ids,
        };
        persisted.digest = format!(
            "sha256:{}",
            sha256_hex(&canonical_json_bytes(&partition_payload).unwrap())
        );
        oversized_partition.payload_sha256 = format!(
            "sha256:{}",
            sha256_hex(&canonical_json_bytes(&oversized_partition.payload).unwrap())
        );
        let oversized_partition_ref = write_content_addressed_json(
            root.path(),
            &root.path().join("evidence"),
            "catalog-partition",
            &oversized_partition,
        )
        .unwrap();
        let oversized_partition_ref = CatalogPartitionArtifactRef {
            path: oversized_partition_ref.path,
            artifact_sha256: format!("sha256:{}", oversized_partition_ref.sha256),
            payload_sha256: oversized_partition.payload_sha256,
        };
        assert!(
            read_catalog_partition_artifact(root.path(), &oversized_partition_ref)
                .expect_err(
                    "an oversized canonical partition must fail before membership admission"
                )
                .contains("event cohort partition exceeds the bounded Ready entry count")
        );
    }

    #[test]
    fn catalog_partition_writer_rejects_an_oversized_relative_artifact_path() {
        let root = tempfile::tempdir().unwrap();
        let catalog = PolymarketReadyEventCatalog::default();
        let partition = EventCohortPartition::from_ready_catalog(&catalog, 1_000).unwrap();
        let directory = root.path().join(vec!["nested"; 200].join("/"));

        assert!(
            write_catalog_partition_artifact(root.path(), &directory, &catalog, &partition)
                .expect_err("the writer must reject an artifact reference it cannot safely return")
                .contains("catalog partition artifact path exceeds the bounded length")
        );
        assert!(!directory.exists());
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

    #[test]
    fn first_held_out_decision_is_the_label_availability_cutoff() {
        let mut train = ready_receipt('a', "train", 100, 900);
        train.availability.as_mut().unwrap().settlement = Utc.timestamp_millis_opt(1_500).single();
        let held_out = ready_receipt('b', "held-out", 2_000, 2_500);

        let partition = EventCohortPartition::from_ready_entries([&train, &held_out], 1_000)
            .expect("settlement before the first held-out decision remains admissible");
        assert_eq!(partition.label_availability_cutoff_ms(), 2_000);

        train.availability.as_mut().unwrap().settlement = Utc.timestamp_millis_opt(2_000).single();
        let error = EventCohortPartition::from_ready_entries([&train, &held_out], 1_000)
            .expect_err("settlement at the first held-out decision must fail closed");
        assert!(error.contains("settlement label unavailable by cutoff"));
    }
}
