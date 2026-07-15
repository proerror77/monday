use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::{create_dir_all, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, TimeZone, Utc};
use ploy_research::{
    assign_chronological_event_splits, event_index_to_frame, split_assignments_to_frame,
    standard_event_root_dataset_artifacts, DatasetBuildManifest, DatasetBuildStats,
    DatasetSkipCounts, DatasetSourceWindow, DatasetSplit, DatasetSplitAssignment,
    DatasetSplitCounts, DatasetSplitPolicy, EventIndexEntry,
};
use polars::io::parquet::read::ParquetReader;
use polars::io::parquet::write::ParquetWriter;
use polars::prelude::*;
use serde::Serialize;

#[derive(Debug, Clone)]
struct Config {
    dataset_root: PathBuf,
    output_root: PathBuf,
    window_events: usize,
    max_windows: Option<usize>,
    dry_run: bool,
}

#[derive(Debug, Clone)]
struct PlannedWindow {
    id: usize,
    events: Vec<EventIndexEntry>,
}

#[derive(Debug, Clone, Serialize)]
struct RollingDatasetReport {
    source_dataset: String,
    output_root: String,
    window_events: usize,
    minimum_window_events: usize,
    total_source_events: usize,
    exported_windows: usize,
    skipped_events: usize,
    dry_run: bool,
    datasets_file: Option<String>,
    windows: Vec<WindowReport>,
}

