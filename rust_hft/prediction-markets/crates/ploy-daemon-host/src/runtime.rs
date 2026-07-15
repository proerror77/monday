use crate::config::PlatformConfig;
use crate::events::EventBroker;
use crate::http::publish_snapshot_events;
use chrono::{DateTime, Utc};
use ploy_connectivity::{
    DisabledLiveExecutionGateway, ExecutionError, ExecutionOutcome, LiveExecutionGateway,
};
use ploy_deployments::WorkerSupervisor;
use ploy_operator_contracts::{
    ActiveAlert, DeploymentApplyRequest, DeploymentControlRequest, DeploymentRuntimeMode,
    DeploymentState, DesiredState, ObservedState, OrderControlResponse, OrderReplaceRequest,
    PaperIntentResponse, PlatformMetrics, ProposalActionKind, ProposalCreateRequest,
    ProposalDecisionRequest, SafetyProposal, TradingStateSnapshot,
};
use ploy_platform::{ControlPlane, DeploymentRecord};
use ploy_platform_runtime::runtime_support::{
    account_token_exposure_envelope, intent_risk_effect, IntentAdmissionSource,
};
use ploy_platform_runtime::{
    apply_deployment as apply_deployment_record,
    apply_live_intent_outcome as apply_live_runtime_intent_outcome, apply_loaded_registry_state,
    build_trading_state_snapshot, cancel_order as cancel_runtime_order,
    control_deployment as control_deployment_record,
    enforce_exposure_limit as enforce_intent_exposure_limit, ensure_intent_allowed,
    execute_live_intent as execute_live_runtime_intent, load_proposal_store, load_registry_records,
    load_trading_runtimes, mark_live_runtime_degraded as mark_runtime_degraded_state,
    mark_runtime_healthy as mark_runtime_healthy_state, mark_venue_healthy, order_state_wire,
    prepare_live_intent as prepare_live_runtime_intent,
    reconcile_live_fills as reconcile_runtime_live_fills,
    refresh_source_health as refresh_platform_source_health,
    replace_order as replace_runtime_order,
    set_deployment_max_gross_exposure as set_record_max_gross_exposure,
    submit_paper_intent as submit_paper_runtime_intent, tick_workers as tick_platform_workers,
    write_json, LiveHealthConfig, PreparedLiveIntent, ProposalStore, ReconcileStatus,
    WorkerTickConfig,
};
use ploy_trading::{TradingIntent, TradingRuntime, TradingRuntimeSnapshot};
use rust_decimal::Decimal;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

#[derive(Debug)]
pub enum PreparedIntentSubmission {
    Complete(PaperIntentResponse),
    Live(PreparedDaemonLiveIntent),
}

#[derive(Debug)]
pub struct PreparedDaemonLiveIntent {
    deployment_id: String,
    prepared: PreparedLiveIntent,
    gateway: Arc<dyn LiveExecutionGateway>,
}

impl PreparedDaemonLiveIntent {
    pub fn execute(&self) -> Result<ExecutionOutcome, ExecutionError> {
        execute_live_runtime_intent(self.gateway.as_ref(), &self.prepared)
    }
}

fn response_for_runtime_order(
    deployment_id: &str,
    order: &ploy_trading::OrderRecord,
) -> PaperIntentResponse {
    PaperIntentResponse {
        deployment_id: deployment_id.to_string(),
        intent_id: order.intent_id.clone(),
        order_id: order.order_id.clone(),
        state: order_state_wire(order.state),
        venue_order_id: order.venue_order_id.clone(),
        rejection_reason: order.rejection_reason.clone(),
        last_error: order.last_error.clone(),
    }
}

#[derive(Debug, Deserialize)]
struct LiveApprovalReceipt {
    deployment_id: String,
    deploy_sha: String,
    account_id: String,
    max_gross_exposure: String,
    live_config_sha256: String,
    expires_at: DateTime<Utc>,
    ready_for_human_live_approval: bool,
}

pub use ploy_platform_runtime::next_paper_intent_id;

#[allow(dead_code)]
pub fn request_shutdown() {
    SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
}

fn shutdown_requested() -> bool {
    SHUTDOWN_REQUESTED.load(Ordering::SeqCst)
}

#[cfg(test)]
use ploy_trading::FillRecord;

#[derive(Debug)]
pub struct PloyDaemon {
    pub config: PlatformConfig,
    pub control_plane: ControlPlane,
    pub supervisor: WorkerSupervisor,
    pub trading: BTreeMap<String, TradingRuntime>,
    proposals: ProposalStore,
    live_execution: Arc<dyn LiveExecutionGateway>,
    live_reconcile_failures: u32,
    next_live_reconcile_at: Option<DateTime<Utc>>,
    last_live_reconcile_error: Option<String>,
    #[cfg(test)]
    fail_trading_state_write_on_attempt: Option<usize>,
    #[cfg(test)]
    trading_state_write_attempts: usize,
    #[cfg(test)]
    fail_registry_write_on_attempt: Option<usize>,
    #[cfg(test)]
    registry_write_attempts: usize,
    #[cfg(test)]
    fail_status_write_on_attempt: Option<usize>,
    #[cfg(test)]
    status_write_attempts: usize,
}

impl PloyDaemon {
    pub fn boot(config: &PlatformConfig) -> io::Result<Self> {
        Self::boot_with_gateway(config, Box::new(DisabledLiveExecutionGateway))
    }

    #[cfg(test)]
    pub(crate) fn boot_with_live_execution(
        config: &PlatformConfig,
        live_execution: Box<dyn LiveExecutionGateway>,
    ) -> io::Result<Self> {
        Self::boot_with_gateway(config, live_execution)
    }

    fn boot_with_gateway(
        config: &PlatformConfig,
        live_execution: Box<dyn LiveExecutionGateway>,
    ) -> io::Result<Self> {
        let mut normalized_config = config.clone();
        normalized_config.normalize_derived_paths();
        let mut control_plane = ControlPlane::default();
        control_plane
            .system
            .set_status(format!("starting@{}", normalized_config.listen_addr));

        let mut daemon = Self {
            config: normalized_config,
            control_plane,
            supervisor: WorkerSupervisor::default(),
            trading: BTreeMap::new(),
            proposals: ProposalStore::default(),
            live_execution: Arc::from(live_execution),
            live_reconcile_failures: 0,
            next_live_reconcile_at: None,
            last_live_reconcile_error: None,
            #[cfg(test)]
            fail_trading_state_write_on_attempt: None,
            #[cfg(test)]
            trading_state_write_attempts: 0,
            #[cfg(test)]
            fail_registry_write_on_attempt: None,
            #[cfg(test)]
            registry_write_attempts: 0,
            #[cfg(test)]
            fail_status_write_on_attempt: None,
            #[cfg(test)]
            status_write_attempts: 0,
        };
        daemon.load_registry_records_only()?;
        daemon.load_trading_snapshots()?;
        let quarantined = daemon.load_registry()?;
        if quarantined {
            daemon.persist_registry()?;
        }
        daemon.load_proposals()?;
        if daemon.config.trading_state_file.exists() {
            daemon
                .control_plane
                .system
                .mark_recovering(&daemon.config.listen_addr);
        }
        if !quarantined {
            daemon.tick();
        }
        daemon.mark_runtime_healthy();
        if daemon.has_live_deployments() {
            match daemon.probe_live_venue() {
                Ok(()) => daemon.mark_venue_healthy(),
                Err(error) => daemon.mark_live_runtime_degraded(error),
            }
        }

        Ok(daemon)
    }

    pub fn write_runtime_snapshots(&mut self) -> io::Result<()> {
        if let Err(err) = self.load_registry() {
            self.control_plane.system.set_database_connected(false);
            self.control_plane
                .system
                .mark_degraded(&self.config.listen_addr);
            return Err(err);
        }
        self.control_plane.system.set_database_connected(true);
        self.tick();
        if self.has_live_deployments() {
            match self.probe_live_venue() {
                Ok(()) => {
                    self.mark_venue_healthy();
                    match self.reconcile_live_fills() {
                        Ok(ReconcileStatus::Applied(_) | ReconcileStatus::Noop) => {
                            self.mark_runtime_healthy()
                        }
                        Ok(ReconcileStatus::BackoffActive) => {}
                        Err(err) => self.mark_live_runtime_degraded(err),
                    }
                }
                Err(err) => self.mark_live_runtime_degraded(err),
            }
        } else {
            self.mark_runtime_healthy();
        }
        self.refresh_source_health();
        if self.config.circuit_breaker_enabled {
            self.evaluate_circuit_breakers();
        }
        if let Err(err) = self.persist_registry() {
            self.control_plane.system.set_database_connected(false);
            self.control_plane
                .system
                .mark_degraded(&self.config.listen_addr);
            return Err(err);
        }
        self.control_plane.system.set_database_connected(true);
        if let Err(err) = fs::create_dir_all(&self.config.runtime_root) {
            self.control_plane
                .system
                .mark_degraded(&self.config.listen_addr);
            return Err(err);
        }
        self.persist_status_snapshot()?;
        write_json(
            &self.config.deployment_status_file,
            &self.control_plane.deployments.summaries(),
        )?;
        write_json(&self.config.trading_state_file, &self.trading_state())?;
        write_json(&self.config.proposals_file, &self.proposals.all())?;
        Ok(())
    }

    pub fn inspect_deployment(&self, deployment_id: &str) -> Option<DeploymentRecord> {
        self.control_plane.deployments.get(deployment_id).cloned()
    }

    pub fn platform_metrics(&self) -> PlatformMetrics {
        let records = self.control_plane.deployments.records();
        let total_deployments = records.len();
        let live_deployments = records
            .iter()
            .filter(|record| record.runtime_mode == DeploymentRuntimeMode::Live)
            .count();
        let degraded_deployments = records
            .iter()
            .filter(|record| record.observed_state == ObservedState::Degraded)
            .count();
        self.control_plane
            .system
            .metrics(total_deployments, live_deployments, degraded_deployments)
    }

    pub fn active_alerts(&self) -> Vec<ActiveAlert> {
        self.control_plane.system.active_alerts()
    }

    pub fn proposals(&self) -> Vec<SafetyProposal> {
        self.proposals.all()
    }

    pub fn trading_state(&self) -> Vec<TradingStateSnapshot> {
        self.control_plane
            .deployments
            .records()
            .into_iter()
            .filter_map(|record| {
                let snapshot = match self.trading.get(&record.deployment_id) {
                    Some(runtime) => runtime.snapshot(&BTreeMap::new()),
                    None if record.runtime_mode == DeploymentRuntimeMode::Paper => {
                        Default::default()
                    }
                    None => return None,
                };
                Some(build_trading_state_snapshot(record, snapshot))
            })
            .collect()
    }

