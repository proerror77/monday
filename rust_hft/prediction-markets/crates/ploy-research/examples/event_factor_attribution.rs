use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use ploy_research::{
    register_automl_attributions, AutomlFactorAttribution, DatasetBuildManifest, FactorRegistry,
    Regime,
};
use polars::io::parquet::read::ParquetReader;
use polars::prelude::*;
use serde::Serialize;

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
    "obi_10",
    "pm_lag_secs",
];
const ATTRIBUTION_TARGET_LABEL: &str = "settlement_up";
const FACTOR_REGISTRY_ARTIFACT_KIND: &str = "event_ml_factor_registry";
const FACTOR_REGISTRY_ARTIFACT_VERSION: u32 = 1;

#[derive(Debug, Clone)]
struct Config {
    dataset_root: PathBuf,
    entry_secs: i64,
    tolerance_secs: i64,
    top_n: usize,
    output_dir: Option<PathBuf>,
    whitelist_min_importance: f64,
    whitelist_min_stability: f64,
    whitelist_max_features: usize,
    features: Vec<String>,
}

#[derive(Debug, Clone)]
struct Sample {
    y: f64,
    x: Vec<f64>,
}

#[derive(Debug, Clone)]
struct SplitDataset {
    name: &'static str,
    samples: Vec<Sample>,
}

#[derive(Debug, Clone)]
struct FactorAttribution {
    feature: String,
    direction: i8,
    train_auc_lift: f64,
    val_auc_lift: f64,
    test_auc_lift: f64,
    importance: f64,
    stability: f64,
    train_corr: f64,
    val_corr: f64,
    test_corr: f64,
}

#[derive(Debug, Serialize)]
struct AttributionArtifact<'a> {
    dataset: String,
    entry_secs: i64,
    tolerance_secs: i64,
    regime: String,
    features_considered: usize,
    selected_events: Vec<SplitCount>,
    whitelist_min_importance: f64,
    whitelist_min_stability: f64,
    whitelist: Vec<&'a str>,
    attributions: Vec<AttributionRow<'a>>,
}

#[derive(Debug, Serialize)]
struct SplitCount {
    split: &'static str,
    selected_events: usize,
}

#[derive(Debug, Serialize)]
struct AttributionRow<'a> {
    feature: &'a str,
    direction: i8,
    importance: Option<f64>,
    stability: Option<f64>,
    train_auc_lift: Option<f64>,
    val_auc_lift: Option<f64>,
    test_auc_lift: Option<f64>,
    train_corr: Option<f64>,
    val_corr: Option<f64>,
    test_corr: Option<f64>,
}

#[derive(Debug, Serialize)]
struct EventMlFactorRegistryArtifact {
    kind: &'static str,
    version: u32,
    dataset: String,
    entry_secs: i64,
    tolerance_secs: i64,
    regime: String,
    target_label: &'static str,
    selected_events: Vec<SplitCount>,
    whitelist_features: Vec<String>,
    factors: Vec<EventMlFactorRegistryRow>,
}

#[derive(Debug, Serialize)]
struct EventMlFactorRegistryRow {
    factor_name: String,
    source_feature: String,
    target_label: String,
    regime: String,
    direction: i8,
    train_derived_direction: i8,
    registry_score: Option<f64>,
    importance: Option<f64>,
    stability: Option<f64>,
    train_auc_lift: Option<f64>,
    val_auc_lift: Option<f64>,
    test_auc_lift: Option<f64>,
    train_corr: Option<f64>,
    val_corr: Option<f64>,
    test_corr: Option<f64>,
    whitelist_included: bool,
    status: &'static str,
    blockers: Vec<&'static str>,
}

