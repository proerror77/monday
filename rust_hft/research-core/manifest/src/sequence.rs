//! Causal, bounded sequence inputs. These identities grant no training or trading authority.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub const SEQUENCE_DATASET_SCHEMA: &str = "monday.cex_sequence_dataset.v1";
pub const SEQUENCE_HORIZONS_MS: [i64; 3] = [5_000, 10_000, 30_000];
pub const MAX_SEQUENCE_CHANNELS: usize = 64;
pub const MAX_SEQUENCE_CONTEXT: usize = 512;
pub const MAX_SEQUENCE_SHARDS: usize = 4096;

pub fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceInputSpecV1 {
    pub ordered_channels: Vec<String>,
    pub context_rows: usize,
    pub bucket_ms: u64,
}

impl SequenceInputSpecV1 {
    pub fn sol_lob() -> Self {
        let mut ordered_channels = vec!["mid_return_1".to_string()];
        for side in ["bid", "ask"] {
            for level in 1..=5 {
                ordered_channels.push(format!("{side}_{level}_distance_bps"));
                ordered_channels.push(format!("{side}_{level}_log_quantity"));
            }
        }
        ordered_channels.extend(
            [
                "aggregate_trade_log_base_volume",
                "aggregate_trade_signed_base_asinh",
                "aggregate_trade_log_count",
            ]
            .map(str::to_string),
        );
        Self {
            ordered_channels,
            context_rows: 60,
            bucket_ms: 1000,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.ordered_channels.is_empty()
            || self.ordered_channels.len() > MAX_SEQUENCE_CHANNELS
            || self.context_rows < 2
            || self.context_rows > MAX_SEQUENCE_CONTEXT
            || self.bucket_ms != 1_000
            || self.ordered_channels.iter().any(|name| {
                name.is_empty()
                    || name.len() > 64
                    || !name
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            })
            || self.ordered_channels.iter().collect::<BTreeSet<_>>().len()
                != self.ordered_channels.len()
        {
            return Err("invalid sequence channel order, context or clock".into());
        }
        Ok(())
    }

    pub fn digest(&self) -> Result<String, String> {
        self.validate()?;
        Ok(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(self).map_err(|e| e.to_string())?)
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceFrameV1 {
    /// Dataset-wide series identity; a recovery snapshot starts a new series.
    pub series_id: u64,
    pub observed_at_ms: i64,
    pub feature_max_available_at_ms: i64,
    /// Decision-time metadata for the cost gate; never part of learned inputs.
    pub spread_bps: f64,
    pub channels: Vec<f32>,
    /// Fractional simple mid returns in the fixed 5/10/30 second order.
    pub forward_returns: [f32; 3],
    pub label_available_at_ms: [i64; 3],
}

impl SequenceFrameV1 {
    pub fn validate(&self, spec: &SequenceInputSpecV1) -> Result<(), String> {
        spec.validate()?;
        if self.observed_at_ms < 0
            || self.feature_max_available_at_ms < 0
            || self.feature_max_available_at_ms > self.observed_at_ms
            || !self.spread_bps.is_finite()
            || self.spread_bps < 0.0
            || self.observed_at_ms % spec.bucket_ms as i64 != 0
            || self.channels.len() != spec.ordered_channels.len()
            || self
                .channels
                .iter()
                .chain(&self.forward_returns)
                .any(|v| !v.is_finite())
        {
            return Err("invalid sequence frame values or feature availability".into());
        }
        for (horizon, available) in SEQUENCE_HORIZONS_MS.iter().zip(self.label_available_at_ms) {
            let endpoint = self
                .observed_at_ms
                .checked_add(*horizon)
                .ok_or("sequence label clock overflow")?;
            if available < endpoint {
                return Err("sequence label is available before its endpoint".into());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceShardV1 {
    /// A basename under the admitted dataset directory, never a URL or traversal.
    pub file: String,
    pub sha256: String,
    pub bytes: u64,
    pub rows: u64,
    pub first_observed_at_ms: i64,
    pub last_observed_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceDatasetV1 {
    pub schema_version: String,
    pub venue: String,
    pub symbol: String,
    pub source_manifest_sha256: String,
    pub input: SequenceInputSpecV1,
    pub shards: Vec<SequenceShardV1>,
}

impl SequenceDatasetV1 {
    pub fn validate(&self) -> Result<(), String> {
        self.input.validate()?;
        if self.schema_version != SEQUENCE_DATASET_SCHEMA
            || self.venue != "binance-usdm"
            || self.symbol != "SOLUSDT"
            || !valid_sha256(&self.source_manifest_sha256)
            || self.shards.is_empty()
            || self.shards.len() > MAX_SEQUENCE_SHARDS
        {
            return Err("invalid SOL sequence dataset identity".into());
        }
        let mut files = BTreeSet::new();
        let mut previous = None;
        for shard in &self.shards {
            if shard.file.is_empty()
                || shard.file.len() > 128
                || !shard
                    .file
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
                || !shard.file.ends_with(".jsonl")
                || shard.file.starts_with('.')
                || !valid_sha256(&shard.sha256)
                || shard.bytes == 0
                || shard.bytes > 256 * 1024 * 1024
                || shard.rows == 0
                || shard.rows > 86_400
                || shard.first_observed_at_ms < 0
                || shard.last_observed_at_ms < shard.first_observed_at_ms
                || shard.first_observed_at_ms % 1_000 != 0
                || shard.last_observed_at_ms % 1_000 != 0
                || !files.insert(&shard.file)
                || previous.is_some_and(|end| shard.first_observed_at_ms <= end)
            {
                return Err("invalid, duplicate or unordered sequence shard".into());
            }
            previous = Some(shard.last_observed_at_ms);
        }
        Ok(())
    }

    pub fn digest(&self) -> Result<String, String> {
        self.validate()?;
        Ok(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(self).map_err(|e| e.to_string())?)
        ))
    }
}

/// A half-open view. Context may precede the first decision but never history_start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceViewV1 {
    pub history_start_ms: i64,
    pub decision_start_ms: i64,
    pub end_ms: i64,
    /// A frozen training-anchor grid; validation normally retains every second.
    pub decision_stride_ms: i64,
}

impl SequenceViewV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.history_start_ms < 0
            || self.decision_start_ms < self.history_start_ms
            || self.end_ms <= self.decision_start_ms
            || self.decision_stride_ms <= 0
            || self.decision_stride_ms > 86_400_000
            || self.decision_stride_ms % 1000 != 0
            || [self.history_start_ms, self.decision_start_ms, self.end_ms]
                .iter()
                .any(|t| t % 1000 != 0)
        {
            return Err("invalid sequence view clocks".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sequence_identity_binds_channel_order_and_rejects_duplicates() {
        let mut spec = SequenceInputSpecV1 {
            ordered_channels: vec!["a".into(), "b".into()],
            context_rows: 60,
            bucket_ms: 1000,
        };
        let before = spec.digest().unwrap();
        spec.ordered_channels.reverse();
        assert_ne!(before, spec.digest().unwrap());
        spec.ordered_channels[1] = spec.ordered_channels[0].clone();
        assert!(spec.validate().is_err());
    }
    #[test]
    fn sequence_frame_rejects_future_features_and_immature_labels() {
        let spec = SequenceInputSpecV1 {
            ordered_channels: vec!["a".into()],
            context_rows: 60,
            bucket_ms: 1000,
        };
        let mut row = SequenceFrameV1 {
            series_id: 1,
            observed_at_ms: 1000,
            feature_max_available_at_ms: 1000,
            spread_bps: 1.0,
            channels: vec![1.0],
            forward_returns: [0.0; 3],
            label_available_at_ms: [6000, 11000, 31000],
        };
        assert!(row.validate(&spec).is_ok());
        row.feature_max_available_at_ms = 1001;
        assert!(row.validate(&spec).is_err());
        row.feature_max_available_at_ms = 1000;
        row.label_available_at_ms[2] = 30000;
        assert!(row.validate(&spec).is_err());
    }
}