    pub fn apply_deployment(
        &mut self,
        request: DeploymentApplyRequest,
    ) -> io::Result<DeploymentRecord> {
        if request.runtime_mode == DeploymentRuntimeMode::Live
            && request.desired_state == DesiredState::Running
            && request.deployment_state != DeploymentState::Archived
        {
            self.ensure_live_record_resume_approved(
                &request.deployment_id,
                &request.account_id,
                request.max_gross_exposure,
                &request.bundle_id,
            )?;
        }
        let existing = self
            .control_plane
            .deployments
            .get(&request.deployment_id)
            .cloned();
        let deployment_id = request.deployment_id.clone();
        let prior_runtime = self
            .trading
            .get(&deployment_id)
            .map(|runtime| runtime.snapshot(&BTreeMap::new()));
        let runtime_mode_changed = existing
            .as_ref()
            .is_some_and(|current| current.runtime_mode != request.runtime_mode);
        if existing.as_ref().is_some_and(|current| {
            current.runtime_mode != request.runtime_mode || current.account_id != request.account_id
        }) {
            self.ensure_deployment_flat_for_reassignment(&request.deployment_id)?;
        }
        if request.deployment_state == DeploymentState::Archived {
            self.ensure_deployment_flat_for_archive(&request.deployment_id)?;
        }
        if let Some(limit) = request.max_gross_exposure {
            let current = self.account_total_exposure(&request.account_id);
            if current > limit {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "current account exposure {current} exceeds proposed limit {limit} for `{}`",
                        request.deployment_id
                    ),
                ));
            }
        }
        let record = apply_deployment_record(&mut self.control_plane.deployments, request)?;
        let new_live_deployment =
            existing.is_none() && record.runtime_mode == DeploymentRuntimeMode::Live;
        if new_live_deployment {
            self.trading
                .insert(record.deployment_id.clone(), TradingRuntime::default());
        }
        if new_live_deployment || runtime_mode_changed {
            if let Err(error) = self.persist_trading_state() {
                return Err(self.rollback_failed_apply(
                    &deployment_id,
                    existing.clone(),
                    prior_runtime.clone(),
                    error,
                ));
            }
        }
        if let Err(error) = self.persist_registry() {
            return Err(self.rollback_failed_apply(&deployment_id, existing, prior_runtime, error));
        }
        let warning_source = format!("derived_snapshot:{}", record.deployment_id);
        self.control_plane.system.clear_source(&warning_source);
        if let Err(error) = self.write_runtime_snapshots() {
            self.mark_derived_snapshot_failure(&record.deployment_id, &error);
            return Ok(self
                .inspect_deployment(&record.deployment_id)
                .unwrap_or(record));
        }
        Ok(record)
    }

    fn rollback_failed_apply(
        &mut self,
        deployment_id: &str,
        prior_record: Option<DeploymentRecord>,
        prior_runtime: Option<TradingRuntimeSnapshot>,
        apply_error: io::Error,
    ) -> io::Error {
        match prior_record.as_ref() {
            Some(record) => {
                self.control_plane.deployments.upsert(record.clone());
            }
            None => {
                self.control_plane.deployments.remove(deployment_id);
            }
        }
        match prior_runtime {
            Some(snapshot) => {
                self.trading
                    .insert(deployment_id.to_string(), TradingRuntime::restore(snapshot));
            }
            None => {
                self.trading.remove(deployment_id);
            }
        }
        if prior_record
            .as_ref()
            .is_none_or(|record| record.desired_state != DesiredState::Running)
        {
            self.supervisor.stop(deployment_id);
        }

        let mut rollback_errors = Vec::new();
        if let Err(error) = self.persist_trading_state() {
            rollback_errors.push(format!("trading state: {error}"));
        }
        if let Err(error) = self.persist_registry() {
            rollback_errors.push(format!("registry: {error}"));
        }

        if rollback_errors.is_empty() {
            apply_error
        } else {
            io::Error::new(
                apply_error.kind(),
                format!(
                    "{apply_error}; rollback persistence also failed: {}",
                    rollback_errors.join("; ")
                ),
            )
        }
    }

    fn mark_derived_snapshot_failure(&mut self, deployment_id: &str, error: &io::Error) {
        let source_id = format!("derived_snapshot:{deployment_id}");
        self.control_plane
            .system
            .mark_degraded(&self.config.listen_addr);
        self.control_plane.system.note_source_failure(
            source_id,
            "derived_snapshot",
            chrono::Duration::minutes(5),
            format!("core apply committed; derived snapshot refresh failed: {error}"),
        );
        self.control_plane
            .deployments
            .set_observed_state(deployment_id, ObservedState::Degraded);
    }

    pub fn create_proposal(
        &mut self,
        request: ProposalCreateRequest,
    ) -> io::Result<SafetyProposal> {
        if self
            .control_plane
            .deployments
            .get(&request.target_deployment_id)
            .is_none()
        {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "deployment `{}` was not found",
                    request.target_deployment_id
                ),
            ));
        }

        self.proposals.create(request)
    }

    pub fn approve_proposal(
        &mut self,
        proposal_id: &str,
        request: ProposalDecisionRequest,
    ) -> io::Result<Option<SafetyProposal>> {
        let Some(plan) = self.proposals.prepare_approval(proposal_id, request)? else {
            return Ok(None);
        };

        let action_result = match plan.action_kind {
            ProposalActionKind::PauseDeployment => self.control_deployment(
                &plan.target_deployment_id,
                DeploymentControlRequest {
                    desired_state: Some(DesiredState::Paused),
                    deployment_state: None,
                },
            ),
            ProposalActionKind::DrainDeployment => self.control_deployment(
                &plan.target_deployment_id,
                DeploymentControlRequest {
                    desired_state: None,
                    deployment_state: Some(DeploymentState::Draining),
                },
            ),
            ProposalActionKind::ReduceMaxExposure => {
                let max_gross_exposure = plan.proposed_max_gross_exposure.ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "proposal missing proposed_max_gross_exposure",
                    )
                })?;
                self.set_deployment_max_gross_exposure(
                    &plan.target_deployment_id,
                    Some(max_gross_exposure),
                )?;
                Ok(self.inspect_deployment(&plan.target_deployment_id))
            }
        };

        match action_result {
            Ok(_) => Ok(Some(
                self.proposals
                    .mark_approved(proposal_id, plan.decision_note)?,
            )),
            Err(err) => {
                self.proposals.mark_failed(proposal_id, &err)?;
                Err(err)
            }
        }
    }

    pub fn reject_proposal(
        &mut self,
        proposal_id: &str,
        request: ProposalDecisionRequest,
    ) -> io::Result<Option<SafetyProposal>> {
        self.proposals.reject(proposal_id, request)
    }

    pub fn control_deployment(
        &mut self,
        deployment_id: &str,
        request: DeploymentControlRequest,
    ) -> io::Result<Option<DeploymentRecord>> {
        if request.desired_state == Some(DesiredState::Running) {
            self.ensure_live_resume_approved(deployment_id)?;
        }
        if request.deployment_state == Some(DeploymentState::Archived) {
            self.ensure_deployment_flat_for_archive(deployment_id)?;
        }
        let record =
            control_deployment_record(&mut self.control_plane.deployments, deployment_id, request)?;
        self.persist_registry()?;
        self.write_runtime_snapshots()?;
        Ok(record)
    }

    fn ensure_live_resume_approved(&self, deployment_id: &str) -> io::Result<()> {
        let Some(record) = self.control_plane.deployments.get(deployment_id) else {
            return Ok(());
        };
        if record.runtime_mode != DeploymentRuntimeMode::Live {
            return Ok(());
        }
        self.ensure_live_record_resume_approved(
            deployment_id,
            &record.account_id,
            record.max_gross_exposure,
            &record.bundle_id,
        )
    }

    fn ensure_live_record_resume_approved(
        &self,
        deployment_id: &str,
        account_id: &str,
        max_gross_exposure: Option<Decimal>,
        bundle_id: &str,
    ) -> io::Result<()> {
        let Some(receipt_path) = self.config.live_approval_file.as_ref() else {
            #[cfg(test)]
            return Ok(());
            #[cfg(not(test))]
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "live resume requires PLOY_LIVE_APPROVAL_FILE",
            ));
        };
        let release_sha = self.config.release_sha.as_deref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "live resume requires PLOY_RELEASE_SHA when approval enforcement is configured",
            )
        })?;
        let raw = fs::read(receipt_path).map_err(|error| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("live resume approval receipt is unavailable: {error}"),
            )
        })?;
        let receipt: LiveApprovalReceipt = serde_json::from_slice(&raw).map_err(|error| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("live resume approval receipt is invalid: {error}"),
            )
        })?;
        let mut config_path = self.config.strategy_config_root.join(bundle_id);
        if config_path.extension().is_none() {
            config_path.set_extension("toml");
        }
        let config_bytes = fs::read(&config_path).map_err(|error| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("live strategy config cannot be hashed: {error}"),
            )
        })?;
        let config_sha256 = format!("{:x}", Sha256::digest(&config_bytes));
        let receipt_cap = receipt
            .max_gross_exposure
            .parse::<Decimal>()
            .map_err(|error| {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("live approval exposure cap is invalid: {error}"),
                )
            })?;
        let expected_cap = max_gross_exposure.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "live deployment has no exposure cap",
            )
        })?;
        let approved = receipt.ready_for_human_live_approval
            && receipt.deployment_id == deployment_id
            && receipt.deploy_sha == release_sha
            && receipt.account_id == account_id
            && receipt_cap == expected_cap
            && receipt.live_config_sha256 == config_sha256
            && receipt.expires_at > Utc::now()
            && receipt.expires_at <= Utc::now() + chrono::Duration::minutes(20);
        if !approved {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "live resume approval receipt does not match deployment, release, config, cap, or expiry",
            ));
        }
        Ok(())
    }

    fn ensure_deployment_flat_for_archive(&self, deployment_id: &str) -> io::Result<()> {
        let risk = self
            .trading
            .get(deployment_id)
            .map(|runtime| runtime.snapshot(&BTreeMap::new()).risk)
            .unwrap_or_default();
        if risk.active_orders > 0 || risk.open_positions > 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "deployment `{deployment_id}` cannot be archived with {} active orders or {} open positions",
                    risk.active_orders, risk.open_positions
                ),
            ));
        }
        Ok(())
    }

    fn ensure_deployment_flat_for_reassignment(&self, deployment_id: &str) -> io::Result<()> {
        let runtime = self.trading.get(deployment_id).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "deployment `{deployment_id}` cannot change runtime_mode or account_id without a canonical ledger"
                ),
            )
        })?;
        let active_orders = runtime.orders().active_orders();
        let open_positions = runtime.positions().positions().count();
        if active_orders > 0 || open_positions > 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "deployment `{deployment_id}` cannot change runtime_mode or account_id with {active_orders} nonterminal orders or {open_positions} nonzero positions"
                ),
            ));
        }
        Ok(())
    }

    pub fn submit_intent(&mut self, intent: TradingIntent) -> io::Result<PaperIntentResponse> {
        self.submit_intent_idempotent(intent, None)
    }

    pub fn submit_intent_idempotent(
        &mut self,
        intent: TradingIntent,
        idempotency_key: Option<&str>,
    ) -> io::Result<PaperIntentResponse> {
        self.submit_intent_idempotent_from(
            intent,
            idempotency_key,
            IntentAdmissionSource::AuthenticatedOperator,
        )
    }

    pub fn submit_intent_idempotent_from(
        &mut self,
        intent: TradingIntent,
        idempotency_key: Option<&str>,
        source: IntentAdmissionSource,
    ) -> io::Result<PaperIntentResponse> {
        match self.prepare_intent_idempotent_from(intent, idempotency_key, source)? {
            PreparedIntentSubmission::Complete(response) => Ok(response),
            PreparedIntentSubmission::Live(prepared) => {
                let outcome = prepared.execute();
                self.finish_prepared_live_intent(prepared, outcome)
            }
        }
    }

    pub fn prepare_intent_idempotent_from(
        &mut self,
        intent: TradingIntent,
        idempotency_key: Option<&str>,
        source: IntentAdmissionSource,
    ) -> io::Result<PreparedIntentSubmission> {
        let mut deployment = self
            .control_plane
            .deployments
            .get(&intent.deployment_id)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "deployment not found"))?;
        if let Some(response) =
            self.account_idempotent_response(&deployment.account_id, &intent, idempotency_key)?
        {
            return Ok(PreparedIntentSubmission::Complete(response));
        }

        self.refresh_source_health();
        deployment = self
            .control_plane
            .deployments
            .get(&intent.deployment_id)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "deployment not found"))?;
        let deployments = self.control_plane.deployments.records();
        if deployments.iter().any(|record| {
            record.runtime_mode == DeploymentRuntimeMode::Live
                && record.deployment_state != DeploymentState::Archived
                && record
                    .account_id
                    .trim()
                    .eq_ignore_ascii_case(deployment.account_id.trim())
                && !self.trading.contains_key(&record.deployment_id)
        }) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "account `{}` has a live deployment without canonical trading state",
                    deployment.account_id
                ),
            ));
        }
        let exposure = account_token_exposure_envelope(
            &deployments,
            &self.trading,
            &deployment.account_id,
            &intent.token_id,
        );
        let risk_effect = intent_risk_effect(&intent, exposure);
        let venue_health_fresh = self
            .control_plane
            .system
            .source_is_fresh_at("venue:polymarket", Utc::now());
        ensure_intent_allowed(
            &deployment,
            &intent,
            risk_effect,
            venue_health_fresh,
            source,
        )?;
        enforce_intent_exposure_limit(
            &deployment,
            &intent,
            self.account_total_exposure(&deployment.account_id),
        )?;

        match deployment.runtime_mode {
            DeploymentRuntimeMode::Paper => Ok(PreparedIntentSubmission::Complete(
                self.submit_paper_intent_idempotent(intent, idempotency_key)?,
            )),
            DeploymentRuntimeMode::Live => {
                self.prepare_live_intent_submission(intent, idempotency_key)
            }
        }
    }

    fn account_idempotent_response(
        &self,
        account_id: &str,
        intent: &TradingIntent,
        idempotency_key: Option<&str>,
    ) -> io::Result<Option<PaperIntentResponse>> {
        let Some(key) = idempotency_key.map(str::trim).filter(|key| !key.is_empty()) else {
            return Ok(None);
        };
        for deployment in self
            .control_plane
            .deployments
            .records()
            .into_iter()
            .filter(|deployment| deployment.account_id == account_id)
        {
            let Some(runtime) = self.trading.get(&deployment.deployment_id) else {
                continue;
            };
            let Some(order) = runtime
                .orders()
                .orders()
                .find(|order| order.idempotency_key.as_deref() == Some(key))
            else {
                continue;
            };
            let existing = runtime
                .intent(&order.intent_id)
                .expect("idempotent order has intent");
            if existing.market_id != intent.market_id
                || existing.token_id != intent.token_id
                || existing.side != intent.side
                || existing.quantity != intent.quantity
                || existing.limit_price != intent.limit_price
                || existing.purpose != intent.purpose
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "idempotency key payload mismatch",
                ));
            }
            return Ok(Some(PaperIntentResponse {
                deployment_id: order.deployment_id.clone(),
                intent_id: order.intent_id.clone(),
                order_id: order.order_id.clone(),
                state: order_state_wire(order.state),
                venue_order_id: order.venue_order_id.clone(),
                rejection_reason: order.rejection_reason.clone(),
                last_error: order.last_error.clone(),
            }));
        }
        Ok(None)
    }

    pub fn submit_paper_intent(
        &mut self,
        intent: TradingIntent,
    ) -> io::Result<PaperIntentResponse> {
        self.submit_paper_intent_idempotent(intent, None)
    }

    fn submit_paper_intent_idempotent(
        &mut self,
        intent: TradingIntent,
        idempotency_key: Option<&str>,
    ) -> io::Result<PaperIntentResponse> {
        let deployment = self
            .control_plane
            .deployments
            .get(&intent.deployment_id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "deployment not found"))?;

        if deployment.runtime_mode != DeploymentRuntimeMode::Paper {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "only paper deployments are supported by the local trading runtime",
            ));
        }
        if deployment.desired_state != DesiredState::Running {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "paper intents require a running deployment",
            ));
        }

        let runtime = self
            .trading
            .entry(intent.deployment_id.clone())
            .or_default();
        submit_paper_runtime_intent(runtime, deployment, intent, idempotency_key)
    }

    pub fn cancel_order(
        &mut self,
        deployment_id: &str,
        order_id: &str,
    ) -> io::Result<OrderControlResponse> {
        let deployment = self
            .control_plane
            .deployments
            .get(deployment_id)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "deployment not found"))?;
        let runtime = self.trading.get_mut(deployment_id).ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "deployment has no trading state")
        })?;
        cancel_runtime_order(
            runtime,
            self.live_execution.as_ref(),
            &deployment,
            deployment_id,
            order_id,
        )
    }

    pub fn replace_order(
        &mut self,
        deployment_id: &str,
        order_id: &str,
        request: OrderReplaceRequest,
    ) -> io::Result<OrderControlResponse> {
        let deployment = self
            .control_plane
            .deployments
            .get(deployment_id)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "deployment not found"))?;
        let current_total_exposure = self.account_total_exposure(&deployment.account_id);
        let runtime = self.trading.get_mut(deployment_id).ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "deployment has no trading state")
        })?;
        replace_runtime_order(
            runtime,
            self.live_execution.as_ref(),
            &deployment,
            deployment_id,
            order_id,
            request,
            current_total_exposure,
        )
    }

    fn prepare_live_intent_submission(
        &mut self,
        intent: TradingIntent,
        idempotency_key: Option<&str>,
    ) -> io::Result<PreparedIntentSubmission> {
        let deployment_id = intent.deployment_id.clone();
        let prepared = prepare_live_runtime_intent(
            self.trading.entry(deployment_id.clone()).or_default(),
            intent,
            idempotency_key,
        )?;
        match prepared {
            PreparedLiveIntent::Existing(response) => {
                Ok(PreparedIntentSubmission::Complete(response))
            }
            prepared @ PreparedLiveIntent::Pending { .. } => {
                self.persist_trading_state()?;
                Ok(PreparedIntentSubmission::Live(PreparedDaemonLiveIntent {
                    deployment_id,
                    prepared,
                    gateway: Arc::clone(&self.live_execution),
                }))
            }
        }
    }

    pub fn finish_prepared_live_intent(
        &mut self,
        prepared: PreparedDaemonLiveIntent,
        outcome: Result<ExecutionOutcome, ExecutionError>,
    ) -> io::Result<PaperIntentResponse> {
        let submission_ambiguous = outcome.is_err();
        let deployment_id = prepared.deployment_id;
        let mut response = apply_live_runtime_intent_outcome(
            self.trading
                .get_mut(&deployment_id)
                .expect("prepared live runtime"),
            prepared.prepared,
            outcome,
        )?;
        let mut pause_live = submission_ambiguous;
        if !submission_ambiguous {
            if let Err(error) = self.persist_trading_state() {
                self.trading
                    .get_mut(&deployment_id)
                    .expect("prepared live runtime")
                    .mark_order_unknown(
                        &response.order_id,
                        format!("final persistence failed: {error}"),
                    );
                response = response_for_runtime_order(
                    &deployment_id,
                    self.trading
                        .get(&deployment_id)
                        .and_then(|runtime| runtime.order(&response.order_id))
                        .expect("prepared live order"),
                );
                pause_live = true;
            }
        }
        if pause_live {
            let _ = control_deployment_record(
                &mut self.control_plane.deployments,
                &deployment_id,
                DeploymentControlRequest {
                    desired_state: Some(DesiredState::Paused),
                    deployment_state: None,
                },
            )?;
            self.mark_live_runtime_degraded(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                response
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "live submission outcome unknown".to_string()),
            ));
            self.control_plane
                .deployments
                .set_observed_state(&deployment_id, ObservedState::Degraded);
            self.persist_registry()?;
            self.persist_trading_state()?;
        }
        Ok(response)
    }

    fn persist_trading_state(&mut self) -> io::Result<()> {
        #[cfg(test)]
        {
            self.trading_state_write_attempts += 1;
            if self.fail_trading_state_write_on_attempt == Some(self.trading_state_write_attempts) {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "injected trading-state persistence failure",
                ));
            }
        }
        fs::create_dir_all(&self.config.runtime_root)?;
        write_json(&self.config.trading_state_file, &self.trading_state())
    }

    pub fn reconcile_live_fills(&mut self) -> io::Result<ReconcileStatus> {
        if let Some(next_attempt_at) = self.next_live_reconcile_at {
            if Utc::now() < next_attempt_at {
                return Ok(ReconcileStatus::BackoffActive);
            }
        }

        let result = reconcile_runtime_live_fills(
            self.live_execution.as_ref(),
            &self.control_plane.deployments.records(),
            &mut self.trading,
        )?;

        if matches!(result, ReconcileStatus::Noop) {
            self.live_reconcile_failures = 0;
            self.next_live_reconcile_at = None;
            self.last_live_reconcile_error = None;
            self.control_plane.system.note_live_reconcile_healthy();
            return Ok(ReconcileStatus::Noop);
        }

        self.live_reconcile_failures = 0;
        self.next_live_reconcile_at = None;
        self.last_live_reconcile_error = None;
        self.control_plane.system.note_live_reconcile_healthy();

        Ok(result)
    }

    fn has_live_deployments(&self) -> bool {
        self.control_plane
            .deployments
            .records()
            .into_iter()
            .any(|record| {
                record.runtime_mode == DeploymentRuntimeMode::Live
                    && record.deployment_state != DeploymentState::Archived
            })
    }

    fn probe_live_venue(&self) -> io::Result<()> {
        self.live_execution
            .probe()
            .map_err(|error| io::Error::new(io::ErrorKind::Other, error.to_string()))
    }

    fn latest_trade_time(&self) -> Option<DateTime<Utc>> {
        self.trading
            .values()
            .filter_map(TradingRuntime::last_fill_time)
            .max()
    }

    fn mark_runtime_healthy(&mut self) {
        let health_config = self.live_health_config();
        let latest_trade_time = self.latest_trade_time();
        mark_runtime_healthy_state(&mut self.control_plane, &health_config, latest_trade_time);
    }

    fn mark_venue_healthy(&mut self) {
        let health_config = self.live_health_config();
        mark_venue_healthy(&mut self.control_plane, &health_config);
    }

    fn mark_live_runtime_degraded(&mut self, err: io::Error) {
        let health_config = self.live_health_config();
        mark_runtime_degraded_state(
            &mut self.control_plane,
            &health_config,
            &mut self.live_reconcile_failures,
            &mut self.next_live_reconcile_at,
            &mut self.last_live_reconcile_error,
            &err,
        );
    }

    fn evaluate_circuit_breakers(&mut self) {
        // Placeholder until circuit-breaker policy is fully reintroduced on the
        // new platform-runtime path. Keeping this as a no-op preserves the
        // current compile contract without inventing behavior.
    }

    fn live_health_config(&self) -> LiveHealthConfig {
        LiveHealthConfig {
            listen_addr: self.config.listen_addr.clone(),
            live_reconcile_stale_after_ms: self.config.live_reconcile_stale_after_ms,
            venue_stale_after_ms: self.config.venue_stale_after_ms,
            live_reconcile_backoff_base_ms: self.config.live_reconcile_backoff_base_ms,
            live_reconcile_backoff_max_ms: self.config.live_reconcile_backoff_max_ms,
        }
    }

    #[cfg(test)]
    pub fn record_fill(&mut self, deployment_id: &str, fill: FillRecord) {
        if let Some(runtime) = self.trading.get_mut(deployment_id) {
            runtime.record_fill(fill);
        }
    }

    fn account_total_exposure(&self, account_id: &str) -> Decimal {
        self.control_plane
            .deployments
            .records()
            .into_iter()
            .filter(|deployment| deployment.account_id == account_id)
            .map(|deployment| {
                self.trading
                    .get(&deployment.deployment_id)
                    .map(|runtime| runtime.snapshot(&BTreeMap::new()).risk.total_gross_exposure)
                    .unwrap_or_default()
            })
            .sum()
    }

    fn load_registry(&mut self) -> io::Result<bool> {
        let records = load_registry_records(&self.config.registry_file)?;
        let worker_tick_config = WorkerTickConfig {
            listen_addr: self.config.listen_addr.clone(),
            worker_heartbeat_stale_after_ms: self.config.worker_heartbeat_stale_after_ms,
            runner_binary: self.config.runner_binary.clone(),
            strategy_config_root: self.config.strategy_config_root.clone(),
            working_directory: deployment_working_directory(&self.config),
            canonical_live_ledgers: self.trading.keys().cloned().collect(),
        };
        let quarantined = apply_loaded_registry_state(
            records,
            &mut self.control_plane.deployments,
            &mut self.supervisor,
            &mut self.trading,
            &worker_tick_config,
        );

        Ok(quarantined)
    }

    fn load_registry_records_only(&mut self) -> io::Result<()> {
        for record in load_registry_records(&self.config.registry_file)? {
            self.control_plane.deployments.upsert(record);
        }
        Ok(())
    }

    fn load_trading_snapshots(&mut self) -> io::Result<()> {
        let runtimes = load_trading_runtimes(&self.config.trading_state_file, |deployment_id| {
            self.control_plane
                .deployments
                .get(deployment_id)
                .map(|record| record.runtime_mode.clone())
        })?;
        self.trading.extend(runtimes);
        Ok(())
    }

    fn load_proposals(&mut self) -> io::Result<()> {
        self.proposals = load_proposal_store(&self.config.proposals_file)?;
        Ok(())
    }

    fn tick(&mut self) {
        let tick_config = WorkerTickConfig {
            listen_addr: self.config.listen_addr.clone(),
            worker_heartbeat_stale_after_ms: self.config.worker_heartbeat_stale_after_ms,
            runner_binary: self.config.runner_binary.clone(),
            strategy_config_root: self.config.strategy_config_root.clone(),
            working_directory: deployment_working_directory(&self.config),
            canonical_live_ledgers: self.trading.keys().cloned().collect(),
        };
        tick_platform_workers(&mut self.control_plane, &mut self.supervisor, &tick_config);
    }

    fn refresh_source_health(&mut self) {
        refresh_platform_source_health(&mut self.control_plane, &self.config.listen_addr);
    }

    fn persist_registry(&mut self) -> io::Result<()> {
        #[cfg(test)]
        {
            self.registry_write_attempts += 1;
            if self.fail_registry_write_on_attempt == Some(self.registry_write_attempts) {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "injected registry persistence failure",
                ));
            }
        }
        write_json(
            &self.config.registry_file,
            &self.control_plane.deployments.records(),
        )
    }

    fn persist_status_snapshot(&mut self) -> io::Result<()> {
        #[cfg(test)]
        {
            self.status_write_attempts += 1;
            if self.fail_status_write_on_attempt == Some(self.status_write_attempts) {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "injected status snapshot persistence failure",
                ));
            }
        }
        write_json(
            &self.config.status_file,
            &self.control_plane.system.status(),
        )
    }

    fn set_deployment_max_gross_exposure(
        &mut self,
        deployment_id: &str,
        max_gross_exposure: Option<Decimal>,
    ) -> io::Result<()> {
        let target = self.inspect_deployment(deployment_id).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("deployment `{deployment_id}` was not found"),
            )
        })?;
        let current_exposure = if target.runtime_mode == DeploymentRuntimeMode::Live {
            Some(self.account_total_exposure(&target.account_id))
        } else {
            self.trading
                .get(deployment_id)
                .map(|runtime| runtime.snapshot(&BTreeMap::new()).risk.total_gross_exposure)
        };
        let prior_records = self.control_plane.deployments.records();

        let record = set_record_max_gross_exposure(
            &mut self.control_plane.deployments,
            deployment_id,
            max_gross_exposure,
            current_exposure,
        )?;

        if let Err(error) = self.persist_registry() {
            for prior in prior_records {
                self.control_plane.deployments.upsert(prior);
            }
            return Err(error);
        }

        if self.trading.contains_key(&record.deployment_id) {
            let _ = self.enforce_current_exposure_limit(&record);
        }
        Ok(())
    }

    fn enforce_current_exposure_limit(&self, record: &DeploymentRecord) -> io::Result<()> {
        let Some(limit) = record.max_gross_exposure else {
            return Ok(());
        };
        let Some(runtime) = self.trading.get(&record.deployment_id) else {
            return Ok(());
        };
        let current = runtime.snapshot(&BTreeMap::new()).risk.total_gross_exposure;
        if current > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "current exposure {} exceeds proposed limit {} for `{}`",
                    current, limit, record.deployment_id
                ),
            ));
        }
        Ok(())
    }
}

