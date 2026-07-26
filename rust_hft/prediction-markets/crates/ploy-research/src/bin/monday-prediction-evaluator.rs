//! monday-prediction-evaluator — executable PM5D factor walk-forward review
//!
//! The training window fits each factor's direction and selected-quantile
//! threshold. The following test window only applies that trained threshold and
//! scores executable PnL after PM CLOB fillability.

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use ploy_research::factors_v2::{
    build_full_depth_execution_matrix_with_event_rows,
    validate_prediction_research_prior_for_source_kind, FullDepthExecutionEventRow,
    SettlementProbabilityComponentProfile,
};
use ploy_research::prediction_loop::{
    current_prediction_policy_snapshot_id, validate_prediction_snapshot_sources,
};
use ploy_research::prediction_mcts::{
    PredictionMctsCandidate, PredictionMctsEvaluation, SettlementTrainingEvidence,
};
use ploy_research::{
    autofactor_matrix_from_v2, build_factor_observations_v2_with_deribit_and_pm_books,
    build_factor_stability_report, build_prediction_research_feedback,
    build_settlement_probability_promotion_gate_report,
    build_settlement_probability_report_with_prior,
    build_settlement_training_probability_report_with_prior, evaluate_reprice_pilot_selection,
    fit_reprice_pilot_selection, format_autofactor_reports, format_factor_combo_v1_report,
    format_factor_stability_report, format_factor_walk_forward_v2_report,
    format_fillability_review_v1_report, format_full_depth_execution_matrix_report,
    format_liquidity_gate_v1_report, format_liquidity_gated_alpha_v1_report,
    format_meta_label_walk_forward_v1_report, format_repricing_ic_report,
    format_settlement_probability_promotion_gate_report, format_settlement_probability_report,
    format_settlement_probability_walk_forward_report, format_trade_formation_v1_report,
    liquidity_gate_v1_with_deribit_and_pm_books,
    liquidity_gated_alpha_v1_with_deribit_and_pm_books, load_research_snapshot,
    mine_domain_autofactors_from_v2_with_guidance, read_formula_mcts_checkpoint,
    review_fillability_v1_with_deribit_and_pm_books, review_repricing_ic_with_deribit_and_pm_books,
    review_trade_formation_v1_with_deribit_and_pm_books, split_reprice_rows_by_event_cohort,
    validate_prediction_research_prior, validate_snapshot_request_coverage,
    walk_forward_factor_combo_v1_with_deribit_and_pm_books,
    walk_forward_factors_v2_with_deribit_and_pm_books,
    walk_forward_meta_label_v1_with_deribit_and_pm_books,
    walk_forward_settlement_probability_report_with_prior,
    walk_forward_settlement_verdict_report_with_prior,
    write_alpha_search_artifacts_with_state_and_runtime_feedback,
    write_side_bound_alpha_search_artifacts_with_state_and_runtime_feedback,
    AlphaSearchArtifactSummary, AlphaSearchRuntimeFeedback, AlphaZooSnapshot, AutoFactorOptions,
    AutoFactorV2Target, FactorComboV1Options, FactorObservation, FactorObservationV2,
    FactorReviewOptions, FactorStabilityOptions, FactorWalkForwardOptions,
    FillabilityReviewOptions, FullDepthExecutionMatrixOptions, FullDepthExecutionMatrixReport,
    LiquidityGateV1Options, LiquidityGatedAlphaV1Options, LlmPriorSpec,
    MetaLabelWalkForwardOptions, RepricePilotMetrics, RepricePilotSelection, RepricingIcOptions,
    ResearchSnapshotRequest, ReviewSide, SettlementProbabilityDataQualityMode,
    SettlementProbabilityPromotionGateOptions, SettlementProbabilityPromotionGateReport,
    SettlementProbabilityReport, SettlementProbabilityReportOptions,
    SettlementProbabilityTimeCohort, SettlementProbabilityWalkForwardAggregate,
    SettlementProbabilityWalkForwardOptions, SettlementProbabilityWalkForwardReport,
    SettlementProbabilityWalkForwardWindow, SettlementVerdictWalkForwardAggregate,
    SettlementVerdictWalkForwardReport, SettlementVerdictWalkForwardWindow,
    TradeFormationReviewOptions,
};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, serde::Serialize)]
struct ReportArtifactContext<'a> {
    mission_id: &'a str,
    search_policy_snapshot_id: &'a str,
    snapshot_hash: &'a str,
    snapshot_contract_hash: &'a str,
    start: &'a DateTime<Utc>,
    end: &'a DateTime<Utc>,
    time_cohort_boundary_ms: Option<i64>,
    event_window_secs: Option<i64>,
}

#[derive(serde::Serialize)]
struct SettlementBaselineArtifact<'a> {
    schema_version: &'static str,
    non_finite_floats: &'static str,
    #[serde(flatten)]
    context: ReportArtifactContext<'a>,
    settlement_probability: &'a SettlementProbabilityReport,
    settlement_probability_walk_forward: SettlementProbabilityWalkForwardArtifact<'a>,
    settlement_verdict_walk_forward: SettlementVerdictWalkForwardArtifact<'a>,
    promotion_gate: &'a SettlementProbabilityPromotionGateReport,
}

#[derive(serde::Serialize)]
struct SettlementProbabilityWalkForwardArtifact<'a> {
    windows: &'a [SettlementProbabilityWalkForwardWindow],
    aggregates: &'a [SettlementProbabilityWalkForwardAggregate],
}

#[derive(serde::Serialize)]
struct SettlementVerdictWalkForwardArtifact<'a> {
    windows: &'a [SettlementVerdictWalkForwardWindow],
    aggregates: &'a [SettlementVerdictWalkForwardAggregate],
}

#[derive(serde::Serialize)]
struct FullDepthExecutionArtifactProfile<'a> {
    options: &'a FullDepthExecutionMatrixOptions,
    rows: Vec<&'a FullDepthExecutionEventRow>,
}

#[derive(serde::Serialize)]
struct FullDepthExecutionArtifact<'a> {
    schema_version: &'static str,
    non_finite_floats: &'static str,
    #[serde(flatten)]
    context: ReportArtifactContext<'a>,
    side: ReviewSide,
    observed: FullDepthExecutionArtifactProfile<'a>,
    conservative: FullDepthExecutionArtifactProfile<'a>,
}

#[derive(serde::Serialize)]
struct PipelineSmokeReport<'a> {
    schema_version: &'static str,
    status: &'static str,
    scope: &'static str,
    mission_id: &'a str,
    task: &'a str,
    snapshot_hash: &'a str,
    snapshot_contract_id: &'a str,
    search_policy_snapshot_id: &'a str,
    claims_excluded: [&'static str; 6],
}

#[derive(serde::Serialize)]
struct RepricePilotSearchArtifact {
    summary: AlphaSearchArtifactSummary,
    formula_mcts_checkpoint_sha256: String,
    mcts_state_sha256: String,
    mcts_expansion_plan_sha256: String,
}

#[derive(Clone, serde::Serialize)]
struct RepricePilotEpisodeCohorts {
    key: &'static str,
    train_market_ids: Vec<String>,
    test_market_ids: Vec<String>,
}

#[derive(serde::Serialize)]
struct RepricePilotArtifact<'a> {
    schema_version: &'static str,
    non_finite_floats: &'static str,
    #[serde(flatten)]
    context: ReportArtifactContext<'a>,
    status: &'static str,
    episode_cohorts: RepricePilotEpisodeCohorts,
    target: String,
    side: ReviewSide,
    train_rows: usize,
    test_rows: usize,
    excluded_rows: usize,
    search: RepricePilotSearchArtifact,
    selection: &'a RepricePilotSelection,
    test: &'a RepricePilotMetrics,
}

#[derive(Clone, Copy)]
struct RepricePilotConfig<'a> {
    symbols: &'a [String],
    event_window_secs: Option<i64>,
    lob_sample_secs: i32,
    pm_book_sample_secs: i32,
    observation_sample_secs: i64,
    expected_policy: Option<&'a str>,
    alpha_search_output_dir: Option<&'a str>,
    report_output_dir: Option<&'a str>,
}

fn require_report_identity<'a>(
    snapshot_hash: &'a str,
    snapshot_contract_hash: Option<&'a str>,
    mission_id: Option<&'a str>,
    search_policy_snapshot_id: Option<&'a str>,
) -> Result<(&'a str, &'a str, &'a str, &'a str), String> {
    fn required<'a>(value: Option<&'a str>, field: &str) -> Result<&'a str, String> {
        value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("evaluator report output requires {field}"))
    }
    Ok((
        required(Some(snapshot_hash), "snapshot_hash")?,
        required(snapshot_contract_hash, "snapshot_contract_hash")?,
        required(mission_id, "mission_id")?,
        required(search_policy_snapshot_id, "search_policy_snapshot_id")?,
    ))
}

fn validate_reprice_pilot_config(
    args: &[String],
    config: &RepricePilotConfig<'_>,
) -> Result<(), String> {
    if config.symbols.len() != 1 || prediction_underlying_symbol(&config.symbols[0]) != "BTC" {
        return Err("--reprice-pilot-10s requires exactly one BTC symbol".to_string());
    }
    if config.event_window_secs != Some(300) {
        return Err("--reprice-pilot-10s requires --event-window-secs 300".to_string());
    }
    if config.lob_sample_secs != 1
        || config.pm_book_sample_secs != 1
        || config.observation_sample_secs != 1
    {
        return Err(
            "--reprice-pilot-10s requires --lob-sample-secs, --pm-book-sample-secs, and --observation-sample-secs to all equal 1".to_string(),
        );
    }
    for (value, flag) in [
        (
            config.expected_policy,
            "--expected-search-policy-snapshot-id",
        ),
        (config.alpha_search_output_dir, "--alpha-search-output-dir"),
        (config.report_output_dir, "--report-output-dir"),
    ] {
        if value.is_none_or(|value| value.trim().is_empty()) {
            return Err(format!("--reprice-pilot-10s requires {flag}"));
        }
    }
    for flag in [
        "--alpha-search-plan-json",
        "--alpha-search-llm-prior-json",
        "--alpha-search-state-json",
        "--formula-mcts-checkpoint-json",
        "--alpha-zoo-snapshot-json",
        "--candidate-strategy-replay-json",
    ] {
        if flag_present(args, flag) {
            return Err(format!(
                "--reprice-pilot-10s does not accept {flag}; search must use this run's train cohort only"
            ));
        }
    }
    Ok(())
}

fn validate_report_observation_count(
    report_output_dir: Option<&str>,
    observation_count: usize,
) -> Result<(), &'static str> {
    if report_output_dir.is_some() && observation_count == 0 {
        return Err("requested evaluator reports require at least one observation");
    }
    Ok(())
}

fn reject_report_digest_sibling(
    report_output_dir: &Path,
    prefix: &str,
    output_path: &Path,
) -> Result<(), String> {
    let filename_prefix = format!("{prefix}-");
    let entries = std::fs::read_dir(report_output_dir)
        .map_err(|error| format!("read report directory: {error}"))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("read report directory entry: {error}"))?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path != output_path && name.starts_with(&filename_prefix) && name.ends_with(".json") {
            return Err(format!("report {prefix} has different digest sibling"));
        }
    }
    Ok(())
}

