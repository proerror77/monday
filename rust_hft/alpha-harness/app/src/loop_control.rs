use crate::{
    cli::{
        print_json, EngineChoice, LearnMissionArgs, LoopRunArgs, LoopStatusArgs, LoopTargetChoice,
        RecoverLegacyCheckpointArgs,
    },
    governance, mission,
};
use alpha_domain::{
    runtime_stage_is_healthy, AttributionMode, LoopCompletionPolicy, LoopRun, LoopRunStatus,
    LoopStage, LoopStageRecord, LoopStageStatus, LoopStopReason, LoopTargetStage, MissionStatus,
};
use alpha_store::{AlphaStore, StoreError};
use anyhow::{bail, Context};
use chrono::Utc;

pub fn run_loop(args: LoopRunArgs) -> anyhow::Result<()> {
    validate_loop_args(&args)?;
    let target_stage = match args.target_stage {
        LoopTargetChoice::Researching => LoopTargetStage::Researching,
        LoopTargetChoice::WalkForwardKept => LoopTargetStage::WalkForwardKept,
        LoopTargetChoice::HoldoutPassed => LoopTargetStage::HoldoutPassed,
        LoopTargetChoice::PaperHealthy => LoopTargetStage::PaperHealthy,
        LoopTargetChoice::ShadowHealthy => LoopTargetStage::ShadowHealthy,
        LoopTargetChoice::LiveSmallEligible => LoopTargetStage::LiveSmallEligible,
    };
    let mut run = load_or_create_run(&args, target_stage)?;
    if matches!(
        run.status,
        LoopRunStatus::Completed | LoopRunStatus::BudgetExhausted | LoopRunStatus::Failed
    ) {
        return print_json(&run);
    }
    if matches!(run.status, LoopRunStatus::Pending | LoopRunStatus::Paused) {
        run.start(Utc::now())?;
        persist_run(&args, &run)?;
    }

    let mut mission_ids = std::iter::once(run.root_mission_id.clone())
        .chain(run.child_mission_ids.clone())
        .collect::<Vec<_>>();
    let mut cursor = 0_usize;
    while cursor < mission_ids.len() {
        if cursor >= run.completion_policy.max_research_missions {
            stop_and_persist(
                &args,
                &mut run,
                LoopStopReason::MissionLimitReached { attempted: cursor },
            )?;
            return print_json(&run);
        }
        let mission_id = mission_ids[cursor].clone();
        cursor += 1;
        let store = AlphaStore::open(&args.mission.db)?;
        let stored_mission = store.get_mission(&mission_id)?;
        drop(store);

        let should_execute = !matches!(
            stored_mission.status,
            MissionStatus::Completed | MissionStatus::BudgetExhausted | MissionStatus::Failed
        );
        let (status, reason_text) = if should_execute {
            let mut mission_args = args.mission.clone();
            mission_args.mission_id = mission_id.clone();
            let resume = matches!(
                stored_mission.status,
                MissionStatus::Paused | MissionStatus::Running
            );
            let report = match mission::execute_mission(&mission_args, resume) {
                Ok(report) => report,
                Err(error) => {
                    let reason = format!("research execution failed: {error:#}");
                    append_stage_if_changed(
                        &args,
                        &mut run,
                        &mission_id,
                        LoopStage::Researching,
                        LoopStageStatus::Failed,
                        reason.clone(),
                    )?;
                    stop_and_persist(
                        &args,
                        &mut run,
                        LoopStopReason::MissionFailed {
                            mission_id: mission_id.clone(),
                            code: reason,
                        },
                    )?;
                    return Err(error)
                        .with_context(|| format!("loop research mission {mission_id} failed"));
                }
            };
            let status = report.status.clone();
            (
                status.clone(),
                report
                    .terminal_reason
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?
                    .unwrap_or_else(|| format!("{:?}", status)),
            )
        } else {
            (
                stored_mission.status.clone(),
                mission_reason_text(&stored_mission)?,
            )
        };

        if !should_execute
            || matches!(
                status,
                MissionStatus::Completed
                    | MissionStatus::Paused
                    | MissionStatus::BudgetExhausted
                    | MissionStatus::Failed
            )
        {
            append_stage_if_changed(
                &args,
                &mut run,
                &mission_id,
                LoopStage::Researching,
                stage_status(&status),
                reason_text.clone(),
            )?;
        }

        match status {
            MissionStatus::Paused => {
                stop_and_persist(
                    &args,
                    &mut run,
                    LoopStopReason::MissionPaused { mission_id },
                )?;
                return print_json(&run);
            }
            MissionStatus::Failed => {
                stop_and_persist(
                    &args,
                    &mut run,
                    LoopStopReason::MissionFailed {
                        mission_id,
                        code: reason_text,
                    },
                )?;
                return print_json(&run);
            }
            MissionStatus::Completed => {
                match progress_completed_mission(&args, &mut run, &mission_id)? {
                    CompletionProgress::ReachedTarget => {
                        let stage = run.completion_policy.target_stage.clone();
                        stop_and_persist(
                            &args,
                            &mut run,
                            LoopStopReason::TargetStageReached { mission_id, stage },
                        )?;
                        return print_json(&run);
                    }
                    CompletionProgress::AwaitingEvidence(stage) => {
                        stop_and_persist(
                            &args,
                            &mut run,
                            LoopStopReason::AwaitingEvidence { mission_id, stage },
                        )?;
                        return print_json(&run);
                    }
                }
            }
            MissionStatus::BudgetExhausted => {
                let outcome = close_learning_stage(&args, &mut run, &mission_id)?;
                if outcome.follow_up_mission_ids.is_empty() {
                    stop_and_persist(
                        &args,
                        &mut run,
                        LoopStopReason::ResearchBudgetExhausted { mission_id },
                    )?;
                    return print_json(&run);
                }
                for follow_up_id in outcome.follow_up_mission_ids {
                    run.add_child_mission(follow_up_id.clone())?;
                    if !mission_ids.contains(&follow_up_id) {
                        mission_ids.push(follow_up_id);
                    }
                }
                persist_run(&args, &run)?;
            }
            MissionStatus::Pending | MissionStatus::Running => {
                bail!("mission execution returned a non-stopped status")
            }
        }
    }

    stop_and_persist(
        &args,
        &mut run,
        LoopStopReason::MissionLimitReached { attempted: cursor },
    )?;
    print_json(&run)
}

pub fn loop_status(args: LoopStatusArgs) -> anyhow::Result<()> {
    let store = AlphaStore::open(args.db)?;
    print_json(&store.get_loop_run(&args.loop_run_id)?)
}

pub fn recover_legacy_checkpoint(args: RecoverLegacyCheckpointArgs) -> anyhow::Result<()> {
    let replacement = AlphaStore::open(args.db)?.fork_legacy_checkpoint(
        &args.mission_id,
        &args.replacement_mission_id,
        Utc::now(),
    )?;
    print_json(&replacement)
}

fn validate_loop_args(args: &LoopRunArgs) -> anyhow::Result<()> {
    mission::validate_live_formula_engine(args.mission.engine)?;
    if !matches!(args.mission.engine, EngineChoice::Mcts) {
        bail!(
            "durable LoopRun supports only mcts live-capable exact-resume engines; run gp, offline-rl, or llm as standalone lab missions"
        );
    }
    if args.max_research_missions == 0 {
        bail!("max_research_missions must be positive");
    }
    if args.mission.feature_fields.is_empty()
        || args
            .mission
            .feature_fields
            .iter()
            .any(|field| field.trim().is_empty())
    {
        bail!("loop feature fields are required");
    }
    mission::validate_live_feature_fields(&args.mission.feature_fields)
}