fn main() -> Result<()> {
    let config = parse_config()?;
    let manifest = read_manifest(&config.dataset_root)?;
    manifest
        .validate_contract()
        .map_err(|message| anyhow::anyhow!(message))
        .context("dataset manifest contract validation failed")?;

    let train = load_split_dataset(
        "train",
        &artifact_path(&config.dataset_root, &manifest.artifacts.observations.train),
        &config.features,
        config.entry_secs,
        config.tolerance_secs,
    )?;
    let val = load_split_dataset(
        "val",
        &artifact_path(&config.dataset_root, &manifest.artifacts.observations.val),
        &config.features,
        config.entry_secs,
        config.tolerance_secs,
    )?;
    let test = load_split_dataset(
        "test",
        &artifact_path(&config.dataset_root, &manifest.artifacts.observations.test),
        &config.features,
        config.entry_secs,
        config.tolerance_secs,
    )?;

    if train.samples.len() < 20 {
        bail!(
            "training sample too small for attribution: {} selected events",
            train.samples.len()
        );
    }

    let attributions = attribute_features(&config.features, &train, &val, &test);
    let regime = Regime::from_secs(config.entry_secs);
    let mut registry = FactorRegistry::new();
    let registry_items = attributions
        .iter()
        .map(|item| AutomlFactorAttribution {
            name: item.feature.clone(),
            importance: item.importance,
            direction: item.direction,
            stability: item.stability,
        })
        .collect::<Vec<_>>();
    register_automl_attributions(
        &mut registry,
        regime,
        ATTRIBUTION_TARGET_LABEL,
        &registry_items,
    );

    print_report(
        &manifest,
        &config,
        [&train, &val, &test],
        &attributions,
        &registry,
        regime,
    );
    if let Some(output_dir) = &config.output_dir {
        write_artifacts(
            output_dir,
            &config,
            [&train, &val, &test],
            &attributions,
            &registry,
            regime,
        )?;
    }

    Ok(())
}

fn parse_config() -> Result<Config> {
    let args: Vec<String> = std::env::args().collect();
    let dataset_root = flag_value(&args, "--dataset")
        .or_else(|| flag_value(&args, "--dataset-root"))
        .map(PathBuf::from)
        .context("--dataset <event-root-dataset-dir> is required")?;
    let entry_secs = parse_i64_flag(&args, "--entry-secs", 60)?;
    let tolerance_secs = parse_i64_flag(&args, "--tolerance-secs", 30)?;
    let top_n = parse_usize_flag(&args, "--top-n", 12)?;
    let output_dir = flag_value(&args, "--output-dir").map(PathBuf::from);
    let whitelist_min_importance = parse_f64_flag(&args, "--whitelist-min-importance", 0.05)?;
    let whitelist_min_stability = parse_f64_flag(&args, "--whitelist-min-stability", 0.0)?;
    let whitelist_max_features = parse_usize_flag(&args, "--whitelist-max-features", top_n)?;
    let features = flag_value(&args, "--features")
        .map(|raw| parse_string_list(&raw))
        .filter(|features| !features.is_empty())
        .unwrap_or_else(|| {
            DEFAULT_FEATURES
                .iter()
                .map(|name| (*name).to_string())
                .collect()
        });

    if entry_secs < 0 {
        bail!("--entry-secs must be non-negative");
    }
    if tolerance_secs < 0 {
        bail!("--tolerance-secs must be non-negative");
    }
    if top_n == 0 {
        bail!("--top-n must be positive");
    }
    if whitelist_min_importance < 0.0 {
        bail!("--whitelist-min-importance must be non-negative");
    }
    if whitelist_min_stability < 0.0 {
        bail!("--whitelist-min-stability must be non-negative");
    }
    if whitelist_max_features == 0 {
        bail!("--whitelist-max-features must be positive");
    }

    Ok(Config {
        dataset_root,
        entry_secs,
        tolerance_secs,
        top_n,
        output_dir,
        whitelist_min_importance,
        whitelist_min_stability,
        whitelist_max_features,
        features,
    })
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
}

fn parse_i64_flag(args: &[String], flag: &str, default: i64) -> Result<i64> {
    flag_value(args, flag)
        .map(|raw| {
            raw.parse::<i64>()
                .with_context(|| format!("invalid {flag}: {raw}"))
        })
        .transpose()
        .map(|value| value.unwrap_or(default))
}

fn parse_usize_flag(args: &[String], flag: &str, default: usize) -> Result<usize> {
    flag_value(args, flag)
        .map(|raw| {
            raw.parse::<usize>()
                .with_context(|| format!("invalid {flag}: {raw}"))
        })
        .transpose()
        .map(|value| value.unwrap_or(default))
}