#[derive(Debug, Clone, Serialize)]
struct WindowReport {
    id: usize,
    dataset_dir: String,
    start_time: DateTime<Utc>,
    end_time: DateTime<Utc>,
    events: usize,
    observations: SplitCountReport,
    events_per_split: SplitCountReport,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
struct SplitCountReport {
    train: usize,
    val: usize,
    test: usize,
}

#[derive(Debug, Clone)]
struct WindowArtifacts {
    event_index: Vec<EventIndexEntry>,
    split_assignments: Vec<DatasetSplitAssignment>,
    observation_counts: SplitCountReport,
}

fn main() -> Result<()> {
    let config = parse_config()?;
    let report = split_event_root_dataset(&config)?;

    eprintln!("event_dataset_rolling_windows_status=completed");
    eprintln!("exported_windows={}", report.exported_windows);
    eprintln!("skipped_events={}", report.skipped_events);
    if !report.dry_run {
        eprintln!(
            "artifact_rolling_dataset_report={}",
            config
                .output_root
                .join("rolling_datasets_report.json")
                .display()
        );
        eprintln!(
            "artifact_rolling_dataset_report_md={}",
            config
                .output_root
                .join("rolling_datasets_report.md")
                .display()
        );
        eprintln!(
            "artifact_rolling_datasets={}",
            config.output_root.join("rolling_datasets.txt").display()
        );
    }

    Ok(())
}

fn split_event_root_dataset(config: &Config) -> Result<RollingDatasetReport> {
    let source_manifest = read_manifest(&config.dataset_root)?;
    source_manifest
        .validate_contract()
        .map_err(|message| anyhow::anyhow!(message))
        .context("dataset manifest contract validation failed")?;

    let mut source_events = read_event_index(&config.dataset_root, &source_manifest)?;
    source_events.sort_by(|lhs, rhs| {
        lhs.end_time
            .cmp(&rhs.end_time)
            .then_with(|| lhs.symbol.cmp(&rhs.symbol))
            .then_with(|| lhs.event_id.cmp(&rhs.event_id))
    });
    ensure_unique_event_ids(&source_events)?;

    let minimum_window_events = minimum_events_for_policy(&source_manifest.split_policy);
    if config.window_events < minimum_window_events {
        bail!(
            "--window-events {} is too small for the canonical split policy; minimum is {}",
            config.window_events,
            minimum_window_events
        );
    }

    let (planned_windows, skipped_events) = plan_rolling_windows(
        &source_events,
        config.window_events,
        minimum_window_events,
        config.max_windows,
    )?;

    let mut report = RollingDatasetReport {
        source_dataset: config.dataset_root.display().to_string(),
        output_root: config.output_root.display().to_string(),
        window_events: config.window_events,
        minimum_window_events,
        total_source_events: source_events.len(),
        exported_windows: planned_windows.len(),
        skipped_events,
        dry_run: config.dry_run,
        datasets_file: (!config.dry_run).then(|| {
            config
                .output_root
                .join("rolling_datasets.txt")
                .display()
                .to_string()
        }),
        windows: Vec::with_capacity(planned_windows.len()),
    };

    if config.dry_run {
        for planned in planned_windows {
            let artifacts = reassign_window_splits(&planned.events, &source_manifest.split_policy)?;
            report.windows.push(window_report(
                planned.id,
                &window_dataset_dir(&config.output_root, planned.id),
                &artifacts,
            )?);
        }
        print_markdown_report(&report);
        return Ok(report);
    }

    let observations = read_all_split_dataframes(
        &config.dataset_root,
        &source_manifest.artifacts.observations.train,
        &source_manifest.artifacts.observations.val,
        &source_manifest.artifacts.observations.test,
    )
    .context("read source observation split parquet artifacts")?;
    let event_summaries = read_all_split_dataframes(
        &config.dataset_root,
        &source_manifest.artifacts.event_summaries.train,
        &source_manifest.artifacts.event_summaries.val,
        &source_manifest.artifacts.event_summaries.test,
    )
    .context("read source event summary split parquet artifacts")?;

    create_dir_all(&config.output_root)
        .with_context(|| format!("create output root {}", config.output_root.display()))?;
    let mut dataset_dirs = Vec::with_capacity(planned_windows.len());

    for planned in planned_windows {
        let output_dir = window_dataset_dir(&config.output_root, planned.id);
        let artifacts = write_window_dataset(
            &source_manifest,
            planned.id,
            &planned.events,
            &observations,
            &event_summaries,
            &output_dir,
        )
        .with_context(|| format!("write rolling window {}", planned.id))?;

        report
            .windows
            .push(window_report(planned.id, &output_dir, &artifacts)?);
        dataset_dirs.push(output_dir);
    }

    write_report_files(&report, &dataset_dirs, &config.output_root)?;
    Ok(report)
}

fn parse_config() -> Result<Config> {
    let args: Vec<String> = std::env::args().collect();
    if has_flag(&args, "--help") || has_flag(&args, "-h") {
        print_help();
        std::process::exit(0);
    }

    let dataset_root = flag_value(&args, "--dataset")
        .or_else(|| flag_value(&args, "--dataset-root"))
        .map(PathBuf::from)
        .context("--dataset <event-root-dataset-dir> is required")?;
    let window_events = parse_usize_flag(&args, "--window-events", 150)?;
    let max_windows = optional_usize_flag(&args, "--max-windows")?;
    let output_root = flag_value(&args, "--output-root")
        .map(PathBuf::from)
        .unwrap_or_else(|| default_output_root(&dataset_root, window_events));
    let dry_run = has_flag(&args, "--dry-run");

    if window_events == 0 {
        bail!("--window-events must be greater than zero");
    }
    if matches!(max_windows, Some(0)) {
        bail!("--max-windows must be greater than zero when provided");
    }

    Ok(Config {
        dataset_root,
        output_root,
        window_events,
        max_windows,
        dry_run,
    })
}

fn plan_rolling_windows(
    events: &[EventIndexEntry],
    window_events: usize,
    minimum_window_events: usize,
    max_windows: Option<usize>,
) -> Result<(Vec<PlannedWindow>, usize)> {
    if events.len() < minimum_window_events {
        bail!(
            "source dataset has {} events, below minimum rolling window size {}",
            events.len(),
            minimum_window_events
        );
    }

    let mut windows = Vec::new();
    let mut skipped_events = 0usize;
    let mut offset = 0usize;

    while offset < events.len() {
        if max_windows.is_some_and(|limit| windows.len() >= limit) {
            skipped_events += events.len() - offset;
            break;
        }

        let end = (offset + window_events).min(events.len());
        let chunk = &events[offset..end];
        if chunk.len() < minimum_window_events {
            skipped_events += chunk.len();
            break;
        }

        windows.push(PlannedWindow {
            id: windows.len() + 1,
            events: chunk.to_vec(),
        });
        offset = end;
    }

    if windows.is_empty() {
        bail!("no rolling windows could be planned");
    }

    Ok((windows, skipped_events))
}

fn write_window_dataset(
    source_manifest: &DatasetBuildManifest,
    window_id: usize,
    source_events: &[EventIndexEntry],
    observations: &DataFrame,
    event_summaries: &DataFrame,
    output_dir: &Path,
) -> Result<WindowArtifacts> {
    let artifacts = reassign_window_splits(source_events, &source_manifest.split_policy)?;

    let train_ids = split_event_ids(&artifacts.split_assignments, DatasetSplit::Train);
    let val_ids = split_event_ids(&artifacts.split_assignments, DatasetSplit::Val);
    let test_ids = split_event_ids(&artifacts.split_assignments, DatasetSplit::Test);

    let obs_train = filter_by_event_ids(observations, &train_ids)?;
    let obs_val = filter_by_event_ids(observations, &val_ids)?;
    let obs_test = filter_by_event_ids(observations, &test_ids)?;
    let summaries_train = filter_by_event_ids(event_summaries, &train_ids)?;
    let summaries_val = filter_by_event_ids(event_summaries, &val_ids)?;
    let summaries_test = filter_by_event_ids(event_summaries, &test_ids)?;

    let mut manifest = source_manifest.clone();
    manifest.built_at = Utc::now();
    manifest.source_window = source_window_for(&artifacts.event_index)?;
    manifest.artifacts = standard_event_root_dataset_artifacts();
    manifest.stats = DatasetBuildStats {
        total_events: artifacts.event_index.len(),
        total_observations: obs_train.height() + obs_val.height() + obs_test.height(),
        events_per_split: split_counts_for_assignments(&artifacts.split_assignments),
        observations_per_split: DatasetSplitCounts {
            train: obs_train.height(),
            val: obs_val.height(),
            test: obs_test.height(),
        },
        skip_counts: DatasetSkipCounts::default(),
    };
    manifest
        .validate_contract()
        .map_err(|message| anyhow::anyhow!(message))
        .with_context(|| format!("window {window_id} manifest contract validation failed"))?;

    write_manifest_json(&manifest, output_dir)?;
    write_parquet(
        event_index_to_frame(&artifacts.event_index)?,
        &artifact_path(output_dir, &manifest.artifacts.event_index),
    )?;
    write_parquet(
        split_assignments_to_frame(&artifacts.split_assignments)?,
        &artifact_path(output_dir, &manifest.artifacts.split_assignments),
    )?;
    write_parquet(
        obs_train,
        &artifact_path(output_dir, &manifest.artifacts.observations.train),
    )?;
    write_parquet(
        obs_val,
        &artifact_path(output_dir, &manifest.artifacts.observations.val),
    )?;
    write_parquet(
        obs_test,
        &artifact_path(output_dir, &manifest.artifacts.observations.test),
    )?;
    write_parquet(
        summaries_train,
        &artifact_path(output_dir, &manifest.artifacts.event_summaries.train),
    )?;
    write_parquet(
        summaries_val,
        &artifact_path(output_dir, &manifest.artifacts.event_summaries.val),
    )?;
    write_parquet(
        summaries_test,
        &artifact_path(output_dir, &manifest.artifacts.event_summaries.test),
    )?;

    Ok(WindowArtifacts {
        event_index: artifacts.event_index,
        split_assignments: artifacts.split_assignments,
        observation_counts: SplitCountReport {
            train: manifest.stats.observations_per_split.train,
            val: manifest.stats.observations_per_split.val,
            test: manifest.stats.observations_per_split.test,
        },
    })
}

fn reassign_window_splits(
    source_events: &[EventIndexEntry],
    policy: &DatasetSplitPolicy,
) -> Result<WindowArtifacts> {
    let chronology: Vec<_> = source_events
        .iter()
        .map(EventIndexEntry::chronology_key)
        .collect();
    let assignments = assign_chronological_event_splits(&chronology, policy)
        .map_err(|error| anyhow::anyhow!("assign window splits: {error:?}"))?;
    let assignment_by_event_id: BTreeMap<_, _> = assignments
        .iter()
        .map(|assignment| (assignment.event_id.as_str(), assignment))
        .collect();

    let mut event_index = Vec::with_capacity(source_events.len());
    for event in source_events {
        let assignment = assignment_by_event_id
            .get(event.event_id.as_str())
            .with_context(|| format!("missing reassignment for event {}", event.event_id))?;
        let mut entry = event.clone();
        entry.split = assignment.split;
        entry.split_rank = assignment.split_rank;
        event_index.push(entry);
    }

    Ok(WindowArtifacts {
        event_index,
        split_assignments: assignments,
        observation_counts: SplitCountReport::default(),
    })
}

fn read_manifest(root: &Path) -> Result<DatasetBuildManifest> {
    let path = root.join("event_manifest.json");
    let file = File::open(&path).with_context(|| format!("open manifest {}", path.display()))?;
    serde_json::from_reader(file).with_context(|| format!("parse manifest {}", path.display()))
}

fn read_event_index(root: &Path, manifest: &DatasetBuildManifest) -> Result<Vec<EventIndexEntry>> {
    let path = artifact_path(root, &manifest.artifacts.event_index);
    let df = read_parquet(&path)?;
    let event_ids = df.column("event_id")?.str()?;
    let symbols = df.column("symbol")?.str()?;
    let start_times = df.column("start_time")?.i64()?;
    let end_times = df.column("end_time")?.i64()?;
    let observation_counts = df.column("observation_row_count")?.i64()?;
    let settlement_available = df.column("settlement_label_available")?.bool()?;
    let repricing_counts = df.column("repricing_label_row_count_30s")?.i64()?;
    let regime_versions = df.column("regime_version")?.str()?;

    let mut entries = Vec::with_capacity(df.height());
    for row_idx in 0..df.height() {
        let event_id = event_ids
            .get(row_idx)
            .with_context(|| format!("event_index row {row_idx} missing event_id"))?;
        let symbol = symbols
            .get(row_idx)
            .with_context(|| format!("event_index row {row_idx} missing symbol"))?;
        let start_time = millis_to_datetime(
            start_times
                .get(row_idx)
                .with_context(|| format!("event_index row {row_idx} missing start_time"))?,
        )?;
        let end_time = millis_to_datetime(
            end_times
                .get(row_idx)
                .with_context(|| format!("event_index row {row_idx} missing end_time"))?,
        )?;
        let observation_row_count = usize_from_i64(
            observation_counts.get(row_idx).with_context(|| {
                format!("event_index row {row_idx} missing observation_row_count")
            })?,
            "observation_row_count",
        )?;
        let repricing_label_row_count_30s = usize_from_i64(
            repricing_counts.get(row_idx).with_context(|| {
                format!("event_index row {row_idx} missing repricing_label_row_count_30s")
            })?,
            "repricing_label_row_count_30s",
        )?;
        let regime_version = regime_versions
            .get(row_idx)
            .with_context(|| format!("event_index row {row_idx} missing regime_version"))?;

        entries.push(EventIndexEntry {
            event_id: event_id.to_string(),
            symbol: symbol.to_string(),
            start_time,
            end_time,
            split: DatasetSplit::Train,
            split_rank: 0,
            observation_row_count,
            settlement_label_available: settlement_available.get(row_idx).unwrap_or(false),
            repricing_label_row_count_30s,
            regime_version: regime_version.to_string(),
        });
    }

    Ok(entries)
}

fn read_all_split_dataframes(root: &Path, train: &str, val: &str, test: &str) -> Result<DataFrame> {
    let mut frames = [
        artifact_path(root, train),
        artifact_path(root, val),
        artifact_path(root, test),
    ]
    .into_iter()
    .map(|path| read_parquet(&path))
    .collect::<Result<Vec<_>>>()?;

    let mut combined = frames
        .drain(..1)
        .next()
        .context("expected at least one split dataframe")?;
    for frame in frames {
        combined.vstack_mut(&frame)?;
    }
    Ok(combined)
}

fn read_parquet(path: &Path) -> Result<DataFrame> {
    let file = File::open(path).with_context(|| format!("open parquet {}", path.display()))?;
    ParquetReader::new(file)
        .finish()
        .with_context(|| format!("read parquet {}", path.display()))
}

fn filter_by_event_ids(df: &DataFrame, event_ids: &BTreeSet<String>) -> Result<DataFrame> {
    let event_id_column = df.column("event_id")?.str()?;
    let mask = BooleanChunked::from_iter_values(
        "event_id_filter".into(),
        (0..df.height()).map(|row_idx| {
            event_id_column
                .get(row_idx)
                .is_some_and(|event_id| event_ids.contains(event_id))
        }),
    );
    df.filter(&mask).context("filter dataframe by event_id")
}

fn write_manifest_json(manifest: &DatasetBuildManifest, output_root: &Path) -> Result<()> {
    let path = artifact_path(output_root, &manifest.artifacts.event_manifest);
    ensure_parent_dir(&path)?;
    let file =
        File::create(&path).with_context(|| format!("create manifest {}", path.display()))?;
    serde_json::to_writer_pretty(file, manifest)
        .with_context(|| format!("write manifest {}", path.display()))
}

fn write_parquet(mut df: DataFrame, path: &Path) -> Result<()> {
    ensure_parent_dir(path)?;
    let file = File::create(path).map_err(|error| PolarsError::IO {
        error: Arc::new(error),
        msg: None,
    })?;
    ParquetWriter::new(file).finish(&mut df)?;
    Ok(())
}

fn write_report_files(
    report: &RollingDatasetReport,
    dataset_dirs: &[PathBuf],
    output_root: &Path,
) -> Result<()> {
    create_dir_all(output_root)
        .with_context(|| format!("create output root {}", output_root.display()))?;
    let json_path = output_root.join("rolling_datasets_report.json");
    let markdown_path = output_root.join("rolling_datasets_report.md");
    let datasets_path = output_root.join("rolling_datasets.txt");

    serde_json::to_writer_pretty(
        File::create(&json_path)
            .with_context(|| format!("create report {}", json_path.display()))?,
        report,
    )
    .with_context(|| format!("write report {}", json_path.display()))?;

    let mut markdown = File::create(&markdown_path)
        .with_context(|| format!("create report {}", markdown_path.display()))?;
    markdown
        .write_all(report_markdown(report).as_bytes())
        .with_context(|| format!("write report {}", markdown_path.display()))?;

    let mut datasets = File::create(&datasets_path)
        .with_context(|| format!("create datasets list {}", datasets_path.display()))?;
    for dir in dataset_dirs {
        writeln!(datasets, "{}", dir.display())
            .with_context(|| format!("write datasets list {}", datasets_path.display()))?;
    }

    Ok(())
}

fn report_markdown(report: &RollingDatasetReport) -> String {
    let mut output = String::new();
    output.push_str("# Event Dataset Rolling Windows\n\n");
    output.push_str(&format!("- source_dataset: `{}`\n", report.source_dataset));
    output.push_str(&format!("- output_root: `{}`\n", report.output_root));
    output.push_str(&format!(
        "- source_events: `{}`\n",
        report.total_source_events
    ));
    output.push_str(&format!("- window_events: `{}`\n", report.window_events));
    output.push_str(&format!(
        "- minimum_window_events: `{}`\n",
        report.minimum_window_events
    ));
    output.push_str(&format!(
        "- exported_windows: `{}`\n",
        report.exported_windows
    ));
    output.push_str(&format!(
        "- skipped_events: `{}`\n\n",
        report.skipped_events
    ));
    output.push_str("| window | events | train | val | test | observations | dataset |\n");
    output.push_str("| --- | ---: | ---: | ---: | ---: | ---: | --- |\n");
    for window in &report.windows {
        let observations =
            window.observations.train + window.observations.val + window.observations.test;
        output.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | `{}` |\n",
            window.id,
            window.events,
            window.events_per_split.train,
            window.events_per_split.val,
            window.events_per_split.test,
            observations,
            window.dataset_dir
        ));
    }
    output
}

