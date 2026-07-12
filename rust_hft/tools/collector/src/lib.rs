#![allow(unexpected_cfgs)]

pub mod feature_matrix;
pub mod source_catalog;

pub use feature_matrix::{
    import_feature_dataset, read_feature_rows, DataModality, FeatureDatasetManifest,
    PointInTimeFeatureRow,
};
pub use source_catalog::{
    acquire_dataset, source_catalog, CandleInterval, DataAcquisitionMission, DatasetManifest,
    DatasetTimeBounds, OhlcvTraceRow, QualityReport, QualityRequirements, SourceCapability,
    SourceDescriptor,
};
