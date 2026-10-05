//! One admitted development fold: fit, independently refit, predict and replay.
use super::*;
use alpha_domain::{
    campaign_control::{
        verify_campaign_root_grant, CampaignAttemptReservationV1, SignedCampaignRootGrantV1,
        VerifiedCampaignRootGrant,
    },
    sequence_study::SequenceStudyModelV1,
};
use alpha_engine::sequence_study::{
    fit_sequence_comparison, predict_sequence_validation, SequenceEnsemble,
    SequenceValidationCoverageV1, SOL_SEQUENCE_POSITION_POLICY,
};
use chrono::Utc;
use hft_backtest::{
    config::verify_and_replay_canonical_target_positions_with_trace,
    engine::{
        TargetPositionDecision, TargetPositionReplayConfig, TargetPositionReplayMetrics,
        TargetPositionReplayTraceEvent,
    },
};
use hft_cex_research_input::sequence::SequenceReader;
use hft_research_manifest::{
    model::{HorizonHoldingPolicyV1, HorizonPositionState},
    sequence::{SequenceDatasetV1, SequenceViewV1},
};
use std::{collections::BTreeMap, fs::OpenOptions, io::BufWriter};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct GroupResult {
    pub model_kind: SequenceStudyModelV1,
    pub state: String,
    pub verified_members: usize,
    pub coverage: Option<SequenceValidationCoverageV1>,
    pub replay: Option<TargetPositionReplayMetrics>,
    /// Predeclared diagnostics. Stress fields do not change `state` or model selection.
    pub report: Option<SequenceGroupReportV1>,
    pub diagnostic: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct FoldResult {
    pub schema_version: String,
    pub campaign_id: String,
    pub request_sha256: String,
    pub root_grant_sha256: String,
    pub study_sha256: String,
    pub fold_id: u8,
    pub training_window_days: u8,
    pub position_policy: String,
    pub primary_fits_attempted: u64,
    pub verification_fits_attempted: u64,
    pub groups: Vec<GroupResult>,
    pub artifacts: BTreeMap<String, String>,
    pub sealed_holdout_opened: bool,
    pub deployment_authority: bool,
}

pub(crate) fn execute(args: CampaignExecuteArgs) -> anyhow::Result<()> {
    let request_bytes = crate::mission_dispatch::final_admission::read_bounded_json::<
        serde_json::Value,
    >(&args.request)?;
    let request: SequenceRequest = serde_json::from_value(request_bytes)?;
    request.validate()?;
    if !args.pre_holdout
        || request.build_source_revision != BUILD_SOURCE_REVISION
        || request.campaign_id != args.campaign_id
        || request.image_identity != args.image_identity
        || hft_research_artifacts::sha256_file(&args.request)? != args.request_sha256
    {
        bail!("sequence worker source, mode or request identity changed");
    }
    let input_dir = args
        .request
        .parent()
        .context("sequence request has no directory")?;
    let signed: SignedCampaignRootGrantV1 = read_json(&input_dir.join("sequence-root-grant.json"))?;
    let keys: BTreeMap<String, String> = read_json(&input_dir.join("sequence-trusted-keys.json"))?;
    let trusted = keys
        .into_iter()
        .map(|(name, key)| {
            let bytes: [u8; 32] = hex::decode(key)?
                .try_into()
                .map_err(|_| anyhow::anyhow!("invalid sequence public key"))?;
            Ok((name, ed25519_dalek::VerifyingKey::from_bytes(&bytes)?))
        })
        .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
    let grant = verify_campaign_root_grant(&signed, &trusted, Utc::now())?;
    let attempt: CampaignAttemptReservationV1 =
        read_json(&input_dir.join("sequence-attempt.json"))?;
    grant.validate_attempt_scope(&attempt, Utc::now())?;
    if attempt.request_sha256 != args.request_sha256
        || attempt.campaign_id != request.campaign_id
        || attempt.declared_trials != 14
        || attempt.policy_revision_id != request.policy_id()?
        || attempt.execution
            != crate::mission_dispatch::sequence_admission::execution_binding(
                &request,
                &grant.grant().execution.controller_image,
            )?
    {
        bail!("sequence worker differs from reserved authority");
    }
    let root = Path::new("/sequence-inputs");
    let result = run_fold(&request, &args.request_sha256, &grant, root, &args.work_dir)?;
    let result_path = args.work_dir.join("result.json");
    write_new_json(&result_path, &result)?;
    let bundle = args.work_dir.join("results.zip");
    pack_results(&args.work_dir, &bundle, &result)?;
    grant.validate_active_at(Utc::now())?;
    let client = Client::builder()
        .timeout(Duration::from_secs(300))
        .redirect(Policy::none())
        .build()?;
    publish_immutable_file(&client, &request.bundle_put_url, &bundle, "application/zip")?;
    publish_immutable_file(
        &client,
        &request.result_put_url,
        &result_path,
        "application/json",
    )?;
    for (url, path, limit) in [
        (
            &request.bundle_readback_url,
            &bundle,
            MAX_RESULT_BUNDLE_BYTES,
        ),
        (&request.result_readback_url, &result_path, 1024 * 1024),
    ] {
        let readback = args.work_dir.join(format!(
            "{}.readback",
            path.file_name().unwrap().to_string_lossy()
        ));
        fetch_to_file(&client, url, &readback, limit)?;
        if hft_research_artifacts::sha256_file(path)?
            != hft_research_artifacts::sha256_file(&readback)?
        {
            bail!("sequence publication readback differs");
        }
    }
    print_json(
        &serde_json::json!({"campaign_id":request.campaign_id,"result_sha256":hft_research_artifacts::sha256_file(&result_path)?,
        "primary_fits_attempted":result.primary_fits_attempted,"verification_fits_attempted":result.verification_fits_attempted,
        "sealed_holdout_opened":false,"state":"development_fold_complete"}),
    )
}

pub(super) fn open_reader(
    root: &Path,
    location: &inputs::DatasetLocation,
    dataset: &SequenceDatasetV1,
    view: SequenceViewV1,
) -> anyhow::Result<SequenceReader> {
    let manifest = location.manifest.path(root)?;
    SequenceReader::open(
        manifest.parent().context("dataset has no parent")?,
        dataset.clone(),
        &location.manifest.sha256,
        view,
    )
    .map_err(anyhow::Error::msg)
}

fn run_fold(
    request: &SequenceRequest,
    request_hash: &str,
    grant: &VerifiedCampaignRootGrant,
    root: &Path,
    output: &Path,
) -> anyhow::Result<FoldResult> {
    let results_dir = output.join("sequence-results");
    std::fs::create_dir_all(output)?;
    std::fs::create_dir(&results_dir).context(
        "sequence result directory must be fresh; completed artifacts are read back, not retrained",
    )?;
    request.inputs.verify_mount(root)?;
    let fold = request
        .plan
        .folds
        .iter()
        .find(|fold| {
            fold.fold_id == request.inputs.fold_id
                && fold.training_window_days == request.inputs.training_window_days
        })
        .context("missing sequence fold")?;
    let train = request
        .inputs
        .verify_view(root, &request.inputs.train, fold.train)?;
    request
        .inputs
        .verify_view(root, &request.inputs.validation, fold.validation)?;
    request.inputs.verify_replay(root, fold.validation)?;
    let mut result = FoldResult {
        schema_version: "monday.sol_sequence_fold_result.v1".into(),
        campaign_id: request.campaign_id.clone(),
        request_sha256: request_hash.into(),
        root_grant_sha256: grant.content_sha256().into(),
        study_sha256: request.plan.content_hash().map_err(anyhow::Error::msg)?,
        fold_id: fold.fold_id,
        training_window_days: fold.training_window_days,
        position_policy: SOL_SEQUENCE_POSITION_POLICY.into(),
        primary_fits_attempted: 0,
        verification_fits_attempted: 0,
        groups: Vec::new(),
        artifacts: BTreeMap::new(),
        sealed_holdout_opened: false,
        deployment_authority: false,
    };
    for kind in &request.plan.models {
        let name = serde_json::to_value(kind)?
            .as_str()
            .context("invalid model kind")?
            .to_string();
        let seeds = if *kind == SequenceStudyModelV1::Ridge {
            vec![0]
        } else {
            request.plan.neural_seeds.clone()
        };
        let mut members = Vec::new();
        let mut failure = None;
        for seed in seeds {
            grant.validate_active_at(Utc::now())?;
            research_event(
                "alpha-harness",
                "sequence_fit_started",
                serde_json::json!({"campaign_id":request.campaign_id,"model":kind,"seed":seed,"purpose":"primary"}),
            );
            let mut reader = open_reader(root, &request.inputs.train, &train, fold.train)?;
            result.primary_fits_attempted += 1;
            let fitted = match fit_sequence_comparison(
                &request.plan,
                fold.fold_id,
                fold.training_window_days,
                *kind,
                seed,
                &mut reader,
            ) {
                Ok(model) => model,
                Err(error) => {
                    failure = Some(error.chars().take(2048).collect());
                    continue;
                }
            };
            drop(reader);
            grant.validate_active_at(Utc::now())?;
            let mut reader = open_reader(root, &request.inputs.train, &train, fold.train)?;
            result.verification_fits_attempted += 1;
            research_event(
                "alpha-harness",
                "sequence_fit_started",
                serde_json::json!({"campaign_id":request.campaign_id,"model":kind,"seed":seed,"purpose":"verification"}),
            );
            let verified = match fit_sequence_comparison(
                &request.plan,
                fold.fold_id,
                fold.training_window_days,
                *kind,
                seed,
                &mut reader,
            ) {
                Ok(model) => model,
                Err(error) => {
                    failure = Some(error.chars().take(2048).collect());
                    continue;
                }
            };
            if fitted.fitted_content_sha256().map_err(anyhow::Error::msg)?
                != verified
                    .fitted_content_sha256()
                    .map_err(anyhow::Error::msg)?
            {
                failure = Some("independent_fitted_value_mismatch".into());
                continue;
            }
            let (metadata, weights) = fitted.bundle().map_err(anyhow::Error::msg)?;
            let meta_name = format!("{name}-{seed}.json");
            let weight_name = format!("{name}-{seed}.weights");
            write_new(&results_dir.join(&meta_name), &metadata)?;
            write_new(&results_dir.join(&weight_name), &weights)?;
            result
                .artifacts
                .insert(meta_name, format!("{:x}", Sha256::digest(&metadata)));
            result
                .artifacts
                .insert(weight_name, format!("{:x}", Sha256::digest(&weights)));
            members.push(fitted);
            research_event(
                "alpha-harness",
                "sequence_fit_verified",
                serde_json::json!({"campaign_id":request.campaign_id,"model":kind,"seed":seed}),
            );
        }
        let group = GroupResult {
            model_kind: *kind,
            state: "fit_failed".into(),
            verified_members: members.len(),
            coverage: None,
            replay: None,
            report: None,
            diagnostic: failure,
        };
        if group.diagnostic.is_some() {
            // Recheck inputs before continuing another independent model family.
            let mut integrity = open_reader(root, &request.inputs.train, &train, fold.train)?;
            integrity.finish_pass().map_err(anyhow::Error::msg)?;
            result.groups.push(group);
            continue;
        }
        let ensemble = SequenceEnsemble::new(&request.plan, members).map_err(anyhow::Error::msg)?;
        grant.validate_active_at(Utc::now())?;
        let evaluated = evaluate_group(
            request,
            root,
            &results_dir,
            &ensemble,
            *kind,
            &mut result.artifacts,
        )?;
        result.groups.push(evaluated);
    }
    Ok(result)
}

pub(super) fn evaluate_group(
    request: &SequenceRequest,
    root: &Path,
    results_dir: &Path,
    ensemble: &SequenceEnsemble,
    kind: SequenceStudyModelV1,
    artifacts: &mut BTreeMap<String, String>,
) -> anyhow::Result<GroupResult> {
    let name = serde_json::to_value(kind)?
        .as_str()
        .context("invalid model kind")?
        .to_owned();
    let fold = request
        .plan
        .folds
        .iter()
        .find(|fold| {
            fold.fold_id == request.inputs.fold_id
                && fold.training_window_days == request.inputs.training_window_days
        })
        .context("missing fold")?;
    let validation =
        request
            .inputs
            .verify_view(root, &request.inputs.validation, fold.validation)?;
    let mut group = GroupResult {
        model_kind: kind,
        state: "pending".into(),
        verified_members: ensemble.members().len(),
        coverage: None,
        replay: None,
        report: None,
        diagnostic: None,
    };
    let predictions_name = format!("{name}-predictions.jsonl");
    let mut predictions = BufWriter::new(
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(results_dir.join(&predictions_name))?,
    );
    let mut decisions = Vec::new();
    let mut moments = [HorizonMoments::default(); 3];
    let holding = HorizonHoldingPolicyV1 {
        horizon_millis: 30000,
    };
    let mut position = HorizonPositionState::default();
    let mut reader = open_reader(
        root,
        &request.inputs.validation,
        &validation,
        fold.validation,
    )?;
    let decision_policy = ensemble
        .decision_policy(&request.plan)
        .map_err(anyhow::Error::msg)?;
    let coverage = predict_sequence_validation(&request.plan, ensemble, &mut reader, |row| {
        for (moment, (predicted, observed)) in moments
            .iter_mut()
            .zip(row.predicted_returns.into_iter().zip(row.observed_returns))
        {
            moment.observe(predicted, f64::from(observed))?;
        }
        let opening = decision_policy.entry_decision(&row)?;
        let entry_target = opening.entry_target.unwrap_or(0.0);
        let action = position.advance(
            &holding,
            opening
                .timestamp_us
                .try_into()
                .map_err(|_| "negative sequence clock")?,
            true,
            Some(entry_target),
        )?;
        decisions.push(TargetPositionDecision {
            timestamp_us: opening.timestamp_us,
            entry_target: Some(entry_target),
            target_position: action.target(),
        });
        serde_json::to_writer(&mut predictions, &row).map_err(|e| e.to_string())?;
        predictions.write_all(b"\n").map_err(|e| e.to_string())
    })
    .map_err(anyhow::Error::msg)?;
    predictions.flush()?;
    artifacts.insert(
        predictions_name.clone(),
        hft_research_artifacts::sha256_file(&results_dir.join(predictions_name))?,
    );
    group.coverage = Some(coverage.clone());
    if !coverage.complete_decision_grid {
        group.state = "incomplete_decision_grid".into();
        return Ok(group);
    }
    group.report = Some(prediction_report(&moments)?);
    let mut next = decisions
        .last()
        .context("complete grid has no decisions")?
        .timestamp_us
        .checked_add(1_000_000)
        .context("tail clock overflow")?;
    let end_us = fold
        .validation
        .end_ms
        .checked_mul(1000)
        .context("end clock overflow")?;
    while next < end_us {
        let action = position
            .advance(&holding, next.try_into()?, false, None)
            .map_err(anyhow::Error::msg)?;
        decisions.push(TargetPositionDecision {
            timestamp_us: next,
            entry_target: Some(0.0),
            target_position: action.target(),
        });
        next = next.checked_add(1_000_000).context("tail clock overflow")?;
    }
    let (state, replay, diagnostic) = replay_group(
        root,
        &request.inputs.replay_artifact,
        &request.inputs.replay_manifest,
        &request.plan.costs,
        end_us,
        &decisions,
        &name,
        results_dir,
        artifacts,
        group.report.as_mut().context("missing sequence report")?,
    )?;
    group.state = state;
    group.replay = replay;
    group.diagnostic = diagnostic;
    Ok(group)
}

/// Both study protocols use the same native IOC execution and economic gates.
#[allow(clippy::too_many_arguments)]
pub(crate) fn replay_group(
    root: &Path,
    replay_artifact: &inputs::Artifact,
    replay_manifest: &inputs::Artifact,
    costs: &alpha_domain::EvaluationCostsV1,
    end_us: i64,
    decisions: &[TargetPositionDecision],
    name: &str,
    results_dir: &Path,
    artifacts: &mut BTreeMap<String, String>,
    report: &mut SequenceGroupReportV1,
) -> anyhow::Result<(String, Option<TargetPositionReplayMetrics>, Option<String>)> {
    let policy =
        alpha_domain::CexEventReplayPolicyV1::controlled_v2("sol-sequence-30s-ioc-v1", 5, 1000)?;
    let config = TargetPositionReplayConfig {
        holding: Some(HorizonHoldingPolicyV1 {
            horizon_millis: 30000,
        }),
        market: "usdm".into(),
        max_depth_levels: 5,
        max_decision_delay_us: policy.max_decision_delay_millis * 1000,
        order_latency_us: policy.order_latency_millis * 1000,
        position_notional_usd: costs.position_notional_usd,
        fee_bps: costs.fee_bps,
        rebate_bps: costs.rebate_bps,
        funding_bps: costs.funding_bps,
        // This is an explicit additional conservative charge, separate from
        // the observed order-arrival price; retained in the replay receipt.
        latency_bps: costs.latency_bps,
        additional_slippage_bps: costs.slippage_bps,
        cross_spread: true,
        capacity_depth_levels: costs.capacity_depth_levels,
        // The encoder consumes verified aggTrade. Matching uses the
        // canonical L2 parquet; it does not contain an execution trade tape.
        trade_tape_declared: false,
    };
    let replay = verify_and_replay_canonical_target_positions_with_trace(
        &replay_artifact.path(root)?,
        &replay_manifest.path(root)?,
        &replay_artifact.sha256,
        &replay_manifest.sha256,
        None,
        Some(end_us),
        decisions,
        &config,
    );
    match replay {
        Ok((evidence, replay)) => {
            if evidence.symbol != "SOLUSDT" || evidence.market != "usdm" {
                bail!("sequence replay instrument changed");
            }
            let trace_name = format!("{name}-replay.jsonl");
            write_new(&results_dir.join(&trace_name), &replay.trace_bytes)?;
            artifacts.insert(trace_name, replay.metrics.trace_sha256.clone());
            let config_name = format!("{name}-replay-config.json");
            write_new_json(&results_dir.join(&config_name), &config)?;
            artifacts.insert(
                config_name.clone(),
                hft_research_artifacts::sha256_file(&results_dir.join(config_name))?,
            );
            report.blocks = blocks_from_trace(&replay.trace_bytes, costs.position_notional_usd)?;
            report.fees = Some(SequenceFeeBreakdownV1 {
                total_fees: replay.metrics.total_fees,
                total_funding_cost: replay.metrics.total_funding_cost,
                total_execution_cost: replay.metrics.total_execution_cost,
                executed_turnover: replay.metrics.executed_turnover,
            });
            report.largest_block_abs_net_return_share = block_concentration(&report.blocks);
            report.stresses = diagnostic_stresses(
                root,
                replay_artifact,
                replay_manifest,
                end_us,
                decisions,
                &config,
            )?;
            let metrics = replay.metrics;
            Ok((
                classify_replay(&metrics, costs.max_book_depth_fraction).into(),
                Some(metrics),
                None,
            ))
        }
        Err(error) => Ok((
            "replay_failed".into(),
            None,
            Some(format!("{error:#}").chars().take(2048).collect()),
        )),
    }
}

pub(crate) fn classify_replay(
    metrics: &TargetPositionReplayMetrics,
    max_depth_fraction: f64,
) -> &'static str {
    let Some(holding) = &metrics.holding else {
        return "unverified_holding";
    };
    if !metrics.cumulative_net_return.is_finite() || !metrics.max_drawdown.is_finite() {
        return "invalid_accounting";
    }
    if holding.incomplete_exit_orders > 0
        || holding.delayed_exit_decisions > 0
        || !metrics.final_inventory.is_finite()
        || metrics.final_inventory.abs() > 1e-8
    {
        return "execution_gate_failed";
    }
    if holding.closed_episodes == 0 {
        return if metrics.order_count == 0 {
            "no_trades_after_costs"
        } else {
            "no_executed_trades"
        };
    }
    if metrics.displayed_depth_unavailable
        || metrics
            .max_same_side_depth_fraction
            .is_none_or(|fraction| fraction > max_depth_fraction)
        || holding.incomplete_exit_orders > 0
        || holding.delayed_exit_decisions > 0
    {
        return "execution_gate_failed";
    }
    if metrics.cumulative_net_return <= 0.0 {
        return "negative_net_return";
    }
    if holding.closed_episodes < 30 || metrics.max_drawdown > 0.05 {
        return "positive_net_insufficient_evidence";
    }
    "development_candidate"
}

