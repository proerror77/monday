use std::fs::{create_dir_all, File};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use polars::io::parquet::write::ParquetWriter;
use polars::prelude::*;

use crate::factors::{export_observations_parquet, EventFactorSummary};

use super::{DatasetSplit, DatasetSplitAssignment, EventIndexEntry, EventRootDatasetBuild};

#[derive(Debug)]
pub enum DatasetExportError {
    Io(std::io::Error),
    Json(serde_json::Error),
    ManifestContract { message: String },
    Polars(PolarsError),
}

impl From<std::io::Error> for DatasetExportError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for DatasetExportError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<PolarsError> for DatasetExportError {
    fn from(error: PolarsError) -> Self {
        Self::Polars(error)
    }
}

pub fn event_index_to_frame(rows: &[EventIndexEntry]) -> PolarsResult<DataFrame> {
    df![
        "event_id" => rows.iter().map(|row| row.event_id.as_str()).collect::<Vec<_>>(),
        "symbol" => rows.iter().map(|row| row.symbol.as_str()).collect::<Vec<_>>(),
        "start_time" => rows.iter().map(|row| row.start_time.timestamp_millis()).collect::<Vec<_>>(),
        "end_time" => rows.iter().map(|row| row.end_time.timestamp_millis()).collect::<Vec<_>>(),
        "split" => rows.iter().map(|row| split_name(row.split)).collect::<Vec<_>>(),
        "split_rank" => rows.iter().map(|row| row.split_rank as i64).collect::<Vec<_>>(),
        "observation_row_count" => rows.iter().map(|row| row.observation_row_count as i64).collect::<Vec<_>>(),
        "settlement_label_available" => rows.iter().map(|row| row.settlement_label_available).collect::<Vec<_>>(),
        "repricing_label_row_count_30s" => rows.iter().map(|row| row.repricing_label_row_count_30s as i64).collect::<Vec<_>>(),
        "regime_version" => rows.iter().map(|row| row.regime_version.as_str()).collect::<Vec<_>>(),
    ]
}

pub fn split_assignments_to_frame(rows: &[DatasetSplitAssignment]) -> PolarsResult<DataFrame> {
    df![
        "event_id" => rows.iter().map(|row| row.event_id.as_str()).collect::<Vec<_>>(),
        "symbol" => rows.iter().map(|row| row.symbol.as_str()).collect::<Vec<_>>(),
        "end_time" => rows.iter().map(|row| row.end_time.timestamp_millis()).collect::<Vec<_>>(),
        "ordered_event_index" => rows.iter().map(|row| row.ordered_event_index as i64).collect::<Vec<_>>(),
        "split" => rows.iter().map(|row| split_name(row.split)).collect::<Vec<_>>(),
        "split_rank" => rows.iter().map(|row| row.split_rank as i64).collect::<Vec<_>>(),
    ]
}