fn print_markdown_report(report: &RollingDatasetReport) {
    eprintln!("{}", report_markdown(report));
}

fn window_report(
    id: usize,
    output_dir: &Path,
    artifacts: &WindowArtifacts,
) -> Result<WindowReport> {
    let source_window = source_window_for(&artifacts.event_index)?;
    Ok(WindowReport {
        id,
        dataset_dir: output_dir.display().to_string(),
        start_time: source_window.start_time,
        end_time: source_window.end_time,
        events: artifacts.event_index.len(),
        observations: artifacts.observation_counts,
        events_per_split: SplitCountReport::from(split_counts_for_assignments(
            &artifacts.split_assignments,
        )),
    })
}

fn source_window_for(events: &[EventIndexEntry]) -> Result<DatasetSourceWindow> {
    let first = events.first().context("window has no events")?;
    let last = events.last().context("window has no events")?;
    let mut symbols: BTreeSet<String> = BTreeSet::new();
    for event in events {
        symbols.insert(event.symbol.clone());
    }
    Ok(DatasetSourceWindow {
        start_time: first.start_time,
        end_time: last.end_time,
        symbols: symbols.into_iter().collect(),
    })
}

fn split_event_ids(
    assignments: &[DatasetSplitAssignment],
    split: DatasetSplit,
) -> BTreeSet<String> {
    assignments
        .iter()
        .filter(|assignment| assignment.split == split)
        .map(|assignment| assignment.event_id.clone())
        .collect()
}

