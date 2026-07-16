//! Research-only supervised models.
//!
//! These modules can train and evaluate probability models, but deliberately do
//! not implement PLOY trading, deployment, or promotion authority interfaces.

#[cfg(feature = "ml")]
pub mod burn_binary;

#[cfg(feature = "ml")]
pub use burn_binary::{
    train_event_disjoint_binary, BinaryArtifactScope, BinaryBundleDigest, BinaryDatasetContract,
    BinaryDecisionRow, BinaryModelManifest, BinaryOosMetrics, BinaryProbabilityModel,
    BinarySettlementAuthority, BinaryTrainingConfig, EventDisjointBinarySplit, FeatureNormalizer,
    VerifiedBinarySnapshot,
};