pub fn event_summaries_to_frame(rows: &[EventFactorSummary]) -> PolarsResult<DataFrame> {
    df![
        "event_id" => rows.iter().map(|row| row.event_id.as_str()).collect::<Vec<_>>(),
        "symbol" => rows.iter().map(|row| row.symbol.as_str()).collect::<Vec<_>>(),
        "last_tick_ts" => rows.iter().map(|row| row.last_tick_ts.timestamp_millis()).collect::<Vec<_>>(),
        "settlement_up" => rows.iter().map(|row| row.settlement_up).collect::<Vec<_>>(),
        "signed_distance_to_beat" => rows.iter().map(|row| row.signed_distance_to_beat).collect::<Vec<_>>(),
        "abs_distance_to_beat" => rows.iter().map(|row| row.abs_distance_to_beat).collect::<Vec<_>>(),
        "drift_10s" => rows.iter().map(|row| row.drift_10s).collect::<Vec<_>>(),
        "drift_30s" => rows.iter().map(|row| row.drift_30s).collect::<Vec<_>>(),
        "flip_age_secs" => rows.iter().map(|row| row.flip_age_secs).collect::<Vec<_>>(),
        "post_flip_drift" => rows.iter().map(|row| row.post_flip_drift).collect::<Vec<_>>(),
        "sigma_horizon" => rows.iter().map(|row| row.sigma_horizon).collect::<Vec<_>>(),
        "fair_prob_up" => rows.iter().map(|row| row.fair_prob_up).collect::<Vec<_>>(),
        "fair_prob_up_clean" => rows.iter().map(|row| row.fair_prob_up_clean).collect::<Vec<_>>(),
        "prob_disagreement" => rows.iter().map(|row| row.prob_disagreement).collect::<Vec<_>>(),
        "implied_sigma_horizon" => rows.iter().map(|row| row.implied_sigma_horizon).collect::<Vec<_>>(),
        "vol_gap" => rows.iter().map(|row| row.vol_gap).collect::<Vec<_>>(),
        "distance_over_sigma" => rows.iter().map(|row| row.distance_over_sigma).collect::<Vec<_>>(),
        "model_prob_up" => rows.iter().map(|row| row.model_prob_up).collect::<Vec<_>>(),
        "chainlink_prob_up" => rows.iter().map(|row| row.chainlink_prob_up).collect::<Vec<_>>(),
        "model_edge_up" => rows.iter().map(|row| row.model_edge_up).collect::<Vec<_>>(),
        "reward_risk_up" => rows.iter().map(|row| row.reward_risk_up).collect::<Vec<_>>(),
        "reward_risk_down" => rows.iter().map(|row| row.reward_risk_down).collect::<Vec<_>>(),
        "obi" => rows.iter().map(|row| row.obi).collect::<Vec<_>>(),
        "spread_bps" => rows.iter().map(|row| row.spread_bps).collect::<Vec<_>>(),
        "microprice_offset_bps" => rows.iter().map(|row| row.microprice_offset_bps).collect::<Vec<_>>(),
        "bid_depth_near" => rows.iter().map(|row| row.bid_depth_near).collect::<Vec<_>>(),
        "ask_depth_near" => rows.iter().map(|row| row.ask_depth_near).collect::<Vec<_>>(),
        "depth_ratio" => rows.iter().map(|row| row.depth_ratio).collect::<Vec<_>>(),
        "depth_imbalance" => rows.iter().map(|row| row.depth_imbalance).collect::<Vec<_>>(),
        "depth_far_ratio" => rows.iter().map(|row| row.depth_far_ratio).collect::<Vec<_>>(),
        "depth_acceleration" => rows.iter().map(|row| row.depth_acceleration).collect::<Vec<_>>(),
        "obi_10" => rows.iter().map(|row| row.obi_10).collect::<Vec<_>>(),
        "pm_up_bid" => rows.iter().map(|row| row.pm_up_bid).collect::<Vec<_>>(),
        "pm_up_ask" => rows.iter().map(|row| row.pm_up_ask).collect::<Vec<_>>(),
        "pm_up_bid_size" => rows.iter().map(|row| row.pm_up_bid_size).collect::<Vec<_>>(),
        "pm_up_ask_size" => rows.iter().map(|row| row.pm_up_ask_size).collect::<Vec<_>>(),
        "pm_down_bid" => rows.iter().map(|row| row.pm_down_bid).collect::<Vec<_>>(),
        "pm_down_ask" => rows.iter().map(|row| row.pm_down_ask).collect::<Vec<_>>(),
        "pm_down_bid_size" => rows.iter().map(|row| row.pm_down_bid_size).collect::<Vec<_>>(),
        "pm_down_ask_size" => rows.iter().map(|row| row.pm_down_ask_size).collect::<Vec<_>>(),
        "pm_lag_secs" => rows.iter().map(|row| row.pm_lag_secs).collect::<Vec<_>>(),
        "cum_obi_delta_5m" => rows.iter().map(|row| row.cum_obi_delta_5m).collect::<Vec<_>>(),
        "cum_depth_delta_5m" => rows.iter().map(|row| row.cum_depth_delta_5m).collect::<Vec<_>>(),
        "cum_mprice_drift_5m" => rows.iter().map(|row| row.cum_mprice_drift_5m).collect::<Vec<_>>(),
        "cum_trade_imbalance_5m" => rows.iter().map(|row| row.cum_trade_imbalance_5m).collect::<Vec<_>>(),
        "cex_bar_return_30s" => rows.iter().map(|row| row.cex_bar_return_30s).collect::<Vec<_>>(),
        "cex_bar_return_60s" => rows.iter().map(|row| row.cex_bar_return_60s).collect::<Vec<_>>(),
        "cex_bar_volume_ratio_30s" => rows.iter().map(|row| row.cex_bar_volume_ratio_30s).collect::<Vec<_>>(),
        "cex_bar_volume_trend_3" => rows.iter().map(|row| row.cex_bar_volume_trend_3).collect::<Vec<_>>(),
        "cex_signed_volume_ratio_30s" => rows.iter().map(|row| row.cex_signed_volume_ratio_30s).collect::<Vec<_>>(),
        "cex_consecutive_up_bars" => rows.iter().map(|row| row.cex_consecutive_up_bars).collect::<Vec<_>>(),
        "cex_consecutive_down_bars" => rows.iter().map(|row| row.cex_consecutive_down_bars).collect::<Vec<_>>(),
        "cex_breakout_volume_score" => rows.iter().map(|row| row.cex_breakout_volume_score).collect::<Vec<_>>(),
    ]
}