fn split_counts_for_assignments(assignments: &[DatasetSplitAssignment]) -> DatasetSplitCounts {
    let mut counts = DatasetSplitCounts::default();
    for assignment in assignments {
        increment_split_count(&mut counts, assignment.split);
    }
    counts
}

fn increment_split_count(counts: &mut DatasetSplitCounts, split: DatasetSplit) {
    match split {
        DatasetSplit::Train => counts.train += 1,
        DatasetSplit::Val => counts.val += 1,
        DatasetSplit::Test => counts.test += 1,
    }
}

impl From<DatasetSplitCounts> for SplitCountReport {
    fn from(value: DatasetSplitCounts) -> Self {
        Self {
            train: value.train,
            val: value.val,
            test: value.test,
        }
    }
}

fn minimum_events_for_policy(policy: &DatasetSplitPolicy) -> usize {
    let mut events = policy.min_unique_events.max(1);
    loop {
        let train_count = events * usize::from(policy.train_percent) / 100;
        let val_count = events * usize::from(policy.val_percent) / 100;
        let test_count = events.saturating_sub(train_count + val_count);
        if val_count >= policy.min_eval_events && test_count >= policy.min_eval_events {
            return events;
        }
        events += 1;
    }
}

fn ensure_unique_event_ids(events: &[EventIndexEntry]) -> Result<()> {
    let mut seen = HashSet::with_capacity(events.len());
    for event in events {
        if !seen.insert(event.event_id.as_str()) {
            bail!("duplicate event_id in event_index: {}", event.event_id);
        }
    }
    Ok(())
}

