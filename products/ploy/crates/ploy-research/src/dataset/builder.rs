use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};

use crate::factors::{
    build_task_grain_derived_artifacts_for_event_ids, FactorObservation, TaskGrainDerivedArtifacts,
};

use super::{
    assign_chronological_event_splits, build_canonical_event_chronology, DatasetArtifacts,
    DatasetBuildManifest, DatasetBuildStats, DatasetLabelContract, DatasetSourceWindow,
    DatasetSplit, DatasetSplitAssignment, DatasetSplitCounts, DatasetSplitPolicy, EventIndexEntry,
    EventMetadataChronologyInput, SplitBuildError, CANONICAL_REGIME_VERSION,
    DATASET_MANIFEST_VERSION,
};

#[derive(Debug, Clone)]
pub struct EventRootDatasetBuildRequest<'a> {
    pub observations: &'a [FactorObservation],
    pub chronology_events: Vec<EventMetadataChronologyInput>,
    pub source_window: DatasetSourceWindow,
    pub artifacts: DatasetArtifacts,
    pub built_at: DateTime<Utc>,
    pub split_policy: DatasetSplitPolicy,
    pub labels: DatasetLabelContract,
    pub feature_families: Vec<String>,
    pub regime_version: String,
}

impl<'a> EventRootDatasetBuildRequest<'a> {
    pub fn new(
        observations: &'a [FactorObservation],
        chronology_events: Vec<EventMetadataChronologyInput>,
        source_window: DatasetSourceWindow,
        artifacts: DatasetArtifacts,
        built_at: DateTime<Utc>,
    ) -> Self {
        Self {
            observations,
            chronology_events,
            source_window,
            artifacts,
            built_at,
            split_policy: DatasetSplitPolicy::default(),
            labels: DatasetLabelContract::default(),
            feature_families: Vec::new(),
            regime_version: CANONICAL_REGIME_VERSION.to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct EventRootDatasetBuild {
    pub event_index: Vec<EventIndexEntry>,
    pub split_assignments: Vec<DatasetSplitAssignment>,
    pub split_artifacts: DatasetSplitDerivedArtifacts,
    pub manifest: DatasetBuildManifest,
}

#[derive(Debug, Clone)]
pub struct DatasetSplitDerivedArtifacts {
    pub train: TaskGrainDerivedArtifacts,
    pub val: TaskGrainDerivedArtifacts,
    pub test: TaskGrainDerivedArtifacts,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DatasetBuildError {
    Split(SplitBuildError),
    DuplicateSplitAssignment { event_id: String },
    SplitAssignmentOrderMismatch { expected: String, found: String },
    ObservationEventMissingFromIndex { event_id: String },
    ManifestContract { message: String },
}

impl From<SplitBuildError> for DatasetBuildError {
    fn from(error: SplitBuildError) -> Self {
        Self::Split(error)
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct ObservationStats {
    observation_row_count: usize,
    settlement_label_available: bool,
    repricing_label_row_count_30s: usize,
}

pub fn build_event_root_dataset(
    request: EventRootDatasetBuildRequest<'_>,
) -> Result<EventRootDatasetBuild, DatasetBuildError> {
    let chronology = build_canonical_event_chronology(request.chronology_events);
    let split_assignments =
        assign_chronological_event_splits(&chronology.ordered_events, &request.split_policy)?;

    let mut assignment_by_event_id = BTreeMap::new();
    for assignment in &split_assignments {
        if assignment_by_event_id
            .insert(assignment.event_id.as_str(), assignment)
            .is_some()
        {
            return Err(DatasetBuildError::DuplicateSplitAssignment {
                event_id: assignment.event_id.clone(),
            });
        }
    }

    let mut observation_stats_by_event_id: BTreeMap<&str, ObservationStats> = BTreeMap::new();
    for observation in request.observations {
        if !assignment_by_event_id.contains_key(observation.event_id.as_str()) {
            return Err(DatasetBuildError::ObservationEventMissingFromIndex {
                event_id: observation.event_id.clone(),
            });
        }

        let stats = observation_stats_by_event_id
            .entry(observation.event_id.as_str())
            .or_default();
        stats.observation_row_count += 1;
        stats.settlement_label_available |= observation.settlement_up.is_finite();
        stats.repricing_label_row_count_30s += usize::from(
            observation
                .future_up_ask_change_30s
                .is_some_and(f64::is_finite),
        );
    }

    let mut event_index = Vec::with_capacity(split_assignments.len());
    for (event, assignment) in chronology
        .ordered_events
        .iter()
        .zip(split_assignments.iter())
    {
        if event.event_id != assignment.event_id {
            return Err(DatasetBuildError::SplitAssignmentOrderMismatch {
                expected: event.event_id.clone(),
                found: assignment.event_id.clone(),
            });
        }

        let observation_stats = observation_stats_by_event_id
            .get(event.event_id.as_str())
            .copied()
            .unwrap_or_default();

        event_index.push(EventIndexEntry {
            event_id: event.event_id.clone(),
            symbol: event.symbol.clone(),
            start_time: event.start_time,
            end_time: event.end_time,
            split: assignment.split,
            split_rank: assignment.split_rank,
            observation_row_count: observation_stats.observation_row_count,
            settlement_label_available: observation_stats.settlement_label_available,
            repricing_label_row_count_30s: observation_stats.repricing_label_row_count_30s,
            regime_version: request.regime_version.clone(),
        });
    }

    let split_artifacts = DatasetSplitDerivedArtifacts {
        train: build_task_grain_derived_artifacts_for_event_ids(
            request.observations,
            split_event_ids(&split_assignments, DatasetSplit::Train),
        ),
        val: build_task_grain_derived_artifacts_for_event_ids(
            request.observations,
            split_event_ids(&split_assignments, DatasetSplit::Val),
        ),
        test: build_task_grain_derived_artifacts_for_event_ids(
            request.observations,
            split_event_ids(&split_assignments, DatasetSplit::Test),
        ),
    };

    let manifest = DatasetBuildManifest {
        manifest_version: DATASET_MANIFEST_VERSION,
        built_at: request.built_at,
        source_window: request.source_window,
        split_policy: request.split_policy,
        labels: request.labels,
        feature_families: request.feature_families,
        regime_version: request.regime_version,
        artifacts: request.artifacts,
        stats: DatasetBuildStats {
            total_events: event_index.len(),
            total_observations: request.observations.len(),
            events_per_split: split_counts_for_events(&event_index),
            observations_per_split: split_counts_for_observations(
                request.observations,
                &assignment_by_event_id,
            ),
            skip_counts: chronology.skip_counts,
        },
    };

    manifest
        .validate_contract()
        .map_err(|message| DatasetBuildError::ManifestContract { message })?;

    Ok(EventRootDatasetBuild {
        event_index,
        split_assignments,
        split_artifacts,
        manifest,
    })
}

fn split_event_ids(
    split_assignments: &[DatasetSplitAssignment],
    split: DatasetSplit,
) -> BTreeSet<String> {
    split_assignments
        .iter()
        .filter(|assignment| assignment.split == split)
        .map(|assignment| assignment.event_id.clone())
        .collect()
}

fn split_counts_for_events(event_index: &[EventIndexEntry]) -> DatasetSplitCounts {
    let mut counts = DatasetSplitCounts::default();
    for entry in event_index {
        increment_split_count(&mut counts, entry.split, 1);
    }
    counts
}

fn split_counts_for_observations(
    observations: &[FactorObservation],
    assignment_by_event_id: &BTreeMap<&str, &DatasetSplitAssignment>,
) -> DatasetSplitCounts {
    let mut counts = DatasetSplitCounts::default();
    for observation in observations {
        if let Some(assignment) = assignment_by_event_id.get(observation.event_id.as_str()) {
            increment_split_count(&mut counts, assignment.split, 1);
        }
    }
    counts
}

fn increment_split_count(counts: &mut DatasetSplitCounts, split: DatasetSplit, amount: usize) {
    match split {
        DatasetSplit::Train => counts.train += amount,
        DatasetSplit::Val => counts.val += amount,
        DatasetSplit::Test => counts.test += amount,
    }
}

pub fn standard_event_root_dataset_artifacts() -> DatasetArtifacts {
    use super::DatasetSplitArtifactPaths;

    DatasetArtifacts {
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
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_event_root_dataset, standard_event_root_dataset_artifacts, DatasetBuildError,
        EventRootDatasetBuildRequest,
    };
    use crate::dataset::{DatasetSourceWindow, DatasetSplit, EventMetadataChronologyInput};
    use crate::factors::FactorObservation;
    use chrono::{Duration, TimeZone, Utc};
    use std::collections::HashSet;

    #[test]
    fn builder_materializes_event_root_splits_without_leakage() {
        let start = Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap();
        let chronology_events = synthetic_chronology_events(140, start);
        let observations = synthetic_observations(140, start);
        let request = EventRootDatasetBuildRequest::new(
            &observations,
            chronology_events,
            source_window(start, 140),
            standard_event_root_dataset_artifacts(),
            start,
        );

        let build = build_event_root_dataset(request).expect("dataset build should succeed");

        assert_eq!(build.event_index.len(), 140);
        assert_eq!(build.split_assignments.len(), 140);
        assert_eq!(build.manifest.stats.events_per_split.train, 98);
        assert_eq!(build.manifest.stats.events_per_split.val, 21);
        assert_eq!(build.manifest.stats.events_per_split.test, 21);
        assert_eq!(build.manifest.stats.observations_per_split.train, 196);
        assert_eq!(build.manifest.stats.observations_per_split.val, 42);
        assert_eq!(build.manifest.stats.observations_per_split.test, 42);
        assert_eq!(build.split_artifacts.train.observation_row_count(), 196);
        assert_eq!(build.split_artifacts.val.event_summary_count(), 21);
        assert_eq!(
            build.split_artifacts.test.repricing_label_row_count_30s(),
            21
        );
        build
            .manifest
            .validate_contract()
            .expect("manifest contract should remain valid");

        for entry in &build.event_index {
            assert_eq!(entry.observation_row_count, 2);
            assert!(entry.settlement_label_available);
            assert_eq!(entry.repricing_label_row_count_30s, 1);
        }

        let train_ids: HashSet<_> = build
            .split_artifacts
            .train
            .event_ids
            .iter()
            .map(String::as_str)
            .collect();
        let val_ids: HashSet<_> = build
            .split_artifacts
            .val
            .event_ids
            .iter()
            .map(String::as_str)
            .collect();
        let test_ids: HashSet<_> = build
            .split_artifacts
            .test
            .event_ids
            .iter()
            .map(String::as_str)
            .collect();

        assert!(train_ids.is_disjoint(&val_ids));
        assert!(train_ids.is_disjoint(&test_ids));
        assert!(val_ids.is_disjoint(&test_ids));
    }

    #[test]
    fn builder_fails_when_an_observation_cannot_be_split_by_event_id() {
        let start = Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap();
        let chronology_events = synthetic_chronology_events(140, start);
        let mut observations = synthetic_observations(140, start);
        observations.push(synthetic_observation(
            "evt-outside",
            "BTCUSDT",
            start,
            0,
            1.0,
            Some(0.01),
        ));

        let request = EventRootDatasetBuildRequest::new(
            &observations,
            chronology_events,
            source_window(start, 140),
            standard_event_root_dataset_artifacts(),
            start,
        );

        let error = build_event_root_dataset(request).expect_err("dataset build should fail");
        assert_eq!(
            error,
            DatasetBuildError::ObservationEventMissingFromIndex {
                event_id: "evt-outside".to_string(),
            }
        );
    }

    #[test]
    fn builder_carries_chronology_skip_counts_into_the_manifest() {
        let start = Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap();
        let mut chronology_events = synthetic_chronology_events(140, start);
        chronology_events.push(EventMetadataChronologyInput {
            event_id: "evt-missing-end".to_string(),
            symbol: "BTCUSDT".to_string(),
            start_time: Some(start),
            end_time: None,
        });
        let observations = synthetic_observations(140, start);
        let request = EventRootDatasetBuildRequest::new(
            &observations,
            chronology_events,
            source_window(start, 140),
            standard_event_root_dataset_artifacts(),
            start,
        );

        let build = build_event_root_dataset(request).expect("dataset build should succeed");

        assert_eq!(build.event_index.len(), 140);
        assert_eq!(build.manifest.stats.skip_counts.missing_end_time, 1);
        assert_eq!(build.manifest.stats.skip_counts.missing_timing_fields, 1);
    }

    fn synthetic_chronology_events(
        count: usize,
        start: chrono::DateTime<Utc>,
    ) -> Vec<EventMetadataChronologyInput> {
        (0..count)
            .rev()
            .map(|idx| EventMetadataChronologyInput {
                event_id: format!("evt-{idx:03}"),
                symbol: if idx % 2 == 0 { "BTCUSDT" } else { "ETHUSDT" }.to_string(),
                start_time: Some(start + Duration::minutes(idx as i64 * 5)),
                end_time: Some(start + Duration::minutes(idx as i64 * 5 + 5)),
            })
            .collect()
    }

    fn synthetic_observations(
        count: usize,
        start: chrono::DateTime<Utc>,
    ) -> Vec<FactorObservation> {
        (0..count)
            .flat_map(|idx| {
                let event_id = format!("evt-{idx:03}");
                let symbol = if idx % 2 == 0 { "BTCUSDT" } else { "ETHUSDT" };
                let event_start = start + Duration::minutes(idx as i64 * 5);
                [
                    synthetic_observation(&event_id, symbol, event_start, 0, 1.0, Some(0.01)),
                    synthetic_observation(&event_id, symbol, event_start, 1, 1.0, None),
                ]
            })
            .collect()
    }

    fn source_window(start: chrono::DateTime<Utc>, event_count: usize) -> DatasetSourceWindow {
        DatasetSourceWindow {
            start_time: start,
            end_time: start + Duration::minutes(event_count as i64 * 5),
            symbols: vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()],
        }
    }

    fn synthetic_observation(
        event_id: &str,
        symbol: &str,
        event_start: chrono::DateTime<Utc>,
        row_idx: i64,
        settlement_up: f64,
        repricing_label: Option<f64>,
    ) -> FactorObservation {
        FactorObservation {
            event_id: event_id.to_string(),
            symbol: symbol.to_string(),
            tick_ts: event_start + Duration::seconds(row_idx * 30),
            up_token_id: String::new(),
            down_token_id: String::new(),
            chainlink_reference_fresh: false,
            binance_spot_fresh: false,
            binance_lob_fresh: false,
            binance_agg_trade_fresh: false,
            event_window_secs: 300,
            time_remaining_secs: 300 - row_idx * 30,
            signed_distance_to_beat: 0.0,
            abs_distance_to_beat: 0.0,
            drift_10s: 0.0,
            drift_30s: 0.0,
            flip_age_secs: 0.0,
            post_flip_drift: 0.0,
            sigma_horizon: 1.0,
            fair_prob_up: 0.5,
            fair_prob_up_clean: 0.5,
            prob_disagreement: 0.0,
            implied_sigma_horizon: 0.1,
            vol_gap: 0.0,
            distance_over_sigma: 0.0,
            model_prob_up: 0.5,
            chainlink_prob_up: f64::NAN,
            model_edge_up: 0.0,
            reward_risk_up: 1.0,
            reward_risk_down: 1.0,
            obi: 0.0,
            spread_bps: 1.0,
            microprice_offset_bps: 0.0,
            bid_depth_near: 0.0,
            ask_depth_near: 0.0,
            depth_ratio: 1.0,
            depth_imbalance: 0.0,
            depth_far_ratio: 1.0,
            depth_acceleration: 0.0,
            obi_10: 0.0,
            pm_up_bid: 0.49,
            pm_up_ask: 0.51,
            pm_up_bid_size: 10.0,
            pm_up_ask_size: 11.0,
            pm_down_bid: 0.49,
            pm_down_ask: 0.51,
            pm_down_bid_size: 12.0,
            pm_down_ask_size: 13.0,
            pm_lag_secs: 0.0,
            settlement_up,
            future_up_ask_change_30s: repricing_label,
            future_up_ask_change_60s: None,
            cum_obi_delta_5m: 0.0,
            cum_depth_delta_5m: 0.0,
            cum_mprice_drift_5m: 0.0,
            cum_trade_imbalance_5m: 0.0,
            cex_bar_return_30s: 0.0,
            cex_bar_return_60s: 0.0,
            cex_bar_volume_ratio_30s: 0.0,
            cex_bar_volume_trend_3: 0.0,
            cex_signed_volume_ratio_30s: 0.0,
            cex_consecutive_up_bars: 0.0,
            cex_consecutive_down_bars: 0.0,
            cex_breakout_volume_score: 0.0,
        }
    }
}
