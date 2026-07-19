//! Pure-Rust Burn binary probability research model.
//!
//! The input contract makes event-disjoint validation and point-in-time cutoffs
//! explicit. The resulting bundle is research-only: this module has no trading,
//! deployment, or promotion authority integration.

use anyhow::{bail, ensure, Context, Result};
use burn::{
    backend::{Autodiff, NdArray},
    module::{AutodiffModule, Initializer, Module, Param},
    nn::{loss::BinaryCrossEntropyLossConfig, Linear, LinearConfig},
    optim::{AdamConfig, GradientsParams, Optimizer},
    tensor::{activation::sigmoid, backend::Backend, Int, Tensor, TensorData},
};
use burn_store::{BurnpackStore, ModuleSnapshot};
use rand::{rngs::StdRng, Rng, SeedableRng};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
};

use crate::{
    prediction_loop::{
        validate_prediction_mission, validate_prediction_snapshot_coverage,
        validate_prediction_snapshot_sources, PredictionResearchMission,
        PREDICTION_EVENT_WINDOW_SECS,
    },
    research_snapshot::{load_research_snapshot, ResearchSnapshot},
    FactorObservation,
};

const MANIFEST_SCHEMA_VERSION: u32 = 3;
const BURN_VERSION: &str = "0.20.1";
const MODEL_FAMILY: &str = "burn_linear_logit";
const BACKEND_NAME: &str = "burn_ndarray_f32";
const OPTIMIZER_NAME: &str = "adam_full_batch";
const MODEL_FILE: &str = "model.bpk";
const MANIFEST_FILE: &str = "manifest.json";
const MAX_MANIFEST_BYTES: u64 = 1_048_576;
const MAX_BURNPACK_METADATA_BYTES: usize = 1_048_576;
const BURNPACK_HEADER_BYTES: usize = 10;
const BURNPACK_MAGIC: u32 = 0x4255_524e;
const BURNPACK_FORMAT_VERSION: u16 = 1;
const MAX_FEATURES: usize = 4_096;
const MIN_STANDARD_DEVIATION: f64 = 1.0e-12;

type InferenceBackend = NdArray<f32>;
type TrainingBackend = Autodiff<InferenceBackend>;

/// Snapshot handle that can only be created by loading and verifying an
/// immutable snapshot against a caller-supplied trusted content digest.
///
/// The inner snapshot is intentionally private and has no mutable accessor, so
/// binary training cannot accept an in-memory structure with forged manifest
/// strings or post-verification feature mutations.
#[derive(Debug)]
pub struct VerifiedBinarySnapshot {
    snapshot: ResearchSnapshot,
}

impl VerifiedBinarySnapshot {
    /// Load a research snapshot, verify its on-disk content hash, and bind it to
    /// the digest supplied by the trusted snapshot registry or mission runner.
    pub fn load(
        snapshot_dir: impl AsRef<Path>,
        expected_snapshot_contract_sha256: &str,
    ) -> Result<Self> {
        ensure!(
            is_prefixed_sha256(expected_snapshot_contract_sha256),
            "expected snapshot contract digest must use sha256:<64 lowercase hex>"
        );
        let snapshot = load_research_snapshot(snapshot_dir)
            .context("load and verify binary research snapshot")?;
        let actual = snapshot
            .manifest
            .snapshot_contract_hash
            .as_deref()
            .context("verified snapshot is missing snapshot_contract_hash")?;
        ensure!(
            actual == expected_snapshot_contract_sha256,
            "binary research snapshot does not match the trusted evaluator-contract digest"
        );
        Ok(Self { snapshot })
    }

    fn snapshot(&self) -> &ResearchSnapshot {
        &self.snapshot
    }

    #[cfg(test)]
    fn from_test_snapshot(snapshot: ResearchSnapshot) -> Self {
        Self { snapshot }
    }
}

/// Settlement authority accepted by this research lane.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BinarySettlementAuthority {
    OfficialResolution,
}

/// Shared wall-clock boundary used by every horizon-specific task for one
/// underlying. Training events must end strictly before this instant, while
/// validation events must start at or after it. Events touching the boundary
/// on the training side are purged so 5m/15m/1h tasks cannot split an
/// overlapping Chainlink path across train and validation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BinaryTimeBoundary {
    pub boundary_ms: i64,
}

impl BinaryTimeBoundary {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.boundary_ms > 0,
            "shared time boundary must be a positive Unix millisecond timestamp"
        );
        Ok(())
    }
}

/// Provenance and feature-order contract for one governed binary dataset.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BinaryDatasetContract {
    pub mission_id: String,
    pub mission_sha256: String,
    pub snapshot_contract_hash: String,
    pub symbols: Vec<String>,
    pub target_horizon_seconds: u32,
    pub max_feature_age_ms: i64,
    pub settlement_authority: BinarySettlementAuthority,
    pub time_boundary: BinaryTimeBoundary,
    pub feature_names: Vec<String>,
    pub feature_schema_sha256: String,
}

impl BinaryDatasetContract {
    /// Bind a five-minute BTC/SOL training projection to a validated prediction
    /// mission and a snapshot loaded by [`VerifiedBinarySnapshot::load`].
    pub fn from_prediction_snapshot(
        verified_snapshot: &VerifiedBinarySnapshot,
        mission: &PredictionResearchMission,
        feature_names: Vec<String>,
    ) -> Result<Self> {
        let snapshot = verified_snapshot.snapshot();
        validate_prediction_mission(mission, &mission.search_policy_snapshot_id)
            .map_err(anyhow::Error::msg)
            .context("validate prediction mission contract")?;
        validate_prediction_snapshot_sources(&snapshot.manifest)
            .map_err(anyhow::Error::msg)
            .context("validate prediction snapshot sources")?;
        validate_prediction_snapshot_coverage(snapshot, mission)
            .map_err(anyhow::Error::msg)
            .context("validate prediction snapshot coverage")?;
        let manifest = &snapshot.manifest;
        ensure!(
            manifest.symbols.len() == 1
                && crate::factors::normalized_underlying_symbol(&manifest.symbols[0])
                    == mission.symbols[0],
            "prediction snapshot manifest must isolate the mission underlying"
        );
        ensure!(
            manifest.immutable_input,
            "binary ML requires an immutable research snapshot"
        );
        ensure!(
            manifest.require_official_settlement,
            "binary ML requires official settlement labels"
        );
        let snapshot_contract_hash = manifest
            .snapshot_contract_hash
            .clone()
            .context("loaded snapshot is missing snapshot_contract_hash")?;
        ensure!(
            mission.data_snapshot_id == snapshot_contract_hash,
            "prediction mission data_snapshot_id does not match snapshot contract hash"
        );
        let max_feature_age_ms = manifest
            .max_quote_age_secs
            .checked_mul(1_000)
            .context("snapshot max_quote_age_secs overflows milliseconds")?;
        let feature_schema_sha256 = feature_schema_sha256(&feature_names)?;
        let contract = Self {
            mission_id: mission.mission_id.clone(),
            mission_sha256: prefixed_sha256_bytes(
                &serde_json::to_vec(mission).context("serialize prediction mission")?,
            ),
            snapshot_contract_hash,
            symbols: mission.symbols.clone(),
            target_horizon_seconds: PREDICTION_EVENT_WINDOW_SECS as u32,
            max_feature_age_ms,
            settlement_authority: BinarySettlementAuthority::OfficialResolution,
            time_boundary: BinaryTimeBoundary {
                boundary_ms: mission.time_cohort_boundary_ms,
            },
            feature_names,
            feature_schema_sha256,
        };
        contract.validate()?;
        Ok(contract)
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            !self.mission_id.trim().is_empty() && self.mission_id.trim() == self.mission_id,
            "mission_id must be a trimmed non-empty string"
        );
        ensure!(
            is_prefixed_sha256(&self.mission_sha256),
            "mission_sha256 must use sha256:<64 lowercase hex>"
        );
        ensure!(
            is_prefixed_sha256(&self.snapshot_contract_hash),
            "snapshot_contract_hash must use sha256:<64 lowercase hex>"
        );
        ensure!(
            self.symbols.len() == 1 && matches!(self.symbols[0].as_str(), "BTC" | "SOL"),
            "binary prediction dataset must isolate exactly one BTC or SOL symbol"
        );
        let mut symbols = HashSet::with_capacity(self.symbols.len());
        for symbol in &self.symbols {
            ensure!(!symbol.trim().is_empty(), "dataset symbol is empty");
            ensure!(
                symbol.trim() == symbol,
                "dataset symbol contains surrounding whitespace"
            );
            ensure!(symbols.insert(symbol), "duplicate dataset symbol: {symbol}");
        }
        ensure!(
            self.target_horizon_seconds == PREDICTION_EVENT_WINDOW_SECS as u32,
            "binary prediction target horizon must be 300 seconds"
        );
        ensure!(
            self.max_feature_age_ms > 0,
            "max feature age must be positive"
        );
        ensure!(
            self.settlement_authority == BinarySettlementAuthority::OfficialResolution,
            "unsupported settlement authority"
        );
        self.time_boundary.validate()?;
        validate_feature_names(&self.feature_names)?;
        ensure!(
            self.feature_schema_sha256 == feature_schema_sha256(&self.feature_names)?,
            "feature schema SHA-256 does not match ordered feature names"
        );
        Ok(())
    }

    fn validate_mission_binding(&self, mission: &PredictionResearchMission) -> Result<()> {
        let mission_sha256 = prefixed_sha256_bytes(
            &serde_json::to_vec(mission).context("serialize expected prediction mission")?,
        );
        ensure!(
            self.mission_id == mission.mission_id && self.mission_sha256 == mission_sha256,
            "binary dataset contract belongs to a different prediction mission"
        );
        ensure!(
            self.snapshot_contract_hash == mission.data_snapshot_id,
            "binary dataset contract snapshot differs from prediction mission"
        );
        ensure!(
            self.symbols == mission.symbols && mission.horizon == "5m",
            "binary dataset symbol or horizon differs from prediction mission"
        );
        ensure!(
            self.time_boundary.boundary_ms == mission.time_cohort_boundary_ms,
            "binary dataset time boundary differs from prediction mission"
        );
        Ok(())
    }
}

/// Selector for one decision-time row in the governed research snapshot.
///
/// This deliberately contains no feature values, clocks, settlement timestamp,
/// or outcome. The trainer materializes all of those fields from the immutable,
/// content-addressed snapshot so callers cannot smuggle labels into features or
/// claim fictitious point-in-time availability.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BinaryDecisionRow {
    pub event_id: String,
    pub decision_at_ms: i64,
}

