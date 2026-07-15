//! Pure-Rust model training for the continuous-contract research lane.
//!
//! This crate deliberately does not contain prediction-market labels or execution
//! adapters. A row is usable only when its forward-return label was available by
//! the declared training cutoff, which keeps model fitting point-in-time.

use burn::{
    backend::{Autodiff, NdArray},
    config::Config,
    module::{AutodiffModule, Module},
    nn::{
        loss::{MseLoss, Reduction},
        Linear, LinearConfig,
    },
    optim::{AdamConfig, GradientsParams, Optimizer},
    tensor::{activation::relu, backend::Backend, Tensor, TensorData},
};
use burn_ndarray::NdArrayDevice;
use burn_store::{BurnpackStore, ModuleSnapshot};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
};

pub type CpuBackend = NdArray<f32>;
pub type CpuAutodiffBackend = Autodiff<CpuBackend>;

static STAGING_COUNTER: AtomicU64 = AtomicU64::new(0);
static MODEL_INITIALIZATION_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Sha256Digest(String);

impl Sha256Digest {
    pub fn of_bytes(bytes: &[u8]) -> Self {
        Self(hex::encode(Sha256::digest(bytes)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Sha256Digest {
    type Error = ContractTrainingError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            Ok(Self(value))
        } else {
            Err(ContractTrainingError::InvalidSha256Digest)
        }
    }
}

impl From<Sha256Digest> for String {
    fn from(value: Sha256Digest) -> Self {
        value.0
    }
}

impl FromStr for Sha256Digest {
    type Err = ContractTrainingError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value.to_owned())
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(transparent)]
pub struct TimestampMs(i64);

impl TimestampMs {
    pub const fn new(value: i64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> i64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(try_from = "u64", into = "u64")]
pub struct PositiveDurationMs(u64);

impl PositiveDurationMs {
    pub fn new(value: u64) -> Result<Self, ContractTrainingError> {
        Self::try_from(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

impl TryFrom<u64> for PositiveDurationMs {
    type Error = ContractTrainingError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        if value == 0 {
            Err(ContractTrainingError::InvalidDuration)
        } else {
            Ok(Self(value))
        }
    }
}

impl From<PositiveDurationMs> for u64 {
    fn from(value: PositiveDurationMs) -> Self {
        value.0
    }
}

macro_rules! non_empty_identifier {
    ($name:ident, $kind:literal) => {
        #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, ContractTrainingError> {
                Self::try_from(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = ContractTrainingError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                if value.is_empty() || value.trim() != value {
                    Err(ContractTrainingError::InvalidIdentifier { kind: $kind })
                } else {
                    Ok(Self(value))
                }
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }
    };
}

non_empty_identifier!(FeatureName, "feature name");
non_empty_identifier!(Symbol, "symbol");
non_empty_identifier!(Venue, "venue");
non_empty_identifier!(SplitId, "split id");

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SplitRole {
    Train,
    Validation,
    Test,
    Holdout,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ContractTrainingRow {
    pub observed_at_ms: TimestampMs,
    pub feature_max_available_at_ms: TimestampMs,
    pub label_available_at_ms: TimestampMs,
    pub features: Vec<f32>,
    pub forward_return: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ContractTrainingConfig {
    pub input_dim: usize,
    pub hidden_dim: usize,
    pub epochs: usize,
    pub learning_rate: f64,
    pub min_rows: usize,
    pub seed: u64,
}

impl Default for ContractTrainingConfig {
    fn default() -> Self {
        Self {
            input_dim: 32,
            hidden_dim: 32,
            epochs: 100,
            learning_rate: 1e-3,
            min_rows: 32,
            seed: 7,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ContractDatasetBinding {
    dataset_manifest_sha256: Sha256Digest,
    feature_manifest_sha256: Sha256Digest,
    label_manifest_sha256: Sha256Digest,
    ordered_features: Vec<FeatureName>,
    symbol: Symbol,
    venue: Venue,
    horizon_ms: PositiveDurationMs,
}

impl ContractDatasetBinding {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        dataset_manifest_sha256: Sha256Digest,
        feature_manifest_sha256: Sha256Digest,
        label_manifest_sha256: Sha256Digest,
        ordered_features: Vec<FeatureName>,
        symbol: Symbol,
        venue: Venue,
        horizon_ms: PositiveDurationMs,
    ) -> Result<Self, ContractTrainingError> {
        let binding = Self {
            dataset_manifest_sha256,
            feature_manifest_sha256,
            label_manifest_sha256,
            ordered_features,
            symbol,
            venue,
            horizon_ms,
        };
        binding.validate_shape()?;
        Ok(binding)
    }

    pub fn dataset_manifest_sha256(&self) -> &Sha256Digest {
        &self.dataset_manifest_sha256
    }

    pub fn feature_manifest_sha256(&self) -> &Sha256Digest {
        &self.feature_manifest_sha256
    }

    pub fn label_manifest_sha256(&self) -> &Sha256Digest {
        &self.label_manifest_sha256
    }

    pub fn ordered_features(&self) -> &[FeatureName] {
        &self.ordered_features
    }

    pub fn symbol(&self) -> &Symbol {
        &self.symbol
    }

    pub fn venue(&self) -> &Venue {
        &self.venue
    }

    pub const fn horizon_ms(&self) -> PositiveDurationMs {
        self.horizon_ms
    }

    fn validate_shape(&self) -> Result<(), ContractTrainingError> {
        if self.ordered_features.is_empty() {
            return Err(ContractTrainingError::DatasetBinding(
                "ordered_features must not be empty".to_string(),
            ));
        }
        let mut unique = std::collections::BTreeSet::new();
        if !self.ordered_features.iter().all(|name| unique.insert(name)) {
            return Err(ContractTrainingError::DatasetBinding(
                "ordered_features must be unique".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PurgedWalkForwardSplit {
    id: SplitId,
    role: SplitRole,
    start_at_ms: TimestampMs,
    training_cutoff_ms: TimestampMs,
    next_split_start_at_ms: TimestampMs,
    purge_ms: PositiveDurationMs,
    embargo_ms: PositiveDurationMs,
}

impl PurgedWalkForwardSplit {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: SplitId,
        role: SplitRole,
        start_at_ms: TimestampMs,
        training_cutoff_ms: TimestampMs,
        next_split_start_at_ms: TimestampMs,
        purge_ms: PositiveDurationMs,
        embargo_ms: PositiveDurationMs,
    ) -> Result<Self, ContractTrainingError> {
        let split = Self {
            id,
            role,
            start_at_ms,
            training_cutoff_ms,
            next_split_start_at_ms,
            purge_ms,
            embargo_ms,
        };
        split.validate_bounds()?;
        Ok(split)
    }

    pub fn id(&self) -> &SplitId {
        &self.id
    }

    pub const fn role(&self) -> SplitRole {
        self.role
    }

    pub const fn start_at_ms(&self) -> TimestampMs {
        self.start_at_ms
    }

    pub const fn training_cutoff_ms(&self) -> TimestampMs {
        self.training_cutoff_ms
    }

    pub const fn next_split_start_at_ms(&self) -> TimestampMs {
        self.next_split_start_at_ms
    }

    pub const fn purge_ms(&self) -> PositiveDurationMs {
        self.purge_ms
    }

    pub const fn embargo_ms(&self) -> PositiveDurationMs {
        self.embargo_ms
    }

    fn validate_bounds(&self) -> Result<(), ContractTrainingError> {
        if self.start_at_ms >= self.training_cutoff_ms {
            return Err(ContractTrainingError::SplitBinding(
                "split start must be before training cutoff".to_string(),
            ));
        }
        if self.next_split_start_at_ms <= self.training_cutoff_ms {
            return Err(ContractTrainingError::SplitBinding(
                "next split must start after training cutoff".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TrainingRequest {
    schema_version: u32,
    rows_artifact_sha256: Sha256Digest,
    dataset: ContractDatasetBinding,
    split: PurgedWalkForwardSplit,
    config: ContractTrainingConfig,
}

impl TrainingRequest {
    pub fn new(
        rows_artifact_sha256: Sha256Digest,
        dataset: ContractDatasetBinding,
        split: PurgedWalkForwardSplit,
        config: ContractTrainingConfig,
    ) -> Result<Self, ContractTrainingError> {
        let request = Self {
            schema_version: 1,
            rows_artifact_sha256,
            dataset,
            split,
            config,
        };
        request.validate()?;
        Ok(request)
    }

    pub fn rows_artifact_sha256(&self) -> &Sha256Digest {
        &self.rows_artifact_sha256
    }

    pub fn dataset(&self) -> &ContractDatasetBinding {
        &self.dataset
    }

    pub fn split(&self) -> &PurgedWalkForwardSplit {
        &self.split
    }

    pub fn config(&self) -> &ContractTrainingConfig {
        &self.config
    }

    pub fn semantic_sha256(&self) -> Result<Sha256Digest, ContractTrainingError> {
        sha256_json_digest(self)
    }

    fn validate(&self) -> Result<(), ContractTrainingError> {
        if self.schema_version != 1 {
            return Err(ContractTrainingError::UnsupportedSchemaVersion {
                artifact: "training request",
                found: self.schema_version,
            });
        }
        validate_config(&self.config)?;
        self.dataset.validate_shape()?;
        self.split.validate_bounds()?;
        if self.dataset.ordered_features.len() != self.config.input_dim {
            return Err(ContractTrainingError::DatasetBinding(
                "ordered_features must match input_dim".to_string(),
            ));
        }
        if self.split.role != SplitRole::Train {
            return Err(ContractTrainingError::SplitBinding(
                "training request split role must be train".to_string(),
            ));
        }
        let horizon = self.dataset.horizon_ms.get();
        if self.split.purge_ms.get() < horizon || self.split.embargo_ms.get() < horizon {
            return Err(ContractTrainingError::SplitBinding(
                "purge and embargo must be at least the prediction horizon".to_string(),
            ));
        }
        let embargo_end = checked_add_duration(
            self.split.training_cutoff_ms,
            self.split.embargo_ms,
            "split embargo boundary",
        )?;
        if self.split.next_split_start_at_ms < embargo_end {
            return Err(ContractTrainingError::SplitBinding(
                "next split starts before the embargo ends".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct SealedTrainingRequest {
    request: TrainingRequest,
    artifact_sha256: Sha256Digest,
}

impl SealedTrainingRequest {
    pub fn from_bytes(
        bytes: &[u8],
        expected_sha256: &Sha256Digest,
    ) -> Result<Self, ContractTrainingError> {
        let actual_sha256 = Sha256Digest::of_bytes(bytes);
        if &actual_sha256 != expected_sha256 {
            return Err(ContractTrainingError::TrainingRequestChecksumMismatch {
                expected: expected_sha256.clone(),
                actual: actual_sha256,
            });
        }
        let request: TrainingRequest = serde_json::from_slice(bytes)
            .map_err(|error| ContractTrainingError::Artifact(error.to_string()))?;
        request.validate()?;
        Ok(Self {
            request,
            artifact_sha256: expected_sha256.clone(),
        })
    }

    pub fn request(&self) -> &TrainingRequest {
        &self.request
    }

    pub fn artifact_sha256(&self) -> &Sha256Digest {
        &self.artifact_sha256
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TrainingBackend {
    BurnNdarrayAutodiff,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TrainingAlgorithm {
    BurnMlpAdamMseV1,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelArtifactFormat {
    BurnpackV1,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceAuthority {
    FitDiagnosticsOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FitDiagnostics {
    pub schema_version: u32,
    pub backend: TrainingBackend,
    pub algorithm: TrainingAlgorithm,
    pub artifact_format: ModelArtifactFormat,
    pub authority: EvidenceAuthority,
    pub trainer_version: String,
    pub row_count: usize,
    pub training_cutoff_ms: TimestampMs,
    pub mse: f64,
    pub directional_accuracy: f64,
    pub config_sha256: Sha256Digest,
    pub feature_manifest_sha256: Sha256Digest,
    pub request_artifact_sha256: Sha256Digest,
    pub request_semantic_sha256: Sha256Digest,
    pub semantic_model_sha256: Sha256Digest,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ContractModelBundleManifest {
    pub schema_version: u32,
    pub model_file: String,
    pub model_file_sha256: Sha256Digest,
    pub semantic_model_sha256: Sha256Digest,
    pub request_artifact_sha256: Sha256Digest,
    pub request_semantic_sha256: Sha256Digest,
    pub request: TrainingRequest,
    pub diagnostics: FitDiagnostics,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SavedContractModelBundle {
    pub model_path: PathBuf,
    pub manifest_path: PathBuf,
    pub manifest_sha256: Sha256Digest,
    pub manifest: ContractModelBundleManifest,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ContractTrainingError {
    #[error("SHA-256 digest must be 64 lowercase hexadecimal characters")]
    InvalidSha256Digest,
    #[error("training request checksum mismatch: expected {expected}, got {actual}")]
    TrainingRequestChecksumMismatch {
        expected: Sha256Digest,
        actual: Sha256Digest,
    },
    #[error("rows artifact checksum mismatch: expected {expected}, got {actual}")]
    RowsArtifactChecksumMismatch {
        expected: Sha256Digest,
        actual: Sha256Digest,
    },
    #[error("duration must be positive")]
    InvalidDuration,
    #[error("invalid {kind}")]
    InvalidIdentifier { kind: &'static str },
    #[error("unsupported {artifact} schema version {found}")]
    UnsupportedSchemaVersion { artifact: &'static str, found: u32 },
    #[error("input_dim and hidden_dim must be positive")]
    InvalidDimensions,
    #[error("epochs and min_rows must be positive")]
    InvalidTrainingBudget,
    #[error("learning_rate must be finite and positive")]
    InvalidLearningRate,
    #[error("expected at least {required} training rows, got {actual}")]
    InsufficientRows { required: usize, actual: usize },
    #[error("row {row} has feature dimension {actual}, expected {expected}")]
    FeatureDimension {
        row: usize,
        expected: usize,
        actual: usize,
    },
    #[error("row {row} contains a non-finite value")]
    NonFiniteValue { row: usize },
    #[error("row {row} is not ordered by observed_at_ms")]
    NonMonotonicObservationTime { row: usize },
    #[error("row {row} label is not strictly after its observation")]
    NonCausalLabel { row: usize },
    #[error("row {row} label was unavailable at training cutoff {cutoff_ms}")]
    LabelUnavailable { row: usize, cutoff_ms: i64 },
    #[error("row {row} feature availability is after its observation")]
    FeatureUnavailable { row: usize },
    #[error("row {row} label is available before the prediction horizon")]
    LabelBeforeHorizon { row: usize },
    #[error("row {row} observation is outside the bound training split")]
    RowOutsideSplit { row: usize },
    #[error("row {row} observation falls inside the split purge interval")]
    RowInsidePurge { row: usize },
    #[error("model artifact I/O failed: {0}")]
    Artifact(String),
    #[error("dataset binding is invalid: {0}")]
    DatasetBinding(String),
    #[error("split binding is invalid: {0}")]
    SplitBinding(String),
    #[error("timestamp overflow while computing {0}")]
    TimestampOverflow(&'static str),
    #[error("model artifact checksum does not match its manifest")]
    ArtifactChecksumMismatch,
    #[error("manifest checksum does not match the externally expected digest")]
    ManifestChecksumMismatch,
    #[error("manifest filename does not match its content digest")]
    ManifestFileNameMismatch,
    #[error("model filename is not a safe content-addressed basename")]
    UnsafeArtifactFileName,
    #[error("loaded model weights do not match the semantic model digest")]
    SemanticModelChecksumMismatch,
    #[error("content-addressed target already exists with different bytes: {0}")]
    CasCollision(String),
    #[error("model bundle is incomplete: {0}")]
    IncompleteBundle(String),
    #[error("model bundle internal consistency check failed: {0}")]
    InternalConsistency(String),
}

#[derive(Config, Debug)]
pub struct ReturnRegressorConfig {
    input_dim: usize,
    hidden_dim: usize,
}

#[derive(Module, Debug)]
pub struct ReturnRegressor<B: burn::tensor::backend::Backend> {
    hidden: Linear<B>,
    output: Linear<B>,
}

impl ReturnRegressorConfig {
    pub fn init_model<B: burn::tensor::backend::Backend>(
        &self,
        device: &B::Device,
    ) -> ReturnRegressor<B> {
        ReturnRegressor {
            hidden: LinearConfig::new(self.input_dim, self.hidden_dim).init(device),
            output: LinearConfig::new(self.hidden_dim, 1).init(device),
        }
    }
}

impl<B: burn::tensor::backend::Backend> ReturnRegressor<B> {
    pub fn forward(&self, features: Tensor<B, 2>) -> Tensor<B, 2> {
        self.output.forward(relu(self.hidden.forward(features)))
    }
}

fn update_semantic_tensor_digest<const D: usize>(
    hasher: &mut Sha256,
    name: &str,
    tensor: Tensor<CpuBackend, D>,
) -> Result<(), ContractTrainingError> {
    let shape = tensor.shape();
    hasher.update((name.len() as u64).to_le_bytes());
    hasher.update(name.as_bytes());
    hasher.update((D as u64).to_le_bytes());
    for dimension in shape.dims {
        hasher.update((dimension as u64).to_le_bytes());
    }
    let values = tensor
        .into_data()
        .into_vec::<f32>()
        .map_err(|error| ContractTrainingError::Artifact(format!("{error:?}")))?;
    hasher.update((values.len() as u64).to_le_bytes());
    for value in values {
        hasher.update(value.to_bits().to_le_bytes());
    }
    Ok(())
}

fn semantic_model_sha256(
    model: &ReturnRegressor<CpuBackend>,
) -> Result<Sha256Digest, ContractTrainingError> {
    let mut hasher = Sha256::new();
    hasher.update(b"contract-return-regressor:burn-mlp-adam-mse-v1");
    update_semantic_tensor_digest(&mut hasher, "hidden.weight", model.hidden.weight.val())?;
    let hidden_bias = model.hidden.bias.as_ref().ok_or_else(|| {
        ContractTrainingError::InternalConsistency("hidden bias is missing".to_string())
    })?;
    update_semantic_tensor_digest(&mut hasher, "hidden.bias", hidden_bias.val())?;
    update_semantic_tensor_digest(&mut hasher, "output.weight", model.output.weight.val())?;
    let output_bias = model.output.bias.as_ref().ok_or_else(|| {
        ContractTrainingError::InternalConsistency("output bias is missing".to_string())
    })?;
    update_semantic_tensor_digest(&mut hasher, "output.bias", output_bias.val())?;
    Ok(Sha256Digest(hex::encode(hasher.finalize())))
}

fn staging_path(output_dir: &Path, stem: &str, extension: &str) -> PathBuf {
    let sequence = STAGING_COUNTER.fetch_add(1, Ordering::Relaxed);
    output_dir.join(format!(
        ".{stem}-{}-{sequence}.{extension}.staging",
        std::process::id()
    ))
}

fn sync_directory(path: &Path) -> Result<(), ContractTrainingError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| ContractTrainingError::Artifact(error.to_string()))
}

fn publish_staged_file(
    staging_path: &Path,
    target_path: &Path,
    expected_sha256: &Sha256Digest,
) -> Result<(), ContractTrainingError> {
    File::open(staging_path)
        .and_then(|file| file.sync_all())
        .map_err(|error| ContractTrainingError::Artifact(error.to_string()))?;
    match fs::hard_link(staging_path, target_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing_bytes = fs::read(target_path)
                .map_err(|read_error| ContractTrainingError::Artifact(read_error.to_string()))?;
            if &Sha256Digest::of_bytes(&existing_bytes) != expected_sha256 {
                let _ = fs::remove_file(staging_path);
                return Err(ContractTrainingError::CasCollision(
                    target_path.display().to_string(),
                ));
            }
        }
        Err(error) => {
            let _ = fs::remove_file(staging_path);
            return Err(ContractTrainingError::Artifact(error.to_string()));
        }
    }
    fs::remove_file(staging_path)
        .map_err(|error| ContractTrainingError::Artifact(error.to_string()))?;
    let parent = target_path.parent().ok_or_else(|| {
        ContractTrainingError::Artifact("content-addressed target has no parent".to_string())
    })?;
    sync_directory(parent)
}

fn publish_bytes(
    bytes: &[u8],
    staging_path: &Path,
    target_path: &Path,
    expected_sha256: &Sha256Digest,
) -> Result<(), ContractTrainingError> {
    let mut staging_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(staging_path)
        .map_err(|error| ContractTrainingError::Artifact(error.to_string()))?;
    if let Err(error) = staging_file
        .write_all(bytes)
        .and_then(|_| staging_file.sync_all())
    {
        let _ = fs::remove_file(staging_path);
        return Err(ContractTrainingError::Artifact(error.to_string()));
    }
    drop(staging_file);
    publish_staged_file(staging_path, target_path, expected_sha256)
}

#[derive(Debug)]
pub struct TrainedContractModel {
    model: ReturnRegressor<CpuBackend>,
    request: TrainingRequest,
    diagnostics: FitDiagnostics,
}

impl TrainedContractModel {
    pub fn request(&self) -> &TrainingRequest {
        &self.request
    }

    pub fn diagnostics(&self) -> &FitDiagnostics {
        &self.diagnostics
    }

    pub fn predict(&self, features: &[f32]) -> Result<f32, ContractTrainingError> {
        if features.len() != self.request.config.input_dim {
            return Err(ContractTrainingError::FeatureDimension {
                row: 0,
                expected: self.request.config.input_dim,
                actual: features.len(),
            });
        }
        if features.iter().any(|value| !value.is_finite()) {
            return Err(ContractTrainingError::NonFiniteValue { row: 0 });
        }
        let device = NdArrayDevice::Cpu;
        let tensor = Tensor::<CpuBackend, 2>::from_data(
            TensorData::new(features.to_vec(), [1, features.len()]),
            &device,
        );
        let values = self
            .model
            .forward(tensor)
            .into_data()
            .into_vec::<f32>()
            .map_err(|error| ContractTrainingError::Artifact(format!("{error:?}")))?;
        values
            .first()
            .copied()
            .ok_or_else(|| ContractTrainingError::Artifact("empty model output".to_string()))
    }

    pub fn save_bundle(
        &self,
        output_dir: impl AsRef<Path>,
    ) -> Result<SavedContractModelBundle, ContractTrainingError> {
        let output_dir = output_dir.as_ref();
        fs::create_dir_all(output_dir)
            .map_err(|error| ContractTrainingError::Artifact(error.to_string()))?;
        let model_staging_path = staging_path(output_dir, "contract-return-regressor", "bpk");
        let mut store = BurnpackStore::from_file(&model_staging_path)
            .auto_extension(false)
            .overwrite(false)
            .metadata("lane", "continuous-contract")
            .metadata("authority", "lab-only")
            .metadata("config_sha256", self.diagnostics.config_sha256.as_str())
            .metadata(
                "request_semantic_sha256",
                self.diagnostics.request_semantic_sha256.as_str(),
            )
            .metadata(
                "semantic_model_sha256",
                self.diagnostics.semantic_model_sha256.as_str(),
            )
            .metadata(
                "dataset_manifest_sha256",
                self.request.dataset.dataset_manifest_sha256.as_str(),
            );
        if let Err(error) = self.model.save_into(&mut store) {
            let _ = fs::remove_file(&model_staging_path);
            return Err(ContractTrainingError::Artifact(error.to_string()));
        }
        drop(store);
        let bytes = fs::read(&model_staging_path)
            .map_err(|error| ContractTrainingError::Artifact(error.to_string()))?;
        let model_sha256 = Sha256Digest::of_bytes(&bytes);
        let model_file = format!("contract-return-regressor-{model_sha256}.bpk");
        let model_path = output_dir.join(&model_file);
        publish_staged_file(&model_staging_path, &model_path, &model_sha256)?;
        let manifest = ContractModelBundleManifest {
            schema_version: 1,
            model_file,
            model_file_sha256: model_sha256,
            semantic_model_sha256: self.diagnostics.semantic_model_sha256.clone(),
            request_artifact_sha256: self.diagnostics.request_artifact_sha256.clone(),
            request_semantic_sha256: self.diagnostics.request_semantic_sha256.clone(),
            request: self.request.clone(),
            diagnostics: self.diagnostics.clone(),
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest)
            .map_err(|error| ContractTrainingError::Artifact(error.to_string()))?;
        let manifest_sha256 = Sha256Digest::of_bytes(&manifest_bytes);
        let manifest_path = output_dir.join(format!("contract-model-{manifest_sha256}.json"));
        let manifest_staging_path = staging_path(output_dir, "contract-model", "json");
        publish_bytes(
            &manifest_bytes,
            &manifest_staging_path,
            &manifest_path,
            &manifest_sha256,
        )?;
        Ok(SavedContractModelBundle {
            model_path,
            manifest_path,
            manifest_sha256,
            manifest,
        })
    }
}

pub fn load_contract_model_bundle(
    manifest_path: impl AsRef<Path>,
    expected_manifest_sha256: &Sha256Digest,
) -> Result<TrainedContractModel, ContractTrainingError> {
    let manifest_path = manifest_path.as_ref();
    let manifest_bytes = fs::read(manifest_path)
        .map_err(|error| ContractTrainingError::Artifact(error.to_string()))?;
    if &Sha256Digest::of_bytes(&manifest_bytes) != expected_manifest_sha256 {
        return Err(ContractTrainingError::ManifestChecksumMismatch);
    }
    let expected_manifest_name = format!("contract-model-{expected_manifest_sha256}.json");
    if manifest_path.file_name().and_then(|name| name.to_str())
        != Some(expected_manifest_name.as_str())
    {
        return Err(ContractTrainingError::ManifestFileNameMismatch);
    }
    let manifest: ContractModelBundleManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| ContractTrainingError::Artifact(error.to_string()))?;
    validate_bundle_manifest(&manifest)?;
    let model_path = manifest_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(&manifest.model_file);
    let model_bytes = fs::read(&model_path)
        .map_err(|error| ContractTrainingError::Artifact(error.to_string()))?;
    if Sha256Digest::of_bytes(&model_bytes) != manifest.model_file_sha256 {
        return Err(ContractTrainingError::ArtifactChecksumMismatch);
    }
    let device = NdArrayDevice::Cpu;
    let config = manifest.request.config();
    let mut model = ReturnRegressorConfig::new(config.input_dim, config.hidden_dim)
        .init_model::<CpuBackend>(&device);
    let mut store =
        BurnpackStore::from_bytes(Some(burn::tensor::Bytes::from_bytes_vec(model_bytes)));
    let applied = model
        .load_from(&mut store)
        .map_err(|error| ContractTrainingError::Artifact(error.to_string()))?;
    if !applied.is_success()
        || !applied.missing.is_empty()
        || !applied.unused.is_empty()
        || applied.applied.is_empty()
    {
        return Err(ContractTrainingError::IncompleteBundle(applied.to_string()));
    }
    if semantic_model_sha256(&model)? != manifest.semantic_model_sha256 {
        return Err(ContractTrainingError::SemanticModelChecksumMismatch);
    }
    Ok(TrainedContractModel {
        model,
        request: manifest.request,
        diagnostics: manifest.diagnostics,
    })
}

pub fn train_contract_model(
    rows_artifact: &[u8],
    sealed_request: &SealedTrainingRequest,
) -> Result<TrainedContractModel, ContractTrainingError> {
    let actual_sha256 = Sha256Digest::of_bytes(rows_artifact);
    let expected_sha256 = sealed_request.request.rows_artifact_sha256();
    if &actual_sha256 != expected_sha256 {
        return Err(ContractTrainingError::RowsArtifactChecksumMismatch {
            expected: expected_sha256.clone(),
            actual: actual_sha256,
        });
    }
    let rows: Vec<ContractTrainingRow> = serde_json::from_slice(rows_artifact)
        .map_err(|error| ContractTrainingError::Artifact(error.to_string()))?;
    validate_bound_training_rows(&rows, &sealed_request.request)?;
    train_parsed_contract_model(&rows, sealed_request)
}

fn train_parsed_contract_model(
    rows: &[ContractTrainingRow],
    sealed_request: &SealedTrainingRequest,
) -> Result<TrainedContractModel, ContractTrainingError> {
    let request = &sealed_request.request;
    let config = &request.config;
    let dataset = &request.dataset;
    let training_cutoff_ms = request.split.training_cutoff_ms;
    validate_dataset_binding(dataset, config)?;

    let device = NdArrayDevice::Cpu;
    let mut model = {
        let initialization_guard = MODEL_INITIALIZATION_LOCK.lock().map_err(|_| {
            ContractTrainingError::Artifact("model initialization lock was poisoned".to_string())
        })?;
        CpuAutodiffBackend::seed(&device, config.seed);
        let model = ReturnRegressorConfig::new(config.input_dim, config.hidden_dim)
            .init_model::<CpuAutodiffBackend>(&device);
        let _ = model.hidden.weight.val();
        let _ = model.hidden.bias.as_ref().map(|bias| bias.val());
        let _ = model.output.weight.val();
        let _ = model.output.bias.as_ref().map(|bias| bias.val());
        drop(initialization_guard);
        model
    };
    let mut optimizer = AdamConfig::new().init();
    let features = rows
        .iter()
        .flat_map(|row| row.features.iter().copied())
        .collect::<Vec<_>>();
    let targets = rows
        .iter()
        .map(|row| row.forward_return)
        .collect::<Vec<_>>();

    for _ in 0..config.epochs {
        let feature_tensor = Tensor::<CpuAutodiffBackend, 2>::from_data(
            TensorData::new(features.clone(), [rows.len(), config.input_dim]),
            &device,
        );
        let target_tensor = Tensor::<CpuAutodiffBackend, 2>::from_data(
            TensorData::new(targets.clone(), [rows.len(), 1]),
            &device,
        );
        let loss = MseLoss::new().forward(
            model.forward(feature_tensor),
            target_tensor,
            Reduction::Mean,
        );
        let gradients = GradientsParams::from_grads(loss.backward(), &model);
        model = optimizer.step(config.learning_rate, model, gradients);
    }

    let model = model.valid();
    let predictions = model
        .forward(Tensor::<CpuBackend, 2>::from_data(
            TensorData::new(features, [rows.len(), config.input_dim]),
            &device,
        ))
        .into_data()
        .into_vec::<f32>()
        .map_err(|error| ContractTrainingError::Artifact(format!("{error:?}")))?;
    let mse = predictions
        .iter()
        .zip(&targets)
        .map(|(prediction, target)| f64::from((prediction - target).powi(2)))
        .sum::<f64>()
        / rows.len() as f64;
    let directional_accuracy = predictions
        .iter()
        .zip(&targets)
        .filter(|(prediction, target)| prediction.signum() == target.signum())
        .count() as f64
        / rows.len() as f64;

    let semantic_model_sha256 = semantic_model_sha256(&model)?;
    let request_semantic_sha256 = request.semantic_sha256()?;
    Ok(TrainedContractModel {
        model,
        request: request.clone(),
        diagnostics: FitDiagnostics {
            schema_version: 1,
            backend: TrainingBackend::BurnNdarrayAutodiff,
            algorithm: TrainingAlgorithm::BurnMlpAdamMseV1,
            artifact_format: ModelArtifactFormat::BurnpackV1,
            authority: EvidenceAuthority::FitDiagnosticsOnly,
            trainer_version: env!("CARGO_PKG_VERSION").to_string(),
            row_count: rows.len(),
            training_cutoff_ms,
            mse,
            directional_accuracy,
            config_sha256: sha256_json_digest(config)?,
            feature_manifest_sha256: dataset.feature_manifest_sha256.clone(),
            request_artifact_sha256: sealed_request.artifact_sha256.clone(),
            request_semantic_sha256,
            semantic_model_sha256,
        },
    })
}

fn sha256_json_digest(value: &impl Serialize) -> Result<Sha256Digest, ContractTrainingError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| ContractTrainingError::Artifact(error.to_string()))?;
    Ok(Sha256Digest::of_bytes(&bytes))
}

fn validate_bundle_manifest(
    manifest: &ContractModelBundleManifest,
) -> Result<(), ContractTrainingError> {
    if manifest.schema_version != 1 {
        return Err(ContractTrainingError::UnsupportedSchemaVersion {
            artifact: "model bundle manifest",
            found: manifest.schema_version,
        });
    }
    manifest.request.validate()?;
    let expected_model_file = format!(
        "contract-return-regressor-{}.bpk",
        manifest.model_file_sha256
    );
    if manifest.model_file != expected_model_file {
        return Err(ContractTrainingError::UnsafeArtifactFileName);
    }
    let request_semantic_sha256 = manifest.request.semantic_sha256()?;
    let config_sha256 = sha256_json_digest(manifest.request.config())?;
    let diagnostics = &manifest.diagnostics;
    if diagnostics.schema_version != 1
        || diagnostics.backend != TrainingBackend::BurnNdarrayAutodiff
        || diagnostics.algorithm != TrainingAlgorithm::BurnMlpAdamMseV1
        || diagnostics.artifact_format != ModelArtifactFormat::BurnpackV1
        || diagnostics.authority != EvidenceAuthority::FitDiagnosticsOnly
        || diagnostics.trainer_version.trim().is_empty()
        || diagnostics.row_count < manifest.request.config.min_rows
        || !diagnostics.mse.is_finite()
        || !diagnostics.directional_accuracy.is_finite()
        || !(0.0..=1.0).contains(&diagnostics.directional_accuracy)
        || diagnostics.training_cutoff_ms != manifest.request.split.training_cutoff_ms
        || diagnostics.config_sha256 != config_sha256
        || diagnostics.feature_manifest_sha256 != manifest.request.dataset.feature_manifest_sha256
        || diagnostics.request_artifact_sha256 != manifest.request_artifact_sha256
        || diagnostics.request_semantic_sha256 != request_semantic_sha256
        || manifest.request_semantic_sha256 != request_semantic_sha256
        || diagnostics.semantic_model_sha256 != manifest.semantic_model_sha256
    {
        return Err(ContractTrainingError::InternalConsistency(
            "manifest fields are not bound to the sealed request and diagnostics".to_string(),
        ));
    }
    Ok(())
}

fn checked_add_duration(
    timestamp: TimestampMs,
    duration: PositiveDurationMs,
    context: &'static str,
) -> Result<TimestampMs, ContractTrainingError> {
    let duration = i64::try_from(duration.get())
        .map_err(|_| ContractTrainingError::TimestampOverflow(context))?;
    timestamp
        .get()
        .checked_add(duration)
        .map(TimestampMs::new)
        .ok_or(ContractTrainingError::TimestampOverflow(context))
}

fn checked_sub_duration(
    timestamp: TimestampMs,
    duration: PositiveDurationMs,
    context: &'static str,
) -> Result<TimestampMs, ContractTrainingError> {
    let duration = i64::try_from(duration.get())
        .map_err(|_| ContractTrainingError::TimestampOverflow(context))?;
    timestamp
        .get()
        .checked_sub(duration)
        .map(TimestampMs::new)
        .ok_or(ContractTrainingError::TimestampOverflow(context))
}

fn validate_config(config: &ContractTrainingConfig) -> Result<(), ContractTrainingError> {
    if config.input_dim == 0 || config.hidden_dim == 0 {
        return Err(ContractTrainingError::InvalidDimensions);
    }
    if config.epochs == 0 || config.min_rows == 0 {
        return Err(ContractTrainingError::InvalidTrainingBudget);
    }
    if !config.learning_rate.is_finite() || config.learning_rate <= 0.0 {
        return Err(ContractTrainingError::InvalidLearningRate);
    }
    Ok(())
}

fn validate_bound_training_rows(
    rows: &[ContractTrainingRow],
    request: &TrainingRequest,
) -> Result<(), ContractTrainingError> {
    validate_config(&request.config)?;
    if rows.len() < request.config.min_rows {
        return Err(ContractTrainingError::InsufficientRows {
            required: request.config.min_rows,
            actual: rows.len(),
        });
    }
    let split_start = request.split.start_at_ms;
    let cutoff = request.split.training_cutoff_ms;
    let latest_observation =
        checked_sub_duration(cutoff, request.split.purge_ms, "split purge boundary")?;
    let horizon = request.dataset.horizon_ms;
    let mut previous_observed_at = None;
    for (index, row) in rows.iter().enumerate() {
        if row.features.len() != request.config.input_dim {
            return Err(ContractTrainingError::FeatureDimension {
                row: index,
                expected: request.config.input_dim,
                actual: row.features.len(),
            });
        }
        if !row.forward_return.is_finite() || row.features.iter().any(|value| !value.is_finite()) {
            return Err(ContractTrainingError::NonFiniteValue { row: index });
        }
        if previous_observed_at.is_some_and(|previous| row.observed_at_ms <= previous) {
            return Err(ContractTrainingError::NonMonotonicObservationTime { row: index });
        }
        if row.observed_at_ms < split_start {
            return Err(ContractTrainingError::RowOutsideSplit { row: index });
        }
        if row.observed_at_ms > latest_observation {
            return Err(ContractTrainingError::RowInsidePurge { row: index });
        }
        if row.feature_max_available_at_ms > row.observed_at_ms {
            return Err(ContractTrainingError::FeatureUnavailable { row: index });
        }
        let earliest_label =
            checked_add_duration(row.observed_at_ms, horizon, "row prediction horizon")?;
        if row.label_available_at_ms < earliest_label {
            return Err(ContractTrainingError::LabelBeforeHorizon { row: index });
        }
        if row.label_available_at_ms > cutoff {
            return Err(ContractTrainingError::LabelUnavailable {
                row: index,
                cutoff_ms: cutoff.get(),
            });
        }
        previous_observed_at = Some(row.observed_at_ms);
    }
    Ok(())
}

fn validate_dataset_binding(
    dataset: &ContractDatasetBinding,
    config: &ContractTrainingConfig,
) -> Result<(), ContractTrainingError> {
    dataset.validate_shape()?;
    if dataset.ordered_features.len() != config.input_dim {
        return Err(ContractTrainingError::DatasetBinding(
            "ordered_features must match input_dim".to_string(),
        ));
    }
    Ok(())
}