const SEQUENCE_REPORT_SCHEMA: &str = "monday.sol_sequence_group_report.v1";
const BLOCK_US: i64 = 6 * 3_600 * 1_000_000;
const HORIZON_MS: [u32; 3] = [5_000, 10_000, 30_000];
pub(crate) const REPORT_NOTE: &str = "Seeds 7 and 11 stay equal-weight and are not a confidence interval. Six-hour block net return is realized cash PnL divided by notional and attributed to the entry block; fees inside a block are those charged on its decisions. cost_plus_50pct and slower_latency_1000ms replay the same entry tape and do not select or retune a model. Canonical replay is L2 IOC taker execution and does not simulate a maker queue.";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SequenceHorizonDiagnosticV1 {
    pub horizon_ms: u32,
    pub count: u64,
    pub mean_absolute_error: f64,
    pub rmse: f64,
    pub mean_absolute_prediction: f64,
    pub mean_absolute_observed: f64,
    pub pearson: Option<f64>,
    pub prediction_std: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SequenceBlockPnlV1 {
    pub block_start_us: i64,
    pub block_us: i64,
    pub net_return: f64,
    pub fees: f64,
    pub funding_cost: f64,
    pub execution_cost: f64,
    pub filled_orders: u64,
    pub closed_episodes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SequenceFeeBreakdownV1 {
    pub total_fees: f64,
    pub total_funding_cost: f64,
    pub total_execution_cost: f64,
    pub executed_turnover: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SequenceStressReplayV1 {
    pub name: String,
    pub cumulative_net_return: Option<f64>,
    pub total_fees: Option<f64>,
    pub total_funding_cost: Option<f64>,
    pub total_execution_cost: Option<f64>,
    pub order_count: Option<u64>,
    pub closed_episodes: Option<u64>,
    pub final_inventory: Option<f64>,
    pub max_drawdown: Option<f64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SequenceGroupReportV1 {
    pub schema_version: String,
    pub horizons: Vec<SequenceHorizonDiagnosticV1>,
    pub blocks: Vec<SequenceBlockPnlV1>,
    pub fees: Option<SequenceFeeBreakdownV1>,
    pub stresses: Vec<SequenceStressReplayV1>,
    pub largest_block_abs_net_return_share: Option<f64>,
    pub uncertainty_note: String,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct HorizonMoments {
    count: u64,
    sum_prediction: f64,
    sum_observed: f64,
    sum_prediction_sq: f64,
    sum_observed_sq: f64,
    sum_product: f64,
    sum_abs_error: f64,
    sum_sq_error: f64,
    sum_abs_prediction: f64,
    sum_abs_observed: f64,
}

impl HorizonMoments {
    pub(crate) fn observe(&mut self, predicted: f64, observed: f64) -> Result<(), String> {
        if !predicted.is_finite() || !observed.is_finite() {
            return Err("non-finite sequence prediction diagnostic".into());
        }
        let error = predicted - observed;
        self.count += 1;
        self.sum_prediction += predicted;
        self.sum_observed += observed;
        self.sum_prediction_sq += predicted * predicted;
        self.sum_observed_sq += observed * observed;
        self.sum_product += predicted * observed;
        self.sum_abs_error += error.abs();
        self.sum_sq_error += error * error;
        self.sum_abs_prediction += predicted.abs();
        self.sum_abs_observed += observed.abs();
        Ok(())
    }

    pub(crate) fn finish(&self, horizon_ms: u32) -> Result<SequenceHorizonDiagnosticV1, String> {
        if self.count == 0 {
            return Err("sequence diagnostic has no predictions".into());
        }
        let n = self.count as f64;
        let variance = |sum: f64, sum_sq: f64| {
            let value = sum_sq - sum * sum / n;
            if value < 0.0 && value > -1e-12 {
                Ok(0.0)
            } else if value.is_finite() && value >= 0.0 {
                Ok(value)
            } else {
                Err("sequence diagnostic variance is not finite".to_string())
            }
        };
        let prediction_variance = variance(self.sum_prediction, self.sum_prediction_sq)?;
        let observed_variance = variance(self.sum_observed, self.sum_observed_sq)?;
        let covariance = self.sum_product - self.sum_prediction * self.sum_observed / n;
        let pearson = if prediction_variance > 0.0 && observed_variance > 0.0 {
            let value = covariance / (prediction_variance.sqrt() * observed_variance.sqrt());
            value.is_finite().then_some(value)
        } else {
            None
        };
        Ok(SequenceHorizonDiagnosticV1 {
            horizon_ms,
            count: self.count,
            mean_absolute_error: self.sum_abs_error / n,
            rmse: (self.sum_sq_error / n).sqrt(),
            mean_absolute_prediction: self.sum_abs_prediction / n,
            mean_absolute_observed: self.sum_abs_observed / n,
            pearson,
            prediction_std: (prediction_variance > 0.0).then_some((prediction_variance / n).sqrt()),
        })
    }
}

fn prediction_report(moments: &[HorizonMoments; 3]) -> anyhow::Result<SequenceGroupReportV1> {
    let mut horizons = Vec::with_capacity(3);
    for (moment, horizon_ms) in moments.iter().zip(HORIZON_MS) {
        horizons.push(moment.finish(horizon_ms).map_err(anyhow::Error::msg)?);
    }
    Ok(SequenceGroupReportV1 {
        schema_version: SEQUENCE_REPORT_SCHEMA.into(),
        horizons,
        blocks: Vec::new(),
        fees: None,
        stresses: Vec::new(),
        largest_block_abs_net_return_share: None,
        uncertainty_note: REPORT_NOTE.into(),
    })
}

struct OpenEpisode {
    start_cash: f64,
    entry_us: i64,
    fees: f64,
    funding_cost: f64,
    execution_cost: f64,
    filled_orders: u64,
}

fn block_slot(
    blocks: &mut BTreeMap<i64, SequenceBlockPnlV1>,
    timestamp_us: i64,
) -> anyhow::Result<&mut SequenceBlockPnlV1> {
    let start = timestamp_us
        .div_euclid(BLOCK_US)
        .checked_mul(BLOCK_US)
        .context("sequence block clock overflow")?;
    if blocks.len() >= 512 && !blocks.contains_key(&start) {
        bail!("sequence block report exceeds its fixed time budget");
    }
    Ok(blocks.entry(start).or_insert(SequenceBlockPnlV1 {
        block_start_us: start,
        block_us: BLOCK_US,
        net_return: 0.0,
        fees: 0.0,
        funding_cost: 0.0,
        execution_cost: 0.0,
        filled_orders: 0,
        closed_episodes: 0,
    }))
}

fn blocks_from_trace(trace: &[u8], notional: f64) -> anyhow::Result<Vec<SequenceBlockPnlV1>> {
    if !notional.is_finite() || notional <= 0.0 {
        bail!("sequence block accounting lacks a positive notional");
    }
    let mut blocks = BTreeMap::<i64, SequenceBlockPnlV1>::new();
    let mut last_flat_cash = notional;
    let mut open: Option<OpenEpisode> = None;
    for line in trace.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        let event: TargetPositionReplayTraceEvent = serde_json::from_slice(line)
            .context("sequence replay trace is not the canonical event schema")?;
        let flat = event.inventory_after.abs() <= 1e-8;
        if open.is_none() && !flat {
            open = Some(OpenEpisode {
                start_cash: last_flat_cash,
                entry_us: event.decision_timestamp_us,
                fees: 0.0,
                funding_cost: 0.0,
                execution_cost: 0.0,
                filled_orders: 0,
            });
        }
        if open.is_some() {
            {
                let episode = open.as_mut().context("open sequence episode")?;
                episode.fees += event.fees;
                episode.funding_cost += event.funding_cost;
                episode.execution_cost += event.execution_cost;
                if event.filled_quantity > 0.0 {
                    episode.filled_orders += 1;
                }
            }
            if flat {
                let finished = open.take().context("open sequence episode")?;
                let pnl = event.cash_after - finished.start_cash;
                if !pnl.is_finite() {
                    bail!("non-finite sequence episode pnl");
                }
                let block = block_slot(&mut blocks, finished.entry_us)?;
                block.net_return += pnl / notional;
                block.fees += finished.fees;
                block.funding_cost += finished.funding_cost;
                block.execution_cost += finished.execution_cost;
                block.filled_orders += finished.filled_orders;
                block.closed_episodes += 1;
                last_flat_cash = event.cash_after;
            }
        } else if flat {
            last_flat_cash = event.cash_after;
            if event.fees != 0.0
                || event.funding_cost != 0.0
                || event.execution_cost != 0.0
                || event.filled_quantity > 0.0
            {
                let block = block_slot(&mut blocks, event.decision_timestamp_us)?;
                block.fees += event.fees;
                block.funding_cost += event.funding_cost;
                block.execution_cost += event.execution_cost;
                if event.filled_quantity > 0.0 {
                    block.filled_orders += 1;
                }
            }
        }
    }
    if open.is_some() {
        bail!("sequence trace ended with an open position");
    }
    Ok(blocks.into_values().collect())
}

fn block_concentration(blocks: &[SequenceBlockPnlV1]) -> Option<f64> {
    let total = blocks
        .iter()
        .map(|block| block.net_return.abs())
        .sum::<f64>();
    let largest = blocks
        .iter()
        .map(|block| block.net_return.abs())
        .fold(0.0, f64::max);
    (total > 0.0 && largest.is_finite() && total.is_finite()).then_some(largest / total)
}

fn diagnostic_stresses(
    root: &Path,
    replay_artifact: &inputs::Artifact,
    replay_manifest: &inputs::Artifact,
    end_us: i64,
    decisions: &[TargetPositionDecision],
    base: &TargetPositionReplayConfig,
) -> anyhow::Result<Vec<SequenceStressReplayV1>> {
    let mut costly = base.clone();
    costly.fee_bps *= 1.5;
    costly.funding_bps *= 1.5;
    costly.latency_bps *= 1.5;
    costly.additional_slippage_bps *= 1.5;
    let mut slower = base.clone();
    slower.order_latency_us = 1_000_000;
    if slower.max_decision_delay_us < slower.order_latency_us {
        slower.max_decision_delay_us = slower.order_latency_us;
    }
    let mut stresses = Vec::with_capacity(2);
    for (name, config) in [
        ("cost_plus_50pct", costly),
        ("slower_latency_1000ms", slower),
    ] {
        let replay = verify_and_replay_canonical_target_positions_with_trace(
            &replay_artifact.path(root)?,
            &replay_manifest.path(root)?,
            &replay_artifact.sha256,
            &replay_manifest.sha256,
            None,
            Some(end_us),
            decisions,
            &config,
        );
        stresses.push(match replay {
            Ok((_, output)) => SequenceStressReplayV1 {
                name: name.into(),
                cumulative_net_return: Some(output.metrics.cumulative_net_return),
                total_fees: Some(output.metrics.total_fees),
                total_funding_cost: Some(output.metrics.total_funding_cost),
                total_execution_cost: Some(output.metrics.total_execution_cost),
                order_count: Some(output.metrics.order_count as u64),
                closed_episodes: output
                    .metrics
                    .holding
                    .as_ref()
                    .map(|holding| holding.closed_episodes as u64),
                final_inventory: Some(output.metrics.final_inventory),
                max_drawdown: Some(output.metrics.max_drawdown),
                error: None,
            },
            Err(error) => SequenceStressReplayV1 {
                name: name.into(),
                cumulative_net_return: None,
                total_fees: None,
                total_funding_cost: None,
                total_execution_cost: None,
                order_count: None,
                closed_episodes: None,
                final_inventory: None,
                max_drawdown: None,
                error: Some(format!("{error:#}").chars().take(512).collect()),
            },
        });
    }
    Ok(stresses)
}

pub(crate) fn write_new(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

pub(crate) fn write_new_json(path: &Path, value: &impl Serialize) -> anyhow::Result<()> {
    write_new(path, &serde_json::to_vec_pretty(value)?)
}

fn pack_results(root: &Path, destination: &Path, result: &FoldResult) -> anyhow::Result<()> {
    pack_result_artifacts(root, destination, "sequence-results", &result.artifacts)
}

pub(crate) fn pack_result_artifacts(
    root: &Path,
    destination: &Path,
    directory: &str,
    artifacts: &BTreeMap<String, String>,
) -> anyhow::Result<()> {
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(destination)?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o600);
    for name in artifacts.keys() {
        zip.start_file(format!("{directory}/{name}"), options)?;
        std::io::copy(&mut File::open(root.join(directory).join(name))?, &mut zip)?;
    }
    zip.start_file("result.json", options)?;
    std::io::copy(&mut File::open(root.join("result.json"))?, &mut zip)?;
    zip.finish()?.sync_all()?;
    if destination.metadata()?.len() > MAX_RESULT_BUNDLE_BYTES {
        bail!("sequence results exceed the result byte budget");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sequence_native_holding_and_zero_trade_classification() {
        let holding = HorizonHoldingPolicyV1 {
            horizon_millis: 30000,
        };
        let config = TargetPositionReplayConfig {
            holding: Some(holding.clone()),
            market: "usdm".into(),
            max_depth_levels: 1,
            max_decision_delay_us: 1_100_000,
            order_latency_us: 100_000,
            position_notional_usd: 100.0,
            fee_bps: 2.0,
            rebate_bps: 0.0,
            funding_bps: 0.0,
            latency_bps: 0.0,
            additional_slippage_bps: 0.0,
            cross_spread: true,
            capacity_depth_levels: 1,
            trade_tape_declared: false,
        };
        for signal in [0.0, 1.0, -1.0] {
            let mut position = HorizonPositionState::default();
            let mut decisions = Vec::new();
            let mut tape = String::new();
            for t in 1..=31 {
                let entry = if t == 1 { signal } else { 0.0 };
                let target = position
                    .advance(&holding, t * 1_000_000, t < 2, Some(entry))
                    .unwrap()
                    .target();
                decisions.push(TargetPositionDecision {
                    timestamp_us: (t * 1_000_000) as i64,
                    entry_target: Some(entry),
                    target_position: target,
                });
                tape.push_str(&serde_json::json!({"timestamp":t*1_000_000+100_000,"sequence":t,"event":if t==1 {"snapshot"} else {"l2_update"},"bids":[[99.99,100.0]],"asks":[[100.01,100.0]]}).to_string());
                tape.push('\n');
            }
            let result = hft_backtest::engine::replay_target_positions_with_trace(
                tape.as_bytes(),
                &decisions,
                &config,
            )
            .unwrap();
            if signal == 0.0 {
                assert_eq!(
                    classify_replay(&result.metrics, 0.05),
                    "no_trades_after_costs"
                );
                assert_eq!(result.metrics.order_count, 0);
            } else {
                assert_eq!(result.metrics.holding.as_ref().unwrap().closed_episodes, 1);
                assert_eq!(result.metrics.order_count, 2);
                assert_eq!(
                    classify_replay(&result.metrics, 0.05),
                    "negative_net_return"
                );
                let mut incomplete = result.metrics.clone();
                incomplete.holding.as_mut().unwrap().closed_episodes = 0;
                incomplete.holding.as_mut().unwrap().incomplete_exit_orders = 1;
                assert_eq!(classify_replay(&incomplete, 0.05), "execution_gate_failed");
            }
        }
    }

    #[test]
    fn sequence_report_records_error_correlation_and_six_hour_pnl() {
        let mut moment = HorizonMoments::default();
        moment.observe(1.0, 1.0).unwrap();
        moment.observe(3.0, 3.0).unwrap();
        let perfect = moment.finish(30_000).unwrap();
        assert!((perfect.pearson.unwrap() - 1.0).abs() < 1e-12);
        assert!(perfect.mean_absolute_error.abs() < 1e-12);
        assert!((perfect.mean_absolute_prediction - 2.0).abs() < 1e-12);
        assert!((perfect.prediction_std.unwrap() - 1.0).abs() < 1e-12);
        let mut flat = HorizonMoments::default();
        flat.observe(1.0, 0.0).unwrap();
        flat.observe(1.0, 2.0).unwrap();
        assert!(flat.finish(5_000).unwrap().pearson.is_none());

        let event = |timestamp, inventory, cash, fees, filled| TargetPositionReplayTraceEvent {
            schema_version: hft_backtest::engine::TARGET_POSITION_REPLAY_TRACE_SCHEMA_VERSION
                .into(),
            decision_index: 0,
            decision_timestamp_us: timestamp,
            order_timestamp_us: timestamp,
            arrival_timestamp_us: timestamp,
            time_in_force: "IOC".into(),
            target_position: inventory,
            side: None,
            order_type: None,
            requested_quantity: filled,
            filled_quantity: filled,
            residual_quantity: 0.0,
            vwap: None,
            fees,
            funding_cost: 0.0,
            execution_cost: 0.0,
            cash_after: cash,
            inventory_after: inventory,
            status: "filled".into(),
            fills: Vec::new(),
        };
        let mut trace = Vec::new();
        for item in [
            event(1_000_000, 1.0, 99.0, 1.0, 1.0),
            event(31_000_000, 0.0, 101.0, 1.0, 1.0),
        ] {
            serde_json::to_writer(&mut trace, &item).unwrap();
            trace.push(b'\n');
        }
        let blocks = blocks_from_trace(&trace, 100.0).unwrap();
        assert_eq!(blocks.len(), 1);
        assert!((blocks[0].net_return - 0.01).abs() < 1e-12);
        assert!((blocks[0].fees - 2.0).abs() < 1e-12);
        assert_eq!(blocks[0].closed_episodes, 1);
        assert_eq!(blocks[0].filled_orders, 2);
        assert!((block_concentration(&blocks).unwrap() - 1.0).abs() < 1e-12);
        let mut idle = Vec::new();
        serde_json::to_writer(&mut idle, &event(1_000_000, 0.0, 100.0, 0.0, 0.0)).unwrap();
        assert!(blocks_from_trace(&idle, 100.0).unwrap().is_empty());
        assert!(blocks_from_trace(
            &serde_json::to_vec(&event(1_000_000, 1.0, 99.0, 1.0, 1.0)).unwrap(),
            100.0
        )
        .is_err());
    }
}