/// Caller-selected, already separated train and validation decision rows.
///
/// The trainer validates that no `event_id` occurs in both partitions and that
/// whole event lifetimes fall on the correct side of the contract's shared
/// time boundary. It does not silently re-split rows, and it derives every
/// value from the bound snapshot, which avoids overlapping event-window and
/// caller-input leakage.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EventDisjointBinarySplit {
    pub contract: BinaryDatasetContract,
    pub train: Vec<BinaryDecisionRow>,
    pub validation: Vec<BinaryDecisionRow>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct MaterializedBinarySample {
    event_id: String,
    event_start_at_ms: i64,
    decision_at_ms: i64,
    settlement_at_ms: i64,
    settlement_exclusive_upper_bound_ms: i64,
    label_observed_at_us: i64,
    features: Vec<f32>,
    outcome: bool,
}

#[derive(Debug, Serialize)]
struct MaterializedBinarySplit<'a> {
    contract: &'a BinaryDatasetContract,
    train: Vec<MaterializedBinarySample>,
    validation: Vec<MaterializedBinarySample>,
}

/// Deterministic full-batch training configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BinaryTrainingConfig {
    pub seed: u64,
    pub epochs: usize,
    pub learning_rate: f64,
    /// Numerical clamp used only when reporting validation log loss.
    pub log_loss_epsilon: f64,
}

impl Default for BinaryTrainingConfig {
    fn default() -> Self {
        Self {
            seed: 42,
            epochs: 250,
            learning_rate: 0.03,
            log_loss_epsilon: 1.0e-7,
        }
    }
}

impl BinaryTrainingConfig {
    fn validate(&self) -> Result<()> {
        ensure!(self.epochs > 0, "epochs must be greater than zero");
        ensure!(
            self.epochs <= 1_000_000,
            "epochs exceeds the research safety bound"
        );
        ensure!(
            self.learning_rate.is_finite() && self.learning_rate > 0.0,
            "learning_rate must be finite and positive"
        );
        ensure!(
            self.log_loss_epsilon.is_finite()
                && self.log_loss_epsilon > 0.0
                && self.log_loss_epsilon < 0.5,
            "log_loss_epsilon must be in (0, 0.5)"
        );
        Ok(())
    }
}

/// Train-only feature normalization parameters stored with the bundle.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FeatureNormalizer {
    pub means: Vec<f64>,
    pub standard_deviations: Vec<f64>,
}

impl FeatureNormalizer {
    fn fit(samples: &[MaterializedBinarySample], input_dim: usize) -> Self {
        let count = samples.len() as f64;
        let mut means = vec![0.0; input_dim];
        for sample in samples {
            for (index, value) in sample.features.iter().enumerate() {
                means[index] += f64::from(*value);
            }
        }
        for mean in &mut means {
            *mean /= count;
        }

        let mut variances = vec![0.0; input_dim];
        for sample in samples {
            for (index, value) in sample.features.iter().enumerate() {
                let centered = f64::from(*value) - means[index];
                variances[index] += centered * centered;
            }
        }
        let standard_deviations = variances
            .into_iter()
            .map(|sum| {
                let standard_deviation = (sum / count).sqrt();
                if standard_deviation < MIN_STANDARD_DEVIATION {
                    1.0
                } else {
                    standard_deviation
                }
            })
            .collect();

        Self {
            means,
            standard_deviations,
        }
    }

    fn validate(&self, input_dim: usize) -> Result<()> {
        ensure!(
            self.means.len() == input_dim && self.standard_deviations.len() == input_dim,
            "normalizer dimension does not match feature schema"
        );
        ensure!(
            self.means.iter().all(|value| value.is_finite()),
            "normalizer contains a non-finite mean"
        );
        ensure!(
            self.standard_deviations
                .iter()
                .all(|value| value.is_finite() && *value > 0.0),
            "normalizer contains an invalid standard deviation"
        );
        Ok(())
    }

    fn transform(&self, features: &[f32]) -> Result<Vec<f32>> {
        ensure!(
            features.len() == self.means.len(),
            "feature width {} does not match model width {}",
            features.len(),
            self.means.len()
        );
        let mut normalized = Vec::with_capacity(features.len());
        for (index, value) in features.iter().enumerate() {
            ensure!(value.is_finite(), "feature {index} is not finite");
            let value =
                ((f64::from(*value) - self.means[index]) / self.standard_deviations[index]) as f32;
            ensure!(
                value.is_finite(),
                "normalized feature {index} is not finite"
            );
            normalized.push(value);
        }
        Ok(normalized)
    }
}

/// Metrics computed once, out of sample, after training completes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BinaryOosMetrics {
    pub sample_count: usize,
    pub event_count: usize,
    pub brier_score: f64,
    pub log_loss: f64,
    pub accuracy: f64,
}

impl BinaryOosMetrics {
    fn validate(&self) -> Result<()> {
        ensure!(self.sample_count > 0, "OOS sample count must be positive");
        ensure!(self.event_count > 0, "OOS event count must be positive");
        ensure!(
            self.brier_score.is_finite() && (0.0..=1.0).contains(&self.brier_score),
            "invalid Brier score in manifest"
        );
        ensure!(
            self.log_loss.is_finite() && self.log_loss >= 0.0,
            "invalid log loss in manifest"
        );
        ensure!(
            self.accuracy.is_finite() && (0.0..=1.0).contains(&self.accuracy),
            "invalid accuracy in manifest"
        );
        Ok(())
    }
}

/// Explicitly non-authoritative artifact scope.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BinaryArtifactScope {
    ResearchOnly,
}

/// Typed JSON sidecar for a Burnpack model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BinaryModelManifest {
    pub schema_version: u32,
    pub artifact_scope: BinaryArtifactScope,
    pub model_family: String,
    pub backend: String,
    pub burn_version: String,
    pub optimizer: String,
    pub dataset_contract: BinaryDatasetContract,
    pub normalizer: FeatureNormalizer,
    pub training: BinaryTrainingConfig,
    pub dataset_sha256: String,
    pub train_sample_count: usize,
    pub train_event_count: usize,
    pub validation_metrics: BinaryOosMetrics,
    pub model_file: String,
    /// `None` only while the in-memory model has not yet been persisted.
    pub model_sha256: Option<String>,
}

/// Digests that must be stored in a trusted research registry after persistence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BinaryBundleDigest {
    pub manifest_sha256: String,
    pub model_sha256: String,
}

impl BinaryBundleDigest {
    fn validate(&self) -> Result<()> {
        ensure!(
            is_sha256(&self.manifest_sha256),
            "expected manifest SHA-256 is invalid"
        );
        ensure!(
            is_sha256(&self.model_sha256),
            "expected Burnpack SHA-256 is invalid"
        );
        Ok(())
    }
}

impl BinaryModelManifest {
    fn validate(&self, persisted: bool) -> Result<()> {
        ensure!(
            self.schema_version == MANIFEST_SCHEMA_VERSION,
            "unsupported binary model manifest schema {}",
            self.schema_version
        );
        ensure!(
            self.artifact_scope == BinaryArtifactScope::ResearchOnly,
            "binary model artifact is not research-only"
        );
        ensure!(
            self.model_family == MODEL_FAMILY,
            "unsupported model family"
        );
        ensure!(self.backend == BACKEND_NAME, "unsupported Burn backend");
        ensure!(
            self.burn_version == BURN_VERSION,
            "unsupported Burn artifact version"
        );
        ensure!(self.optimizer == OPTIMIZER_NAME, "unsupported optimizer");
        self.dataset_contract.validate()?;
        self.normalizer
            .validate(self.dataset_contract.feature_names.len())?;
        self.training.validate()?;
        ensure!(
            is_sha256(&self.dataset_sha256),
            "invalid dataset SHA-256 in manifest"
        );
        ensure!(
            self.train_sample_count > 0 && self.train_event_count > 0,
            "training counts must be positive"
        );
        self.validation_metrics.validate()?;
        ensure!(self.model_file == MODEL_FILE, "unexpected model file path");
        if persisted {
            ensure!(
                self.model_sha256.as_deref().is_some_and(is_sha256),
                "persisted manifest is missing a valid model SHA-256"
            );
        } else if let Some(hash) = &self.model_sha256 {
            ensure!(is_sha256(hash), "invalid model SHA-256 in manifest");
        }
        Ok(())
    }
}

#[derive(Module, Debug)]
struct BurnBinaryLinear<B: Backend> {
    projection: Linear<B>,
}

impl<B: Backend> BurnBinaryLinear<B> {
    fn forward(&self, features: Tensor<B, 2>) -> Tensor<B, 2> {
        self.projection.forward(features)
    }

    fn zeros(input_dim: usize, device: &B::Device) -> Self {
        Self {
            projection: LinearConfig::new(input_dim, 1)
                .with_initializer(Initializer::Zeros)
                .init(device),
        }
    }
}

impl BurnBinaryLinear<TrainingBackend> {
    fn from_seed(
        input_dim: usize,
        seed: u64,
        device: &<TrainingBackend as Backend>::Device,
    ) -> Self {
        // Generate weights from a local RNG so determinism does not depend on a
        // process-global backend RNG or concurrent research jobs.
        let mut rng = StdRng::seed_from_u64(seed);
        let bound = (1.0 / input_dim as f32).sqrt();
        let weights = (0..input_dim)
            .map(|_| rng.gen_range(-bound..bound))
            .collect::<Vec<f32>>();
        let bias = vec![rng.gen_range(-bound..bound)];
        Self {
            projection: Linear {
                weight: Param::from_data(TensorData::new(weights, [input_dim, 1]), device),
                bias: Some(Param::from_data(TensorData::new(bias, [1]), device)),
            },
        }
    }
}

/// Trained research model and its evidence manifest.
///
/// This type intentionally does not implement `StrategyModel`, `SignalSource`,
/// deployment, or promotion traits.
#[derive(Debug)]
pub struct BinaryProbabilityModel {
    model: BurnBinaryLinear<InferenceBackend>,
    manifest: BinaryModelManifest,
}

impl BinaryProbabilityModel {
    pub fn manifest(&self) -> &BinaryModelManifest {
        &self.manifest
    }

    /// Produce probabilities for research evaluation after verifying the exact
    /// ordered feature schema used by the caller.
    pub fn predict_probabilities(
        &self,
        expected_feature_schema_sha256: &str,
        feature_names: &[String],
        feature_rows: &[Vec<f32>],
    ) -> Result<Vec<f64>> {
        ensure!(!feature_rows.is_empty(), "prediction batch is empty");
        ensure!(
            expected_feature_schema_sha256 == self.manifest.dataset_contract.feature_schema_sha256,
            "prediction feature schema SHA-256 does not match the model"
        );
        ensure!(
            feature_names == self.manifest.dataset_contract.feature_names.as_slice(),
            "prediction feature schema does not match the trained ordered schema"
        );
        ensure!(
            feature_schema_sha256(feature_names)?
                == self.manifest.dataset_contract.feature_schema_sha256,
            "prediction feature schema SHA-256 does not match the model"
        );
        predict_probabilities(&self.model, &self.manifest.normalizer, feature_rows)
    }