fn write_content_addressed_report<T: serde::Serialize>(
    report_output_dir: &Path,
    prefix: &str,
    value: &T,
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(report_output_dir)
        .map_err(|error| format!("create report directory: {error}"))?;
    let json = serde_json::to_string_pretty(value)
        .map_err(|error| format!("serialize {prefix}: {error}"))?;
    let bytes = format!("{json}\n");
    let digest = format!("{:x}", Sha256::digest(bytes.as_bytes()));
    let output_path = report_output_dir.join(format!("{prefix}-{digest}.json"));
    let lock_path = report_output_dir.join(format!(".{prefix}.lock"));
    std::fs::create_dir(&lock_path)
        .map_err(|error| format!("acquire report lock {}: {error}", lock_path.display()))?;
    let result = (|| -> Result<(), String> {
        reject_report_digest_sibling(report_output_dir, prefix, &output_path)?;
        let temp_path = lock_path.join("report.tmp");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .map_err(|error| format!("create report temp {}: {error}", temp_path.display()))?;
        file.write_all(bytes.as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|error| format!("write report temp {}: {error}", temp_path.display()))?;
        reject_report_digest_sibling(report_output_dir, prefix, &output_path)?;
        match std::fs::hard_link(&temp_path, &output_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                let existing = std::fs::read(&output_path)
                    .map_err(|error| format!("read existing report: {error}"))?;
                if existing == bytes.as_bytes() {
                    Ok(())
                } else {
                    Err("existing report has different bytes".to_string())
                }
            }
            Err(error) => Err(format!("publish report {}: {error}", output_path.display())),
        }
    })();
    let cleanup = std::fs::remove_dir_all(&lock_path);
    let directory_sync = std::fs::File::open(report_output_dir).and_then(|dir| dir.sync_all());
    result?;
    cleanup.map_err(|error| format!("remove report lock {}: {error}", lock_path.display()))?;
    directory_sync.map_err(|error| format!("sync report directory: {error}"))?;
    Ok(output_path)
}

fn write_pipeline_smoke_report(
    report_output_dir: &Path,
    mission_id: &str,
    task: &str,
    snapshot_hash: &str,
    snapshot_contract_id: &str,
    search_policy_snapshot_id: &str,
) -> Result<PathBuf, String> {
    write_content_addressed_report(
        report_output_dir,
        "pipeline-smoke",
        &PipelineSmokeReport {
            schema_version: "monday.prediction.pipeline_smoke.v1",
            status: "completed",
            scope: "pipeline_compatibility_only",
            mission_id,
            task,
            snapshot_hash,
            snapshot_contract_id,
            search_policy_snapshot_id,
            claims_excluded: [
                "alpha",
                "profitability",
                "paper",
                "shadow",
                "live",
                "promotion",
            ],
        },
    )
}

fn write_report_set<S: serde::Serialize, U: serde::Serialize, D: serde::Serialize>(
    report_output_dir: &Path,
    settlement: &S,
    up: &U,
    down: &D,
) -> Result<[PathBuf; 3], String> {
    Ok([
        write_content_addressed_report(report_output_dir, "settlement-baseline", settlement)?,
        write_content_addressed_report(report_output_dir, "full-depth-execution-up", up)?,
        write_content_addressed_report(report_output_dir, "full-depth-execution-down", down)?,
    ])
}

fn execution_rows_for_side<'a>(
    rows: &'a [FullDepthExecutionEventRow],
    side: ReviewSide,
) -> Vec<&'a FullDepthExecutionEventRow> {
    rows.iter().filter(|row| row.side == side).collect()
}

fn full_depth_execution_artifact<'a>(
    context: ReportArtifactContext<'a>,
    side: ReviewSide,
    execution_matrix: &'a FullDepthExecutionMatrixReport,
    execution_event_rows: &'a [FullDepthExecutionEventRow],
    conservative_execution_matrix: &'a FullDepthExecutionMatrixReport,
    conservative_execution_event_rows: &'a [FullDepthExecutionEventRow],
) -> FullDepthExecutionArtifact<'a> {
    FullDepthExecutionArtifact {
        schema_version: "monday.polymarket.full_depth_execution.v2",
        non_finite_floats: "null",
        context,
        side,
        observed: FullDepthExecutionArtifactProfile {
            options: &execution_matrix.options,
            rows: execution_rows_for_side(execution_event_rows, side),
        },
        conservative: FullDepthExecutionArtifactProfile {
            options: &conservative_execution_matrix.options,
            rows: execution_rows_for_side(conservative_execution_event_rows, side),
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn write_report_artifacts(
    report_output_dir: &Path,
    snapshot_hash: &str,
    snapshot_contract_hash: Option<&str>,
    mission_id: Option<&str>,
    search_policy_snapshot_id: Option<&str>,
    start: &DateTime<Utc>,
    end: &DateTime<Utc>,
    time_cohort_boundary_ms: Option<i64>,
    event_window_secs: Option<i64>,
    execution_matrix: &FullDepthExecutionMatrixReport,
    execution_event_rows: &[FullDepthExecutionEventRow],
    conservative_execution_matrix: &FullDepthExecutionMatrixReport,
    conservative_execution_event_rows: &[FullDepthExecutionEventRow],
    settlement_probability_report: &SettlementProbabilityReport,
    settlement_probability_walk_forward_report: &SettlementProbabilityWalkForwardReport,
    settlement_verdict_walk_forward_report: &SettlementVerdictWalkForwardReport,
    promotion_gate_report: &SettlementProbabilityPromotionGateReport,
) -> Result<[PathBuf; 3], String> {
    let (snapshot_hash, snapshot_contract_hash, mission_id, search_policy_snapshot_id) =
        require_report_identity(
            snapshot_hash,
            snapshot_contract_hash,
            mission_id,
            search_policy_snapshot_id,
        )?;
    let context = ReportArtifactContext {
        mission_id,
        search_policy_snapshot_id,
        snapshot_hash,
        snapshot_contract_hash,
        start,
        end,
        time_cohort_boundary_ms,
        event_window_secs,
    };
    let settlement = SettlementBaselineArtifact {
        schema_version: "monday.polymarket.settlement_baseline.v1",
        non_finite_floats: "null",
        context,
        settlement_probability: settlement_probability_report,
        settlement_probability_walk_forward: SettlementProbabilityWalkForwardArtifact {
            windows: &settlement_probability_walk_forward_report.windows,
            aggregates: &settlement_probability_walk_forward_report.aggregates,
        },
        settlement_verdict_walk_forward: SettlementVerdictWalkForwardArtifact {
            windows: &settlement_verdict_walk_forward_report.windows,
            aggregates: &settlement_verdict_walk_forward_report.aggregates,
        },
        promotion_gate: promotion_gate_report,
    };
    let up = full_depth_execution_artifact(
        context,
        ReviewSide::Up,
        execution_matrix,
        execution_event_rows,
        conservative_execution_matrix,
        conservative_execution_event_rows,
    );
    let down = full_depth_execution_artifact(
        context,
        ReviewSide::Down,
        execution_matrix,
        execution_event_rows,
        conservative_execution_matrix,
        conservative_execution_event_rows,
    );
    write_report_set(report_output_dir, &settlement, &up, &down)
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("read search evidence {}: {error}", path.display()))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn sorted_distinct_reprice_pilot_market_ids<'a>(
    market_ids: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    let mut market_ids = market_ids
        .into_iter()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    market_ids.sort_unstable();
    market_ids.dedup();
    market_ids
}

#[allow(clippy::too_many_arguments)]
fn run_reprice_pilot(
    rows: &[FactorObservationV2],
    boundary: DateTime<Utc>,
    horizon_secs: i64,
    alpha_search_output_dir: &Path,
    report_output_dir: &Path,
    snapshot_hash: &str,
    snapshot_contract_hash: Option<&str>,
    mission_id: Option<&str>,
    search_policy_snapshot_id: Option<&str>,
    start: &DateTime<Utc>,
    end: &DateTime<Utc>,
    time_cohort_boundary_ms: i64,
    event_window_secs: Option<i64>,
    top_quantile: f64,
    min_observations: usize,
    min_full_depth_entry_fill_rate: f64,
) -> Result<[PathBuf; 2], String> {
    let (train_refs, test_refs) = split_reprice_rows_by_event_cohort(rows, boundary)
        .map_err(|error| format!("validate reprice market episodes: {error}"))?;
    let train_rows = train_refs.into_iter().cloned().collect::<Vec<_>>();
    let test_rows = test_refs.into_iter().cloned().collect::<Vec<_>>();
    let excluded_rows = rows
        .len()
        .saturating_sub(train_rows.len().saturating_add(test_rows.len()));
    if train_rows.is_empty() || test_rows.is_empty() {
        return Err(format!(
            "reprice pilot requires non-empty complete train and test event cohorts; train_rows={} test_rows={} excluded_rows={excluded_rows}",
            train_rows.len(),
            test_rows.len(),
        ));
    }
    let episode_cohorts = RepricePilotEpisodeCohorts {
        key: "polymarket market_id carried as FactorObservationV2.event_id",
        train_market_ids: sorted_distinct_reprice_pilot_market_ids(
            train_rows.iter().map(|row| row.event_id.as_str()),
        ),
        test_market_ids: sorted_distinct_reprice_pilot_market_ids(
            test_rows.iter().map(|row| row.event_id.as_str()),
        ),
    };
    let (snapshot_hash, snapshot_contract_hash, mission_id, search_policy_snapshot_id) =
        require_report_identity(
            snapshot_hash,
            snapshot_contract_hash,
            mission_id,
            search_policy_snapshot_id,
        )?;
    let context = ReportArtifactContext {
        mission_id,
        search_policy_snapshot_id,
        snapshot_hash,
        snapshot_contract_hash,
        start,
        end,
        time_cohort_boundary_ms: Some(time_cohort_boundary_ms),
        event_window_secs,
    };
    let options = AutoFactorOptions {
        min_observations: min_observations.max(50),
        min_window_observations: min_observations.max(20),
        min_top_bucket_full_depth_entry_fill_rate: min_full_depth_entry_fill_rate,
        ..Default::default()
    };
    let run_side = |side| -> Result<PathBuf, String> {
        let target = match horizon_secs {
            10 => AutoFactorV2Target::FullDepthRepricePnl10s(side),
            15 => AutoFactorV2Target::FullDepthRepricePnl15s(side),
            _ => return Err(format!("unsupported reprice pilot horizon {horizon_secs}s")),
        };
        let train_side = train_rows
            .iter()
            .filter(|row| row.side == side)
            .cloned()
            .collect::<Vec<_>>();
        let test_side = test_rows
            .iter()
            .filter(|row| row.side == side)
            .cloned()
            .collect::<Vec<_>>();
        if train_side.is_empty() || test_side.is_empty() {
            return Err(format!(
                "reprice pilot target={} side={} requires non-empty train and test rows; train={} test={}",
                target.as_str(),
                side.as_str(),
                train_side.len(),
                test_side.len(),
            ));
        }
        let reports =
            mine_domain_autofactors_from_v2_with_guidance(&train_side, target, &options, &[], None)
                .map_err(|error| format!("mine train-only reprice candidates: {error}"))?;
        let input_names = autofactor_matrix_from_v2(&train_side)
            .map_err(|error| format!("build train-only reprice matrix: {error}"))?
            .input_names()
            .into_iter()
            .collect::<Vec<_>>();
        let summary = write_side_bound_alpha_search_artifacts_with_state_and_runtime_feedback(
            alpha_search_output_dir,
            target.as_str(),
            side,
            &input_names,
            &reports,
            &options,
            None,
            None,
            None,
        )
        .map_err(|error| format!("write train-only MCTS search artifacts: {error}"))?;
        let candidate_name = summary.best_candidate.as_deref().ok_or_else(|| {
            format!(
                "MCTS produced no candidate for target={} side={}",
                target.as_str(),
                side.as_str()
            )
        })?;
        let candidate = reports
            .iter()
            .find(|report| report.name == candidate_name)
            .ok_or_else(|| {
                format!("MCTS candidate {candidate_name} was not in its train-only report")
            })?;
        let selection = fit_reprice_pilot_selection(candidate, &train_side, target, top_quantile)
            .map_err(|error| format!("fit train-only reprice selection: {error}"))?;
        let test = evaluate_reprice_pilot_selection(&selection, &test_side, target)
            .map_err(|error| format!("evaluate frozen reprice selection: {error}"))?;
        let search_dir = Path::new(&summary.output_dir);
        let search = RepricePilotSearchArtifact {
            formula_mcts_checkpoint_sha256: sha256_file(
                &search_dir.join("formula-mcts-checkpoint.json"),
            )?,
            mcts_state_sha256: sha256_file(&search_dir.join("mcts-state.json"))?,
            mcts_expansion_plan_sha256: sha256_file(&search_dir.join("mcts-expansion-plan.json"))?,
            summary,
        };
        let artifact = RepricePilotArtifact {
            schema_version: "monday.polymarket.reprice_pilot.v2",
            non_finite_floats: "null",
            context,
            status: "pilot_not_promotable",
            episode_cohorts: episode_cohorts.clone(),
            target: target.as_str().to_string(),
            side,
            train_rows: train_side.len(),
            test_rows: test_side.len(),
            excluded_rows,
            search,
            selection: &selection,
            test: &test,
        };
        let prefix = format!("reprice-{horizon_secs}s-{}", side.as_str());
        let path = write_content_addressed_report(report_output_dir, &prefix, &artifact)?;
        eprintln!(
            "reprice pilot evidence written target={} side={} train_rows={} test_rows={} selected_test_decisions={} path={}",
            target.as_str(),
            side.as_str(),
            train_side.len(),
            test_side.len(),
            test.selected_decisions,
            path.display()
        );
        Ok(path)
    };
    Ok([run_side(ReviewSide::Up)?, run_side(ReviewSide::Down)?])
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|window| window[0] == flag)
        .map(|window| window[1].clone())
}