fn parse_f64_flag(args: &[String], flag: &str, default: f64) -> Result<f64> {
    flag_value(args, flag)
        .map(|raw| {
            raw.parse::<f64>()
                .with_context(|| format!("invalid {flag}: {raw}"))
        })
        .transpose()
        .map(|value| value.unwrap_or(default))
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

fn load_split_dataset(
    name: &'static str,
    path: &Path,
    feature_names: &[String],
    entry_secs: i64,
    tolerance_secs: i64,
) -> Result<SplitDataset> {
    let file = File::open(path).with_context(|| format!("open parquet {}", path.display()))?;
    let df = ParquetReader::new(file)
        .finish()
        .with_context(|| format!("read parquet {}", path.display()))?;

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

    let mut by_event: BTreeMap<String, (i64, Sample)> = BTreeMap::new();
    for row_idx in 0..df.height() {
        let Some(event_id) = event_ids.get(row_idx) else {
            continue;
        };
        let Some(seconds) = time_remaining.get(row_idx) else {
            continue;
        };
        let distance = (seconds - entry_secs).abs();
        if distance > tolerance_secs {
            continue;
        }

        let Some(y) = labels.get(row_idx) else {
            continue;
        };
        let Some(up_ask) = pm_up_ask.get(row_idx) else {
            continue;
        };
        let Some(down_ask) = pm_down_ask.get(row_idx) else {
            continue;
        };
        if !is_binary_label(y) || !valid_entry_price(up_ask) || !valid_entry_price(down_ask) {
            continue;
        }

        let mut x = Vec::with_capacity(feature_columns.len());
        let mut valid = true;
        for column in &feature_columns {
            let value = column.get(row_idx).unwrap_or(f64::NAN);
            if value.is_finite() {
                x.push(value);
            } else {
                valid = false;
                break;
            }
        }
        if !valid {
            continue;
        }

        let sample = Sample { y, x };
        match by_event.get(event_id) {
            Some((current_distance, _)) if *current_distance <= distance => {}
            _ => {
                by_event.insert(event_id.to_string(), (distance, sample));
            }
        }
    }

    Ok(SplitDataset {
        name,
        samples: by_event.into_values().map(|(_, sample)| sample).collect(),
    })
}

fn is_binary_label(value: f64) -> bool {
    value.is_finite() && (value == 0.0 || value == 1.0)
}

fn valid_entry_price(value: f64) -> bool {
    value.is_finite() && value > 0.0 && value < 1.0
}

fn attribute_features(
    feature_names: &[String],
    train: &SplitDataset,
    val: &SplitDataset,
    test: &SplitDataset,
) -> Vec<FactorAttribution> {
    let mut attributions = feature_names
        .iter()
        .enumerate()
        .map(|(feature_idx, feature)| {
            let train_auc_lift = signed_auc_lift(train, feature_idx);
            let val_auc_lift = signed_auc_lift(val, feature_idx);
            let test_auc_lift = signed_auc_lift(test, feature_idx);
            let direction = if train_auc_lift >= 0.0 { 1 } else { -1 };
            let importance = val_auc_lift.abs();
            let stability = stability_score(train_auc_lift, val_auc_lift, test_auc_lift);

            FactorAttribution {
                feature: feature.clone(),
                direction,
                train_auc_lift,
                val_auc_lift,
                test_auc_lift,
                importance,
                stability,
                train_corr: pearson_corr(train, feature_idx),
                val_corr: pearson_corr(val, feature_idx),
                test_corr: pearson_corr(test, feature_idx),
            }
        })
        .collect::<Vec<_>>();

    attributions.sort_by(|left, right| {
        right
            .importance
            .partial_cmp(&left.importance)
            .unwrap_or(Ordering::Equal)
            .then_with(|| {
                right
                    .stability
                    .partial_cmp(&left.stability)
                    .unwrap_or(Ordering::Equal)
            })
    });
    attributions
}

fn signed_auc_lift(split: &SplitDataset, feature_idx: usize) -> f64 {
    let scored = split
        .samples
        .iter()
        .map(|sample| (sample.x[feature_idx], sample.y))
        .collect::<Vec<_>>();
    let auc = auc_pairwise(&scored);
    if auc.is_finite() {
        auc - 0.5
    } else {
        f64::NAN
    }
}

fn stability_score(train: f64, val: f64, test: f64) -> f64 {
    if !train.is_finite() || !val.is_finite() || !test.is_finite() {
        return 0.0;
    }
    let sign = train.signum();
    if ![val, test]
        .iter()
        .all(|score| score.signum() == sign && score.abs() > 0.0)
    {
        return 0.0;
    }
    let magnitude = val.abs().min(test.abs()) / train.abs().max(1e-9);
    magnitude.min(1.0)
}

fn pearson_corr(split: &SplitDataset, feature_idx: usize) -> f64 {
    if split.samples.len() < 2 {
        return f64::NAN;
    }

    let n = split.samples.len() as f64;
    let x_mean = split
        .samples
        .iter()
        .map(|sample| sample.x[feature_idx])
        .sum::<f64>()
        / n;
    let y_mean = split.samples.iter().map(|sample| sample.y).sum::<f64>() / n;
    let mut covariance = 0.0;
    let mut x_variance = 0.0;
    let mut y_variance = 0.0;
    for sample in &split.samples {
        let x_delta = sample.x[feature_idx] - x_mean;
        let y_delta = sample.y - y_mean;
        covariance += x_delta * y_delta;
        x_variance += x_delta * x_delta;
        y_variance += y_delta * y_delta;
    }

    let denom = (x_variance * y_variance).sqrt();
    if denom > 1e-12 {
        covariance / denom
    } else {
        f64::NAN
    }
}

fn auc_pairwise(scored: &[(f64, f64)]) -> f64 {
    let positives = scored
        .iter()
        .filter(|(_, label)| *label > 0.5)
        .map(|(score, _)| *score)
        .collect::<Vec<_>>();
    let negatives = scored
        .iter()
        .filter(|(_, label)| *label <= 0.5)
        .map(|(score, _)| *score)
        .collect::<Vec<_>>();

    if positives.is_empty() || negatives.is_empty() {
        return f64::NAN;
    }

    let mut wins = 0.0;
    for positive in &positives {
        for negative in &negatives {
            wins += match positive.partial_cmp(negative).unwrap_or(Ordering::Equal) {
                Ordering::Greater => 1.0,
                Ordering::Equal => 0.5,
                Ordering::Less => 0.0,
            };
        }
    }

    wins / (positives.len() * negatives.len()) as f64
}

fn print_report(
    manifest: &DatasetBuildManifest,
    config: &Config,
    splits: [&SplitDataset; 3],
    attributions: &[FactorAttribution],
    registry: &FactorRegistry,
    regime: Regime,
) {
    eprintln!("=== Event Factor Attribution ===");
    eprintln!("dataset={}", config.dataset_root.display());
    eprintln!(
        "source_window={} -> {} symbols={:?}",
        manifest.source_window.start_time,
        manifest.source_window.end_time,
        manifest.source_window.symbols
    );
    eprintln!(
        "entry_secs={} tolerance_secs={} regime={:?} features={}",
        config.entry_secs,
        config.tolerance_secs,
        regime,
        config.features.len()
    );
    eprintln!(
        "selected_events train={} val={} test={}",
        format_split_count(splits[0]),
        format_split_count(splits[1]),
        format_split_count(splits[2])
    );

    eprintln!();
    eprintln!(
        "{:<28} {:>3} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9}",
        "feature", "dir", "score", "stab", "tr_auc", "val_auc", "te_auc", "tr_r", "val_r", "test_r"
    );
    for item in attributions.iter().take(config.top_n) {
        eprintln!(
            "{:<28} {:>3} {:>9.4} {:>9.4} {:>9.4} {:>9.4} {:>9.4} {:>9.4} {:>9.4} {:>9.4}",
            item.feature,
            if item.direction >= 0 { "+" } else { "-" },
            item.importance,
            item.stability,
            item.train_auc_lift,
            item.val_auc_lift,
            item.test_auc_lift,
            item.train_corr,
            item.val_corr,
            item.test_corr
        );
    }

    let registered = registry.top_n(regime, "settlement_up", config.top_n);
    eprintln!();
    eprintln!(
        "registered_automl_factors={} top_registry_entries={}",
        registry.all().len(),
        registered.len()
    );
    for meta in registered {
        eprintln!(
            "{:<34} ic={:>8.4} dir={} stability={:>8.4}",
            meta.name,
            meta.ic,
            if meta.direction >= 0 { "+" } else { "-" },
            meta.stability
        );
    }
    eprintln!(
        "\nNote: this is AutoML-style factor attribution, not hyperparameter search. Ranking uses event-held-out validation AUC lift for importance and train direction for registry direction; use it to prioritize factors, not to claim live edge."
    );
}

fn format_split_count(split: &SplitDataset) -> String {
    format!("{}={}", split.name, split.samples.len())
}

fn write_artifacts(
    output_dir: &Path,
    config: &Config,
    splits: [&SplitDataset; 3],
    attributions: &[FactorAttribution],
    registry: &FactorRegistry,
    regime: Regime,
) -> Result<()> {
    fs::create_dir_all(output_dir)
        .with_context(|| format!("create output dir {}", output_dir.display()))?;

    let whitelist = governed_feature_whitelist(attributions, config);
    let artifact = AttributionArtifact {
        dataset: config.dataset_root.display().to_string(),
        entry_secs: config.entry_secs,
        tolerance_secs: config.tolerance_secs,
        regime: format!("{regime:?}"),
        features_considered: config.features.len(),
        selected_events: splits
            .iter()
            .map(|split| SplitCount {
                split: split.name,
                selected_events: split.samples.len(),
            })
            .collect(),
        whitelist_min_importance: config.whitelist_min_importance,
        whitelist_min_stability: config.whitelist_min_stability,
        whitelist: whitelist.iter().map(|item| item.feature.as_str()).collect(),
        attributions: attributions.iter().map(attribution_row).collect(),
    };

    let json_path = output_dir.join("factor_attributions.json");
    let json_file =
        File::create(&json_path).with_context(|| format!("create {}", json_path.display()))?;
    serde_json::to_writer_pretty(json_file, &artifact)
        .with_context(|| format!("write {}", json_path.display()))?;

    let registry_artifact =
        build_factor_registry_artifact(config, splits, attributions, registry, regime, &whitelist);
    let registry_json_path = output_dir.join("event_ml_factor_registry.json");
    let registry_json_file = File::create(&registry_json_path)
        .with_context(|| format!("create {}", registry_json_path.display()))?;
    serde_json::to_writer_pretty(registry_json_file, &registry_artifact)
        .with_context(|| format!("write {}", registry_json_path.display()))?;

    let whitelist_path = output_dir.join("feature_whitelist.txt");
    let mut whitelist_file = File::create(&whitelist_path)
        .with_context(|| format!("create {}", whitelist_path.display()))?;
    for item in &whitelist {
        writeln!(whitelist_file, "{}", item.feature)
            .with_context(|| format!("write {}", whitelist_path.display()))?;
    }

    let markdown_path = output_dir.join("feature_whitelist.md");
    let mut markdown_file = File::create(&markdown_path)
        .with_context(|| format!("create {}", markdown_path.display()))?;
    writeln!(markdown_file, "# Event Feature Whitelist")?;
    writeln!(markdown_file)?;
    writeln!(
        markdown_file,
        "- dataset: `{}`",
        config.dataset_root.display()
    )?;
    writeln!(markdown_file, "- entry_secs: `{}`", config.entry_secs)?;
    writeln!(
        markdown_file,
        "- tolerance_secs: `{}`",
        config.tolerance_secs
    )?;
    writeln!(
        markdown_file,
        "- rule: importance >= `{}` and stability > `{}`",
        config.whitelist_min_importance, config.whitelist_min_stability
    )?;
    writeln!(markdown_file)?;
    writeln!(
        markdown_file,
        "| feature | dir | importance | stability | train_auc | val_auc | test_auc |"
    )?;
    writeln!(
        markdown_file,
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: |"
    )?;
    for item in &whitelist {
        writeln!(
            markdown_file,
            "| `{}` | {} | {:.4} | {:.4} | {:.4} | {:.4} | {:.4} |",
            item.feature,
            if item.direction >= 0 { "+" } else { "-" },
            item.importance,
            item.stability,
            item.train_auc_lift,
            item.val_auc_lift,
            item.test_auc_lift
        )?;
    }

    let registry_markdown_path = output_dir.join("event_ml_factor_registry.md");
    let mut registry_markdown_file = File::create(&registry_markdown_path)
        .with_context(|| format!("create {}", registry_markdown_path.display()))?;
    writeln!(registry_markdown_file, "# Event ML Factor Registry")?;
    writeln!(registry_markdown_file)?;
    writeln!(
        registry_markdown_file,
        "- dataset: `{}`",
        config.dataset_root.display()
    )?;
    writeln!(
        registry_markdown_file,
        "- target_label: `{}`",
        ATTRIBUTION_TARGET_LABEL
    )?;
    writeln!(registry_markdown_file, "- regime: `{:?}`", regime)?;
    writeln!(registry_markdown_file)?;
    writeln!(
        registry_markdown_file,
        "| factor | source_feature | dir | score | importance | stability | status | blockers |"
    )?;
    writeln!(
        registry_markdown_file,
        "| --- | --- | ---: | ---: | ---: | ---: | --- | --- |"
    )?;
    for row in &registry_artifact.factors {
        writeln!(
            registry_markdown_file,
            "| `{}` | `{}` | {} | {} | {} | {} | `{}` | {} |",
            row.factor_name,
            row.source_feature,
            if row.direction >= 0 { "+" } else { "-" },
            fmt_optional(row.registry_score),
            fmt_optional(row.importance),
            fmt_optional(row.stability),
            row.status,
            if row.blockers.is_empty() {
                "-".to_string()
            } else {
                row.blockers.join(", ")
            }
        )?;
    }

    eprintln!(
        "artifacts_dir={} whitelist_features={}",
        output_dir.display(),
        whitelist.len()
    );
    eprintln!("artifact_factor_attributions={}", json_path.display());
    eprintln!(
        "artifact_event_ml_factor_registry={}",
        registry_json_path.display()
    );
    eprintln!("artifact_feature_whitelist={}", whitelist_path.display());
    Ok(())
}

fn build_factor_registry_artifact(
    config: &Config,
    splits: [&SplitDataset; 3],
    attributions: &[FactorAttribution],
    registry: &FactorRegistry,
    regime: Regime,
    whitelist: &[&FactorAttribution],
) -> EventMlFactorRegistryArtifact {
    let attribution_by_feature = attributions
        .iter()
        .map(|item| (item.feature.as_str(), item))
        .collect::<BTreeMap<_, _>>();
    let whitelist_features = whitelist
        .iter()
        .map(|item| item.feature.clone())
        .collect::<BTreeSet<_>>();
    let factors = registry
        .all()
        .iter()
        .filter(|meta| meta.regime == regime && meta.label == ATTRIBUTION_TARGET_LABEL)
        .map(|meta| {
            let source_feature = meta
                .name
                .strip_prefix("automl:")
                .unwrap_or(meta.name.as_str());
            let attribution = attribution_by_feature.get(source_feature).copied();
            let whitelist_included = whitelist_features.contains(source_feature);
            EventMlFactorRegistryRow {
                factor_name: meta.name.clone(),
                source_feature: source_feature.to_string(),
                target_label: meta.label.clone(),
                regime: format!("{:?}", meta.regime),
                direction: meta.direction,
                train_derived_direction: meta.direction,
                registry_score: finite(meta.ic),
                importance: attribution.and_then(|item| finite(item.importance)),
                stability: attribution.and_then(|item| finite(item.stability)),
                train_auc_lift: attribution.and_then(|item| finite(item.train_auc_lift)),
                val_auc_lift: attribution.and_then(|item| finite(item.val_auc_lift)),
                test_auc_lift: attribution.and_then(|item| finite(item.test_auc_lift)),
                train_corr: attribution.and_then(|item| finite(item.train_corr)),
                val_corr: attribution.and_then(|item| finite(item.val_corr)),
                test_corr: attribution.and_then(|item| finite(item.test_corr)),
                whitelist_included,
                status: if whitelist_included {
                    "governed_feature"
                } else {
                    "report_only"
                },
                blockers: if whitelist_included {
                    Vec::new()
                } else {
                    vec!["not_in_governed_whitelist"]
                },
            }
        })
        .collect();

    EventMlFactorRegistryArtifact {
        kind: FACTOR_REGISTRY_ARTIFACT_KIND,
        version: FACTOR_REGISTRY_ARTIFACT_VERSION,
        dataset: config.dataset_root.display().to_string(),
        entry_secs: config.entry_secs,
        tolerance_secs: config.tolerance_secs,
        regime: format!("{regime:?}"),
        target_label: ATTRIBUTION_TARGET_LABEL,
        selected_events: splits
            .iter()
            .map(|split| SplitCount {
                split: split.name,
                selected_events: split.samples.len(),
            })
            .collect(),
        whitelist_features: whitelist_features.into_iter().collect(),
        factors,
    }
}

fn governed_feature_whitelist<'a>(
    attributions: &'a [FactorAttribution],
    config: &Config,
) -> Vec<&'a FactorAttribution> {
    attributions
        .iter()
        .filter(|item| {
            item.importance.is_finite()
                && item.stability.is_finite()
                && item.importance >= config.whitelist_min_importance
                && item.stability > config.whitelist_min_stability
        })
        .take(config.whitelist_max_features)
        .collect()
}