fn deployment_working_directory(config: &PlatformConfig) -> std::path::PathBuf {
    if config.registry_file.is_absolute() {
        let path = config.registry_file.as_path();
        let parent = path.parent();
        let grandparent = parent.and_then(|value| value.parent());
        let root = grandparent.and_then(|value| value.parent());
        if let Some(root) = root {
            return root.to_path_buf();
        }
    }

    std::env::current_dir().unwrap_or_else(|_| ".".into())
}

#[cfg(test)]
pub(crate) fn seed_empty_live_ledgers(config: &PlatformConfig) {
    let snapshots = load_registry_records(&config.registry_file)
        .expect("load test registry")
        .into_iter()
        .filter(|record| record.runtime_mode == DeploymentRuntimeMode::Live)
        .map(|record| {
            build_trading_state_snapshot(
                record,
                TradingRuntime::default().snapshot(&BTreeMap::new()),
            )
        })
        .collect::<Vec<_>>();
    fs::create_dir_all(
        config
            .trading_state_file
            .parent()
            .expect("test trading state parent"),
    )
    .expect("create test trading state parent");
    write_json(&config.trading_state_file, &snapshots).expect("seed canonical live ledgers");
}

pub fn run_shared_forever(
    daemon: Arc<Mutex<PloyDaemon>>,
    events: Arc<EventBroker>,
) -> io::Result<()> {
    loop {
        if shutdown_requested() {
            eprintln!("ployd: shutdown signal received, writing final snapshots");
            let mut daemon = daemon
                .lock()
                .map_err(|_| io::Error::new(io::ErrorKind::Other, "daemon lock poisoned"))?;
            if let Err(err) = daemon.write_runtime_snapshots() {
                eprintln!("ployd: final snapshot write failed: {err}");
            }
            eprintln!("ployd: shutdown complete");
            return Ok(());
        }
        let tick_interval_ms = {
            let mut daemon = daemon
                .lock()
                .map_err(|_| io::Error::new(io::ErrorKind::Other, "daemon lock poisoned"))?;
            if let Err(err) = daemon.write_runtime_snapshots() {
                eprintln!("ployd tick degraded: {err}");
            }
            publish_snapshot_events(&daemon, &events);
            daemon.config.tick_interval_ms
        };
        thread::sleep(Duration::from_millis(tick_interval_ms));
    }
}

#[cfg(test)]
mod tests {
    use super::{seed_empty_live_ledgers, PloyDaemon, ReconcileStatus};
    use crate::config::PlatformConfig;
    use crate::test_support::StaticExecutionGateway;
    use ploy_connectivity::{
        CancellationOutcome, CancellationRequest, ExecutionError, ExecutionOutcome,
        ExecutionRequest, LiveExecutionGateway, ReplaceOutcome, ReplaceRequest,
        MONDAY_LIVE_EXECUTION_DISABLED,
    };
    use ploy_operator_contracts::{
        DeploymentApplyRequest, DeploymentControlRequest, DeploymentRuntimeMode, DeploymentState,
        DesiredState, ObservedState, PaperIntentResponse,
    };
    use ploy_platform::DeploymentRecord;
    use ploy_platform_runtime::{
        live_reconcile_backoff_ms, runtime_support::IntentAdmissionSource,
    };
    use ploy_trading::{
        FillRecord, IntentPurpose, OrderRecord, OrderState, PositionSnapshot, TradeSide,
        TradingIntent, TradingRuntime, TradingRuntimeSnapshot,
    };
    use rust_decimal_macros::dec;
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::ErrorKind;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    const LIVE_WALLET: &str = "0x1111111111111111111111111111111111111111";
    const OTHER_LIVE_WALLET: &str = "0x2222222222222222222222222222222222222222";

