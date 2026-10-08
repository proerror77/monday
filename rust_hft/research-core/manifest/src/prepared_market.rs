//! Immutable, bounded numerical market inputs. Feature files contain no targets.
//!
//! This module owns the immutable requests, receipts, coverage and format limits.
//! Numerical shard IO lives in hft-prepared-market-io.
use crate::{
    market_encoder::{
        digest, MarketFeatureDatasetV1, MarketTargetDatasetV1, FEATURE_PARQUET_SCHEMA,
        TARGET_PARQUET_SCHEMA, TASK_HORIZON_MS,
    },
    sequence::{valid_sha256, SequenceInputSpecV1, SequenceViewV1, MAX_SEQUENCE_CONTEXT},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const PREPARED_MARKET_VIEW_SCHEMA: &str = "monday.prepared_market_view.v1";
pub const MAX_PREPARED_SHARD_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_PREPARED_GROUP_ROWS: usize = 4_096;
/// Fourteen days of decisions plus bounded causal context and label maturity.
pub const MAX_PREPARED_MARKET_ROWS: u64 = 14 * 86_400 + MAX_SEQUENCE_CONTEXT as u64 + 30;
pub const MAX_GROUP_UNCOMPRESSED_BYTES: i64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedMarketArtifactV1 {
    pub file: String,
    pub sha256: String,
}
impl PreparedMarketArtifactV1 {
    pub fn validate(&self) -> Result<(), String> {
        if !valid_sha256(&self.sha256)
            || self.file.is_empty()
            || self.file.len() > 1024
            || self.file.starts_with('/')
            || self.file.split('/').any(|p| {
                p.is_empty()
                    || p == "."
                    || p == ".."
                    || !p
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
            })
        {
            return Err("invalid prepared artifact reference".into());
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedMarketRequestSourceV1 {
    pub feature_dataset_sha256: String,
    pub target_dataset_sha256: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedMarketDataRequestV1 {
    pub schema_version: String,
    pub sources: Vec<PreparedMarketRequestSourceV1>,
    pub transform_sha256: String,
    pub input: SequenceInputSpecV1,
    pub view: SequenceViewV1,
    pub anchor_end_ms: i64,
    pub purpose: String,
    pub qualified_anchors: bool,
}
impl PreparedMarketDataRequestV1 {
    pub fn validate(&self) -> Result<(), String> {
        self.input.validate()?;
        self.view.validate()?;
        let mut identities = BTreeSet::new();
        if self.schema_version != "monday.market_data_request.v1"
            || self.sources.is_empty()
            || self.sources.len() > 512
            || !valid_sha256(&self.transform_sha256)
            || !matches!(
                self.purpose.as_str(),
                "label_free" | "pre_holdout_supervised"
            )
            || self.sources.iter().any(|s| {
                !valid_sha256(&s.feature_dataset_sha256)
                    || !identities.insert(&s.feature_dataset_sha256)
                    || s.target_dataset_sha256
                        .as_deref()
                        .is_some_and(|h| !valid_sha256(h))
                    || s.target_dataset_sha256.is_some()
                        != (self.purpose == "pre_holdout_supervised")
            })
            || self.view.decision_start_ms - self.view.history_start_ms
                < (self.input.context_rows as i64 - 1) * 1000
            || self.anchor_end_ms <= self.view.decision_start_ms
            || self.anchor_end_ms > self.view.end_ms
            || self.anchor_end_ms % 1000 != 0
            || self.view.end_ms - self.view.history_start_ms
                > MAX_PREPARED_MARKET_ROWS as i64 * 1000
            || self.anchor_end_ms - self.view.decision_start_ms > 14 * 86_400_000
        {
            return Err("invalid prepared data request or admitted time bounds".into());
        }
        Ok(())
    }
    pub fn digest(&self) -> Result<String, String> {
        self.validate()?;
        digest(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedMarketReadyReceiptV2 {
    pub schema_version: String,
    pub producer_source_revision: String,
    pub producer_image: String,
    pub request_sha256: String,
    pub request: PreparedMarketDataRequestV1,
    pub prepared_view_sha256: String,
    pub prepared_view: PreparedMarketViewV1,
    pub feature_manifest: PreparedMarketArtifactV1,
    pub target_manifest: Option<PreparedMarketArtifactV1>,
    pub qualified_anchors: Option<PreparedMarketArtifactV1>,
}
pub fn validate_prepared_producer(revision: &str, image: &str) -> Result<(), String> {
    let valid_revision = revision.len() == 40
        && revision
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    let valid_image = image.split_once("@sha256:").is_some_and(|(name, hash)| {
        !name.is_empty() && !name.bytes().any(|b| b.is_ascii_whitespace()) && valid_sha256(hash)
    });
    if !valid_revision || !valid_image {
        return Err("prepared converter requires exact source and image identities".into());
    }
    Ok(())
}
impl PreparedMarketReadyReceiptV2 {
    pub fn validate(&self) -> Result<(), String> {
        validate_prepared_producer(&self.producer_source_revision, &self.producer_image)?;
        self.request.validate()?;
        self.prepared_view.validate()?;
        self.feature_manifest.validate()?;
        if let Some(v) = &self.target_manifest {
            v.validate()?;
        }
        if let Some(v) = &self.qualified_anchors {
            v.validate()?;
        }
        if self.schema_version != "monday.market_ready_receipt.v2"
            || self.request.digest()? != self.request_sha256
            || self.prepared_view.digest()? != self.prepared_view_sha256
            || self.prepared_view.view != self.request.view
            || self.prepared_view.transform_sha256 != self.request.transform_sha256
            || self.feature_manifest.sha256 != self.prepared_view.feature_dataset_sha256
            || self.target_manifest.as_ref().map(|v| &v.sha256)
                != self.prepared_view.target_dataset_sha256.as_ref()
            || self.qualified_anchors.as_ref().map(|v| &v.sha256)
                != self.prepared_view.qualified_anchors_sha256.as_ref()
            || self.target_manifest.is_some() != (self.request.purpose == "pre_holdout_supervised")
            || self.qualified_anchors.is_some() != self.request.qualified_anchors
            || self.request.sources.len() != self.prepared_view.sources.len()
            || self
                .request
                .sources
                .iter()
                .zip(&self.prepared_view.sources)
                .any(|(r, s)| {
                    r.feature_dataset_sha256 != s.feature_dataset_sha256
                        || r.target_dataset_sha256 != s.target_dataset_sha256
                })
        {
            return Err(
                "prepared ready receipt differs from its request or output identities".into(),
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedMarketSourceV1 {
    pub feature_dataset_sha256: String,
    pub target_dataset_sha256: Option<String>,
    pub source_manifest_sha256: String,
    pub transform_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedMarketSeriesV1 {
    pub series_id: u64,
    pub first_observed_at_ms: i64,
    pub last_observed_at_ms: i64,
    pub rows: u64,
}

/// Adjacent observations surrounding a missing interval. Series boundaries are
/// recorded separately and are never silently filled or forward interpolated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedMarketGapV1 {
    pub last_before_ms: i64,
    pub first_after_ms: i64,
}

/// The ordered source union and actual output identities survive changes to a
/// consumer's seed, batch size, optimizer, or experiment name. Splits and eligible
/// anchors are explicit time views; this contract grants no holdout access.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedMarketViewV1 {
    pub schema_version: String,
    pub sources: Vec<PreparedMarketSourceV1>,
    pub source_feature_dataset_sha256: String,
    pub source_target_dataset_sha256: Option<String>,
    pub source_manifest_sha256: String,
    pub transform_sha256: String,
    pub data_watermark_ms: i64,
    pub view: SequenceViewV1,
    pub feature_dataset_sha256: String,
    pub target_dataset_sha256: Option<String>,
    pub qualified_anchors_sha256: Option<String>,
    pub series: Vec<PreparedMarketSeriesV1>,
    pub gaps: Vec<PreparedMarketGapV1>,
}

/// A single source retains its identity; a union binds its explicit order.
pub fn source_union_digest(hashes: &[String]) -> Result<String, String> {
    if hashes.is_empty() || hashes.iter().any(|v| !valid_sha256(v)) {
        return Err("invalid prepared source union".into());
    }
    if hashes.len() == 1 {
        Ok(hashes[0].clone())
    } else {
        digest(&hashes)
    }
}

impl PreparedMarketViewV1 {
    pub fn validate(&self) -> Result<(), String> {
        self.view.validate()?;
        if self.schema_version != PREPARED_MARKET_VIEW_SCHEMA
            || self.sources.is_empty()
            || self.sources.len() > 4096
            || !valid_sha256(&self.feature_dataset_sha256)
            || !valid_sha256(&self.transform_sha256)
            || self
                .target_dataset_sha256
                .as_deref()
                .is_some_and(|v| !valid_sha256(v))
            || self
                .qualified_anchors_sha256
                .as_deref()
                .is_some_and(|v| !valid_sha256(v))
            || self.data_watermark_ms < self.view.end_ms
            || self.view.end_ms - self.view.history_start_ms
                > MAX_PREPARED_MARKET_ROWS as i64 * 1000
            || self.series.is_empty()
            || self
                .series
                .first()
                .is_none_or(|s| s.first_observed_at_ms != self.view.history_start_ms)
            || self.series.len() as u64 > MAX_PREPARED_MARKET_ROWS
            || self.gaps.len() as u64 > MAX_PREPARED_MARKET_ROWS
        {
            return Err("invalid prepared market view identity or bounds".into());
        }
        let mut identities = BTreeSet::new();
        for source in &self.sources {
            if !valid_sha256(&source.feature_dataset_sha256)
                || !valid_sha256(&source.source_manifest_sha256)
                || source.transform_sha256 != self.transform_sha256
                || source
                    .target_dataset_sha256
                    .as_deref()
                    .is_some_and(|v| !valid_sha256(v))
                || !identities.insert(&source.feature_dataset_sha256)
            {
                return Err("invalid or duplicate prepared market source".into());
            }
        }
        let feature_hashes = self
            .sources
            .iter()
            .map(|s| s.feature_dataset_sha256.clone())
            .collect::<Vec<_>>();
        let manifest_hashes = self
            .sources
            .iter()
            .map(|s| s.source_manifest_sha256.clone())
            .collect::<Vec<_>>();
        let target_hashes = self
            .sources
            .iter()
            .map(|s| s.target_dataset_sha256.clone())
            .collect::<Option<Vec<_>>>();
        let expected_targets = target_hashes
            .as_deref()
            .map(source_union_digest)
            .transpose()?;
        if self.source_feature_dataset_sha256 != source_union_digest(&feature_hashes)?
            || self.source_manifest_sha256 != source_union_digest(&manifest_hashes)?
            || self.source_target_dataset_sha256 != expected_targets
            || (self.target_dataset_sha256.is_some() && expected_targets.is_none())
        {
            return Err("prepared view source union is not bound to its catalog".into());
        }
        let mut previous = None;
        let mut rows = 0_u64;
        for series in &self.series {
            if series.rows == 0
                || series.first_observed_at_ms < self.view.history_start_ms
                || series.last_observed_at_ms >= self.view.end_ms
                || series.first_observed_at_ms > series.last_observed_at_ms
                || series.first_observed_at_ms % 1000 != 0
                || series.last_observed_at_ms % 1000 != 0
                || series.rows
                    > ((series.last_observed_at_ms - series.first_observed_at_ms) / 1000 + 1) as u64
                || previous.is_some_and(|t| series.first_observed_at_ms <= t)
            {
                return Err("invalid actual prepared series coverage".into());
            }
            rows = rows
                .checked_add(series.rows)
                .ok_or("prepared row count overflow")?;
            previous = Some(series.last_observed_at_ms);
        }
        if rows > MAX_PREPARED_MARKET_ROWS {
            return Err("prepared view exceeds 14-day and context row budget".into());
        }
        previous = None;
        for gap in &self.gaps {
            if gap.last_before_ms < self.view.history_start_ms
                || gap.first_after_ms >= self.view.end_ms
                || gap
                    .last_before_ms
                    .checked_add(1000)
                    .is_none_or(|t| gap.first_after_ms <= t)
                || gap.last_before_ms % 1000 != 0
                || gap.first_after_ms % 1000 != 0
                || previous.is_some_and(|t| gap.last_before_ms < t)
            {
                return Err("invalid or overlapping prepared gaps".into());
            }
            previous = Some(gap.first_after_ms);
        }
        Ok(())
    }
    pub fn digest(&self) -> Result<String, String> {
        self.validate()?;
        digest(self)
    }

    /// Binds the manifest's actual coverage to its referenced prepared datasets.
    /// Physical checksums and decoded coverage must additionally be verified by
    /// the readers before the caller publishes a ready receipt.
    pub fn validate_datasets(
        &self,
        features: &MarketFeatureDatasetV1,
        targets: Option<&MarketTargetDatasetV1>,
    ) -> Result<(), String> {
        self.validate()?;
        features.validate()?;
        let declared_rows = self.series.iter().map(|s| s.rows).sum::<u64>();
        if features.schema_version != FEATURE_PARQUET_SCHEMA
            || features.digest()? != self.feature_dataset_sha256
            || features.source_manifest_sha256 != self.source_manifest_sha256
            || features.shards.iter().map(|s| s.rows).sum::<u64>() != declared_rows
            || features.shards.first().map(|s| s.first_observed_at_ms)
                != self.series.first().map(|s| s.first_observed_at_ms)
            || features.shards.last().map(|s| s.last_observed_at_ms)
                != self.series.last().map(|s| s.last_observed_at_ms)
            || features.shards.iter().any(|s| {
                s.first_observed_at_ms < self.view.history_start_ms
                    || s.last_observed_at_ms >= self.view.end_ms
            })
        {
            return Err("prepared view does not bind its feature dataset coverage".into());
        }
        match (targets, self.target_dataset_sha256.as_deref()) {
            (None, None) => Ok(()),
            (Some(targets), Some(expected)) => {
                targets.validate()?;
                if targets.schema_version != TARGET_PARQUET_SCHEMA
                    || targets.digest()? != expected
                    || targets.feature_dataset_sha256 != self.feature_dataset_sha256
                    || targets.shards.iter().any(|s| {
                        s.first_observed_at_ms < self.view.history_start_ms
                            || s.last_observed_at_ms
                                .checked_add(TASK_HORIZON_MS)
                                .is_none_or(|t| t >= self.view.end_ms)
                    })
                {
                    return Err("prepared view does not bind mature target dataset".into());
                }
                Ok(())
            }
            _ => Err("prepared target dataset presence mismatch".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sequence::SequenceShardV1;
    fn input() -> SequenceInputSpecV1 {
        SequenceInputSpecV1 {
            ordered_channels: vec!["first".into(), "second".into()],
            context_rows: 60,
            bucket_ms: 1000,
        }
    }
    #[test]
    fn prepared_view_binds_ordered_union_and_actual_gap_coverage() {
        let sources = vec![
            PreparedMarketSourceV1 {
                feature_dataset_sha256: "a".repeat(64),
                target_dataset_sha256: Some("b".repeat(64)),
                source_manifest_sha256: "c".repeat(64),
                transform_sha256: "d".repeat(64),
            },
            PreparedMarketSourceV1 {
                feature_dataset_sha256: "e".repeat(64),
                target_dataset_sha256: Some("f".repeat(64)),
                source_manifest_sha256: "1".repeat(64),
                transform_sha256: "d".repeat(64),
            },
        ];
        let mut view = PreparedMarketViewV1 {
            schema_version: PREPARED_MARKET_VIEW_SCHEMA.into(),
            source_feature_dataset_sha256: source_union_digest(
                &sources
                    .iter()
                    .map(|s| s.feature_dataset_sha256.clone())
                    .collect::<Vec<_>>(),
            )
            .unwrap(),
            source_target_dataset_sha256: Some(
                source_union_digest(
                    &sources
                        .iter()
                        .map(|s| s.target_dataset_sha256.clone().unwrap())
                        .collect::<Vec<_>>(),
                )
                .unwrap(),
            ),
            source_manifest_sha256: source_union_digest(
                &sources
                    .iter()
                    .map(|s| s.source_manifest_sha256.clone())
                    .collect::<Vec<_>>(),
            )
            .unwrap(),
            sources,
            transform_sha256: "d".repeat(64),
            data_watermark_ms: 100000,
            view: SequenceViewV1 {
                history_start_ms: 0,
                decision_start_ms: 59000,
                end_ms: 100000,
                decision_stride_ms: 1000,
            },
            feature_dataset_sha256: "2".repeat(64),
            target_dataset_sha256: Some("3".repeat(64)),
            qualified_anchors_sha256: None,
            series: vec![
                PreparedMarketSeriesV1 {
                    series_id: 1,
                    first_observed_at_ms: 0,
                    last_observed_at_ms: 3000,
                    rows: 4,
                },
                PreparedMarketSeriesV1 {
                    series_id: 2,
                    first_observed_at_ms: 5000,
                    last_observed_at_ms: 99000,
                    rows: 95,
                },
            ],
            gaps: vec![PreparedMarketGapV1 {
                last_before_ms: 3000,
                first_after_ms: 5000,
            }],
        };
        view.validate().unwrap();
        let features = MarketFeatureDatasetV1 {
            schema_version: FEATURE_PARQUET_SCHEMA.into(),
            venue: "binance-usdm".into(),
            symbol: "SOLUSDT".into(),
            source_manifest_sha256: view.source_manifest_sha256.clone(),
            input: input(),
            shards: vec![SequenceShardV1 {
                file: "features.parquet".into(),
                sha256: "4".repeat(64),
                bytes: 128,
                rows: 99,
                first_observed_at_ms: 0,
                last_observed_at_ms: 99000,
            }],
        };
        view.feature_dataset_sha256 = features.digest().unwrap();
        view.target_dataset_sha256 = None;
        view.validate_datasets(&features, None).unwrap();
        let mut false_coverage = features.clone();
        false_coverage.shards[0].rows -= 1;
        view.feature_dataset_sha256 = false_coverage.digest().unwrap();
        assert!(view.validate_datasets(&false_coverage, None).is_err());
        view.feature_dataset_sha256 = features.digest().unwrap();
        view.sources.reverse();
        assert!(view.validate().is_err());
        view.sources.reverse();
        view.series[1].rows = 96;
        assert!(view.validate().is_err());
        view.series[1].rows = 95;
        for source in &mut view.sources {
            source.target_dataset_sha256 = None;
        }
        view.source_target_dataset_sha256 = None;
        let request = PreparedMarketDataRequestV1 {
            schema_version: "monday.market_data_request.v1".into(),
            sources: view
                .sources
                .iter()
                .map(|s| PreparedMarketRequestSourceV1 {
                    feature_dataset_sha256: s.feature_dataset_sha256.clone(),
                    target_dataset_sha256: None,
                })
                .collect(),
            transform_sha256: view.transform_sha256.clone(),
            input: input(),
            view: view.view,
            anchor_end_ms: view.view.end_ms,
            purpose: "label_free".into(),
            qualified_anchors: false,
        };
        let mut ready = PreparedMarketReadyReceiptV2 {
            schema_version: "monday.market_ready_receipt.v2".into(),
            producer_source_revision: "a".repeat(40),
            producer_image: format!("registry/data@sha256:{}", "b".repeat(64)),
            request_sha256: request.digest().unwrap(),
            request,
            prepared_view_sha256: view.digest().unwrap(),
            feature_manifest: PreparedMarketArtifactV1 {
                file: "features/manifest.json".into(),
                sha256: view.feature_dataset_sha256.clone(),
            },
            prepared_view: view,
            target_manifest: None,
            qualified_anchors: None,
        };
        ready.validate().unwrap();
        ready.producer_source_revision = "source-unbound".into();
        assert!(ready.validate().is_err());
        ready.producer_source_revision = "a".repeat(40);
        ready.request.sources.reverse();
        ready.request_sha256 = ready.request.digest().unwrap();
        assert!(ready.validate().is_err());
    }
}