    /// Persist a Burnpack plus typed JSON manifest in a new directory.
    /// Existing directories are never overwritten.
    pub fn save_bundle(&mut self, bundle_dir: &Path) -> Result<BinaryBundleDigest> {
        self.manifest.validate(false)?;
        ensure!(
            !bundle_dir.exists(),
            "refusing to overwrite existing model bundle {}",
            bundle_dir.display()
        );
        fs::create_dir(bundle_dir)
            .with_context(|| format!("create binary model bundle {}", bundle_dir.display()))?;

        let write_result = self.write_bundle(bundle_dir);
        if write_result.is_err() {
            let _ = fs::remove_dir_all(bundle_dir);
        }
        write_result
    }

    fn write_bundle(&mut self, bundle_dir: &Path) -> Result<BinaryBundleDigest> {
        let model_path = bundle_dir.join(MODEL_FILE);
        let mut store = BurnpackStore::from_file(&model_path)
            .auto_extension(false)
            .metadata("artifact_scope", "research_only")
            .metadata("dataset_sha256", self.manifest.dataset_sha256.clone())
            .metadata(
                "snapshot_contract_hash",
                self.manifest
                    .dataset_contract
                    .snapshot_contract_hash
                    .clone(),
            )
            .metadata(
                "mission_sha256",
                self.manifest.dataset_contract.mission_sha256.clone(),
            )
            .metadata(
                "feature_schema_sha256",
                self.manifest.dataset_contract.feature_schema_sha256.clone(),
            )
            .metadata("manifest_schema", MANIFEST_SCHEMA_VERSION.to_string());
        self.model
            .save_into(&mut store)
            .context("write Burnpack model")?;

        let mut persisted_manifest = self.manifest.clone();
        persisted_manifest.model_sha256 = Some(sha256_file(&model_path)?);
        persisted_manifest.validate(true)?;

        let manifest_path = bundle_dir.join(MANIFEST_FILE);
        let mut manifest_file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&manifest_path)
            .with_context(|| format!("create {}", manifest_path.display()))?;
        let manifest_bytes = serde_json::to_vec_pretty(&persisted_manifest)
            .context("serialize binary model manifest")?;
        manifest_file
            .write_all(&manifest_bytes)
            .with_context(|| format!("write {}", manifest_path.display()))?;
        manifest_file
            .sync_all()
            .with_context(|| format!("sync {}", manifest_path.display()))?;

        let digest = BinaryBundleDigest {
            manifest_sha256: sha256_file(&manifest_path)?,
            model_sha256: persisted_manifest
                .model_sha256
                .clone()
                .expect("persisted manifest validation requires model SHA-256"),
        };
        self.manifest = persisted_manifest;
        Ok(digest)
    }

    /// Load a bundle fail-closed. `expected_bundle` must come from a trusted
    /// research registry, not from another file inside `bundle_dir`.
    pub fn load_bundle(
        bundle_dir: &Path,
        expected_bundle: &BinaryBundleDigest,
        expected_mission: &PredictionResearchMission,
    ) -> Result<Self> {
        expected_bundle.validate()?;
        let manifest_path = bundle_dir.join(MANIFEST_FILE);
        let metadata = fs::metadata(&manifest_path)
            .with_context(|| format!("stat {}", manifest_path.display()))?;
        ensure!(
            metadata.len() <= MAX_MANIFEST_BYTES,
            "binary model manifest exceeds size limit"
        );
        ensure!(
            sha256_file(&manifest_path)? == expected_bundle.manifest_sha256,
            "manifest SHA-256 does not match trusted registry digest"
        );
        let manifest_bytes = fs::read(&manifest_path)
            .with_context(|| format!("read {}", manifest_path.display()))?;
        let manifest: BinaryModelManifest =
            serde_json::from_slice(&manifest_bytes).context("parse binary model manifest")?;
        manifest.validate(true)?;
        manifest
            .dataset_contract
            .validate_mission_binding(expected_mission)?;

        let model_path = bundle_dir.join(&manifest.model_file);
        let actual_hash = sha256_file(&model_path)?;
        ensure!(
            actual_hash == expected_bundle.model_sha256,
            "Burnpack SHA-256 does not match trusted registry digest"
        );
        ensure!(
            manifest.model_sha256.as_deref() == Some(actual_hash.as_str()),
            "Burnpack SHA-256 does not match manifest"
        );
        validate_burnpack_metadata(&model_path, &manifest)?;

        let device = Default::default();
        let mut model = BurnBinaryLinear::<InferenceBackend>::zeros(
            manifest.dataset_contract.feature_names.len(),
            &device,
        );
        let mut store = BurnpackStore::from_file(&model_path).auto_extension(false);
        let applied = model.load_from(&mut store).context("load Burnpack model")?;
        ensure!(
            applied.is_success(),
            "Burnpack did not fully apply to the binary model: {applied:?}"
        );

        Ok(Self { model, manifest })
    }
}

/// Train a deterministic Burn binary probability model and evaluate it once on
/// the caller-selected event-disjoint validation partition. Feature values and
/// labels are always materialized from the bound governed snapshot.
pub fn train_event_disjoint_binary(
    verified_snapshot: &VerifiedBinarySnapshot,
    mission: &PredictionResearchMission,
    split: &EventDisjointBinarySplit,
    config: BinaryTrainingConfig,
) -> Result<BinaryProbabilityModel> {
    let snapshot = verified_snapshot.snapshot();
    config.validate()?;
    split.contract.validate()?;
    split.contract.validate_mission_binding(mission)?;
    let expected_contract = BinaryDatasetContract::from_prediction_snapshot(
        verified_snapshot,
        mission,
        split.contract.feature_names.clone(),
    )?;
    ensure!(
        split.contract == expected_contract,
        "binary dataset contract does not match the validated prediction snapshot"
    );
    let input_dim = split.contract.feature_names.len();
    let materialized = materialize_split(split, snapshot)?;
    validate_split(&materialized, input_dim)?;

    let normalizer = FeatureNormalizer::fit(&materialized.train, input_dim);
    normalizer.validate(input_dim)?;
    let train_features = normalized_flattened_features(&materialized.train, &normalizer)?;
    let train_labels = materialized
        .train
        .iter()
        .map(|sample| i64::from(sample.outcome))
        .collect::<Vec<i64>>();

    let device = Default::default();
    TrainingBackend::seed(&device, config.seed);
    let mut model = BurnBinaryLinear::<TrainingBackend>::from_seed(input_dim, config.seed, &device);
    let features = Tensor::<TrainingBackend, 2>::from_data(
        TensorData::new(train_features, [materialized.train.len(), input_dim]),
        &device,
    );
    let labels = Tensor::<TrainingBackend, 2, Int>::from_data(
        TensorData::new(train_labels, [materialized.train.len(), 1]),
        &device,
    );
    let loss = BinaryCrossEntropyLossConfig::new()
        .with_logits(true)
        .init(&device);
    let mut optimizer = AdamConfig::new().init();

    // Validation data is deliberately not materialized or read in this loop.
    for _ in 0..config.epochs {
        let logits = model.forward(features.clone());
        let objective = loss.forward(logits, labels.clone());
        let gradients = GradientsParams::from_grads(objective.backward(), &model);
        model = optimizer.step(config.learning_rate, model, gradients);
    }

    let model = model.valid();
    let validation_rows = materialized
        .validation
        .iter()
        .map(|sample| sample.features.clone())
        .collect::<Vec<_>>();
    let validation_probabilities = predict_probabilities(&model, &normalizer, &validation_rows)?;
    let validation_metrics = compute_metrics(
        &materialized.validation,
        &validation_probabilities,
        config.log_loss_epsilon,
    )?;

    let manifest = BinaryModelManifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        artifact_scope: BinaryArtifactScope::ResearchOnly,
        model_family: MODEL_FAMILY.to_owned(),
        backend: BACKEND_NAME.to_owned(),
        burn_version: BURN_VERSION.to_owned(),
        optimizer: OPTIMIZER_NAME.to_owned(),
        dataset_contract: split.contract.clone(),
        normalizer,
        training: config,
        dataset_sha256: sha256_bytes(
            &serde_json::to_vec(&materialized).context("serialize governed binary split")?,
        ),
        train_sample_count: materialized.train.len(),
        train_event_count: event_count(&materialized.train),
        validation_metrics,
        model_file: MODEL_FILE.to_owned(),
        model_sha256: None,
    };
    manifest.validate(false)?;

    Ok(BinaryProbabilityModel { model, manifest })
}

fn validate_feature_names(feature_names: &[String]) -> Result<()> {
    ensure!(!feature_names.is_empty(), "feature schema is empty");
    ensure!(
        feature_names.len() <= MAX_FEATURES,
        "feature schema exceeds {MAX_FEATURES} columns"
    );
    let mut unique = HashSet::with_capacity(feature_names.len());
    for name in feature_names {
        ensure!(!name.trim().is_empty(), "feature name is empty");
        ensure!(
            name.trim() == name,
            "feature name contains surrounding whitespace: {name:?}"
        );
        ensure!(
            unique.insert(name.as_str()),
            "duplicate feature name: {name}"
        );
        ensure!(
            registered_feature_accessor(name).is_some(),
            "feature {name:?} is not a registered point-in-time binary feature"
        );
    }
    Ok(())
}

type RegisteredFeatureAccessor = fn(&FactorObservation) -> f64;

