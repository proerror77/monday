use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const DATASET_MANIFEST_VERSION: u32 = 1;
pub const CANONICAL_REGIME_VERSION: &str = "pm_binary_v1";
pub const SETTLEMENT_LABEL: &str = "settlement_up";
pub const REPRICING_LABEL_30S: &str = "future_up_ask_change_30s";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatasetSplit {
    Train,
    Val,
    Test,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChronologyAnchor {
    EndTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChronologyOrdering {
    EndTimeSymbolEventId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatasetSplitPolicy {
    pub chronology_anchor: ChronologyAnchor,
    pub chronology_ordering: ChronologyOrdering,
    pub train_percent: u8,
    pub val_percent: u8,
    pub test_percent: u8,
    pub min_unique_events: usize,
    pub min_eval_events: usize,
}

impl Default for DatasetSplitPolicy {
    fn default() -> Self {
        Self {
            chronology_anchor: ChronologyAnchor::EndTime,
            chronology_ordering: ChronologyOrdering::EndTimeSymbolEventId,
            train_percent: 70,
            val_percent: 15,
            test_percent: 15,
            min_unique_events: 3,
            min_eval_events: 20,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventChronologyKey {
    pub event_id: String,
    pub symbol: String,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatasetSplitAssignment {
    pub event_id: String,
    pub symbol: String,
    pub end_time: DateTime<Utc>,
    pub ordered_event_index: usize,
    pub split: DatasetSplit,
    pub split_rank: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventIndexEntry {
    pub event_id: String,
    pub symbol: String,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub split: DatasetSplit,
    pub split_rank: usize,
    pub observation_row_count: usize,
    pub settlement_label_available: bool,
    pub repricing_label_row_count_30s: usize,
    pub regime_version: String,
}

impl EventIndexEntry {
    pub fn chronology_key(&self) -> EventChronologyKey {
        EventChronologyKey {
            event_id: self.event_id.clone(),
            symbol: self.symbol.clone(),
            start_time: self.start_time,
            end_time: self.end_time,
        }
    }

    pub fn split_assignment(&self, ordered_event_index: usize) -> DatasetSplitAssignment {
        DatasetSplitAssignment {
            event_id: self.event_id.clone(),
            symbol: self.symbol.clone(),
            end_time: self.end_time,
            ordered_event_index,
            split: self.split,
            split_rank: self.split_rank,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatasetSplitCounts {
    pub train: usize,
    pub val: usize,
    pub test: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatasetSkipCounts {
    pub missing_start_time: usize,
    pub missing_end_time: usize,
    pub missing_timing_fields: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatasetSplitArtifactPaths {
    pub train: String,
    pub val: String,
    pub test: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatasetArtifacts {
    pub event_index: String,
    pub event_manifest: String,
    pub split_assignments: String,
    pub observations: DatasetSplitArtifactPaths,
    pub event_summaries: DatasetSplitArtifactPaths,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatasetLabelContract {
    pub observation_labels: Vec<String>,
    pub event_summary_labels: Vec<String>,
}

impl Default for DatasetLabelContract {
    fn default() -> Self {
        Self {
            observation_labels: vec![REPRICING_LABEL_30S.to_string()],
            event_summary_labels: vec![SETTLEMENT_LABEL.to_string()],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatasetSourceWindow {
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub symbols: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatasetBuildStats {
    pub total_events: usize,
    pub total_observations: usize,
    pub events_per_split: DatasetSplitCounts,
    pub observations_per_split: DatasetSplitCounts,
    pub skip_counts: DatasetSkipCounts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatasetBuildManifest {
    pub manifest_version: u32,
    pub built_at: DateTime<Utc>,
    pub source_window: DatasetSourceWindow,
    pub split_policy: DatasetSplitPolicy,
    pub labels: DatasetLabelContract,
    pub feature_families: Vec<String>,
    pub regime_version: String,
    pub artifacts: DatasetArtifacts,
    pub stats: DatasetBuildStats,
}

impl DatasetBuildManifest {
    pub fn validate_contract(&self) -> Result<(), String> {
        if self.manifest_version != DATASET_MANIFEST_VERSION {
            return Err(format!(
                "manifest_version {} does not match contract {}",
                self.manifest_version, DATASET_MANIFEST_VERSION
            ));
        }

        if self.split_policy != DatasetSplitPolicy::default() {
            return Err(
                "manifest split policy does not match the canonical first-slice contract"
                    .to_string(),
            );
        }

        if !self
            .labels
            .observation_labels
            .iter()
            .any(|label| label == REPRICING_LABEL_30S)
        {
            return Err(format!(
                "observation labels must include {REPRICING_LABEL_30S}"
            ));
        }

        if !self
            .labels
            .event_summary_labels
            .iter()
            .any(|label| label == SETTLEMENT_LABEL)
        {
            return Err(format!(
                "event summary labels must include {SETTLEMENT_LABEL}"
            ));
        }

        if self.regime_version.trim().is_empty() {
            return Err("regime_version must not be empty".to_string());
        }

        for path in [
            &self.artifacts.event_index,
            &self.artifacts.event_manifest,
            &self.artifacts.split_assignments,
            &self.artifacts.observations.train,
            &self.artifacts.observations.val,
            &self.artifacts.observations.test,
            &self.artifacts.event_summaries.train,
            &self.artifacts.event_summaries.val,
            &self.artifacts.event_summaries.test,
        ] {
            if path.trim().is_empty() {
                return Err("artifact paths must not be empty".to_string());
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DatasetArtifacts, DatasetBuildManifest, DatasetBuildStats, DatasetLabelContract,
        DatasetSkipCounts, DatasetSourceWindow, DatasetSplit, DatasetSplitArtifactPaths,
        DatasetSplitCounts, DatasetSplitPolicy, EventIndexEntry, CANONICAL_REGIME_VERSION,
        DATASET_MANIFEST_VERSION,
    };
    use chrono::{TimeZone, Utc};

    #[test]
    fn split_policy_pins_the_first_contract() {
        let policy = DatasetSplitPolicy::default();
        assert_eq!(policy.train_percent, 70);
        assert_eq!(policy.val_percent, 15);
        assert_eq!(policy.test_percent, 15);
        assert_eq!(policy.min_unique_events, 3);
        assert_eq!(policy.min_eval_events, 20);
    }

    #[test]
    fn event_index_entry_projects_canonical_contract_views() {
        let entry = EventIndexEntry {
            event_id: "evt-1".to_string(),
            symbol: "BTC".to_string(),
            start_time: Utc.with_ymd_and_hms(2026, 4, 1, 12, 0, 0).unwrap(),
            end_time: Utc.with_ymd_and_hms(2026, 4, 1, 12, 5, 0).unwrap(),
            split: DatasetSplit::Train,
            split_rank: 7,
            observation_row_count: 42,
            settlement_label_available: true,
            repricing_label_row_count_30s: 30,
            regime_version: CANONICAL_REGIME_VERSION.to_string(),
        };

        let chronology = entry.chronology_key();
        assert_eq!(chronology.event_id, "evt-1");
        assert_eq!(chronology.symbol, "BTC");
        assert_eq!(chronology.end_time, entry.end_time);

        let assignment = entry.split_assignment(11);
        assert_eq!(assignment.event_id, "evt-1");
        assert_eq!(assignment.split, DatasetSplit::Train);
        assert_eq!(assignment.ordered_event_index, 11);
        assert_eq!(assignment.split_rank, 7);
    }

    #[test]
    fn manifest_round_trips_and_validates() {
        let manifest = DatasetBuildManifest {
            manifest_version: DATASET_MANIFEST_VERSION,
            built_at: Utc.with_ymd_and_hms(2026, 4, 24, 0, 0, 0).unwrap(),
            source_window: DatasetSourceWindow {
                start_time: Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap(),
                end_time: Utc.with_ymd_and_hms(2026, 4, 7, 23, 59, 59).unwrap(),
                symbols: vec!["BTC".to_string(), "ETH".to_string()],
            },
            split_policy: DatasetSplitPolicy::default(),
            labels: DatasetLabelContract::default(),
            feature_families: vec!["microstructure".to_string(), "repricing".to_string()],
            regime_version: CANONICAL_REGIME_VERSION.to_string(),
            artifacts: DatasetArtifacts {
                event_index: "event_index.parquet".to_string(),
                event_manifest: "event_manifest.json".to_string(),
                split_assignments: "split_assignments.parquet".to_string(),
                observations: DatasetSplitArtifactPaths {
                    train: "observations_train.parquet".to_string(),
                    val: "observations_val.parquet".to_string(),
                    test: "observations_test.parquet".to_string(),
                },
                event_summaries: DatasetSplitArtifactPaths {
                    train: "event_summaries_train.parquet".to_string(),
                    val: "event_summaries_val.parquet".to_string(),
                    test: "event_summaries_test.parquet".to_string(),
                },
            },
            stats: DatasetBuildStats {
                total_events: 120,
                total_observations: 4_800,
                events_per_split: DatasetSplitCounts {
                    train: 84,
                    val: 18,
                    test: 18,
                },
                observations_per_split: DatasetSplitCounts {
                    train: 3_360,
                    val: 720,
                    test: 720,
                },
                skip_counts: DatasetSkipCounts {
                    missing_start_time: 2,
                    missing_end_time: 1,
                    missing_timing_fields: 3,
                },
            },
        };

        manifest
            .validate_contract()
            .expect("manifest must validate");

        let json = serde_json::to_string_pretty(&manifest).expect("manifest serializes");
        let reparsed: DatasetBuildManifest =
            serde_json::from_str(&json).expect("manifest deserializes");

        assert_eq!(reparsed, manifest);
        reparsed
            .validate_contract()
            .expect("round-tripped manifest must still validate");
    }
}
