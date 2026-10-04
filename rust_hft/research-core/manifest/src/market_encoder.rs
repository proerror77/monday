//! Label-free market representation and explicit supervised adaptation contracts.
use crate::sequence::{
    valid_sha256, validate_sequence_shards_with_extension, SequenceInputSpecV1, SequenceShardV1,
    SequenceViewV1,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const FEATURE_SCHEMA: &str = "monday.market_features.v1";
pub const TARGET_SCHEMA: &str = "monday.market_targets.v1";
pub const FEATURE_PARQUET_SCHEMA: &str = "monday.market_features.parquet.v1";
pub const TARGET_PARQUET_SCHEMA: &str = "monday.market_targets.parquet.v1";
pub const ENCODER_SCHEMA: &str = "monday.market_encoder.v1";
pub const TASK_SCHEMA: &str = "monday.market_task.v1";
pub const TASK_HORIZON_MS: i64 = 30_000;

pub fn digest<T: Serialize>(value: &T) -> Result<String, String> {
    Ok(bytes_digest(
        &serde_json::to_vec(value).map_err(|e| e.to_string())?,
    ))
}
pub fn bytes_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn validate_market_shards(shards: &[SequenceShardV1], parquet: bool) -> Result<(), String> {
    validate_sequence_shards_with_extension(shards, if parquet { ".parquet" } else { ".jsonl" })?;
    if parquet
        && shards
            .iter()
            .any(|s| s.bytes > crate::prepared_market::MAX_PREPARED_SHARD_BYTES)
    {
        return Err("prepared market shard exceeds bounded input buffer".into());
    }
    let (bytes, rows) = shards
        .iter()
        .try_fold((0_u64, 0_u64), |(bytes, rows), s| {
            Some((bytes.checked_add(s.bytes)?, rows.checked_add(s.rows)?))
        })
        .ok_or("market dataset size overflow")?;
    let max_rows = if parquet {
        crate::prepared_market::MAX_PREPARED_MARKET_ROWS
    } else {
        14 * 86_400
    };
    if bytes > 8 * 1024 * 1024 * 1024 || rows > max_rows {
        return Err("market dataset exceeds byte or row budget".into());
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketFeatureFrameV1 {
    pub series_id: u64,
    pub observed_at_ms: i64,
    pub feature_max_available_at_ms: i64,
    pub channels: Vec<f32>,
}
impl MarketFeatureFrameV1 {
    pub fn validate(&self, input: &SequenceInputSpecV1) -> Result<(), String> {
        if self.observed_at_ms < 0
            || self.observed_at_ms % 1000 != 0
            || self.feature_max_available_at_ms < 0
            || self.feature_max_available_at_ms > self.observed_at_ms
            || self.channels.len() != input.ordered_channels.len()
            || self.channels.iter().any(|x| !x.is_finite())
        {
            return Err("invalid label-free feature frame or availability".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketTargetFrameV1 {
    pub series_id: u64,
    pub observed_at_ms: i64,
    pub available_at_ms: i64,
    pub simple_return: f32,
    pub spread_bps: f64,
}
impl MarketTargetFrameV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.observed_at_ms < 0
            || self.observed_at_ms % 1000 != 0
            || self
                .observed_at_ms
                .checked_add(TASK_HORIZON_MS)
                .is_none_or(|t| self.available_at_ms < t)
            || !self.simple_return.is_finite()
            || !self.spread_bps.is_finite()
            || self.spread_bps < 0.0
        {
            return Err("invalid 30-second market target".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketFeatureDatasetV1 {
    pub schema_version: String,
    pub venue: String,
    pub symbol: String,
    pub source_manifest_sha256: String,
    pub input: SequenceInputSpecV1,
    pub shards: Vec<SequenceShardV1>,
}
impl MarketFeatureDatasetV1 {
    pub fn validate(&self) -> Result<(), String> {
        self.input.validate()?;
        if !matches!(
            self.schema_version.as_str(),
            FEATURE_SCHEMA | FEATURE_PARQUET_SCHEMA
        ) || self.venue != "binance-usdm"
            || self.symbol != "SOLUSDT"
            || !valid_sha256(&self.source_manifest_sha256)
        {
            return Err("invalid market feature dataset identity".into());
        }
        validate_market_shards(&self.shards, self.schema_version == FEATURE_PARQUET_SCHEMA)
    }
    pub fn digest(&self) -> Result<String, String> {
        self.validate()?;
        digest(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketTargetDatasetV1 {
    pub schema_version: String,
    pub feature_dataset_sha256: String,
    pub horizon_ms: i64,
    pub shards: Vec<SequenceShardV1>,
}
impl MarketTargetDatasetV1 {
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(
            self.schema_version.as_str(),
            TARGET_SCHEMA | TARGET_PARQUET_SCHEMA
        ) || !valid_sha256(&self.feature_dataset_sha256)
            || self.horizon_ms != TASK_HORIZON_MS
        {
            return Err("invalid market target dataset binding".into());
        }
        validate_market_shards(&self.shards, self.schema_version == TARGET_PARQUET_SCHEMA)
    }
    pub fn digest(&self) -> Result<String, String> {
        self.validate()?;
        digest(self)
    }
}

/// The same causal encoder is used for reconstruction and every downstream arm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketEncoderSpecV1 {
    pub input: SequenceInputSpecV1,
    pub hidden_channels: usize,
}
impl MarketEncoderSpecV1 {
    pub fn validate(&self) -> Result<(), String> {
        self.input.validate()?;
        if self.input.context_rows != 60 || !(2..=64).contains(&self.hidden_channels) {
            return Err("market encoder requires a 60-row causal TCN and bounded width".into());
        }
        Ok(())
    }
}

/// A data view has no optimizer budget. Evaluation and Ridge readers use the
/// same admitted clocks without pretending to be a neural training request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketDataReadRequestV1 {
    pub feature_dataset_sha256: String,
    /// Training may pin an eligible anchor index; evaluation uses the full grid.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qualified_anchors_sha256: Option<String>,
    pub input: SequenceInputSpecV1,
    pub view: SequenceViewV1,
    pub anchor_end_ms: i64,
}
impl MarketDataReadRequestV1 {
    pub fn validate(&self) -> Result<(), String> {
        self.input.validate()?;
        self.view.validate()?;
        if !valid_sha256(&self.feature_dataset_sha256)
            || self
                .qualified_anchors_sha256
                .as_deref()
                .is_some_and(|hash| !valid_sha256(hash))
            || self.view.decision_start_ms - self.view.history_start_ms
                < ((self.input.context_rows - 1) * 1000) as i64
            || self.anchor_end_ms <= self.view.decision_start_ms
            || self.anchor_end_ms > self.view.end_ms
            || self.anchor_end_ms % 1000 != 0
        {
            return Err("invalid market data view identity or clocks".into());
        }
        Ok(())
    }
}

/// Contains only structural eligibility, never target values. The producer
/// joins causal contexts to mature target timestamps before any model fitting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketTrainingAnchorV1 {
    pub series_id: u64,
    pub observed_at_ms: i64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketTrainingAnchorSetV1 {
    pub schema_version: String,
    pub feature_dataset_sha256: String,
    pub view: SequenceViewV1,
    pub anchor_end_ms: i64,
    pub anchors: Vec<MarketTrainingAnchorV1>,
}
impl MarketTrainingAnchorSetV1 {
    pub fn validate(&self) -> Result<(), String> {
        self.view.validate()?;
        if self.schema_version != "monday.market_training_anchors.v1"
            || !valid_sha256(&self.feature_dataset_sha256)
            || self.anchor_end_ms <= self.view.decision_start_ms
            || self.anchor_end_ms > self.view.end_ms
            || self.anchors.is_empty()
            || self.anchors.len() > 32_768
            || self
                .anchors
                .windows(2)
                .any(|rows| rows[0].observed_at_ms >= rows[1].observed_at_ms)
            || self.anchors.iter().any(|a| {
                a.observed_at_ms < self.view.decision_start_ms
                    || a.observed_at_ms >= self.anchor_end_ms
                    || (a.observed_at_ms - self.view.decision_start_ms)
                        % self.view.decision_stride_ms
                        != 0
            })
        {
            return Err("invalid qualified market training anchors".into());
        }
        Ok(())
    }
    pub fn digest(&self) -> Result<String, String> {
        self.validate()?;
        digest(self)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketFitRequestV1 {
    pub feature_dataset_sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qualified_anchors_sha256: Option<String>,
    pub spec: MarketEncoderSpecV1,
    pub view: SequenceViewV1,
    /// Exclusive anchor end. Labels, if used, must mature before view.end_ms.
    pub anchor_end_ms: i64,
    pub seed: u64,
    pub batch_size: usize,
    pub updates: usize,
    pub learning_rate: f64,
    pub min_examples: u64,
    pub max_examples: u64,
}
impl MarketFitRequestV1 {
    pub fn read_request(&self) -> MarketDataReadRequestV1 {
        MarketDataReadRequestV1 {
            feature_dataset_sha256: self.feature_dataset_sha256.clone(),
            qualified_anchors_sha256: self.qualified_anchors_sha256.clone(),
            input: self.spec.input.clone(),
            view: self.view,
            anchor_end_ms: self.anchor_end_ms,
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        self.read_request().validate()?;
        self.spec.validate()?;
        self.view.validate()?;
        if !valid_sha256(&self.feature_dataset_sha256)
            || self.view.decision_start_ms - self.view.history_start_ms < 59_000
            || self.anchor_end_ms <= self.view.decision_start_ms
            || self.anchor_end_ms > self.view.end_ms
            || self.anchor_end_ms % 1000 != 0
            || !(1..=256).contains(&self.batch_size)
            || !(1..=16_384).contains(&self.updates)
            || !self.learning_rate.is_finite()
            || self.learning_rate <= 0.0
            || self.learning_rate > 0.01
            || self.min_examples < 2
            || self.max_examples < self.min_examples
            || self.max_examples > 32_768
            || self.updates as u64 * (self.batch_size as u64) < self.max_examples
        {
            return Err("invalid market fit input, clocks or finite training budget".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdaptationModeV1 {
    Scratch,
    LinearProbe,
    FullFineTune,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketAdaptationRequestV1 {
    pub fit: MarketFitRequestV1,
    pub target_dataset_sha256: String,
    pub mode: AdaptationModeV1,
    pub parent_checkpoint_sha256: Option<String>,
    pub head_seed: u64,
}
impl MarketAdaptationRequestV1 {
    pub fn validate(&self) -> Result<(), String> {
        self.fit.validate()?;
        let valid_parent = self
            .parent_checkpoint_sha256
            .as_deref()
            .is_some_and(valid_sha256);
        if !valid_sha256(&self.target_dataset_sha256)
            || match self.mode {
                AdaptationModeV1::Scratch => self.parent_checkpoint_sha256.is_some(),
                _ => !valid_parent,
            }
            || self
                .fit
                .anchor_end_ms
                .checked_add(TASK_HORIZON_MS)
                .is_none_or(|t| t > self.fit.view.end_ms)
            || (self.mode == AdaptationModeV1::FullFineTune && self.fit.updates < 2)
        {
            return Err("invalid market adaptation parent, target or maturity binding".into());
        }
        Ok(())
    }
    pub fn head_only_updates(&self) -> usize {
        match self.mode {
            AdaptationModeV1::Scratch => 0,
            AdaptationModeV1::LinearProbe => self.fit.updates,
            AdaptationModeV1::FullFineTune => self.fit.updates.div_ceil(10),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketFeatureScalingV1 {
    pub means: Vec<f64>,
    pub scales: Vec<f64>,
    pub unique_frames: u64,
    pub examples: u64,
}
impl MarketFeatureScalingV1 {
    pub fn validate(&self, request: &MarketFitRequestV1) -> Result<(), String> {
        self.validate_for_data(
            &request.read_request(),
            request.min_examples,
            request.max_examples,
        )
    }
    pub fn validate_for_data(
        &self,
        request: &MarketDataReadRequestV1,
        min_examples: u64,
        max_examples: u64,
    ) -> Result<(), String> {
        request.validate()?;
        let n = request.input.ordered_channels.len();
        if min_examples < 2
            || max_examples < min_examples
            || max_examples > 32_768
            || self.means.len() != n
            || self.scales.len() != n
            || self.means.iter().any(|v| !v.is_finite())
            || self.scales.iter().any(|v| !v.is_finite() || *v <= 0.0)
            || self.examples < min_examples
            || self.examples > max_examples
            || self.unique_frames < request.input.context_rows as u64
            || self.unique_frames > self.examples * request.input.context_rows as u64
            || self.unique_frames
                > ((request.view.end_ms - request.view.history_start_ms) / 1000) as u64
        {
            return Err("invalid train-only market scaling".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketFitDiagnosticsV1 {
    pub updates: usize,
    pub example_visits: u64,
    pub losses: Vec<f64>,
    pub gradient_norms: Vec<f64>,
    pub initial_encoder_values_sha256: String,
    pub final_encoder_values_sha256: String,
}
impl MarketFitDiagnosticsV1 {
    pub fn validate(&self, fit: &MarketFitRequestV1) -> Result<(), String> {
        if self.updates != fit.updates
            || self.losses.len() != fit.updates
            || self.gradient_norms.len() != fit.updates
            || self.example_visits < fit.updates as u64
            || self.example_visits > (fit.updates * fit.batch_size) as u64
            || self
                .losses
                .iter()
                .any(|v| !v.is_finite() || *v < 0.0 || *v > 1e6)
            || self
                .gradient_norms
                .iter()
                .any(|v| !v.is_finite() || *v < 0.0 || *v > 100.0)
            || !valid_sha256(&self.initial_encoder_values_sha256)
            || !valid_sha256(&self.final_encoder_values_sha256)
        {
            return Err("invalid bounded market training diagnostics".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn market_adaptation_binds_mature_targets_and_exact_parent_mode() {
        let fit = MarketFitRequestV1 {
            feature_dataset_sha256: "a".repeat(64),
            qualified_anchors_sha256: None,
            spec: MarketEncoderSpecV1 {
                input: SequenceInputSpecV1::sol_lob(),
                hidden_channels: 16,
            },
            view: SequenceViewV1 {
                history_start_ms: 0,
                decision_start_ms: 59000,
                end_ms: 200000,
                decision_stride_ms: 1000,
            },
            anchor_end_ms: 170000,
            seed: 7,
            batch_size: 16,
            updates: 80,
            learning_rate: 0.001,
            min_examples: 64,
            max_examples: 128,
        };
        let mut r = MarketAdaptationRequestV1 {
            fit,
            target_dataset_sha256: "b".repeat(64),
            mode: AdaptationModeV1::Scratch,
            parent_checkpoint_sha256: None,
            head_seed: 101,
        };
        r.validate().unwrap();
        r.parent_checkpoint_sha256 = Some("c".repeat(64));
        assert!(r.validate().is_err());
        r.mode = AdaptationModeV1::LinearProbe;
        r.validate().unwrap();
        assert_eq!(r.head_only_updates(), 80);
        r.mode = AdaptationModeV1::FullFineTune;
        r.validate().unwrap();
        assert_eq!(r.head_only_updates(), 8);
        r.fit.anchor_end_ms += 1000;
        assert!(r.validate().is_err());
        r.fit.anchor_end_ms -= 1000;
        r.fit.max_examples = 2000;
        assert!(r.validate().is_err());
    }
    #[test]
    fn market_scaling_json_preserves_f64_parameter_identity() {
        let scale = MarketFeatureScalingV1 {
            means: vec![0.12442158216451653],
            scales: vec![0.9888153076171875],
            unique_frames: 100,
            examples: 64,
        };
        let bytes = serde_json::to_vec(&scale).unwrap();
        let restored: MarketFeatureScalingV1 = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(scale, restored);
        assert_eq!(bytes, serde_json::to_vec(&restored).unwrap());
    }
}