pub fn export_event_root_dataset_parquet(
    build: &EventRootDatasetBuild,
    output_root: &Path,
) -> Result<(), DatasetExportError> {
    build
        .manifest
        .validate_contract()
        .map_err(|message| DatasetExportError::ManifestContract { message })?;

    write_manifest_json(build, output_root)?;
    write_parquet(
        event_index_to_frame(&build.event_index)?,
        &artifact_path(output_root, &build.manifest.artifacts.event_index),
    )?;
    write_parquet(
        split_assignments_to_frame(&build.split_assignments)?,
        &artifact_path(output_root, &build.manifest.artifacts.split_assignments),
    )?;

    export_observations_with_parent_dirs(
        &build.split_artifacts.train.observation_rows,
        &artifact_path(output_root, &build.manifest.artifacts.observations.train),
    )?;
    export_observations_with_parent_dirs(
        &build.split_artifacts.val.observation_rows,
        &artifact_path(output_root, &build.manifest.artifacts.observations.val),
    )?;
    export_observations_with_parent_dirs(
        &build.split_artifacts.test.observation_rows,
        &artifact_path(output_root, &build.manifest.artifacts.observations.test),
    )?;

    write_parquet(
        event_summaries_to_frame(&build.split_artifacts.train.event_summaries)?,
        &artifact_path(output_root, &build.manifest.artifacts.event_summaries.train),
    )?;
    write_parquet(
        event_summaries_to_frame(&build.split_artifacts.val.event_summaries)?,
        &artifact_path(output_root, &build.manifest.artifacts.event_summaries.val),
    )?;
    write_parquet(
        event_summaries_to_frame(&build.split_artifacts.test.event_summaries)?,
        &artifact_path(output_root, &build.manifest.artifacts.event_summaries.test),
    )?;

    Ok(())
}

fn write_manifest_json(
    build: &EventRootDatasetBuild,
    output_root: &Path,
) -> Result<(), DatasetExportError> {
    let path = artifact_path(output_root, &build.manifest.artifacts.event_manifest);
    ensure_parent_dir(&path)?;
    let file = File::create(path)?;
    serde_json::to_writer_pretty(file, &build.manifest)?;
    Ok(())
}

fn export_observations_with_parent_dirs(
    rows: &[crate::factors::FactorObservation],
    path: &Path,
) -> Result<(), DatasetExportError> {
    ensure_parent_dir(path)?;
    export_observations_parquet(rows, path)?;
    Ok(())
}

fn write_parquet(mut df: DataFrame, path: &Path) -> Result<(), DatasetExportError> {
    ensure_parent_dir(path)?;
    let file = File::create(path).map_err(|error| PolarsError::IO {
        error: Arc::new(error),
        msg: None,
    })?;
    ParquetWriter::new(file).finish(&mut df)?;
    Ok(())
}

fn ensure_parent_dir(path: &Path) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        create_dir_all(parent)?;
    }
    Ok(())
}

fn artifact_path(output_root: &Path, artifact: &str) -> PathBuf {
    let artifact_path = Path::new(artifact);
    if artifact_path.is_absolute() {
        artifact_path.to_path_buf()
    } else {
        output_root.join(artifact_path)
    }
}

