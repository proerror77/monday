mod engines;
mod evaluation;
mod queue;

use chrono::{DateTime, Utc};
use engines::{
    query_codex_focused_subagent, query_codex_scan, query_codex_strategy_completion,
    query_grok_builder_context, query_grok_strategy_completion,
};
use evaluation::{
    array_len, evaluate_agent_run_contract_with_evidence, evaluate_structured_output,
    validate_admission, validate_verified_evidence_requirements, validate_verified_evidence_scope,
    AdmissionLimits, ContractEvaluation,
};
use ploy_control_client::ControlPlaneClient;
use ploy_operator_contracts::{
    AgentRunRecord, AgentToolCallRecord, DeploymentSummary, SystemStatus, TradingStateSnapshot,
};
use ploy_research::{verify_prediction_evidence, VerifiedPredictionEvidenceReceipt};
use queue::{append_jsonl_sync, append_text_sync, write_text_sync, QueueStore};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs::{self, File};
use std::path::{Component, Path, PathBuf};
use std::thread;
use std::time::Duration;
use uuid::Uuid;

pub use queue::{finalize_needs_retry, ClaimedBatch, QueuedAgentRunRequest, RetryFailpoint};

pub type Result<T> = std::result::Result<T, SidecarError>;

#[derive(Debug, thiserror::Error)]
pub enum SidecarError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error("{0}")]
    Message(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskCompletion {
    pub status: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grok_decision: Option<String>,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
    pub blockers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_action: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SidecarConfig {
    pub engine: String,
    pub poll_interval: Duration,
    pub scan_enabled: bool,
    pub dry_run: bool,
    pub max_retries: u32,
    pub admission: AdmissionLimits,
    pub runtime_root: PathBuf,
    pub evidence_root: PathBuf,
}

impl SidecarConfig {
    pub fn from_env() -> Result<Self> {
        let engine = nonempty_env("SIDECAR_AGENT_ENGINE").unwrap_or_else(|| "codex".to_string());
        if !matches!(engine.as_str(), "codex" | "grok") {
            return Err(SidecarError::Message(format!(
                "SIDECAR_AGENT_ENGINE must be codex or grok, got {engine}"
            )));
        }
        if nonempty_env("CODEX_CLI_SANDBOX")
            .as_deref()
            .is_some_and(|value| value != "read-only")
        {
            return Err(SidecarError::Message(
                "CODEX_CLI_SANDBOX is an assertion and must be read-only".to_string(),
            ));
        }
        if nonempty_env("CODEX_CLI_WORKDIR").is_some() {
            return Err(SidecarError::Message(
                "CODEX_CLI_WORKDIR is not supported; Codex runs in an isolated temporary directory"
                    .to_string(),
            ));
        }
        let poll_seconds = env_u64("SIDECAR_POLL_INTERVAL_SECS", 300);
        if poll_seconds == 0 {
            return Err(SidecarError::Message(
                "SIDECAR_POLL_INTERVAL_SECS must be greater than zero".to_string(),
            ));
        }
        let scan_enabled = env_bool("SIDECAR_SCAN_ENABLED", false);
        if scan_enabled {
            return Err(SidecarError::Message(
                "SIDECAR_SCAN_ENABLED requires a bundled Rust sports/market evidence adapter"
                    .to_string(),
            ));
        }
        let runtime_root = nonempty_env("PLOY_RUNTIME_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("run/platform"));
        let evidence_root = nonempty_env("PLOY_PREDICTION_EVIDENCE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| runtime_root.join("prediction-evidence"));
        Ok(Self {
            engine,
            poll_interval: Duration::from_secs(poll_seconds),
            scan_enabled,
            dry_run: env_bool("SIDECAR_DRY_RUN", true),
            max_retries: env_u64("SIDECAR_AGENT_RUN_MAX_RETRIES", 1).min(u32::MAX as u64) as u32,
            admission: AdmissionLimits::from_env(),
            runtime_root,
            evidence_root,
        })
    }
}

pub struct Sidecar {
    config: SidecarConfig,
    store: QueueStore,
    client: ControlPlaneClient,
    _worker_lease: File,
}

impl Sidecar {
    pub fn from_env() -> Result<Self> {
        let config = SidecarConfig::from_env()?;
        let mut client = ControlPlaneClient::from_runtime_root(config.runtime_root.clone());
        client.admin_token = None;
        client.operator_token = None;
        client.sidecar_token = Some(require_sidecar_auth_token(nonempty_env(
            "PLOY_SIDECAR_AUTH_TOKEN",
        ))?);
        client.control_plane_addr = control_plane_addr()?;
        let store = QueueStore::from_env(&config.runtime_root)?;
        let worker_lease = store.acquire_worker_lease()?;
        Ok(Self {
            config,
            store,
            client,
            _worker_lease: worker_lease,
        })
    }

    pub fn run_cycle(&mut self) -> Result<()> {
        if let Some(mut batch) = self.store.claim()? {
            for queued in batch.requests.clone() {
                let mut record = self.run_queued_request(&queued)?;
                if record.status == "needs_retry" {
                    let reason = retry_reason(&record);
                    if queued.attempt().saturating_add(1) > self.config.max_retries {
                        mark_retry_exhausted(
                            &mut record,
                            queued.attempt(),
                            self.config.max_retries,
                            &reason,
                        );
                        self.record_run(&record)?;
                        batch.complete(&queued)?;
                        eprintln!("retry limit reached for {}: {reason}", queued.run_id);
                        continue;
                    }
                    let retry = finalize_needs_retry(
                        &self.store,
                        &queued,
                        &reason,
                        self.config.max_retries,
                        || self.record_run(&record),
                        || batch.complete(&queued),
                        None,
                    )?;
                    if let Some(retry) = retry {
                        eprintln!(
                            "requeued {} ({}/{}) after needs_retry: {reason}",
                            queued.run_id,
                            retry.attempt(),
                            self.config.max_retries
                        );
                    }
                } else {
                    self.record_run(&record)?;
                    batch.complete(&queued)?;
                }
            }
            batch.acknowledge()?;
        }
        if self.config.scan_enabled {
            let record = self.run_scan_cycle()?;
            self.record_run(&record)?;
        }
        Ok(())
    }

    fn run_queued_request(&self, queued: &QueuedAgentRunRequest) -> Result<AgentRunRecord> {
        let started_at = Utc::now();
        let runtime_load = self.runtime_context();
        let runtime = runtime_load.runtime;
        let runtime_failure = runtime_load.failure_reason;
        let harness_context = self.read_harness_context(6_000)?;
        let mut tool_calls = runtime.receipts.clone();
        let mut turns_remaining = queued.request.max_turns;
        let mut session_id = None;
        let mut completion = None;
        let mut failure_reason = None;
        let mut focused_results = Vec::new();
        let mut grok_context = None;
        let mut verified_evidence: Option<VerifiedPredictionEvidenceReceipt> = None;
        let mut evidence_prompt = queued.request.run_packet.clone();

        let execution = (|| -> Result<()> {
            if let Some(reason) = validate_admission(&queued.request, self.config.admission) {
                return Err(SidecarError::Message(reason));
            }
            if let Some(refs) = queued.request.prediction_evidence.as_ref() {
                let verifier_refs: ploy_research::PredictionEvidenceRefs =
                    serde_json::from_value(serde_json::to_value(refs)?).map_err(|error| {
                        SidecarError::Message(format!("prediction evidence DTO rejected: {error}"))
                    })?;
                let receipt =
                    verify_queued_prediction_evidence(&verifier_refs, &self.config.evidence_root)
                        .map_err(|reason| {
                        SidecarError::Message(format!("prediction evidence rejected: {reason}"))
                    })?;
                validate_verified_evidence_requirements(&queued.request, &receipt)
                    .map_err(SidecarError::Message)?;
                validate_verified_evidence_scope(&queued.request, &receipt)
                    .map_err(SidecarError::Message)?;
                evidence_prompt = format!(
                    "{}\n\nVerified evidence summary (parent-produced, read-only): {}",
                    queued.request.run_packet,
                    receipt.prompt_summary()
                );
                verified_evidence = Some(receipt);
            }
            if let Some(reason) = runtime_failure.as_ref() {
                if verified_evidence.is_none() || !offline_research_allowed(&queued.request) {
                    return Err(SidecarError::Message(reason.clone()));
                }
            }

            if queued.request.strategy_profile.contains("grok_builder") {
                consume_turn(&mut turns_remaining)?;
                match query_grok_builder_context(
                    &queued.request.objective,
                    &evidence_prompt,
                    &queued.request.run_contract,
                ) {
                    Ok(context) => {
                        tool_calls.push(AgentToolCallRecord {
                            name: "xai__grok_chat_completions".to_string(),
                            status: if context.is_some() {
                                "called"
                            } else {
                                "not_configured"
                            }
                            .to_string(),
                        });
                        grok_context = context;
                    }
                    Err(err) => {
                        tool_calls.push(AgentToolCallRecord {
                            name: "xai__grok_chat_completions".to_string(),
                            status: "failed".to_string(),
                        });
                        focused_results.push(FocusedResult {
                            profile: "grok-evidence".to_string(),
                            status: "partial".to_string(),
                            summary: format!("Grok API failed: {err}"),
                            tool_calls: Vec::new(),
                        });
                    }
                }
            }

            for profile in select_profiles(queued, &harness_context) {
                consume_turn(&mut turns_remaining)?;
                let result = self.run_focused_subagent(
                    profile,
                    queued,
                    &runtime.wire,
                    &harness_context,
                    &evidence_prompt,
                );
                tool_calls.push(AgentToolCallRecord {
                    name: format!("subagent__{profile}"),
                    status: result.status.clone(),
                });
                tool_calls.extend(result.tool_calls.clone());
                focused_results.push(result);
            }

            consume_turn(&mut turns_remaining)?;
            if self.config.engine == "grok" {
                let (result, context) = query_grok_strategy_completion(
                    &queued.request.objective,
                    &evidence_prompt,
                    &queued.request.run_contract,
                    &runtime.wire,
                    &harness_context,
                )?;
                session_id = Some(format!("xai:{}", context.model));
                completion = Some(result);
                tool_calls.push(AgentToolCallRecord {
                    name: "xai__grok_chat_completions".to_string(),
                    status: "called".to_string(),
                });
                grok_context = Some(context);
            } else {
                let focused_json = serde_json::to_value(&focused_results)?;
                let (result, codex) = query_codex_strategy_completion(
                    &queued.request.objective,
                    &evidence_prompt,
                    &queued.request.run_contract,
                    &runtime.wire,
                    &harness_context,
                    &focused_json,
                    grok_context.as_ref(),
                )?;
                session_id = Some(codex.session_id);
                completion = Some(result);
                tool_calls.push(AgentToolCallRecord {
                    name: "codex_cli__exec".to_string(),
                    status: "called".to_string(),
                });
                tool_calls.extend(codex.tool_calls);
            }
            Ok(())
        })();
        if let Err(err) = execution {
            failure_reason = Some(err.to_string());
        }

        let mut request = serde_json::to_value(&queued.request)?;
        if let Some(request) = request.as_object_mut() {
            request.insert("queue_attempt".to_string(), json!(queued.attempt()));
        }
        let subagents = json!({
            "focused_subagents": focused_results,
            "grok_api": grok_context,
        });
        Ok(build_run_record(RunRecordParams {
            run_id: queued.run_id.clone(),
            cycle_kind: "agentic_strategy".to_string(),
            started_at,
            finished_at: Some(Utc::now()),
            session_id,
            model: if self.config.engine == "grok" {
                format!(
                    "xai:{}",
                    grok_context
                        .as_ref()
                        .map(|context| context.model.clone())
                        .or_else(|| nonempty_env("XAI_MODEL"))
                        .unwrap_or_else(|| "grok-4.5".to_string())
                )
            } else {
                format!(
                    "codex-cli:{}",
                    nonempty_env("CODEX_CLI_MODEL").unwrap_or_else(|| "default".to_string())
                )
            },
            runtime,
            tool_calls,
            structured_output: None,
            failure_reason,
            completion,
            request: Some(request),
            harness_subagents: Some(subagents),
            evidence: verified_evidence,
        }))
    }

    fn run_focused_subagent(
        &self,
        profile: &str,
        queued: &QueuedAgentRunRequest,
        runtime_context: &Value,
        harness_context: &str,
        evidence_packet: &str,
    ) -> FocusedResult {
        let prompt = if profile == "grok-evidence" {
            format!(
                "Collect only sports, X, and market evidence for this Strategy Builder run. Return a compact evidence summary.\n\nObjective:\n{}\n\nRun contract:\n{}",
                queued.request.objective, queued.request.run_contract
            )
        } else {
            format!(
                "Collect only replay, backtest, config comparison, and oversight parity evidence for this Strategy Builder run.\n\nObjective:\n{}\n\nRun contract:\n{}",
                queued.request.objective, queued.request.run_contract
            )
        };
        let prompt = format!("{prompt}\n\nParent evidence packet summary:\n{evidence_packet}");
        match query_codex_focused_subagent(profile, &prompt, runtime_context, harness_context) {
            Ok((completion, codex)) => {
                let mut tool_calls = vec![AgentToolCallRecord {
                    name: format!("codex_cli__{profile}"),
                    status: "called".to_string(),
                }];
                tool_calls.extend(codex.tool_calls);
                FocusedResult {
                    profile: profile.to_string(),
                    status: completion.status,
                    summary: completion.summary,
                    tool_calls,
                }
            }
            Err(err) => FocusedResult {
                profile: profile.to_string(),
                status: "failed".to_string(),
                summary: err.to_string(),
                tool_calls: vec![AgentToolCallRecord {
                    name: format!("codex_cli__{profile}"),
                    status: "failed".to_string(),
                }],
            },
        }
    }

    fn run_scan_cycle(&self) -> Result<AgentRunRecord> {
        let started_at = Utc::now();
        let runtime_load = self.runtime_context();
        let runtime = runtime_load.runtime;
        let harness_context = self.read_harness_context(6_000)?;
        let mut session_id = None;
        let mut tool_calls = runtime.receipts.clone();
        let mut structured_output = None;
        let mut failure_reason = runtime_load.failure_reason;
        if failure_reason.is_none() {
            match query_codex_scan(&started_at.to_rfc3339(), &runtime.wire, &harness_context) {
                Ok(result) => {
                    session_id = Some(result.session_id);
                    tool_calls.extend(result.tool_calls);
                    tool_calls.push(AgentToolCallRecord {
                        name: "codex_cli__exec".to_string(),
                        status: "called".to_string(),
                    });
                    structured_output = result.value.is_object().then_some(result.value);
                }
                Err(err) => failure_reason = Some(err.to_string()),
            }
        }
        Ok(build_run_record(RunRecordParams {
            run_id: Uuid::new_v4().to_string(),
            cycle_kind: "research_oversight".to_string(),
            started_at,
            finished_at: Some(Utc::now()),
            session_id,
            model: format!(
                "codex-cli:{}",
                nonempty_env("CODEX_CLI_MODEL").unwrap_or_else(|| "default".to_string())
            ),
            runtime,
            tool_calls,
            structured_output,
            failure_reason,
            completion: None,
            request: None,
            harness_subagents: None,
            evidence: None,
        }))
    }

    fn runtime_context(&self) -> RuntimeContextLoad {
        let system_result = self.client.live_system_snapshot();
        let deployments_result = self.client.live_deployment_summaries();
        let trading_result = self.client.live_trading_state();
        let receipts = vec![
            read_receipt("parent__get_system_status", system_result.is_ok()),
            read_receipt("parent__list_deployments", deployments_result.is_ok()),
            read_receipt("parent__get_trading_state", trading_result.is_ok()),
        ];
        let failures = [
            ("system status", system_result.as_ref().err()),
            ("deployment summaries", deployments_result.as_ref().err()),
            ("trading state", trading_result.as_ref().err()),
        ]
        .into_iter()
        .filter_map(|(label, error)| error.map(|error| format!("{label}: {error}")))
        .collect::<Vec<_>>();
        let system = system_result.ok();
        let deployments = deployments_result.ok();
        let trading = trading_result.ok();
        let platform_status = system.as_ref().map(|status| status.status.clone());
        let deployment_count = deployments.as_ref().map(Vec::len).unwrap_or_default();
        RuntimeContextLoad {
            runtime: RuntimeContext {
                wire: sanitize_runtime_context(
                    system.as_ref(),
                    deployments.as_deref(),
                    trading.as_deref(),
                ),
                platform_status,
                deployment_count,
                receipts,
            },
            failure_reason: (!failures.is_empty()).then(|| {
                format!(
                    "live control-plane context unavailable; model execution skipped: {}",
                    failures.join("; ")
                )
            }),
        }
    }

    fn read_harness_context(&self, max_chars: usize) -> Result<String> {
        append_text_sync(&self.store.harness_events_path, "")?;
        let body = match fs::read_to_string(&self.store.harness_context_path) {
            Ok(body) => body,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                write_text_sync(&self.store.harness_context_path, DEFAULT_HARNESS_CONTEXT)?;
                DEFAULT_HARNESS_CONTEXT.to_string()
            }
            Err(err) => return Err(err.into()),
        };
        Ok(last_chars(&body, max_chars))
    }

    fn record_run(&self, record: &AgentRunRecord) -> Result<()> {
        append_jsonl_sync(&self.store.runs_path, record)?;
        if let Some(learning) = learning_from_record(record) {
            if let Err(err) = self.record_harness_learning(&learning) {
                eprintln!(
                    "failed to record harness learning for {}: {err}",
                    record.run_id
                );
            }
        }
        Ok(())
    }

    fn record_harness_learning(&self, learning: &HarnessLearning) -> Result<()> {
        append_jsonl_sync(&self.store.harness_events_path, learning)?;
        append_text_sync(
            &self.store.harness_context_path,
            &format!(
                "\n## {} {}\n\n- run: {} ({})\n- summary: {}\n- suggested_change: {}\n{}",
                learning.created_at,
                learning.category,
                learning.run_id,
                learning.cycle_kind,
                learning.summary,
                learning.suggested_change,
                learning
                    .subagent_profile
                    .as_ref()
                    .map(|value| format!("- subagent_profile: {value}\n"))
                    .unwrap_or_default()
            ),
        )
    }
}

#[derive(Debug, Clone, Serialize)]
struct FocusedResult {
    profile: String,
    status: String,
    summary: String,
    #[serde(rename = "toolCalls")]
    tool_calls: Vec<AgentToolCallRecord>,
}

#[derive(Debug, Clone)]
struct RuntimeContext {
    wire: Value,
    platform_status: Option<String>,
    deployment_count: usize,
    receipts: Vec<AgentToolCallRecord>,
}

#[derive(Debug, Clone)]
struct RuntimeContextLoad {
    runtime: RuntimeContext,
    failure_reason: Option<String>,
}

fn read_receipt(name: &str, succeeded: bool) -> AgentToolCallRecord {
    AgentToolCallRecord {
        name: name.to_string(),
        status: if succeeded { "completed" } else { "failed" }.to_string(),
    }
}

fn sanitize_runtime_context(
    system: Option<&SystemStatus>,
    deployments: Option<&[DeploymentSummary]>,
    trading: Option<&[TradingStateSnapshot]>,
) -> Value {
    let system = system.map(|status| {
        json!({
            "status": status.status,
            "uptime_seconds": status.uptime_seconds,
            "error_count_1h": status.error_count_1h,
        })
    });
    let deployments = deployments.map(|items| {
        json!({
            "total": items.len(),
            "running": items.iter().filter(|item| item.desired_state == ploy_operator_contracts::DesiredState::Running).count(),
            "paused": items.iter().filter(|item| item.desired_state == ploy_operator_contracts::DesiredState::Paused).count(),
            "stopped": items.iter().filter(|item| item.desired_state == ploy_operator_contracts::DesiredState::Stopped).count(),
            "sample": items.iter().take(12).map(|item| json!({
                "deployment_id": item.deployment_id,
                "runtime_mode": item.runtime_mode,
                "desired_state": item.desired_state,
                "observed_state": item.observed_state,
            })).collect::<Vec<_>>(),
        })
    });
    let trading = trading.map(|items| {
        let gross_exposure = items
            .iter()
            .filter_map(|item| item.risk.gross_exposure.to_string().parse::<f64>().ok())
            .sum::<f64>();
        let net_pnl = items
            .iter()
            .filter_map(|item| item.pnl.net_pnl.to_string().parse::<f64>().ok())
            .sum::<f64>();
        json!({
            "tracked_deployments": items.len(),
            "pending_intents": items.iter().map(|item| item.risk.pending_intents).sum::<usize>(),
            "active_orders": items.iter().map(|item| item.risk.active_orders).sum::<usize>(),
            "open_positions": items.iter().map(|item| item.risk.open_positions).sum::<usize>(),
            "gross_exposure": gross_exposure,
            "net_pnl": net_pnl,
            "sample": items.iter().take(12).map(|item| json!({
                "deployment_id": item.deployment_id,
                "runtime_mode": item.runtime_mode,
                "pending_intents": item.risk.pending_intents,
                "active_orders": item.risk.active_orders,
                "open_positions": item.risk.open_positions,
                "net_pnl": item.pnl.net_pnl,
            })).collect::<Vec<_>>(),
        })
    });
    json!({
        "system": system,
        "deployments": deployments,
        "trading": trading,
    })
}

fn summarize_request(request: Option<&Value>) -> Value {
    let Some(request) = request else {
        return Value::Null;
    };
    json!({
        "objective": request.get("objective"),
        "strategy_profile": request.get("strategy_profile"),
        "autonomy_mode": request.get("autonomy_mode"),
        "target_evidence": request.get("target_evidence"),
        "symbols": request.get("symbols"),
        "prediction_scope": request.get("prediction_scope"),
        "max_turns": request.get("max_turns"),
        "budget_usd": request.get("budget_usd"),
        "queue_attempt": request.get("queue_attempt"),
    })
}

fn deployment_sample(runtime: &Value) -> Vec<String> {
    runtime
        .pointer("/deployments/sample")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("deployment_id").and_then(Value::as_str))
        .take(8)
        .map(str::to_string)
        .collect()
}

fn summarize_output_items(output: Option<&Value>, key: &str, fields: &[&str]) -> Vec<String> {
    output
        .and_then(|value| value.get(key))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(12)
        .map(|item| {
            fields
                .iter()
                .map(|field| item.get(field).and_then(Value::as_str).unwrap_or("unknown"))
                .collect::<Vec<_>>()
                .join(":")
        })
        .collect()
}

struct RunRecordParams {
    run_id: String,
    cycle_kind: String,
    started_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
    session_id: Option<String>,
    model: String,
    runtime: RuntimeContext,
    tool_calls: Vec<AgentToolCallRecord>,
    structured_output: Option<Value>,
    failure_reason: Option<String>,
    completion: Option<AgentTaskCompletion>,
    request: Option<Value>,
    harness_subagents: Option<Value>,
    evidence: Option<VerifiedPredictionEvidenceReceipt>,
}

fn build_run_record(params: RunRecordParams) -> AgentRunRecord {
    let contract_evaluation = evaluate_agent_run_contract_with_evidence(
        params.request.as_ref(),
        &params.tool_calls,
        params.completion.as_ref(),
        params.failure_reason.as_deref(),
        params.evidence.as_ref(),
    );
    let structured_evaluation = params
        .structured_output
        .as_ref()
        .map(evaluate_structured_output);
    let evaluation = contract_evaluation
        .as_ref()
        .and_then(|value| serde_json::to_value(value).ok())
        .or_else(|| structured_evaluation.clone());
    let status = run_status(
        params.failure_reason.as_deref(),
        params.completion.as_ref(),
        params.finished_at.is_some(),
        contract_evaluation.as_ref(),
    );
    let deployment_sample = deployment_sample(&params.runtime.wire);
    let runtime_context = Some(json!({
        "deployment_sample": deployment_sample,
        "oversight_signal_summary": [],
        "oversight_playbook_summary": [],
        "diagnostic_candidates": [],
        "request": summarize_request(params.request.as_ref()),
        "platform_summary": params.runtime.wire,
    }));
    let output_summary = if params.structured_output.is_some()
        || params.completion.is_some()
        || contract_evaluation.is_some()
        || params.evidence.is_some()
        || params.harness_subagents.is_some()
    {
        Some(json!({
            "contract_evaluation": contract_evaluation,
            "verified_prediction_evidence": params.evidence,
            "task_completion": params.completion,
            "research_report_summaries": summarize_output_items(
                params.structured_output.as_ref(),
                "research_reports",
                &["kind", "subject", "status"],
            ),
            "oversight_alert_summaries": summarize_output_items(
                params.structured_output.as_ref(),
                "oversight_alerts",
                &["severity", "kind", "deployment_id"],
            ),
            "operator_recommendation_summaries": summarize_output_items(
                params.structured_output.as_ref(),
                "operator_recommendations",
                &["kind", "target"],
            ),
            "harness_subagents": params.harness_subagents.unwrap_or_else(|| json!([])),
        }))
    } else {
        None
    };
    let research_reports = params
        .structured_output
        .as_ref()
        .map(|output| array_len(output, "research_reports"))
        .unwrap_or_default();
    let oversight_alerts = params
        .structured_output
        .as_ref()
        .map(|output| array_len(output, "oversight_alerts"))
        .unwrap_or_default();
    let operator_recommendations = params
        .structured_output
        .as_ref()
        .map(|output| array_len(output, "operator_recommendations"))
        .unwrap_or_default();
    let mut record = AgentRunRecord {
        run_id: params.run_id,
        cycle_kind: params.cycle_kind,
        status,
        started_at: params.started_at,
        finished_at: params.finished_at,
        session_id: params.session_id,
        model: params.model,
        platform_status: params.runtime.platform_status,
        deployment_count: params.runtime.deployment_count,
        oversight_signal_count: 0,
        oversight_playbook_count: 0,
        total_cost_usd: None,
        tool_calls: params.tool_calls,
        research_reports,
        oversight_alerts,
        operator_recommendations,
        failure_reason: params.failure_reason,
        runtime_context,
        output_summary,
        evaluation,
    };
    if let Some(learning) = derive_harness_learning(&record) {
        let mut summary = record
            .output_summary
            .take()
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        summary.insert(
            "harness_learning".to_string(),
            serde_json::to_value(learning).unwrap_or(Value::Null),
        );
        record.output_summary = Some(Value::Object(summary));
    }
    record
}

fn run_status(
    failure_reason: Option<&str>,
    completion: Option<&AgentTaskCompletion>,
    finished: bool,
    contract: Option<&ContractEvaluation>,
) -> String {
    if failure_reason.is_some() {
        return "failed".to_string();
    }
    if completion.map(|value| value.status.as_str()) == Some("blocked")
        || contract.map(|value| value.status.as_str()) == Some("blocked")
    {
        return "blocked".to_string();
    }
    if contract.map(|value| value.status.as_str()) == Some("needs_retry") {
        return "needs_retry".to_string();
    }
    if completion.map(|value| value.status.as_str()) == Some("partial") {
        return "partial".to_string();
    }
    match contract.map(|value| value.status.as_str()) {
        Some("passed") if finished => "succeeded".to_string(),
        Some("passed") => "started".to_string(),
        Some(_) => "failed".to_string(),
        None if finished => "succeeded".to_string(),
        None => "started".to_string(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HarnessLearning {
    kind: String,
    run_id: String,
    cycle_kind: String,
    category: String,
    summary: String,
    suggested_change: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    subagent_profile: Option<String>,
    created_at: String,
}

fn derive_harness_learning(record: &AgentRunRecord) -> Option<HarnessLearning> {
    let summary = record.output_summary.as_ref()?.as_object()?;
    let evaluation = summary.get("contract_evaluation")?.as_object();
    let checks = evaluation
        .and_then(|value| value.get("checks"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let needs_retry = checks
        .iter()
        .find(|check| check.get("status").and_then(Value::as_str) == Some("needs_retry"));
    let blocked = checks
        .iter()
        .find(|check| check.get("status").and_then(Value::as_str) == Some("blocked"));

    if record.status == "needs_retry" {
        let check = needs_retry?;
        let name = check.get("name").and_then(Value::as_str).unwrap_or("");
        let detail = check
            .get("detail")
            .and_then(Value::as_str)
            .unwrap_or("contract check did not pass");
        let (category, suggested_change, subagent_profile) = match name {
            "completion_signal" => (
                "completion_gap",
                "Tighten the run prompt or completion sentinel for this strategy family.",
                Some("completion-sentinel"),
            ),
            "grok_decision" | "grok_evidence_tools" => (
                "tool_gap",
                "Split Grok evidence collection into a focused profile before synthesis.",
                Some("grok-evidence"),
            ),
            "executable_replay" | "runtime_parity" => (
                "tool_gap",
                "Route replay and parity evidence to a focused verification profile.",
                Some("replay-parity"),
            ),
            _ => (
                "tool_gap",
                "Review the missing tool or context before increasing retry count.",
                None,
            ),
        };
        return Some(learning(
            record,
            category,
            detail,
            suggested_change,
            subagent_profile,
        ));
    }
    if record.status == "blocked" {
        let detail = blocked?
            .get("detail")
            .and_then(Value::as_str)
            .unwrap_or("blocked by policy");
        return Some(learning(
            record,
            "approval_gate",
            detail,
            "Keep this as a human approval or policy decision; do not add mutation tools.",
            None,
        ));
    }
    if record.status == "failed" {
        return record.failure_reason.as_deref().map(|reason| {
            learning(
                record,
                "runtime_error",
                reason,
                "Add a narrow recovery note or health check only if this error repeats.",
                Some("runtime-recovery"),
            )
        });
    }
    if record.status == "partial" {
        let detail = summary
            .get("task_completion")
            .and_then(|value| value.get("summary"))
            .and_then(Value::as_str)
            .unwrap_or("partial completion");
        return Some(learning(
            record,
            "negative_result",
            detail,
            "Preserve as negative evidence; retry only when the blocker changes.",
            None,
        ));
    }
    None
}

fn learning(
    record: &AgentRunRecord,
    category: &str,
    summary: &str,
    suggested_change: &str,
    subagent_profile: Option<&str>,
) -> HarnessLearning {
    HarnessLearning {
        kind: "harness_learning".to_string(),
        run_id: record.run_id.clone(),
        cycle_kind: record.cycle_kind.clone(),
        category: category.to_string(),
        summary: summary.to_string(),
        suggested_change: suggested_change.to_string(),
        subagent_profile: subagent_profile.map(str::to_string),
        created_at: Utc::now().to_rfc3339(),
    }
}

fn learning_from_record(record: &AgentRunRecord) -> Option<HarnessLearning> {
    record
        .output_summary
        .as_ref()?
        .get("harness_learning")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

fn select_profiles<'a>(
    queued: &'a QueuedAgentRunRequest,
    harness_context: &'a str,
) -> Vec<&'static str> {
    let mut profiles = Vec::new();
    if queued.request.strategy_profile.contains("grok_builder")
        || contract_enabled(&queued.request.run_contract, "requires_grok_decision")
        || harness_context.contains("subagent_profile: grok-evidence")
    {
        profiles.push("grok-evidence");
    }
    if contract_enabled(&queued.request.run_contract, "requires_executable_replay")
        || contract_enabled(&queued.request.run_contract, "requires_runtime_parity")
        || harness_context.contains("subagent_profile: replay-parity")
    {
        profiles.push("replay-parity");
    }
    profiles.truncate(2);
    profiles
}

fn contract_enabled(contract: &str, key: &str) -> bool {
    contract.lines().any(|line| {
        line.trim()
            .split_once('=')
            .map(|(candidate, value)| candidate.trim() == key && value.trim() == "true")
            .unwrap_or(false)
    })
}

fn retry_reason(record: &AgentRunRecord) -> String {
    record
        .output_summary
        .as_ref()
        .and_then(|value| value.pointer("/contract_evaluation/checks"))
        .and_then(Value::as_array)
        .and_then(|checks| {
            checks.iter().find_map(|check| {
                (check.get("status").and_then(Value::as_str) == Some("needs_retry")).then(|| {
                    format!(
                        "{}: {}",
                        check
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("contract"),
                        check
                            .get("detail")
                            .and_then(Value::as_str)
                            .unwrap_or("needs retry")
                    )
                })
            })
        })
        .unwrap_or_else(|| "contract evaluation requested retry".to_string())
}

fn mark_retry_exhausted(
    record: &mut AgentRunRecord,
    queue_attempt: u32,
    max_retries: u32,
    reason: &str,
) {
    let failure = format!(
        "retry exhausted at queue attempt {queue_attempt} (max_retries={max_retries}): {reason}"
    );
    record.status = "failed".to_string();
    record.failure_reason = Some(failure.clone());
    let mut summary = record
        .output_summary
        .take()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    summary.remove("harness_learning");
    summary.insert(
        "retry_exhausted".to_string(),
        json!({
            "queue_attempt": queue_attempt,
            "max_retries": max_retries,
            "reason": reason,
        }),
    );
    record.output_summary = Some(Value::Object(summary));
    if let Some(learning) = derive_harness_learning(record) {
        record
            .output_summary
            .as_mut()
            .and_then(Value::as_object_mut)
            .expect("retry summary is an object")
            .insert(
                "harness_learning".to_string(),
                serde_json::to_value(learning).unwrap_or(Value::Null),
            );
    }
}

fn consume_turn(turns_remaining: &mut u32) -> Result<()> {
    if *turns_remaining == 0 {
        return Err(SidecarError::Message(
            "agent run max_turns exhausted".to_string(),
        ));
    }
    *turns_remaining -= 1;
    Ok(())
}

pub fn run_awaited_poll_loop<C, W, S>(
    mut cycle: C,
    mut wait: W,
    mut should_continue: S,
) -> Result<()>
where
    C: FnMut() -> Result<()>,
    W: FnMut(),
    S: FnMut() -> bool,
{
    while should_continue() {
        cycle()?;
        if should_continue() {
            wait();
        }
    }
    Ok(())
}

pub fn run() -> Result<()> {
    let mut sidecar = Sidecar::from_env()?;
    eprintln!("ploy-agent-sidecar started");
    eprintln!("engine={}", sidecar.config.engine);
    eprintln!("dry_run={}", sidecar.config.dry_run);
    eprintln!("scan_enabled={}", sidecar.config.scan_enabled);
    eprintln!("poll_interval={}s", sidecar.config.poll_interval.as_secs());
    eprintln!("max_turns={}", sidecar.config.admission.max_turns);
    eprintln!("max_budget_usd={}", sidecar.config.admission.max_budget_usd);
    let poll_interval = sidecar.config.poll_interval;
    run_awaited_poll_loop(
        || sidecar.run_cycle(),
        || thread::sleep(poll_interval),
        || true,
    )
}

fn control_plane_addr() -> Result<String> {
    let raw = nonempty_env("PLOY_API_URL").unwrap_or_else(|| "http://localhost:8081".to_string());
    control_plane_addr_from_url(&raw)
}

fn verify_queued_prediction_evidence(
    refs: &ploy_research::PredictionEvidenceRefs,
    allowed_root: &Path,
) -> std::result::Result<VerifiedPredictionEvidenceReceipt, String> {
    let allowed_root = if allowed_root.is_absolute() {
        allowed_root.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("resolve configured prediction evidence root: {error}"))?
            .join(allowed_root)
    };
    let allowed_root = fs::canonicalize(&allowed_root)
        .map_err(|error| format!("canonicalize configured prediction evidence root: {error}"))?;
    let allowed_metadata = std::fs::symlink_metadata(&allowed_root)
        .map_err(|error| format!("inspect configured prediction evidence root: {error}"))?;
    if !allowed_metadata.is_dir() || allowed_metadata.file_type().is_symlink() {
        return Err(
            "configured prediction evidence root must be a non-symlink directory".to_string(),
        );
    }
    let requested_root = PathBuf::from(&refs.artifact_root);
    if !requested_root.is_absolute()
        || requested_root
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(format!(
            "prediction evidence artifact_root must remain under the configured local root {}",
            allowed_root.display()
        ));
    }
    let requested_root = fs::canonicalize(&requested_root)
        .map_err(|error| format!("canonicalize prediction evidence artifact_root: {error}"))?;
    if !requested_root.starts_with(&allowed_root) {
        return Err(format!(
            "prediction evidence artifact_root must remain under the configured local root {}",
            allowed_root.display()
        ));
    }
    verify_prediction_evidence(refs)
}

fn offline_research_allowed(request: &ploy_operator_contracts::AgentRunCreateRequest) -> bool {
    request.autonomy_mode == "research_until_blocked"
        && matches!(
            request.target_evidence.as_str(),
            "diagnostic" | "factor_attribution"
        )
}

fn control_plane_addr_from_url(raw: &str) -> Result<String> {
    let address = raw.strip_prefix("http://").ok_or_else(|| {
        SidecarError::Message("PLOY_API_URL must be an http:// control-plane URL".to_string())
    })?;
    let address = address.trim_end_matches('/');
    if address.is_empty()
        || address.contains('/')
        || address.contains('@')
        || address.contains('?')
        || address.contains('#')
    {
        return Err(SidecarError::Message(format!(
            "PLOY_API_URL must not contain a path: {raw}"
        )));
    }
    let (host, port) = if let Some(rest) = address.strip_prefix('[') {
        let (host, suffix) = rest.split_once(']').ok_or_else(|| {
            SidecarError::Message(format!("PLOY_API_URL has an invalid IPv6 host: {raw}"))
        })?;
        let port = suffix.strip_prefix(':').ok_or_else(|| {
            SidecarError::Message(format!("PLOY_API_URL must include a port: {raw}"))
        })?;
        (host, port)
    } else {
        address.rsplit_once(':').ok_or_else(|| {
            SidecarError::Message(format!("PLOY_API_URL must include a port: {raw}"))
        })?
    };
    if port.parse::<u16>().is_err() {
        return Err(SidecarError::Message(format!(
            "PLOY_API_URL has an invalid port: {raw}"
        )));
    }
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    if !loopback {
        return Err(SidecarError::Message(
            "PLOY_API_URL must use loopback HTTP; remote control-plane transport requires TLS"
                .to_string(),
        ));
    }
    Ok(address.to_string())
}

fn last_chars(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    value
        .chars()
        .skip(count.saturating_sub(max_chars))
        .collect()
}

fn nonempty_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn require_sidecar_auth_token(value: Option<String>) -> Result<String> {
    value.ok_or_else(|| {
        SidecarError::Message(
            "PLOY_SIDECAR_AUTH_TOKEN is required for live read-only control-plane access"
                .to_string(),
        )
    })
}

fn env_u64(key: &str, default: u64) -> u64 {
    nonempty_env(key)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_bool(key: &str, default: bool) -> bool {
    match nonempty_env(key).as_deref() {
        Some("1" | "true" | "TRUE" | "True") => true,
        Some("0" | "false" | "FALSE" | "False") => false,
        Some(_) | None => default,
    }
}

const DEFAULT_HARNESS_CONTEXT: &str = "# Harness Meta-Context\n\nThis file is maintained by the Rust sidecar from completed agent runs.\n\n## Guardrails\n\n- Live trading, deployment changes, and paper intents stay approval-gated.\n- Treat repeated needs_retry as a harness/tool/prompt gap, not as success.\n- Candidate profile changes are proposals until a human lands code.\n";

#[cfg(test)]
mod tests {
    use super::*;
    use ploy_operator_contracts::{
        AgentRunCreateRequest, DeploymentRuntimeMode, DeploymentState, DesiredState, ObservedState,
    };
    use std::cell::Cell;

    #[test]
    fn awaited_poll_cycles_do_not_overlap() {
        let active = Cell::new(0);
        let maximum = Cell::new(0);
        let completed = Cell::new(0);
        run_awaited_poll_loop(
            || {
                active.set(active.get() + 1);
                maximum.set(maximum.get().max(active.get()));
                active.set(active.get() - 1);
                completed.set(completed.get() + 1);
                Ok(())
            },
            || {},
            || completed.get() < 3,
        )
        .expect("poll loop");
        assert_eq!(maximum.get(), 1);
        assert_eq!(completed.get(), 3);
    }

    #[test]
    fn contract_pass_cannot_upgrade_partial_or_blocked_completion() {
        let contract = ContractEvaluation {
            kind: "agent_run_contract".to_string(),
            status: "passed".to_string(),
            checks: vec![],
        };
        let mut completion = AgentTaskCompletion {
            status: "partial".to_string(),
            summary: "incomplete".to_string(),
            decision: None,
            grok_decision: None,
            evidence: vec![],
            blockers: vec![],
            next_action: None,
        };
        assert_eq!(
            run_status(None, Some(&completion), true, Some(&contract)),
            "partial"
        );
        completion.status = "blocked".to_string();
        assert_eq!(
            run_status(None, Some(&completion), true, Some(&contract)),
            "blocked"
        );
    }

    #[test]
    fn control_plane_http_is_loopback_only() {
        assert_eq!(
            control_plane_addr_from_url("http://127.0.0.1:8081").expect("IPv4 loopback"),
            "127.0.0.1:8081"
        );
        assert_eq!(
            control_plane_addr_from_url("http://[::1]:8081").expect("IPv6 loopback"),
            "[::1]:8081"
        );
        assert!(control_plane_addr_from_url("http://10.0.0.5:8081").is_err());
        assert!(control_plane_addr_from_url("https://localhost:8081").is_err());
    }

    #[test]
    fn sidecar_auth_token_is_mandatory() {
        assert!(require_sidecar_auth_token(None)
            .expect_err("missing token")
            .to_string()
            .contains("PLOY_SIDECAR_AUTH_TOKEN is required"));
        assert_eq!(
            require_sidecar_auth_token(Some("sidecar-secret".to_string())).expect("token"),
            "sidecar-secret"
        );
    }

    #[test]
    fn approval_gate_learning_never_proposes_mutation() {
        let mut record = AgentRunRecord {
            run_id: "test".to_string(),
            cycle_kind: "agentic_strategy".to_string(),
            status: "blocked".to_string(),
            started_at: Utc::now(),
            finished_at: Some(Utc::now()),
            session_id: None,
            model: "test".to_string(),
            platform_status: None,
            deployment_count: 0,
            oversight_signal_count: 0,
            oversight_playbook_count: 0,
            total_cost_usd: None,
            tool_calls: vec![],
            research_reports: 0,
            oversight_alerts: 0,
            operator_recommendations: 0,
            failure_reason: None,
            runtime_context: None,
            output_summary: Some(json!({
                "contract_evaluation": {
                    "checks": [{"name":"approval_gate","status":"blocked","detail":"denied"}]
                }
            })),
            evaluation: None,
        };
        let learning = derive_harness_learning(&record).expect("learning");
        assert_eq!(learning.category, "approval_gate");
        assert!(learning
            .suggested_change
            .contains("do not add mutation tools"));
        record.output_summary = None;
        assert!(derive_harness_learning(&record).is_none());
    }

    #[test]
    fn exhausted_retry_becomes_an_explicit_terminal_failure() {
        let runtime = RuntimeContext {
            wire: json!({}),
            platform_status: None,
            deployment_count: 0,
            receipts: vec![],
        };
        let completion = AgentTaskCompletion {
            status: "partial".to_string(),
            summary: "missing evidence".to_string(),
            decision: Some("blocked".to_string()),
            grok_decision: Some("not_queried".to_string()),
            evidence: vec![],
            blockers: vec!["missing evidence".to_string()],
            next_action: Some("retry".to_string()),
        };
        let mut record = build_run_record(RunRecordParams {
            run_id: "retry-exhausted".to_string(),
            cycle_kind: "agentic_strategy".to_string(),
            started_at: Utc::now(),
            finished_at: Some(Utc::now()),
            session_id: None,
            model: "test".to_string(),
            runtime,
            tool_calls: vec![],
            structured_output: None,
            failure_reason: None,
            completion: Some(completion),
            request: Some(json!({
                "run_contract": "completion_signal = \"required\"",
                "queue_attempt": 1,
            })),
            harness_subagents: None,
            evidence: None,
        });
        assert_eq!(record.status, "needs_retry");
        assert!(record
            .runtime_context
            .as_ref()
            .and_then(|value| value.get("deployment_sample"))
            .is_some());
        assert!(record
            .output_summary
            .as_ref()
            .and_then(|value| value.get("research_report_summaries"))
            .is_some());
        mark_retry_exhausted(&mut record, 1, 1, "completion missing");
        assert_eq!(record.status, "failed");
        assert!(record
            .failure_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("retry exhausted")));
        assert_eq!(
            record
                .output_summary
                .as_ref()
                .and_then(|value| value.pointer("/retry_exhausted/max_retries"))
                .and_then(Value::as_u64),
            Some(1)
        );
    }

    #[test]
    fn runtime_records_only_sanitized_aggregates_and_request_summary() {
        let deployment = DeploymentSummary {
            deployment_id: "paper-one".to_string(),
            runtime_mode: DeploymentRuntimeMode::Paper,
            account_id: "secret-account".to_string(),
            max_gross_exposure: None,
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Running,
            observed_state: ObservedState::Running,
        };
        let trading = TradingStateSnapshot {
            deployment_id: "paper-one".to_string(),
            ..TradingStateSnapshot::default()
        };
        let sanitized = sanitize_runtime_context(None, Some(&[deployment]), Some(&[trading]));
        let encoded = sanitized.to_string();
        assert!(encoded.contains("paper-one"));
        for forbidden in [
            "secret-account",
            "venue_order_id",
            "idempotency_key",
            "last_error",
        ] {
            assert!(!encoded.contains(forbidden), "persisted {forbidden}");
        }
        for raw_collection in ["intents", "orders", "fills", "positions"] {
            assert!(
                sanitized
                    .pointer(&format!("/trading/{raw_collection}"))
                    .is_none(),
                "persisted raw {raw_collection}"
            );
        }

        let request = json!({
            "objective": "bounded",
            "strategy_profile": "test",
            "autonomy_mode": "monitor_only",
            "target_evidence": "diagnostic",
            "symbols": ["BTC"],
            "max_turns": 1,
            "budget_usd": 0.1,
            "queue_attempt": 2,
            "run_packet": "private packet",
            "run_contract": "private contract",
        });
        let summary = summarize_request(Some(&request));
        assert_eq!(summary["queue_attempt"], 2);
        assert!(summary.get("run_packet").is_none());
        assert!(summary.get("run_contract").is_none());
    }

    #[test]
    fn queued_evidence_root_cannot_escape_configured_local_root() {
        let temp = tempfile::tempdir().expect("create temp root");
        let dir = temp.path().to_path_buf();
        let allowed = dir.join("allowed");
        let outside = dir.join("outside");
        fs::create_dir_all(&allowed).expect("allowed root");
        fs::create_dir_all(&outside).expect("outside root");
        let digest = |byte: char| format!("sha256:{}", byte.to_string().repeat(64));
        let mut refs: ploy_research::PredictionEvidenceRefs = serde_json::from_value(json!({
            "artifact_root": outside,
            "mission": {"path":"mission.json","artifact_sha256":digest('a'),"mission_sha256":digest('b')},
            "catalog_partition": {"path":"catalog.json","artifact_sha256":digest('c'),"payload_sha256":digest('d'),"cohort_manifest_id":digest('e'),"partition_digest":digest('f'),"policy_snapshot_id":digest('1')},
            "snapshot": {"path":"snapshot","snapshot_hash":"2".repeat(16),"snapshot_contract_hash":digest('2')},
            "result_bundle": {"path":"result.json","artifact_sha256":digest('3'),"receipt_sha256":digest('4')},
            "reports": [],
            "terminal_receipt": {"path":"result.json","artifact_sha256":digest('3'),"terminal_receipt_sha256":digest('4')}
        }))
        .expect("refs");
        refs.artifact_root = outside.display().to_string();
        let error = verify_queued_prediction_evidence(&refs, &allowed).expect_err("root escape");
        assert!(error.contains("under the configured local root"));
    }

    #[cfg(unix)]
    #[test]
    fn queued_evidence_root_symlink_cannot_escape_configured_local_root() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("create temp root");
        let dir = temp.path().to_path_buf();
        let allowed = dir.join("allowed");
        let outside = dir.join("outside");
        let link = allowed.join("link");
        fs::create_dir_all(&allowed).expect("allowed root");
        fs::create_dir_all(outside.join("job")).expect("outside root");
        symlink(&outside, &link).expect("symlink outside root");
        let digest = |byte: char| format!("sha256:{}", byte.to_string().repeat(64));
        let refs: ploy_research::PredictionEvidenceRefs = serde_json::from_value(json!({
            "artifact_root": link.join("job"),
            "mission": {"path":"mission.json","artifact_sha256":digest('a'),"mission_sha256":digest('b')},
            "catalog_partition": {"path":"catalog.json","artifact_sha256":digest('c'),"payload_sha256":digest('d'),"cohort_manifest_id":digest('e'),"partition_digest":digest('f'),"policy_snapshot_id":digest('1')},
            "snapshot": {"path":"snapshot","snapshot_hash":"2".repeat(16),"snapshot_contract_hash":digest('2')},
            "result_bundle": {"path":"result.json","artifact_sha256":digest('3'),"receipt_sha256":digest('4')},
            "reports": [],
            "terminal_receipt": {"path":"result.json","artifact_sha256":digest('3'),"terminal_receipt_sha256":digest('4')}
        }))
        .expect("refs");
        let error = verify_queued_prediction_evidence(&refs, &allowed)
            .expect_err("symlinked root must be rejected");
        assert!(error.contains("under the configured local root"));
    }

    #[test]
    #[ignore = "requires the production evidence handoff gate"]
    fn production_writer_evidence_queue_reaches_terminal() {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;

        let handoff_parent = std::env::var_os("PLOY_TEST_EVIDENCE_HANDOFF")
            .map(PathBuf::from)
            .expect("PLOY_TEST_EVIDENCE_HANDOFF is required");
        let refs_path = handoff_parent.join("refs.json");
        let refs_value: ploy_operator_contracts::diagnostics::PredictionEvidenceRefs =
            serde_json::from_slice(&fs::read(&refs_path).expect("production refs"))
                .expect("typed production refs");
        let verifier_refs: ploy_research::PredictionEvidenceRefs =
            serde_json::from_value(serde_json::to_value(&refs_value).expect("serialize refs"))
                .expect("verifier refs");
        let expected = verify_prediction_evidence(&verifier_refs).expect("production evidence");

        let mock_path = handoff_parent.join("mock-codex");
        assert_eq!(
            std::env::var_os("CODEX_CLI_BIN").map(PathBuf::from),
            Some(mock_path.clone()),
            "CI must provide the handoff mock path"
        );
        let mock = "#!/bin/sh\nset -eu\nout=\nwhile [ \"$#\" -gt 0 ]; do case \"$1\" in --output-last-message) out=$2; shift 2;; *) shift;; esac; done\ntest -n \"$out\"\n/bin/cat > \"$0.prompt\"\nprintf 'called\\n' >> \"$0.calls\"\n/bin/cat > \"$out\" <<'JSON'\n{\"status\":\"success\",\"summary\":\"Reviewed parent evidence\",\"decision\":\"pass\",\"grok_decision\":\"not_queried\",\"evidence\":[],\"blockers\":[],\"next_action\":\"Preserve research evidence\"}\nJSON\nprintf '%s\\n' '{\"type\":\"turn.completed\"}'\n";
        fs::write(&mock_path, mock).expect("write mock Codex");
        #[cfg(unix)]
        fs::set_permissions(&mock_path, fs::Permissions::from_mode(0o700)).expect("chmod mock");

        let queue_dir = tempfile::tempdir().expect("queue temp");
        let store = QueueStore::for_dir(queue_dir.path());
        let make_request = |run_id: &str,
                            refs: ploy_operator_contracts::diagnostics::PredictionEvidenceRefs,
                            target_evidence: &str,
                            run_contract: &str| {
            QueuedAgentRunRequest {
                run_id: run_id.to_string(),
                created_at: Utc::now().to_rfc3339(),
                request: serde_json::from_value(json!({
                    "objective":"Review the parent-produced prediction evidence",
                    "strategy_profile":"prediction_evidence_review",
                    "autonomy_mode":"research_until_blocked",
                    "target_evidence":target_evidence,
                    "symbols":["BTCUSDT"],
                    "max_turns":3,
                    "budget_usd":1.0,
                    "run_packet":"Review the parent-produced prediction evidence.",
                    "run_contract":run_contract,
                    "prediction_scope":{"product":"BTC","task":"settlement_probability"},
                    "prediction_evidence": refs
                }))
                .expect("queue request"),
                attempt: None,
                last_retry_reason: None,
                last_retried_at: None,
            }
        };
        append_jsonl_sync(
            &store.requests_path,
            &make_request(
                "production-writer-evidence",
                refs_value.clone(),
                "diagnostic",
                "completion_signal = \"required\"\nrequires_full_depth_clob = true\nrequires_operator_approval = true",
            ),
        )
        .expect("queue production evidence request");
        let mut client = ControlPlaneClient::from_runtime_root(queue_dir.path().join("platform"));
        client.control_plane_addr = "127.0.0.1:1".to_string();
        let mut sidecar = Sidecar {
            config: SidecarConfig {
                engine: "codex".to_string(),
                poll_interval: Duration::from_secs(1),
                scan_enabled: false,
                dry_run: true,
                max_retries: 1,
                admission: AdmissionLimits {
                    max_turns: 30,
                    max_budget_usd: 1.0,
                },
                runtime_root: queue_dir.path().join("platform"),
                evidence_root: PathBuf::from(&refs_value.artifact_root),
            },
            store: store.clone(),
            client,
            _worker_lease: store.acquire_worker_lease().expect("worker lease"),
        };
        sidecar.run_cycle().expect("run production evidence queue");
        assert!(!store.requests_path.exists());
        assert!(!store.in_progress_path.exists());
        let records = fs::read_to_string(&store.runs_path).expect("terminal run record");
        let record: AgentRunRecord =
            serde_json::from_str(records.lines().next().unwrap()).expect("parse terminal record");
        assert_eq!(record.status, "succeeded");
        assert!(record.finished_at.is_some() && record.failure_reason.is_none());
        assert!(record
            .tool_calls
            .iter()
            .any(|call| call.name == "codex_cli__exec"));
        let output = record.output_summary.as_ref().expect("output summary");
        assert_eq!(output["contract_evaluation"]["status"], "passed");
        assert_eq!(
            output["verified_prediction_evidence"],
            serde_json::to_value(&expected).unwrap()
        );
        assert!(fs::read_to_string(mock_path.with_extension("prompt"))
            .unwrap()
            .contains(&expected.prompt_summary()));
        assert_eq!(
            fs::read_to_string(mock_path.with_extension("calls"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        sidecar.run_cycle().expect("idempotent empty queue");
        assert_eq!(
            fs::read_to_string(mock_path.with_extension("calls"))
                .unwrap()
                .lines()
                .count(),
            1
        );

        let mut wrong_product = make_request(
            "production-writer-evidence-wrong-product",
            refs_value.clone(),
            "diagnostic",
            "completion_signal = \"required\"\nrequires_full_depth_clob = true\nrequires_operator_approval = true",
        );
        wrong_product.request.symbols = vec!["ETHUSDT".to_string()];
        wrong_product
            .request
            .prediction_scope
            .as_mut()
            .expect("prediction scope")
            .product = "ETH".to_string();
        append_jsonl_sync(&store.requests_path, &wrong_product).expect("queue product mismatch");
        sidecar
            .run_cycle()
            .expect("reject product mismatch before model");
        let records = fs::read_to_string(&store.runs_path).expect("product mismatch records");
        let wrong_product_record = records
            .lines()
            .map(|line| serde_json::from_str::<AgentRunRecord>(line).unwrap())
            .find(|record| record.run_id == "production-writer-evidence-wrong-product")
            .unwrap();
        assert_eq!(wrong_product_record.status, "failed");
        assert!(
            wrong_product_record
                .failure_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("verified Mission product")),
            "{:?}",
            wrong_product_record.failure_reason
        );
        assert_eq!(
            fs::read_to_string(mock_path.with_extension("calls"))
                .unwrap()
                .lines()
                .count(),
            1
        );

        let mut wrong_task = make_request(
            "production-writer-evidence-wrong-task",
            refs_value.clone(),
            "diagnostic",
            "completion_signal = \"required\"\nrequires_full_depth_clob = true\nrequires_operator_approval = true",
        );
        let wrong_task_scope = wrong_task
            .request
            .prediction_scope
            .as_mut()
            .expect("prediction scope");
        wrong_task_scope.task = "up_execution".to_string();
        wrong_task_scope.prediction_horizon_secs = Some(10);
        append_jsonl_sync(&store.requests_path, &wrong_task).expect("queue task mismatch");
        sidecar
            .run_cycle()
            .expect("reject task mismatch before model");
        let records = fs::read_to_string(&store.runs_path).expect("task mismatch records");
        let wrong_task_record = records
            .lines()
            .map(|line| serde_json::from_str::<AgentRunRecord>(line).unwrap())
            .find(|record| record.run_id == "production-writer-evidence-wrong-task")
            .unwrap();
        assert_eq!(wrong_task_record.status, "failed");
        assert!(wrong_task_record
            .failure_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("verified Mission task")));
        assert_eq!(
            fs::read_to_string(mock_path.with_extension("calls"))
                .unwrap()
                .lines()
                .count(),
            1
        );

        let offline_candidate = make_request(
            "production-writer-evidence-offline-candidate",
            refs_value.clone(),
            "dry_run_candidate",
            "completion_signal = \"required\"\nrequires_full_depth_clob = true\nrequires_operator_approval = true",
        );
        append_jsonl_sync(&store.requests_path, &offline_candidate)
            .expect("queue offline candidate");
        sidecar
            .run_cycle()
            .expect("reject offline candidate before model");
        let records = fs::read_to_string(&store.runs_path).expect("offline candidate records");
        let offline_record = records
            .lines()
            .map(|line| serde_json::from_str::<AgentRunRecord>(line).unwrap())
            .find(|record| record.run_id == "production-writer-evidence-offline-candidate")
            .unwrap();
        assert_eq!(offline_record.status, "failed");
        assert!(offline_record
            .failure_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("live control-plane context unavailable")));
        assert_eq!(
            fs::read_to_string(mock_path.with_extension("calls"))
                .unwrap()
                .lines()
                .count(),
            1
        );

        let mut bad_refs = refs_value.clone();
        append_jsonl_sync(
            &store.requests_path,
            &make_request(
                "production-writer-evidence-target-executable",
                refs_value.clone(),
                "executable_replay",
                "completion_signal = \"required\"\nrequires_full_depth_clob = true\nrequires_operator_approval = true",
            ),
        )
        .expect("queue target capability request");
        sidecar
            .run_cycle()
            .expect("reject target capability before model");
        let records = fs::read_to_string(&store.runs_path).expect("target capability records");
        let target_record = records
            .lines()
            .map(|line| serde_json::from_str::<AgentRunRecord>(line).unwrap())
            .find(|record| record.run_id == "production-writer-evidence-target-executable")
            .unwrap();
        assert_eq!(target_record.status, "failed");
        assert!(target_record
            .failure_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("executable_replay")));
        assert_eq!(
            fs::read_to_string(mock_path.with_extension("calls"))
                .unwrap()
                .lines()
                .count(),
            1
        );

        append_jsonl_sync(
            &store.requests_path,
            &make_request(
                "production-writer-evidence-missing-audit",
                refs_value.clone(),
                "diagnostic",
                "completion_signal = \"required\"\nrequires_data_audit = true\nrequires_full_depth_clob = true\nrequires_operator_approval = true",
            ),
        )
        .expect("queue data audit capability request");
        sidecar
            .run_cycle()
            .expect("reject missing audit before model");
        let records = fs::read_to_string(&store.runs_path).expect("audit capability records");
        let audit_record = records
            .lines()
            .map(|line| serde_json::from_str::<AgentRunRecord>(line).unwrap())
            .find(|record| record.run_id == "production-writer-evidence-missing-audit")
            .unwrap();
        assert_eq!(audit_record.status, "failed");
        assert!(audit_record
            .failure_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("data_audit")));
        assert_eq!(
            fs::read_to_string(mock_path.with_extension("calls"))
                .unwrap()
                .lines()
                .count(),
            1
        );

        bad_refs.terminal_receipt.artifact.path = "missing-terminal.json".to_string();
        append_jsonl_sync(
            &store.requests_path,
            &make_request(
                "production-writer-evidence-bad-terminal",
                bad_refs,
                "diagnostic",
                "completion_signal = \"required\"\nrequires_full_depth_clob = true\nrequires_operator_approval = true",
            ),
        )
        .expect("queue bad terminal request");
        sidecar
            .run_cycle()
            .expect("reject bad terminal before model");
        let records = fs::read_to_string(&store.runs_path).expect("terminal records");
        let bad_record = records
            .lines()
            .map(|line| serde_json::from_str::<AgentRunRecord>(line).unwrap())
            .find(|record| record.run_id == "production-writer-evidence-bad-terminal")
            .unwrap();
        assert_eq!(bad_record.status, "failed");
        assert!(bad_record
            .failure_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("prediction evidence rejected")));
        assert_eq!(
            fs::read_to_string(mock_path.with_extension("calls"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        verify_prediction_evidence(&verifier_refs).expect("original refs remain valid");
        fs::write(
            handoff_parent.join("consumer-proof.json"),
            json!({
                "run_id": record.run_id,
                "status": record.status,
                "snapshot_hash": expected.snapshot_hash()
            })
            .to_string(),
        )
        .expect("consumer proof");
    }

    #[test]
    fn run_cycle_consumes_claimed_request_and_records_terminal_result() {
        let temp = tempfile::tempdir().expect("create temp directory");
        let dir = temp.path().to_path_buf();
        let store = QueueStore::for_dir(&dir);
        append_jsonl_sync(
            &store.requests_path,
            &QueuedAgentRunRequest {
                run_id: "run-consumer-proof".to_string(),
                created_at: Utc::now().to_rfc3339(),
                request: AgentRunCreateRequest {
                    objective: "consumer proof".to_string(),
                    strategy_profile: "test".to_string(),
                    autonomy_mode: "research_until_blocked".to_string(),
                    target_evidence: "diagnostic".to_string(),
                    symbols: vec![],
                    max_turns: 3,
                    budget_usd: 1.0,
                    run_packet: "packet".to_string(),
                    run_contract: "completion_signal = \"required\"".to_string(),
                    prediction_scope: None,
                    prediction_evidence: None,
                },
                attempt: None,
                last_retry_reason: None,
                last_retried_at: None,
            },
        )
        .expect("queue request");
        let mut client = ControlPlaneClient::from_runtime_root(dir.join("platform"));
        client.control_plane_addr = "127.0.0.1:1".to_string();
        let mut sidecar = Sidecar {
            config: SidecarConfig {
                engine: "codex".to_string(),
                poll_interval: Duration::from_secs(1),
                scan_enabled: false,
                dry_run: true,
                max_retries: 1,
                admission: AdmissionLimits {
                    max_turns: 30,
                    max_budget_usd: 1.0,
                },
                runtime_root: dir.join("platform"),
                evidence_root: dir.join("evidence"),
            },
            store: store.clone(),
            client,
            _worker_lease: store.acquire_worker_lease().expect("worker lease"),
        };

        sidecar.run_cycle().expect("consume request");
        assert!(!store.in_progress_path.exists());
        assert!(!store.requests_path.exists());
        let records = fs::read_to_string(&store.runs_path).expect("run records");
        assert!(records.contains("\"run_id\":\"run-consumer-proof\""));
        assert!(records.contains("\"status\":\"failed\""));
        let record =
            serde_json::from_str::<AgentRunRecord>(records.lines().next().expect("record"))
                .expect("parse record");
        assert!(record
            .failure_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("live control-plane context unavailable")));
        assert!(!record
            .tool_calls
            .iter()
            .any(|call| call.name.contains("codex") || call.name.contains("xai")));
    }
}