fn fmt_optional(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.4}"))
        .unwrap_or_else(|| "-".to_string())
}

fn attribution_row(item: &FactorAttribution) -> AttributionRow<'_> {
    AttributionRow {
        feature: &item.feature,
        direction: item.direction,
        importance: finite(item.importance),
        stability: finite(item.stability),
        train_auc_lift: finite(item.train_auc_lift),
        val_auc_lift: finite(item.val_auc_lift),
        test_auc_lift: finite(item.test_auc_lift),
        train_corr: finite(item.train_corr),
        val_corr: finite(item.val_corr),
        test_corr: finite(item.test_corr),
    }
}

fn finite(value: f64) -> Option<f64> {
    value.is_finite().then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(samples: &[(f64, &[f64])]) -> SplitDataset {
        SplitDataset {
            name: "test",
            samples: samples
                .iter()
                .map(|(y, x)| Sample {
                    y: *y,
                    x: x.to_vec(),
                })
                .collect(),
        }
    }

    #[test]
    fn auc_pairwise_handles_ties() {
        let scored = [(0.8, 1.0), (0.5, 1.0), (0.5, 0.0), (0.2, 0.0)];
        let auc = auc_pairwise(&scored);

        assert!((auc - 0.875).abs() < 1e-9);
    }

    #[test]
    fn attribution_ranks_validation_signal() {
        let train = split(&[
            (1.0, &[2.0, 0.0]),
            (1.0, &[1.0, 0.0]),
            (0.0, &[0.0, 1.0]),
            (0.0, &[-1.0, 1.0]),
        ]);
        let val = split(&[(1.0, &[3.0, 0.0]), (0.0, &[-2.0, 1.0])]);
        let test = split(&[(1.0, &[4.0, 0.0]), (0.0, &[-3.0, 1.0])]);
        let features = vec!["signal".to_string(), "noise".to_string()];

        let attributions = attribute_features(&features, &train, &val, &test);

        assert_eq!(attributions[0].feature, "signal");
        assert!(attributions[0].importance > 0.0);
        assert!(attributions[0].stability > 0.0);
    }

    #[test]
    fn stability_requires_oos_direction_alignment() {
        assert_eq!(stability_score(0.2, -0.1, 0.1), 0.0);
        assert!(stability_score(0.2, 0.1, 0.1) > 0.0);
    }

    #[test]
    fn parse_string_list_omits_empty_items() {
        assert_eq!(parse_string_list("a,, b, "), vec!["a", "b"]);
    }

    #[test]
    fn governed_whitelist_requires_stable_nonzero_signal() {
        let config = Config {
            dataset_root: PathBuf::from("/tmp/events"),
            entry_secs: 60,
            tolerance_secs: 30,
            top_n: 12,
            output_dir: None,
            whitelist_min_importance: 0.05,
            whitelist_min_stability: 0.0,
            whitelist_max_features: 10,
            features: vec![],
        };
        let attributions = vec![
            FactorAttribution {
                feature: "stable".to_string(),
                direction: 1,
                train_auc_lift: 0.2,
                val_auc_lift: 0.1,
                test_auc_lift: 0.1,
                importance: 0.1,
                stability: 0.5,
                train_corr: 0.0,
                val_corr: 0.0,
                test_corr: 0.0,
            },
            FactorAttribution {
                feature: "validation_only".to_string(),
                direction: 1,
                train_auc_lift: 0.2,
                val_auc_lift: 0.2,
                test_auc_lift: -0.2,
                importance: 0.2,
                stability: 0.0,
                train_corr: 0.0,
                val_corr: 0.0,
                test_corr: 0.0,
            },
        ];

        let whitelist = governed_feature_whitelist(&attributions, &config);

        assert_eq!(whitelist.len(), 1);
        assert_eq!(whitelist[0].feature, "stable");
    }

    #[test]
    fn factor_registry_artifact_preserves_governed_status_and_direction() {
        let config = Config {
            dataset_root: PathBuf::from("/tmp/events"),
            entry_secs: 60,
            tolerance_secs: 30,
            top_n: 12,
            output_dir: None,
            whitelist_min_importance: 0.05,
            whitelist_min_stability: 0.0,
            whitelist_max_features: 10,
            features: vec![],
        };
        let attributions = vec![
            FactorAttribution {
                feature: "stable".to_string(),
                direction: -1,
                train_auc_lift: -0.2,
                val_auc_lift: -0.1,
                test_auc_lift: -0.1,
                importance: 0.1,
                stability: 0.5,
                train_corr: -0.3,
                val_corr: -0.2,
                test_corr: -0.1,
            },
            FactorAttribution {
                feature: "thin".to_string(),
                direction: 1,
                train_auc_lift: 0.2,
                val_auc_lift: 0.2,
                test_auc_lift: -0.2,
                importance: 0.2,
                stability: 0.0,
                train_corr: 0.3,
                val_corr: 0.2,
                test_corr: -0.1,
            },
        ];
        let regime = Regime::from_secs(config.entry_secs);
        let mut registry = FactorRegistry::new();
        let registry_items = attributions
            .iter()
            .map(|item| AutomlFactorAttribution {
                name: item.feature.clone(),
                importance: item.importance,
                direction: item.direction,
                stability: item.stability,
            })
            .collect::<Vec<_>>();
        register_automl_attributions(
            &mut registry,
            regime,
            ATTRIBUTION_TARGET_LABEL,
            &registry_items,
        );
        let whitelist = governed_feature_whitelist(&attributions, &config);

        let artifact = build_factor_registry_artifact(
            &config,
            [&split(&[]), &split(&[]), &split(&[])],
            &attributions,
            &registry,
            regime,
            &whitelist,
        );

        assert_eq!(artifact.kind, FACTOR_REGISTRY_ARTIFACT_KIND);
        assert_eq!(artifact.target_label, ATTRIBUTION_TARGET_LABEL);
        assert_eq!(artifact.factors.len(), 2);
        let stable = artifact
            .factors
            .iter()
            .find(|row| row.source_feature == "stable")
            .expect("stable registry row");
        assert_eq!(stable.factor_name, "automl:stable");
        assert_eq!(stable.train_derived_direction, -1);
        assert!(stable.whitelist_included);
        assert_eq!(stable.status, "governed_feature");
        assert!(stable.blockers.is_empty());

        let thin = artifact
            .factors
            .iter()
            .find(|row| row.source_feature == "thin")
            .expect("thin registry row");
        assert!(!thin.whitelist_included);
        assert_eq!(thin.status, "report_only");
        assert_eq!(thin.blockers, vec!["not_in_governed_whitelist"]);
    }
}