fn flag_present(args: &[String], flag: &str) -> bool {
    args.iter().any(|arg| arg == flag)
}

#[derive(Debug)]
struct PipelineSmokeArgs {
    task: String,
    snapshot_dir: String,
    mission_id: String,
    snapshot_hash: String,
    snapshot_contract_id: String,
    search_policy_snapshot_id: String,
    report_output_dir: String,
}

fn validate_pipeline_smoke_args(args: &[String]) -> Result<PipelineSmokeArgs, String> {
    const FLAGS: [&str; 7] = [
        "--pipeline-smoke-task",
        "--snapshot-dir",
        "--mission-id",
        "--snapshot-hash",
        "--snapshot-contract-id",
        "--expected-search-policy-snapshot-id",
        "--report-output-dir",
    ];
    if args.len() != 1 + FLAGS.len() * 2
        || args
            .iter()
            .filter(|value| value.starts_with("--"))
            .any(|flag| !FLAGS.contains(&flag.as_str()))
    {
        return Err(
            "--pipeline-smoke-task only accepts its typed snapshot/report arguments".into(),
        );
    }
    let value = |flag: &str| -> Result<String, String> {
        let matches = args
            .windows(2)
            .filter(|window| window[0] == flag)
            .map(|window| window[1].trim())
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [value] if !value.is_empty() && !value.starts_with("--") => Ok((*value).to_string()),
            _ => Err(format!("--pipeline-smoke-task requires exactly one {flag}")),
        }
    };
    let parsed = PipelineSmokeArgs {
        task: value("--pipeline-smoke-task")?,
        snapshot_dir: value("--snapshot-dir")?,
        mission_id: value("--mission-id")?,
        snapshot_hash: value("--snapshot-hash")?,
        snapshot_contract_id: value("--snapshot-contract-id")?,
        search_policy_snapshot_id: value("--expected-search-policy-snapshot-id")?,
        report_output_dir: value("--report-output-dir")?,
    };
    if parsed.task != "settlement_probability" {
        return Err("--pipeline-smoke-task is not an admitted typed task".into());
    }
    Ok(parsed)
}

fn run_pipeline_smoke(args: &[String]) -> Result<(), String> {
    let args = validate_pipeline_smoke_args(args)?;
    let snapshot = load_research_snapshot(Path::new(&args.snapshot_dir))
        .map_err(|error| format!("load pipeline smoke snapshot: {error:#}"))?;
    let actual_snapshot_hash = snapshot
        .manifest
        .snapshot_hash
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "pipeline smoke snapshot is missing snapshot_hash".to_string())?;
    if actual_snapshot_hash != args.snapshot_hash {
        return Err("pipeline smoke snapshot hash does not match admitted digest".into());
    }
    validate_prediction_snapshot_contract_id(
        &args.snapshot_contract_id,
        snapshot.manifest.snapshot_contract_hash.as_deref(),
    )?;
    validate_expected_prediction_policy(Some(&args.search_policy_snapshot_id), None)?;
    let path = write_pipeline_smoke_report(
        Path::new(&args.report_output_dir),
        &args.mission_id,
        &args.task,
        &args.snapshot_hash,
        &args.snapshot_contract_id,
        &args.search_policy_snapshot_id,
    )?;
    eprintln!(
        "pipeline smoke compatibility report written {}",
        path.display()
    );
    Ok(())
}

fn prediction_underlying_symbol(raw: &str) -> String {
    let normalized = raw.trim().to_ascii_uppercase().replace(['/', '-', '_'], "");
    for quote in ["USDT", "USDC", "USD"] {
        if let Some(base) = normalized.strip_suffix(quote) {
            if !base.is_empty() {
                return base.to_string();
            }
        }
    }
    normalized
}

fn validate_prediction_snapshot_contract_id(
    prior_snapshot_id: &str,
    snapshot_contract_hash: Option<&str>,
) -> Result<(), String> {
    let snapshot_contract_hash = snapshot_contract_hash
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "loaded snapshot is missing snapshot_contract_hash".to_string())?;
    if prior_snapshot_id != snapshot_contract_hash {
        return Err(format!(
            "prediction research prior data_snapshot_id {prior_snapshot_id} does not match loaded snapshot_contract_hash {snapshot_contract_hash}"
        ));
    }
    Ok(())
}

fn validate_prediction_snapshot_profile(
    source_kind: &str,
    baseline_flag: bool,
) -> Result<SettlementProbabilityComponentProfile, String> {
    let baseline = source_kind == ploy_research::POLYMARKET_CHAINLINK_BASELINE_SOURCE_KIND;
    match (baseline, baseline_flag) {
        (true, true) => Ok(SettlementProbabilityComponentProfile::MarketMidpointOnly),
        (true, false) => {
            Err("reduced-authority snapshot requires --polymarket-chainlink-baseline".to_string())
        }
        (false, true) => Err(
            "--polymarket-chainlink-baseline does not match the loaded snapshot profile"
                .to_string(),
        ),
        (false, false) => Ok(SettlementProbabilityComponentProfile::FullSurface),
    }
}

fn validate_expected_prediction_policy(
    expected: Option<&str>,
    governed_prior: Option<&LlmPriorSpec>,
) -> Result<(), String> {
    let expected = match (expected, governed_prior.is_some()) {
        (Some(expected), _) => expected,
        (None, true) => {
            return Err(
                "governed prediction evaluation requires --expected-search-policy-snapshot-id"
                    .to_string(),
            )
        }
        (None, false) => return Ok(()),
    };
    let current = current_prediction_policy_snapshot_id();
    if expected != current {
        return Err(format!(
            "expected prediction policy {expected} does not match current evaluator policy {current}"
        ));
    }
    if governed_prior
        .is_some_and(|prior| prior.search_policy_snapshot_id.as_deref() != Some(expected))
    {
        return Err("governed prior policy differs from current evaluator policy".to_string());
    }
    Ok(())
}

fn parse_time_cohort_boundary(
    raw: Option<&str>,
    required: bool,
) -> Result<Option<DateTime<Utc>>, String> {
    let Some(raw) = raw else {
        return if required {
            Err("governed prediction evaluation requires --time-cohort-boundary-ms".to_string())
        } else {
            Ok(None)
        };
    };
    let boundary_ms = raw
        .parse::<i64>()
        .map_err(|_| "--time-cohort-boundary-ms must be a positive integer".to_string())?;
    if boundary_ms <= 0 {
        return Err("--time-cohort-boundary-ms must be a positive integer".to_string());
    }
    let boundary = Utc
        .timestamp_millis_opt(boundary_ms)
        .single()
        .ok_or_else(|| "--time-cohort-boundary-ms is outside the UTC range".to_string())?;
    Ok(Some(boundary))
}

