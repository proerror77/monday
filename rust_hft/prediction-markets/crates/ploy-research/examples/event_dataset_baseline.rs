use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use ploy_research::DatasetBuildManifest;
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
const BASELINE_MODEL_ARTIFACT_KIND: &str = "event_ml_logistic_baseline_model";
const BASELINE_MODEL_ARTIFACT_VERSION: u32 = 1;
const BASELINE_TARGET_LABEL: &str = "settlement_up";

#[derive(Debug, Clone)]
struct Config {
    dataset_root: PathBuf,
    entry_secs: i64,
    tolerance_secs: i64,
    min_edge: f64,
    epochs: usize,
    learning_rate: f64,
    l2: f64,
    output_json: Option<PathBuf>,
    features: Vec<String>,
}

#[derive(Debug, Clone)]
struct Sample {
    event_id: String,
    y: f64,
    x: Vec<f64>,
    pm_up_ask: f64,
    pm_down_ask: f64,
    time_remaining_secs: i64,
}

#[derive(Debug, Clone)]
struct SplitDataset {
    name: &'static str,
    source_rows: usize,
    samples: Vec<Sample>,
}

#[derive(Debug, Clone)]
struct Standardizer {
    means: Vec<f64>,
    stds: Vec<f64>,
}

#[derive(Debug, Clone)]
struct LogisticModel {
    intercept: f64,
    weights: Vec<f64>,
    standardizer: Standardizer,
}

#[derive(Debug, Clone)]
struct Metrics {
    split: &'static str,
    source_rows: usize,
    samples: usize,
    positives: usize,
    avg_time_remaining_secs: f64,
    accuracy: f64,
    logloss: f64,
    brier: f64,
    auc: f64,
    avg_probability: f64,
    trades: usize,
    wins: usize,
    pnl: f64,
    cost: f64,
    avg_entry: f64,
}

#[derive(Debug, Serialize)]
struct BaselineArtifact<'a> {
    dataset: String,
    entry_secs: i64,
    tolerance_secs: i64,
    min_edge: f64,
    epochs: usize,
    learning_rate: f64,
    l2: f64,
    features: &'a [String],
    model: BaselineModelArtifact<'a>,
    metrics: Vec<MetricArtifact>,
    top_weights: Vec<ModelWeight<'a>>,
}

#[derive(Debug, Serialize)]
struct BaselineModelArtifact<'a> {
    kind: &'static str,
    version: u32,
    family: &'static str,
    target_label: &'static str,
    feature_schema: &'a [String],
    intercept: f64,
    weights: Vec<ModelWeight<'a>>,
    standardizer: StandardizerArtifact<'a>,
}

#[derive(Debug, Serialize)]
struct StandardizerArtifact<'a> {
    method: &'static str,
    fit_split: &'static str,
    features: Vec<FeatureStandardizer<'a>>,
}

#[derive(Debug, Serialize)]
struct FeatureStandardizer<'a> {
    feature: &'a str,
    mean: f64,
    std: f64,
}

#[derive(Debug, Serialize)]
struct MetricArtifact {
    split: &'static str,
    source_rows: usize,
    samples: usize,
    positives: usize,
    avg_time_remaining_secs: f64,
    accuracy: f64,
    logloss: f64,
    brier: f64,
    auc: Option<f64>,
    avg_probability: f64,
    trades: usize,
    wins: usize,
    pnl: f64,
    cost: f64,
    roi: f64,
    avg_entry: f64,
}

