//! Canonical finalized Campaign budget handoff. This path creates no root grant,
//! approval, Kubernetes Job or new budget. Data equivalence gates signed export.
use super::{admission, load_submission, render_controlled_manifest, validate_submission};
use crate::cli::print_json;
use hft_research_dispatch_io::validate_cluster_target;
use alpha_store::campaign_ledger::VerifiedCampaignPlatformBudget;
use anyhow::Context;
use clap::Args;
use serde_json::json;
use std::path::PathBuf;
mod fixed_campaign;
#[cfg(all(test, feature = "scientific"))]
mod fixed_campaign_tests;
mod native_witness;
mod released_build;
mod signed_export;
mod signed_revocations;
mod worker_configuration;
pub use signed_export::PlatformExportArgs;
pub use signed_revocations::PlatformRevocationsArgs;
pub fn export(args: PlatformExportArgs) -> anyhow::Result<()> {
    signed_export::export(args)
}
pub fn export_revocations(args: PlatformRevocationsArgs) -> anyhow::Result<()> {
    signed_revocations::export(args)
}

#[derive(Debug, Clone, Args)]
pub struct PlatformPrepareArgs {
    #[arg(long)]
    pub submission: PathBuf,
    #[arg(long)]
    pub control: PathBuf,
    #[arg(long)]
    pub context: String,
    #[arg(long)]
    pub namespace: String,
}

/// Retains the source admission object so export can reacquire its live guards.
/// No deserializer or public constructor can turn caller JSON into this state.
pub(super) struct PreparedPlatformCampaignBudget {
    admission: admission::Admission,
    validated: super::ValidatedSubmission,
    manifest: serde_json::Value,
}
impl PreparedPlatformCampaignBudget {
    pub(super) fn budget(&mut self) -> anyhow::Result<VerifiedCampaignPlatformBudget> {
        self.admission.platform_budget()
    }
}

pub fn prepare(args: PlatformPrepareArgs) -> anyhow::Result<()> {
    let mut prepared = prepare_budget(&args)?;
    let budget = prepared.budget()?;
    let reservation = budget.reservation();
    print_json(&json!({
        "schema_version":"monday.platform_campaign_budget_inspection.v1",
        "operation_id":reservation.operation_id()?,
        "operation_sha256":budget.operation_sha256()?,
        "native_request_sha256":reservation.request_sha256,
        "root_grant_sha256":budget.root().content_sha256(),
        "root_receipt_sha256":budget.root_receipt().content_sha256,
        "reservation_receipt_sha256":budget.reservation_receipt().content_sha256,
        "approval_sha256":budget.approval_sha256(),
        "reserved_trials":reservation.declared_trials,
        "reserved_job_seconds":reservation.reserved_job_seconds,
        "reserved_llm_tokens":reservation.reserved_llm_tokens,
        "authority_expires_at":budget.authority_expires_at(),
        "execution":reservation.execution,
        "existing_platform_transfer":budget.existing_transfer(),
        "signed_export_stage":"not_performed_by_prepare",
        "data_equivalence_required":true
    }))
}

fn prepare_budget(args: &PlatformPrepareArgs) -> anyhow::Result<PreparedPlatformCampaignBudget> {
    prepare_budget_with(args, |source| source.publish_receipts())
}

fn prepare_budget_with(
    args: &PlatformPrepareArgs,
    publish: impl FnOnce(&mut admission::Admission) -> anyhow::Result<()>,
) -> anyhow::Result<PreparedPlatformCampaignBudget> {
    validate_cluster_target(&args.context, &args.namespace)?;
    anyhow::ensure!(
        !super::sequence_admission::is_study_submission(&args.submission)?
            && !super::final_admission::is_final_submission(&args.submission)?,
        "platform budget preparation currently accepts canonical pre-holdout Campaign requests"
    );
    // Reuse actual finalization and Job inspection. The caller never supplies
    // a fabricated reservation, trial count, Root, resources or request hash.
    let validated = validate_submission(load_submission(&args.submission)?)?;
    let manifest = render_controlled_manifest(
        &validated,
        &args.namespace,
        &admission::read_control(&args.control)?,
    )?;
    let mut source = admission::Admission::open(
        &args.control,
        &validated,
        &manifest,
        &args.context,
        &args.namespace,
    )?;
    source
        .prepare()
        .context("canonical native reservation failed")?;
    publish(&mut source)
        .context("native charge retained; source receipt publication/readback failed")?;
    Ok(PreparedPlatformCampaignBudget {
        admission: source,
        validated,
        manifest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_or_untrusted_control_cannot_prepare_an_operation() {
        let args = PlatformPrepareArgs {
            submission: "/nonexistent/finalized.json".into(),
            control: "/nonexistent/control.json".into(),
            context: "research-context".into(),
            namespace: "monday-research".into(),
        };
        let mut called = false;
        assert!(prepare_budget_with(&args, |_| {
            called = true;
            Ok(())
        })
        .is_err());
        assert!(!called);
        let mut wrong = args;
        wrong.namespace = "default".into();
        assert!(prepare_budget_with(&wrong, |_| {
            called = true;
            Ok(())
        })
        .is_err());
        assert!(!called);
    }
}