    fn temp_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("duration")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("ployd-{label}-{unique}"));
        let bin_dir = root.join("bin");
        std::fs::create_dir_all(&bin_dir).expect("create bin dir");
        let runner = bin_dir.join("ploy-runner");
        std::fs::write(&runner, "#!/bin/sh\nsleep 30\n").expect("write fake runner");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&runner)
                .expect("runner metadata")
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&runner, perms).expect("set runner permissions");
        }
        root
    }

    fn paused_live_request(deployment_id: &str, account_id: &str) -> DeploymentApplyRequest {
        DeploymentApplyRequest {
            deployment_id: deployment_id.to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: DeploymentRuntimeMode::Live,
            account_id: account_id.to_string(),
            max_gross_exposure: Some(dec!(5)),
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Paused,
        }
    }

    #[test]
    fn production_boot_wires_the_monday_disabled_live_gateway() {
        let root = temp_dir("monday-disabled-live");
        let runtime_root = root.join("run/platform");
        let config = PlatformConfig {
            registry_file: root.join("data/state/deployments.json"),
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            ..PlatformConfig::default()
        };
        let daemon = PloyDaemon::boot(&config).expect("boot with disabled live execution");
        let expected = ExecutionError::Configuration(MONDAY_LIVE_EXECUTION_DISABLED.to_string());

        assert_eq!(daemon.live_execution.probe(), Err(expected));
    }

    #[test]
    fn configured_live_resume_requires_matching_unexpired_approval_receipt() {
        let root = temp_dir("live-approval-receipt");
        let runtime_root = root.join("run/platform");
        let strategy_root = root.join("config/strategies");
        let approval_file = root.join("data/live-approvals/pending.json");
        fs::create_dir_all(&strategy_root).expect("strategy root");
        fs::create_dir_all(approval_file.parent().expect("approval parent"))
            .expect("approval root");
        let live_config = strategy_root.join("example.toml");
        fs::write(&live_config, "[runtime]\nmode = \"live\"\n").expect("live config");
        let config_sha256 = format!(
            "{:x}",
            Sha256::digest(fs::read(&live_config).expect("config bytes"))
        );
        let config = PlatformConfig {
            registry_file: root.join("data/state/deployments.json"),
            strategy_config_root: strategy_root,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            release_sha: Some("a".repeat(40)),
            live_approval_file: Some(approval_file.clone()),
            ..PlatformConfig::default()
        };
        let mut daemon = PloyDaemon::boot(&config).expect("boot");
        let mut running_apply = paused_live_request("example.live", LIVE_WALLET);
        running_apply.desired_state = DesiredState::Running;
        let error = daemon
            .apply_deployment(running_apply)
            .expect_err("live running apply without approval must fail");
        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        daemon
            .apply_deployment(paused_live_request("example.live", LIVE_WALLET))
            .expect("apply paused live");

        let resume = DeploymentControlRequest {
            desired_state: Some(DesiredState::Running),
            deployment_state: None,
        };
        let error = daemon
            .control_deployment("example.live", resume.clone())
            .expect_err("missing approval must fail");
        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert_eq!(
            daemon
                .inspect_deployment("example.live")
                .expect("deployment")
                .desired_state,
            DesiredState::Paused
        );

        fs::write(
            &approval_file,
            serde_json::json!({
                "deployment_id": "example.live",
                "deploy_sha": "a".repeat(40),
                "account_id": LIVE_WALLET,
                "max_gross_exposure": "5",
                "live_config_sha256": config_sha256,
                "expires_at": chrono::Utc::now() + chrono::Duration::minutes(5),
                "ready_for_human_live_approval": true
            })
            .to_string(),
        )
        .expect("approval receipt");
        daemon
            .control_deployment("example.live", resume.clone())
            .expect("matching approval resumes");
        daemon
            .control_deployment(
                "example.live",
                DeploymentControlRequest {
                    desired_state: Some(DesiredState::Paused),
                    deployment_state: None,
                },
            )
            .expect("pause");

        fs::write(
            &live_config,
            "[runtime]\nmode = \"live\"\nthrottle_hz = 20\n",
        )
        .expect("mutate config");
        let error = daemon
            .control_deployment("example.live", resume)
            .expect_err("config drift must fail");
        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
    }

    fn order_runtime(deployment_id: &str, state: OrderState) -> TradingRuntime {
        TradingRuntime::restore(TradingRuntimeSnapshot {
            orders: vec![OrderRecord {
                order_id: "order-1".to_string(),
                intent_id: "intent-1".to_string(),
                deployment_id: deployment_id.to_string(),
                token_id: "token-1".to_string(),
                requested_qty: dec!(1),
                limit_price: Some(dec!(0.5)),
                venue_order_id: Some("venue-1".to_string()),
                venue_order_history: Vec::new(),
                revision: 0,
                state,
                state_changed_at: Some(chrono::Utc::now()),
                filled_qty: if state == OrderState::PartiallyFilled {
                    dec!(0.5)
                } else {
                    dec!(0)
                },
                rejection_reason: None,
                last_error: None,
                idempotency_key: None,
            }],
            ..TradingRuntimeSnapshot::default()
        })
    }

    fn create_flat_idempotent_paper_history(
        daemon: &mut PloyDaemon,
        deployment_id: &str,
    ) -> (TradingIntent, PaperIntentResponse) {
        daemon
            .apply_deployment(DeploymentApplyRequest {
                deployment_id: deployment_id.to_string(),
                bundle_id: "old-paper".to_string(),
                runtime_mode: DeploymentRuntimeMode::Paper,
                account_id: "paper:test-mode".to_string(),
                max_gross_exposure: Some(dec!(5)),
                deployment_state: DeploymentState::Enabled,
                desired_state: DesiredState::Running,
            })
            .expect("apply paper");
        let intent = TradingIntent {
            intent_id: format!("intent-{deployment_id}"),
            deployment_id: deployment_id.to_string(),
            market_id: "market-1".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(1),
            limit_price: Some(dec!(0.5)),
            purpose: IntentPurpose::Entry,
            created_at: chrono::Utc::now(),
        };
        let response = daemon
            .submit_intent_idempotent(intent.clone(), Some("mode-stable-key"))
            .expect("submit idempotent paper intent");
        daemon
            .cancel_order(deployment_id, &response.order_id)
            .expect("cancel to flat terminal history");
        daemon
            .control_deployment(
                deployment_id,
                DeploymentControlRequest {
                    desired_state: Some(DesiredState::Paused),
                    deployment_state: None,
                },
            )
            .expect("pause flat deployment");
        (intent, response)
    }

    #[derive(Debug, Default, Clone)]
    struct FlakyReconcileGateway {
        attempts: Arc<Mutex<usize>>,
    }

    #[derive(Debug)]
    struct PendingPersistGateway {
        trading_state_file: PathBuf,
    }

    #[derive(Debug, Clone)]
    struct CountingAckGateway {
        submits: Arc<AtomicUsize>,
    }

    impl LiveExecutionGateway for CountingAckGateway {
        fn probe(&self) -> Result<(), ExecutionError> {
            Ok(())
        }

        fn submit(&self, _request: &ExecutionRequest) -> Result<ExecutionOutcome, ExecutionError> {
            let attempt = self.submits.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(ExecutionOutcome::Acknowledged {
                venue_order_id: format!("venue-{attempt}"),
            })
        }

        fn cancel(
            &self,
            _request: &CancellationRequest,
        ) -> Result<CancellationOutcome, ExecutionError> {
            Ok(CancellationOutcome::Canceled)
        }

        fn replace(&self, _request: &ReplaceRequest) -> Result<ReplaceOutcome, ExecutionError> {
            unreachable!()
        }

        fn reconcile_fills(
            &self,
            _tracked_orders: &[ploy_connectivity::TrackedOrder],
        ) -> Result<Vec<FillRecord>, ExecutionError> {
            Ok(Vec::new())
        }
    }

    impl LiveExecutionGateway for PendingPersistGateway {
        fn probe(&self) -> Result<(), ExecutionError> {
            Ok(())
        }

        fn submit(&self, _request: &ExecutionRequest) -> Result<ExecutionOutcome, ExecutionError> {
            let snapshots: serde_json::Value = serde_json::from_slice(
                &fs::read(&self.trading_state_file).expect("pending state persisted before submit"),
            )
            .expect("pending state json");
            assert_eq!(snapshots[0]["orders"][0]["state"], "pending");
            Err(ExecutionError::Transport("response lost".to_string()))
        }

        fn cancel(
            &self,
            _request: &CancellationRequest,
        ) -> Result<CancellationOutcome, ExecutionError> {
            Ok(CancellationOutcome::Canceled)
        }

        fn replace(&self, _request: &ReplaceRequest) -> Result<ReplaceOutcome, ExecutionError> {
            unreachable!()
        }

        fn reconcile_fills(
            &self,
            _tracked_orders: &[ploy_connectivity::TrackedOrder],
        ) -> Result<Vec<FillRecord>, ExecutionError> {
            Ok(Vec::new())
        }
    }

    impl LiveExecutionGateway for FlakyReconcileGateway {
        fn probe(&self) -> Result<(), ExecutionError> {
            Ok(())
        }

        fn submit(&self, _request: &ExecutionRequest) -> Result<ExecutionOutcome, ExecutionError> {
            Ok(ExecutionOutcome::Acknowledged {
                venue_order_id: "venue-live-health-1".to_string(),
            })
        }

        fn cancel(
            &self,
            _request: &CancellationRequest,
        ) -> Result<CancellationOutcome, ExecutionError> {
            Ok(CancellationOutcome::Canceled)
        }

        fn replace(&self, _request: &ReplaceRequest) -> Result<ReplaceOutcome, ExecutionError> {
            Ok(ReplaceOutcome::Replaced {
                venue_order_id: "venue-live-health-2".to_string(),
            })
        }

        fn reconcile_fills(
            &self,
            _tracked_orders: &[ploy_connectivity::TrackedOrder],
        ) -> Result<Vec<FillRecord>, ExecutionError> {
            let mut attempts = self.attempts.lock().expect("attempts lock");
            *attempts += 1;
            if *attempts == 1 {
                Err(ExecutionError::Transport("gateway offline".to_string()))
            } else {
                Ok(Vec::new())
            }
        }
    }

    #[test]
    fn boot_persists_unsafe_live_account_scope_quarantine() {
        let root = temp_dir("legacy-live-cutover");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("mkdir");
        fs::write(
            &registry_file,
            serde_json::json!([{
                "deployment_id": "legacy.live",
                "bundle_id": "example",
                "runtime_mode": "live",
                "account_id": "legacy-live-wallet",
                "max_gross_exposure": "5",
                "deployment_state": "enabled",
                "desired_state": "running",
                "observed_state": "starting"
            }])
            .to_string(),
        )
        .expect("registry");
        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            ..PlatformConfig::default()
        };

        seed_empty_live_ledgers(&config);
        let daemon = PloyDaemon::boot(&config).expect("boot fail closed");
        let deployment = daemon
            .inspect_deployment("legacy.live")
            .expect("deployment");
        assert_eq!(deployment.desired_state, DesiredState::Paused);
        assert_eq!(deployment.observed_state, ObservedState::Degraded);
        assert!(daemon.trading.contains_key("legacy.live"));
        assert!(daemon.supervisor.status("legacy.live").is_none());
        let persisted: serde_json::Value =
            serde_json::from_slice(&fs::read(&config.registry_file).expect("quarantined registry"))
                .expect("registry json");
        assert_eq!(persisted[0]["desired_state"], "paused");
        assert_eq!(persisted[0]["observed_state"], "degraded");

        let restarted = PloyDaemon::boot(&config).expect("restart fail closed");
        assert!(restarted.trading.contains_key("legacy.live"));
        assert_eq!(
            restarted
                .inspect_deployment("legacy.live")
                .expect("persisted quarantine")
                .desired_state,
            DesiredState::Paused
        );
        assert!(restarted.supervisor.status("legacy.live").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn paper_snapshot_with_live_registry_is_quarantined_and_pid_is_killed() {
        let root = temp_dir("snapshot-mode-mismatch");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("mkdir");
        fs::write(
            &registry_file,
            serde_json::json!([{
                "deployment_id":"mismatch.live", "bundle_id":"example",
                "runtime_mode":"live", "account_id": LIVE_WALLET,
                "max_gross_exposure":"5",
                "deployment_state":"enabled", "desired_state":"running",
                "observed_state":"starting"
            }])
            .to_string(),
        )
        .expect("registry");
        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            runner_binary: PathBuf::from("/bin/sh"),
            ..PlatformConfig::default()
        };
        let paper_record = DeploymentRecord {
            deployment_id: "mismatch.live".to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: DeploymentRuntimeMode::Paper,
            account_id: "paper:test-mismatch".to_string(),
            max_gross_exposure: None,
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Running,
            observed_state: ObservedState::Running,
        };
        super::write_json(
            &config.trading_state_file,
            &[super::build_trading_state_snapshot(
                paper_record,
                TradingRuntime::default().snapshot(&BTreeMap::new()),
            )],
        )
        .expect("paper snapshot");
        let pid_file = runtime_root.join("workers/mismatch.live.pid");
        let ready_file = runtime_root.join("workers/mismatch.live.ready");
        fs::create_dir_all(pid_file.parent().expect("pid parent")).expect("pid parent");
        let mut legacy = std::process::Command::new("/bin/sh")
            .args([
                "-c",
                "touch \"$READY_FILE\"; sleep 30; :",
                "worker",
                "--deployment-id",
                "mismatch.live",
            ])
            .env("READY_FILE", &ready_file)
            .spawn()
            .expect("legacy worker");
        let legacy_pid = legacy.id();
        let ready = (0..100).any(|_| {
            if ready_file.exists() {
                true
            } else {
                std::thread::sleep(std::time::Duration::from_millis(10));
                false
            }
        });
        assert!(ready, "legacy worker did not finish exec setup");
        fs::write(&pid_file, format!("{legacy_pid}\n")).expect("pidfile");

        let daemon = PloyDaemon::boot(&config).expect("fail-closed boot");
        let record = daemon.inspect_deployment("mismatch.live").expect("record");
        assert_eq!(record.desired_state, DesiredState::Paused);
        assert_eq!(record.observed_state, ObservedState::Degraded);
        assert!(!daemon.trading.contains_key("mismatch.live"));
        assert!(daemon.supervisor.status("mismatch.live").is_none());
        let exited = (0..100).any(|_| match legacy.try_wait() {
            Ok(Some(_)) => true,
            Ok(None) => {
                std::thread::sleep(std::time::Duration::from_millis(10));
                false
            }
            Err(error) => panic!("wait legacy: {error}"),
        });
        if !exited {
            let _ = legacy.kill();
            let _ = legacy.wait();
        }
        assert!(exited, "mode-mismatched legacy worker survived cutover");
    }

    #[test]
    fn new_live_apply_seeds_and_persists_empty_canonical_ledger() {
        let root = temp_dir("new-live-canonical-ledger");
        let runtime_root = root.join("run/platform");
        let config = PlatformConfig {
            registry_file: root.join("data/state/deployments.json"),
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            ..PlatformConfig::default()
        };
        let mut daemon = PloyDaemon::boot(&config).expect("boot");
        daemon
            .apply_deployment(paused_live_request("new.live", LIVE_WALLET))
            .expect("apply new live");

        assert!(daemon.trading.contains_key("new.live"));
        let snapshots: serde_json::Value =
            serde_json::from_slice(&fs::read(&config.trading_state_file).expect("ledger file"))
                .expect("ledger json");
        let snapshot = snapshots
            .as_array()
            .and_then(|items| {
                items
                    .iter()
                    .find(|item| item["deployment_id"] == "new.live")
            })
            .expect("new live canonical ledger");
        assert_eq!(snapshot["orders"], serde_json::json!([]));
        assert_eq!(snapshot["positions"], serde_json::json!([]));
    }

    #[test]
    fn failed_first_live_ledger_persist_rolls_back_new_apply_without_resurrection() {
        let root = temp_dir("apply-ledger-failure-rollback");
        let runtime_root = root.join("run/platform");
        let config = PlatformConfig {
            registry_file: root.join("data/state/deployments.json"),
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            ..PlatformConfig::default()
        };
        let mut daemon = PloyDaemon::boot(&config).expect("boot");
        daemon.fail_trading_state_write_on_attempt = Some(1);

        daemon
            .apply_deployment(paused_live_request("failed.live", LIVE_WALLET))
            .expect_err("injected ledger persistence failure");
        assert!(daemon.inspect_deployment("failed.live").is_none());
        assert!(!daemon.trading.contains_key("failed.live"));
        assert!(daemon.supervisor.status("failed.live").is_none());

        daemon.write_runtime_snapshots().expect("later tick");
        assert!(daemon.inspect_deployment("failed.live").is_none());
        assert!(!daemon.trading.contains_key("failed.live"));
        assert!(daemon.supervisor.status("failed.live").is_none());
    }

    #[test]
    fn failed_registry_persist_after_ledger_write_rolls_back_and_restarts_absent() {
        let root = temp_dir("apply-registry-failure-rollback");
        let runtime_root = root.join("run/platform");
        let config = PlatformConfig {
            registry_file: root.join("data/state/deployments.json"),
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            ..PlatformConfig::default()
        };
        let mut daemon = PloyDaemon::boot(&config).expect("boot");
        daemon.fail_registry_write_on_attempt = Some(daemon.registry_write_attempts + 1);

        daemon
            .apply_deployment(paused_live_request("failed.live", LIVE_WALLET))
            .expect_err("injected registry persistence failure");
        assert!(daemon.inspect_deployment("failed.live").is_none());
        assert!(!daemon.trading.contains_key("failed.live"));
        assert!(daemon.supervisor.status("failed.live").is_none());

        daemon.write_runtime_snapshots().expect("later tick");
        let restarted = PloyDaemon::boot(&config).expect("restart");
        assert!(restarted.inspect_deployment("failed.live").is_none());
        assert!(!restarted.trading.contains_key("failed.live"));
        assert!(restarted.supervisor.status("failed.live").is_none());
    }

    #[test]
    fn failed_update_persist_restores_old_record_runtime_and_idempotency() {
        let root = temp_dir("apply-update-failure-rollback");
        let runtime_root = root.join("run/platform");
        let config = PlatformConfig {
            registry_file: root.join("data/state/deployments.json"),
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            ..PlatformConfig::default()
        };
        let mut daemon = PloyDaemon::boot(&config).expect("boot");
        daemon
            .apply_deployment(DeploymentApplyRequest {
                deployment_id: "existing.paper".to_string(),
                bundle_id: "old-bundle".to_string(),
                runtime_mode: DeploymentRuntimeMode::Paper,
                account_id: "paper:test-old".to_string(),
                max_gross_exposure: Some(dec!(5)),
                deployment_state: DeploymentState::Enabled,
                desired_state: DesiredState::Running,
            })
            .expect("initial apply");
        let intent = TradingIntent {
            intent_id: "intent-existing".to_string(),
            deployment_id: "existing.paper".to_string(),
            market_id: "market-1".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(1),
            limit_price: Some(dec!(0.5)),
            purpose: IntentPurpose::Entry,
            created_at: chrono::Utc::now(),
        };
        let original_response = daemon
            .submit_intent_idempotent(intent.clone(), Some("stable-key"))
            .expect("submit");
        daemon
            .control_deployment(
                "existing.paper",
                DeploymentControlRequest {
                    desired_state: Some(DesiredState::Paused),
                    deployment_state: None,
                },
            )
            .expect("pause");
        let old_record = daemon
            .inspect_deployment("existing.paper")
            .expect("old record");
        let old_runtime = daemon
            .trading
            .get("existing.paper")
            .expect("old runtime")
            .snapshot(&BTreeMap::new());
        daemon.fail_registry_write_on_attempt = Some(daemon.registry_write_attempts + 1);

        daemon
            .apply_deployment(DeploymentApplyRequest {
                deployment_id: "existing.paper".to_string(),
                bundle_id: "new-bundle".to_string(),
                runtime_mode: DeploymentRuntimeMode::Paper,
                account_id: "paper:test-old".to_string(),
                max_gross_exposure: Some(dec!(5)),
                deployment_state: DeploymentState::Enabled,
                desired_state: DesiredState::Paused,
            })
            .expect_err("injected update persistence failure");

        assert_eq!(
            daemon.inspect_deployment("existing.paper"),
            Some(old_record)
        );
        assert_eq!(
            daemon
                .trading
                .get("existing.paper")
                .expect("restored runtime")
                .snapshot(&BTreeMap::new()),
            old_runtime
        );
        let replay = daemon
            .submit_intent_idempotent(intent, Some("stable-key"))
            .expect("restored idempotency replay");
        assert_eq!(replay, original_response);
    }

    #[test]
    fn second_registry_write_failure_accepts_core_apply_and_marks_degraded() {
        let root = temp_dir("apply-derived-registry-failure");
        let runtime_root = root.join("run/platform");
        let config = PlatformConfig {
            registry_file: root.join("data/state/deployments.json"),
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            ..PlatformConfig::default()
        };
        let mut daemon = PloyDaemon::boot(&config).expect("boot");
        daemon.fail_registry_write_on_attempt = Some(daemon.registry_write_attempts + 2);
        let mut request = paused_live_request("accepted.live", LIVE_WALLET);
        request.desired_state = DesiredState::Running;

        let applied = daemon
            .apply_deployment(request.clone())
            .expect("core-persisted apply is accepted");
        assert_eq!(applied.deployment_id, "accepted.live");
        assert!(config.registry_file.exists());
        assert!(config.trading_state_file.exists());
        assert_eq!(daemon.control_plane.deployments.records().len(), 1);
        assert!(daemon.control_plane.system.is_degraded());
        assert!(!daemon.active_alerts().is_empty());
        let first_pid = daemon
            .supervisor
            .status("accepted.live")
            .and_then(|status| status.pid);

        daemon
            .apply_deployment(request)
            .expect("idempotent client retry");
        assert_eq!(daemon.control_plane.deployments.records().len(), 1);
        assert_eq!(
            daemon
                .supervisor
                .status("accepted.live")
                .and_then(|status| status.pid),
            first_pid
        );
    }

    #[test]
    fn status_snapshot_failure_accepts_core_apply_and_marks_degraded() {
        let root = temp_dir("apply-derived-status-failure");
        let runtime_root = root.join("run/platform");
        let config = PlatformConfig {
            registry_file: root.join("data/state/deployments.json"),
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            ..PlatformConfig::default()
        };
        let mut daemon = PloyDaemon::boot(&config).expect("boot");
        daemon.fail_status_write_on_attempt = Some(1);
        let mut request = paused_live_request("accepted.live", LIVE_WALLET);
        request.desired_state = DesiredState::Running;

        daemon
            .apply_deployment(request.clone())
            .expect("core-persisted apply is accepted");
        assert!(config.registry_file.exists());
        assert!(config.trading_state_file.exists());
        assert_eq!(daemon.control_plane.deployments.records().len(), 1);
        assert!(daemon.control_plane.system.is_degraded());
        assert!(!daemon.active_alerts().is_empty());

        daemon
            .apply_deployment(request)
            .expect("idempotent client retry");
        assert_eq!(daemon.control_plane.deployments.records().len(), 1);
    }

    #[test]
    fn paper_to_live_core_snapshot_stays_aligned_when_derived_status_write_fails() {
        let root = temp_dir("paper-live-derived-failure");
        let runtime_root = root.join("run/platform");
        let config = PlatformConfig {
            registry_file: root.join("data/state/deployments.json"),
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            ..PlatformConfig::default()
        };
        let mut daemon = PloyDaemon::boot(&config).expect("boot");
        let (_intent, _response) =
            create_flat_idempotent_paper_history(&mut daemon, "aligned.mode");
        daemon.fail_status_write_on_attempt = Some(daemon.status_write_attempts + 1);

        let applied = daemon
            .apply_deployment(DeploymentApplyRequest {
                deployment_id: "aligned.mode".to_string(),
                bundle_id: "new-live".to_string(),
                runtime_mode: DeploymentRuntimeMode::Live,
                account_id: LIVE_WALLET.to_string(),
                max_gross_exposure: Some(dec!(5)),
                deployment_state: DeploymentState::Enabled,
                desired_state: DesiredState::Running,
            })
            .expect("durable mode change accepted despite derived failure");
        assert_eq!(applied.runtime_mode, DeploymentRuntimeMode::Live);
        let registry: serde_json::Value =
            serde_json::from_slice(&fs::read(&config.registry_file).expect("registry"))
                .expect("registry json");
        let snapshots: serde_json::Value =
            serde_json::from_slice(&fs::read(&config.trading_state_file).expect("snapshot"))
                .expect("snapshot json");
        assert_eq!(registry[0]["runtime_mode"], "live");
        assert_eq!(snapshots[0]["runtime_mode"], "live");

        let mut restarted = PloyDaemon::boot(&config).expect("restart aligned live");
        assert!(restarted.trading.contains_key("aligned.mode"));
        let record = restarted
            .inspect_deployment("aligned.mode")
            .expect("record");
        assert_eq!(record.runtime_mode, DeploymentRuntimeMode::Live);
        assert_eq!(record.desired_state, DesiredState::Running);
        restarted.supervisor.stop("aligned.mode");
        daemon.supervisor.stop("aligned.mode");
    }

    #[test]
    fn paper_to_live_core_ledger_failure_restores_paper_memory_and_disk() {
        let root = temp_dir("paper-live-core-failure");
        let runtime_root = root.join("run/platform");
        let config = PlatformConfig {
            registry_file: root.join("data/state/deployments.json"),
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            ..PlatformConfig::default()
        };
        let mut daemon = PloyDaemon::boot(&config).expect("boot");
        create_flat_idempotent_paper_history(&mut daemon, "rollback.mode");
        daemon.fail_trading_state_write_on_attempt = Some(daemon.trading_state_write_attempts + 1);

        daemon
            .apply_deployment(DeploymentApplyRequest {
                deployment_id: "rollback.mode".to_string(),
                bundle_id: "new-live".to_string(),
                runtime_mode: DeploymentRuntimeMode::Live,
                account_id: LIVE_WALLET.to_string(),
                max_gross_exposure: Some(dec!(5)),
                deployment_state: DeploymentState::Enabled,
                desired_state: DesiredState::Running,
            })
            .expect_err("core ledger write must fail mode change");
        assert_eq!(
            daemon
                .inspect_deployment("rollback.mode")
                .expect("record")
                .runtime_mode,
            DeploymentRuntimeMode::Paper
        );
        let registry: serde_json::Value =
            serde_json::from_slice(&fs::read(&config.registry_file).expect("registry"))
                .expect("registry json");
        let snapshots: serde_json::Value =
            serde_json::from_slice(&fs::read(&config.trading_state_file).expect("snapshot"))
                .expect("snapshot json");
        assert_eq!(registry[0]["runtime_mode"], "paper");
        assert_eq!(snapshots[0]["runtime_mode"], "paper");
        let restarted = PloyDaemon::boot(&config).expect("restart paper");
        assert_eq!(
            restarted
                .inspect_deployment("rollback.mode")
                .expect("record")
                .runtime_mode,
            DeploymentRuntimeMode::Paper
        );
    }

    #[test]
    fn live_to_paper_persists_aligned_snapshot_and_idempotency_history() {
        let root = temp_dir("live-paper-history");
        let runtime_root = root.join("run/platform");
        let config = PlatformConfig {
            registry_file: root.join("data/state/deployments.json"),
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            ..PlatformConfig::default()
        };
        let mut daemon = PloyDaemon::boot(&config).expect("boot");
        let (intent, original_response) =
            create_flat_idempotent_paper_history(&mut daemon, "history.mode");
        daemon
            .apply_deployment(DeploymentApplyRequest {
                deployment_id: "history.mode".to_string(),
                bundle_id: "live-bundle".to_string(),
                runtime_mode: DeploymentRuntimeMode::Live,
                account_id: LIVE_WALLET.to_string(),
                max_gross_exposure: Some(dec!(5)),
                deployment_state: DeploymentState::Enabled,
                desired_state: DesiredState::Paused,
            })
            .expect("paper to live");
        daemon
            .apply_deployment(DeploymentApplyRequest {
                deployment_id: "history.mode".to_string(),
                bundle_id: "paper-bundle".to_string(),
                runtime_mode: DeploymentRuntimeMode::Paper,
                account_id: "paper:test-mode".to_string(),
                max_gross_exposure: Some(dec!(5)),
                deployment_state: DeploymentState::Enabled,
                desired_state: DesiredState::Paused,
            })
            .expect("live to paper");
        let snapshots: serde_json::Value =
            serde_json::from_slice(&fs::read(&config.trading_state_file).expect("snapshot"))
                .expect("snapshot json");
        assert_eq!(snapshots[0]["runtime_mode"], "paper");
        assert_eq!(snapshots[0]["orders"][0]["state"], "canceled");
        assert_eq!(
            snapshots[0]["orders"][0]["idempotency_key"],
            "mode-stable-key"
        );

        let mut restarted = PloyDaemon::boot(&config).expect("restart paper");
        let replay = restarted
            .submit_intent_idempotent(intent, Some("mode-stable-key"))
            .expect("idempotency history restored");
        assert_eq!(replay.order_id, original_response.order_id);
        assert_eq!(replay.state, "canceled");
    }

    #[test]
    fn paper_registry_without_snapshot_keeps_existing_bootstrap_behavior() {
        let root = temp_dir("paper-bootstrap-unchanged");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("mkdir");
        fs::write(
            &registry_file,
            serde_json::json!([{
                "deployment_id": "legacy.paper",
                "bundle_id": "example",
                "runtime_mode": "paper",
                "account_id": "paper:test-legacy",
                "max_gross_exposure": "5",
                "deployment_state": "enabled",
                "desired_state": "running",
                "observed_state": "starting"
            }])
            .to_string(),
        )
        .expect("registry");
        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            ..PlatformConfig::default()
        };

        let daemon = PloyDaemon::boot(&config).expect("paper boot");
        assert!(daemon.trading.contains_key("legacy.paper"));
        assert!(daemon.supervisor.status("legacy.paper").is_some());
    }

    #[test]
    fn mode_change_rejects_each_nonterminal_order_state_before_registry_mutation() {
        for state in [
            OrderState::Pending,
            OrderState::Acknowledged,
            OrderState::PartiallyFilled,
            OrderState::Unknown,
        ] {
            let root = temp_dir("mode-change-active-order");
            let runtime_root = root.join("run/platform");
            let config = PlatformConfig {
                registry_file: root.join("data/state/deployments.json"),
                runtime_root: runtime_root.clone(),
                status_file: runtime_root.join("system-status.json"),
                deployment_status_file: runtime_root.join("deployments.json"),
                trading_state_file: runtime_root.join("trading-state.json"),
                ..PlatformConfig::default()
            };
            let mut daemon = PloyDaemon::boot(&config).expect("boot");
            daemon
                .apply_deployment(paused_live_request("reassign.live", LIVE_WALLET))
                .expect("apply");
            daemon.trading.insert(
                "reassign.live".to_string(),
                order_runtime("reassign.live", state),
            );

            let mut request = paused_live_request("reassign.live", LIVE_WALLET);
            request.runtime_mode = DeploymentRuntimeMode::Paper;
            request.account_id = "paper:test-reassigned".to_string();
            let error = daemon
                .apply_deployment(request)
                .expect_err("nonterminal order blocks mode reassignment");
            assert_eq!(error.kind(), ErrorKind::InvalidInput);
            assert_eq!(
                daemon
                    .inspect_deployment("reassign.live")
                    .expect("record")
                    .runtime_mode,
                DeploymentRuntimeMode::Live
            );
        }
    }

    #[test]
    fn account_move_rejects_nonzero_position_before_registry_mutation() {
        let root = temp_dir("account-move-position");
        let runtime_root = root.join("run/platform");
        let config = PlatformConfig {
            registry_file: root.join("data/state/deployments.json"),
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            ..PlatformConfig::default()
        };
        let mut daemon = PloyDaemon::boot(&config).expect("boot");
        daemon
            .apply_deployment(paused_live_request("move.live", LIVE_WALLET))
            .expect("apply");
        daemon.trading.insert(
            "move.live".to_string(),
            TradingRuntime::restore(TradingRuntimeSnapshot {
                positions: vec![PositionSnapshot {
                    token_id: "token-1".to_string(),
                    net_qty: dec!(1),
                    avg_entry_price: dec!(0.5),
                    realized_pnl: dec!(0),
                }],
                ..TradingRuntimeSnapshot::default()
            }),
        );

        let error = daemon
            .apply_deployment(paused_live_request("move.live", OTHER_LIVE_WALLET))
            .expect_err("position blocks account move");
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert_eq!(
            daemon
                .inspect_deployment("move.live")
                .expect("record")
                .account_id,
            LIVE_WALLET
        );
    }

    #[test]
    fn flat_ledger_with_terminal_history_allows_mode_and_account_reassignment() {
        let root = temp_dir("flat-reassignment");
        let runtime_root = root.join("run/platform");
        let config = PlatformConfig {
            registry_file: root.join("data/state/deployments.json"),
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            ..PlatformConfig::default()
        };
        let mut daemon = PloyDaemon::boot(&config).expect("boot");
        daemon
            .apply_deployment(paused_live_request("flat.live", LIVE_WALLET))
            .expect("apply");
        daemon.trading.insert(
            "flat.live".to_string(),
            order_runtime("flat.live", OrderState::Canceled),
        );
        let mut request = paused_live_request("flat.live", LIVE_WALLET);
        request.runtime_mode = DeploymentRuntimeMode::Paper;
        request.account_id = "paper:test-new".to_string();

        let updated = daemon.apply_deployment(request).expect("flat reassignment");
        assert_eq!(updated.runtime_mode, DeploymentRuntimeMode::Paper);
        assert_eq!(updated.account_id, "paper:test-new");
    }

    #[test]
    fn daemon_loads_platform_config() {
        let config = PlatformConfig {
            listen_addr: "127.0.0.1:9090".to_string(),
            ..PlatformConfig::default()
        };

        let daemon = PloyDaemon::boot(&config).expect("boot");
        let status = daemon.control_plane.system.status();
        assert_eq!(status.status, "running@127.0.0.1:9090");
    }

    #[test]
    fn daemon_writes_runtime_snapshots_for_operator_clients() {
        let root = temp_dir("snapshots");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.paper",
                    "bundle_id": "example",
                    "runtime_mode": "paper",
                    "account_id": "paper:test-restored",
                    "desired_state": "running",
                    "observed_state": "starting"
                }
            ])
            .to_string(),
        )
        .expect("write registry");

        let config = PlatformConfig {
            registry_file: registry_file.clone(),
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            ..PlatformConfig::default()
        };

        let mut daemon = PloyDaemon::boot(&config).expect("boot");
        daemon.write_runtime_snapshots().expect("write snapshots");

        let status: ploy_operator_contracts::SystemStatus =
            serde_json::from_str(&fs::read_to_string(&config.status_file).expect("status file"))
                .expect("status json");
        assert!(status.status.starts_with("running@"));
        assert!(status.database_connected);

        let deployments: Vec<ploy_operator_contracts::DeploymentSummary> = serde_json::from_str(
            &fs::read_to_string(&config.deployment_status_file).expect("deployment file"),
        )
        .expect("deployment json");
        assert_eq!(deployments.len(), 1);
        assert_eq!(deployments[0].deployment_id, "example.paper");
        assert_eq!(deployments[0].desired_state, DesiredState::Running);
        assert_eq!(deployments[0].observed_state, ObservedState::Running);
    }

    #[test]
    fn daemon_records_paper_trade_into_trading_state_snapshot() {
        let root = temp_dir("trading-state");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.paper",
                    "bundle_id": "example",
                    "runtime_mode": "paper",
                    "account_id": "paper:test-restored",
                    "desired_state": "running",
                    "observed_state": "starting"
                }
            ])
            .to_string(),
        )
        .expect("write registry");

        let config = PlatformConfig {
            registry_file: registry_file.clone(),
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            ..PlatformConfig::default()
        };

        let mut daemon = PloyDaemon::boot(&config).expect("boot");
        daemon
            .submit_paper_intent(TradingIntent {
                intent_id: "intent-1".to_string(),
                deployment_id: "example.paper".to_string(),
                market_id: "market-1".to_string(),
                token_id: "yes-token".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(5),
                limit_price: Some(dec!(0.40)),
                purpose: IntentPurpose::Entry,
                created_at: chrono::Utc::now(),
            })
            .expect("submit intent");
        daemon.record_fill(
            "example.paper",
            FillRecord {
                fill_id: "fill-1".to_string(),
                order_id: "order-intent-1".to_string(),
                token_id: "yes-token".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(5),
                price: dec!(0.40),
                fee: dec!(0.05),
                timestamp: chrono::Utc::now(),
            },
        );
        daemon.write_runtime_snapshots().expect("write snapshots");

        let trading_state: Vec<ploy_operator_contracts::TradingStateSnapshot> =
            serde_json::from_str(
                &fs::read_to_string(&config.trading_state_file).expect("trading state file"),
            )
            .expect("trading state json");
        assert_eq!(trading_state.len(), 1);
        assert_eq!(trading_state[0].deployment_id, "example.paper");
        assert_eq!(trading_state[0].orders.len(), 1);
        assert_eq!(trading_state[0].fills.len(), 1);
        assert_eq!(trading_state[0].positions.len(), 1);
        assert_eq!(trading_state[0].risk.open_positions, 1);
    }

    #[test]
    fn daemon_rejects_paper_intent_when_deployment_is_not_running() {
        let root = temp_dir("paper-intent-gate");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.paper",
                    "bundle_id": "example",
                    "runtime_mode": "paper",
                    "account_id": "paper:test-restored",
                    "desired_state": "paused",
                    "observed_state": "paused"
                }
            ])
            .to_string(),
        )
        .expect("write registry");

        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            ..PlatformConfig::default()
        };

        let mut daemon = PloyDaemon::boot(&config).expect("boot");
        let err = daemon
            .submit_paper_intent(TradingIntent {
                intent_id: "intent-1".to_string(),
                deployment_id: "example.paper".to_string(),
                market_id: "market-1".to_string(),
                token_id: "yes-token".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(5),
                limit_price: Some(dec!(0.40)),
                purpose: IntentPurpose::Entry,
                created_at: chrono::Utc::now(),
            })
            .expect_err("paused deployment should reject intents");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn idempotent_replay_precedes_current_state_gate_and_rejects_payload_mismatch() {
        let root = temp_dir("idempotent-state-gate");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([{
                "deployment_id": "example.paper",
                "bundle_id": "example",
                "runtime_mode": "paper",
                "account_id": "paper:test-restored",
                "desired_state": "running",
                "observed_state": "running"
            }])
            .to_string(),
        )
        .expect("write registry");
        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            ..PlatformConfig::default()
        };
        let make_intent = |intent_id: &str, quantity| TradingIntent {
            intent_id: intent_id.to_string(),
            deployment_id: "example.paper".to_string(),
            market_id: "market-1".to_string(),
            token_id: "yes-token".to_string(),
            side: TradeSide::Buy,
            quantity,
            limit_price: Some(dec!(0.40)),
            purpose: IntentPurpose::Entry,
            created_at: chrono::Utc::now(),
        };

        let mut daemon = PloyDaemon::boot(&config).expect("boot");
        let first = daemon
            .submit_intent_idempotent(make_intent("intent-1", dec!(2)), Some("request-1"))
            .expect("first submit");
        daemon
            .control_deployment(
                "example.paper",
                DeploymentControlRequest {
                    desired_state: Some(DesiredState::Paused),
                    deployment_state: None,
                },
            )
            .expect("pause deployment");

        let replay = daemon
            .submit_intent_idempotent(make_intent("intent-2", dec!(2)), Some("request-1"))
            .expect("replay bypasses current state gate");
        assert_eq!(replay, first);

        let mismatch = daemon
            .submit_intent_idempotent(make_intent("intent-3", dec!(1)), Some("request-1"))
            .expect_err("mismatched payload must reject");
        assert!(mismatch
            .to_string()
            .contains("idempotency key payload mismatch"));
    }

    #[test]
    fn account_scoped_idempotency_survives_restart_and_cross_deployment_replay() {
        let root = temp_dir("account-idempotency");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {"deployment_id":"a.live","bundle_id":"a","runtime_mode":"live","account_id":LIVE_WALLET,"max_gross_exposure":"5","desired_state":"running","observed_state":"running"},
                {"deployment_id":"b.live","bundle_id":"b","runtime_mode":"live","account_id":LIVE_WALLET,"max_gross_exposure":"5","desired_state":"running","observed_state":"running"},
                {"deployment_id":"c.paper","bundle_id":"c","runtime_mode":"paper","account_id":"paper:test-other","max_gross_exposure":"5","desired_state":"running","observed_state":"running"}
            ])
            .to_string(),
        )
        .expect("write registry");
        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            ..PlatformConfig::default()
        };
        let submits = Arc::new(AtomicUsize::new(0));
        let make_intent = |deployment_id: &str, intent_id: &str, quantity| TradingIntent {
            intent_id: intent_id.to_string(),
            deployment_id: deployment_id.to_string(),
            market_id: "market-1".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Buy,
            quantity,
            limit_price: Some(dec!(0.40)),
            purpose: IntentPurpose::Entry,
            created_at: chrono::Utc::now(),
        };
        seed_empty_live_ledgers(&config);
        let mut daemon = PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(CountingAckGateway {
                submits: submits.clone(),
            }),
        )
        .expect("boot");
        let first = daemon
            .submit_intent_idempotent(make_intent("a.live", "intent-a", dec!(1)), Some("key-1"))
            .expect("first submit");
        let replay = daemon
            .submit_intent_idempotent(make_intent("b.live", "intent-b", dec!(1)), Some("key-1"))
            .expect("same-account replay");
        assert_eq!(replay.order_id, first.order_id);
        assert_eq!(replay.deployment_id, "a.live");
        assert!(daemon
            .submit_intent_idempotent(
                make_intent("b.live", "intent-mismatch", dec!(2)),
                Some("key-1")
            )
            .expect_err("mismatch")
            .to_string()
            .contains("payload mismatch"));
        daemon
            .submit_intent_idempotent(make_intent("c.paper", "intent-c", dec!(1)), Some("key-1"))
            .expect("different account may reuse key");
        assert_eq!(submits.load(Ordering::SeqCst), 1);

        daemon
            .write_runtime_snapshots()
            .expect("persist before restart");
        let mut restored = PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(CountingAckGateway {
                submits: submits.clone(),
            }),
        )
        .expect("restore");
        let restored_replay = restored
            .submit_intent_idempotent(
                make_intent("b.live", "intent-after-restart", dec!(1)),
                Some("key-1"),
            )
            .expect("restored replay");
        assert_eq!(restored_replay.order_id, first.order_id);
        assert_eq!(submits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn daemon_routes_live_intent_into_acknowledged_order_snapshot() {
        let root = temp_dir("live-intent-ack");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.live",
                    "bundle_id": "example",
                    "runtime_mode": "live",
                    "account_id": LIVE_WALLET,
                    "max_gross_exposure": "5",
                    "desired_state": "running",
                    "observed_state": "starting"
                }
            ])
            .to_string(),
        )
        .expect("write registry");

        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            ..PlatformConfig::default()
        };

        seed_empty_live_ledgers(&config);
        let mut daemon = PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(StaticExecutionGateway::acknowledged("venue-live-1")),
        )
        .expect("boot");
        let response = daemon
            .submit_intent(TradingIntent {
                intent_id: "intent-live-1".to_string(),
                deployment_id: "example.live".to_string(),
                market_id: "market-1".to_string(),
                token_id: "yes-token".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(3),
                limit_price: Some(dec!(0.44)),
                purpose: IntentPurpose::Entry,
                created_at: chrono::Utc::now(),
            })
            .expect("submit live intent");

        assert_eq!(response.state, "acknowledged");

        let trading_state = daemon.trading_state();
        assert_eq!(trading_state.len(), 1);
        assert_eq!(trading_state[0].deployment_id, "example.live");
        assert_eq!(trading_state[0].orders.len(), 1);
        assert_eq!(trading_state[0].orders[0].state, "acknowledged");
        assert_eq!(
            trading_state[0].orders[0].venue_order_id.as_deref(),
            Some("venue-live-1")
        );
    }

    #[test]
    fn fresh_running_live_allows_risk_increase() {
        let root = temp_dir("fresh-running-live-intent");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([{
                "deployment_id": "example.live",
                "bundle_id": "example",
                "runtime_mode": "live",
                "account_id": LIVE_WALLET,
                "max_gross_exposure": "5",
                "deployment_state": "enabled",
                "desired_state": "running",
                "observed_state": "running"
            }])
            .to_string(),
        )
        .expect("write registry");
        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            ..PlatformConfig::default()
        };
        seed_empty_live_ledgers(&config);
        let mut blocked = PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(
                StaticExecutionGateway::acknowledged("unused").with_probe_result(Err(
                    ExecutionError::Transport("venue unreachable".to_string()),
                )),
            ),
        )
        .expect("boot fail closed");
        blocked
            .control_plane
            .deployments
            .set_observed_state("example.live", ObservedState::Running);
        let error = blocked
            .submit_intent_idempotent_from(
                TradingIntent {
                    intent_id: "intent-stale-live".to_string(),
                    deployment_id: "example.live".to_string(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(1),
                    limit_price: Some(dec!(0.5)),
                    purpose: IntentPurpose::Entry,
                    created_at: chrono::Utc::now(),
                },
                None,
                IntentAdmissionSource::Worker,
            )
            .expect_err("unreachable venue must block live risk increase");
        assert!(error.to_string().contains("fresh venue health"));

        let mut daemon = PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(StaticExecutionGateway::acknowledged("venue-fresh-1")),
        )
        .expect("boot");
        daemon
            .control_plane
            .deployments
            .set_observed_state("example.live", ObservedState::Running);

        let response = daemon
            .submit_intent_idempotent_from(
                TradingIntent {
                    intent_id: "intent-fresh-live".to_string(),
                    deployment_id: "example.live".to_string(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(1),
                    limit_price: Some(dec!(0.5)),
                    purpose: IntentPurpose::Entry,
                    created_at: chrono::Utc::now(),
                },
                None,
                IntentAdmissionSource::Worker,
            )
            .expect("fresh running live admission");

        assert_eq!(response.state, "acknowledged");
    }

    #[test]
    fn live_submission_persists_pending_before_side_effect_and_pauses_on_unknown() {
        let root = temp_dir("live-pending-before-submit");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([{
                "deployment_id": "example.live",
                "bundle_id": "example",
                "runtime_mode": "live",
                "account_id": LIVE_WALLET,
                "max_gross_exposure": "5",
                "desired_state": "running",
                "observed_state": "running"
            }])
            .to_string(),
        )
        .expect("write registry");
        let trading_state_file = runtime_root.join("trading-state.json");
        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: trading_state_file.clone(),
            ..PlatformConfig::default()
        };
        seed_empty_live_ledgers(&config);
        let mut daemon = PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(PendingPersistGateway { trading_state_file }),
        )
        .expect("boot");

        let response = daemon
            .submit_intent(TradingIntent {
                intent_id: "request-intent-unknown".to_string(),
                deployment_id: "example.live".to_string(),
                market_id: "market-1".to_string(),
                token_id: "yes-token".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(1),
                limit_price: Some(dec!(0.41)),
                purpose: IntentPurpose::Entry,
                created_at: chrono::Utc::now(),
            })
            .expect("unknown response");

        assert_eq!(response.state, "unknown");
        let deployment = daemon
            .inspect_deployment("example.live")
            .expect("deployment");
        assert_eq!(deployment.desired_state, DesiredState::Paused);
        assert_eq!(deployment.observed_state, ObservedState::Degraded);
    }

    #[test]
    fn acknowledged_submit_with_final_persistence_failure_becomes_durable_unknown() {
        let root = temp_dir("ack-final-persist-failure");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([{"deployment_id":"example.live","bundle_id":"example","runtime_mode":"live","account_id":LIVE_WALLET,"max_gross_exposure":"5","desired_state":"running","observed_state":"running"}]).to_string(),
        )
        .expect("registry");
        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            ..PlatformConfig::default()
        };
        let submits = Arc::new(AtomicUsize::new(0));
        let gateway = CountingAckGateway {
            submits: submits.clone(),
        };
        seed_empty_live_ledgers(&config);
        let mut daemon =
            PloyDaemon::boot_with_live_execution(&config, Box::new(gateway.clone())).expect("boot");
        daemon.fail_trading_state_write_on_attempt = Some(2);
        let intent = TradingIntent {
            intent_id: "intent-1".to_string(),
            deployment_id: "example.live".to_string(),
            market_id: "market-1".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(1),
            limit_price: Some(dec!(0.40)),
            purpose: IntentPurpose::Entry,
            created_at: chrono::Utc::now(),
        };
        let response = daemon
            .submit_intent_idempotent(intent.clone(), Some("key-1"))
            .expect("unknown response");
        assert_eq!(response.state, "unknown");
        assert_eq!(submits.load(Ordering::SeqCst), 1);
        let deployment = daemon
            .inspect_deployment("example.live")
            .expect("deployment");
        assert_eq!(deployment.desired_state, DesiredState::Paused);
        assert_eq!(deployment.observed_state, ObservedState::Degraded);
        let persisted: serde_json::Value = serde_json::from_slice(
            &fs::read(&config.trading_state_file).expect("durable unknown snapshot"),
        )
        .expect("snapshot json");
        assert_eq!(persisted[0]["orders"][0]["state"], "unknown");
        let replay = daemon
            .submit_intent_idempotent(intent, Some("key-1"))
            .expect("idempotent unknown replay");
        assert_eq!(replay.state, "unknown");
        assert_eq!(submits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn daemon_records_live_rejection_in_canonical_ledger() {
        let root = temp_dir("live-intent-reject");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.live",
                    "bundle_id": "example",
                    "runtime_mode": "live",
                    "account_id": LIVE_WALLET,
                    "max_gross_exposure": "5",
                    "desired_state": "running",
                    "observed_state": "starting"
                }
            ])
            .to_string(),
        )
        .expect("write registry");

        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            ..PlatformConfig::default()
        };

        seed_empty_live_ledgers(&config);
        let mut daemon = PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(StaticExecutionGateway::rejected("market closed")),
        )
        .expect("boot");
        let response = daemon
            .submit_intent(TradingIntent {
                intent_id: "intent-live-2".to_string(),
                deployment_id: "example.live".to_string(),
                market_id: "market-1".to_string(),
                token_id: "yes-token".to_string(),
                side: TradeSide::Sell,
                quantity: dec!(2),
                limit_price: Some(dec!(0.55)),
                purpose: IntentPurpose::Entry,
                created_at: chrono::Utc::now(),
            })
            .expect("submit live intent");

        assert_eq!(response.state, "rejected");

        let trading_state = daemon.trading_state();
        assert_eq!(trading_state[0].orders.len(), 1);
        assert_eq!(trading_state[0].orders[0].state, "rejected");
        assert_eq!(
            trading_state[0].orders[0].rejection_reason.as_deref(),
            Some("market closed")
        );
    }

    #[test]
    fn daemon_records_live_gateway_transport_ambiguity_as_unknown() {
        let root = temp_dir("live-intent-transport-error");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.live",
                    "bundle_id": "example",
                    "runtime_mode": "live",
                    "account_id": LIVE_WALLET,
                    "max_gross_exposure": "5",
                    "desired_state": "running",
                    "observed_state": "starting"
                }
            ])
            .to_string(),
        )
        .expect("write registry");

        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            ..PlatformConfig::default()
        };

        seed_empty_live_ledgers(&config);
        let mut daemon = PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(StaticExecutionGateway::failed(
                ploy_connectivity::ExecutionError::Transport("gateway offline".to_string()),
            )),
        )
        .expect("boot");
        let response = daemon
            .submit_intent(TradingIntent {
                intent_id: "intent-live-transport".to_string(),
                deployment_id: "example.live".to_string(),
                market_id: "market-1".to_string(),
                token_id: "yes-token".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(3),
                limit_price: Some(dec!(0.44)),
                purpose: IntentPurpose::Entry,
                created_at: chrono::Utc::now(),
            })
            .expect("transport ambiguity response");

        assert_eq!(response.state, "unknown");
        let trading_state = daemon.trading_state();
        assert_eq!(trading_state[0].orders.len(), 1);
        assert_eq!(trading_state[0].orders[0].state, "unknown");
        assert!(trading_state[0].orders[0].rejection_reason.is_none());
        assert!(trading_state[0].orders[0]
            .last_error
            .as_deref()
            .expect("last_error")
            .contains("gateway offline"));
    }

    #[test]
    fn daemon_cancels_live_order_through_gateway_and_updates_ledger() {
        let root = temp_dir("live-order-cancel");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.live",
                    "bundle_id": "example",
                    "runtime_mode": "live",
                    "account_id": LIVE_WALLET,
                    "max_gross_exposure": "5",
                    "desired_state": "running",
                    "observed_state": "starting"
                }
            ])
            .to_string(),
        )
        .expect("write registry");

        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            ..PlatformConfig::default()
        };

        let gateway = StaticExecutionGateway::acknowledged("venue-live-cancel-1")
            .with_cancel_result(Ok(CancellationOutcome::Canceled));
        seed_empty_live_ledgers(&config);
        let mut daemon =
            PloyDaemon::boot_with_live_execution(&config, Box::new(gateway)).expect("boot");
        daemon
            .submit_intent(TradingIntent {
                intent_id: "intent-live-cancel".to_string(),
                deployment_id: "example.live".to_string(),
                market_id: "market-1".to_string(),
                token_id: "yes-token".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(2),
                limit_price: Some(dec!(0.55)),
                purpose: IntentPurpose::Entry,
                created_at: chrono::Utc::now(),
            })
            .expect("submit live intent");

        let response = daemon
            .cancel_order("example.live", "order-intent-live-cancel")
            .expect("cancel live order");

        assert_eq!(response.state, "canceled");
        let trading_state = daemon.trading_state();
        assert_eq!(trading_state[0].orders[0].state, "canceled");
        assert_eq!(trading_state[0].risk.pending_intents, 0);
        assert_eq!(trading_state[0].risk.active_orders, 0);
    }

    #[test]
    fn daemon_replaces_live_order_through_gateway_and_preserves_logical_order() {
        let root = temp_dir("live-order-replace");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.live",
                    "bundle_id": "example",
                    "runtime_mode": "live",
                    "account_id": LIVE_WALLET,
                    "max_gross_exposure": "5",
                    "desired_state": "running",
                    "observed_state": "starting"
                }
            ])
            .to_string(),
        )
        .expect("write registry");

        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            ..PlatformConfig::default()
        };

        let gateway = StaticExecutionGateway::acknowledged("venue-live-replace-1")
            .with_replace_result(Ok(ReplaceOutcome::Replaced {
                venue_order_id: "venue-live-replace-2".to_string(),
            }));
        seed_empty_live_ledgers(&config);
        let mut daemon =
            PloyDaemon::boot_with_live_execution(&config, Box::new(gateway)).expect("boot");
        daemon
            .submit_intent(TradingIntent {
                intent_id: "intent-live-replace".to_string(),
                deployment_id: "example.live".to_string(),
                market_id: "market-1".to_string(),
                token_id: "yes-token".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(2),
                limit_price: Some(dec!(0.55)),
                purpose: IntentPurpose::Entry,
                created_at: chrono::Utc::now(),
            })
            .expect("submit live intent");

        let response = daemon
            .replace_order(
                "example.live",
                "order-intent-live-replace",
                ploy_operator_contracts::OrderReplaceRequest {
                    quantity: dec!(3),
                    limit_price: Some(dec!(0.57)),
                },
            )
            .expect("replace live order");

        assert_eq!(response.state, "acknowledged");
        assert_eq!(response.order_id, "order-intent-live-replace");
        assert_eq!(response.revision, 1);
        assert_eq!(
            response.venue_order_id.as_deref(),
            Some("venue-live-replace-2")
        );
        assert_eq!(
            response.venue_order_history,
            vec!["venue-live-replace-1".to_string()]
        );
        assert_eq!(response.requested_qty, dec!(3));
        assert_eq!(response.limit_price, Some(dec!(0.57)));

        let trading_state = daemon.trading_state();
        assert_eq!(
            trading_state[0].orders[0].order_id,
            "order-intent-live-replace"
        );
        assert_eq!(trading_state[0].orders[0].revision, 1);
        assert_eq!(
            trading_state[0].orders[0].venue_order_history,
            vec!["venue-live-replace-1".to_string()]
        );
        assert_eq!(
            trading_state[0].orders[0].venue_order_id.as_deref(),
            Some("venue-live-replace-2")
        );
    }

    #[test]
    fn daemon_rejects_replace_when_requested_qty_is_below_filled_qty() {
        let root = temp_dir("live-order-replace-invalid");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.live",
                    "bundle_id": "example",
                    "runtime_mode": "live",
                    "account_id": LIVE_WALLET,
                    "max_gross_exposure": "5",
                    "desired_state": "running",
                    "observed_state": "starting"
                }
            ])
            .to_string(),
        )
        .expect("write registry");

        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            ..PlatformConfig::default()
        };

        let fill = FillRecord {
            fill_id: "fill-live-replace-1".to_string(),
            order_id: "order-intent-live-replace-invalid".to_string(),
            token_id: "yes-token".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(2),
            price: dec!(0.55),
            fee: dec!(0.01),
            timestamp: chrono::Utc::now(),
        };
        seed_empty_live_ledgers(&config);
        let mut daemon = PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(
                StaticExecutionGateway::acknowledged("venue-live-replace-invalid-1")
                    .with_reconciled_fills(vec![fill]),
            ),
        )
        .expect("boot");
        daemon
            .submit_intent(TradingIntent {
                intent_id: "intent-live-replace-invalid".to_string(),
                deployment_id: "example.live".to_string(),
                market_id: "market-1".to_string(),
                token_id: "yes-token".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(3),
                limit_price: Some(dec!(0.55)),
                purpose: IntentPurpose::Entry,
                created_at: chrono::Utc::now(),
            })
            .expect("submit live intent");
        assert!(matches!(
            daemon.reconcile_live_fills().expect("reconcile fills"),
            ReconcileStatus::Applied(1)
        ));

        let error = daemon
            .replace_order(
                "example.live",
                "order-intent-live-replace-invalid",
                ploy_operator_contracts::OrderReplaceRequest {
                    quantity: dec!(1),
                    limit_price: Some(dec!(0.57)),
                },
            )
            .expect_err("replace should fail");

        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert!(error
            .to_string()
            .contains("cannot be below filled quantity"));
    }

    #[test]
    fn concurrent_account_submissions_cannot_exceed_cap() {
        let root = temp_dir("account-exposure-limit");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            ..PlatformConfig::default()
        };

        let mut daemon = PloyDaemon::boot(&config).expect("boot");
        for deployment_id in ["acct-a.paper", "acct-b.paper"] {
            daemon
                .apply_deployment(DeploymentApplyRequest {
                    deployment_id: deployment_id.to_string(),
                    bundle_id: "example".to_string(),
                    runtime_mode: DeploymentRuntimeMode::Paper,
                    account_id: "paper:test-shared".to_string(),
                    max_gross_exposure: Some(dec!(2.5)),
                    deployment_state: DeploymentState::Enabled,
                    desired_state: DesiredState::Running,
                })
                .expect("apply deployment");
        }

        let daemon = Arc::new(Mutex::new(daemon));
        let barrier = Arc::new(Barrier::new(3));
        let submissions = [
            ("acct-a.paper", "intent-account-a", "market-1", "yes-token"),
            ("acct-b.paper", "intent-account-b", "market-2", "no-token"),
        ]
        .map(|(deployment_id, intent_id, market_id, token_id)| {
            let daemon = daemon.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                daemon
                    .lock()
                    .expect("daemon lock")
                    .submit_intent(TradingIntent {
                        intent_id: intent_id.to_string(),
                        deployment_id: deployment_id.to_string(),
                        market_id: market_id.to_string(),
                        token_id: token_id.to_string(),
                        side: TradeSide::Buy,
                        quantity: dec!(3),
                        limit_price: Some(dec!(0.5)),
                        purpose: IntentPurpose::Entry,
                        created_at: chrono::Utc::now(),
                    })
            })
        });
        barrier.wait();
        let results = submissions.map(|handle| handle.join().expect("submission thread"));

        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        let error = results
            .into_iter()
            .find_map(Result::err)
            .expect("one submission must exceed the shared cap");
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert!(error.to_string().contains("paper:test-shared"));
        assert!(error.to_string().contains("max_gross_exposure"));
    }

    #[test]
    fn daemon_rejects_replacement_when_it_would_exceed_account_exposure_limit() {
        let root = temp_dir("account-exposure-replace");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            ..PlatformConfig::default()
        };

        let mut daemon = PloyDaemon::boot(&config).expect("boot");
        daemon
            .apply_deployment(DeploymentApplyRequest {
                deployment_id: "example.paper".to_string(),
                bundle_id: "example".to_string(),
                runtime_mode: DeploymentRuntimeMode::Paper,
                account_id: "paper:test-replace".to_string(),
                max_gross_exposure: Some(dec!(1.5)),
                deployment_state: DeploymentState::Enabled,
                desired_state: DesiredState::Running,
            })
            .expect("apply deployment");
        daemon
            .submit_intent(TradingIntent {
                intent_id: "intent-paper-replace".to_string(),
                deployment_id: "example.paper".to_string(),
                market_id: "market-1".to_string(),
                token_id: "yes-token".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(2),
                limit_price: Some(dec!(0.5)),
                purpose: IntentPurpose::Entry,
                created_at: chrono::Utc::now(),
            })
            .expect("submit intent");

        let error = daemon
            .replace_order(
                "example.paper",
                "order-intent-paper-replace",
                ploy_operator_contracts::OrderReplaceRequest {
                    quantity: dec!(4),
                    limit_price: Some(dec!(0.5)),
                },
            )
            .expect_err("replacement should exceed exposure limit");

        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert!(error.to_string().contains("paper:test-replace"));
        assert!(error.to_string().contains("next_total=2.0"));
    }

    #[test]
    fn daemon_pause_then_resume_restarts_paper_worker() {
        let root = temp_dir("pause-resume-worker");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            ..PlatformConfig::default()
        };

        let mut daemon = PloyDaemon::boot(&config).expect("boot");
        daemon
            .apply_deployment(DeploymentApplyRequest {
                deployment_id: "example.paper".to_string(),
                bundle_id: "example".to_string(),
                runtime_mode: DeploymentRuntimeMode::Paper,
                account_id: "paper:test-worker".to_string(),
                max_gross_exposure: Some(dec!(5)),
                deployment_state: DeploymentState::Enabled,
                desired_state: DesiredState::Running,
            })
            .expect("apply deployment");

        let initial_pid = daemon
            .supervisor
            .status("example.paper")
            .and_then(|status| status.pid)
            .expect("initial pid");

        daemon
            .control_deployment(
                "example.paper",
                DeploymentControlRequest {
                    desired_state: Some(DesiredState::Paused),
                    deployment_state: None,
                },
            )
            .expect("pause deployment");
        assert!(daemon
            .supervisor
            .status("example.paper")
            .and_then(|status| status.pid)
            .is_none());

        daemon
            .control_deployment(
                "example.paper",
                DeploymentControlRequest {
                    desired_state: Some(DesiredState::Running),
                    deployment_state: None,
                },
            )
            .expect("resume deployment");

        let resumed = daemon
            .supervisor
            .status("example.paper")
            .expect("resumed status");
        assert!(matches!(
            resumed.observed_state,
            ObservedState::Starting | ObservedState::Running
        ));
        assert!(resumed.pid.is_some());
        assert_ne!(resumed.pid, Some(initial_pid));
    }

    #[test]
    fn daemon_rejects_archive_with_active_orders() {
        let root = temp_dir("archive-active-orders");
        let runtime_root = root.join("run/platform");
        let config = PlatformConfig {
            registry_file: root.join("data/state/deployments.json"),
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            ..PlatformConfig::default()
        };
        let mut daemon = PloyDaemon::boot(&config).expect("boot");
        daemon
            .apply_deployment(DeploymentApplyRequest {
                deployment_id: "example.paper".to_string(),
                bundle_id: "example".to_string(),
                runtime_mode: DeploymentRuntimeMode::Paper,
                account_id: "paper:test-archive".to_string(),
                max_gross_exposure: Some(dec!(5)),
                deployment_state: DeploymentState::Enabled,
                desired_state: DesiredState::Running,
            })
            .expect("apply deployment");
        daemon
            .submit_paper_intent(TradingIntent {
                intent_id: "intent-active".to_string(),
                deployment_id: "example.paper".to_string(),
                market_id: "market-1".to_string(),
                token_id: "yes-token".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(1),
                limit_price: Some(dec!(0.5)),
                purpose: IntentPurpose::Entry,
                created_at: chrono::Utc::now(),
            })
            .expect("submit intent");

        let error = daemon
            .control_deployment(
                "example.paper",
                DeploymentControlRequest {
                    desired_state: None,
                    deployment_state: Some(DeploymentState::Archived),
                },
            )
            .expect_err("active order must block archive");

        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert_eq!(
            daemon
                .inspect_deployment("example.paper")
                .expect("deployment")
                .deployment_state,
            DeploymentState::Enabled
        );
    }

    #[test]
    fn daemon_rejects_cap_reduction_below_account_exposure() {
        let root = temp_dir("cap-below-account-exposure");
        let runtime_root = root.join("run/platform");
        let config = PlatformConfig {
            registry_file: root.join("data/state/deployments.json"),
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            ..PlatformConfig::default()
        };
        let mut daemon = PloyDaemon::boot(&config).expect("boot");

        for (deployment_id, quantity) in [("first.paper", dec!(1)), ("second.paper", dec!(2))] {
            daemon
                .apply_deployment(DeploymentApplyRequest {
                    deployment_id: deployment_id.to_string(),
                    bundle_id: "example".to_string(),
                    runtime_mode: DeploymentRuntimeMode::Paper,
                    account_id: "paper:test-cap-shared".to_string(),
                    max_gross_exposure: Some(dec!(5)),
                    deployment_state: DeploymentState::Enabled,
                    desired_state: DesiredState::Running,
                })
                .expect("apply deployment");
            let intent_id = format!("intent-{deployment_id}");
            daemon
                .submit_paper_intent(TradingIntent {
                    intent_id: intent_id.clone(),
                    deployment_id: deployment_id.to_string(),
                    market_id: "market-1".to_string(),
                    token_id: format!("token-{deployment_id}"),
                    side: TradeSide::Buy,
                    quantity,
                    limit_price: Some(dec!(0.5)),
                    purpose: IntentPurpose::Entry,
                    created_at: chrono::Utc::now(),
                })
                .expect("submit intent");
            daemon.record_fill(
                deployment_id,
                FillRecord {
                    fill_id: format!("fill-{deployment_id}"),
                    order_id: format!("order-{intent_id}"),
                    token_id: format!("token-{deployment_id}"),
                    side: TradeSide::Buy,
                    quantity,
                    price: dec!(0.5),
                    fee: dec!(0),
                    timestamp: chrono::Utc::now(),
                },
            );
        }

        let error = daemon
            .apply_deployment(DeploymentApplyRequest {
                deployment_id: "first.paper".to_string(),
                bundle_id: "example".to_string(),
                runtime_mode: DeploymentRuntimeMode::Paper,
                account_id: "paper:test-cap-shared".to_string(),
                max_gross_exposure: Some(dec!(0.75)),
                deployment_state: DeploymentState::Enabled,
                desired_state: DesiredState::Running,
            })
            .expect_err("account exposure must block cap reduction");

        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert_eq!(
            daemon
                .inspect_deployment("first.paper")
                .expect("deployment")
                .max_gross_exposure,
            Some(dec!(5))
        );
    }

    #[test]
    fn daemon_reconciles_live_fill_into_canonical_ledger() {
        let root = temp_dir("live-fill-reconcile");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.live",
                    "bundle_id": "example",
                    "runtime_mode": "live",
                    "account_id": LIVE_WALLET,
                    "max_gross_exposure": "5",
                    "desired_state": "running",
                    "observed_state": "starting"
                }
            ])
            .to_string(),
        )
        .expect("write registry");

        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            ..PlatformConfig::default()
        };

        let fill = FillRecord {
            fill_id: "fill-live-1".to_string(),
            order_id: "order-intent-live-3".to_string(),
            token_id: "yes-token".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(3),
            price: dec!(0.44),
            fee: dec!(0.02),
            timestamp: chrono::Utc::now(),
        };

        seed_empty_live_ledgers(&config);
        let mut daemon = PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(
                StaticExecutionGateway::acknowledged("venue-live-3")
                    .with_reconciled_fills(vec![fill]),
            ),
        )
        .expect("boot");
        daemon
            .submit_intent(TradingIntent {
                intent_id: "intent-live-3".to_string(),
                deployment_id: "example.live".to_string(),
                market_id: "market-1".to_string(),
                token_id: "yes-token".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(3),
                limit_price: Some(dec!(0.44)),
                purpose: IntentPurpose::Entry,
                created_at: chrono::Utc::now(),
            })
            .expect("submit live intent");

        let reconciled = daemon.reconcile_live_fills().expect("reconcile fills");
        assert_eq!(reconciled, ReconcileStatus::Applied(1));

        let trading_state = daemon.trading_state();
        assert_eq!(trading_state[0].fills.len(), 1);
        assert_eq!(trading_state[0].orders[0].state, "filled");
        assert_eq!(trading_state[0].positions[0].net_qty, dec!(3));
    }

    #[test]
    fn daemon_restores_live_orders_and_reconciles_after_restart() {
        let root = temp_dir("live-restart-recovery");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.live",
                    "bundle_id": "example",
                    "runtime_mode": "live",
                    "account_id": LIVE_WALLET,
                    "max_gross_exposure": "5",
                    "desired_state": "running",
                    "observed_state": "starting"
                }
            ])
            .to_string(),
        )
        .expect("write registry");

        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            ..PlatformConfig::default()
        };

        seed_empty_live_ledgers(&config);
        let mut daemon = PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(StaticExecutionGateway::acknowledged("venue-live-restart-1")),
        )
        .expect("boot");
        daemon
            .submit_intent(TradingIntent {
                intent_id: "intent-live-restart".to_string(),
                deployment_id: "example.live".to_string(),
                market_id: "market-1".to_string(),
                token_id: "yes-token".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(4),
                limit_price: Some(dec!(0.41)),
                purpose: IntentPurpose::Entry,
                created_at: chrono::Utc::now(),
            })
            .expect("submit live intent");
        daemon.write_runtime_snapshots().expect("write snapshots");

        let mut restored = PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(
                StaticExecutionGateway::acknowledged("unused").with_reconciled_fills(vec![
                    FillRecord {
                        fill_id: "fill-live-restart".to_string(),
                        order_id: "order-intent-live-restart".to_string(),
                        token_id: "yes-token".to_string(),
                        side: TradeSide::Buy,
                        quantity: dec!(4),
                        price: dec!(0.41),
                        fee: dec!(0.04),
                        timestamp: chrono::Utc::now(),
                    },
                ]),
            ),
        )
        .expect("restore daemon");

        let restored_state = restored.trading_state();
        assert_eq!(restored_state[0].orders.len(), 1);
        assert_eq!(restored_state[0].orders[0].state, "acknowledged");
        assert_eq!(
            restored_state[0].orders[0].venue_order_id.as_deref(),
            Some("venue-live-restart-1")
        );

        let recorded = restored
            .reconcile_live_fills()
            .expect("reconcile restored fills");
        assert_eq!(recorded, ReconcileStatus::Applied(1));

        let reconciled_state = restored.trading_state();
        assert_eq!(reconciled_state[0].fills.len(), 1);
        assert_eq!(reconciled_state[0].orders[0].state, "filled");
        assert_eq!(reconciled_state[0].positions.len(), 1);
        assert_eq!(reconciled_state[0].positions[0].net_qty, dec!(4));
    }

    #[test]
    fn daemon_reconcile_is_idempotent_for_duplicate_fill_ids() {
        let root = temp_dir("live-fill-idempotent");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.live",
                    "bundle_id": "example",
                    "runtime_mode": "live",
                    "account_id": LIVE_WALLET,
                    "max_gross_exposure": "5",
                    "desired_state": "running",
                    "observed_state": "starting"
                }
            ])
            .to_string(),
        )
        .expect("write registry");

        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            ..PlatformConfig::default()
        };

        let fill = FillRecord {
            fill_id: "fill-live-dup".to_string(),
            order_id: "order-intent-live-4".to_string(),
            token_id: "yes-token".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(1),
            price: dec!(0.41),
            fee: dec!(0.01),
            timestamp: chrono::Utc::now(),
        };

        seed_empty_live_ledgers(&config);
        let mut daemon = PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(
                StaticExecutionGateway::acknowledged("venue-live-4")
                    .with_reconciled_fills(vec![fill]),
            ),
        )
        .expect("boot");
        daemon
            .submit_intent(TradingIntent {
                intent_id: "intent-live-4".to_string(),
                deployment_id: "example.live".to_string(),
                market_id: "market-1".to_string(),
                token_id: "yes-token".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(1),
                limit_price: Some(dec!(0.41)),
                purpose: IntentPurpose::Entry,
                created_at: chrono::Utc::now(),
            })
            .expect("submit live intent");

        assert_eq!(
            daemon.reconcile_live_fills().expect("reconcile fills"),
            ReconcileStatus::Applied(1)
        );
        assert_eq!(
            daemon.reconcile_live_fills().expect("reconcile fills"),
            ReconcileStatus::Noop
        );

        let trading_state = daemon.trading_state();
        assert_eq!(trading_state[0].fills.len(), 1);
        assert_eq!(trading_state[0].orders[0].filled_qty, dec!(1));
    }

    #[test]
    fn daemon_surfaces_transient_reconcile_failures_as_degraded_then_recovering() {
        let root = temp_dir("live-health-recovery");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.live",
                    "bundle_id": "example",
                    "runtime_mode": "live",
                    "account_id": LIVE_WALLET,
                    "max_gross_exposure": "5",
                    "desired_state": "running",
                    "observed_state": "starting"
                }
            ])
            .to_string(),
        )
        .expect("write registry");

        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            live_reconcile_backoff_base_ms: 0,
            live_reconcile_backoff_max_ms: 0,
            ..PlatformConfig::default()
        };

        seed_empty_live_ledgers(&config);
        let mut daemon = PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(FlakyReconcileGateway::default()),
        )
        .expect("boot");
        daemon
            .submit_intent(TradingIntent {
                intent_id: "intent-live-health".to_string(),
                deployment_id: "example.live".to_string(),
                market_id: "market-1".to_string(),
                token_id: "yes-token".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(1),
                limit_price: Some(dec!(0.41)),
                purpose: IntentPurpose::Entry,
                created_at: chrono::Utc::now(),
            })
            .expect("submit live intent");

        daemon.write_runtime_snapshots().expect("degraded snapshot");
        assert!(daemon
            .control_plane
            .system
            .status()
            .status
            .starts_with("degraded@"));
        assert_eq!(daemon.control_plane.system.status().error_count_1h, 1);
        assert_eq!(
            daemon
                .inspect_deployment("example.live")
                .expect("deployment")
                .observed_state,
            ObservedState::Degraded
        );

        daemon
            .write_runtime_snapshots()
            .expect("recovering snapshot");
        assert!(daemon
            .control_plane
            .system
            .status()
            .status
            .starts_with("recovering@"));
        assert_eq!(
            daemon
                .inspect_deployment("example.live")
                .expect("deployment")
                .observed_state,
            ObservedState::Running
        );

        daemon.write_runtime_snapshots().expect("running snapshot");
        assert!(daemon
            .control_plane
            .system
            .status()
            .status
            .starts_with("running@"));
    }

    #[test]
    fn live_reconcile_backoff_doubles_until_maximum() {
        assert_eq!(live_reconcile_backoff_ms(1, 1_000, 30_000), 1_000);
        assert_eq!(live_reconcile_backoff_ms(2, 1_000, 30_000), 2_000);
        assert_eq!(live_reconcile_backoff_ms(3, 1_000, 30_000), 4_000);
        assert_eq!(live_reconcile_backoff_ms(10, 1_000, 30_000), 30_000);
    }

    #[test]
    fn daemon_skips_live_reconcile_while_backoff_is_active() {
        let root = temp_dir("live-health-backoff");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.live",
                    "bundle_id": "example",
                    "runtime_mode": "live",
                    "account_id": LIVE_WALLET,
                    "max_gross_exposure": "5",
                    "desired_state": "running",
                    "observed_state": "starting"
                }
            ])
            .to_string(),
        )
        .expect("write registry");

        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            live_reconcile_backoff_base_ms: 60_000,
            live_reconcile_backoff_max_ms: 60_000,
            ..PlatformConfig::default()
        };

        seed_empty_live_ledgers(&config);
        let mut daemon = PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(FlakyReconcileGateway::default()),
        )
        .expect("boot");
        daemon
            .submit_intent(TradingIntent {
                intent_id: "intent-live-backoff".to_string(),
                deployment_id: "example.live".to_string(),
                market_id: "market-1".to_string(),
                token_id: "yes-token".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(1),
                limit_price: Some(dec!(0.41)),
                purpose: IntentPurpose::Entry,
                created_at: chrono::Utc::now(),
            })
            .expect("submit live intent");

        daemon.write_runtime_snapshots().expect("degraded snapshot");
        let status = daemon.control_plane.system.status();
        assert_eq!(status.live_reconcile_failures, 1);
        assert!(status.next_live_reconcile_at.is_some());
        assert_eq!(
            daemon.reconcile_live_fills().expect("backoff reconcile"),
            ReconcileStatus::BackoffActive
        );
    }

    #[test]
    fn daemon_surfaces_live_source_failures_in_metrics_and_alerts() {
        let root = temp_dir("live-source-alerts");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.live",
                    "bundle_id": "example",
                    "runtime_mode": "live",
                    "account_id": LIVE_WALLET,
                    "max_gross_exposure": "5",
                    "desired_state": "running",
                    "observed_state": "starting"
                }
            ])
            .to_string(),
        )
        .expect("write registry");

        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            live_reconcile_backoff_base_ms: 0,
            live_reconcile_backoff_max_ms: 0,
            ..PlatformConfig::default()
        };

        seed_empty_live_ledgers(&config);
        let mut daemon = PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(FlakyReconcileGateway::default()),
        )
        .expect("boot");
        daemon
            .submit_intent(TradingIntent {
                intent_id: "intent-live-metrics".to_string(),
                deployment_id: "example.live".to_string(),
                market_id: "market-1".to_string(),
                token_id: "yes-token".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(1),
                limit_price: Some(dec!(0.41)),
                purpose: IntentPurpose::Entry,
                created_at: chrono::Utc::now(),
            })
            .expect("submit live intent");

        daemon.write_runtime_snapshots().expect("degraded snapshot");

        let metrics = daemon.platform_metrics();
        assert_eq!(metrics.stale_sources, 2);
        assert_eq!(metrics.active_alerts, 2);
        assert_eq!(metrics.live_reconcile_failures, 1);
        assert!(metrics
            .heartbeats
            .iter()
            .any(|status| status.source_id == "live_reconcile"));
        assert!(metrics
            .heartbeats
            .iter()
            .any(|status| status.source_id == "venue:polymarket"));

        let alerts = daemon.active_alerts();
        assert_eq!(alerts.len(), 2);
        assert!(alerts
            .iter()
            .any(|alert| alert.source_id == "live_reconcile"));
        assert!(alerts
            .iter()
            .any(|alert| alert.source_id == "venue:polymarket"));
        assert!(daemon
            .control_plane
            .system
            .status()
            .status
            .starts_with("degraded@"));
    }
}