#[derive(Debug, Serialize)]
struct ModelWeight<'a> {
    feature: &'a str,
    weight: f64,
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

    ensure_disjoint_events(&[&train, &val, &test])?;

    let model = fit_logistic(
        &train.samples,
        config.features.len(),
        config.epochs,
        config.learning_rate,
        config.l2,
    )?;

    let train_metrics = evaluate(&model, &train, config.min_edge);
    let val_metrics = evaluate(&model, &val, config.min_edge);
    let test_metrics = evaluate(&model, &test, config.min_edge);

    print_report(
        &manifest,
        &config,
        [&train_metrics, &val_metrics, &test_metrics],
        &model,
    );
    if let Some(path) = &config.output_json {
        write_artifact(
            path,
            &config,
            [&train_metrics, &val_metrics, &test_metrics],
            &model,
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
    let min_edge = parse_f64_flag(&args, "--min-edge", 0.0)?;
    let epochs = parse_usize_flag(&args, "--epochs", 500)?;
    let learning_rate = parse_f64_flag(&args, "--learning-rate", 0.05)?;
    let l2 = parse_f64_flag(&args, "--l2", 1e-3)?;
    let output_json = flag_value(&args, "--output-json").map(PathBuf::from);
    let features = flag_value(&args, "--features")
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
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
    if !(0.0..1.0).contains(&learning_rate) {
        bail!("--learning-rate must be in [0, 1)");
    }
    if l2 < 0.0 {
        bail!("--l2 must be non-negative");
    }

    Ok(Config {
        dataset_root,
        entry_secs,
        tolerance_secs,
        min_edge,
        epochs,
        learning_rate,
        l2,
        output_json,
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

        let sample = Sample {
            event_id: event_id.to_string(),
            y,
            x,
            pm_up_ask: up_ask,
            pm_down_ask: down_ask,
            time_remaining_secs: seconds,
        };

        match by_event.get(event_id) {
            Some((current_distance, _)) if *current_distance <= distance => {}
            _ => {
                by_event.insert(event_id.to_string(), (distance, sample));
            }
        }
    }

    Ok(SplitDataset {
        name,
        source_rows,
        samples: by_event.into_values().map(|(_, sample)| sample).collect(),
    })
}

fn is_binary_label(value: f64) -> bool {
    value.is_finite() && (value == 0.0 || value == 1.0)
}

fn valid_entry_price(value: f64) -> bool {
    value.is_finite() && value > 0.0 && value < 1.0
}

fn ensure_disjoint_events(splits: &[&SplitDataset]) -> Result<()> {
    let mut seen = HashSet::new();
    for split in splits {
        for sample in &split.samples {
            if !seen.insert(sample.event_id.clone()) {
                bail!(
                    "event_id {} appears in more than one selected split",
                    sample.event_id
                );
            }
        }
    }
    Ok(())
}

fn fit_logistic(
    samples: &[Sample],
    feature_count: usize,
    epochs: usize,
    learning_rate: f64,
    l2: f64,
) -> Result<LogisticModel> {
    if samples.len() < 20 {
        bail!(
            "training sample too small: {} selected events",
            samples.len()
        );
    }

    let standardizer = Standardizer::fit(samples, feature_count)?;
    let normalized = samples
        .iter()
        .map(|sample| standardizer.transform(&sample.x))
        .collect::<Vec<_>>();

    let mut intercept = 0.0;
    let mut weights = vec![0.0; feature_count];
    let n = samples.len() as f64;

    for _ in 0..epochs {
        let mut intercept_grad = 0.0;
        let mut weight_grads = vec![0.0; feature_count];
        for (sample, x) in samples.iter().zip(&normalized) {
            let z = intercept + dot(&weights, x);
            let error = sigmoid(z) - sample.y;
            intercept_grad += error;
            for (grad, value) in weight_grads.iter_mut().zip(x) {
                *grad += error * value;
            }
        }

        intercept -= learning_rate * intercept_grad / n;
        for (weight, grad) in weights.iter_mut().zip(weight_grads) {
            *weight -= learning_rate * (grad / n + l2 * *weight);
        }
    }

    Ok(LogisticModel {
        intercept,
        weights,
        standardizer,
    })
}

impl Standardizer {
    fn fit(samples: &[Sample], feature_count: usize) -> Result<Self> {
        let mut means = vec![0.0; feature_count];
        for sample in samples {
            if sample.x.len() != feature_count {
                bail!("sample feature length mismatch");
            }
            for (mean, value) in means.iter_mut().zip(&sample.x) {
                *mean += value;
            }
        }
        for mean in &mut means {
            *mean /= samples.len() as f64;
        }

        let mut variances = vec![0.0; feature_count];
        for sample in samples {
            for ((variance, value), mean) in variances.iter_mut().zip(&sample.x).zip(&means) {
                let delta = value - mean;
                *variance += delta * delta;
            }
        }

        let stds = variances
            .into_iter()
            .map(|variance| {
                let std = (variance / samples.len() as f64).sqrt();
                if std.is_finite() && std > 1e-12 {
                    std
                } else {
                    1.0
                }
            })
            .collect();

        Ok(Self { means, stds })
    }

    fn transform(&self, x: &[f64]) -> Vec<f64> {
        x.iter()
            .zip(&self.means)
            .zip(&self.stds)
            .map(|((value, mean), std)| (value - mean) / std)
            .collect()
    }
}

impl LogisticModel {
    fn predict_probability(&self, sample: &Sample) -> f64 {
        let x = self.standardizer.transform(&sample.x);
        sigmoid(self.intercept + dot(&self.weights, &x))
    }
}

fn evaluate(model: &LogisticModel, split: &SplitDataset, min_edge: f64) -> Metrics {
    let mut correct = 0usize;
    let mut positives = 0usize;
    let mut logloss = 0.0;
    let mut brier = 0.0;
    let mut probability_sum = 0.0;
    let mut time_remaining_sum = 0.0;
    let mut scored = Vec::with_capacity(split.samples.len());
    let mut trades = 0usize;
    let mut wins = 0usize;
    let mut pnl = 0.0;
    let mut cost = 0.0;

    for sample in &split.samples {
        let p = model.predict_probability(sample);
        let y_bool = sample.y > 0.5;
        let predicted = p >= 0.5;
        if predicted == y_bool {
            correct += 1;
        }
        if y_bool {
            positives += 1;
        }
        probability_sum += p;
        time_remaining_sum += sample.time_remaining_secs as f64;
        logloss += binary_logloss(p, sample.y);
        let error = p - sample.y;
        brier += error * error;
        scored.push((p, sample.y));

        if let Some(trade) = choose_trade(p, sample, min_edge) {
            trades += 1;
            if trade.won {
                wins += 1;
            }
            pnl += trade.pnl;
            cost += trade.entry_price;
        }
    }

    let n = split.samples.len().max(1) as f64;
    Metrics {
        split: split.name,
        source_rows: split.source_rows,
        samples: split.samples.len(),
        positives,
        avg_time_remaining_secs: time_remaining_sum / n,
        accuracy: correct as f64 / n,
        logloss: logloss / n,
        brier: brier / n,
        auc: auc_pairwise(&scored),
        avg_probability: probability_sum / n,
        trades,
        wins,
        pnl,
        cost,
        avg_entry: if trades > 0 {
            cost / trades as f64
        } else {
            0.0
        },
    }
}

#[derive(Debug, Clone, Copy)]
struct TradeOutcome {
    won: bool,
    entry_price: f64,
    pnl: f64,
}

fn choose_trade(p_up: f64, sample: &Sample, min_edge: f64) -> Option<TradeOutcome> {
    let up_edge = p_up - sample.pm_up_ask;
    let p_down = 1.0 - p_up;
    let down_edge = p_down - sample.pm_down_ask;

    if up_edge < min_edge && down_edge < min_edge {
        return None;
    }

    if up_edge >= down_edge {
        let won = sample.y > 0.5;
        let pnl = if won {
            1.0 - sample.pm_up_ask
        } else {
            -sample.pm_up_ask
        };
        Some(TradeOutcome {
            won,
            entry_price: sample.pm_up_ask,
            pnl,
        })
    } else {
        let won = sample.y <= 0.5;
        let pnl = if won {
            1.0 - sample.pm_down_ask
        } else {
            -sample.pm_down_ask
        };
        Some(TradeOutcome {
            won,
            entry_price: sample.pm_down_ask,
            pnl,
        })
    }
}

fn binary_logloss(probability: f64, label: f64) -> f64 {
    let p = probability.clamp(1e-9, 1.0 - 1e-9);
    -(label * p.ln() + (1.0 - label) * (1.0 - p).ln())
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

fn sigmoid(value: f64) -> f64 {
    if value >= 0.0 {
        1.0 / (1.0 + (-value).exp())
    } else {
        let exp = value.exp();
        exp / (1.0 + exp)
    }
}

fn dot(weights: &[f64], values: &[f64]) -> f64 {
    weights
        .iter()
        .zip(values)
        .map(|(weight, value)| weight * value)
        .sum()
}

fn print_report(
    manifest: &DatasetBuildManifest,
    config: &Config,
    metrics: [&Metrics; 3],
    model: &LogisticModel,
) {
    eprintln!("=== Event Dataset Baseline ===");
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
        "entry_secs={} tolerance_secs={} min_edge={} epochs={} learning_rate={} l2={} features={}",
        config.entry_secs,
        config.tolerance_secs,
        config.min_edge,
        config.epochs,
        config.learning_rate,
        config.l2,
        config.features.len()
    );
    eprintln!();
    eprintln!(
        "{:<6} {:>10} {:>7} {:>7} {:>7} {:>7} {:>8} {:>8} {:>8} {:>8} {:>7} {:>7} {:>10} {:>9} {:>9}",
        "split",
        "src_rows",
        "events",
        "avg_t",
        "pos%",
        "acc%",
        "logloss",
        "brier",
        "auc",
        "avg_p",
        "trades",
        "wr%",
        "pnl",
        "roi%",
        "avg_entry"
    );
    for metric in metrics {
        let pos_rate = if metric.samples > 0 {
            metric.positives as f64 / metric.samples as f64 * 100.0
        } else {
            0.0
        };
        let win_rate = if metric.trades > 0 {
            metric.wins as f64 / metric.trades as f64 * 100.0
        } else {
            0.0
        };
        let roi = if metric.cost > 0.0 {
            metric.pnl / metric.cost * 100.0
        } else {
            0.0
        };
        eprintln!(
            "{:<6} {:>10} {:>7} {:>7.1} {:>6.1}% {:>6.1}% {:>8.4} {:>8.4} {:>8.4} {:>8.4} {:>7} {:>6.1}% {:>10.4} {:>8.2}% {:>9.4}",
            metric.split,
            metric.source_rows,
            metric.samples,
            metric.avg_time_remaining_secs,
            pos_rate,
            metric.accuracy * 100.0,
            metric.logloss,
            metric.brier,
            metric.auc,
            metric.avg_probability,
            metric.trades,
            win_rate,
            metric.pnl,
            roi,
            metric.avg_entry
        );
    }
    eprintln!();
    eprintln!("Top weights by absolute value:");
    let mut weighted_features = config
        .features
        .iter()
        .zip(&model.weights)
        .map(|(feature, weight)| (feature.as_str(), *weight))
        .collect::<Vec<_>>();
    weighted_features.sort_by(|(_, left), (_, right)| {
        right
            .abs()
            .partial_cmp(&left.abs())
            .unwrap_or(Ordering::Equal)
    });
    for (feature, weight) in weighted_features.into_iter().take(12) {
        eprintln!("{feature:<28} {weight:>9.5}");
    }
    eprintln!(
        "\nNote: fixed-hyperparameter baseline; validation/test are event-held-out. PnL ignores fees, fill probability, latency, and slippage."
    );
}

fn write_artifact(
    path: &Path,
    config: &Config,
    metrics: [&Metrics; 3],
    model: &LogisticModel,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create output dir {}", parent.display()))?;
    }
    let artifact = BaselineArtifact {
        dataset: config.dataset_root.display().to_string(),
        entry_secs: config.entry_secs,
        tolerance_secs: config.tolerance_secs,
        min_edge: config.min_edge,
        epochs: config.epochs,
        learning_rate: config.learning_rate,
        l2: config.l2,
        features: &config.features,
        model: baseline_model_artifact(&config.features, model),
        metrics: metrics.into_iter().map(metric_artifact).collect(),
        top_weights: top_weights(&config.features, model, 12),
    };
    let file = File::create(path).with_context(|| format!("create {}", path.display()))?;
    serde_json::to_writer_pretty(file, &artifact)
        .with_context(|| format!("write {}", path.display()))?;
    eprintln!("artifact_baseline_metrics={}", path.display());
    Ok(())
}

fn baseline_model_artifact<'a>(
    features: &'a [String],
    model: &LogisticModel,
) -> BaselineModelArtifact<'a> {
    BaselineModelArtifact {
        kind: BASELINE_MODEL_ARTIFACT_KIND,
        version: BASELINE_MODEL_ARTIFACT_VERSION,
        family: "logistic_regression",
        target_label: BASELINE_TARGET_LABEL,
        feature_schema: features,
        intercept: model.intercept,
        weights: features
            .iter()
            .zip(&model.weights)
            .map(|(feature, weight)| ModelWeight {
                feature,
                weight: *weight,
            })
            .collect(),
        standardizer: StandardizerArtifact {
            method: "zscore",
            fit_split: "train",
            features: features
                .iter()
                .zip(&model.standardizer.means)
                .zip(&model.standardizer.stds)
                .map(|((feature, mean), std)| FeatureStandardizer {
                    feature,
                    mean: *mean,
                    std: *std,
                })
                .collect(),
        },
    }
}

