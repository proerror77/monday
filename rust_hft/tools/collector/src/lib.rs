#![allow(unexpected_cfgs)]

pub mod source_catalog;

pub use source_catalog::{
    acquire_dataset, source_catalog, DataAcquisitionMission, DatasetManifest, OhlcvTraceRow,
    QualityReport, QualityRequirements, SourceCapability, SourceDescriptor,
};
