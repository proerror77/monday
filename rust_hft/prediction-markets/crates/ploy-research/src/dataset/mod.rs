mod builder;
mod chronology;
mod contracts;
#[cfg(feature = "polars-export")]
mod export;
mod split;

pub use builder::{
    build_event_root_dataset, standard_event_root_dataset_artifacts, DatasetBuildError,
    DatasetSplitDerivedArtifacts, EventRootDatasetBuild, EventRootDatasetBuildRequest,
};
pub use chronology::{
    build_canonical_event_chronology, EventChronologyBuild, EventMetadataChronologyInput,
};
pub use contracts::{
    ChronologyAnchor, ChronologyOrdering, DatasetArtifacts, DatasetBuildManifest,
    DatasetBuildStats, DatasetLabelContract, DatasetSkipCounts, DatasetSourceWindow, DatasetSplit,
    DatasetSplitArtifactPaths, DatasetSplitAssignment, DatasetSplitCounts, DatasetSplitPolicy,
    EventChronologyKey, EventIndexEntry, CANONICAL_REGIME_VERSION, DATASET_MANIFEST_VERSION,
    REPRICING_LABEL_30S, SETTLEMENT_LABEL,
};
#[cfg(feature = "polars-export")]
pub use export::{
    event_index_to_frame, event_summaries_to_frame, export_event_root_dataset_parquet,
    split_assignments_to_frame, DatasetExportError,
};
pub use split::{assign_chronological_event_splits, SplitBuildError};