fn metric_artifact(metric: &Metrics) -> MetricArtifact {
    MetricArtifact {
        split: metric.split,
        source_rows: metric.source_rows,
        samples: metric.samples,
        positives: metric.positives,
        avg_time_remaining_secs: metric.avg_time_remaining_secs,
        accuracy: metric.accuracy,
        logloss: metric.logloss,
        brier: metric.brier,
        auc: metric.auc.is_finite().then_some(metric.auc),
        avg_probability: metric.avg_probability,
        trades: metric.trades,
        wins: metric.wins,
        pnl: metric.pnl,
        cost: metric.cost,
        roi: if metric.cost > 0.0 {
            metric.pnl / metric.cost
        } else {
            0.0
        },
        avg_entry: metric.avg_entry,
    }
}

fn top_weights<'a>(
    features: &'a [String],
    model: &LogisticModel,
    limit: usize,
) -> Vec<ModelWeight<'a>> {
    let mut weighted_features = features
        .iter()
        .zip(&model.weights)
        .map(|(feature, weight)| (feature.as_str(), *weight))
        .collect::<Vec<_>>();
    weighted_features.sort_by(|(_, left), (_, right)| {
        right
            .abs()
            .partial_cmp(&left.abs())
            .unwrap_or(Ordering::Equal)
    });
    weighted_features
        .into_iter()
        .take(limit)
        .map(|(feature, weight)| ModelWeight { feature, weight })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(event_id: &str, y: f64, x: &[f64], up_ask: f64, down_ask: f64) -> Sample {
        Sample {
            event_id: event_id.to_string(),
            y,
            x: x.to_vec(),
            pm_up_ask: up_ask,
            pm_down_ask: down_ask,
            time_remaining_secs: 60,
        }
    }

    #[test]
    fn logistic_baseline_learns_simple_separable_signal() {
        let samples = (0..80)
            .map(|idx| {
                if idx % 2 == 0 {
                    sample(&format!("up-{idx}"), 1.0, &[1.0, 0.5], 0.4, 0.6)
                } else {
                    sample(&format!("down-{idx}"), 0.0, &[-1.0, -0.5], 0.6, 0.4)
                }
            })
            .collect::<Vec<_>>();

        let model = fit_logistic(&samples, 2, 200, 0.1, 1e-4).unwrap();
        let split = SplitDataset {
            name: "test",
            source_rows: samples.len(),
            samples,
        };
        let metrics = evaluate(&model, &split, 0.0);

        assert!(metrics.accuracy > 0.95);
        assert!(metrics.auc > 0.95);
        assert!(metrics.pnl > 0.0);
    }

    #[test]
    fn baseline_artifact_contains_full_runtime_model_contract() {
        let features = vec!["distance".to_string(), "edge".to_string()];
        let model = LogisticModel {
            intercept: 0.25,
            weights: vec![0.5, -0.75],
            standardizer: Standardizer {
                means: vec![1.0, 2.0],
                stds: vec![3.0, 4.0],
            },
        };

        let artifact = baseline_model_artifact(&features, &model);

        assert_eq!(artifact.kind, BASELINE_MODEL_ARTIFACT_KIND);
        assert_eq!(artifact.version, BASELINE_MODEL_ARTIFACT_VERSION);
        assert_eq!(artifact.family, "logistic_regression");
        assert_eq!(artifact.target_label, BASELINE_TARGET_LABEL);
        assert_eq!(artifact.feature_schema, &features);
        assert_eq!(artifact.intercept, 0.25);
        assert_eq!(artifact.weights.len(), 2);
        assert_eq!(artifact.weights[0].feature, "distance");
        assert_eq!(artifact.weights[0].weight, 0.5);
        assert_eq!(artifact.weights[1].feature, "edge");
        assert_eq!(artifact.weights[1].weight, -0.75);
        assert_eq!(artifact.standardizer.method, "zscore");
        assert_eq!(artifact.standardizer.fit_split, "train");
        assert_eq!(artifact.standardizer.features[0].feature, "distance");
        assert_eq!(artifact.standardizer.features[0].mean, 1.0);
        assert_eq!(artifact.standardizer.features[0].std, 3.0);
    }

    #[test]
    fn event_overlap_check_rejects_split_leakage() {
        let train = SplitDataset {
            name: "train",
            source_rows: 1,
            samples: vec![sample("evt-1", 1.0, &[1.0], 0.4, 0.6)],
        };
        let val = SplitDataset {
            name: "val",
            source_rows: 1,
            samples: vec![sample("evt-1", 0.0, &[-1.0], 0.6, 0.4)],
        };

        assert!(ensure_disjoint_events(&[&train, &val]).is_err());
    }

    #[test]
    fn auc_pairwise_handles_ties() {
        let scored = [(0.8, 1.0), (0.5, 1.0), (0.5, 0.0), (0.2, 0.0)];
        let auc = auc_pairwise(&scored);

        assert!((auc - 0.875).abs() < 1e-9);
    }

    #[test]
    fn metric_artifact_reports_roi_as_fraction() {
        let metric = Metrics {
            split: "test",
            source_rows: 1,
            samples: 1,
            positives: 1,
            avg_time_remaining_secs: 60.0,
            accuracy: 1.0,
            logloss: 0.1,
            brier: 0.1,
            auc: f64::NAN,
            avg_probability: 0.7,
            trades: 1,
            wins: 1,
            pnl: 0.25,
            cost: 0.5,
            avg_entry: 0.5,
        };

        let artifact = metric_artifact(&metric);

        assert_eq!(artifact.auc, None);
        assert_eq!(artifact.roi, 0.5);
    }
}