fn validate_time_cohort_range(
    boundary: DateTime<Utc>,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<(), String> {
    if boundary <= start || boundary >= end {
        return Err(format!(
            "time-cohort boundary {boundary} must be strictly inside evaluator range [{start}, {end})"
        ));
    }
    Ok(())
}

fn settlement_time_cohort_from_args(
    args: &[String],
    required: bool,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    event_window_secs: Option<i64>,
) -> Result<Option<SettlementProbabilityTimeCohort>, String> {
    let Some(boundary) = parse_time_cohort_boundary(
        flag_value(args, "--time-cohort-boundary-ms").as_deref(),
        required,
    )?
    else {
        return Ok(None);
    };
    validate_time_cohort_range(boundary, start, end)?;
    let event_window_secs = event_window_secs
        .filter(|&value| !required || value == 300)
        .ok_or_else(|| {
            "prediction evaluator time cohort requires --event-window-secs (300 when governed)"
                .to_string()
        })?;
    SettlementProbabilityTimeCohort::new(boundary, event_window_secs).map(Some)
}

fn parse_date_start(raw: &str) -> DateTime<Utc> {
    let date = NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .unwrap_or_else(|_| panic!("invalid date: {raw}"));
    Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
}

fn parse_date_end(raw: &str) -> DateTime<Utc> {
    let date = NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .unwrap_or_else(|_| panic!("invalid date: {raw}"));
    let next_day = date
        .succ_opt()
        .unwrap_or_else(|| panic!("invalid end date: {raw}"));
    Utc.from_utc_datetime(&next_day.and_hms_opt(0, 0, 0).unwrap())
}

fn parse_timestamp(raw: &str) -> DateTime<Utc> {
    raw.parse::<DateTime<Utc>>()
        .unwrap_or_else(|_| panic!("invalid timestamp: {raw}"))
}

fn parse_data_quality_mode(raw: Option<String>) -> SettlementProbabilityDataQualityMode {
    match raw.as_deref().unwrap_or("strict_continuous") {
        "strict_continuous" | "strict-continuous" => {
            SettlementProbabilityDataQualityMode::StrictContinuous
        }
        "event_complete" | "event-complete" => SettlementProbabilityDataQualityMode::EventComplete,
        value => panic!("invalid --data-quality-mode: {value}"),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReportSuite {
    Core,
    Full,
}

impl ReportSuite {
    fn parse(raw: Option<String>) -> Self {
        match raw.as_deref().unwrap_or("full") {
            "core" => Self::Core,
            "full" => Self::Full,
            value => panic!("invalid --report-suite: {value}"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Full => "full",
        }
    }
}

fn replay_parity_evidence(path: &str) -> (bool, String) {
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("read replay parity JSON {path} failed: {err}"));
    let json: serde_json::Value = serde_json::from_str(&raw)
        .unwrap_or_else(|err| panic!("parse replay parity JSON {path} failed: {err}"));
    let runtime_ready = json
        .pointer("/runtime_evidence_comparison/strict_parity_ready")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let event_ready = json
        .pointer("/runtime_evidence_comparison/events/strict_parity_ready")
        .or_else(|| json.pointer("/event_comparison/strict_parity_ready"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let risk_flags = json
        .get("blocking_risk_flags")
        .or_else(|| json.get("risk_flags"))
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let advisory_flags = json
        .get("advisory_flags")
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let decision = json
        .get("decision")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("<missing>");
    let ready = runtime_ready && event_ready && risk_flags.is_empty() && decision == "continue";
    let evidence = format!(
        "replay_parity_json={} runtime_ready={} event_ready={} blocking_flags={} advisory_flags={} decision={}",
        path,
        runtime_ready,
        event_ready,
        if risk_flags.is_empty() {
            "<none>".to_string()
        } else {
            risk_flags.join("|")
        },
        if advisory_flags.is_empty() {
            "<none>".to_string()
        } else {
            advisory_flags.join("|")
        },
        decision,
    );
    (ready, evidence)
}

#[cfg(test)]
mod tests {
    use super::{
        parse_time_cohort_boundary, replay_parity_evidence, require_report_identity,
        settlement_time_cohort_from_args, sorted_distinct_reprice_pilot_market_ids,
        validate_expected_prediction_policy, validate_pipeline_smoke_args,
        validate_prediction_snapshot_contract_id, validate_prediction_snapshot_profile,
        validate_report_observation_count, validate_reprice_pilot_config,
        validate_time_cohort_range, write_pipeline_smoke_report, write_report_set,
        ReportArtifactContext, RepricePilotConfig,
    };
    use chrono::{TimeZone, Utc};
    use ploy_research::prediction_loop::current_prediction_policy_snapshot_id;
    use sha2::{Digest, Sha256};
    use std::fs;

    fn write_parity_fixture(name: &str, payload: &str) -> String {
        let path = std::env::temp_dir().join(format!(
            "ploy-factor-walk-forward-{name}-{}-{}.json",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        fs::write(&path, payload).expect("write parity fixture");
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn governed_prediction_policy_must_match_the_current_evaluator() {
        let current = current_prediction_policy_snapshot_id();
        validate_expected_prediction_policy(Some(&current), None).expect("current baseline policy");
        assert!(
            validate_expected_prediction_policy(Some("sha256:stale"), None)
                .expect_err("stale controller policy must fail")
                .contains("does not match")
        );
    }

    #[test]
    fn reduced_authority_profile_requires_an_exact_explicit_flag_match() {
        let baseline = ploy_research::POLYMARKET_CHAINLINK_BASELINE_SOURCE_KIND;
        assert_eq!(
            validate_prediction_snapshot_profile(baseline, true).unwrap(),
            ploy_research::factors_v2::SettlementProbabilityComponentProfile::MarketMidpointOnly
        );
        assert!(validate_prediction_snapshot_profile(baseline, false).is_err());
        assert!(validate_prediction_snapshot_profile("verified_artifacts", true).is_err());
        assert_eq!(
            validate_prediction_snapshot_profile("verified_artifacts", false).unwrap(),
            ploy_research::factors_v2::SettlementProbabilityComponentProfile::FullSurface
        );
    }

    #[test]
    fn reprice_pilot_configuration_fails_closed() {
        let btc = vec!["BTCUSDT".to_string()];
        let config = RepricePilotConfig {
            symbols: &btc,
            event_window_secs: Some(300),
            lob_sample_secs: 1,
            pm_book_sample_secs: 1,
            observation_sample_secs: 1,
            expected_policy: Some("sha256:policy"),
            alpha_search_output_dir: Some("/tmp/alpha"),
            report_output_dir: Some("/tmp/reports"),
        };
        validate_reprice_pilot_config(&[], &config).expect("complete BTC pilot configuration");

        let eth = vec!["ETHUSDT".to_string()];
        assert!(validate_reprice_pilot_config(
            &[],
            &RepricePilotConfig {
                symbols: &eth,
                ..config
            },
        )
        .expect_err("non-BTC pilot must fail")
        .contains("exactly one BTC"));
        assert!(validate_reprice_pilot_config(
            &[],
            &RepricePilotConfig {
                observation_sample_secs: 30,
                ..config
            },
        )
        .expect_err("non-1s observations must fail")
        .contains("all equal 1"));
        assert!(validate_reprice_pilot_config(
            &[],
            &RepricePilotConfig {
                alpha_search_output_dir: None,
                ..config
            },
        )
        .expect_err("missing alpha evidence output must fail")
        .contains("--alpha-search-output-dir"));
        let stale_state = vec![
            "--alpha-search-state-json".to_string(),
            "stale-state.json".to_string(),
        ];
        assert!(validate_reprice_pilot_config(&stale_state, &config)
            .expect_err("pre-seeded MCTS state can leak held-out evidence")
            .contains("does not accept --alpha-search-state-json"));
        let bare_stale_state = vec!["--alpha-search-state-json".to_string()];
        assert!(validate_reprice_pilot_config(&bare_stale_state, &config)
            .expect_err("a bare pre-seeded MCTS flag must also fail closed")
            .contains("does not accept --alpha-search-state-json"));
    }

    #[test]
    fn reprice_pilot_market_ids_are_sorted_and_distinct() {
        assert_eq!(
            sorted_distinct_reprice_pilot_market_ids(["market-c", "market-a", "market-c"]),
            vec!["market-a".to_string(), "market-c".to_string()]
        );
    }

    #[test]
    fn governed_evaluator_rejects_missing_or_invalid_time_cohort_boundary() {
        let boundary_ms = 1_700_001_000_000_i64;
        let start = Utc
            .timestamp_millis_opt(boundary_ms - 1_000)
            .single()
            .unwrap();
        let end = Utc
            .timestamp_millis_opt(boundary_ms + 1_000)
            .single()
            .unwrap();
        assert!(
            settlement_time_cohort_from_args(&[], true, start, end, Some(300))
                .expect_err("governed baseline must carry a boundary")
                .contains("requires --time-cohort-boundary-ms")
        );
        for raw in ["0", "-1", "not-a-timestamp"] {
            let args = vec!["--time-cohort-boundary-ms".to_string(), raw.to_string()];
            assert!(settlement_time_cohort_from_args(&args, true, start, end, Some(300)).is_err());
        }

        let boundary = parse_time_cohort_boundary(Some(&boundary_ms.to_string()), true)
            .expect("valid governed boundary")
            .expect("present boundary");
        assert_eq!(boundary.timestamp_millis(), boundary_ms);
        validate_time_cohort_range(boundary, start, end).expect("interior boundary");
        assert!(validate_time_cohort_range(start, start, end).is_err());
        assert!(validate_time_cohort_range(end, start, end).is_err());
        let args = vec!["--time-cohort-boundary-ms".into(), boundary_ms.to_string()];
        assert!(settlement_time_cohort_from_args(&args, true, start, end, Some(900)).is_err());
    }

    #[test]
    fn replay_parity_requires_runtime_and_event_strict_readiness() {
        let path = write_parity_fixture(
            "event-not-ready",
            r#"{
              "decision": "continue",
              "blocking_risk_flags": [],
              "runtime_evidence_comparison": {"strict_parity_ready": true},
              "event_comparison": {"strict_parity_ready": false}
            }"#,
        );

        let (ready, evidence) = replay_parity_evidence(&path);

        assert!(!ready);
        assert!(evidence.contains("runtime_ready=true"));
        assert!(evidence.contains("event_ready=false"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn replay_parity_accepts_runtime_and_event_strict_readiness() {
        let path = write_parity_fixture(
            "ready",
            r#"{
              "decision": "continue",
              "blocking_risk_flags": [],
              "runtime_evidence_comparison": {"strict_parity_ready": true},
              "event_comparison": {"strict_parity_ready": true}
            }"#,
        );

        let (ready, evidence) = replay_parity_evidence(&path);

        assert!(ready, "{evidence}");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn replay_parity_prefers_runtime_event_comparison_over_legacy_event_summary() {
        let path = write_parity_fixture(
            "runtime-event-ready",
            r#"{
              "decision": "continue",
              "blocking_risk_flags": [],
              "runtime_evidence_comparison": {
                "strict_parity_ready": true,
                "events": {"strict_parity_ready": true}
              },
              "event_comparison": {"strict_parity_ready": false}
            }"#,
        );

        let (ready, evidence) = replay_parity_evidence(&path);

        assert!(ready, "{evidence}");
        assert!(evidence.contains("runtime_ready=true"));
        assert!(evidence.contains("event_ready=true"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn prediction_prior_matches_snapshot_contract_hash_not_content_hash() {
        let contract_hash = format!("sha256:{}", "a".repeat(64));
        let content_hash = "b".repeat(64);

        assert!(
            validate_prediction_snapshot_contract_id(&contract_hash, Some(&contract_hash)).is_ok()
        );
        assert!(
            validate_prediction_snapshot_contract_id(&content_hash, Some(&contract_hash)).is_err()
        );
        assert!(validate_prediction_snapshot_contract_id(&contract_hash, None).is_err());
    }

    #[test]
    fn evaluator_report_set_is_content_addressed_stable_and_rejects_tampering() {
        #[derive(serde::Serialize)]
        struct ArtifactFixture<'a> {
            schema_version: &'static str,
            #[serde(flatten)]
            context: ReportArtifactContext<'a>,
            value: f64,
        }

        let root = std::env::temp_dir().join(format!(
            "monday-prediction-evaluator-reports-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock after epoch")
                .as_nanos()
        ));
        let start = Utc.timestamp_opt(0, 0).single().unwrap();
        let context = ReportArtifactContext {
            mission_id: "mission-1",
            search_policy_snapshot_id: "policy-1",
            snapshot_hash: "snapshot-1",
            snapshot_contract_hash: "contract-1",
            start: &start,
            end: &start,
            time_cohort_boundary_ms: Some(1_700_000_500_000),
            event_window_secs: Some(300),
        };
        let artifact = |schema_version: &'static str, value: f64| ArtifactFixture {
            schema_version,
            context,
            value,
        };
        let settlement = artifact("monday.polymarket.settlement_baseline.v1", 1.0);
        let up = artifact("monday.polymarket.full_depth_execution.v1", 2.0);
        let down = artifact("monday.polymarket.full_depth_execution.v1", 3.0);

        let first = write_report_set(&root, &settlement, &up, &down).expect("first report set");
        let expected_prefixes = [
            "settlement-baseline-",
            "full-depth-execution-up-",
            "full-depth-execution-down-",
        ];
        assert_eq!(fs::read_dir(&root).expect("report directory").count(), 3);
        let first_bytes = first
            .iter()
            .map(|path| fs::read(path).expect("read first report"))
            .collect::<Vec<_>>();
        for ((path, prefix), bytes) in first.iter().zip(expected_prefixes).zip(first_bytes.iter()) {
            let digest = format!("{:x}", Sha256::digest(bytes));
            let json: serde_json::Value =
                serde_json::from_slice(bytes).expect("parse report artifact");
            assert_eq!(
                path.file_name()
                    .expect("report filename")
                    .to_str()
                    .expect("UTF-8 report filename"),
                format!("{prefix}{digest}.json")
            );
            assert_eq!(json["mission_id"], "mission-1");
            assert_eq!(json["search_policy_snapshot_id"], "policy-1");
        }

        let second = write_report_set(&root, &settlement, &up, &down).expect("stable report set");
        assert_eq!(second, first);
        assert_eq!(fs::read_dir(&root).expect("report directory").count(), 3);
        for (path, expected) in second.iter().zip(first_bytes) {
            assert_eq!(fs::read(path).expect("read stable report"), expected);
        }

        let changed_up = artifact("monday.polymarket.full_depth_execution.v1", 4.0);
        let changed_error = write_report_set(&root, &settlement, &changed_up, &down)
            .expect_err("different digest sibling must fail closed");
        assert!(changed_error.contains("different digest sibling"));
        assert_eq!(fs::read_dir(&root).expect("report directory").count(), 3);

        fs::write(&first[1], b"tampered\n").expect("tamper hash-named report");
        write_report_set(&root, &settlement, &up, &down)
            .expect_err("existing hash-named path with different bytes must fail closed");
        assert_eq!(fs::read_dir(&root).expect("report directory").count(), 3);

        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let non_finite =
                serde_json::to_value(artifact("test", value)).expect("serialize non-finite");
            assert!(non_finite["value"].is_null(), "value={value}");
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn evaluator_report_writer_gate_requires_snapshot_and_mission_identity() {
        for (snapshot, contract, mission, policy) in [
            ("", Some("contract"), Some("mission"), Some("policy")),
            ("snapshot", None, Some("mission"), Some("policy")),
            ("snapshot", Some("contract"), None, Some("policy")),
            ("snapshot", Some("contract"), Some("mission"), None),
        ] {
            assert!(require_report_identity(snapshot, contract, mission, policy).is_err());
        }
    }

    #[test]
    fn evaluator_report_mode_rejects_empty_observations() {
        assert!(validate_report_observation_count(Some("reports"), 0).is_err());
        assert!(validate_report_observation_count(Some("reports"), 1).is_ok());
        assert!(validate_report_observation_count(None, 0).is_ok());
    }

    #[test]
    fn pipeline_smoke_report_is_content_addressed_and_excludes_activation_claims() {
        let root = tempfile::tempdir().expect("create private temporary report directory");
        let path = write_pipeline_smoke_report(
            root.path(),
            "mission-1",
            "settlement_probability",
            "0123456789abcdef",
            "sha256:contract",
            "sha256:policy",
        )
        .expect("write mechanical compatibility report");

        let bytes = fs::read(&path).expect("read pipeline smoke report");
        let report: serde_json::Value = serde_json::from_slice(&bytes).expect("parse report");
        assert_eq!(report["status"], "completed");
        assert_eq!(report["task"], "settlement_probability");
        assert_eq!(
            report["claims_excluded"],
            serde_json::json!([
                "alpha",
                "profitability",
                "paper",
                "shadow",
                "live",
                "promotion"
            ])
        );
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some(format!("pipeline-smoke-{:x}.json", Sha256::digest(&bytes)).as_str())
        );
    }

    #[test]
    fn pipeline_smoke_rejects_any_non_smoke_evaluator_flag() {
        let args = vec![
            "monday-prediction-evaluator".to_string(),
            "--pipeline-smoke-task".to_string(),
            "settlement_probability".to_string(),
            "--snapshot-dir".to_string(),
            "snapshot".to_string(),
            "--mission-id".to_string(),
            "mission".to_string(),
            "--snapshot-hash".to_string(),
            "0123456789abcdef".to_string(),
            "--snapshot-contract-id".to_string(),
            "sha256:contract".to_string(),
            "--expected-search-policy-snapshot-id".to_string(),
            "sha256:policy".to_string(),
            "--report-output-dir".to_string(),
            "reports".to_string(),
            "--prediction-mcts-selected-candidate-json".to_string(),
            "candidate.json".to_string(),
        ];

        assert!(validate_pipeline_smoke_args(&args)
            .expect_err("pipeline smoke must reject MCTS input")
            .contains("only accepts"));
    }

    #[test]
    fn pipeline_smoke_rejects_execution_tasks_until_their_admission_capability_exists() {
        let args = vec![
            "monday-prediction-evaluator".to_string(),
            "--pipeline-smoke-task".to_string(),
            "up_execution:10".to_string(),
            "--snapshot-dir".to_string(),
            "snapshot".to_string(),
            "--mission-id".to_string(),
            "mission".to_string(),
            "--snapshot-hash".to_string(),
            "0123456789abcdef".to_string(),
            "--snapshot-contract-id".to_string(),
            "sha256:contract".to_string(),
            "--expected-search-policy-snapshot-id".to_string(),
            "sha256:policy".to_string(),
            "--report-output-dir".to_string(),
            "reports".to_string(),
        ];

        assert!(validate_pipeline_smoke_args(&args)
            .expect_err("unsupported execution task must fail closed")
            .contains("not an admitted typed task"));
    }

    #[test]
    fn pipeline_smoke_rejects_promotion_flags() {
        let args = vec![
            "monday-prediction-evaluator".to_string(),
            "--pipeline-smoke-task".to_string(),
            "settlement_probability".to_string(),
            "--snapshot-dir".to_string(),
            "snapshot".to_string(),
            "--mission-id".to_string(),
            "mission".to_string(),
            "--snapshot-hash".to_string(),
            "0123456789abcdef".to_string(),
            "--snapshot-contract-id".to_string(),
            "sha256:contract".to_string(),
            "--expected-search-policy-snapshot-id".to_string(),
            "sha256:policy".to_string(),
            "--report-output-dir".to_string(),
            "reports".to_string(),
            "--promote".to_string(),
            "paper".to_string(),
        ];

        assert!(validate_pipeline_smoke_args(&args)
            .expect_err("pipeline smoke must reject promotion")
            .contains("only accepts"));
    }
}

fn alpha_search_plan_factor_names(path: &str) -> Vec<String> {
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("read alpha search plan JSON {path} failed: {err}"));
    let json: serde_json::Value = serde_json::from_str(&raw)
        .unwrap_or_else(|err| panic!("parse alpha search plan JSON {path} failed: {err}"));
    json.get("selected_nodes")
        .and_then(serde_json::Value::as_array)
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|node| node.get("factor_name"))
                .filter_map(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn read_llm_prior(path: &str) -> LlmPriorSpec {
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("read alpha search LLM prior JSON {path} failed: {err}"));
    serde_json::from_str(&raw)
        .unwrap_or_else(|err| panic!("parse alpha search LLM prior JSON {path} failed: {err}"))
}

fn read_alpha_zoo_snapshot(path: &str) -> AlphaZooSnapshot {
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("read alpha zoo snapshot JSON {path} failed: {err}"));
    serde_json::from_str(&raw)
        .unwrap_or_else(|err| panic!("parse alpha zoo snapshot JSON {path} failed: {err}"))
}

fn runtime_feedback_from_candidate_replay(path: &str) -> Option<AlphaSearchRuntimeFeedback> {
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("read candidate strategy replay JSON {path} failed: {err}"));
    let json: serde_json::Value = serde_json::from_str(&raw)
        .unwrap_or_else(|err| panic!("parse candidate strategy replay JSON {path} failed: {err}"));
    let runtime_score = json
        .get("runtime_score")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    let base_factor = runtime_score
        .strip_prefix("autofactor_formula:")
        .unwrap_or(runtime_score.as_str())
        .to_string();
    if base_factor.is_empty() {
        return None;
    }
    let diagnostics = json
        .get("strategy_diagnostics")
        .and_then(serde_json::Value::as_object);
    let counterfactual = json
        .get("score_counterfactual")
        .and_then(serde_json::Value::as_object);
    let metric = |key: &str| -> usize {
        diagnostics
            .and_then(|values| values.get(key))
            .and_then(serde_json::Value::as_u64)
            .map(|value| value as usize)
            .unwrap_or(0)
    };
    let configured_threshold = counterfactual
        .and_then(|values| values.get("configured_entry_threshold"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("0.25");
    let direct_passes = counterfactual
        .and_then(|values| values.get("direct_pass_counts"))
        .and_then(|counts| counts.get(configured_threshold))
        .or_else(|| {
            counterfactual
                .and_then(|values| values.get("direct_pass_counts"))
                .and_then(|counts| counts.get("0.25"))
        })
        .and_then(serde_json::Value::as_u64)
        .map(|value| value as usize)
        .unwrap_or_else(|| metric("settlement_autofactor_predictive_score_ge_025"));
    Some(AlphaSearchRuntimeFeedback {
        version: None,
        target: None,
        side: None,
        runtime_score,
        base_factor,
        entry_signals: metric("entry_signals"),
        direct_passes_at_configured_threshold: direct_passes,
        formula_evaluations: metric("settlement_autofactor_formula_evaluations").max(
            counterfactual
                .and_then(|values| values.get("formula_evaluations"))
                .and_then(serde_json::Value::as_u64)
                .map(|value| value as usize)
                .unwrap_or(0),
        ),
        depth_fillable: metric("settlement_autofactor_depth_fillable").max(
            counterfactual
                .and_then(|values| values.get("depth_fillable"))
                .and_then(serde_json::Value::as_u64)
                .map(|value| value as usize)
                .unwrap_or(0),
        ),
        executable_edge_pass_min_edge: metric(
            "settlement_autofactor_executable_edge_pass_min_edge",
        ),
    })
}

fn filter_autofactor_reports(
    reports: Vec<ploy_research::AutoFactorReport>,
    factor_name_filter: Option<&str>,
) -> Vec<ploy_research::AutoFactorReport> {
    let Some(filter) = factor_name_filter
        .map(str::trim)
        .filter(|filter| !filter.is_empty())
    else {
        return reports;
    };
    reports
        .into_iter()
        .filter(|report| report.name.contains(filter))
        .collect()
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if flag_present(&args, "--pipeline-smoke-task") {
        run_pipeline_smoke(&args).unwrap_or_else(|reason| {
            eprintln!("ERROR: {reason}");
            std::process::exit(2);
        });
        return;
    }
    if flag_value(&args, "--db-url").is_some() {
        eprintln!("ERROR: direct DB factor walk-forward has been removed; pass --snapshot-dir");
        std::process::exit(2);
    }
    let start = flag_value(&args, "--start-ts")
        .map(|raw| parse_timestamp(&raw))
        .unwrap_or_else(|| {
            parse_date_start(&flag_value(&args, "--start-date").expect("--start-date required"))
        });
    let end = flag_value(&args, "--end-ts")
        .map(|raw| parse_timestamp(&raw))
        .unwrap_or_else(|| {
            parse_date_end(&flag_value(&args, "--end-date").expect("--end-date required"))
        });
    let symbols: Vec<String> = flag_value(&args, "--symbols")
        .unwrap_or_else(|| "BTCUSDT".to_string())
        .split(',')
        .map(str::trim)
        .filter(|symbol| !symbol.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    let lob_sample_secs: i32 = flag_value(&args, "--lob-sample-secs")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(30);
    let pm_book_sample_secs: i32 = flag_value(&args, "--pm-book-sample-secs")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(lob_sample_secs);
    let max_quote_age_secs: i64 = flag_value(&args, "--max-quote-age-secs")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(30);
    let observation_sample_secs: i64 = flag_value(&args, "--observation-sample-secs")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(30);
    let event_window_secs = flag_value(&args, "--event-window-secs").map(|raw| {
        raw.parse::<i64>()
            .ok()
            .filter(|value| *value > 0)
            .unwrap_or_else(|| panic!("invalid --event-window-secs: {raw}"))
    });
    let review = FactorReviewOptions {
        stake_usd: flag_value(&args, "--stake-usd")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(15.0),
        min_observations: flag_value(&args, "--min-observations")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(20),
        top_quantile: flag_value(&args, "--top-quantile")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(0.2),
        max_quote_age_secs,
    };
    let options = FactorWalkForwardOptions {
        review,
        train_window_days: flag_value(&args, "--train-window-days")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(2),
        test_window_days: flag_value(&args, "--test-window-days")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(1),
        step_days: flag_value(&args, "--step-days")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(1),
        train_window_hours: flag_value(&args, "--train-window-hours")
            .filter(|raw| !raw.trim().is_empty())
            .and_then(|raw| raw.parse().ok()),
        test_window_hours: flag_value(&args, "--test-window-hours")
            .filter(|raw| !raw.trim().is_empty())
            .and_then(|raw| raw.parse().ok()),
        step_hours: flag_value(&args, "--step-hours")
            .filter(|raw| !raw.trim().is_empty())
            .and_then(|raw| raw.parse().ok()),
        top_n: flag_value(&args, "--top-n")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(20),
        factor_name_filter: flag_value(&args, "--factor-name-filter"),
    };
    let report_suite = ReportSuite::parse(flag_value(&args, "--report-suite"));

    eprintln!(
        "monday-prediction-evaluator: {} -> {} for {:?}, stake_usd={:.2}, train_window={}, test_window={}, step={}, event_window_secs={}, observation_sample_secs={}, pm_book_sample_secs={}, factor_name_filter={}, report_suite={}",
        start,
        end,
        symbols,
        options.review.stake_usd,
        options.train_window_label(),
        options.test_window_label(),
        options.step_label(),
        event_window_secs
            .map(|value| value.to_string())
            .unwrap_or_else(|| "<all>".to_string()),
        observation_sample_secs,
        pm_book_sample_secs,
        options.factor_name_filter.as_deref().unwrap_or("<none>"),
        report_suite.as_str()
    );

    let snapshot_dir = flag_value(&args, "--snapshot-dir");
    let replay_parity_json = flag_value(&args, "--replay-parity-json");
    let candidate_strategy_replay_json = flag_value(&args, "--candidate-strategy-replay-json");
    let alpha_search_output_dir = flag_value(&args, "--alpha-search-output-dir");
    let report_output_dir = flag_value(&args, "--report-output-dir");
    let mission_id = flag_value(&args, "--mission-id");
    let alpha_search_plan_json = flag_value(&args, "--alpha-search-plan-json");
    let alpha_search_llm_prior_json = flag_value(&args, "--alpha-search-llm-prior-json");
    let prediction_mcts_training_candidate_json =
        flag_value(&args, "--prediction-mcts-training-candidate-json");
    let prediction_mcts_selected_candidate_json =
        flag_value(&args, "--prediction-mcts-selected-candidate-json");
    let polymarket_chainlink_baseline = flag_present(&args, "--polymarket-chainlink-baseline");
    if prediction_mcts_training_candidate_json.is_some()
        && prediction_mcts_selected_candidate_json.is_some()
    {
        panic!("prediction MCTS evaluator accepts exactly one candidate stage");
    }
    let expected_prediction_policy = flag_value(&args, "--expected-search-policy-snapshot-id");
    let reprice_pilot_horizon_secs = match (
        flag_present(&args, "--reprice-pilot-10s"),
        flag_present(&args, "--reprice-pilot-15s"),
    ) {
        (false, false) => None,
        (true, false) => Some(10),
        (false, true) => Some(15),
        (true, true) => panic!("prediction evaluator accepts exactly one reprice pilot horizon"),
    };
    let settlement_time_cohort = settlement_time_cohort_from_args(
        &args,
        expected_prediction_policy.is_some() || reprice_pilot_horizon_secs.is_some(),
        start,
        end,
        event_window_secs,
    )
    .unwrap_or_else(|reason| panic!("prediction evaluator time cohort mismatch: {reason}"));
    let time_cohort_boundary_ms = flag_value(&args, "--time-cohort-boundary-ms").map(|raw| {
        raw.parse::<i64>()
            .expect("validated --time-cohort-boundary-ms")
    });
    if reprice_pilot_horizon_secs.is_some() {
        validate_reprice_pilot_config(
            &args,
            &RepricePilotConfig {
                symbols: &symbols,
                event_window_secs,
                lob_sample_secs,
                pm_book_sample_secs,
                observation_sample_secs,
                expected_policy: expected_prediction_policy.as_deref(),
                alpha_search_output_dir: alpha_search_output_dir.as_deref(),
                report_output_dir: report_output_dir.as_deref(),
            },
        )
        .unwrap_or_else(|reason| panic!("reprice pilot configuration mismatch: {reason}"));
    }
    let llm_prior = alpha_search_llm_prior_json.as_deref().map(read_llm_prior);
    let governed_prediction_prior = llm_prior
        .as_ref()
        .filter(|prior| prior.mission_id.is_some() || !prior.probability_blends.is_empty());
    validate_expected_prediction_policy(
        expected_prediction_policy.as_deref(),
        governed_prediction_prior,
    )
    .unwrap_or_else(|reason| panic!("prediction evaluator policy mismatch: {reason}"));
    if let Some(prior) = governed_prediction_prior {
        validate_prediction_research_prior(prior)
            .unwrap_or_else(|reason| panic!("prediction research prior is not governed: {reason}"));
        if prior.horizon.as_deref() == Some("5m") && event_window_secs != Some(300) {
            panic!(
                "governed 5m prediction research requires --event-window-secs 300, got {}",
                event_window_secs
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "<missing>".to_string())
            );
        }
    }
    if let Some(path) = alpha_search_llm_prior_json.as_deref() {
        eprintln!("alpha search typed LLM prior loaded from {path}");
    }
    let alpha_search_state_json = flag_value(&args, "--alpha-search-state-json");
    if let Some(path) = alpha_search_state_json.as_deref() {
        panic!(
            "legacy alpha search state `{path}` is not resumable; use --formula-mcts-checkpoint-json"
        );
    }
    let formula_mcts_checkpoint_json = flag_value(&args, "--formula-mcts-checkpoint-json");
    let mcts_state = formula_mcts_checkpoint_json.as_deref().map(|path| {
        read_formula_mcts_checkpoint(path)
            .unwrap_or_else(|err| panic!("read Formula MCTS checkpoint JSON {path} failed: {err}"))
    });
    if let Some(path) = formula_mcts_checkpoint_json.as_deref() {
        eprintln!("Formula MCTS checkpoint loaded from {path}");
    }
    let alpha_zoo_snapshot_json = flag_value(&args, "--alpha-zoo-snapshot-json");
    let data_quality_mode = parse_data_quality_mode(flag_value(&args, "--data-quality-mode"));
    let require_deribit = flag_present(&args, "--require-deribit");
    let min_event_complete_events = flag_value(&args, "--min-event-complete-events")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(20);
    let min_event_complete_rows = flag_value(&args, "--min-event-complete-rows")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(40);
    let min_promotion_entry_fill_rate = flag_value(&args, "--min-promotion-entry-fill-rate")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or_else(|| {
            SettlementProbabilityPromotionGateOptions::default().min_entry_fill_rate
        });
    let snapshot_dir = snapshot_dir.unwrap_or_else(|| {
        eprintln!("ERROR: --snapshot-dir is required for monday-prediction-evaluator");
        std::process::exit(2);
    });
    let snapshot_provenance: String;
    let snapshot_hash: String;
    let snapshot_contract_hash: Option<String>;
    let snapshot_data_audit_status: Option<String>;
    let snapshot_source_kind: String;
    let settlement_component_profile: SettlementProbabilityComponentProfile;
    let include_deribit: bool;
    let (observations, deribit_snapshots, all_pm_book_snapshots): (
        Vec<FactorObservation>,
        Vec<_>,
        Vec<_>,
    ) = {
        let started = std::time::Instant::now();
        let snapshot =
            load_research_snapshot(&snapshot_dir).expect("load research snapshot failed");
        snapshot_source_kind = snapshot.manifest.source_kind.clone();
        settlement_component_profile = validate_prediction_snapshot_profile(
            &snapshot_source_kind,
            polymarket_chainlink_baseline,
        )
        .unwrap_or_else(|reason| panic!("prediction snapshot profile mismatch: {reason}"));
        if expected_prediction_policy.is_some() {
            validate_prediction_snapshot_sources(&snapshot.manifest)
                .unwrap_or_else(|reason| panic!("prediction snapshot sources invalid: {reason}"));
        }
        let snapshot_symbol_set: HashSet<&str> = snapshot
            .manifest
            .symbols
            .iter()
            .map(String::as_str)
            .collect();
        let missing_symbols: Vec<&str> = symbols
            .iter()
            .map(String::as_str)
            .filter(|symbol| !snapshot_symbol_set.contains(symbol))
            .collect();
        if !missing_symbols.is_empty() {
            eprintln!(
                "ERROR: requested symbols {:?} are not present in snapshot symbols {:?}",
                missing_symbols, snapshot.manifest.symbols
            );
            std::process::exit(2);
        }
        let validation_symbols = snapshot.manifest.symbols.clone();
        validate_snapshot_request_coverage(
            &snapshot.manifest,
            ResearchSnapshotRequest {
                symbols: &validation_symbols,
                start,
                end,
                lob_sample_secs,
                pm_book_sample_secs,
                observation_sample_secs,
                max_quote_age_secs,
                stake_usd: options.review.stake_usd,
                require_official_settlement: true,
            },
        )
        .expect("snapshot does not cover requested walk-forward inputs");
        snapshot_hash = snapshot.manifest.snapshot_hash.clone().unwrap_or_default();
        snapshot_contract_hash = snapshot.manifest.snapshot_contract_hash.clone();
        eprintln!(
            "snapshot: schema={} hash={} generated_at={} observations={} deribit={} pm_books={} load_ms={}",
            snapshot.manifest.schema_version,
            if snapshot_hash.is_empty() { "<missing>" } else { &snapshot_hash },
            snapshot.manifest.generated_at,
            snapshot.observations.len(),
            snapshot.deribit_snapshots.len(),
            snapshot.pm_book_snapshots.len(),
            started.elapsed().as_millis()
        );
        snapshot_data_audit_status = snapshot.manifest.data_audit_status.clone();
        include_deribit = snapshot.manifest.include_deribit;
        snapshot_provenance = format!(
            "# Snapshot\nsnapshot_schema={}\nsnapshot_hash={}\nsnapshot_contract_hash={}\nsnapshot_generated_at={}\nsnapshot_optimizer_data_dir={}\nsnapshot_data_requirements={}\nsnapshot_data_audit_status={}\nsnapshot_data_audit_report={}\nsnapshot_include_deribit={}\n",
            snapshot.manifest.schema_version,
            if snapshot_hash.is_empty() { "<missing>" } else { &snapshot_hash },
            snapshot_contract_hash.as_deref().unwrap_or("<missing>"),
            snapshot.manifest.generated_at,
            snapshot
                .manifest
                .optimizer_data_dir
                .as_deref()
                .unwrap_or("<missing>"),
            if snapshot.manifest.data_requirements.is_empty() {
                "<unspecified>".to_string()
            } else {
                snapshot.manifest.data_requirements.join(",")
            },
            snapshot
                .manifest
                .data_audit_status
                .as_deref()
                .unwrap_or("<not-recorded>"),
            snapshot
                .manifest
                .data_audit_report
                .as_deref()
                .unwrap_or("<not-recorded>"),
            snapshot.manifest.include_deribit
        );
        let requested_symbol_set: HashSet<&str> = symbols.iter().map(String::as_str).collect();
        let mut observations: Vec<FactorObservation> = snapshot
            .observations
            .into_iter()
            .filter(|row| requested_symbol_set.contains(row.symbol.as_str()))
            .filter(|row| {
                event_window_secs.is_none_or(|expected| row.event_window_secs == expected)
            })
            .collect();
        let event_ids: HashSet<String> = observations
            .iter()
            .map(|row| row.event_id.clone())
            .collect();
        let deribit_snapshots: Vec<_> = snapshot
            .deribit_snapshots
            .into_iter()
            .filter(|row| requested_symbol_set.contains(row.symbol.as_str()))
            .collect();
        let pm_book_snapshots: Vec<_> = snapshot
            .pm_book_snapshots
            .into_iter()
            .filter(|row| event_ids.contains(&row.event_id))
            .collect();
        observations.shrink_to_fit();
        eprintln!(
            "snapshot filtered: requested_symbols={:?} event_window_secs={} observations={} deribit={} pm_books={}",
            symbols,
            event_window_secs
                .map(|value| value.to_string())
                .unwrap_or_else(|| "<all>".to_string()),
            observations.len(),
            deribit_snapshots.len(),
            pm_book_snapshots.len()
        );
        (observations, deribit_snapshots, pm_book_snapshots)
    };

    if report_output_dir.is_some() {
        require_report_identity(
            &snapshot_hash,
            snapshot_contract_hash.as_deref(),
            mission_id.as_deref(),
            expected_prediction_policy.as_deref(),
        )
        .unwrap_or_else(|reason| panic!("evaluator report identity invalid: {reason}"));
    }

    if let Some(prior) = governed_prediction_prior {
        validate_prediction_research_prior_for_source_kind(prior, &snapshot_source_kind)
            .unwrap_or_else(|reason| panic!("prediction research prior is ineligible: {reason}"));
        let prior_snapshot_id = prior
            .data_snapshot_id
            .as_deref()
            .expect("validated prediction snapshot id");
        validate_prediction_snapshot_contract_id(
            prior_snapshot_id,
            snapshot_contract_hash.as_deref(),
        )
        .unwrap_or_else(|reason| panic!("{reason}"));
        let prior_symbol = prediction_underlying_symbol(&prior.symbols[0]);
        let requested_underlyings = symbols
            .iter()
            .map(|symbol| prediction_underlying_symbol(symbol))
            .collect::<HashSet<_>>();
        if requested_underlyings.len() != 1 || !requested_underlyings.contains(&prior_symbol) {
            panic!(
                "prediction research prior symbol {} does not match requested symbols {:?}",
                prior.symbols[0], symbols
            );
        }
        if observations.is_empty() {
            panic!(
                "governed 5m prediction research found no 300s observations; legacy snapshots default event_window_secs to 0 and must be rebuilt"
            );
        }
        if observations.iter().any(|row| row.event_window_secs != 300) {
            panic!("governed 5m prediction evaluator received a non-300s observation");
        }
    }

    if observations.is_empty() {
        validate_report_observation_count(report_output_dir.as_deref(), observations.len())
            .unwrap_or_else(|reason| panic!("evaluator report output failed: {reason}"));
        eprintln!("no observations — check date range, symbols, quote coverage, and settlements");
        return;
    }

    let autofactor_rows = build_factor_observations_v2_with_deribit_and_pm_books(
        &observations,
        &deribit_snapshots,
        &all_pm_book_snapshots,
        &options.review,
    );
    let prediction_mcts_candidate = prediction_mcts_training_candidate_json
        .as_deref()
        .or(prediction_mcts_selected_candidate_json.as_deref())
        .map(|candidate_path| {
            let candidate: PredictionMctsCandidate = std::fs::read(candidate_path)
                .map_err(|error| format!("read prediction MCTS candidate: {error}"))
                .and_then(|bytes| {
                    serde_json::from_slice(&bytes)
                        .map_err(|error| format!("parse prediction MCTS candidate: {error}"))
                })
                .unwrap_or_else(|reason| {
                    panic!("prediction MCTS training input invalid: {reason}")
                });
            let prior = governed_prediction_prior
                .filter(|prior| prior.probability_blends.len() == 1)
                .unwrap_or_else(|| panic!("prediction MCTS training requires one governed blend"));
            if prior.mission_id.as_deref() != Some(candidate.identity.mission_id.as_str())
                || prior.data_snapshot_id.as_deref()
                    != Some(candidate.identity.data_snapshot_id.as_str())
                || prior.symbols.as_slice() != [candidate.identity.symbol.as_str()]
                || prior.horizon.as_deref() != Some(candidate.identity.horizon.as_str())
                || prior.probability_blends[0] != candidate.probability_blend
            {
                panic!("prediction MCTS candidate does not match governed evaluator prior");
            }
            candidate
        });
    if prediction_mcts_training_candidate_json.is_some() {
        let candidate = prediction_mcts_candidate
            .as_ref()
            .expect("training candidate was loaded");
        let prior = governed_prediction_prior
            .filter(|prior| prior.probability_blends.len() == 1)
            .expect("training candidate prior was validated");
        let training = build_settlement_training_probability_report_with_prior(
            &autofactor_rows,
            start,
            end,
            Some(prior),
            SettlementProbabilityWalkForwardOptions {
                walk_forward: options.clone(),
                probability: SettlementProbabilityReportOptions {
                    min_bucket_observations: options.review.min_observations.max(20),
                    component_profile: settlement_component_profile,
                    ..Default::default()
                },
                time_cohort: settlement_time_cohort,
                ..Default::default()
            },
        );
        let model = format!("q_llm_{}", candidate.probability_blend.name);
        let metrics = training
            .baselines
            .iter()
            .find(|row| row.model == model)
            .unwrap_or_else(|| panic!("prediction MCTS training produced no candidate metrics"));
        let evidence = PredictionMctsEvaluation {
            training_settlement: SettlementTrainingEvidence {
                candidate_id: candidate.candidate_id.clone(),
                identity: candidate.identity.clone(),
                probability_blend_sha256: candidate.probability_blend_sha256.clone(),
                training_cohort_id: training.training_cohort_id,
                event_count: training.event_count,
                mean_brier_score: metrics.brier_score,
                mean_log_loss: metrics.log_loss,
            },
            held_out_settlement: None,
            execution: None,
        };
        let output_dir = report_output_dir
            .as_deref()
            .unwrap_or_else(|| panic!("prediction MCTS training requires --report-output-dir"));
        let path = write_content_addressed_report(
            Path::new(output_dir),
            "prediction-mcts-training-evidence",
            &evidence,
        )
        .unwrap_or_else(|reason| panic!("write prediction MCTS training evidence: {reason}"));
        println!("{snapshot_provenance}");
        eprintln!(
            "prediction MCTS training complete candidate_id={} path={}",
            candidate.candidate_id,
            path.display()
        );
        return;
    }
    if let Some(horizon_secs) = reprice_pilot_horizon_secs {
        let boundary_ms = time_cohort_boundary_ms
            .expect("--reprice-pilot-10s validated --time-cohort-boundary-ms");
        let boundary = Utc
            .timestamp_millis_opt(boundary_ms)
            .single()
            .expect("--reprice-pilot-10s validated time cohort boundary");
        println!("{snapshot_provenance}");
        let paths = run_reprice_pilot(
            &autofactor_rows,
            boundary,
            horizon_secs,
            Path::new(
                alpha_search_output_dir
                    .as_deref()
                    .expect("--reprice-pilot-10s validated alpha output"),
            ),
            Path::new(
                report_output_dir
                    .as_deref()
                    .expect("--reprice-pilot-10s validated report output"),
            ),
            &snapshot_hash,
            snapshot_contract_hash.as_deref(),
            mission_id.as_deref(),
            expected_prediction_policy.as_deref(),
            &start,
            &end,
            boundary_ms,
            event_window_secs,
            options.review.top_quantile,
            options.review.min_observations,
            min_promotion_entry_fill_rate,
        )
        .unwrap_or_else(|error| panic!("run reprice pilot failed: {error}"));
        eprintln!(
            "reprice pilot complete status=pilot_not_promotable up={} down={}",
            paths[0].display(),
            paths[1].display()
        );
        return;
    }

    let report = walk_forward_factors_v2_with_deribit_and_pm_books(
        &observations,
        &deribit_snapshots,
        &all_pm_book_snapshots,
        start,
        end,
        options.clone(),
    );
    println!("{snapshot_provenance}");
    println!("{}", format_factor_walk_forward_v2_report(&report));
    let (execution_matrix, execution_event_rows) =
        build_full_depth_execution_matrix_with_event_rows(
            &observations,
            &all_pm_book_snapshots,
            FullDepthExecutionMatrixOptions {
                min_bucket_observations: options.review.min_observations.max(20),
                max_quote_age_secs,
                ..Default::default()
            },
        );
    println!(
        "{}",
        format_full_depth_execution_matrix_report(&execution_matrix, options.top_n)
    );
    let (conservative_execution_matrix, conservative_execution_event_rows) =
        build_full_depth_execution_matrix_with_event_rows(
            &observations,
            &all_pm_book_snapshots,
            FullDepthExecutionMatrixOptions {
                visible_depth_haircut: 0.5,
                max_levels: Some(3),
                min_bucket_observations: options.review.min_observations.max(20),
                max_quote_age_secs,
                ..Default::default()
            },
        );
    println!(
        "{}",
        format_full_depth_execution_matrix_report(&conservative_execution_matrix, options.top_n)
    );
    if report_suite == ReportSuite::Full {
        let repricing_ic_report = review_repricing_ic_with_deribit_and_pm_books(
            &observations,
            &deribit_snapshots,
            &all_pm_book_snapshots,
            RepricingIcOptions {
                review: options.review.clone(),
                ..Default::default()
            },
        );
        println!(
            "{}",
            format_repricing_ic_report(&repricing_ic_report, options.top_n)
        );
    }
    let settlement_probability_report = build_settlement_probability_report_with_prior(
        &autofactor_rows,
        llm_prior.as_ref(),
        SettlementProbabilityReportOptions {
            min_bucket_observations: options.review.min_observations.max(20),
            component_profile: settlement_component_profile,
            ..Default::default()
        },
    );
    println!(
        "{}",
        format_settlement_probability_report(&settlement_probability_report)
    );
    let settlement_walk_forward_options = SettlementProbabilityWalkForwardOptions {
        walk_forward: options.clone(),
        probability: SettlementProbabilityReportOptions {
            min_bucket_observations: options.review.min_observations.max(20),
            component_profile: settlement_component_profile,
            ..Default::default()
        },
        time_cohort: settlement_time_cohort,
        ..Default::default()
    };
    let settlement_probability_walk_forward_report =
        walk_forward_settlement_probability_report_with_prior(
            &autofactor_rows,
            start,
            end,
            llm_prior.as_ref(),
            settlement_walk_forward_options.clone(),
        );
    let settlement_verdict_walk_forward_report = walk_forward_settlement_verdict_report_with_prior(
        &autofactor_rows,
        start,
        end,
        llm_prior.as_ref(),
        settlement_walk_forward_options,
    );
    println!(
        "{}",
        format_settlement_probability_walk_forward_report(
            &settlement_probability_walk_forward_report
        )
    );
    let event_complete_rows = autofactor_rows
        .iter()
        .filter(|row| {
            row.label_full_depth_entry_fillable
                && row.label_conservative_entry_fillable
                && row.label_settlement_win.is_some()
                && row.label_full_depth_executable_pnl_15u.is_some()
                && row.label_conservative_executable_pnl_15u.is_some()
                && (!include_deribit || row.deribit_mark_iv.is_finite())
        })
        .collect::<Vec<_>>();
    let event_complete_events = event_complete_rows
        .iter()
        .map(|row| row.event_id.as_str())
        .collect::<HashSet<_>>()
        .len();
    let replay_parity_status = replay_parity_json.as_deref().map(replay_parity_evidence);
    let promotion_gate_report = build_settlement_probability_promotion_gate_report(
        &settlement_probability_report,
        &settlement_probability_walk_forward_report,
        &execution_matrix,
        &conservative_execution_matrix,
        SettlementProbabilityPromotionGateOptions {
            stake_usd: options.review.stake_usd,
            min_entry_fill_rate: min_promotion_entry_fill_rate,
            require_deribit,
            include_deribit,
            data_audit_status: snapshot_data_audit_status,
            data_quality_mode,
            event_complete_events,
            event_complete_rows: event_complete_rows.len(),
            min_event_complete_events,
            min_event_complete_rows,
            global_full_depth_entry_fill_rate: Some(report.health.full_depth_entry_fill_rate()),
            replay_parity_ready: replay_parity_status
                .as_ref()
                .map(|(ready, _)| *ready)
                .unwrap_or(false),
            replay_parity_evidence: replay_parity_status.map(|(_, evidence)| evidence),
            ..Default::default()
        },
    );
    println!(
        "{}",
        format_settlement_probability_promotion_gate_report(&promotion_gate_report)
    );
    if let Some(report_output_dir) = report_output_dir.as_deref() {
        write_report_artifacts(
            Path::new(report_output_dir),
            &snapshot_hash,
            snapshot_contract_hash.as_deref(),
            mission_id.as_deref(),
            expected_prediction_policy.as_deref(),
            &start,
            &end,
            time_cohort_boundary_ms,
            event_window_secs,
            &execution_matrix,
            &execution_event_rows,
            &conservative_execution_matrix,
            &conservative_execution_event_rows,
            &settlement_probability_report,
            &settlement_probability_walk_forward_report,
            &settlement_verdict_walk_forward_report,
            &promotion_gate_report,
        )
        .unwrap_or_else(|error| panic!("write evaluator report artifacts failed: {error}"));
    }
    if let (Some(output_root), Some(prior)) =
        (alpha_search_output_dir.as_deref(), llm_prior.as_ref())
    {
        if let Some(feedback) = build_prediction_research_feedback(
            prior,
            &settlement_verdict_walk_forward_report,
            promotion_gate_report.options.min_positive_window_ratio,
        ) {
            let output_dir = std::path::Path::new(output_root)
                .join(AutoFactorV2Target::FullDepthSettlementExecutablePnl.as_str());
            std::fs::create_dir_all(&output_dir).unwrap_or_else(|err| {
                panic!(
                    "create prediction research feedback directory {} failed: {err}",
                    output_dir.display()
                )
            });
            let json = serde_json::to_string_pretty(&feedback)
                .expect("serialize prediction research feedback");
            let bytes = format!("{json}\n");
            let content_hash = format!("{:x}", Sha256::digest(bytes.as_bytes()));
            let output_path =
                output_dir.join(format!("prediction-research-feedback-{content_hash}.json"));
            if output_path.exists() {
                let existing = std::fs::read(&output_path).unwrap_or_else(|err| {
                    panic!(
                        "read existing prediction research feedback {} failed: {err}",
                        output_path.display()
                    )
                });
                assert_eq!(
                    existing,
                    bytes.as_bytes(),
                    "content-addressed prediction feedback collision at {}",
                    output_path.display()
                );
            } else {
                std::fs::write(&output_path, bytes).unwrap_or_else(|err| {
                    panic!(
                        "write prediction research feedback {} failed: {err}",
                        output_path.display()
                    )
                });
            }
            eprintln!(
                "prediction research feedback written mission_id={} candidates={} path={}",
                feedback.mission_id,
                feedback.candidates.len(),
                output_path.display()
            );
        }
    }
    let autofactor_options = AutoFactorOptions {
        min_observations: options.review.min_observations.max(50),
        min_window_observations: options.review.min_observations.max(20),
        min_top_bucket_full_depth_entry_fill_rate: min_promotion_entry_fill_rate,
        ..Default::default()
    };
    let alpha_search_input_names = alpha_search_output_dir.as_ref().and_then(|_| {
        autofactor_matrix_from_v2(&autofactor_rows)
            .map(|matrix| matrix.input_names().into_iter().collect::<Vec<_>>())
            .map_err(|err| {
                eprintln!("alpha search matrix build failed: {err}");
                err
            })
            .ok()
    });
    let alpha_search_plan_names = alpha_search_plan_json
        .as_deref()
        .map(alpha_search_plan_factor_names)
        .unwrap_or_default();
    let alpha_zoo = alpha_zoo_snapshot_json
        .as_deref()
        .map(read_alpha_zoo_snapshot);
    if let Some(path) = alpha_zoo_snapshot_json.as_deref() {
        eprintln!("alpha zoo snapshot loaded from {path}");
    }
    let runtime_feedback = candidate_strategy_replay_json
        .as_deref()
        .and_then(runtime_feedback_from_candidate_replay);
    if let Some(feedback) = runtime_feedback.as_ref() {
        eprintln!(
            "alpha search runtime feedback loaded: runtime_score={} entry_signals={} direct_passes={} executable_edge_pass_min_edge={} formula_evaluations={}",
            feedback.runtime_score,
            feedback.entry_signals,
            feedback.direct_passes_at_configured_threshold,
            feedback.executable_edge_pass_min_edge,
            feedback.formula_evaluations
        );
    }
    if let Some(path) = alpha_search_plan_json.as_deref() {
        eprintln!(
            "alpha search MCTS plan loaded: {} selected nodes from {}",
            alpha_search_plan_names.len(),
            path
        );
    }
    for target in [
        AutoFactorV2Target::FullDepthRepricePnl10s(ReviewSide::Up),
        AutoFactorV2Target::FullDepthRepricePnl10s(ReviewSide::Down),
        AutoFactorV2Target::FullDepthRepricePnl15s(ReviewSide::Up),
        AutoFactorV2Target::FullDepthRepricePnl15s(ReviewSide::Down),
        AutoFactorV2Target::FullDepthRepricePnl30s(ReviewSide::Up),
        AutoFactorV2Target::FullDepthRepricePnl30s(ReviewSide::Down),
        AutoFactorV2Target::FullDepthSettlementExecutablePnl,
        AutoFactorV2Target::TradeableFullDepthSettlementPnl,
    ] {
        let target_name = target.as_str();
        let side = target.review_side();
        let plan_names = side
            .is_none()
            .then_some(alpha_search_plan_names.as_slice())
            .unwrap_or_default();
        let lane_llm_prior = side.is_none().then_some(llm_prior.as_ref()).flatten();
        match mine_domain_autofactors_from_v2_with_guidance(
            &autofactor_rows,
            target,
            &autofactor_options,
            plan_names,
            lane_llm_prior,
        ) {
            Ok(reports) => {
                let reports =
                    filter_autofactor_reports(reports, options.factor_name_filter.as_deref());
                let side_suffix = side
                    .map(|side| format!(" side={}", side.as_str()))
                    .unwrap_or_default();
                println!("# AutoFactor target={target_name}{side_suffix}");
                println!("{}", format_autofactor_reports(&reports, options.top_n));
                if let (Some(output_dir), Some(input_names)) = (
                    alpha_search_output_dir.as_deref(),
                    alpha_search_input_names.as_ref(),
                ) {
                    if side.is_none() {
                        match write_alpha_search_artifacts_with_state_and_runtime_feedback(
                            output_dir,
                            target_name,
                            input_names,
                            &reports,
                            &autofactor_options,
                            mcts_state.as_ref(),
                            runtime_feedback.as_ref(),
                            llm_prior.as_ref(),
                            alpha_zoo.as_ref(),
                        ) {
                            Ok(summary) => eprintln!(
                                "alpha search artifacts written target={} candidates={} rejected={} best={} dir={}",
                                summary.target,
                                summary.candidate_count,
                                summary.rejected_count,
                                summary.best_candidate.as_deref().unwrap_or("<none>"),
                                summary.output_dir
                            ),
                            Err(err) => eprintln!(
                                "alpha search artifact write failed for {}: {err}",
                                target_name
                            ),
                        }
                    }
                }
            }
            Err(err) => {
                eprintln!("autofactor seed report failed for {}: {err}", target_name);
            }
        }
    }
    if report_suite == ReportSuite::Core {
        return;
    }
    let fillability_report = review_fillability_v1_with_deribit_and_pm_books(
        &observations,
        &deribit_snapshots,
        &all_pm_book_snapshots,
        FillabilityReviewOptions {
            review: options.review.clone(),
            ..Default::default()
        },
    );
    println!(
        "{}",
        format_fillability_review_v1_report(&fillability_report, options.top_n)
    );
    let liquidity_gate_report = liquidity_gate_v1_with_deribit_and_pm_books(
        &observations,
        &deribit_snapshots,
        &all_pm_book_snapshots,
        LiquidityGateV1Options {
            review: options.review.clone(),
            ..Default::default()
        },
    );
    println!(
        "{}",
        format_liquidity_gate_v1_report(&liquidity_gate_report)
    );
    let gated_alpha_report = liquidity_gated_alpha_v1_with_deribit_and_pm_books(
        &observations,
        &deribit_snapshots,
        &all_pm_book_snapshots,
        start,
        end,
        LiquidityGatedAlphaV1Options {
            gate: LiquidityGateV1Options {
                review: options.review.clone(),
                ..Default::default()
            },
            walk_forward: options.clone(),
        },
    );
    println!(
        "{}",
        format_liquidity_gated_alpha_v1_report(&gated_alpha_report, options.top_n)
    );
    let formation_report = review_trade_formation_v1_with_deribit_and_pm_books(
        &observations,
        &deribit_snapshots,
        &all_pm_book_snapshots,
        TradeFormationReviewOptions {
            review: options.review.clone(),
            gate: LiquidityGateV1Options {
                review: options.review.clone(),
                ..Default::default()
            },
            top_n: options.top_n,
            ..Default::default()
        },
    );
    println!("{}", format_trade_formation_v1_report(&formation_report));
    let meta_label_report = walk_forward_meta_label_v1_with_deribit_and_pm_books(
        &observations,
        &deribit_snapshots,
        &all_pm_book_snapshots,
        start,
        end,
        MetaLabelWalkForwardOptions {
            review: options.review.clone(),
            gate: LiquidityGateV1Options {
                review: options.review.clone(),
                ..Default::default()
            },
            train_window_days: options.train_window_days,
            test_window_days: options.test_window_days,
            step_days: options.step_days,
            top_n: options.top_n,
            ..Default::default()
        },
    );
    println!(
        "{}",
        format_meta_label_walk_forward_v1_report(&meta_label_report)
    );
    let stability_report =
        build_factor_stability_report(&report, FactorStabilityOptions::default());
    println!(
        "{}",
        format_factor_stability_report(&stability_report, options.top_n)
    );
    let combo_report = walk_forward_factor_combo_v1_with_deribit_and_pm_books(
        &observations,
        &deribit_snapshots,
        &all_pm_book_snapshots,
        start,
        end,
        FactorComboV1Options {
            walk_forward: options,
            ..Default::default()
        },
    );
    println!("{}", format_factor_combo_v1_report(&combo_report));
}