fn window_dataset_dir(output_root: &Path, window_id: usize) -> PathBuf {
    output_root.join(format!("event_root_window_{window_id:03}"))
}

fn artifact_path(root: &Path, artifact: &str) -> PathBuf {
    let path = Path::new(artifact);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn ensure_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        create_dir_all(parent).with_context(|| format!("create directory {}", parent.display()))?;
    }
    Ok(())
}

fn millis_to_datetime(millis: i64) -> Result<DateTime<Utc>> {
    Utc.timestamp_millis_opt(millis)
        .single()
        .with_context(|| format!("invalid UTC timestamp millis {millis}"))
}

fn usize_from_i64(value: i64, field: &str) -> Result<usize> {
    usize::try_from(value).with_context(|| format!("{field} must be non-negative, got {value}"))
}

fn default_output_root(dataset_root: &Path, window_events: usize) -> PathBuf {
    let seconds = Utc::now().timestamp();
    dataset_root
        .join("rolling_event_datasets")
        .join(format!("window_events_{window_events}_{seconds}"))
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|arg| arg == flag)
}

fn parse_usize_flag(args: &[String], flag: &str, default: usize) -> Result<usize> {
    match flag_value(args, flag) {
        Some(raw) => raw
            .parse::<usize>()
            .with_context(|| format!("parse {flag}={raw} as usize")),
        None => Ok(default),
    }
}

