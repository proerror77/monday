//! Independent terminal readback and accounting for the same dispatched Job.

use super::{
    admission::Admission, load_submission, render_manifest, validate_job_readback,
    validate_submission,
};
use crate::{
    cli::{print_json, MissionDispatchSubmitArgs},
    mission_campaign::readback_pre_holdout_terminal,
    prediction_dispatch::{kubectl_binary, kubectl_json, validate_cluster_target},
};
use alpha_domain::campaign_control::CampaignAttemptSettlementV1;
use alpha_store::campaign_ledger::CampaignDispatchSettlementV1;
use anyhow::{bail, Context};
use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::time::Duration;

pub(super) fn settle(args: MissionDispatchSubmitArgs) -> anyhow::Result<()> {
    validate_cluster_target(&args.context, &args.namespace)?;
    let validated = validate_submission(load_submission(&args.submission)?)?;
    let manifest = render_manifest(&validated, &args.namespace)?;
    let control = args
        .control
        .or_else(|| std::env::var_os("MONDAY_CAMPAIGN_CONTROL").map(Into::into))
        .context("Campaign settlement requires --control or MONDAY_CAMPAIGN_CONTROL")?;
    let mut admission = Admission::open_for_settlement(
        &control,
        &validated,
        &manifest,
        &args.context,
        &args.namespace,
    )?;
    let record = admission.record()?;
    if record.settlement.is_none() {
        let kubectl = kubectl_binary();
        let job = kubectl_json(
            &kubectl,
            &args.context,
            &args.namespace,
            [
                "--request-timeout=30s",
                "get",
                "job",
                &validated.job_name,
                "-o",
                "json",
            ],
            "read back terminal Campaign Job",
        )?;
        let selector = format!("job-name={}", validated.job_name);
        let pods = kubectl_json(
            &kubectl,
            &args.context,
            &args.namespace,
            [
                "--request-timeout=30s",
                "get",
                "pods",
                "-l",
                &selector,
                "-o",
                "json",
            ],
            "read back terminal Campaign Pod",
        )?;
        let items = pods["items"]
            .as_array()
            .context("terminal Pod list is missing items")?;
        if items.len() != 1 {
            bail!("Campaign settlement requires exactly one execution Pod; keep uncertain attempts charged");
        }
        let bound_uid = record
            .claim
            .job_uid
            .as_deref()
            .context("Campaign Job has no durable UID binding")?;
        let (job_uid, pod_uid) =
            validate_terminal_provenance(&manifest["items"][1], &job, &items[0], bound_uid)?;
        let client = Client::builder()
            .timeout(Duration::from_secs(120))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let (outcome, consumed_trials, evidence_sha256) = readback_pre_holdout_terminal(
            &client,
            &validated.submission.request,
            &validated.request_sha256,
            &admission.reservation.execution.evaluation_protocol_sha256,
        )?;
        let evidence = CampaignDispatchSettlementV1 {
            job_uid,
            pod_uid,
            settlement: CampaignAttemptSettlementV1 {
                operation_id: admission.reservation.operation_id()?,
                reservation_sha256: admission.reservation.content_hash()?,
                evidence_sha256,
                outcome,
                consumed_trials: Some(consumed_trials),
            },
        };
        admission.settle(&evidence)?;
    }
    let record = admission.record()?;
    if record.terminal_pod_uid.is_none() {
        bail!("existing settlement lacks independent dispatch terminal provenance");
    }
    let settlement = record
        .settlement
        .context("dispatch settlement was not persisted")?;
    // If publication previously lost its response, reuse the durable event's
    // exact bytes. Do not require a TTL-deleted Job to be present a second time.
    admission.publish_receipts()?;
    let report = json!({
        "status": "settled", "operation_id": settlement.operation_id,
        "campaign_id": admission.reservation.campaign_id,
        "request_sha256": admission.reservation.request_sha256,
        "job_name": validated.job_name, "job_uid": record.claim.job_uid,
        "pod_uid": record.terminal_pod_uid,
        "campaign_result_sha256": settlement.evidence_sha256,
        "outcome": settlement.outcome, "consumed_trials": settlement.consumed_trials,
    });
    crate::mission_runner::research_event(
        "alpha-harness",
        "campaign_dispatch_settled",
        report.clone(),
    );
    print_json(&report)
}

pub(super) fn validate_terminal_provenance(
    expected_job: &Value,
    job: &Value,
    pod: &Value,
    bound_job_uid: &str,
) -> anyhow::Result<(String, String)> {
    let job_name = expected_job["metadata"]["name"]
        .as_str()
        .context("missing Job name")?;
    let request_sha256 = expected_job["metadata"]["annotations"]["research.monday/request-sha256"]
        .as_str()
        .context("missing Job request digest")?;
    validate_job_readback(job, expected_job, job_name, request_sha256, false)?;
    let condition = |kind: &str| {
        job["status"]["conditions"]
            .as_array()
            .is_some_and(|conditions| {
                conditions
                    .iter()
                    .any(|c| c["type"] == kind && c["status"] == "True")
            })
    };
    if job["metadata"]["uid"] != bound_job_uid
        || !condition("Complete")
        || condition("Failed")
        || job["status"]["active"].as_u64().unwrap_or(0) != 0
        || job["status"]["failed"].as_u64().unwrap_or(0) != 0
        || job["status"]["succeeded"].as_u64() != Some(1)
    {
        bail!("Campaign Job is not the bound successful terminal attempt");
    }
    let uid = pod["metadata"]["uid"]
        .as_str()
        .context("terminal Pod UID is missing")?;
    if uid.trim().is_empty() || uid.chars().any(char::is_control) {
        bail!("terminal Pod UID is invalid");
    }
    let expected_container = &expected_job["spec"]["template"]["spec"]["containers"][0];
    let containers = pod["spec"]["containers"]
        .as_array()
        .context("terminal Pod containers are missing")?;
    let statuses = pod["status"]["containerStatuses"]
        .as_array()
        .context("terminal Pod status is missing")?;
    if containers.len() != 1 || statuses.len() != 1 {
        bail!("terminal Pod must contain only the Campaign executor");
    }
    let owned = pod["metadata"]["ownerReferences"]
        .as_array()
        .is_some_and(|owners| {
            owners.iter().any(|owner| {
                owner["kind"] == "Job" && owner["name"] == job_name && owner["uid"] == bound_job_uid
            })
        });
    let status = &statuses[0];
    let image_id = status["imageID"]
        .as_str()
        .context("terminal Pod imageID is missing")?;
    let digest = image_id.rsplit_once("sha256:").map(|(_, value)| value);
    let expected_digest =
        expected_job["metadata"]["annotations"]["research.monday/image-digest"].as_str();
    if !owned
        || pod["status"]["phase"] != "Succeeded"
        || pod["metadata"]["annotations"]["research.monday/request-sha256"] != request_sha256
        || pod["spec"]["automountServiceAccountToken"] != false
        || status["name"] != "alpha-campaign"
        || status["state"]["terminated"]["exitCode"] != 0
        || status["restartCount"].as_u64() != Some(0)
        || digest != expected_digest
    {
        bail!("terminal Pod does not bind the request, Job UID and pinned executor image");
    }
    for field in [
        "name",
        "image",
        "command",
        "args",
        "resources",
        "env",
        "envFrom",
    ] {
        if containers[0][field] != expected_container[field] {
            bail!("terminal Pod execution field {field} differs from the admitted Job");
        }
    }
    Ok((bound_job_uid.into(), uid.into()))
}