/// Closed registry of decision-time fields that the binary trainer is allowed
/// to project from a governed snapshot. Settlement and `future_*` fields are
/// intentionally absent.
fn registered_feature_accessor(name: &str) -> Option<RegisteredFeatureAccessor> {
    match name {
        "time_remaining_secs" => Some(|row| row.time_remaining_secs as f64),
        "signed_distance_to_beat" => Some(|row| row.signed_distance_to_beat),
        "abs_distance_to_beat" => Some(|row| row.abs_distance_to_beat),
        "drift_10s" => Some(|row| row.drift_10s),
        "drift_30s" => Some(|row| row.drift_30s),
        "flip_age_secs" => Some(|row| row.flip_age_secs),
        "post_flip_drift" => Some(|row| row.post_flip_drift),
        "sigma_horizon" => Some(|row| row.sigma_horizon),
        "fair_prob_up" => Some(|row| row.fair_prob_up),
        "fair_prob_up_clean" => Some(|row| row.fair_prob_up_clean),
        "prob_disagreement" => Some(|row| row.prob_disagreement),
        "implied_sigma_horizon" => Some(|row| row.implied_sigma_horizon),
        "vol_gap" => Some(|row| row.vol_gap),
        "distance_over_sigma" => Some(|row| row.distance_over_sigma),
        "model_prob_up" => Some(|row| row.model_prob_up),
        "chainlink_prob_up" => Some(|row| row.chainlink_prob_up),
        "model_edge_up" => Some(|row| row.model_edge_up),
        "reward_risk_up" => Some(|row| row.reward_risk_up),
        "reward_risk_down" => Some(|row| row.reward_risk_down),
        "obi" => Some(|row| row.obi),
        "spread_bps" => Some(|row| row.spread_bps),
        "microprice_offset_bps" => Some(|row| row.microprice_offset_bps),
        "bid_depth_near" => Some(|row| row.bid_depth_near),
        "ask_depth_near" => Some(|row| row.ask_depth_near),
        "depth_ratio" => Some(|row| row.depth_ratio),
        "depth_imbalance" => Some(|row| row.depth_imbalance),
        "depth_far_ratio" => Some(|row| row.depth_far_ratio),
        "depth_acceleration" => Some(|row| row.depth_acceleration),
        "obi_10" => Some(|row| row.obi_10),
        "pm_up_bid" => Some(|row| row.pm_up_bid),
        "pm_up_ask" => Some(|row| row.pm_up_ask),
        "pm_up_bid_size" => Some(|row| row.pm_up_bid_size),
        "pm_up_ask_size" => Some(|row| row.pm_up_ask_size),
        "pm_down_bid" => Some(|row| row.pm_down_bid),
        "pm_down_ask" => Some(|row| row.pm_down_ask),
        "pm_down_bid_size" => Some(|row| row.pm_down_bid_size),
        "pm_down_ask_size" => Some(|row| row.pm_down_ask_size),
        "pm_lag_secs" => Some(|row| row.pm_lag_secs),
        "cum_obi_delta_5m" => Some(|row| row.cum_obi_delta_5m),
        "cum_depth_delta_5m" => Some(|row| row.cum_depth_delta_5m),
        "cum_mprice_drift_5m" => Some(|row| row.cum_mprice_drift_5m),
        "cum_trade_imbalance_5m" => Some(|row| row.cum_trade_imbalance_5m),
        "cex_bar_return_30s" => Some(|row| row.cex_bar_return_30s),
        "cex_bar_return_60s" => Some(|row| row.cex_bar_return_60s),
        "cex_bar_volume_ratio_30s" => Some(|row| row.cex_bar_volume_ratio_30s),
        "cex_bar_volume_trend_3" => Some(|row| row.cex_bar_volume_trend_3),
        "cex_signed_volume_ratio_30s" => Some(|row| row.cex_signed_volume_ratio_30s),
        "cex_consecutive_up_bars" => Some(|row| row.cex_consecutive_up_bars),
        "cex_consecutive_down_bars" => Some(|row| row.cex_consecutive_down_bars),
        "cex_breakout_volume_score" => Some(|row| row.cex_breakout_volume_score),
        _ => None,
    }
}

fn extract_registered_feature(row: &FactorObservation, name: &str) -> Result<f32> {
    let accessor = registered_feature_accessor(name)
        .with_context(|| format!("feature {name:?} is not registered for binary research"))?;
    let value = accessor(row);
    ensure!(
        value.is_finite(),
        "snapshot feature {name:?} is non-finite for event {:?} at {}",
        row.event_id,
        row.tick_ts
    );
    let value = value as f32;
    ensure!(
        value.is_finite(),
        "snapshot feature {name:?} exceeds f32 range for event {:?} at {}",
        row.event_id,
        row.tick_ts
    );
    Ok(value)
}

fn validate_split(split: &MaterializedBinarySplit<'_>, input_dim: usize) -> Result<()> {
    ensure!(!split.train.is_empty(), "training partition is empty");
    ensure!(
        !split.validation.is_empty(),
        "validation partition is empty"
    );
    let train_events = validate_partition("train", &split.train, input_dim)?;
    let validation_events = validate_partition("validation", &split.validation, input_dim)?;
    if let Some(overlap) = train_events.intersection(&validation_events).next() {
        bail!("event-disjoint split violation: event {overlap:?} appears in train and validation");
    }
    let boundary_ms = split.contract.time_boundary.boundary_ms;
    if let Some(sample) = split
        .train
        .iter()
        .find(|sample| sample.settlement_exclusive_upper_bound_ms > boundary_ms)
    {
        bail!(
            "training event crosses shared time boundary: event {:?} end is in [{}, {}) but must be before {}",
            sample.event_id,
            sample.settlement_at_ms,
            sample.settlement_exclusive_upper_bound_ms,
            boundary_ms
        );
    }
    if let Some(sample) = split
        .validation
        .iter()
        .find(|sample| sample.event_start_at_ms < boundary_ms)
    {
        bail!(
            "validation event crosses shared time boundary: event {:?} starts at {} but must start at or after {}",
            sample.event_id,
            sample.event_start_at_ms,
            boundary_ms
        );
    }
    let latest_training_label_us = split
        .train
        .iter()
        .map(|sample| sample.label_observed_at_us)
        .max()
        .expect("non-empty training partition was checked above");
    let earliest_validation_decision_us = split
        .validation
        .iter()
        .map(|sample| {
            sample
                .decision_at_ms
                .checked_mul(1_000)
                .context("validation decision timestamp overflows microseconds")
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .min()
        .expect("non-empty validation partition was checked above");
    ensure!(
        latest_training_label_us <= earliest_validation_decision_us,
        "OOS cutoff violation: a training label was observed after validation decisions begin"
    );
    ensure!(
        split.train.iter().any(|sample| sample.outcome)
            && split.train.iter().any(|sample| !sample.outcome),
        "training partition must contain both binary outcomes"
    );
    Ok(())
}

fn validate_partition<'a>(
    partition: &str,
    samples: &'a [MaterializedBinarySample],
    input_dim: usize,
) -> Result<HashSet<&'a str>> {
    let mut events = HashSet::new();
    let mut event_contracts: HashMap<&str, (i64, i64, i64, i64, bool)> = HashMap::new();
    for (row, sample) in samples.iter().enumerate() {
        ensure!(
            !sample.event_id.trim().is_empty(),
            "{partition} row {row} has an empty event_id"
        );
        ensure!(
            sample.event_id.trim() == sample.event_id,
            "{partition} row {row} event_id contains surrounding whitespace"
        );
        ensure!(
            sample.features.len() == input_dim,
            "{partition} row {row} feature width {} does not match schema width {input_dim}",
            sample.features.len()
        );
        ensure!(
            sample.features.iter().all(|value| value.is_finite()),
            "{partition} row {row} contains a non-finite feature"
        );
        ensure!(
            sample.decision_at_ms < sample.settlement_at_ms,
            "settlement cutoff violation in {partition} row {row}: decision is not before settlement"
        );
        ensure!(
            sample.event_start_at_ms < sample.settlement_at_ms,
            "event interval violation in {partition} row {row}: event start is not before settlement"
        );
        let settlement_at_us = sample
            .settlement_at_ms
            .checked_mul(1_000)
            .context("settlement timestamp overflows microseconds")?;
        ensure!(
            sample.label_observed_at_us >= settlement_at_us,
            "settlement label violation in {partition} row {row}: official outcome was observed before settlement"
        );
        let event_id = sample.event_id.as_str();
        if let Some((
            event_start_at_ms,
            settlement_at_ms,
            settlement_exclusive_upper_bound_ms,
            label_observed_at_us,
            outcome,
        )) = event_contracts.get(event_id)
        {
            ensure!(
                *event_start_at_ms == sample.event_start_at_ms
                    && *settlement_at_ms == sample.settlement_at_ms
                    && *settlement_exclusive_upper_bound_ms
                        == sample.settlement_exclusive_upper_bound_ms
                    && *label_observed_at_us == sample.label_observed_at_us
                    && *outcome == sample.outcome,
                "inconsistent settlement contract for event {event_id:?} in {partition}"
            );
        } else {
            event_contracts.insert(
                event_id,
                (
                    sample.event_start_at_ms,
                    sample.settlement_at_ms,
                    sample.settlement_exclusive_upper_bound_ms,
                    sample.label_observed_at_us,
                    sample.outcome,
                ),
            );
        }
        events.insert(event_id);
    }
    Ok(events)
}