fn optional_usize_flag(args: &[String], flag: &str) -> Result<Option<usize>> {
    flag_value(args, flag)
        .map(|raw| {
            raw.parse::<usize>()
                .with_context(|| format!("parse {flag}={raw} as usize"))
        })
        .transpose()
}

fn print_help() {
    eprintln!(
        r#"Event dataset rolling window splitter

Usage:
  cargo run -p ploy-research --example event_dataset_rolling_windows --features polars-export -- \
    --dataset <event-root-dir> --output-root <dir> --window-events 150

Options:
  --dataset <dir>         Source event-root dataset directory.
  --output-root <dir>     Output root for child event-root datasets.
  --window-events <n>     Chronological events per child dataset. Default: 150.
  --max-windows <n>       Export at most n windows.
  --dry-run               Plan windows without writing child datasets.
"#
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use ploy_research::{
        build_event_root_dataset, export_event_root_dataset_parquet, DatasetSourceWindow,
        EventMetadataChronologyInput, EventRootDatasetBuildRequest, FactorObservation,
    };
    use std::fs;

    #[test]
    fn minimum_event_count_matches_default_eval_split_floor() {
        assert_eq!(
            minimum_events_for_policy(&DatasetSplitPolicy::default()),
            134
        );
    }

    #[test]
    fn planning_skips_too_small_final_remainder() {
        let start = Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap();
        let events = synthetic_event_index(430, start);

        let (windows, skipped) = plan_rolling_windows(&events, 150, 134, None).unwrap();

        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].events.len(), 150);
        assert_eq!(windows[1].events.len(), 150);
        assert_eq!(skipped, 130);
    }

    #[test]
    fn reassigns_splits_inside_each_window_without_cross_window_leakage() {
        let start = Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap();
        let events = synthetic_event_index(150, start);

        let artifacts = reassign_window_splits(&events, &DatasetSplitPolicy::default()).unwrap();
        let counts = split_counts_for_assignments(&artifacts.split_assignments);

        assert_eq!(counts.train, 105);
        assert_eq!(counts.val, 22);
        assert_eq!(counts.test, 23);

        let train = split_event_ids(&artifacts.split_assignments, DatasetSplit::Train);
        let val = split_event_ids(&artifacts.split_assignments, DatasetSplit::Val);
        let test = split_event_ids(&artifacts.split_assignments, DatasetSplit::Test);
        assert!(train.is_disjoint(&val));
        assert!(train.is_disjoint(&test));
        assert!(val.is_disjoint(&test));
    }

    #[test]
    fn writes_valid_child_event_root_datasets() {
        let start = Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap();
        let source_root = temp_path("event-root-source");
        let output_root = temp_path("event-root-windows");
        let build = synthetic_build(450, start);
        export_event_root_dataset_parquet(&build, &source_root).unwrap();

        let config = Config {
            dataset_root: source_root.clone(),
            output_root: output_root.clone(),
            window_events: 150,
            max_windows: Some(3),
            dry_run: false,
        };
        let report = split_event_root_dataset(&config).unwrap();

        assert_eq!(report.exported_windows, 3);
        assert_eq!(report.skipped_events, 0);
        assert!(output_root.join("rolling_datasets_report.json").exists());
        assert!(output_root.join("rolling_datasets_report.md").exists());
        assert!(output_root.join("rolling_datasets.txt").exists());

        for window in 1..=3 {
            let child_root = output_root.join(format!("event_root_window_{window:03}"));
            let manifest = read_manifest(&child_root).unwrap();
            manifest.validate_contract().unwrap();
            assert_eq!(manifest.stats.total_events, 150);
            assert_eq!(manifest.stats.events_per_split.train, 105);
            assert_eq!(manifest.stats.events_per_split.val, 22);
            assert_eq!(manifest.stats.events_per_split.test, 23);
            assert_eq!(manifest.stats.observations_per_split.train, 210);
            assert_eq!(manifest.stats.observations_per_split.val, 44);
            assert_eq!(manifest.stats.observations_per_split.test, 46);

            for artifact in [
                &manifest.artifacts.event_index,
                &manifest.artifacts.split_assignments,
                &manifest.artifacts.observations.train,
                &manifest.artifacts.observations.val,
                &manifest.artifacts.observations.test,
                &manifest.artifacts.event_summaries.train,
                &manifest.artifacts.event_summaries.val,
                &manifest.artifacts.event_summaries.test,
            ] {
                assert!(child_root.join(artifact).exists(), "missing {artifact}");
            }
        }

        let _ = fs::remove_dir_all(source_root);
        let _ = fs::remove_dir_all(output_root);
    }

    fn synthetic_build(
        count: usize,
        start: chrono::DateTime<Utc>,
    ) -> ploy_research::EventRootDatasetBuild {
        let chronology_events = synthetic_chronology_events(count, start);
        let observations = synthetic_observations(count, start);
        let request = EventRootDatasetBuildRequest::new(
            &observations,
            chronology_events,
            DatasetSourceWindow {
                start_time: start,
                end_time: start + Duration::minutes(count as i64 * 5),
                symbols: vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()],
            },
            standard_event_root_dataset_artifacts(),
            start,
        );
        build_event_root_dataset(request).unwrap()
    }

    fn synthetic_event_index(count: usize, start: chrono::DateTime<Utc>) -> Vec<EventIndexEntry> {
        (0..count)
            .map(|idx| {
                let event_start = start + Duration::minutes(idx as i64 * 5);
                EventIndexEntry {
                    event_id: format!("evt-{idx:03}"),
                    symbol: if idx % 2 == 0 { "BTCUSDT" } else { "ETHUSDT" }.to_string(),
                    start_time: event_start,
                    end_time: event_start + Duration::minutes(5),
                    split: DatasetSplit::Train,
                    split_rank: idx,
                    observation_row_count: 2,
                    settlement_label_available: true,
                    repricing_label_row_count_30s: 1,
                    regime_version: "pm_binary_v1".to_string(),
                }
            })
            .collect()
    }

    fn synthetic_chronology_events(
        count: usize,
        start: chrono::DateTime<Utc>,
    ) -> Vec<EventMetadataChronologyInput> {
        (0..count)
            .rev()
            .map(|idx| {
                let event_start = start + Duration::minutes(idx as i64 * 5);
                EventMetadataChronologyInput {
                    event_id: format!("evt-{idx:03}"),
                    symbol: if idx % 2 == 0 { "BTCUSDT" } else { "ETHUSDT" }.to_string(),
                    start_time: Some(event_start),
                    end_time: Some(event_start + Duration::minutes(5)),
                }
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
                    synthetic_observation(&event_id, symbol, event_start, 0, Some(0.01)),
                    synthetic_observation(&event_id, symbol, event_start, 1, None),
                ]
            })
            .collect()
    }

    fn synthetic_observation(
        event_id: &str,
        symbol: &str,
        event_start: chrono::DateTime<Utc>,
        row_idx: i64,
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
            settlement_up: if row_idx % 2 == 0 { 1.0 } else { 0.0 },
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

    fn temp_path(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ploy-{prefix}-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ))
    }
}
