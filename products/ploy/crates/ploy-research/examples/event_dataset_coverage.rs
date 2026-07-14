use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use ploy_research::DatasetBuildManifest;
use polars::io::parquet::read::ParquetReader;
use polars::prelude::*;

const DEFAULT_FEATURES: &[&str] = &[
    "signed_distance_to_beat",
    "abs_distance_to_beat",
    "drift_10s",
    "drift_30s",
    "flip_age_secs",
    "post_flip_drift",
    "sigma_horizon",
    "fair_prob_up",
    "fair_prob_up_clean",
    "prob_disagreement",
    "implied_sigma_horizon",
    "vol_gap",
    "distance_over_sigma",
    "model_prob_up",
    "model_edge_up",
    "reward_risk_up",
    "reward_risk_down",
    "obi",
    "spread_bps",
    "microprice_offset_bps",
    "depth_ratio",
    "depth_imbalance",
    "depth_far_ratio",
    "depth_acceleration",
    "obi_10",
    "pm_lag_secs",
];

#[derive(Debug, Clone)]
struct Config {
    dataset_root: PathBuf,
    entry_secs: Vec<i64>,
    tolerances: Vec<i64>,
    features: Vec<String>,
}

#[derive(Debug, Clone)]
struct ObservationStatus {
    time_remaining_secs: i64,
    has_binary_label: bool,
    has_valid_price: bool,
    has_finite_features: bool,
    feature_finite: Vec<bool>,
}

#[derive(Debug, Clone)]
struct SplitCoverage {
    name: &'static str,
    source_rows: usize,
    events: BTreeMap<String, Vec<ObservationStatus>>,
}

#[derive(Debug, Default, Clone)]
struct CoverageStats {
    split: &'static str,
    entry_secs: i64,
    tolerance_secs: i64,
    events: usize,
    covered: usize,
    missing_window: usize,
    invalid_label: usize,
    invalid_price: usize,
    invalid_features: usize,
    avg_time_remaining_secs: f64,
    avg_abs_error_secs: f64,
}

fn main() -> Result<()> {
    let config = parse_config()?;
    let manifest = read_manifest(&config.dataset_root)?;
    manifest
        .validate_contract()
        .map_err(|message| anyhow::anyhow!(message))
        .context("dataset manifest contract validation failed")?;

    let train = load_split_coverage(
        "train",
        &artifact_path(&config.dataset_root, &manifest.artifacts.observations.train),
        &config.features,
    )?;
    let val = load_split_coverage(
        "val",
        &artifact_path(&config.dataset_root, &manifest.artifacts.observations.val),
        &config.features,
    )?;
    let test = load_split_coverage(
        "test",
        &artifact_path(&config.dataset_root, &manifest.artifacts.observations.test),
        &config.features,
    )?;

    print_report(&manifest, &config, [&train, &val, &test]);
    Ok(())
}

fn parse_config() -> Result<Config> {
    let args: Vec<String> = std::env::args().collect();
    let dataset_root = flag_value(&args, "--dataset")
        .or_else(|| flag_value(&args, "--dataset-root"))
        .map(PathBuf::from)
        .context("--dataset <event-root-dataset-dir> is required")?;
    let entry_secs = parse_i64_list_flag(&args, "--entry-secs", &[30, 60, 90, 120, 180, 240])?;
    let tolerances = parse_i64_list_flag(&args, "--tolerances", &[5, 15, 30, 60, 300])?;
    let features = flag_value(&args, "--features")
        .map(|raw| parse_string_list(&raw))
        .filter(|features| !features.is_empty())
        .unwrap_or_else(|| {
            DEFAULT_FEATURES
                .iter()
                .map(|name| (*name).to_string())
                .collect()
        });

    if entry_secs.iter().any(|seconds| *seconds < 0) {
        bail!("--entry-secs values must be non-negative");
    }
    if tolerances.iter().any(|seconds| *seconds < 0) {
        bail!("--tolerances values must be non-negative");
    }

    Ok(Config {
        dataset_root,
        entry_secs,
        tolerances,
        features,
    })
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
}