fn materialize_split<'a>(
    split: &'a EventDisjointBinarySplit,
    snapshot: &ResearchSnapshot,
) -> Result<MaterializedBinarySplit<'a>> {
    let target_horizon_seconds = i64::from(split.contract.target_horizon_seconds);
    let target_horizon_ms = target_horizon_seconds
        .checked_mul(1_000)
        .context("binary target horizon overflows milliseconds")?;
    let mut governed_rows = HashMap::<(&str, i64), &FactorObservation>::new();
    for row in &snapshot.observations {
        let decision_at_ms = row.tick_ts.timestamp_millis();
        ensure!(
            governed_rows
                .insert((row.event_id.as_str(), decision_at_ms), row)
                .is_none(),
            "bound prediction snapshot contains duplicate decision row {:?} at {decision_at_ms}",
            row.event_id
        );
    }

    let materialize_partition = |partition: &str,
                                 selectors: &[BinaryDecisionRow]|
     -> Result<Vec<_>> {
        let mut seen = HashSet::with_capacity(selectors.len());
        selectors
                .iter()
                .enumerate()
                .map(|(index, selector)| {
                    ensure!(
                        !selector.event_id.trim().is_empty()
                            && selector.event_id.trim() == selector.event_id,
                        "{partition} row {index} has an invalid event_id"
                    );
                    ensure!(
                        seen.insert((selector.event_id.as_str(), selector.decision_at_ms)),
                        "{partition} contains duplicate decision selector {:?} at {}",
                        selector.event_id,
                        selector.decision_at_ms
                    );
                    let snapshot_row = governed_rows
                        .get(&(selector.event_id.as_str(), selector.decision_at_ms))
                        .copied()
                        .with_context(|| {
                            format!(
                                "{partition} row {index} is not an exact decision row in the bound prediction snapshot"
                            )
                        })?;
                    ensure!(
                        snapshot_row.event_window_secs == target_horizon_seconds,
                        "snapshot event window {} does not match product-family horizon {} for event {:?}",
                        snapshot_row.event_window_secs,
                        target_horizon_seconds,
                        snapshot_row.event_id
                    );
                    ensure!(
                        snapshot_row.time_remaining_secs > 0,
                        "snapshot decision is not before settlement for event {:?}",
                        snapshot_row.event_id
                    );
                    let settlement_at_ms = selector
                        .decision_at_ms
                        .checked_add(
                            snapshot_row
                                .time_remaining_secs
                                .checked_mul(1_000)
                                .context("snapshot settlement offset overflow")?,
                        )
                        .context("snapshot settlement timestamp overflow")?;
                    // `decision_at_ms` floors the nanosecond tick timestamp and
                    // `time_remaining_secs` truncates a positive sub-second remainder.
                    // Add one second plus one millisecond for a strict exclusive upper
                    // bound so neither truncation can admit a crossing training event.
                    let settlement_exclusive_upper_bound_ms = settlement_at_ms
                        .checked_add(1_001)
                        .context("snapshot settlement upper bound overflow")?;
                    let event_start_at_ms = settlement_at_ms
                        .checked_sub(target_horizon_ms)
                        .context("snapshot event start timestamp overflow")?;
                    let outcome = match snapshot_row.settlement_up {
                        0.0 => false,
                        1.0 => true,
                        _ => bail!(
                            "snapshot event {} has a non-binary label",
                            snapshot_row.event_id
                        ),
                    };
                    let label_observed_at_us = snapshot_row
                        .official_resolution_observed_at
                        .context("snapshot row is missing official resolution observation clock")?
                        .timestamp_micros();
                    let features = split
                        .contract
                        .feature_names
                        .iter()
                        .map(|name| extract_registered_feature(snapshot_row, name))
                        .collect::<Result<Vec<_>>>()?;
                    Ok(MaterializedBinarySample {
                        event_id: selector.event_id.clone(),
                        event_start_at_ms,
                        decision_at_ms: selector.decision_at_ms,
                        settlement_at_ms,
                        settlement_exclusive_upper_bound_ms,
                        label_observed_at_us,
                        features,
                        outcome,
                    })
                })
                .collect()
    };

    Ok(MaterializedBinarySplit {
        contract: &split.contract,
        train: materialize_partition("train", &split.train)?,
        validation: materialize_partition("validation", &split.validation)?,
    })
}

fn normalized_flattened_features(
    samples: &[MaterializedBinarySample],
    normalizer: &FeatureNormalizer,
) -> Result<Vec<f32>> {
    let mut flattened = Vec::with_capacity(samples.len() * normalizer.means.len());
    for sample in samples {
        flattened.extend(normalizer.transform(&sample.features)?);
    }
    Ok(flattened)
}

fn predict_probabilities(
    model: &BurnBinaryLinear<InferenceBackend>,
    normalizer: &FeatureNormalizer,
    feature_rows: &[Vec<f32>],
) -> Result<Vec<f64>> {
    ensure!(!feature_rows.is_empty(), "prediction batch is empty");
    let mut flattened = Vec::with_capacity(feature_rows.len() * normalizer.means.len());
    for features in feature_rows {
        flattened.extend(normalizer.transform(features)?);
    }

    let device = Default::default();
    let features = Tensor::<InferenceBackend, 2>::from_data(
        TensorData::new(flattened, [feature_rows.len(), normalizer.means.len()]),
        &device,
    );
    let probabilities = sigmoid(model.forward(features))
        .into_data()
        .to_vec::<f32>()
        .context("read Burn binary probabilities")?;
    ensure!(
        probabilities.len() == feature_rows.len(),
        "Burn output row count does not match prediction input"
    );
    probabilities
        .into_iter()
        .enumerate()
        .map(|(row, probability)| {
            ensure!(
                probability.is_finite() && (0.0..=1.0).contains(&probability),
                "invalid probability at row {row}"
            );
            Ok(f64::from(probability))
        })
        .collect()
}

fn compute_metrics(
    samples: &[MaterializedBinarySample],
    probabilities: &[f64],
    epsilon: f64,
) -> Result<BinaryOosMetrics> {
    ensure!(
        samples.len() == probabilities.len() && !samples.is_empty(),
        "metric input lengths are invalid"
    );
    let mut squared_error = 0.0;
    let mut negative_log_likelihood = 0.0;
    let mut correct = 0usize;
    for (sample, probability) in samples.iter().zip(probabilities) {
        ensure!(
            probability.is_finite() && (0.0..=1.0).contains(probability),
            "metric input contains an invalid probability"
        );
        let label = f64::from(sample.outcome);
        squared_error += (probability - label).powi(2);
        let clamped = probability.clamp(epsilon, 1.0 - epsilon);
        negative_log_likelihood -= label * clamped.ln() + (1.0 - label) * (1.0 - clamped).ln();
        if (*probability >= 0.5) == sample.outcome {
            correct += 1;
        }
    }
    let count = samples.len() as f64;
    Ok(BinaryOosMetrics {
        sample_count: samples.len(),
        event_count: event_count(samples),
        brier_score: squared_error / count,
        log_loss: negative_log_likelihood / count,
        accuracy: correct as f64 / count,
    })
}

fn event_count(samples: &[MaterializedBinarySample]) -> usize {
    samples
        .iter()
        .map(|sample| sample.event_id.as_str())
        .collect::<HashSet<_>>()
        .len()
}

#[derive(Deserialize)]
struct BurnpackMetadataView {
    #[serde(default)]
    metadata: BTreeMap<String, String>,
}

fn validate_burnpack_metadata(path: &Path, manifest: &BinaryModelManifest) -> Result<()> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut header = [0_u8; BURNPACK_HEADER_BYTES];
    file.read_exact(&mut header)
        .with_context(|| format!("read Burnpack header {}", path.display()))?;
    ensure!(
        u32::from_le_bytes(header[0..4].try_into().expect("fixed header range")) == BURNPACK_MAGIC,
        "invalid Burnpack magic"
    );
    ensure!(
        u16::from_le_bytes(header[4..6].try_into().expect("fixed header range"))
            == BURNPACK_FORMAT_VERSION,
        "unsupported Burnpack format version"
    );
    let metadata_size =
        u32::from_le_bytes(header[6..10].try_into().expect("fixed header range")) as usize;
    ensure!(
        metadata_size <= MAX_BURNPACK_METADATA_BYTES,
        "Burnpack metadata exceeds research size limit"
    );
    let mut metadata_bytes = vec![0_u8; metadata_size];
    file.read_exact(&mut metadata_bytes)
        .with_context(|| format!("read Burnpack metadata {}", path.display()))?;
    let metadata: BurnpackMetadataView =
        ciborium::de::from_reader(metadata_bytes.as_slice()).context("decode Burnpack metadata")?;

    let expected = [
        ("artifact_scope", "research_only".to_owned()),
        ("dataset_sha256", manifest.dataset_sha256.clone()),
        (
            "snapshot_contract_hash",
            manifest.dataset_contract.snapshot_contract_hash.clone(),
        ),
        (
            "mission_sha256",
            manifest.dataset_contract.mission_sha256.clone(),
        ),
        (
            "feature_schema_sha256",
            manifest.dataset_contract.feature_schema_sha256.clone(),
        ),
        ("manifest_schema", MANIFEST_SCHEMA_VERSION.to_string()),
    ];
    for (key, expected_value) in expected {
        ensure!(
            metadata.metadata.get(key) == Some(&expected_value),
            "Burnpack metadata {key:?} does not match the trusted manifest"
        );
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex_digest(hasher.finalize().as_slice()))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_digest(hasher.finalize().as_slice())
}

fn prefixed_sha256_bytes(bytes: &[u8]) -> String {
    format!("sha256:{}", sha256_bytes(bytes))
}

