#![allow(unexpected_cfgs)]

pub mod source_catalog;

pub use source_catalog::{
    acquire_dataset, source_catalog, CandleInterval, DataAcquisitionMission, DatasetManifest,
    DatasetTimeBounds, OhlcvTraceRow, QualityReport, QualityRequirements, SourceCapability,
    SourceDescriptor,
};