fn parse_i64_list_flag(args: &[String], flag: &str, default: &[i64]) -> Result<Vec<i64>> {
    let mut values = flag_value(args, flag)
        .map(|raw| {
            parse_string_list(&raw)
                .into_iter()
                .map(|value| {
                    value
                        .parse::<i64>()
                        .with_context(|| format!("invalid {flag} value: {value}"))
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_else(|| default.to_vec());
    values.sort_unstable();
    values.dedup();
    Ok(values)
}

fn parse_string_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn read_manifest(root: &Path) -> Result<DatasetBuildManifest> {
    let path = root.join("event_manifest.json");
    let file = File::open(&path).with_context(|| format!("open manifest {}", path.display()))?;
    serde_json::from_reader(file).with_context(|| format!("parse manifest {}", path.display()))
}

fn artifact_path(root: &Path, artifact: &str) -> PathBuf {
    let path = Path::new(artifact);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn load_split_coverage(
    name: &'static str,
    path: &Path,
    feature_names: &[String],
) -> Result<SplitCoverage> {
    let file = File::open(path).with_context(|| format!("open parquet {}", path.display()))?;
    let df = ParquetReader::new(file)
        .finish()
        .with_context(|| format!("read parquet {}", path.display()))?;
    let source_rows = df.height();

    let event_ids = df.column("event_id")?.str()?;
    let time_remaining = df.column("time_remaining_secs")?.i64()?;
    let labels = df.column("settlement_up")?.f64()?;
    let pm_up_ask = df.column("pm_up_ask")?.f64()?;
    let pm_down_ask = df.column("pm_down_ask")?.f64()?;
    let feature_columns = feature_names
        .iter()
        .map(|feature| {
            df.column(feature)
                .with_context(|| format!("feature column missing: {feature}"))?
                .f64()
                .with_context(|| format!("feature column must be f64: {feature}"))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut events: BTreeMap<String, Vec<ObservationStatus>> = BTreeMap::new();
    for row_idx in 0..df.height() {
        let Some(event_id) = event_ids.get(row_idx) else {
            continue;
        };
        let Some(seconds) = time_remaining.get(row_idx) else {
            continue;
        };

        let label = labels.get(row_idx).unwrap_or(f64::NAN);
        let up_ask = pm_up_ask.get(row_idx).unwrap_or(f64::NAN);
        let down_ask = pm_down_ask.get(row_idx).unwrap_or(f64::NAN);
        let feature_finite = feature_columns
            .iter()
            .map(|column| column.get(row_idx).is_some_and(f64::is_finite))
            .collect::<Vec<_>>();
        let has_finite_features = feature_finite.iter().all(|finite| *finite);

        events
            .entry(event_id.to_string())
            .or_default()
            .push(ObservationStatus {
                time_remaining_secs: seconds,
                has_binary_label: is_binary_label(label),
                has_valid_price: valid_entry_price(up_ask) && valid_entry_price(down_ask),
                has_finite_features,
                feature_finite,
            });
    }

    Ok(SplitCoverage {
        name,
        source_rows,
        events,
    })
}

fn is_binary_label(value: f64) -> bool {
    value.is_finite() && (value == 0.0 || value == 1.0)
}

fn valid_entry_price(value: f64) -> bool {
    value.is_finite() && value > 0.0 && value < 1.0
}

fn print_report(manifest: &DatasetBuildManifest, config: &Config, splits: [&SplitCoverage; 3]) {
    eprintln!("=== Event Dataset Coverage ===");
    eprintln!("dataset={}", config.dataset_root.display());
    eprintln!(
        "source_window={} -> {} symbols={:?}",
        manifest.source_window.start_time,
        manifest.source_window.end_time,
        manifest.source_window.symbols
    );
    eprintln!(
        "manifest_events={} observations={} split_events train={} val={} test={}",
        manifest.stats.total_events,
        manifest.stats.total_observations,
        manifest.stats.events_per_split.train,
        manifest.stats.events_per_split.val,
        manifest.stats.events_per_split.test
    );
    eprintln!(
        "entry_secs={:?} tolerances={:?} features={}",
        config.entry_secs,
        config.tolerances,
        config.features.len()
    );

    eprintln!();
    eprintln!("Base coverage by split:");
    eprintln!(
        "{:<6} {:>10} {:>7} {:>7} {:>7} {:>7} {:>9}",
        "split", "src_rows", "events", "label", "price", "feature", "all_any"
    );
    for split in splits {
        let base = base_coverage(split);
        eprintln!(
            "{:<6} {:>10} {:>7} {:>7} {:>7} {:>7} {:>8}",
            split.name,
            split.source_rows,
            split.events.len(),
            base.has_label,
            base.has_price,
            base.has_features,
            base.has_all
        );
    }

    eprintln!();
    eprintln!("Finite feature coverage by split:");
    eprintln!(
        "{:<28} {:>12} {:>12} {:>12}",
        "feature", "train", "val", "test"
    );
    for (feature_idx, feature) in config.features.iter().enumerate() {
        let train = finite_feature_event_count(splits[0], feature_idx);
        let val = finite_feature_event_count(splits[1], feature_idx);
        let test = finite_feature_event_count(splits[2], feature_idx);
        eprintln!(
            "{:<28} {:>5}/{:<6} {:>5}/{:<6} {:>5}/{:<6}",
            feature,
            train,
            splits[0].events.len(),
            val,
            splits[1].events.len(),
            test,
            splits[2].events.len()
        );
    }

    eprintln!();
    eprintln!("Entry coverage:");
    eprintln!(
        "{:<6} {:>6} {:>5} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>8}",
        "split",
        "entry",
        "tol",
        "events",
        "cover",
        "cover%",
        "no_win",
        "label",
        "price",
        "feature",
        "avg_err"
    );
    for split in splits {
        for entry_secs in &config.entry_secs {
            for tolerance_secs in &config.tolerances {
                let stats = coverage_stats(split, *entry_secs, *tolerance_secs);
                let coverage_pct = if stats.events > 0 {
                    stats.covered as f64 / stats.events as f64 * 100.0
                } else {
                    0.0
                };
                eprintln!(
                    "{:<6} {:>6} {:>5} {:>7} {:>7} {:>6.1}% {:>7} {:>7} {:>7} {:>7} {:>8.1}",
                    stats.split,
                    stats.entry_secs,
                    stats.tolerance_secs,
                    stats.events,
                    stats.covered,
                    coverage_pct,
                    stats.missing_window,
                    stats.invalid_label,
                    stats.invalid_price,
                    stats.invalid_features,
                    stats.avg_abs_error_secs
                );
            }
        }
    }

    eprintln!(
        "\nNote: coverage counts require one row per event with binary settlement label, valid up/down ask prices, and finite selected ML features. This is data-readiness diagnostics, not model performance."
    );
}

#[derive(Debug, Default)]
struct BaseCoverage {
    has_label: usize,
    has_price: usize,
    has_features: usize,
    has_all: usize,
}

fn base_coverage(split: &SplitCoverage) -> BaseCoverage {
    split
        .events
        .values()
        .fold(BaseCoverage::default(), |mut base, observations| {
            if observations.iter().any(|obs| obs.has_binary_label) {
                base.has_label += 1;
            }
            if observations.iter().any(|obs| obs.has_valid_price) {
                base.has_price += 1;
            }
            if observations.iter().any(|obs| obs.has_finite_features) {
                base.has_features += 1;
            }
            if observations.iter().any(|obs| obs.is_usable()) {
                base.has_all += 1;
            }
            base
        })
}

fn finite_feature_event_count(split: &SplitCoverage, feature_idx: usize) -> usize {
    split
        .events
        .values()
        .filter(|observations| {
            observations.iter().any(|obs| {
                obs.feature_finite
                    .get(feature_idx)
                    .copied()
                    .unwrap_or(false)
            })
        })
        .count()
}

fn coverage_stats(split: &SplitCoverage, entry_secs: i64, tolerance_secs: i64) -> CoverageStats {
    let mut stats = CoverageStats {
        split: split.name,
        entry_secs,
        tolerance_secs,
        events: split.events.len(),
        ..CoverageStats::default()
    };
    let mut selected_time_sum = 0.0;
    let mut selected_abs_error_sum = 0.0;

    for observations in split.events.values() {
        let window = observations
            .iter()
            .filter(|obs| (obs.time_remaining_secs - entry_secs).abs() <= tolerance_secs)
            .collect::<Vec<_>>();
        if window.is_empty() {
            stats.missing_window += 1;
            continue;
        }

        let Some(selected) = window
            .iter()
            .filter(|obs| obs.is_usable())
            .min_by_key(|obs| (obs.time_remaining_secs - entry_secs).abs())
        else {
            classify_invalid_window(&mut stats, &window);
            continue;
        };

        stats.covered += 1;
        selected_time_sum += selected.time_remaining_secs as f64;
        selected_abs_error_sum += (selected.time_remaining_secs - entry_secs).abs() as f64;
    }

    if stats.covered > 0 {
        stats.avg_time_remaining_secs = selected_time_sum / stats.covered as f64;
        stats.avg_abs_error_secs = selected_abs_error_sum / stats.covered as f64;
    }
    stats
}

fn classify_invalid_window(stats: &mut CoverageStats, window: &[&ObservationStatus]) {
    if !window.iter().any(|obs| obs.has_binary_label) {
        stats.invalid_label += 1;
    } else if !window
        .iter()
        .any(|obs| obs.has_binary_label && obs.has_valid_price)
    {
        stats.invalid_price += 1;
    } else {
        stats.invalid_features += 1;
    }
}

impl ObservationStatus {
    fn is_usable(&self) -> bool {
        self.has_binary_label && self.has_valid_price && self.has_finite_features
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn obs(
        time_remaining_secs: i64,
        has_binary_label: bool,
        has_valid_price: bool,
        has_finite_features: bool,
    ) -> ObservationStatus {
        ObservationStatus {
            time_remaining_secs,
            has_binary_label,
            has_valid_price,
            has_finite_features,
            feature_finite: vec![has_finite_features],
        }
    }

    fn split(events: &[(&str, Vec<ObservationStatus>)]) -> SplitCoverage {
        SplitCoverage {
            name: "test",
            source_rows: events.iter().map(|(_, rows)| rows.len()).sum(),
            events: events
                .iter()
                .map(|(event_id, rows)| ((*event_id).to_string(), rows.clone()))
                .collect(),
        }
    }

    #[test]
    fn coverage_stats_counts_usable_nearest_rows() {
        let split = split(&[
            (
                "a",
                vec![obs(62, true, true, true), obs(90, true, true, true)],
            ),
            ("b", vec![obs(61, true, false, true)]),
            ("c", vec![obs(10, true, true, true)]),
        ]);

        let stats = coverage_stats(&split, 60, 5);

        assert_eq!(stats.events, 3);
        assert_eq!(stats.covered, 1);
        assert_eq!(stats.invalid_price, 1);
        assert_eq!(stats.missing_window, 1);
        assert_eq!(stats.avg_time_remaining_secs, 62.0);
        assert_eq!(stats.avg_abs_error_secs, 2.0);
    }

    #[test]
    fn base_coverage_counts_any_row_per_event() {
        let split = split(&[
            (
                "a",
                vec![obs(60, true, false, true), obs(30, true, true, true)],
            ),
            ("b", vec![obs(60, true, true, false)]),
        ]);

        let base = base_coverage(&split);

        assert_eq!(base.has_label, 2);
        assert_eq!(base.has_price, 2);
        assert_eq!(base.has_features, 1);
        assert_eq!(base.has_all, 1);
    }

    #[test]
    fn finite_feature_event_count_counts_any_finite_row() {
        let split = split(&[
            (
                "a",
                vec![ObservationStatus {
                    time_remaining_secs: 60,
                    has_binary_label: true,
                    has_valid_price: true,
                    has_finite_features: false,
                    feature_finite: vec![true, false],
                }],
            ),
            (
                "b",
                vec![ObservationStatus {
                    time_remaining_secs: 60,
                    has_binary_label: true,
                    has_valid_price: true,
                    has_finite_features: false,
                    feature_finite: vec![false, true],
                }],
            ),
        ]);

        assert_eq!(finite_feature_event_count(&split, 0), 1);
        assert_eq!(finite_feature_event_count(&split, 1), 1);
        assert_eq!(finite_feature_event_count(&split, 2), 0);
    }

    #[test]
    fn parse_i64_list_flag_sorts_and_deduplicates() {
        let args = vec![
            "cmd".to_string(),
            "--entry-secs".to_string(),
            "120,60,120".to_string(),
        ];

        let values = parse_i64_list_flag(&args, "--entry-secs", &[30]).unwrap();

        assert_eq!(values, vec![60, 120]);
    }

    #[test]
    fn parse_string_list_omits_empty_items() {
        assert_eq!(parse_string_list("a,, b, "), vec!["a", "b"]);
    }

    #[test]
    fn usable_requires_label_price_and_features() {
        assert!(obs(60, true, true, true).is_usable());
        assert!(!obs(60, true, false, true).is_usable());
        assert!(!obs(60, false, true, true).is_usable());
        assert!(!obs(60, true, true, false).is_usable());
    }

    #[test]
    fn all_configured_default_features_are_unique() {
        let unique = DEFAULT_FEATURES.iter().collect::<BTreeSet<_>>();
        assert_eq!(unique.len(), DEFAULT_FEATURES.len());
    }
}
