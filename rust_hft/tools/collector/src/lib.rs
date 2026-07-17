#![allow(unexpected_cfgs)]

pub mod feature_matrix;
pub mod lob_archiver;
pub mod polymarket_parity;
pub mod polymarket_raw;
pub mod polymarket_research_import;
pub mod polymarket_research_select;
pub mod polymarket_upload;
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
