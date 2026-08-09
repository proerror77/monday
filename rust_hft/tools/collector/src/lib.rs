#![allow(unexpected_cfgs)]

pub mod binance_fee_artifact;
pub mod binance_usdm_reference_artifact;
pub mod binance_usdm_reference_collector;
pub mod binance_usdm_reference_upload;
pub mod feature_matrix;
pub mod lob_archiver;
pub mod polymarket_evidence_artifact;
pub mod polymarket_parity;
pub mod polymarket_raw;
pub mod polymarket_research_import;
pub mod polymarket_research_normalize;
pub mod polymarket_research_select;
pub mod polymarket_upload;
pub mod runtime_latency_evidence;
pub mod source_catalog;

pub use feature_matrix::{
    import_feature_dataset, read_feature_rows, DataModality, FeatureDatasetManifest,
    FeatureLabelSpec, PointInTimeFeatureRow,
};
pub use source_catalog::{
    acquire_dataset, source_catalog, CandleInterval, DataAcquisitionMission, DatasetManifest,
    DatasetTimeBounds, OhlcvTraceRow, QualityReport, QualityRequirements, SourceCapability,
    SourceDescriptor,
};