fn split_name(split: DatasetSplit) -> &'static str {
    match split {
        DatasetSplit::Train => "train",
        DatasetSplit::Val => "val",
        DatasetSplit::Test => "test",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        event_index_to_frame, event_summaries_to_frame, export_event_root_dataset_parquet,
        split_assignments_to_frame,
    };
    use crate::dataset::{
        build_event_root_dataset, standard_event_root_dataset_artifacts, DatasetSourceWindow,
        EventMetadataChronologyInput, EventRootDatasetBuildRequest,
    };
    use crate::factors::FactorObservation;
    use chrono::{Duration, TimeZone, Utc};
    use std::collections::HashSet;
    use std::fs;

    #[test]
    fn frames_preserve_dataset_artifact_row_grain() {
        let build = synthetic_build();

        assert_eq!(
            event_index_to_frame(&build.event_index).unwrap().height(),
            140
        );
        assert_eq!(
            split_assignments_to_frame(&build.split_assignments)
                .unwrap()
                .height(),
            140
        );
        assert_eq!(
            event_summaries_to_frame(&build.split_artifacts.train.event_summaries)
                .unwrap()
                .height(),
            98
        );
        assert_eq!(
            event_summaries_to_frame(&build.split_artifacts.val.event_summaries)
                .unwrap()
                .height(),
            21
        );
        assert_eq!(
            event_summaries_to_frame(&build.split_artifacts.test.event_summaries)
                .unwrap()
                .height(),
            21
        );
    }

    #[test]
    fn export_writes_manifest_and_all_split_parquet_artifacts() {
        let build = synthetic_build();
        let root = std::env::temp_dir().join(format!(
            "ploy-event-root-export-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));

        export_event_root_dataset_parquet(&build, &root).expect("export should succeed");

        let expected_paths = [
            &build.manifest.artifacts.event_manifest,
            &build.manifest.artifacts.event_index,
            &build.manifest.artifacts.split_assignments,
            &build.manifest.artifacts.observations.train,
            &build.manifest.artifacts.observations.val,
            &build.manifest.artifacts.observations.test,
            &build.manifest.artifacts.event_summaries.train,
            &build.manifest.artifacts.event_summaries.val,
            &build.manifest.artifacts.event_summaries.test,
        ];

        for artifact in expected_paths {
            let path = root.join(artifact);
            let metadata = fs::metadata(&path).unwrap_or_else(|_| {
                panic!("expected artifact to exist: {}", path.display());
            });
            assert!(
                metadata.len() > 0,
                "expected artifact to be non-empty: {}",
                path.display()
            );
        }

        let manifest_json =
            fs::read_to_string(root.join(&build.manifest.artifacts.event_manifest)).unwrap();
        assert!(manifest_json.contains("\"total_events\": 140"));
        assert!(manifest_json.contains("\"total_observations\": 280"));

        fs::remove_dir_all(root).expect("temporary export directory should be removed");
    }

    fn synthetic_build() -> crate::dataset::EventRootDatasetBuild {
        let start = Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap();
        let chronology_events = synthetic_chronology_events(140, start);
        let observations = synthetic_observations(140, start);
        let request = EventRootDatasetBuildRequest::new(
            &observations,
            chronology_events,
            DatasetSourceWindow {
                start_time: start,
                end_time: start + Duration::minutes(700),
                symbols: vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()],
            },
            standard_event_root_dataset_artifacts(),
            start,
        );

        build_event_root_dataset(request).expect("synthetic dataset should build")
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
        let selected_test_label_events: HashSet<usize> = (119..140).collect();
        (0..count)
            .flat_map(|idx| {
                let event_id = format!("evt-{idx:03}");
                let symbol = if idx % 2 == 0 { "BTCUSDT" } else { "ETHUSDT" };
                let event_start = start + Duration::minutes(idx as i64 * 5);
                [
                    synthetic_observation(
                        &event_id,
                        symbol,
                        event_start,
                        0,
                        1.0,
                        selected_test_label_events.contains(&idx).then_some(0.01),
                    ),
                    synthetic_observation(&event_id, symbol, event_start, 1, 1.0, None),
                ]
            })
            .collect()
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
            official_resolution_observed_at: None,
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