fn load_or_create_run(
    args: &LoopRunArgs,
    target_stage: LoopTargetStage,
) -> anyhow::Result<LoopRun> {
    let mut store = AlphaStore::open(&args.mission.db)?;
    match store.get_loop_run(&args.loop_run_id) {
        Ok(run) => {
            if run.root_mission_id != args.mission.mission_id
                || run.completion_policy.target_stage != target_stage
                || run.completion_policy.max_research_missions != args.max_research_missions
            {
                bail!("existing loop run does not match the requested root mission or policy");
            }
            Ok(run)
        }
        Err(StoreError::NotFound) => {
            store.get_mission(&args.mission.mission_id)?;
            let now = Utc::now();
            let run = LoopRun {
                loop_run_id: args.loop_run_id.clone(),
                root_mission_id: args.mission.mission_id.clone(),
                completion_policy: LoopCompletionPolicy {
                    target_stage,
                    max_research_missions: args.max_research_missions,
                },
                child_mission_ids: vec![],
                stage_records: vec![],
                status: LoopRunStatus::Pending,
                stop_reason: None,
                created_at: now,
                updated_at: now,
            };
            store.create_loop_run(&run)?;
            Ok(run)
        }
        Err(error) => Err(error.into()),
    }
}

fn progress_completed_mission(
    args: &LoopRunArgs,
    run: &mut LoopRun,
    mission_id: &str,
) -> anyhow::Result<CompletionProgress> {
    let research_reason = latest_stage(run, mission_id, LoopStage::Researching)
        .map(|record| record.reason.clone())
        .unwrap_or_else(|| "mission completed".to_string());
    if run.completion_policy.target_stage == LoopTargetStage::Researching {
        return Ok(CompletionProgress::ReachedTarget);
    }
    let walk_forward_candidates = governance::validated_walk_forward_candidates(
        &AlphaStore::open(&args.mission.db)?,
        mission_id,
    )?;
    if walk_forward_candidates.is_empty() {
        return Ok(CompletionProgress::AwaitingEvidence(
            LoopTargetStage::WalkForwardKept,
        ));
    }
    append_stage_if_changed(
        args,
        run,
        mission_id,
        LoopStage::WalkForwardKept,
        LoopStageStatus::Completed,
        format!(
            "{research_reason}; canonical v3 walk-forward candidates: {}",
            walk_forward_candidates.join(",")
        ),
    )?;

    if run.completion_policy.target_stage == LoopTargetStage::WalkForwardKept {
        return Ok(CompletionProgress::ReachedTarget);
    }

    let target_stage = run.completion_policy.target_stage.clone();
    let (candidate_id, events) = {
        let store = AlphaStore::open(&args.mission.db)?;
        let Some(candidate_id) = store.sealed_passed_candidate_for_mission(mission_id)? else {
            return Ok(CompletionProgress::AwaitingEvidence(
                LoopTargetStage::HoldoutPassed,
            ));
        };
        if !walk_forward_candidates.contains(&candidate_id) {
            return Ok(CompletionProgress::AwaitingEvidence(
                LoopTargetStage::HoldoutPassed,
            ));
        }
        let events = if matches!(
            target_stage,
            LoopTargetStage::PaperHealthy
                | LoopTargetStage::ShadowHealthy
                | LoopTargetStage::LiveSmallEligible
        ) {
            store.runtime_attributions_for_mission(mission_id)?
        } else {
            Vec::new()
        };
        (candidate_id, events)
    };
    append_stage_if_changed(
        args,
        run,
        mission_id,
        LoopStage::HoldoutPassed,
        LoopStageStatus::Completed,
        format!("sealed holdout passed for candidate {candidate_id}"),
    )?;
    if target_stage == LoopTargetStage::HoldoutPassed {
        return Ok(CompletionProgress::ReachedTarget);
    }

    if !runtime_stage_is_healthy(&events, &candidate_id, AttributionMode::Paper) {
        return Ok(CompletionProgress::AwaitingEvidence(
            LoopTargetStage::PaperHealthy,
        ));
    }
    append_stage_if_changed(
        args,
        run,
        mission_id,
        LoopStage::PaperHealthy,
        LoopStageStatus::Completed,
        format!("paper runtime healthy for candidate {candidate_id}"),
    )?;
    if target_stage == LoopTargetStage::PaperHealthy {
        return Ok(CompletionProgress::ReachedTarget);
    }

    if !runtime_stage_is_healthy(&events, &candidate_id, AttributionMode::Shadow) {
        return Ok(CompletionProgress::AwaitingEvidence(
            LoopTargetStage::ShadowHealthy,
        ));
    }
    append_stage_if_changed(
        args,
        run,
        mission_id,
        LoopStage::ShadowHealthy,
        LoopStageStatus::Completed,
        format!("shadow runtime healthy for candidate {candidate_id}"),
    )?;
    if target_stage == LoopTargetStage::ShadowHealthy {
        return Ok(CompletionProgress::ReachedTarget);
    }

    let Some(approval_id) = AlphaStore::open(&args.mission.db)?.live_small_eligibility_approval(
        mission_id,
        &candidate_id,
        Utc::now(),
    )?
    else {
        return Ok(CompletionProgress::AwaitingEvidence(
            LoopTargetStage::LiveSmallEligible,
        ));
    };
    append_stage_if_changed(
        args,
        run,
        mission_id,
        LoopStage::LiveSmallEligible,
        LoopStageStatus::Completed,
        format!(
            "human approval {approval_id} attests reconciliation, reduce-only exit, and shadow soak evidence for candidate {candidate_id}; runtime activation remains separately gated"
        ),
    )?;
    Ok(CompletionProgress::ReachedTarget)
}

fn close_learning_stage(
    args: &LoopRunArgs,
    run: &mut LoopRun,
    mission_id: &str,
) -> anyhow::Result<alpha_engine::learning::LearningOutcome> {
    let learning_args = LearnMissionArgs {
        db: args.mission.db.clone(),
        mission_id: mission_id.to_string(),
        repeated_failure_threshold: args.repeated_failure_threshold,
        max_critic_tokens: args.max_critic_tokens,
        llm_critic: args.llm_critic,
    };
    let outcome = match mission::execute_learning(&learning_args) {
        Ok(outcome) => outcome,
        Err(error) => {
            let reason = format!("learning execution failed: {error:#}");
            append_stage_if_changed(
                args,
                run,
                mission_id,
                LoopStage::Learning,
                LoopStageStatus::Failed,
                reason.clone(),
            )?;
            run.stop(
                LoopStopReason::MissionFailed {
                    mission_id: mission_id.to_string(),
                    code: reason,
                },
                Utc::now(),
            )?;
            persist_run(args, run)?;
            return Err(error).context("loop learning stage failed");
        }
    };
    append_stage_if_changed(
        args,
        run,
        mission_id,
        LoopStage::Learning,
        LoopStageStatus::Completed,
        format!(
            "directives={}, follow_ups={}, pinned_policies={}",
            outcome.directive_ids.len(),
            outcome.follow_up_mission_ids.len(),
            outcome.pinned_policy_revision_ids.len()
        ),
    )?;
    Ok(outcome)
}

fn latest_stage<'a>(
    run: &'a LoopRun,
    mission_id: &str,
    stage: LoopStage,
) -> Option<&'a LoopStageRecord> {
    run.stage_records
        .iter()
        .rev()
        .find(|record| record.mission_id == mission_id && record.stage == stage)
}

fn append_stage_if_changed(
    args: &LoopRunArgs,
    run: &mut LoopRun,
    mission_id: &str,
    stage: LoopStage,
    status: LoopStageStatus,
    reason: String,
) -> anyhow::Result<()> {
    if latest_stage(run, mission_id, stage.clone())
        .is_some_and(|record| record.status == status && record.reason == reason)
    {
        return Ok(());
    }
    let record_id = format!(
        "{}:{}:{}:{}",
        run.loop_run_id,
        mission_id,
        stage_slug(&stage),
        run.stage_records.len() + 1
    );
    run.append_stage(LoopStageRecord {
        record_id,
        mission_id: mission_id.to_string(),
        stage,
        status,
        reason,
        recorded_at: Utc::now(),
    })?;
    persist_run(args, run)
}

fn stop_and_persist(
    args: &LoopRunArgs,
    run: &mut LoopRun,
    reason: LoopStopReason,
) -> anyhow::Result<()> {
    run.stop(reason, Utc::now())?;
    persist_run(args, run)
}

fn persist_run(args: &LoopRunArgs, run: &LoopRun) -> anyhow::Result<()> {
    AlphaStore::open(&args.mission.db)?.save_loop_run(run)?;
    Ok(())
}

