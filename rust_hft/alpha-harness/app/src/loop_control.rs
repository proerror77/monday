use crate::{
    cli::{
        print_json, LearnMissionArgs, LoopRunArgs, LoopStatusArgs, LoopTargetChoice,
        RecoverLegacyCheckpointArgs,
    },
    mission,
};
use alpha_domain::{
    AttributionKind, AttributionMode, AttributionOutcome, LoopCompletionPolicy, LoopRun,
    LoopRunStatus, LoopStage, LoopStageRecord, LoopStageStatus, LoopStopReason, LoopTargetStage,
    MissionStatus, RuntimeAttributionEvent,
};
use alpha_store::{AlphaStore, StoreError};
use anyhow::{bail, Context};
use chrono::Utc;

pub fn run_loop(args: LoopRunArgs) -> anyhow::Result<()> {
    if args.max_research_missions == 0 {
        bail!("max_research_missions must be positive");
    }
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
    append_stage_if_changed(
        args,
        run,
        mission_id,
        LoopStage::WalkForwardKept,
        LoopStageStatus::Completed,
        research_reason,
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

fn runtime_stage_is_healthy(
    events: &[RuntimeAttributionEvent],
    candidate_id: &str,
    mode: AttributionMode,
) -> bool {
    #[derive(Default)]
    struct DeploymentHealth {
        activated: bool,
        unhealthy: bool,
        strategies: std::collections::BTreeMap<String, StrategyHealth>,
    }

    #[derive(Default)]
    struct StrategyHealth {
        healthy_snapshot: bool,
        fill: bool,
    }

    let mut by_deployment = std::collections::BTreeMap::<String, DeploymentHealth>::new();
    for event in events
        .iter()
        .filter(|event| event.asset_revision_id == candidate_id && event.mode == mode)
    {
        let health = by_deployment
            .entry(event.deployment_id.clone())
            .or_default();
        if matches!(
            event.outcome,
            AttributionOutcome::Failed
                | AttributionOutcome::Decayed
                | AttributionOutcome::RolledBack
        ) || event.kind == AttributionKind::StreamGap
        {
            health.unhealthy = true;
            continue;
        }
        if event.kind == AttributionKind::Activation
            && event.outcome == AttributionOutcome::Activated
        {
            health.activated = true;
        }
        if event.outcome == AttributionOutcome::Healthy
            && event.kind == AttributionKind::PortfolioSnapshot
        {
            if let Some(strategy_id) = event.strategy_id.as_ref() {
                health
                    .strategies
                    .entry(strategy_id.clone())
                    .or_default()
                    .healthy_snapshot = true;
            }
        }
        if event.kind == AttributionKind::Fill {
            if let Some(strategy_id) = event.strategy_id.as_ref() {
                health
                    .strategies
                    .entry(strategy_id.clone())
                    .or_default()
                    .fill = true;
            }
        }
    }

    by_deployment.values().any(|health| {
        !health.unhealthy
            && health.activated
            && health
                .strategies
                .values()
                .any(|strategy| strategy.healthy_snapshot && strategy.fill)
    })
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
            DatasetArgs, EngineChoice, EvaluateArgs, JsonLogArgs, PromoteArgs, RunMissionArgs,
            SignDeploymentArgs,
        },
        data_mission, governance,
    };
    use alpha_domain::{
        AllowedIntentType, ApprovalClass, CandidateArtifact, DeploymentEnvelope, EngineKind,
        IterationVerdict, LiveSmallEligibilityEvidence, MissionTerminalReason, ResearchIteration,
        ResearchMission, SearchBudgetUsage,
    };
    use alpha_store::{ApprovalRecord, MemoryRecord, RegistryRevision};
    use chrono::Duration;
    use hft_collector::{DatasetManifest, OhlcvTraceRow, QualityReport};
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
        let start = Utc::now() - Duration::hours(2);
        let rows = (0..80)
            .map(|index| {
                let close = 100.0 * 1.001_f64.powi(index);
                OhlcvTraceRow {
                    event_time: start + Duration::minutes(i64::from(index)),
                    exchange_time: start + Duration::minutes(i64::from(index)),
                    receive_time: start + Duration::minutes(i64::from(index)),
                    available_time: start + Duration::minutes(i64::from(index)),
                    ingestion_time: start + Duration::minutes(i64::from(index)),
                    source: "binance-public".to_string(),
                    schema_version: "binance-kline-v1".to_string(),
                    quality_flags: vec![],
                    symbol: "BTCUSDT".to_string(),
                    open: close * 0.9995,
                    high: close * 1.001,
                    low: close * 0.999,
                    close,
                    volume: 1.0,
                }
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
            schema_version: "binance-kline-v1".to_string(),
            artifact_path,
            artifact_sha256,
            quality: QualityReport {
                rows: rows.len(),
                parse_failures: 0,
                non_monotonic_events: 0,
                non_finite_values: 0,
            },
            created_at: start,
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
                    "scope_hash": governance::deployment_scope_hash(&envelope).unwrap(),
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
                offline_trace: None,
                max_new_iterations: None,
                dataset: DatasetArgs {
                    dataset_manifest: db.with_extension("missing-manifest.json"),
                    initial_train_rows: 1,
                    validation_rows: 1,
                    fold_count: 1,
                    purge_rows: 0,
                    embargo_rows: 0,
                    sealed_holdout_rows: 1,
                    fee_bps: 1.0,
                    funding_bps: 0.0,
                    latency_bps: 0.5,
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

    fn create_completed_mission(db: &PathBuf, mission_id: &str, candidate_id: &str) {
        let now = Utc::now();
        let mut store = AlphaStore::open(db).unwrap();
        store
            .create_mission(&mission_fixture(now, mission_id))
            .unwrap();
        let iteration = ResearchIteration {
            iteration_id: format!("iteration-{mission_id}"),
            mission_id: mission_id.to_string(),
            parent_candidate_ids: vec![],
            engine: EngineKind::ManualSeed,
            hypothesis: "fixture candidate".to_string(),
            candidate_artifact_id: Some(candidate_id.to_string()),
            evaluation_artifact_id: None,
            budget_usage: SearchBudgetUsage::default(),
            verdict: IterationVerdict::Keep,
            failure_class: None,
            failure_explanation: None,
            created_at: now,
        };
        let candidate = CandidateArtifact::Program(serde_json::json!({
            "candidate_id": candidate_id,
        }));
        store
            .append_iteration(&iteration, Some((candidate_id, &candidate)), None)
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
        let candidate_hash = store
            .mission_lineage(mission_id)
            .unwrap()
            .candidates
            .into_iter()
            .find(|candidate| candidate.candidate_id == candidate_id)
            .unwrap()
            .content_hash;
        store
            .put_registry_revision(&RegistryRevision {
                revision_id: format!("sealed-evaluation:{candidate_id}"),
                registry_kind: "sealed_evaluation".to_string(),
                asset_id: candidate_id.to_string(),
                parent_revision_id: None,
                payload: serde_json::json!({
                    "mission_id": mission_id,
                    "candidate_content_hash": candidate_hash,
                    "dataset_manifest_id": "dataset-loop",
                    "evaluation": {
                        "passed": true,
                        "evaluator_version": "sealed-holdout-v1"
                    }
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
            "Formula": {"Terminal": {"Field": "signal"}}
        }))
        .unwrap();
        let iteration = ResearchIteration {
            iteration_id: "iteration-e2e".to_string(),
            mission_id: mission_id.to_string(),
            parent_candidate_ids: vec![],
            engine: EngineKind::ManualSeed,
            hypothesis: "positive momentum persists".to_string(),
            candidate_artifact_id: Some(candidate_id.to_string()),
            evaluation_artifact_id: None,
            budget_usage: SearchBudgetUsage::default(),
            verdict: IterationVerdict::Keep,
            failure_class: None,
            failure_explanation: None,
            created_at: now,
        };
        {
            let mut store = AlphaStore::open(&db).unwrap();
            store.create_mission(&research_mission).unwrap();
            store
                .append_iteration(&iteration, Some((candidate_id, &candidate)), None)
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

        let dataset_args = DatasetArgs {
            dataset_manifest: manifest_path.clone(),
            initial_train_rows: 20,
            validation_rows: 5,
            fold_count: 3,
            purge_rows: 1,
            embargo_rows: 1,
            sealed_holdout_rows: 10,
            fee_bps: 0.0,
            funding_bps: 0.0,
            latency_bps: 0.0,
        };
        governance::evaluate(EvaluateArgs {
            db: db.clone(),
            mission_id: mission_id.to_string(),
            candidate_id: candidate_id.to_string(),
            dataset: dataset_args.clone(),
        })
        .unwrap();
        governance::promote(PromoteArgs {
            db: db.clone(),
            mission_id: mission_id.to_string(),
            candidate_id: candidate_id.to_string(),
            promotion_id: None,
        })
        .unwrap();

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
        let mut feedback = Vec::new();
        for event in &events {
            serde_json::to_writer(&mut feedback, event).unwrap();
            feedback.push(b'\n');
        }
        std::fs::write(&feedback_path, feedback).unwrap();
        governance::ingest_feedback_log(JsonLogArgs {
            db: db.clone(),
            log: feedback_path,
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