fn feature_schema_sha256(feature_names: &[String]) -> Result<String> {
    validate_feature_names(feature_names)?;
    let bytes = serde_json::to_vec(feature_names).context("serialize ordered feature schema")?;
    Ok(prefixed_sha256_bytes(&bytes))
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("write to String cannot fail");
    }
    encoded
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_prefixed_sha256(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(is_sha256)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        prediction_loop::{
            research_brief_snapshot_id, PredictionSearchBudget, PREDICTION_LOOP_TARGET,
            PREDICTION_MISSION_SCHEMA_VERSION, REQUIRED_BINANCE_DATA_REQUIREMENTS,
            REQUIRED_BINANCE_SOURCE_SURFACES, REQUIRED_CHAINLINK_SOURCE_SURFACE,
            REQUIRED_POLYMARKET_SOURCE_SURFACE, REQUIRED_SETTLEMENT_SOURCE_SURFACE,
        },
        research_snapshot::{
            write_research_snapshot, ResearchSnapshotArtifacts, ResearchSnapshotInputArtifact,
            ResearchSnapshotPhaseTiming, ResearchSnapshotPmBookSource, ResearchSnapshotRowCounts,
            ResearchSnapshotSourceSurface,
        },
        FactorObservation, ResearchPmBookLevel, ResearchPmBookSnapshot,
    };
    use chrono::{Duration, TimeZone, Utc};

    struct TestContext {
        mission: PredictionResearchMission,
        snapshot: VerifiedBinarySnapshot,
        split: EventDisjointBinarySplit,
    }

    #[derive(Clone)]
    struct TestDecision {
        event_id: String,
        decision_at_ms: i64,
        settlement_at_ms: i64,
        features: [f64; 2],
        outcome: bool,
    }

    fn sample(partition: &str, index: usize, outcome: bool) -> TestDecision {
        let base = 1_700_000_000_000_i64 + index as i64 * 60_000;
        let direction = if outcome { 1.0 } else { -1.0 };
        let decision_at_ms = base + 1_000;
        let settlement_at_ms = base + 50_000;
        TestDecision {
            event_id: format!("{partition}-event-{index}"),
            decision_at_ms,
            settlement_at_ms,
            features: [direction + index as f64 * 0.001, direction * 0.5],
            outcome,
        }
    }

    fn selector(sample: &TestDecision) -> BinaryDecisionRow {
        BinaryDecisionRow {
            event_id: sample.event_id.clone(),
            decision_at_ms: sample.decision_at_ms,
        }
    }

    fn feature_names() -> Vec<String> {
        vec!["cex_bar_return_30s".to_owned(), "obi".to_owned()]
    }

    fn time_boundary(train: &[TestDecision], validation: &[TestDecision]) -> BinaryTimeBoundary {
        let latest_training_event_end = train
            .iter()
            .map(|sample| sample.settlement_at_ms)
            .max()
            .expect("non-empty training decisions");
        let earliest_validation_event_start = validation
            .iter()
            .map(|sample| sample.settlement_at_ms - PREDICTION_EVENT_WINDOW_SECS * 1_000)
            .min()
            .expect("non-empty validation decisions");
        assert!(latest_training_event_end < earliest_validation_event_start);
        BinaryTimeBoundary {
            boundary_ms: earliest_validation_event_start,
        }
    }

    fn mission() -> PredictionResearchMission {
        let mut mission = PredictionResearchMission {
            schema_version: PREDICTION_MISSION_SCHEMA_VERSION.to_owned(),
            mission_id: "polymarket-btc-5m-burn-v1".to_owned(),
            lane: "prediction_market".to_owned(),
            objective: "Estimate official BTC five-minute settlement probability.".to_owned(),
            hypothesis_scope:
                "Test Binance flow and Polymarket depth with official settlement labels.".to_owned(),
            mutable_scope: vec!["probability_blend_weights".to_owned()],
            data_snapshot_id: format!("sha256:{}", "1".repeat(64)),
            target: PREDICTION_LOOP_TARGET.to_owned(),
            symbols: vec!["BTC".to_owned()],
            horizon: "5m".to_owned(),
            time_cohort_boundary_ms: 1_700_001_000_000,
            prompt_snapshot_id: String::new(),
            search_policy_snapshot_id: format!("sha256:{}", "2".repeat(64)),
            search_budget: PredictionSearchBudget {
                max_candidates: 6,
                max_llm_calls: 2,
                max_seconds: 900,
            },
        };
        mission.prompt_snapshot_id = research_brief_snapshot_id(&mission);
        mission
    }

    fn observation(sample: &TestDecision) -> FactorObservation {
        let tick_ts = Utc
            .timestamp_millis_opt(sample.decision_at_ms)
            .single()
            .expect("valid test timestamp");
        FactorObservation {
            event_id: sample.event_id.clone(),
            symbol: "BTCUSDT".to_owned(),
            tick_ts,
            up_token_id: format!("{}-up", sample.event_id),
            down_token_id: format!("{}-down", sample.event_id),
            chainlink_reference_fresh: true,
            binance_spot_fresh: true,
            binance_lob_fresh: true,
            binance_agg_trade_fresh: true,
            event_window_secs: PREDICTION_EVENT_WINDOW_SECS,
            time_remaining_secs: (sample.settlement_at_ms - sample.decision_at_ms) / 1_000,
            signed_distance_to_beat: 0.0,
            abs_distance_to_beat: 0.0,
            drift_10s: 0.0,
            drift_30s: 0.0,
            flip_age_secs: 0.0,
            post_flip_drift: 0.0,
            sigma_horizon: 0.01,
            fair_prob_up: 0.5,
            fair_prob_up_clean: 0.5,
            prob_disagreement: 0.0,
            implied_sigma_horizon: 0.01,
            vol_gap: 0.0,
            distance_over_sigma: 0.0,
            model_prob_up: 0.5,
            chainlink_prob_up: 0.5,
            model_edge_up: 0.0,
            reward_risk_up: 1.0,
            reward_risk_down: 1.0,
            obi: sample.features[1],
            spread_bps: 1.0,
            microprice_offset_bps: 0.0,
            bid_depth_near: 10.0,
            ask_depth_near: 10.0,
            depth_ratio: 1.0,
            depth_imbalance: 0.0,
            depth_far_ratio: 1.0,
            depth_acceleration: 0.0,
            obi_10: 0.0,
            pm_up_bid: 0.49,
            pm_up_ask: 0.51,
            pm_up_bid_size: 10.0,
            pm_up_ask_size: 10.0,
            pm_down_bid: 0.49,
            pm_down_ask: 0.51,
            pm_down_bid_size: 10.0,
            pm_down_ask_size: 10.0,
            pm_lag_secs: 0.0,
            settlement_up: f64::from(sample.outcome),
            official_resolution_observed_at: Utc
                .timestamp_millis_opt(sample.settlement_at_ms + 100)
                .single(),
            future_up_ask_change_30s: None,
            future_up_ask_change_60s: None,
            cum_obi_delta_5m: 0.0,
            cum_depth_delta_5m: 0.0,
            cum_mprice_drift_5m: 0.0,
            cum_trade_imbalance_5m: 0.0,
            cex_bar_return_30s: sample.features[0],
            cex_bar_return_60s: 0.0,
            cex_bar_volume_ratio_30s: 0.0,
            cex_bar_volume_trend_3: 0.0,
            cex_signed_volume_ratio_30s: 0.0,
            cex_consecutive_up_bars: 0.0,
            cex_consecutive_down_bars: 0.0,
            cex_breakout_volume_score: 0.0,
        }
    }

    fn books(sample: &TestDecision) -> Vec<ResearchPmBookSnapshot> {
        let tick_ts = Utc
            .timestamp_millis_opt(sample.decision_at_ms)
            .single()
            .expect("valid test timestamp");
        [("up", "up"), ("down", "down")]
            .into_iter()
            .map(|(token_suffix, side)| ResearchPmBookSnapshot {
                event_id: sample.event_id.clone(),
                token_id: format!("{}-{token_suffix}", sample.event_id),
                side: side.to_owned(),
                ts: tick_ts,
                bids: vec![ResearchPmBookLevel {
                    price: 0.49,
                    size: 10.0,
                }],
                asks: vec![ResearchPmBookLevel {
                    price: 0.51,
                    size: 10.0,
                }],
            })
            .collect()
    }

    fn raw_snapshot(samples: &[TestDecision]) -> ResearchSnapshot {
        let generated_at_ms = samples
            .iter()
            .map(|sample| sample.settlement_at_ms + 100)
            .max()
            .expect("non-empty test decisions");
        let generated_at = Utc
            .timestamp_millis_opt(generated_at_ms)
            .single()
            .expect("valid test timestamp");
        let observations = samples.iter().map(observation).collect::<Vec<_>>();
        let pm_book_snapshots = samples.iter().flat_map(books).collect::<Vec<_>>();
        ResearchSnapshot {
            manifest: crate::ResearchSnapshotManifest {
                schema_version: crate::RESEARCH_SNAPSHOT_SCHEMA_VERSION.to_owned(),
                snapshot_hash: Some("0123456789abcdef".to_owned()),
                snapshot_contract_hash: Some(format!("sha256:{}", "1".repeat(64))),
                generated_at,
                git_sha: None,
                symbols: vec!["BTCUSDT".to_owned()],
                start: generated_at - Duration::days(1),
                end: generated_at + Duration::days(2),
                history_start: generated_at - Duration::days(3),
                lob_sample_secs: 1,
                pm_book_sample_secs: Some(1),
                observation_sample_secs: 1,
                max_quote_age_secs: 30,
                stake_usd: 15.0,
                require_official_settlement: true,
                immutable_input: true,
                source_kind: "unit_test".to_owned(),
                optimizer_data_dir: Some("unit-test-immutable-source".to_owned()),
                source_surfaces: REQUIRED_BINANCE_SOURCE_SURFACES
                    .iter()
                    .copied()
                    .chain([
                        REQUIRED_CHAINLINK_SOURCE_SURFACE,
                        REQUIRED_POLYMARKET_SOURCE_SURFACE,
                        REQUIRED_SETTLEMENT_SOURCE_SURFACE,
                    ])
                    .map(|name| ResearchSnapshotSourceSurface {
                        name: name.to_owned(),
                        role: match name {
                            REQUIRED_CHAINLINK_SOURCE_SURFACE => {
                                "opening_reference_and_expiry_price_source".to_owned()
                            }
                            REQUIRED_POLYMARKET_SOURCE_SURFACE => {
                                "execution_depth_context".to_owned()
                            }
                            REQUIRED_SETTLEMENT_SOURCE_SURFACE => "settlement_labels".to_owned(),
                            _ => "binance_prediction_context".to_owned(),
                        },
                        gate_category: if name == REQUIRED_POLYMARKET_SOURCE_SURFACE {
                            "required_for_execution".to_owned()
                        } else {
                            "required_for_prediction".to_owned()
                        },
                        raw_full_fidelity: name == REQUIRED_SETTLEMENT_SOURCE_SURFACE,
                        snapshot_sampled: name != REQUIRED_SETTLEMENT_SOURCE_SURFACE,
                        sample_secs: (name != REQUIRED_SETTLEMENT_SOURCE_SURFACE).then_some(1),
                        row_count: (name != REQUIRED_SETTLEMENT_SOURCE_SURFACE)
                            .then_some(samples.len()),
                        notes: "point-in-time governed test surface".to_owned(),
                    })
                    .collect(),
                input_artifacts: vec![ResearchSnapshotInputArtifact {
                    name: "unit".to_owned(),
                    path: "unit".to_owned(),
                    content_hash: None,
                    row_count: Some(samples.len()),
                }],
                data_requirements: REQUIRED_BINANCE_DATA_REQUIREMENTS
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
                data_audit_status: Some("ok".to_owned()),
                data_audit_report: Some("audit.json".to_owned()),
                include_deribit: false,
                artifacts: ResearchSnapshotArtifacts::default(),
                row_counts: ResearchSnapshotRowCounts {
                    observations: observations.len(),
                    deribit_snapshots: 0,
                    pm_book_snapshots: pm_book_snapshots.len(),
                },
                phase_timings: vec![ResearchSnapshotPhaseTiming {
                    phase: "unit".to_owned(),
                    elapsed_ms: 1,
                    rows: Some(samples.len()),
                }],
                quality_flags: vec![],
                pm_book_source: ResearchSnapshotPmBookSource::default(),
            },
            observations,
            deribit_snapshots: vec![],
            pm_book_snapshots,
        }
    }

    fn snapshot(samples: &[TestDecision]) -> VerifiedBinarySnapshot {
        VerifiedBinarySnapshot::from_test_snapshot(raw_snapshot(samples))
    }

    fn context() -> TestContext {
        let train = (0..12)
            .map(|index| sample("train", index, index % 2 == 0))
            .collect::<Vec<_>>();
        let validation = (20..26)
            .map(|index| sample("validation", index, index % 2 == 0))
            .collect::<Vec<_>>();
        let all_samples = train.iter().chain(&validation).cloned().collect::<Vec<_>>();
        let mut mission = mission();
        mission.time_cohort_boundary_ms = time_boundary(&train, &validation).boundary_ms;
        let snapshot = snapshot(&all_samples);
        let contract =
            BinaryDatasetContract::from_prediction_snapshot(&snapshot, &mission, feature_names())
                .expect("valid governed test contract");
        TestContext {
            mission,
            snapshot,
            split: EventDisjointBinarySplit {
                contract,
                train: train.iter().map(selector).collect(),
                validation: validation.iter().map(selector).collect(),
            },
        }
    }

    fn validation_rows(context: &TestContext) -> Vec<Vec<f32>> {
        materialize_split(&context.split, context.snapshot.snapshot())
            .expect("valid governed test split")
            .validation
            .into_iter()
            .map(|sample| sample.features)
            .collect()
    }

    fn test_config() -> BinaryTrainingConfig {
        BinaryTrainingConfig {
            seed: 7,
            epochs: 120,
            learning_rate: 0.05,
            log_loss_epsilon: 1.0e-7,
        }
    }

    #[test]
    fn rejects_event_overlap_between_train_and_validation() {
        let mut context = context();
        context.split.validation[0] = context.split.train[0].clone();

        let error = train_event_disjoint_binary(
            &context.snapshot,
            &context.mission,
            &context.split,
            test_config(),
        )
        .expect_err("overlapping event must fail");
        assert!(error.to_string().contains("event-disjoint split violation"));
    }

    #[test]
    fn rejects_training_event_that_touches_shared_time_boundary() {
        let mut context = context();
        let last_train = context.split.train.last().unwrap();
        let row = context
            .snapshot
            .snapshot
            .observations
            .iter()
            .find(|row| row.event_id == last_train.event_id)
            .unwrap();
        context.mission.time_cohort_boundary_ms =
            row.tick_ts.timestamp_millis() + row.time_remaining_secs * 1_000;
        context.split.contract = BinaryDatasetContract::from_prediction_snapshot(
            &context.snapshot,
            &context.mission,
            feature_names(),
        )
        .unwrap();

        let error = train_event_disjoint_binary(
            &context.snapshot,
            &context.mission,
            &context.split,
            test_config(),
        )
        .expect_err("training event ending at the shared boundary must be purged");
        assert!(error
            .to_string()
            .contains("training event crosses shared time boundary"));
    }

    #[test]
    fn rejects_training_event_that_may_cross_boundary_after_second_truncation() {
        let mut context = context();
        let last_train = context.split.train.last().unwrap();
        let row = context
            .snapshot
            .snapshot
            .observations
            .iter()
            .find(|row| row.event_id == last_train.event_id)
            .unwrap();
        let truncated_event_end = row.tick_ts.timestamp_millis() + row.time_remaining_secs * 1_000;
        context.mission.time_cohort_boundary_ms = truncated_event_end + 500;
        context.split.contract = BinaryDatasetContract::from_prediction_snapshot(
            &context.snapshot,
            &context.mission,
            feature_names(),
        )
        .unwrap();

        let error = train_event_disjoint_binary(
            &context.snapshot,
            &context.mission,
            &context.split,
            test_config(),
        )
        .expect_err("sub-second uncertainty must not round a training event before the boundary");
        assert!(error
            .to_string()
            .contains("training event crosses shared time boundary"));
    }

    #[test]
    fn rejects_training_event_that_crosses_boundary_after_nanosecond_and_second_truncation() {
        let mut context = context();
        let last_train = context.split.train.last().unwrap().clone();
        let boundary_ms = last_train.decision_at_ms + 2_000;
        let row = context
            .snapshot
            .snapshot
            .observations
            .iter_mut()
            .find(|row| row.event_id == last_train.event_id)
            .unwrap();
        row.tick_ts = Utc
            .timestamp_millis_opt(last_train.decision_at_ms)
            .single()
            .unwrap()
            + Duration::microseconds(500);
        row.time_remaining_secs = 1;
        context.mission.time_cohort_boundary_ms = boundary_ms;
        context.split.contract = BinaryDatasetContract::from_prediction_snapshot(
            &context.snapshot,
            &context.mission,
            feature_names(),
        )
        .unwrap();

        let error = train_event_disjoint_binary(
            &context.snapshot,
            &context.mission,
            &context.split,
            test_config(),
        )
        .expect_err("combined timestamp truncation must not admit a crossing event");
        assert!(error
            .to_string()
            .contains("training event crosses shared time boundary"));
    }

    #[test]
    fn rejects_validation_event_that_starts_before_shared_time_boundary() {
        let mut context = context();
        let first_validation = context.split.validation.first().unwrap();
        context.mission.time_cohort_boundary_ms = first_validation.decision_at_ms;
        context.split.contract = BinaryDatasetContract::from_prediction_snapshot(
            &context.snapshot,
            &context.mission,
            feature_names(),
        )
        .unwrap();

        let error = train_event_disjoint_binary(
            &context.snapshot,
            &context.mission,
            &context.split,
            test_config(),
        )
        .expect_err("validation event overlapping the training cohort must fail");
        assert!(error
            .to_string()
            .contains("validation event crosses shared time boundary"));
    }

    #[test]
    fn rejects_snapshot_row_outside_the_product_family_horizon() {
        let mut context = context();
        context.snapshot.snapshot.observations[0].event_window_secs = 900;

        let error = train_event_disjoint_binary(
            &context.snapshot,
            &context.mission,
            &context.split,
            test_config(),
        )
        .expect_err("mixed-horizon rows must not enter a product-family model");
        assert!(format!("{error:#}").contains("horizon"));
    }

    #[test]
    fn rejects_dataset_time_boundary_that_differs_from_the_mission() {
        let mut context = context();
        context.split.contract.time_boundary.boundary_ms += 1;

        let error = train_event_disjoint_binary(
            &context.snapshot,
            &context.mission,
            &context.split,
            test_config(),
        )
        .expect_err("caller-selected boundary must not drift from the mission");
        assert!(error
            .to_string()
            .contains("time boundary differs from prediction mission"));
    }

    #[test]
    fn rejects_stale_non_finite_and_invalid_settlement_snapshot_rows() {
        let mut stale = context();
        stale.snapshot.snapshot.observations[0].binance_lob_fresh = false;
        let error = train_event_disjoint_binary(
            &stale.snapshot,
            &stale.mission,
            &stale.split,
            test_config(),
        )
        .expect_err("stale feature must fail");
        assert!(format!("{error:#}").contains("fresh Binance L2 context"));

        let mut non_finite = context();
        non_finite.snapshot.snapshot.observations[0].cex_bar_return_30s = f64::NAN;
        let error = train_event_disjoint_binary(
            &non_finite.snapshot,
            &non_finite.mission,
            &non_finite.split,
            test_config(),
        )
        .expect_err("non-finite registered feature must fail");
        assert!(error.to_string().contains("snapshot feature"));

        let mut settlement = context();
        settlement.snapshot.snapshot.observations[0].time_remaining_secs = 0;
        let error = train_event_disjoint_binary(
            &settlement.snapshot,
            &settlement.mission,
            &settlement.split,
            test_config(),
        )
        .expect_err("decision at settlement must fail");
        assert!(error
            .to_string()
            .contains("snapshot decision is not before settlement"));
    }

    #[test]
    fn rejects_unregistered_label_features_and_caller_supplied_values() {
        let context = context();
        for forbidden in ["settlement_up", "future_up_ask_change_30s", "unknown"] {
            let error = BinaryDatasetContract::from_prediction_snapshot(
                &context.snapshot,
                &context.mission,
                vec![forbidden.to_owned()],
            )
            .expect_err("labels and unknown fields must not enter the feature registry");
            assert!(error.to_string().contains("not a registered point-in-time"));
        }

        let selector = serde_json::json!({
            "event_id": context.split.train[0].event_id,
            "decision_at_ms": context.split.train[0].decision_at_ms,
            "features": [1.0],
            "outcome": true
        });
        assert!(serde_json::from_value::<BinaryDecisionRow>(selector).is_err());
    }

    #[test]
    fn rejects_training_labels_observed_after_validation_begins() {
        let mut context = context();
        let validation_start = context.split.validation[0].decision_at_ms;
        let train_selector = context.split.train.last().unwrap();
        let row = context
            .snapshot
            .snapshot
            .observations
            .iter_mut()
            .find(|row| row.event_id == train_selector.event_id)
            .unwrap();
        row.official_resolution_observed_at =
            Utc.timestamp_micros(validation_start * 1_000 + 1).single();

        let error = train_event_disjoint_binary(
            &context.snapshot,
            &context.mission,
            &context.split,
            test_config(),
        )
        .expect_err("future training label must not validate past decisions");
        assert!(error.to_string().contains("OOS cutoff violation"));
    }

    #[test]
    fn rejects_duplicate_decision_selectors() {
        let mut context = context();
        context.split.train.push(context.split.train[0].clone());

        let error = train_event_disjoint_binary(
            &context.snapshot,
            &context.mission,
            &context.split,
            test_config(),
        )
        .expect_err("duplicate selectors must not reweight governed observations");
        assert!(error.to_string().contains("duplicate decision selector"));
    }

    #[test]
    fn written_snapshot_must_reload_with_trusted_contract_before_training() {
        let train = (0..12)
            .map(|index| sample("train", index, index % 2 == 0))
            .collect::<Vec<_>>();
        let validation = (20..26)
            .map(|index| sample("validation", index, index % 2 == 0))
            .collect::<Vec<_>>();
        let all_samples = train.iter().chain(&validation).cloned().collect::<Vec<_>>();
        let temp = tempfile::tempdir().unwrap();
        let written = write_research_snapshot(temp.path(), raw_snapshot(&all_samples)).unwrap();
        let trusted_contract = written
            .snapshot_contract_hash
            .clone()
            .expect("writer emits evaluator contract SHA-256");

        assert_eq!(written.snapshot_hash.as_deref().unwrap().len(), 16);
        let mismatch =
            VerifiedBinarySnapshot::load(temp.path(), &format!("sha256:{}", "f".repeat(64)))
                .expect_err("registry digest mismatch must fail closed");
        assert!(mismatch
            .to_string()
            .contains("trusted evaluator-contract digest"));

        let snapshot = VerifiedBinarySnapshot::load(temp.path(), &trusted_contract).unwrap();
        let mut mission = mission();
        mission.data_snapshot_id = trusted_contract;
        mission.time_cohort_boundary_ms = time_boundary(&train, &validation).boundary_ms;
        let split = EventDisjointBinarySplit {
            contract: BinaryDatasetContract::from_prediction_snapshot(
                &snapshot,
                &mission,
                feature_names(),
            )
            .unwrap(),
            train: train.iter().map(selector).collect(),
            validation: validation.iter().map(selector).collect(),
        };

        let model =
            train_event_disjoint_binary(&snapshot, &mission, &split, test_config()).unwrap();
        assert_eq!(
            model.manifest().dataset_contract.snapshot_contract_hash,
            mission.data_snapshot_id
        );
        assert_eq!(
            model.manifest().validation_metrics.sample_count,
            validation.len()
        );
    }

    #[test]
    fn rejects_rows_not_in_the_bound_snapshot() {
        let mut context = context();
        context.split.train[0].event_id = "unbound-event".to_owned();

        let error = train_event_disjoint_binary(
            &context.snapshot,
            &context.mission,
            &context.split,
            test_config(),
        )
        .expect_err("unbound decision row must fail");
        assert!(error
            .to_string()
            .contains("not an exact decision row in the bound prediction snapshot"));
    }

    #[test]
    fn rejects_snapshot_without_isolated_symbol_or_official_authority() {
        let mut wrong_symbol = context();
        wrong_symbol.snapshot.snapshot.manifest.symbols = vec!["SOLUSDT".to_owned()];
        let error = train_event_disjoint_binary(
            &wrong_symbol.snapshot,
            &wrong_symbol.mission,
            &wrong_symbol.split,
            test_config(),
        )
        .expect_err("cross-symbol snapshot must fail");
        assert!(error.to_string().contains("isolate the mission underlying"));

        let mut unofficial = context();
        unofficial
            .snapshot
            .snapshot
            .manifest
            .require_official_settlement = false;
        let error = train_event_disjoint_binary(
            &unofficial.snapshot,
            &unofficial.mission,
            &unofficial.split,
            test_config(),
        )
        .expect_err("unofficial labels must fail");
        assert!(error.to_string().contains("official settlement labels"));
    }

    #[test]
    fn training_is_seeded_and_reports_oos_metrics() {
        let context = context();
        let model_a = train_event_disjoint_binary(
            &context.snapshot,
            &context.mission,
            &context.split,
            test_config(),
        )
        .unwrap();
        let model_b = train_event_disjoint_binary(
            &context.snapshot,
            &context.mission,
            &context.split,
            test_config(),
        )
        .unwrap();
        let rows = validation_rows(&context);
        let schema_hash = &context.split.contract.feature_schema_sha256;
        let names = &context.split.contract.feature_names;
        let probabilities_a = model_a
            .predict_probabilities(schema_hash, names, &rows)
            .unwrap();
        let probabilities_b = model_b
            .predict_probabilities(schema_hash, names, &rows)
            .unwrap();

        assert_eq!(probabilities_a, probabilities_b);
        assert_eq!(model_a.manifest().training.seed, 7);
        assert_eq!(
            model_a.manifest().artifact_scope,
            BinaryArtifactScope::ResearchOnly
        );
        assert_eq!(model_a.manifest().validation_metrics.sample_count, 6);
        assert_eq!(model_a.manifest().validation_metrics.event_count, 6);
        assert!(model_a.manifest().validation_metrics.brier_score < 0.25);
        assert!(model_a.manifest().validation_metrics.log_loss.is_finite());
        assert!(model_a.manifest().validation_metrics.accuracy >= 0.5);
        assert!(model_a.manifest().model_sha256.is_none());
    }

    #[test]
    fn normalizer_is_fit_from_train_partition_only() {
        let mut context = context();
        let validation_ids = context
            .split
            .validation
            .iter()
            .map(|row| row.event_id.as_str())
            .collect::<HashSet<_>>();
        for observation in &mut context.snapshot.snapshot.observations {
            if validation_ids.contains(observation.event_id.as_str()) {
                observation.cex_bar_return_30s += 10_000.0;
            }
        }
        let materialized = materialize_split(&context.split, context.snapshot.snapshot()).unwrap();
        let expected_train_mean = materialized
            .train
            .iter()
            .map(|sample| f64::from(sample.features[0]))
            .sum::<f64>()
            / materialized.train.len() as f64;

        let model = train_event_disjoint_binary(
            &context.snapshot,
            &context.mission,
            &context.split,
            test_config(),
        )
        .unwrap();
        assert_eq!(model.manifest().normalizer.means[0], expected_train_mean);
    }

    #[test]
    fn prediction_requires_the_ordered_feature_schema_hash() {
        let context = context();
        let model = train_event_disjoint_binary(
            &context.snapshot,
            &context.mission,
            &context.split,
            test_config(),
        )
        .unwrap();
        let rows = validation_rows(&context);
        let mut reversed = context.split.contract.feature_names.clone();
        reversed.reverse();
        let reversed_hash = feature_schema_sha256(&reversed).unwrap();

        let error = model
            .predict_probabilities(&reversed_hash, &reversed, &rows)
            .expect_err("wrong feature schema hash must fail");
        assert!(error.to_string().contains("schema SHA-256"));

        let error = model
            .predict_probabilities(
                &context.split.contract.feature_schema_sha256,
                &reversed,
                &rows,
            )
            .expect_err("wrong feature order must fail");
        assert!(error.to_string().contains("trained ordered schema"));
    }

    #[test]
    fn burnpack_and_typed_manifest_round_trip_predictions() {
        let context = context();
        let rows = validation_rows(&context);
        let mut model = train_event_disjoint_binary(
            &context.snapshot,
            &context.mission,
            &context.split,
            test_config(),
        )
        .unwrap();
        let before = model
            .predict_probabilities(
                &context.split.contract.feature_schema_sha256,
                &context.split.contract.feature_names,
                &rows,
            )
            .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path().join("binary-research-model");

        let digest = model.save_bundle(&bundle).unwrap();
        let burnpack = fs::read(bundle.join(MODEL_FILE)).unwrap();
        assert_eq!(
            u32::from_le_bytes(burnpack[..4].try_into().unwrap()),
            0x4255_524e
        );
        let manifest: BinaryModelManifest =
            serde_json::from_slice(&fs::read(bundle.join(MANIFEST_FILE)).unwrap()).unwrap();
        assert_eq!(manifest.schema_version, MANIFEST_SCHEMA_VERSION);
        assert!(manifest.model_sha256.as_deref().is_some_and(is_sha256));

        let loaded =
            BinaryProbabilityModel::load_bundle(&bundle, &digest, &context.mission).unwrap();
        let after = loaded
            .predict_probabilities(
                &context.split.contract.feature_schema_sha256,
                &context.split.contract.feature_names,
                &rows,
            )
            .unwrap();
        assert_eq!(before, after);
        assert_eq!(loaded.manifest(), &manifest);
    }

    #[test]
    fn load_rejects_registry_rehashed_v2_manifest_without_shared_time_boundary() {
        let context = context();
        let mut model = train_event_disjoint_binary(
            &context.snapshot,
            &context.mission,
            &context.split,
            test_config(),
        )
        .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path().join("binary-research-model");
        let digest = model.save_bundle(&bundle).unwrap();
        let manifest_path = bundle.join(MANIFEST_FILE);
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest["schema_version"] = serde_json::json!(2);
        manifest["dataset_contract"]
            .as_object_mut()
            .unwrap()
            .remove("time_boundary");
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let rehashed = BinaryBundleDigest {
            manifest_sha256: sha256_file(&manifest_path).unwrap(),
            model_sha256: digest.model_sha256,
        };

        let error = BinaryProbabilityModel::load_bundle(&bundle, &rehashed, &context.mission)
            .expect_err("trusted digest must not make a v2 manifest acceptable");
        assert!(error.to_string().contains("parse binary model manifest"));
    }

    #[test]
    fn load_rejects_tampered_burnpack() {
        let context = context();
        let mut model = train_event_disjoint_binary(
            &context.snapshot,
            &context.mission,
            &context.split,
            test_config(),
        )
        .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path().join("binary-research-model");
        let digest = model.save_bundle(&bundle).unwrap();
        let model_path = bundle.join(MODEL_FILE);
        let mut bytes = fs::read(&model_path).unwrap();
        let last = bytes.last_mut().unwrap();
        *last ^= 0x01;
        fs::write(model_path, bytes).unwrap();

        let error = BinaryProbabilityModel::load_bundle(&bundle, &digest, &context.mission)
            .expect_err("tampered Burnpack must fail");
        assert!(error.to_string().contains("trusted registry digest"));
    }

    #[test]
    fn load_rejects_tampered_manifest_and_cross_mission_bundle() {
        let context = context();
        let mut model = train_event_disjoint_binary(
            &context.snapshot,
            &context.mission,
            &context.split,
            test_config(),
        )
        .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path().join("binary-research-model");
        let digest = model.save_bundle(&bundle).unwrap();

        let mut other_mission = context.mission.clone();
        other_mission.mission_id = "polymarket-btc-5m-other-mission".to_owned();
        let error = BinaryProbabilityModel::load_bundle(&bundle, &digest, &other_mission)
            .expect_err("cross-mission bundle must fail");
        assert!(error.to_string().contains("different prediction mission"));

        let manifest_path = bundle.join(MANIFEST_FILE);
        let mut manifest_bytes = fs::read(&manifest_path).unwrap();
        manifest_bytes.push(b'\n');
        fs::write(&manifest_path, manifest_bytes).unwrap();
        let error = BinaryProbabilityModel::load_bundle(&bundle, &digest, &context.mission)
            .expect_err("tampered manifest must fail");
        assert!(error
            .to_string()
            .contains("manifest SHA-256 does not match trusted registry digest"));
    }

    #[test]
    fn load_rejects_burnpack_metadata_that_disagrees_with_manifest() {
        let context = context();
        let mut model = train_event_disjoint_binary(
            &context.snapshot,
            &context.mission,
            &context.split,
            test_config(),
        )
        .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path().join("binary-research-model");
        model.save_bundle(&bundle).unwrap();

        let model_path = bundle.join(MODEL_FILE);
        let mut burnpack = fs::read(&model_path).unwrap();
        let metadata_size = u32::from_le_bytes(burnpack[6..10].try_into().unwrap()) as usize;
        let metadata = &burnpack[BURNPACK_HEADER_BYTES..BURNPACK_HEADER_BYTES + metadata_size];
        let marker = b"research_only";
        let marker_start = metadata
            .windows(marker.len())
            .position(|window| window == marker)
            .expect("artifact_scope metadata marker");
        burnpack[BURNPACK_HEADER_BYTES + marker_start + marker.len() - 1] = b'x';
        fs::write(&model_path, burnpack).unwrap();

        let manifest_path = bundle.join(MANIFEST_FILE);
        let mut manifest: BinaryModelManifest =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        let model_sha256 = sha256_file(&model_path).unwrap();
        manifest.model_sha256 = Some(model_sha256.clone());
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let digest = BinaryBundleDigest {
            manifest_sha256: sha256_file(&manifest_path).unwrap(),
            model_sha256,
        };

        let error = BinaryProbabilityModel::load_bundle(&bundle, &digest, &context.mission)
            .expect_err("Burnpack metadata mismatch must fail");
        assert!(error.to_string().contains("artifact_scope"));
    }
}