fn stage_slug(stage: &LoopStage) -> &'static str {
    match stage {
        LoopStage::Researching => "researching",
        LoopStage::WalkForwardKept => "walkforward_kept",
        LoopStage::HoldoutPassed => "holdout_passed",
        LoopStage::PaperHealthy => "paper_healthy",
        LoopStage::ShadowHealthy => "shadow_healthy",
        LoopStage::LiveSmallEligible => "live_small_eligible",
        LoopStage::Learning => "learning",
    }
}

fn stage_status(status: &MissionStatus) -> LoopStageStatus {
    match status {
        MissionStatus::Completed => LoopStageStatus::Completed,
        MissionStatus::Paused => LoopStageStatus::Paused,
        MissionStatus::BudgetExhausted => LoopStageStatus::BudgetExhausted,
        MissionStatus::Failed => LoopStageStatus::Failed,
        MissionStatus::Pending | MissionStatus::Running => LoopStageStatus::Failed,
    }
}

fn mission_reason_text(mission: &alpha_domain::ResearchMission) -> anyhow::Result<String> {
    Ok(mission
        .terminal_reason
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?
        .unwrap_or_else(|| format!("{:?}", mission.status)))
}

enum CompletionProgress {
    ReachedTarget,
    AwaitingEvidence(LoopTargetStage),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        cli::{
            DatasetArgs, EngineChoice, EvaluateArgs, FeedbackLogArgs, PromoteArgs, RunMissionArgs,
            SignDeploymentArgs, ValidationArgs,
        },
        data_mission, governance,
    };
    use alpha_domain::{
        deployment_scope_hash, sign_runtime_attribution_event, AllowedIntentType, ApprovalClass,
        AttributionKind, AttributionOutcome, CandidateArtifact, DeploymentEnvelope, EngineKind,
        EvaluationCostsV1, EvaluationLabelSpecV1, EvaluationProtocolV1, EvaluationWalkForwardV1,
        IterationVerdict, LiveSmallEligibilityEvidence, MissionTerminalReason, ResearchIteration,
        ResearchMission, RuntimeAttributionEvent, SearchBudgetUsage,
        SEALED_HOLDOUT_EVALUATOR_VERSION,
    };
    use alpha_engine::{
        evaluation::prepare_dataset,
        formula_evaluator::{FormulaEvaluator, WALK_FORWARD_EVALUATOR_VERSION},
        CandidateEvaluation, CandidateEvaluator, EngineProposal, EvaluationMetrics,
        FoldEvaluationMetrics, FoldPredictiveMetrics, PredictiveMetrics,
    };
    use alpha_store::{ApprovalRecord, EvaluationRecord, MemoryRecord, RegistryRevision};
    use chrono::Duration;
    use hft_collector::{
        CandleInterval, DatasetManifest, DatasetTimeBounds, OhlcvTraceRow, QualityReport,
    };
    use sha2::{Digest, Sha256};
    use std::{collections::BTreeMap, path::PathBuf};

    fn temp_db_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "{name}-{}-{}.duckdb",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap()
        ))
    }

    fn governed_dataset_fixture(mission_id: &str) -> (PathBuf, PathBuf, DatasetManifest) {
        let directory = std::env::temp_dir().join(format!(
            "alpha-loop-e2e-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let created_at = Utc::now();
        let start = created_at - Duration::minutes(500);
        let rows = (0..500)
            .scan(100.0_f64, |close, index| {
                let return_rate = if (index / 3) % 2 == 0 { 0.01 } else { -0.01 };
                *close *= 1.0 + return_rate;
                let close = *close;
                let open = close / (1.0 + return_rate);
                let event_time = start + Duration::minutes(i64::from(index));
                let available_time = event_time + Duration::minutes(1);
                Some(OhlcvTraceRow {
                    event_time,
                    exchange_time: available_time - Duration::milliseconds(1),
                    receive_time: created_at,
                    available_time,
                    ingestion_time: created_at,
                    source: "binance-public".to_string(),
                    schema_version: "binance-kline-v2".to_string(),
                    quality_flags: vec![],
                    symbol: "BTCUSDT".to_string(),
                    interval: CandleInterval::OneMinute,
                    open,
                    high: open.max(close) * 1.001,
                    low: open.min(close) * 0.999,
                    close,
                    volume: 1.0,
                })
            })
            .collect::<Vec<_>>();
        let mut bytes = Vec::new();
        for row in &rows {
            serde_json::to_writer(&mut bytes, row).unwrap();
            bytes.push(b'\n');
        }
        let artifact_sha256 = hex::encode(Sha256::digest(&bytes));
        let artifact_path = directory.join(format!("{artifact_sha256}.jsonl"));
        std::fs::write(&artifact_path, bytes).unwrap();
        let manifest = DatasetManifest {
            manifest_id: format!("dataset-{artifact_sha256}"),
            mission_id: mission_id.to_string(),
            source_id: "binance-public".to_string(),
            symbol: "BTCUSDT".to_string(),
            schema_version: "binance-kline-v2".to_string(),
            interval: CandleInterval::OneMinute,
            time_bounds: DatasetTimeBounds {
                first_event_time: rows.first().unwrap().event_time,
                last_event_time: rows.last().unwrap().event_time,
                last_exchange_time: rows.last().unwrap().exchange_time,
                first_receive_time: rows.first().unwrap().receive_time,
                last_receive_time: rows.last().unwrap().receive_time,
                first_available_time: rows.first().unwrap().available_time,
                last_available_time: rows.last().unwrap().available_time,
                first_ingestion_time: rows.first().unwrap().ingestion_time,
                last_ingestion_time: rows.last().unwrap().ingestion_time,
            },
            artifact_path,
            artifact_sha256,
            quality: QualityReport {
                rows: rows.len(),
                parse_failures: 0,
                non_monotonic_events: 0,
                non_finite_values: 0,
                duplicate_timestamps: 0,
                interval_gaps: 0,
                open_or_partial_candles: 0,
                point_in_time_violations: 0,
                invalid_ohlc_rows: 0,
                non_positive_price_rows: 0,
                negative_volume_rows: 0,
                latest_candle_age_millis: 1,
                max_staleness_millis: 120_000,
                stale: false,
            },
            created_at,
        };
        let manifest_path = directory.join("manifest.json");
        data_mission::write_json_atomic(&manifest_path, &manifest).unwrap();
        (directory, manifest_path, manifest)
    }

    fn persist_governed_runtime_deployment(
        db: &std::path::Path,
        directory: &std::path::Path,
        candidate_id: &str,
        mode: AttributionMode,
        now: chrono::DateTime<Utc>,
    ) -> (String, String) {
        let mut store = AlphaStore::open(db).unwrap();
        let promotion = store
            .get_promotion(&format!("promotion:{candidate_id}"))
            .unwrap();
        let bundle = store
            .get_strategy_bundle(&promotion.record.bundle_id)
            .unwrap();
        let (suffix, intent, approval_class) = match mode {
            AttributionMode::Paper => {
                ("paper", AllowedIntentType::StartPaper, ApprovalClass::Paper)
            }
            AttributionMode::Shadow => (
                "shadow",
                AllowedIntentType::StartShadow,
                ApprovalClass::Shadow,
            ),
            AttributionMode::LiveSmall => panic!("test does not activate live-small"),
        };
        let deployment_id = format!("deployment-{candidate_id}-{suffix}");
        let approval_id = format!("approval-{suffix}");
        let envelope = DeploymentEnvelope {
            deployment_id: deployment_id.clone(),
            asset_revision_id: candidate_id.to_string(),
            promotion_id: promotion.record.promotion_id.clone(),
            promotion_manifest_hash: promotion.content_hash,
            bundle_id: bundle.bundle_id.clone(),
            bundle_hash: bundle.bundle_hash,
            runtime_config_hash: "d".repeat(64),
            risk_policy_hash: "e".repeat(64),
            account_id: "account-e2e".to_string(),
            venue: "binance".to_string(),
            instruments: vec!["BTCUSDT".to_string()],
            allowed_intent_types: vec![intent],
            max_notional: 100.0,
            max_symbol_exposure: 50.0,
            max_order_size: 10.0,
            max_slippage_bps: 2.0,
            valid_from: now - Duration::minutes(1),
            expires_at: now + Duration::minutes(30),
            nonce: format!("nonce-{candidate_id}-{suffix}"),
            approval_class,
            approval_signatures: vec![approval_id.clone()],
            payload_hash: String::new(),
        };
        store
            .record_approval(&ApprovalRecord {
                approval_id,
                approval_class: suffix.to_string(),
                subject_id: promotion.record.promotion_id,
                payload: serde_json::json!({
                    "scope_hash": deployment_scope_hash(&envelope).unwrap(),
                }),
                signer_id: Some(format!("risk-officer-{suffix}")),
                valid_from: Some(now - Duration::minutes(1)),
                expires_at: Some(now + Duration::minutes(30)),
                revoked_at: None,
                revoked_by: None,
                revocation_reason: None,
                created_at: now - Duration::minutes(1),
            })
            .unwrap();
        drop(store);

        let envelope_path = directory.join(format!("{suffix}-envelope.json"));
        let signing_key_path = directory.join(format!("{suffix}-signing-key.hex"));
        let signed_path = directory.join(format!("{suffix}-signed.json"));
        data_mission::write_json_atomic(&envelope_path, &envelope).unwrap();
        std::fs::write(&signing_key_path, hex::encode([9_u8; 32])).unwrap();
        governance::sign_deployment(SignDeploymentArgs {
            db: db.to_path_buf(),
            envelope: envelope_path,
            signing_key: signing_key_path,
            key_id: "test-key".to_string(),
            output: signed_path,
        })
        .unwrap();
        (deployment_id, bundle.bundle_id)
    }

    fn runtime_health_events(
        deployment_id: &str,
        candidate_id: &str,
        strategy_id: &str,
        mode: AttributionMode,
        now: chrono::DateTime<Utc>,
    ) -> Vec<RuntimeAttributionEvent> {
        vec![
            RuntimeAttributionEvent {
                event_id: format!("{deployment_id}:activation"),
                deployment_id: deployment_id.to_string(),
                asset_revision_id: candidate_id.to_string(),
                mission_id: None,
                mode: mode.clone(),
                outcome: AttributionOutcome::Activated,
                kind: AttributionKind::Activation,
                strategy_id: None,
                order_id: None,
                account_id: None,
                venue: None,
                symbol: None,
                metrics: BTreeMap::new(),
                reason: None,
                observed_at: now,
            },
            RuntimeAttributionEvent {
                event_id: format!("{deployment_id}:portfolio"),
                deployment_id: deployment_id.to_string(),
                asset_revision_id: candidate_id.to_string(),
                mission_id: None,
                mode: mode.clone(),
                outcome: AttributionOutcome::Healthy,
                kind: AttributionKind::PortfolioSnapshot,
                strategy_id: Some(strategy_id.to_string()),
                order_id: None,
                account_id: Some("account-e2e".to_string()),
                venue: Some("binance".to_string()),
                symbol: Some("BTCUSDT".to_string()),
                metrics: BTreeMap::from([
                    ("gross_pnl_coverage_complete".to_string(), 1.0),
                    ("mark_coverage_complete".to_string(), 1.0),
                ]),
                reason: None,
                observed_at: now + Duration::seconds(1),
            },
            RuntimeAttributionEvent {
                event_id: format!("{deployment_id}:fill"),
                deployment_id: deployment_id.to_string(),
                asset_revision_id: candidate_id.to_string(),
                mission_id: None,
                mode,
                outcome: AttributionOutcome::Healthy,
                kind: AttributionKind::Fill,
                strategy_id: Some(strategy_id.to_string()),
                order_id: Some(format!("order-{deployment_id}")),
                account_id: Some("account-e2e".to_string()),
                venue: Some("binance".to_string()),
                symbol: Some("BTCUSDT".to_string()),
                metrics: BTreeMap::from([("fill_quantity".to_string(), 1.0)]),
                reason: None,
                observed_at: now + Duration::seconds(2),
            },
        ]
    }

    fn mission_fixture(now: chrono::DateTime<Utc>, mission_id: &str) -> ResearchMission {
        serde_json::from_value(serde_json::json!({
            "mission_id": mission_id,
            "objective": "verify loop orchestration",
            "hypothesis_scope": "fixture",
            "mutable_scope": ["factor_ast"],
            "dataset_manifest_id": "dataset-loop",
            "baseline_artifact_id": null,
            "validation_mode": "MissionValidator",
            "validator_spec": {},
            "search_budget": {
                "max_candidates": 1,
                "max_expansions": 1,
                "max_tokens": 0,
                "max_seconds": 1
            },
            "completion_policy": {"min_kept_candidates": 1},
            "prompt_snapshot_id": null,
            "search_policy_snapshot_id": "policy-1",
            "status": "Pending",
            "terminal_reason": null,
            "created_at": now,
            "updated_at": now
        }))
        .unwrap()
    }

    fn loop_args(db: PathBuf, mission_id: &str, target_stage: LoopTargetChoice) -> LoopRunArgs {
        LoopRunArgs {
            mission: RunMissionArgs {
                db: db.clone(),
                mission_id: mission_id.to_string(),
                engine: EngineChoice::Mcts,
                seed: 7,
                feature_fields: vec!["book_imbalance".to_string()],
                offline_trace: None,
                max_new_iterations: None,
                dataset: DatasetArgs {
                    dataset_manifest: db.with_extension("missing-manifest.json"),
                    validation: ValidationArgs {
                        initial_train_rows: 1,
                        validation_rows: 30,
                        fold_count: 2,
                        purge_rows: 1,
                        embargo_rows: 0,
                        sealed_holdout_rows: 30,
                        fee_bps: 1.0,
                        funding_bps: 0.0,
                        latency_bps: 0.5,
                        label_horizon_buckets: 1,
                        observation_frequency_millis: 60_000,
                    },
                },
            },
            loop_run_id: format!("loop-{mission_id}"),
            target_stage,
            max_research_missions: 1,
            repeated_failure_threshold: 3,
            max_critic_tokens: 500,
            llm_critic: false,
        }
    }

    fn canonical_evaluation(version: &str, max_candidates: usize) -> CandidateEvaluation {
        let evaluator = FormulaEvaluator::for_trials(max_candidates).unwrap();
        let fold_count = if version == WALK_FORWARD_EVALUATOR_VERSION {
            2
        } else {
            1
        };
        let folds = (1..=fold_count)
            .map(|fold_index| FoldEvaluationMetrics {
                fold_index,
                row_count: 30,
                trade_count: 30,
                mean_net_return: 0.001,
                cumulative_net_return: 0.03,
                max_drawdown: 0.01,
                net_sharpe: 1.0,
                raw_score: 2.0,
            })
            .collect::<Vec<_>>();
        let predictive = PredictiveMetrics::from_folds(
            (1..=fold_count)
                .map(|fold_index| FoldPredictiveMetrics {
                    fold_index,
                    row_count: 30,
                    time_series_ic: Some(0.1),
                    time_series_rank_ic: Some(0.1),
                })
                .collect(),
        );
        let protocol = EvaluationProtocolV1::new(
            EvaluationWalkForwardV1 {
                initial_train_rows: 1,
                validation_rows: 30,
                fold_count: 2,
                purge_rows: 1,
                embargo_rows: 0,
                sealed_holdout_rows: 30,
            },
            EvaluationCostsV1 {
                fee_bps: 1.0,
                funding_bps: 0.0,
                latency_bps: 0.5,
            },
            EvaluationLabelSpecV1 {
                horizon_buckets: 1,
                observation_frequency_millis: 60_000,
            },
        )
        .unwrap();
        let protocol_hash = protocol.content_hash().unwrap();
        CandidateEvaluation {
            passed: true,
            score: 2.0,
            failure_reasons: vec![],
            evaluator_version: version.to_string(),
            evaluator_config: evaluator.config_evidence().unwrap(),
            evaluation_protocol: Some(protocol),
            evaluation_protocol_hash: Some(protocol_hash),
            metrics: EvaluationMetrics {
                predictive,
                row_count: 30 * fold_count,
                trade_count: 30 * fold_count,
                mean_net_return: 0.001,
                cumulative_net_return: 0.03 * fold_count as f64,
                max_drawdown: 0.01,
                net_sharpe: 1.0,
                raw_score: 2.0,
                adjusted_score: 2.0,
                folds,
            },
        }
    }

    fn create_completed_mission(db: &PathBuf, mission_id: &str, candidate_id: &str) {
        create_completed_mission_with_engine(db, mission_id, candidate_id, EngineKind::ManualSeed);
    }

    fn create_completed_mission_with_engine(
        db: &PathBuf,
        mission_id: &str,
        candidate_id: &str,
        engine: EngineKind,
    ) {
        create_completed_mission_with_engine_and_dataset(
            db,
            mission_id,
            candidate_id,
            engine,
            None,
        );
    }

    fn create_completed_mission_with_engine_and_dataset(
        db: &PathBuf,
        mission_id: &str,
        candidate_id: &str,
        engine: EngineKind,
        dataset: Option<&DatasetManifest>,
    ) {
        let now = Utc::now();
        let mut store = AlphaStore::open(db).unwrap();
        let mut mission = mission_fixture(now, mission_id);
        if let Some(dataset) = dataset {
            mission.dataset_manifest_id =
                serde_json::from_value(serde_json::json!(dataset.manifest_id)).unwrap();
        }
        store.create_mission(&mission).unwrap();
        if let Some(dataset) = dataset {
            store
                .put_registry_revision(&RegistryRevision {
                    revision_id: dataset.manifest_id.clone(),
                    registry_kind: "dataset".to_string(),
                    asset_id: dataset.symbol.clone(),
                    parent_revision_id: None,
                    payload: serde_json::to_value(dataset).unwrap(),
                    created_at: dataset.created_at,
                })
                .unwrap();
        }
        let evaluation_id = format!("evaluation-{mission_id}");
        let iteration = ResearchIteration {
            iteration_id: format!("iteration-{mission_id}"),
            mission_id: mission_id.to_string(),
            parent_candidate_ids: vec![],
            engine,
            hypothesis: "fixture candidate".to_string(),
            candidate_artifact_id: Some(candidate_id.to_string()),
            evaluation_artifact_id: Some(evaluation_id.clone()),
            budget_usage: SearchBudgetUsage::default(),
            verdict: IterationVerdict::Keep,
            failure_class: None,
            failure_explanation: None,
            created_at: now,
        };
        let candidate: CandidateArtifact = serde_json::from_value(serde_json::json!({
            "Formula": {"Terminal": {"Field": "mid_price"}}
        }))
        .unwrap();
        let evaluation = canonical_evaluation(
            WALK_FORWARD_EVALUATOR_VERSION,
            mission.search_budget.max_candidates,
        );
        let evaluation = EvaluationRecord {
            evaluation_id,
            mission_id: mission_id.to_string(),
            candidate_id: candidate_id.to_string(),
            payload: serde_json::to_value(evaluation).unwrap(),
            created_at: now,
        };
        store
            .append_iteration(
                &iteration,
                Some((candidate_id, &candidate)),
                Some(&evaluation),
            )
            .unwrap();
        store
            .transition_mission(mission_id, MissionStatus::Running, now)
            .unwrap();
        store
            .finish_mission(
                mission_id,
                MissionTerminalReason::CompletionPolicySatisfied { kept_candidates: 1 },
                now,
            )
            .unwrap();
    }

    fn add_sealed_holdout_pass(db: &PathBuf, mission_id: &str, candidate_id: &str) {
        let mut store = AlphaStore::open(db).unwrap();
        let dataset_manifest_id = store
            .get_mission(mission_id)
            .unwrap()
            .dataset_manifest_id
            .as_str()
            .to_string();
        let candidate_hash = store
            .mission_lineage(mission_id)
            .unwrap()
            .candidates
            .into_iter()
            .find(|candidate| candidate.candidate_id == candidate_id)
            .unwrap()
            .content_hash;
        let evaluation = canonical_evaluation(SEALED_HOLDOUT_EVALUATOR_VERSION, 1);
        let evaluation_protocol_hash = evaluation
            .evaluation_protocol_hash
            .as_deref()
            .unwrap()
            .to_string();
        store
            .put_registry_revision(&RegistryRevision {
                revision_id: governance::sealed_evaluation_revision_id(
                    candidate_id,
                    SEALED_HOLDOUT_EVALUATOR_VERSION,
                ),
                registry_kind: "sealed_evaluation".to_string(),
                asset_id: candidate_id.to_string(),
                parent_revision_id: None,
                payload: serde_json::json!({
                    "mission_id": mission_id,
                    "candidate_content_hash": candidate_hash,
                    "dataset_manifest_id": dataset_manifest_id,
                    "evaluation_protocol_hash": evaluation_protocol_hash,
                    "evaluation": evaluation,
                }),
                created_at: Utc::now(),
            })
            .unwrap();
    }

    fn add_runtime_health(
        db: &PathBuf,
        mission_id: &str,
        candidate_id: &str,
        mode: AttributionMode,
        prefix: &str,
    ) {
        let mut store = AlphaStore::open(db).unwrap();
        let now = Utc::now();
        let account_id = format!("{prefix}-account");
        let strategy_id = format!("{prefix}-strategy");
        let venue = "binance".to_string();
        let symbol = "BTCUSDT".to_string();

        for event in [
            RuntimeAttributionEvent {
                event_id: format!("{prefix}-activation"),
                deployment_id: format!("deployment-{prefix}"),
                asset_revision_id: candidate_id.to_string(),
                mission_id: Some(mission_id.to_string()),
                mode: mode.clone(),
                outcome: AttributionOutcome::Activated,
                kind: AttributionKind::Activation,
                strategy_id: None,
                order_id: None,
                account_id: None,
                venue: None,
                symbol: None,
                metrics: BTreeMap::new(),
                reason: None,
                observed_at: now,
            },
            RuntimeAttributionEvent {
                event_id: format!("{prefix}-snapshot"),
                deployment_id: format!("deployment-{prefix}"),
                asset_revision_id: candidate_id.to_string(),
                mission_id: Some(mission_id.to_string()),
                mode: mode.clone(),
                outcome: AttributionOutcome::Healthy,
                kind: AttributionKind::PortfolioSnapshot,
                strategy_id: Some(strategy_id.clone()),
                order_id: None,
                account_id: Some(account_id.clone()),
                venue: Some(venue.clone()),
                symbol: None,
                metrics: BTreeMap::new(),
                reason: None,
                observed_at: now,
            },
            RuntimeAttributionEvent {
                event_id: format!("{prefix}-fill"),
                deployment_id: format!("deployment-{prefix}"),
                asset_revision_id: candidate_id.to_string(),
                mission_id: Some(mission_id.to_string()),
                mode,
                outcome: AttributionOutcome::Healthy,
                kind: AttributionKind::Fill,
                strategy_id: Some(strategy_id),
                order_id: Some(format!("order-{prefix}")),
                account_id: Some(account_id),
                venue: Some(venue),
                symbol: Some(symbol),
                metrics: BTreeMap::new(),
                reason: None,
                observed_at: now,
            },
        ] {
            event.validate().unwrap();
            store
                .append_memory(&MemoryRecord {
                    event_id: event.event_id.clone(),
                    mission_id: Some(mission_id.to_string()),
                    payload: serde_json::json!({
                        "kind": "runtime_attribution",
                        "event": event,
                    }),
                    created_at: now,
                })
                .unwrap();
        }
    }

    fn stage_names(run: &LoopRun) -> Vec<LoopStage> {
        run.stage_records
            .iter()
            .map(|record| record.stage.clone())
            .collect()
    }

    #[test]
    fn durable_loop_rejects_engines_without_exact_checkpoint_semantics() {
        for engine in [EngineChoice::Gp, EngineChoice::OfflineRl, EngineChoice::Llm] {
            let db = temp_db_path("alpha-loop-unsupported-engine");
            let mut args = loop_args(db.clone(), "mission-loop", LoopTargetChoice::Researching);
            args.mission.engine = engine;

            let error = run_loop(args).unwrap_err().to_string();
            assert!(error.contains("supports only mcts live-capable"));
            assert!(
                !db.exists(),
                "validation must happen before durable state is opened"
            );
        }
    }

    #[test]
    fn durable_loop_rejects_live_capability_failures_before_opening_state() {
        let cases = [
            (
                EngineChoice::Bayesian,
                vec!["book_imbalance".to_string()],
                "Bayesian window search is research-only",
            ),
            (
                EngineChoice::Mcts,
                vec!["signal".to_string()],
                "feature field signal is not live executable",
            ),
            (
                EngineChoice::Mcts,
                vec!["best_bid".to_string(), "bar_return".to_string()],
                "feature fields span live event domains",
            ),
        ];

        for (engine, feature_fields, expected) in cases {
            let db = temp_db_path("alpha-loop-live-capability");
            let mut args = loop_args(db.clone(), "mission-loop", LoopTargetChoice::Researching);
            args.mission.engine = engine;
            args.mission.feature_fields = feature_fields;

            let error = run_loop(args).unwrap_err().to_string();
            assert!(error.contains(expected), "unexpected error: {error}");
            assert!(
                !db.exists(),
                "live capability validation must happen before durable state is opened"
            );
        }
    }

    #[test]
    fn shadow_target_pauses_until_holdout_evidence_exists() {
        let db = temp_db_path("alpha-loop-holdout");
        create_completed_mission(&db, "mission-loop", "candidate-1");

        run_loop(loop_args(
            db.clone(),
            "mission-loop",
            LoopTargetChoice::ShadowHealthy,
        ))
        .unwrap();

        let run = AlphaStore::open(&db)
            .unwrap()
            .get_loop_run("loop-mission-loop")
            .unwrap();
        assert_eq!(run.status, LoopRunStatus::Paused);
        assert_eq!(
            run.stop_reason,
            Some(LoopStopReason::AwaitingEvidence {
                mission_id: "mission-loop".to_string(),
                stage: LoopTargetStage::HoldoutPassed,
            })
        );
        assert_eq!(
            stage_names(&run),
            vec![LoopStage::Researching, LoopStage::WalkForwardKept]
        );
        let _ = std::fs::remove_file(db);
    }

    #[test]
    fn researching_target_stops_without_recording_later_stages() {
        let db = temp_db_path("alpha-loop-researching");
        create_completed_mission(&db, "mission-loop", "candidate-1");

        run_loop(loop_args(
            db.clone(),
            "mission-loop",
            LoopTargetChoice::Researching,
        ))
        .unwrap();

        let run = AlphaStore::open(&db)
            .unwrap()
            .get_loop_run("loop-mission-loop")
            .unwrap();
        assert_eq!(run.status, LoopRunStatus::Completed);
        assert_eq!(stage_names(&run), vec![LoopStage::Researching]);
        let _ = std::fs::remove_file(db);
    }

    #[test]
    fn manual_keep_without_v3_evaluation_cannot_reach_walk_forward_stage() {
        let db = temp_db_path("alpha-loop-legacy-walk-forward");
        let mission_id = "mission-loop";
        let candidate_id = "candidate-legacy";
        let now = Utc::now();
        let mut store = AlphaStore::open(&db).unwrap();
        store
            .create_mission(&mission_fixture(now, mission_id))
            .unwrap();
        let candidate: CandidateArtifact = serde_json::from_value(serde_json::json!({
            "Formula": {"Terminal": {"Field": "mid_price"}}
        }))
        .unwrap();
        store
            .append_iteration(
                &ResearchIteration {
                    iteration_id: "iteration-legacy".to_string(),
                    mission_id: mission_id.to_string(),
                    parent_candidate_ids: vec![],
                    engine: EngineKind::ManualSeed,
                    hypothesis: "legacy keep".to_string(),
                    candidate_artifact_id: Some(candidate_id.to_string()),
                    evaluation_artifact_id: None,
                    budget_usage: SearchBudgetUsage::default(),
                    verdict: IterationVerdict::Keep,
                    failure_class: None,
                    failure_explanation: None,
                    created_at: now,
                },
                Some((candidate_id, &candidate)),
                None,
            )
            .unwrap();
        store
            .transition_mission(mission_id, MissionStatus::Running, now)
            .unwrap();
        store
            .finish_mission(
                mission_id,
                MissionTerminalReason::CompletionPolicySatisfied { kept_candidates: 1 },
                now,
            )
            .unwrap();
        drop(store);

        run_loop(loop_args(
            db.clone(),
            mission_id,
            LoopTargetChoice::WalkForwardKept,
        ))
        .unwrap();

        let run = AlphaStore::open(&db)
            .unwrap()
            .get_loop_run("loop-mission-loop")
            .unwrap();
        assert_eq!(run.status, LoopRunStatus::Paused);
        assert_eq!(
            run.stop_reason,
            Some(LoopStopReason::AwaitingEvidence {
                mission_id: mission_id.to_string(),
                stage: LoopTargetStage::WalkForwardKept,
            })
        );
        assert_eq!(stage_names(&run), vec![LoopStage::Researching]);
        let _ = std::fs::remove_file(db);
    }

    #[test]
    fn legacy_sealed_record_is_not_reported_as_an_idempotent_v3_evaluation() {
        let db = temp_db_path("alpha-loop-legacy-sealed");
        let mission_id = "mission-loop";
        let candidate_id = "candidate-1";
        let (directory, manifest_path, manifest) = governed_dataset_fixture(mission_id);
        create_completed_mission_with_engine_and_dataset(
            &db,
            mission_id,
            candidate_id,
            EngineKind::ManualSeed,
            Some(&manifest),
        );
        let mut store = AlphaStore::open(&db).unwrap();
        let candidate_hash = store.mission_lineage(mission_id).unwrap().candidates[0]
            .content_hash
            .clone();
        store
            .put_registry_revision(&RegistryRevision {
                revision_id: governance::sealed_evaluation_revision_id(
                    candidate_id,
                    SEALED_HOLDOUT_EVALUATOR_VERSION,
                ),
                registry_kind: "sealed_evaluation".to_string(),
                asset_id: candidate_id.to_string(),
                parent_revision_id: None,
                payload: serde_json::json!({
                    "mission_id": mission_id,
                    "candidate_content_hash": candidate_hash,
                    "dataset_manifest_id": manifest.manifest_id,
                    "evaluation": {
                        "passed": true,
                        "score": 1.0,
                        "failure_reasons": [],
                        "evaluator_version": "sealed-holdout-v1"
                    }
                }),
                created_at: Utc::now(),
            })
            .unwrap();
        drop(store);

        let mut args = loop_args(db.clone(), mission_id, LoopTargetChoice::HoldoutPassed);
        args.mission.dataset.dataset_manifest = manifest_path;
        let error = governance::evaluate(EvaluateArgs {
            db: db.clone(),
            mission_id: mission_id.to_string(),
            candidate_id: candidate_id.to_string(),
            model_root: None,
            dataset: args.mission.dataset,
        })
        .unwrap_err();
        assert!(format!("{error:#}").contains("legacy or malformed"));
        let _ = std::fs::remove_file(db);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn offline_rl_candidate_remains_lab_only_with_sealed_evidence_present() {
        let db = temp_db_path("alpha-loop-offline-rl-lab");
        let mission_id = "mission-loop";
        let candidate_id = "candidate-rl";
        let (directory, manifest_path, manifest) = governed_dataset_fixture(mission_id);
        create_completed_mission_with_engine_and_dataset(
            &db,
            mission_id,
            candidate_id,
            EngineKind::OfflineReinforcementLearning,
            Some(&manifest),
        );
        add_sealed_holdout_pass(&db, mission_id, candidate_id);

        let mut args = loop_args(db.clone(), mission_id, LoopTargetChoice::HoldoutPassed);
        args.mission.dataset.dataset_manifest = manifest_path;
        let dataset = args.mission.dataset.clone();
        run_loop(args).unwrap();
        let run = AlphaStore::open(&db)
            .unwrap()
            .get_loop_run("loop-mission-loop")
            .unwrap();
        assert_eq!(run.status, LoopRunStatus::Paused);
        assert_eq!(
            run.stop_reason,
            Some(LoopStopReason::AwaitingEvidence {
                mission_id: mission_id.to_string(),
                stage: LoopTargetStage::HoldoutPassed,
            })
        );
        assert_eq!(
            stage_names(&run),
            vec![LoopStage::Researching, LoopStage::WalkForwardKept]
        );

        let error = governance::evaluate(EvaluateArgs {
            db: db.clone(),
            mission_id: mission_id.to_string(),
            candidate_id: candidate_id.to_string(),
            model_root: None,
            dataset,
        })
        .unwrap_err();
        assert!(format!("{error:#}").contains("lab search-policy output"));
        let _ = std::fs::remove_file(db);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn shadow_target_does_not_complete_at_paper_health() {
        let db = temp_db_path("alpha-loop-paper");
        create_completed_mission(&db, "mission-loop", "candidate-1");
        add_sealed_holdout_pass(&db, "mission-loop", "candidate-1");
        add_runtime_health(
            &db,
            "mission-loop",
            "candidate-1",
            AttributionMode::Paper,
            "paper",
        );

        run_loop(loop_args(
            db.clone(),
            "mission-loop",
            LoopTargetChoice::ShadowHealthy,
        ))
        .unwrap();

        let run = AlphaStore::open(&db)
            .unwrap()
            .get_loop_run("loop-mission-loop")
            .unwrap();
        assert_eq!(run.status, LoopRunStatus::Paused);
        assert_eq!(
            run.stop_reason,
            Some(LoopStopReason::AwaitingEvidence {
                mission_id: "mission-loop".to_string(),
                stage: LoopTargetStage::ShadowHealthy,
            })
        );
        assert_eq!(
            stage_names(&run),
            vec![
                LoopStage::Researching,
                LoopStage::WalkForwardKept,
                LoopStage::HoldoutPassed,
                LoopStage::PaperHealthy,
            ]
        );
        let _ = std::fs::remove_file(db);
    }

    #[test]
    fn shadow_target_completes_in_stage_order_with_full_evidence() {
        let db = temp_db_path("alpha-loop-shadow");
        create_completed_mission(&db, "mission-loop", "candidate-1");
        add_sealed_holdout_pass(&db, "mission-loop", "candidate-1");
        add_runtime_health(
            &db,
            "mission-loop",
            "candidate-1",
            AttributionMode::Paper,
            "paper",
        );
        add_runtime_health(
            &db,
            "mission-loop",
            "candidate-1",
            AttributionMode::Shadow,
            "shadow",
        );

        run_loop(loop_args(
            db.clone(),
            "mission-loop",
            LoopTargetChoice::ShadowHealthy,
        ))
        .unwrap();

        let run = AlphaStore::open(&db)
            .unwrap()
            .get_loop_run("loop-mission-loop")
            .unwrap();
        assert_eq!(run.status, LoopRunStatus::Completed);
        assert_eq!(
            run.stop_reason,
            Some(LoopStopReason::TargetStageReached {
                mission_id: "mission-loop".to_string(),
                stage: LoopTargetStage::ShadowHealthy,
            })
        );
        assert_eq!(
            stage_names(&run),
            vec![
                LoopStage::Researching,
                LoopStage::WalkForwardKept,
                LoopStage::HoldoutPassed,
                LoopStage::PaperHealthy,
                LoopStage::ShadowHealthy,
            ]
        );
        let _ = std::fs::remove_file(db);
    }

    #[test]
    fn governed_evidence_reaches_live_small_eligibility_without_live_activation() {
        let candidate_id = "candidate-e2e";
        let mission_id = "mission-e2e";
        let db = temp_db_path("alpha-loop-governed-e2e");
        let (directory, manifest_path, manifest) = governed_dataset_fixture(mission_id);
        let now = Utc::now();
        let mut research_mission = mission_fixture(now, mission_id);
        research_mission.dataset_manifest_id =
            serde_json::from_value(serde_json::json!(manifest.manifest_id)).unwrap();
        let candidate: CandidateArtifact = serde_json::from_value(serde_json::json!({
            "Formula": {"Terminal": {"Field": "bar_return"}}
        }))
        .unwrap();
        let dataset_args = DatasetArgs {
            dataset_manifest: manifest_path.clone(),
            validation: ValidationArgs {
                initial_train_rows: 100,
                validation_rows: 96,
                fold_count: 3,
                purge_rows: 1,
                embargo_rows: 1,
                sealed_holdout_rows: 96,
                fee_bps: 0.0,
                funding_bps: 0.0,
                latency_bps: 0.0,
                label_horizon_buckets: 1,
                observation_frequency_millis: 60_000,
            },
        };
        let research_rows = data_mission::load_research_rows(&manifest, 0.0, 0.0, 0.0).unwrap();
        let prepared = prepare_dataset(
            research_rows,
            &dataset_args
                .validation
                .evaluation_protocol(&alpha_domain::EvaluationLabelSpecV1 {
                    horizon_buckets: 1,
                    observation_frequency_millis: 60_000,
                })
                .unwrap(),
            format!("sealed:{}", manifest.manifest_id),
        )
        .unwrap();
        let walk_forward_evaluation =
            FormulaEvaluator::for_trials(research_mission.search_budget.max_candidates)
                .unwrap()
                .evaluate(
                    &EngineProposal {
                        candidate_id: candidate_id.to_string(),
                        hypothesis: "positive momentum persists".to_string(),
                        artifact: candidate.clone(),
                        expansions: 0,
                        tokens: 0,
                        elapsed_ms: 0,
                    },
                    &prepared.engine_context(),
                )
                .unwrap();
        assert!(walk_forward_evaluation.passed);
        let evaluation_id = "evaluation-e2e".to_string();
        let iteration = ResearchIteration {
            iteration_id: "iteration-e2e".to_string(),
            mission_id: mission_id.to_string(),
            parent_candidate_ids: vec![],
            engine: EngineKind::ManualSeed,
            hypothesis: "positive momentum persists".to_string(),
            candidate_artifact_id: Some(candidate_id.to_string()),
            evaluation_artifact_id: Some(evaluation_id.clone()),
            budget_usage: SearchBudgetUsage::default(),
            verdict: IterationVerdict::Keep,
            failure_class: None,
            failure_explanation: None,
            created_at: now,
        };
        let evaluation = EvaluationRecord {
            evaluation_id,
            mission_id: mission_id.to_string(),
            candidate_id: candidate_id.to_string(),
            payload: serde_json::to_value(walk_forward_evaluation).unwrap(),
            created_at: now,
        };
        {
            let mut store = AlphaStore::open(&db).unwrap();
            store
                .put_registry_revision(&RegistryRevision {
                    revision_id: manifest.manifest_id.clone(),
                    registry_kind: "dataset".to_string(),
                    asset_id: manifest.symbol.clone(),
                    parent_revision_id: None,
                    payload: serde_json::to_value(&manifest).unwrap(),
                    created_at: manifest.created_at,
                })
                .unwrap();
            store.create_mission(&research_mission).unwrap();
            store
                .append_iteration(
                    &iteration,
                    Some((candidate_id, &candidate)),
                    Some(&evaluation),
                )
                .unwrap();
            store
                .transition_mission(mission_id, MissionStatus::Running, now)
                .unwrap();
            store
                .finish_mission(
                    mission_id,
                    MissionTerminalReason::CompletionPolicySatisfied { kept_candidates: 1 },
                    now,
                )
                .unwrap();
        }

        governance::evaluate(EvaluateArgs {
            db: db.clone(),
            mission_id: mission_id.to_string(),
            candidate_id: candidate_id.to_string(),
            model_root: None,
            dataset: dataset_args.clone(),
        })
        .unwrap();
        governance::evaluate(EvaluateArgs {
            db: db.clone(),
            mission_id: mission_id.to_string(),
            candidate_id: candidate_id.to_string(),
            model_root: None,
            dataset: dataset_args.clone(),
        })
        .expect("the same evaluation protocol must reuse sealed evidence idempotently");
        governance::promote(PromoteArgs {
            db: db.clone(),
            mission_id: mission_id.to_string(),
            candidate_id: candidate_id.to_string(),
            promotion_id: None,
            bundle_out: None,
            model_root: None,
        })
        .unwrap();
        governance::promote(PromoteArgs {
            db: db.clone(),
            mission_id: mission_id.to_string(),
            candidate_id: candidate_id.to_string(),
            promotion_id: None,
            bundle_out: None,
            model_root: None,
        })
        .expect("an exact promotion replay must be idempotent");
        {
            let store = AlphaStore::open(&db).unwrap();
            let promotion = store
                .get_promotion(&format!("promotion:{candidate_id}"))
                .unwrap();
            let bundle = store
                .get_strategy_bundle(&promotion.record.bundle_id)
                .unwrap();
            let sealed = store
                .get_registry_revision(&promotion.record.sealed_evaluation_id)
                .unwrap();
            let evaluation = sealed.payload.get("evaluation").unwrap();
            let typed: CandidateEvaluation = serde_json::from_value(evaluation.clone()).unwrap();
            typed.validate().unwrap();
            let (_, protocol_hash) = typed.protocol_binding().unwrap();
            assert_eq!(
                sealed
                    .payload
                    .get("evaluation_protocol_hash")
                    .and_then(serde_json::Value::as_str),
                Some(protocol_hash)
            );
            assert_eq!(bundle.evaluation_protocol_hash, protocol_hash);
            assert_eq!(
                promotion.record.evaluation_protocol_hash,
                bundle.evaluation_protocol_hash
            );
            assert_eq!(
                bundle.evaluator_config_hash,
                alpha_domain::canonical_json_hash(evaluation.get("evaluator_config").unwrap())
                    .unwrap()
            );
            assert_eq!(
                bundle.evaluation_metrics_hash,
                alpha_domain::canonical_json_hash(evaluation.get("metrics").unwrap()).unwrap()
            );
            assert_eq!(
                promotion.record.evaluator_config_hash,
                bundle.evaluator_config_hash
            );
            assert_eq!(
                promotion.record.evaluation_metrics_hash,
                bundle.evaluation_metrics_hash
            );
        }

        let mut events = Vec::new();
        let bundle_id;
        {
            let (paper_id, paper_bundle_id) = persist_governed_runtime_deployment(
                &db,
                &directory,
                candidate_id,
                AttributionMode::Paper,
                now,
            );
            let (shadow_id, shadow_bundle_id) = persist_governed_runtime_deployment(
                &db,
                &directory,
                candidate_id,
                AttributionMode::Shadow,
                now,
            );
            assert_eq!(paper_bundle_id, shadow_bundle_id);
            bundle_id = paper_bundle_id;
            let strategy_id = format!("{bundle_id}:BTCUSDT");
            events.extend(runtime_health_events(
                &paper_id,
                candidate_id,
                &strategy_id,
                AttributionMode::Paper,
                now,
            ));
            events.extend(runtime_health_events(
                &shadow_id,
                candidate_id,
                &strategy_id,
                AttributionMode::Shadow,
                now,
            ));
        }
        let feedback_path = directory.join("runtime-feedback.jsonl");
        let trusted_keys_path = directory.join("runtime-feedback-trusted-keys.json");
        let feedback_key = ed25519_dalek::SigningKey::from_bytes(&[11_u8; 32]);
        let mut feedback = Vec::new();
        for event in &events {
            let signed =
                sign_runtime_attribution_event(event.clone(), "runtime-feedback-1", &feedback_key)
                    .unwrap();
            serde_json::to_writer(&mut feedback, &signed).unwrap();
            feedback.push(b'\n');
        }
        std::fs::write(&feedback_path, feedback).unwrap();
        std::fs::write(
            &trusted_keys_path,
            serde_json::to_vec(&BTreeMap::from([(
                "runtime-feedback-1".to_string(),
                hex::encode(feedback_key.verifying_key().to_bytes()),
            )]))
            .unwrap(),
        )
        .unwrap();
        governance::ingest_feedback_log(FeedbackLogArgs {
            db: db.clone(),
            log: feedback_path,
            trusted_keys: trusted_keys_path,
        })
        .unwrap();

        {
            let mut store = AlphaStore::open(&db).unwrap();
            let promotion = store
                .get_promotion(&format!("promotion:{candidate_id}"))
                .unwrap();
            let evidence = LiveSmallEligibilityEvidence {
                candidate_id: candidate_id.to_string(),
                bundle_id: bundle_id.clone(),
                reconciliation_evidence_sha256: "a".repeat(64),
                reduce_only_exit_evidence_sha256: "b".repeat(64),
                shadow_soak_evidence_sha256: "c".repeat(64),
            };
            store
                .record_approval(&ApprovalRecord {
                    approval_id: "live-eligibility-e2e".to_string(),
                    approval_class: "human_live_small".to_string(),
                    subject_id: promotion.record.promotion_id,
                    payload: serde_json::json!({"eligibility": evidence}),
                    signer_id: Some("risk-officer-e2e".to_string()),
                    valid_from: Some(now),
                    expires_at: Some(now + Duration::minutes(30)),
                    revoked_at: None,
                    revoked_by: None,
                    revocation_reason: None,
                    created_at: now,
                })
                .unwrap();
        }

        let mut args = loop_args(db.clone(), mission_id, LoopTargetChoice::LiveSmallEligible);
        args.loop_run_id = "loop-governed-e2e".to_string();
        args.mission.dataset = dataset_args;
        run_loop(args).unwrap();

        let store = AlphaStore::open(&db).unwrap();
        let run = store.get_loop_run("loop-governed-e2e").unwrap();
        assert_eq!(run.status, LoopRunStatus::Completed);
        assert_eq!(
            stage_names(&run),
            vec![
                LoopStage::Researching,
                LoopStage::WalkForwardKept,
                LoopStage::HoldoutPassed,
                LoopStage::PaperHealthy,
                LoopStage::ShadowHealthy,
                LoopStage::LiveSmallEligible,
            ]
        );
        assert!(store
            .runtime_attributions_for_mission(mission_id)
            .unwrap()
            .iter()
            .all(|event| event.mode != AttributionMode::LiveSmall));
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
        let _ = std::fs::remove_file(&db);
        let _ = std::fs::remove_file(format!("{}.integrity-key", db.display()));
    }

    #[test]
    fn runtime_health_requires_one_deployment_to_carry_all_signals() {
        let now = Utc::now();
        let events = vec![
            RuntimeAttributionEvent {
                event_id: "activation-1".to_string(),
                deployment_id: "deployment-a".to_string(),
                asset_revision_id: "candidate-1".to_string(),
                mission_id: Some("mission-1".to_string()),
                mode: AttributionMode::Paper,
                outcome: AttributionOutcome::Activated,
                kind: AttributionKind::Activation,
                strategy_id: None,
                order_id: None,
                account_id: None,
                venue: None,
                symbol: None,
                metrics: BTreeMap::new(),
                reason: None,
                observed_at: now,
            },
            RuntimeAttributionEvent {
                event_id: "snapshot-1".to_string(),
                deployment_id: "deployment-b".to_string(),
                asset_revision_id: "candidate-1".to_string(),
                mission_id: Some("mission-1".to_string()),
                mode: AttributionMode::Paper,
                outcome: AttributionOutcome::Healthy,
                kind: AttributionKind::PortfolioSnapshot,
                strategy_id: Some("strategy-1".to_string()),
                order_id: None,
                account_id: Some("account-1".to_string()),
                venue: Some("binance".to_string()),
                symbol: None,
                metrics: BTreeMap::new(),
                reason: None,
                observed_at: now,
            },
            RuntimeAttributionEvent {
                event_id: "fill-1".to_string(),
                deployment_id: "deployment-b".to_string(),
                asset_revision_id: "candidate-1".to_string(),
                mission_id: Some("mission-1".to_string()),
                mode: AttributionMode::Paper,
                outcome: AttributionOutcome::Healthy,
                kind: AttributionKind::Fill,
                strategy_id: Some("strategy-1".to_string()),
                order_id: Some("order-1".to_string()),
                account_id: Some("account-1".to_string()),
                venue: Some("binance".to_string()),
                symbol: Some("BTCUSDT".to_string()),
                metrics: BTreeMap::new(),
                reason: None,
                observed_at: now,
            },
        ];

        assert!(!runtime_stage_is_healthy(
            &events,
            "candidate-1",
            AttributionMode::Paper
        ));

        let mut same_deployment = events.clone();
        same_deployment[0].deployment_id = "deployment-b".to_string();
        same_deployment[2].strategy_id = Some("strategy-2".to_string());
        assert!(!runtime_stage_is_healthy(
            &same_deployment,
            "candidate-1",
            AttributionMode::Paper
        ));
        same_deployment[2].strategy_id = Some("strategy-1".to_string());
        assert!(runtime_stage_is_healthy(
            &same_deployment,
            "candidate-1",
            AttributionMode::Paper
        ));
    }
}
