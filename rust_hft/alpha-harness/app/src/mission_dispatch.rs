mod admission;
pub(crate) mod controller;
pub(crate) mod final_admission;
pub(crate) mod final_authority;
mod terminal;

use crate::{
    cli::{print_json, MissionDispatchInspectArgs, MissionDispatchSubmitArgs},
    data_mission,
    mission_campaign::{serialize_request, validate_request, CampaignRequest},
    prediction_dispatch::{
        ensure_kubectl_success, kubectl_binary, kubectl_json, kubectl_with_input,
        validate_cluster_target, validate_dns_label,
    },
};
use alpha_domain::{
    campaign_control::{verify_campaign_root_grant, SignedCampaignRootGrantV1},
    campaign_horizon::CampaignNextFamilyParentV1,
};
use alpha_store::campaign_ledger::{
    CampaignLedgerEventV1, CampaignStudyLedgerEventV1, CampaignStudySnapshotV1,
};
use alpha_store::AlphaStore;
use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;

const MAX_SUBMISSION_BYTES: u64 = 1024 * 1024;
const ACTIVE_DEADLINE_SECONDS: u64 = 21_608;

#[derive(Debug)]
pub(crate) struct AuthenticatedCampaignParent {
    pub(crate) parent: CampaignNextFamilyParentV1,
    pub(crate) study_grant: alpha_domain::campaign_study::SignedCampaignStudyGrantV1,
    pub(crate) request: CampaignRequest,
}

#[derive(Debug, Deserialize)]
struct SettlementReadback {
    status: String,
    operation_id: String,
    request_sha256: String,
    campaign_result_sha256: String,
    job_uid: Option<String>,
    pod_uid: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct MissionDispatchSubmission {
    attempt_id: String,
    image: String,
    request: CampaignRequest,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SubmissionRenderReport {
    pub(crate) request_sha256: String,
    pub(crate) submission_identity_sha256: String,
    pub(crate) job_name: String,
    pub(crate) secret_name: String,
}

#[derive(Debug)]
struct ValidatedSubmission {
    submission: MissionDispatchSubmission,
    image_digest: String,
    request_sha256: String,
    request_json: String,
    submission_identity_sha256: String,
    job_name: String,
    secret_name: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SubmissionObjectState {
    #[default]
    Unknown,
    Created,
    Adopted,
}

pub fn inspect(args: MissionDispatchInspectArgs) -> anyhow::Result<()> {
    if final_admission::is_final_submission(&args.submission)? {
        return final_admission::inspect(args);
    }
    let validated = validate_submission(load_submission(&args.submission)?)?;
    let manifest = render_manifest(&validated, "monday-research")?;
    print_json(&admission::inspect_binding(
        &validated,
        &manifest,
        &args.materialization,
        &args.controller_image,
        args.attempt_ordinal,
    )?)
}

pub fn settle(args: MissionDispatchSubmitArgs) -> anyhow::Result<()> {
    if final_admission::is_final_submission(&args.submission)? {
        return final_admission::settle(args);
    }
    terminal::settle(args)
}

pub fn submit(args: MissionDispatchSubmitArgs) -> anyhow::Result<()> {
    if final_admission::is_final_submission(&args.submission)? {
        return final_admission::submit(args);
    }
    validate_cluster_target(&args.context, &args.namespace)?;
    let submission = load_submission(&args.submission)?;
    let validated = validate_submission(submission)?;
    let job_name = validated.job_name.clone();
    let secret_name = validated.secret_name.clone();
    let campaign_id = validated.submission.request.campaign_id.clone();
    let request_sha256 = validated.request_sha256.clone();
    let request_json = validated.request_json.clone();
    let manifest = render_manifest(&validated, &args.namespace)?;
    let control = args
        .control
        .clone()
        .or_else(|| std::env::var_os("MONDAY_CAMPAIGN_CONTROL").map(Into::into))
        .context("Campaign dispatch requires --control or MONDAY_CAMPAIGN_CONTROL")?;
    let mut admission = admission::Admission::open(
        &control,
        &validated,
        &manifest,
        &args.context,
        &args.namespace,
    )?;
    submit_rendered_job(
        &args,
        RenderedDispatch {
            job_name: job_name.clone(),
            secret_name,
            request_sha256: request_sha256.clone(),
            request_json,
            manifest,
        },
        &mut admission,
    )?;

    let report = json!({
        "status": "submitted",
        "context": args.context,
        "namespace": args.namespace,
        "campaign_id": campaign_id,
        "request_sha256": request_sha256,
        "job_name": job_name,
        "operation_id": admission.reservation.operation_id()?,
        "reserved_trials": admission.reservation.declared_trials,
        "execution_scope": "pre_holdout",
    });
    crate::mission_runner::research_event(
        "alpha-harness",
        "campaign_dispatch_submitted",
        report.clone(),
    );
    print_json(&report)
}

/// Read back the parent terminal through the authenticated dispatch ledger.
/// Caller-provided hashes are only cross-checks; the family and Study receipts
/// are the source of truth for a next-family proposal.
pub(crate) fn read_authenticated_campaign_parent(
    control_path: &Path,
    submission_path: &Path,
    result_path: &Path,
    settlement_path: &Path,
    study_id: &str,
    namespace: &str,
) -> anyhow::Result<AuthenticatedCampaignParent> {
    let control = admission::read_control(control_path)?;
    let submission = load_submission(submission_path)?;
    let validated = validate_submission(submission)?;
    let manifest = render_manifest(&validated, namespace)?;
    let inspection = admission::inspect_binding(
        &validated,
        &manifest,
        &control.materialization_path,
        &control.controller_image,
        control.attempt_ordinal,
    )?;
    let signed: SignedCampaignRootGrantV1 = admission::read_json(&control.signed_root_grant_path)?;
    // The parent is historical evidence.  Authenticate its signature at a
    // point inside the grant's own validity window, while the ledger record
    // below supplies the actual settled identity.  Current-time admission is
    // reserved for the target dispatch path.
    verify_campaign_root_grant(
        &signed,
        &admission::read_trusted_keys(&control.trusted_keys_path)?,
        signed.grant.expires_at - chrono::TimeDelta::seconds(1),
    )?;
    let store = AlphaStore::open(&control.ledger_path)?;
    let reservation = inspection
        .historical_reservation_for(&signed.content_sha256, &signed.grant.family.family_id);
    let operation_id = reservation.operation_id()?;
    let record = store.campaign_dispatch_record(&reservation.family_id, &operation_id)?;
    if record.root.signed_grant() != &signed || record.reservation != reservation {
        bail!("parent dispatch ledger differs from the finalized submission");
    }
    let settlement = record
        .settlement
        .clone()
        .context("parent dispatch has no durable settlement")?;
    let terminal_job_uid = record
        .claim
        .job_uid
        .clone()
        .context("parent dispatch has no durable Job UID")?;
    let terminal_pod_uid = record
        .terminal_pod_uid
        .clone()
        .context("parent dispatch has no independently read-back terminal Pod UID")?;
    let result_sha256 = hex::encode(Sha256::digest(admission::read_bounded(
        result_path,
        MAX_SUBMISSION_BYTES,
    )?));
    if result_sha256 != settlement.evidence_sha256 {
        bail!("parent terminal result differs from the settled ledger evidence");
    }
    let readback: SettlementReadback = admission::read_json(settlement_path)?;
    if readback.status != "settled"
        || readback.operation_id != operation_id
        || readback.request_sha256 != reservation.request_sha256
        || readback.campaign_result_sha256 != result_sha256
        || readback.job_uid.as_deref() != Some(terminal_job_uid.as_str())
        || readback.pod_uid.as_deref() != Some(terminal_pod_uid.as_str())
    {
        bail!("parent settlement readback differs from the authenticated ledger");
    }
    let family_receipt_sha256 = store
        .campaign_family_receipts(&reservation.family_id)?
        .into_iter()
        .find_map(|receipt| match &receipt.receipt.event {
            CampaignLedgerEventV1::DispatchSettled { evidence }
                if evidence.settlement == settlement =>
            {
                Some(receipt.content_sha256)
            }
            CampaignLedgerEventV1::AttemptSettled {
                settlement: observed,
            } if observed == &settlement => Some(receipt.content_sha256),
            _ => None,
        })
        .context("parent settlement is missing from the authenticated family ledger")?;
    let study_snapshot: CampaignStudySnapshotV1 = store.campaign_study_snapshot(study_id)?;
    let study_grant = store
        .campaign_study_grant(study_id)?
        .context("parent family has no authenticated Study grant")?;
    let member = study_grant
        .grant
        .members
        .iter()
        .find(|member| {
            member.family_id == reservation.family_id
                && member.root_grant_sha256 == reservation.root_grant_sha256
        })
        .context("parent family is not a finite member of the authenticated Study")?;
    let study_receipt_sha256 = study_snapshot
        .receipts
        .iter()
        .find_map(|receipt| match &receipt.receipt.event {
            CampaignStudyLedgerEventV1::AttemptSettled {
                family_id,
                settlement: observed,
                family_receipt_sha256: linked_family_receipt_sha256,
            } if family_id == &reservation.family_id
                && observed == &settlement
                && linked_family_receipt_sha256 == &family_receipt_sha256 =>
            {
                Some(receipt.content_sha256.clone())
            }
            _ => None,
        })
        .context("parent settlement is missing from the authenticated Study ledger")?;
    if member.execution != reservation.execution {
        bail!("parent root execution input differs from its finite Study member");
    }
    let parent = CampaignNextFamilyParentV1 {
        campaign_id: reservation.campaign_id,
        family_id: reservation.family_id,
        root_grant_sha256: reservation.root_grant_sha256,
        request_sha256: reservation.request_sha256,
        campaign_result_sha256: result_sha256,
        family_settlement_receipt_sha256: family_receipt_sha256,
        study_settlement_receipt_sha256: study_receipt_sha256.clone(),
        study_snapshot_sha256: admission::study_prefix_identity(
            &study_snapshot,
            &study_receipt_sha256,
        )?,
        terminal_job_uid,
        terminal_pod_uid,
    };
    Ok(AuthenticatedCampaignParent {
        parent,
        study_grant,
        request: validated.submission.request,
    })
}

pub(super) trait DispatchAdmission {
    fn prepare(&mut self) -> anyhow::Result<()>;
    fn publish_receipts(&mut self) -> anyhow::Result<()>;
    fn claim(
        &mut self,
    ) -> anyhow::Result<(alpha_store::campaign_ledger::CampaignDispatchClaimV1, bool)>;
    fn bind_job(&mut self, uid: &str) -> anyhow::Result<()>;
    fn guarded<T>(
        &mut self,
        uid: Option<&str>,
        action: impl FnOnce() -> anyhow::Result<T>,
    ) -> anyhow::Result<T>;
}

impl DispatchAdmission for admission::Admission {
    fn prepare(&mut self) -> anyhow::Result<()> {
        admission::Admission::prepare(self)
    }
    fn publish_receipts(&mut self) -> anyhow::Result<()> {
        admission::Admission::publish_receipts(self)
    }
    fn claim(
        &mut self,
    ) -> anyhow::Result<(alpha_store::campaign_ledger::CampaignDispatchClaimV1, bool)> {
        admission::Admission::claim(self)
    }
    fn bind_job(&mut self, uid: &str) -> anyhow::Result<()> {
        admission::Admission::bind_job(self, uid)
    }
    fn guarded<T>(
        &mut self,
        uid: Option<&str>,
        action: impl FnOnce() -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        admission::Admission::guarded(self, uid, action)
    }
}

pub(super) struct RenderedDispatch {
    pub job_name: String,
    pub secret_name: String,
    pub request_sha256: String,
    pub request_json: String,
    pub manifest: Value,
}

fn submit_rendered_job<A: DispatchAdmission>(
    args: &MissionDispatchSubmitArgs,
    rendered: RenderedDispatch,
    admission: &mut A,
) -> anyhow::Result<()> {
    let RenderedDispatch {
        job_name,
        secret_name,
        request_sha256,
        request_json,
        manifest,
    } = rendered;
    admission.prepare()?;
    admission.publish_receipts()?;
    let (claim, first_create) = admission.claim()?;
    admission.publish_receipts()?;
    let kubectl = kubectl_binary();
    let mut job_state = SubmissionObjectState::Unknown;
    let mut secret_state = SubmissionObjectState::Unknown;

    let result = (|| -> anyhow::Result<()> {
        let expected_job = &manifest["items"][1];
        let job = admission.guarded(claim.job_uid.as_deref(), || {
            resolve_dispatch_job(
                &claim,
                first_create,
                expected_job,
                &request_sha256,
                || {
                    create_or_adopt_job(
                        &kubectl,
                        &args.context,
                        &args.namespace,
                        expected_job,
                        &job_name,
                        &request_sha256,
                        &mut job_state,
                    )
                },
                || {
                    kubectl_json(
                        &kubectl,
                        &args.context,
                        &args.namespace,
                        [
                            "--request-timeout=30s",
                            "get",
                            "job",
                            &job_name,
                            "-o",
                            "json",
                        ],
                        "read back claimed Campaign Job",
                    )
                },
            )
        })?;
        let job_uid = job["metadata"]["uid"]
            .as_str()
            .context("CEX Campaign Job readback is missing its UID")?;
        admission.bind_job(job_uid)?;
        admission.publish_receipts()?;
        let secret = secret_with_owner(&manifest["items"][0], &job_name, job_uid)?;
        let secret = admission.guarded(Some(job_uid), || {
            create_or_adopt_secret(
                &kubectl,
                &args.context,
                &args.namespace,
                &secret,
                &secret_name,
                &request_json,
                &job_name,
                job_uid,
                &mut secret_state,
            )
        })?;
        validate_secret_readback(
            &secret,
            &secret_name,
            &request_sha256,
            request_json.as_bytes(),
            &job_name,
            job_uid,
        )?;
        validate_complete_secret_payload(&secret, &manifest["items"][0])?;
        let release_patch_json = serde_json::to_string(&release_job_patch(&job)?)?;
        let release_output = admission.guarded(Some(job_uid), || {
            kubectl_with_input(
                &kubectl,
                &args.context,
                &args.namespace,
                [
                    "--request-timeout=30s",
                    "patch",
                    "job",
                    &job_name,
                    "--type=json",
                    "--patch",
                    &release_patch_json,
                    "-o",
                    "json",
                ],
                &[],
            )
        })?;
        let released_job = serde_json::from_slice(&ensure_kubectl_success(
            release_output,
            "release CEX Campaign Job after identity verification",
        )?)
        .context("parse kubectl output for release CEX Campaign Job after identity verification")?;
        validate_job_readback(
            &released_job,
            expected_job,
            &job_name,
            &request_sha256,
            false,
        )?;
        if released_job["metadata"]["uid"] != job_uid {
            bail!("released CEX Campaign Job readback does not match the created Job UID");
        }
        Ok(())
    })();
    if let Err(error) = result {
        return Err(error.context("Campaign dispatch claim and budget retained for reconciliation; no automatic recreation or refund"));
    }

    Ok(())
}

fn resolve_dispatch_job(
    claim: &alpha_store::campaign_ledger::CampaignDispatchClaimV1,
    first_create: bool,
    expected_job: &Value,
    request_sha256: &str,
    create: impl FnOnce() -> anyhow::Result<Value>,
    get: impl FnOnce() -> anyhow::Result<Value>,
) -> anyhow::Result<Value> {
    let job = if first_create {
        create()?
    } else {
        get().context(
            "claimed Campaign Job is unavailable; retain the reservation, never recreate",
        )?
    };
    let mut observed_state = SubmissionObjectState::Unknown;
    adopt_existing_job(
        &job,
        expected_job,
        &claim.target.job_name,
        request_sha256,
        &mut observed_state,
    )?;
    if let Some(uid) = &claim.job_uid {
        if job["metadata"]["uid"] != *uid {
            bail!("claimed Campaign Job UID changed");
        }
    } else if job["spec"]["suspend"] != true {
        bail!("unbound Campaign Job is already running");
    }
    Ok(job)
}

fn create_or_adopt_job(
    kubectl: &std::path::Path,
    context: &str,
    namespace: &str,
    expected_job: &Value,
    job_name: &str,
    request_sha256: &str,
    job_state: &mut SubmissionObjectState,
) -> anyhow::Result<Value> {
    let job_body = serde_json::to_vec(expected_job)?;
    match kubectl_with_input(
        kubectl,
        context,
        namespace,
        ["--request-timeout=30s", "create", "-f", "-"],
        &job_body,
    )
    .and_then(|output| {
        ensure_kubectl_success(output, "create immutable CEX Campaign Job").map(|_| ())
    }) {
        Ok(()) => {
            *job_state = SubmissionObjectState::Created;
            read_back_job(
                kubectl,
                context,
                namespace,
                expected_job,
                job_name,
                request_sha256,
            )
        }
        Err(error) if is_job_create_conflict(&error) => {
            let job = kubectl_json(
                kubectl,
                context,
                namespace,
                [
                    "--request-timeout=30s",
                    "get",
                    "job",
                    job_name,
                    "-o",
                    "json",
                ],
                "read back immutable CEX Campaign Job",
            )
            .context("read back conflicting immutable CEX Campaign Job")?;
            adopt_existing_job(&job, expected_job, job_name, request_sha256, job_state).context(
                "refuse to adopt non-matching suspended CEX Campaign Job after create conflict",
            )?;
            Ok(job)
        }
        Err(error) => Err(error),
    }
}

#[allow(clippy::too_many_arguments)]
fn create_or_adopt_secret(
    kubectl: &std::path::Path,
    context: &str,
    namespace: &str,
    expected_secret: &Value,
    secret_name: &str,
    request_json: &str,
    job_name: &str,
    job_uid: &str,
    secret_state: &mut SubmissionObjectState,
) -> anyhow::Result<Value> {
    let secret_body = serde_json::to_vec(expected_secret)?;
    match kubectl_with_input(
        kubectl,
        context,
        namespace,
        ["--request-timeout=30s", "create", "-f", "-"],
        &secret_body,
    )
    .and_then(|output| {
        ensure_kubectl_success(output, "create immutable CEX Campaign input Secret").map(|_| ())
    }) {
        Ok(()) => {
            *secret_state = SubmissionObjectState::Created;
            read_back_secret(
                kubectl,
                context,
                namespace,
                secret_name,
                request_json,
                job_name,
                job_uid,
            )
        }
        Err(error) if is_job_create_conflict(&error) => {
            let secret = read_back_secret(
                kubectl,
                context,
                namespace,
                secret_name,
                request_json,
                job_name,
                job_uid,
            )
            .context(
                "refuse to adopt non-matching CEX Campaign input Secret after create conflict",
            )?;
            *secret_state = SubmissionObjectState::Adopted;
            Ok(secret)
        }
        Err(error) => Err(error),
    }
}

fn adopt_existing_job(
    job: &Value,
    expected_job: &Value,
    job_name: &str,
    request_sha256: &str,
    job_state: &mut SubmissionObjectState,
) -> anyhow::Result<()> {
    if validate_job_readback(job, expected_job, job_name, request_sha256, true).is_err() {
        validate_job_readback(job, expected_job, job_name, request_sha256, false)?;
    }
    *job_state = SubmissionObjectState::Adopted;
    Ok(())
}

fn read_back_secret(
    kubectl: &std::path::Path,
    context: &str,
    namespace: &str,
    secret_name: &str,
    request_json: &str,
    job_name: &str,
    job_uid: &str,
) -> anyhow::Result<Value> {
    let secret = kubectl_json(
        kubectl,
        context,
        namespace,
        [
            "--request-timeout=30s",
            "get",
            "secret",
            secret_name,
            "-o",
            "json",
        ],
        "read back immutable CEX Campaign input Secret",
    )?;
    let request_sha256 = hex::encode(Sha256::digest(request_json.as_bytes()));
    validate_secret_readback(
        &secret,
        secret_name,
        &request_sha256,
        request_json.as_bytes(),
        job_name,
        job_uid,
    )?;
    Ok(secret)
}

fn read_back_job(
    kubectl: &std::path::Path,
    context: &str,
    namespace: &str,
    expected_job: &Value,
    job_name: &str,
    request_sha256: &str,
) -> anyhow::Result<Value> {
    let job = kubectl_json(
        kubectl,
        context,
        namespace,
        [
            "--request-timeout=30s",
            "get",
            "job",
            job_name,
            "-o",
            "json",
        ],
        "read back immutable CEX Campaign Job",
    )?;
    validate_job_readback(&job, expected_job, job_name, request_sha256, true)?;
    Ok(job)
}

fn is_job_create_conflict(error: &anyhow::Error) -> bool {
    format!("{error:#}").contains("AlreadyExists")
}

fn job_owner_reference(job_name: &str, job_uid: &str) -> anyhow::Result<Value> {
    if job_uid.trim().is_empty() || job_uid.chars().any(char::is_control) {
        bail!("CEX Campaign Job UID is invalid");
    }
    Ok(json!([{
        "apiVersion": "batch/v1",
        "kind": "Job",
        "name": job_name,
        "uid": job_uid,
        "controller": false,
        "blockOwnerDeletion": false,
    }]))
}

fn secret_with_owner(secret: &Value, job_name: &str, job_uid: &str) -> anyhow::Result<Value> {
    let mut owned = secret.clone();
    owned["metadata"]["ownerReferences"] = job_owner_reference(job_name, job_uid)?;
    Ok(owned)
}

fn release_job_patch(job: &Value) -> anyhow::Result<Value> {
    let uid = job["metadata"]["uid"]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .context("CEX Campaign Job release requires its verified UID")?;
    let version = job["metadata"]["resourceVersion"]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .context("CEX Campaign Job release requires its verified resourceVersion")?;
    // Preconditions and release are one API mutation. A post-release UID check
    // alone is too late if the name has been reused since identity readback.
    // Treat resourceVersion as opaque and preserve its original bytes.
    Ok(json!([
        { "op": "test", "path": "/metadata/uid", "value": uid },
        { "op": "test", "path": "/metadata/resourceVersion", "value": version },
        { "op": "replace", "path": "/spec/suspend", "value": false }
    ]))
}

fn validate_job_readback(
    job: &Value,
    expected_job: &Value,
    job_name: &str,
    request_sha256: &str,
    suspended: bool,
) -> anyhow::Result<()> {
    if job["metadata"]["name"] != job_name
        || job["metadata"]["annotations"]["research.monday/request-sha256"] != request_sha256
        || job["spec"]["suspend"] != suspended
        || job_execution_projection(job) != job_execution_projection(expected_job)
    {
        bail!("CEX Campaign Job readback does not match the submitted identity");
    }
    Ok(())
}

fn job_execution_projection(job: &Value) -> Value {
    let pod = &job["spec"]["template"]["spec"];
    let containers = pod["containers"].as_array();
    let container = containers
        .and_then(|values| values.first())
        .unwrap_or(&Value::Null);
    let volumes = pod["volumes"].as_array();
    let volume_projection = volumes
        .into_iter()
        .flatten()
        .map(|volume| {
            json!({
                "name": volume["name"].clone(),
                "emptyDir": volume["emptyDir"].clone(),
                "secretName": volume["secret"]["secretName"].clone(),
                "secretItems": volume["secret"]["items"].clone(),
            })
        })
        .collect::<Vec<_>>();
    json!({
        "spec": {
            "parallelism": job["spec"]["parallelism"].clone(),
            "completions": job["spec"]["completions"].clone(),
            "backoffLimit": job["spec"]["backoffLimit"].clone(),
            "activeDeadlineSeconds": job["spec"]["activeDeadlineSeconds"].clone(),
            "ttlSecondsAfterFinished": job["spec"]["ttlSecondsAfterFinished"].clone(),
            "template": {
                "spec": {
                    "restartPolicy": pod["restartPolicy"].clone(),
                    "automountServiceAccountToken": pod["automountServiceAccountToken"].clone(),
                    "serviceAccountName": pod["serviceAccountName"].as_str().unwrap_or("default"),
                    "hostNetwork": pod["hostNetwork"].as_bool().unwrap_or(false),
                    "hostPID": pod["hostPID"].as_bool().unwrap_or(false),
                    "hostIPC": pod["hostIPC"].as_bool().unwrap_or(false),
                    "shareProcessNamespace": pod["shareProcessNamespace"].as_bool().unwrap_or(false),
                    "nodeName": pod["nodeName"].as_str().unwrap_or(""),
                    "imagePullSecrets": pod["imagePullSecrets"].clone(),
                    "nodeSelector": pod["nodeSelector"].clone(),
                    "securityContext": pod["securityContext"].clone(),
                    "initContainers": pod["initContainers"].clone(),
                    "containerCount": containers.map_or(0, Vec::len),
                    "container": {
                        "name": container["name"].clone(),
                        "image": container["image"].clone(),
                        "imagePullPolicy": container["imagePullPolicy"].clone(),
                        "command": container["command"].clone(),
                        "args": container["args"].clone(),
                        "resources": container["resources"].clone(),
                        "securityContext": container["securityContext"].clone(),
                        "volumeMounts": container["volumeMounts"].clone(),
                        "env": container["env"].clone(),
                        "envFrom": container["envFrom"].clone(),
                    },
                    "volumeCount": volumes.map_or(0, Vec::len),
                    "volumes": volume_projection,
                }
            }
        }
    })
}

fn validate_secret_readback(
    secret: &Value,
    secret_name: &str,
    request_sha256: &str,
    expected_request: &[u8],
    job_name: &str,
    job_uid: &str,
) -> anyhow::Result<()> {
    let owner = &secret["metadata"]["ownerReferences"][0];
    let encoded_request = secret["data"]["campaign.json"]
        .as_str()
        .context("CEX Campaign input Secret readback is missing campaign.json data")?;
    let decoded_request = decode_base64(encoded_request)?;
    if secret["metadata"]["name"] != secret_name
        || secret["immutable"] != true
        || secret["metadata"]["annotations"]["research.monday/request-sha256"] != request_sha256
        || decoded_request != expected_request
        || hex::encode(Sha256::digest(&decoded_request)) != request_sha256
        || owner["apiVersion"] != "batch/v1"
        || owner["kind"] != "Job"
        || owner["name"] != job_name
        || owner["uid"] != job_uid
    {
        bail!("CEX Campaign input Secret readback does not match the submitted identity");
    }
    Ok(())
}

fn validate_complete_secret_payload(observed: &Value, expected: &Value) -> anyhow::Result<()> {
    let expected = expected["stringData"]
        .as_object()
        .context("expected input Secret lacks stringData")?;
    let observed = observed["data"]
        .as_object()
        .context("input Secret lacks data")?;
    if expected.len() != observed.len() {
        bail!("input Secret has unexpected payload keys");
    }
    for (name, value) in expected {
        let bytes = value
            .as_str()
            .context("expected Secret input is not a string")?
            .as_bytes();
        let readback = observed
            .get(name)
            .and_then(Value::as_str)
            .context("input Secret key missing")?;
        if decode_base64(readback)? != bytes {
            bail!("input Secret payload differs for {name}");
        }
    }
    Ok(())
}

fn decode_base64(value: &str) -> anyhow::Result<Vec<u8>> {
    if value.is_empty() || !value.len().is_multiple_of(4) {
        bail!("CEX Campaign input Secret campaign.json is not valid base64");
    }
    let mut decoded = Vec::with_capacity(value.len() / 4 * 3);
    for chunk in value.as_bytes().as_chunks::<4>().0 {
        let mut quartet = [0u8; 4];
        let mut padding = 0usize;
        for (index, byte) in chunk.iter().copied().enumerate() {
            quartet[index] = match byte {
                b'A'..=b'Z' => byte - b'A',
                b'a'..=b'z' => byte - b'a' + 26,
                b'0'..=b'9' => byte - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                b'=' => {
                    padding += 1;
                    0
                }
                _ => bail!("CEX Campaign input Secret campaign.json is not valid base64"),
            };
            if padding > 0 && byte != b'=' {
                bail!("CEX Campaign input Secret campaign.json is not valid base64");
            }
        }
        if padding > 2 || (padding > 0 && !chunk[(4 - padding)..].iter().all(|byte| *byte == b'='))
        {
            bail!("CEX Campaign input Secret campaign.json is not valid base64");
        }
        decoded.push((quartet[0] << 2) | (quartet[1] >> 4));
        if padding < 2 {
            decoded.push((quartet[1] << 4) | (quartet[2] >> 2));
        }
        if padding == 0 {
            decoded.push((quartet[2] << 6) | quartet[3]);
        }
    }
    Ok(decoded)
}

fn load_submission(path: &std::path::Path) -> anyhow::Result<MissionDispatchSubmission> {
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("open mission dispatch submission {}", path.display()))?;
    if file.metadata()?.len() > MAX_SUBMISSION_BYTES {
        bail!("mission dispatch submission exceeds {MAX_SUBMISSION_BYTES} bytes");
    }
    serde_json::from_reader(&mut file)
        .with_context(|| format!("parse mission dispatch submission {}", path.display()))
}

fn validate_submission(
    submission: MissionDispatchSubmission,
) -> anyhow::Result<ValidatedSubmission> {
    validate_submission_with_request_check(submission, validate_request)
}

fn validate_submission_with_request_check(
    submission: MissionDispatchSubmission,
    check: impl FnOnce(&CampaignRequest) -> anyhow::Result<()>,
) -> anyhow::Result<ValidatedSubmission> {
    validate_dns_label("attempt id", &submission.attempt_id)?;
    check(&submission.request)?;
    let image_digest = image_digest(&submission.image)?;
    if submission.request.image_identity != image_digest {
        bail!("campaign request image identity must match the pinned Job image digest");
    }
    let request_bytes = serialize_request(&submission.request)?;
    if request_bytes.len() as u64 > MAX_SUBMISSION_BYTES {
        bail!("campaign request exceeds {MAX_SUBMISSION_BYTES} bytes");
    }
    let request_sha256 = hex::encode(Sha256::digest(&request_bytes));
    let request_json = String::from_utf8(request_bytes)
        .context("campaign request must serialize as UTF-8 JSON")?;
    let submission_identity_sha256 = sha256_text(&format!(
        "{}:{}",
        submission.attempt_id, submission.request.campaign_id
    ));
    let submission_identity_label = submission_identity_sha256[..32].to_string();
    let job_name = format!("alpha-campaign-{submission_identity_label}");
    let secret_name = format!("{job_name}-inputs");
    Ok(ValidatedSubmission {
        submission,
        image_digest,
        request_sha256,
        request_json,
        submission_identity_sha256,
        job_name,
        secret_name,
    })
}

pub(crate) fn write_submission(
    path: &Path,
    attempt_id: &str,
    image: &str,
    request: CampaignRequest,
) -> anyhow::Result<SubmissionRenderReport> {
    let validated = validate_submission(MissionDispatchSubmission {
        attempt_id: attempt_id.to_string(),
        image: image.to_string(),
        request,
    })?;
    data_mission::write_json_atomic(path, &validated.submission)?;
    Ok(SubmissionRenderReport {
        request_sha256: validated.request_sha256,
        submission_identity_sha256: validated.submission_identity_sha256,
        job_name: validated.job_name,
        secret_name: validated.secret_name,
    })
}

struct DispatchManifestInput<'a> {
    attempt_id: &'a str,
    campaign_id: &'a str,
    job_name: &'a str,
    secret_name: &'a str,
    image: &'a str,
    image_digest: &'a str,
    request_sha256: &'a str,
    request_json: &'a str,
    submission_identity_sha256: &'a str,
    args: Vec<String>,
    active_deadline_seconds: u64,
    trusted_keys_json: Option<&'a str>,
}

fn render_manifest(validated: &ValidatedSubmission, namespace: &str) -> anyhow::Result<Value> {
    render_campaign_manifest(
        DispatchManifestInput {
            attempt_id: &validated.submission.attempt_id,
            campaign_id: &validated.submission.request.campaign_id,
            job_name: &validated.job_name,
            secret_name: &validated.secret_name,
            image: &validated.submission.image,
            image_digest: &validated.image_digest,
            request_sha256: &validated.request_sha256,
            request_json: &validated.request_json,
            submission_identity_sha256: &validated.submission_identity_sha256,
            trusted_keys_json: None,
            active_deadline_seconds: ACTIVE_DEADLINE_SECONDS,
            args: vec![
                "mission".into(),
                "campaign-execute".into(),
                "--pre-holdout".into(),
                "--work-dir".into(),
                "/work".into(),
                "--campaign-id".into(),
                validated.submission.request.campaign_id.clone(),
                "--image-identity".into(),
                validated.image_digest.clone(),
                "--request".into(),
                "/inputs/campaign.json".into(),
                "--request-sha256".into(),
                validated.request_sha256.clone(),
            ],
        },
        namespace,
    )
}

fn render_campaign_manifest(
    input: DispatchManifestInput<'_>,
    namespace: &str,
) -> anyhow::Result<Value> {
    validate_dns_label("namespace", namespace)?;
    let attempt_id = input.attempt_id;
    let campaign_id = input.campaign_id;
    let labels = json!({
        "app.kubernetes.io/name": "monday-alpha-campaign",
        "app.kubernetes.io/part-of": "monday",
        "research.monday/campaign-id": &campaign_id,
    });
    let annotations = json!({
        "research.monday/attempt-id": &attempt_id,
        "research.monday/campaign-id": &campaign_id,
        "research.monday/request-sha256": &input.request_sha256,
        "research.monday/submission-identity-sha256": &input.submission_identity_sha256,
        "research.monday/image-digest": &input.image_digest,
        "research.monday/lane": "cex_research_campaign",
    });
    let mut manifest = json!({
        "apiVersion": "v1",
        "kind": "List",
        "items": [
            {
                "apiVersion": "v1",
                "kind": "Secret",
                "metadata": {
                    "name": input.secret_name,
                    "namespace": namespace,
                    "labels": labels,
                    "annotations": {
                        "research.monday/attempt-id": &attempt_id,
                        "research.monday/campaign-id": &campaign_id,
                        "research.monday/request-sha256": &input.request_sha256,
                    }
                },
                "type": "Opaque",
                "immutable": true,
                "stringData": {
                    "campaign.json": input.request_json,
                },
            },
            {
                "apiVersion": "batch/v1",
                "kind": "Job",
                "metadata": {
                    "name": input.job_name,
                    "namespace": namespace,
                    "labels": labels,
                    "annotations": annotations,
                },
                "spec": {
                    "suspend": true,
                    "parallelism": 1,
                    "completions": 1,
                    "backoffLimit": 0,
                    "activeDeadlineSeconds": input.active_deadline_seconds,
                    "ttlSecondsAfterFinished": 86400,
                    "template": {
                        "metadata": { "labels": labels, "annotations": annotations },
                        "spec": {
                            "restartPolicy": "Never",
                            "automountServiceAccountToken": false,
                            "imagePullSecrets": [{ "name": "monday-acr" }],
                            "nodeSelector": { "kubernetes.io/arch": "amd64", "workload": "backtest" },
                            "securityContext": {
                                "runAsNonRoot": true,
                                "runAsUser": 1000,
                                "runAsGroup": 1000,
                                "fsGroup": 1000,
                                "seccompProfile": { "type": "RuntimeDefault" }
                            },
                            "containers": [{
                                "name": "alpha-campaign",
                                "image": input.image,
                                "imagePullPolicy": "IfNotPresent",
                                "command": ["/usr/local/bin/alpha-harness"],
                                "args": input.args,
                                "resources": {
                                    "requests": { "cpu": "3500m", "memory": "8Gi" },
                                    "limits": { "cpu": "3500m", "memory": "12Gi" }
                                },
                                "securityContext": {
                                    "allowPrivilegeEscalation": false,
                                    "capabilities": { "drop": ["ALL"] },
                                    "readOnlyRootFilesystem": true
                                },
                                "volumeMounts": [
                                    { "name": "work", "mountPath": "/work" },
                                    { "name": "tmp", "mountPath": "/tmp" },
                                    { "name": "inputs", "mountPath": "/inputs", "readOnly": true }
                                ]
                            }],
                            "volumes": [
                                { "name": "work", "emptyDir": { "sizeLimit": "20Gi" } },
                                { "name": "tmp", "emptyDir": {} },
                                {
                                    "name": "inputs",
                                    "secret": {
                                        "secretName": input.secret_name,
                                        "items": [{ "key": "campaign.json", "path": "campaign.json" }]
                                    }
                                }
                            ]
                        }
                    }
                }
            }
        ]
    });
    if let Some(keys) = input.trusted_keys_json {
        manifest["items"][0]["stringData"]["final-trusted-keys.json"] = Value::String(keys.into());
        manifest["items"][1]["spec"]["template"]["spec"]["volumes"][2]["secret"]["items"]
            .as_array_mut()
            .expect("input items")
            .push(json!({"key":"final-trusted-keys.json","path":"final-trusted-keys.json"}));
    }
    Ok(manifest)
}

pub(crate) fn image_digest(image: &str) -> anyhow::Result<String> {
    hft_research_manifest::canonical_image_digest(image).map_err(anyhow::Error::msg)
}

fn sha256_text(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mission_campaign::valid_request_for_tests;

    #[test]
    fn render_is_stable_for_the_same_campaign_identity() {
        let first = validate_submission(valid_submission()).unwrap();
        let second = validate_submission(valid_submission()).unwrap();

        assert_eq!(first.job_name, second.job_name);
        assert_eq!(first.secret_name, second.secret_name);
        assert_eq!(
            first.submission_identity_sha256,
            second.submission_identity_sha256
        );
    }

    #[test]
    fn render_rejects_unpinned_images() {
        let mut submission = valid_submission();
        submission.image = "registry/research-runner:latest".to_string();

        let error = validate_submission(submission).unwrap_err();
        assert!(format!("{error:#}").contains("@sha256 digest"));
    }

    #[test]
    fn render_job_disables_service_account_token() {
        let rendered =
            render_manifest(&validate_submission(valid_submission()).unwrap(), "monday").unwrap();
        let job = &rendered["items"][1];

        assert_eq!(job["spec"]["suspend"], true);
        assert_eq!(job["spec"]["parallelism"], 1);
        assert_eq!(job["spec"]["completions"], 1);
        assert_eq!(job["spec"]["backoffLimit"], 0);
        assert_eq!(job["spec"]["activeDeadlineSeconds"], 21_608);
        assert_eq!(
            job["spec"]["template"]["spec"]["automountServiceAccountToken"],
            false
        );
        assert_eq!(
            job["spec"]["template"]["spec"]["containers"][0]["args"][1],
            "campaign-execute"
        );
    }

    #[test]
    fn final_secret_readback_binds_every_input_including_trust_keys() {
        let expected = json!({"stringData":{"campaign.json":"{}","final-trusted-keys.json":"{}"}});
        let observed = json!({"data":{"campaign.json":"e30=","final-trusted-keys.json":"e30="}});
        validate_complete_secret_payload(&observed, &expected).unwrap();
        let mut changed = observed.clone();
        changed["data"]["final-trusted-keys.json"] = json!("bm8=");
        assert!(validate_complete_secret_payload(&changed, &expected).is_err());
        changed = observed.clone();
        changed["data"]["extra"] = json!("e30=");
        assert!(validate_complete_secret_payload(&changed, &expected).is_err());
        changed = observed;
        changed["data"]
            .as_object_mut()
            .unwrap()
            .remove("final-trusted-keys.json");
        assert!(validate_complete_secret_payload(&changed, &expected).is_err());
    }

    #[test]
    fn campaign_secret_is_owned_by_the_ttl_job() {
        let owner = job_owner_reference("alpha-campaign-test", "job-uid").unwrap();

        assert_eq!(owner[0]["name"], "alpha-campaign-test");
        assert_eq!(owner[0]["uid"], "job-uid");
    }

    #[test]
    fn campaign_secret_creation_inlines_job_owner_reference() {
        let rendered =
            render_manifest(&validate_submission(valid_submission()).unwrap(), "monday").unwrap();
        let secret =
            secret_with_owner(&rendered["items"][0], "alpha-campaign-test", "job-uid").unwrap();

        assert_eq!(
            secret["metadata"]["ownerReferences"][0]["name"],
            "alpha-campaign-test"
        );
        assert_eq!(secret["metadata"]["ownerReferences"][0]["uid"], "job-uid");
    }

    #[test]
    fn campaign_secret_readback_binds_request_and_job() {
        let request_sha256 = hex::encode(Sha256::digest(br#"{}"#));
        let secret = json!({
            "metadata": {
                "name": "alpha-campaign-test-inputs",
                "annotations": { "research.monday/request-sha256": request_sha256 },
                "ownerReferences": [{
                    "apiVersion": "batch/v1",
                    "kind": "Job",
                    "name": "alpha-campaign-test",
                    "uid": "job-uid",
                }],
            },
            "immutable": true,
            "data": { "campaign.json": "e30=" },
        });

        validate_secret_readback(
            &secret,
            "alpha-campaign-test-inputs",
            &request_sha256,
            b"{}",
            "alpha-campaign-test",
            "job-uid",
        )
        .unwrap();
    }

    #[test]
    fn campaign_secret_readback_rejects_request_byte_drift() {
        let request_sha256 = "44136fa355b3678a1146ad16f7e8649e94fb4fc21f1d4a1765e83105416dc9f9";
        let secret = json!({
            "metadata": {
                "name": "alpha-campaign-test-inputs",
                "annotations": { "research.monday/request-sha256": request_sha256 },
                "ownerReferences": [{
                    "apiVersion": "batch/v1",
                    "kind": "Job",
                    "name": "alpha-campaign-test",
                    "uid": "job-uid",
                }],
            },
            "immutable": true,
            "data": { "campaign.json": "eyJmb28iOiJiYXIifQ==" },
        });

        assert!(validate_secret_readback(
            &secret,
            "alpha-campaign-test-inputs",
            request_sha256,
            b"{}",
            "alpha-campaign-test",
            "job-uid",
        )
        .is_err());
    }

    // Model only the merge/test/replace operations used at this API boundary.
    // Apply to a copy: RFC 6902 failure must not persist partial changes.
    fn apply_release_patch(job: &mut Value, patch: &Value) -> anyhow::Result<()> {
        let mut updated = job.clone();
        if let Some(operations) = patch.as_array() {
            for operation in operations {
                let path = operation["path"].as_str().context("patch path")?;
                let target = updated.pointer_mut(path).context("patch target")?;
                match operation["op"].as_str() {
                    Some("test") if *target == operation["value"] => (),
                    Some("replace") => *target = operation["value"].clone(),
                    _ => bail!("patch precondition failed"),
                }
            }
        } else {
            updated["spec"]["suspend"] = patch["spec"]["suspend"].clone();
        }
        *job = updated;
        Ok(())
    }

    #[test]
    fn release_patch_preserves_replaced_or_changed_job() {
        let observed =
            json!({"metadata":{"uid":"original", "resourceVersion":"17"}, "spec":{"suspend":true}});
        let patch = release_job_patch(&observed).unwrap();
        let mut unchanged = observed.clone();
        apply_release_patch(&mut unchanged, &patch).unwrap();
        assert_eq!(unchanged["spec"]["suspend"], false);
        for (field, value) in [("uid", "replacement"), ("resourceVersion", "18")] {
            let mut changed = observed.clone();
            changed["metadata"][field] = value.into();
            let before = changed.clone();
            assert!(
                apply_release_patch(&mut changed, &patch).is_err(),
                "released changed {field}"
            );
            assert_eq!(changed, before);
        }
    }

    #[test]
    fn release_requires_observed_identity_and_allows_verified_retransmission() {
        let observed = json!({"metadata":{"uid":"original", "resourceVersion":"opaque-version"}, "spec":{"suspend":false}});
        for field in ["uid", "resourceVersion"] {
            for invalid in [Value::Null, json!(""), json!(" "), json!(17)] {
                let mut missing = observed.clone();
                missing["metadata"][field] = invalid;
                assert!(release_job_patch(&missing).is_err());
            }
        }
        let mut already_released = observed.clone();
        apply_release_patch(
            &mut already_released,
            &release_job_patch(&observed).unwrap(),
        )
        .unwrap();
        assert_eq!(already_released, observed);
    }

    #[test]
    fn job_readback_requires_expected_suspend_state() {
        let expected_job =
            render_manifest(&validate_submission(valid_submission()).unwrap(), "monday").unwrap();
        let job = json!({
            "metadata": {
                "name": expected_job["items"][1]["metadata"]["name"].clone(),
                "annotations": { "research.monday/request-sha256": "request-sha" },
            },
            "spec": {
                "suspend": true,
                "parallelism": 1,
                "completions": 1,
                "backoffLimit": 0,
                "activeDeadlineSeconds": ACTIVE_DEADLINE_SECONDS,
                "ttlSecondsAfterFinished": 86400,
                "template": expected_job["items"][1]["spec"]["template"].clone(),
            },
        });
        let mut job = job;
        job["spec"]["template"]["spec"]["dnsPolicy"] = json!("ClusterFirst");
        job["spec"]["template"]["spec"]["containers"][0]["terminationMessagePath"] =
            json!("/dev/termination-log");
        job["spec"]["template"]["spec"]["volumes"][2]["secret"]["defaultMode"] = json!(420);

        validate_job_readback(
            &job,
            &expected_job["items"][1],
            expected_job["items"][1]["metadata"]["name"]
                .as_str()
                .unwrap(),
            "request-sha",
            true,
        )
        .unwrap();
        assert!(validate_job_readback(
            &job,
            &expected_job["items"][1],
            expected_job["items"][1]["metadata"]["name"]
                .as_str()
                .unwrap(),
            "request-sha",
            false
        )
        .is_err());
    }

    #[test]
    fn job_readback_rejects_execution_template_drift() {
        let expected_job =
            render_manifest(&validate_submission(valid_submission()).unwrap(), "monday").unwrap();
        let mut job = expected_job["items"][1].clone();
        job["metadata"]["annotations"]["research.monday/request-sha256"] = json!("request-sha");
        job["spec"]["template"]["spec"]["containers"][0]["env"] =
            json!([{ "name": "INJECTED", "value": "1" }]);

        assert!(validate_job_readback(
            &job,
            &expected_job["items"][1],
            expected_job["items"][1]["metadata"]["name"]
                .as_str()
                .unwrap(),
            "request-sha",
            true
        )
        .is_err());

        let mut privileged_job = expected_job["items"][1].clone();
        privileged_job["metadata"]["annotations"]["research.monday/request-sha256"] =
            json!("request-sha");
        privileged_job["spec"]["template"]["spec"]["hostPID"] = json!(true);
        assert!(validate_job_readback(
            &privileged_job,
            &expected_job["items"][1],
            expected_job["items"][1]["metadata"]["name"]
                .as_str()
                .unwrap(),
            "request-sha",
            true
        )
        .is_err());
        for field in ["parallelism", "completions"] {
            let mut multiplied_job = expected_job["items"][1].clone();
            multiplied_job["metadata"]["annotations"]["research.monday/request-sha256"] =
                json!("request-sha");
            multiplied_job["spec"][field] = json!(2);
            assert!(
                validate_job_readback(
                    &multiplied_job,
                    &expected_job["items"][1],
                    expected_job["items"][1]["metadata"]["name"]
                        .as_str()
                        .unwrap(),
                    "request-sha",
                    true
                )
                .is_err(),
                "accepted multiplied Job {field}"
            );
        }
    }

    #[test]
    fn create_conflict_only_adopts_already_exists_jobs() {
        assert!(is_job_create_conflict(&anyhow::anyhow!(
            "kubectl failed to create immutable CEX Campaign Job: jobs.batch \"x\" AlreadyExists"
        )));
        assert!(!is_job_create_conflict(&anyhow::anyhow!(
            "kubectl failed to create immutable CEX Campaign Job: i/o timeout"
        )));
    }

    #[test]
    fn image_digest_rejects_non_canonical_repository_forms() {
        let digest = "1".repeat(64);
        for image in [
            format!("registry/research:latest@sha256:{digest}"),
            format!("Registry/research-runner@sha256:{digest}"),
            format!("registry/research-runner@@sha256:{digest}"),
            format!("registry/research runner@sha256:{digest}"),
        ] {
            assert!(image_digest(&image).is_err(), "{image}");
        }

        assert!(image_digest(&format!("localhost:5000/research/runner@sha256:{digest}")).is_ok());
    }

    #[test]
    fn only_matching_jobs_are_adopted() {
        let expected_job =
            render_manifest(&validate_submission(valid_submission()).unwrap(), "monday").unwrap();
        let job_name = expected_job["items"][1]["metadata"]["name"]
            .as_str()
            .unwrap();
        let request_sha256 = expected_job["items"][1]["metadata"]["annotations"]
            ["research.monday/request-sha256"]
            .as_str()
            .unwrap();

        let mut adopted_state = SubmissionObjectState::Unknown;
        adopt_existing_job(
            &expected_job["items"][1],
            &expected_job["items"][1],
            job_name,
            request_sha256,
            &mut adopted_state,
        )
        .unwrap();
        assert_eq!(adopted_state, SubmissionObjectState::Adopted);

        let mut mismatched_state = SubmissionObjectState::Unknown;
        let mut mismatched_job = expected_job["items"][1].clone();
        mismatched_job["metadata"]["annotations"]["research.monday/request-sha256"] =
            json!("other-request");
        assert!(adopt_existing_job(
            &mismatched_job,
            &expected_job["items"][1],
            job_name,
            request_sha256,
            &mut mismatched_state,
        )
        .is_err());
        assert_eq!(mismatched_state, SubmissionObjectState::Unknown);

        let mut mismatched_released_state = SubmissionObjectState::Unknown;
        let mut mismatched_released_job = expected_job["items"][1].clone();
        mismatched_released_job["spec"]["suspend"] = json!(false);
        mismatched_released_job["metadata"]["annotations"]["research.monday/request-sha256"] =
            json!("other-request");
        assert!(adopt_existing_job(
            &mismatched_released_job,
            &expected_job["items"][1],
            job_name,
            request_sha256,
            &mut mismatched_released_state,
        )
        .is_err());
        assert_eq!(mismatched_released_state, SubmissionObjectState::Unknown);
    }

    #[test]
    fn released_exact_job_is_adoptable_for_retry() {
        let expected_job =
            render_manifest(&validate_submission(valid_submission()).unwrap(), "monday").unwrap();
        let job_name = expected_job["items"][1]["metadata"]["name"]
            .as_str()
            .unwrap();
        let request_sha256 = expected_job["items"][1]["metadata"]["annotations"]
            ["research.monday/request-sha256"]
            .as_str()
            .unwrap();
        let mut released_job = expected_job["items"][1].clone();
        released_job["spec"]["suspend"] = json!(false);

        let mut released_state = SubmissionObjectState::Unknown;
        adopt_existing_job(
            &released_job,
            &expected_job["items"][1],
            job_name,
            request_sha256,
            &mut released_state,
        )
        .unwrap();
        assert_eq!(released_state, SubmissionObjectState::Adopted);
    }

    #[test]
    fn matching_secret_conflict_is_adoptable() {
        let request_sha256 = hex::encode(Sha256::digest(br#"{}"#));
        let secret = json!({
            "metadata": {
                "name": "alpha-campaign-test-inputs",
                "annotations": { "research.monday/request-sha256": request_sha256 },
                "ownerReferences": [{
                    "apiVersion": "batch/v1",
                    "kind": "Job",
                    "name": "alpha-campaign-test",
                    "uid": "job-uid",
                }],
            },
            "immutable": true,
            "data": { "campaign.json": "e30=" },
        });

        validate_secret_readback(
            &secret,
            "alpha-campaign-test-inputs",
            &request_sha256,
            b"{}",
            "alpha-campaign-test",
            "job-uid",
        )
        .unwrap();
    }

    #[test]
    fn claimed_job_retry_never_recreates_a_missing_or_replaced_job() {
        use alpha_store::campaign_ledger::{CampaignDispatchClaimV1, CampaignDispatchTargetV1};
        let validated = validate_submission(valid_submission()).unwrap();
        let manifest = render_manifest(&validated, "monday-research").unwrap();
        let mut job = manifest["items"][1].clone();
        job["metadata"]["uid"] = json!("original");
        job["metadata"]["resourceVersion"] = json!("1");
        let mut claim = CampaignDispatchClaimV1 {
            target: CampaignDispatchTargetV1 {
                context: "context".into(),
                namespace: "monday-research".into(),
                job_name: validated.job_name.clone(),
                manifest_sha256: "a".repeat(64),
            },
            job_uid: Some("original".into()),
            sequence: 4,
        };
        let mut created = false;
        assert!(resolve_dispatch_job(
            &claim,
            false,
            &manifest["items"][1],
            &validated.request_sha256,
            || {
                created = true;
                Ok(job.clone())
            },
            || anyhow::bail!("NotFound")
        )
        .is_err());
        assert!(!created);
        let mut replacement = job.clone();
        replacement["metadata"]["uid"] = json!("replacement");
        assert!(resolve_dispatch_job(
            &claim,
            false,
            &manifest["items"][1],
            &validated.request_sha256,
            || {
                created = true;
                Ok(job.clone())
            },
            || Ok(replacement)
        )
        .is_err());
        assert!(!created);
        resolve_dispatch_job(
            &claim,
            false,
            &manifest["items"][1],
            &validated.request_sha256,
            || {
                created = true;
                Ok(job.clone())
            },
            || Ok(job.clone()),
        )
        .unwrap();
        assert!(!created);
        claim.job_uid = None;
        let mut running = job.clone();
        running["spec"]["suspend"] = json!(false);
        assert!(
            resolve_dispatch_job(
                &claim,
                true,
                &manifest["items"][1],
                &validated.request_sha256,
                || Ok(running),
                || anyhow::bail!("must not read fallback")
            )
            .is_err(),
            "a first claim cannot adopt an already-running unbound Job"
        );
        resolve_dispatch_job(
            &claim,
            true,
            &manifest["items"][1],
            &validated.request_sha256,
            || {
                created = true;
                Ok(job.clone())
            },
            || anyhow::bail!("must not read fallback"),
        )
        .unwrap();
        assert!(created);
    }

    #[test]
    fn terminal_provenance_requires_the_bound_successful_job_pod_and_execution() {
        let validated = validate_submission(valid_submission()).unwrap();
        let manifest = render_manifest(&validated, "monday-research").unwrap();
        let expected_job = &manifest["items"][1];
        let mut job = expected_job.clone();
        job["metadata"]["uid"] = json!("bound-job");
        job["spec"]["suspend"] = json!(false);
        job["status"] = json!({"succeeded":1,"conditions":[{"type":"Complete","status":"True"}]});
        let pod = json!({
            "metadata":{"uid":"pod-1","annotations":expected_job["spec"]["template"]["metadata"]["annotations"],
                "ownerReferences":[{"kind":"Job","name":validated.job_name,"uid":"bound-job"}]},
            "spec":expected_job["spec"]["template"]["spec"],
            "status":{"phase":"Succeeded","containerStatuses":[{"name":"alpha-campaign",
                "imageID":validated.submission.image,"restartCount":0,"state":{"terminated":{"exitCode":0}}}]},
        });
        assert_eq!(
            terminal::validate_terminal_provenance(expected_job, &job, &pod, "bound-job").unwrap(),
            ("bound-job".into(), "pod-1".into())
        );
        for pointer in ["/metadata/uid", "/status/succeeded"] {
            let mut changed = job.clone();
            *changed.pointer_mut(pointer).unwrap() = json!("different");
            assert!(terminal::validate_terminal_provenance(
                expected_job,
                &changed,
                &pod,
                "bound-job"
            )
            .is_err());
        }
        for pointer in [
            "/metadata/ownerReferences/0/uid",
            "/status/containerStatuses/0/imageID",
            "/status/containerStatuses/0/restartCount",
            "/status/containerStatuses/0/state/terminated/exitCode",
            "/spec/containers/0/args",
            "/status/phase",
        ] {
            let mut changed = pod.clone();
            *changed.pointer_mut(pointer).unwrap() = json!("different");
            assert!(
                terminal::validate_terminal_provenance(expected_job, &job, &changed, "bound-job")
                    .is_err(),
                "accepted changed {pointer}"
            );
        }
    }

    struct AdmissionFixture {
        inputs: crate::mission_render::tests::Fixture,
        control: std::path::PathBuf,
        validated: ValidatedSubmission,
        manifest: Value,
    }

    impl AdmissionFixture {
        fn new() -> Self {
            use alpha_domain::campaign_control::*;
            use alpha_domain::campaign_horizon::CampaignLabelHorizonV1;
            use alpha_domain::campaign_study::*;
            use alpha_store::{AlphaStore, ApprovalRecord};
            use chrono::{TimeDelta, Utc};
            use ed25519_dalek::SigningKey;
            use std::collections::{BTreeMap, BTreeSet};
            let inputs = crate::mission_render::tests::Fixture::canonical();
            let mut submission = valid_submission();
            submission.request = crate::mission_campaign::request_for_materialization_for_tests(
                &inputs.materialization_path,
            );
            let old_campaign_id = submission.request.campaign_id.clone();
            let materialization_metadata: Value =
                serde_json::from_slice(&std::fs::read(&inputs.materialization_path).unwrap())
                    .unwrap();
            let campaign_inputs_path = inputs._root.path().join("campaign-inputs.json");
            std::fs::write(
                &campaign_inputs_path,
                serde_json::to_vec(&json!({
                    "mission_id": materialization_metadata["mission_id"],
                    "output_prefix": "target",
                }))
                .unwrap(),
            )
            .unwrap();
            submission.request.campaign_inputs_sha256 =
                crate::mission_runner::sha256_file(&campaign_inputs_path).unwrap();
            submission.request.research_plan.label_horizon =
                Some(alpha_domain::campaign_horizon::CampaignLabelHorizonV1::canonical());
            submission.request.campaign_id =
                crate::mission_campaign::expected_campaign_id(&submission.request).unwrap();
            for round in &mut submission.request.rounds {
                round.mission_put_url = round
                    .mission_put_url
                    .replace(&old_campaign_id, &submission.request.campaign_id);
                round.mission_readback_url = round
                    .mission_readback_url
                    .replace(&old_campaign_id, &submission.request.campaign_id);
                round.result_put_url = round
                    .result_put_url
                    .replace(&old_campaign_id, &submission.request.campaign_id);
                round.result_readback_url = round
                    .result_readback_url
                    .replace(&old_campaign_id, &submission.request.campaign_id);
            }
            submission.request.campaign_result_put_url = submission
                .request
                .campaign_result_put_url
                .replace(&old_campaign_id, &submission.request.campaign_id);
            submission.request.campaign_result_readback_url = submission
                .request
                .campaign_result_readback_url
                .replace(&old_campaign_id, &submission.request.campaign_id);
            let data_fingerprint = crate::mission_campaign::campaign_data_fingerprint_sha256(
                &submission.request.campaign_inputs_sha256,
                &submission.request.producer_source_revision,
                &submission.request.feature_sha256,
                &submission.request.materialization_sha256,
                &submission.request.replay_artifact_sha256,
                &submission.request.replay_manifest_sha256,
            )
            .unwrap();
            for round in &mut submission.request.rounds {
                round.identity.data_fingerprint_sha256 = data_fingerprint.clone();
            }
            let previous_campaign_id = submission.request.campaign_id.clone();
            submission.request.campaign_id =
                crate::mission_campaign::expected_campaign_id(&submission.request).unwrap();
            for round in &mut submission.request.rounds {
                round.mission_put_url = round
                    .mission_put_url
                    .replace(&previous_campaign_id, &submission.request.campaign_id);
                round.mission_readback_url = round
                    .mission_readback_url
                    .replace(&previous_campaign_id, &submission.request.campaign_id);
                round.result_put_url = round
                    .result_put_url
                    .replace(&previous_campaign_id, &submission.request.campaign_id);
                round.result_readback_url = round
                    .result_readback_url
                    .replace(&previous_campaign_id, &submission.request.campaign_id);
            }
            submission.request.campaign_result_put_url = submission
                .request
                .campaign_result_put_url
                .replace(&previous_campaign_id, &submission.request.campaign_id);
            submission.request.campaign_result_readback_url = submission
                .request
                .campaign_result_readback_url
                .replace(&previous_campaign_id, &submission.request.campaign_id);
            let validated = validate_submission(submission).unwrap();
            let manifest = render_manifest(&validated, "monday-research").unwrap();
            let controller_image = format!("registry/controller@sha256:{}", "e".repeat(64));
            let inspection = serde_json::to_value(
                admission::inspect_binding(
                    &validated,
                    &manifest,
                    &inputs.materialization_path,
                    &controller_image,
                    0,
                )
                .unwrap(),
            )
            .unwrap();
            let now = Utc::now();
            let grant = CampaignRootGrantV1 {
                schema_version: ROOT_GRANT_SCHEMA.into(),
                root_id: "dispatch-root".into(),
                family: CampaignFamilyPolicyV1 {
                    family_id: "dispatch-study".into(),
                    definition_sha256: "a".repeat(64),
                    max_trials: 1000,
                },
                execution_scope: CampaignExecutionScope::PreHoldout,
                execution: serde_json::from_value(inspection["execution"].clone()).unwrap(),
                allowed_policy_revision_ids: BTreeSet::from([
                    inspection["policy_revision_id"].as_str().unwrap().into(),
                    format!("cex-search-policy-{}", "f".repeat(64)),
                ]),
                max_follow_ups: 1,
                budget: CampaignRootBudgetV1 {
                    max_trials: 1000,
                    max_job_attempts: 2,
                    max_job_seconds: 100_000,
                    max_llm_tokens: 0,
                },
                valid_from: now - TimeDelta::minutes(1),
                expires_at: now + TimeDelta::hours(24),
            };
            let signing_key = SigningKey::from_bytes(&[19; 32]);
            let signed = sign_campaign_root_grant(grant, "operator".into(), &signing_key).unwrap();
            let target_grant = CampaignRootGrantV1 {
                schema_version: ROOT_GRANT_SCHEMA.into(),
                root_id: "dispatch-target-root".into(),
                family: CampaignFamilyPolicyV1 {
                    family_id: "dispatch-target".into(),
                    definition_sha256: "c".repeat(64),
                    max_trials: 1000,
                },
                execution_scope: signed.grant.execution_scope.clone(),
                execution: signed.grant.execution.clone(),
                allowed_policy_revision_ids: signed.grant.allowed_policy_revision_ids.clone(),
                max_follow_ups: 1,
                budget: signed.grant.budget.clone(),
                valid_from: signed.grant.valid_from,
                expires_at: signed.grant.expires_at,
            };
            let target_key = SigningKey::from_bytes(&[29; 32]);
            let signed_target =
                sign_campaign_root_grant(target_grant, "target-operator".into(), &target_key)
                    .unwrap();
            let root = inputs._root.path();
            std::fs::write(
                root.join("grant.json"),
                serde_json::to_vec(&signed).unwrap(),
            )
            .unwrap();
            std::fs::write(
                root.join("target-grant.json"),
                serde_json::to_vec(&signed_target).unwrap(),
            )
            .unwrap();
            std::fs::write(
                root.join("keys.json"),
                serde_json::to_vec(&json!({
                    "operator": hex::encode(signing_key.verifying_key().as_bytes()),
                    "target-operator": hex::encode(target_key.verifying_key().as_bytes()),
                }))
                .unwrap(),
            )
            .unwrap();
            let approval = ApprovalRecord {
                approval_id: "dispatch-approval".into(),
                approval_class: "campaign_root".into(),
                subject_id: signed.grant.root_id.clone(),
                payload: json!({"grant_sha256": signed.content_sha256, "family_id": signed.grant.family.family_id}),
                signer_id: Some("operator".into()),
                valid_from: Some(signed.grant.valid_from),
                expires_at: Some(signed.grant.expires_at),
                revoked_at: None,
                revoked_by: None,
                revocation_reason: None,
                created_at: signed.grant.valid_from,
            };
            let mut store = AlphaStore::open(root.join("ledger.duckdb")).unwrap();
            store.record_approval(&approval).unwrap();
            let verified_root = verify_campaign_root_grant(
                &signed,
                &BTreeMap::from([("operator".into(), signing_key.verifying_key())]),
                now,
            )
            .unwrap();
            store
                .register_campaign_root(&verified_root, &approval.approval_id, now)
                .unwrap();
            let target_approval = ApprovalRecord {
                approval_id: "dispatch-target-approval".into(),
                approval_class: "campaign_root".into(),
                subject_id: signed_target.grant.root_id.clone(),
                payload: json!({
                    "grant_sha256": signed_target.content_sha256,
                    "family_id": signed_target.grant.family.family_id,
                }),
                signer_id: Some("target-operator".into()),
                valid_from: Some(signed_target.grant.valid_from),
                expires_at: Some(signed_target.grant.expires_at),
                revoked_at: None,
                revoked_by: None,
                revocation_reason: None,
                created_at: signed_target.grant.valid_from,
            };
            store.record_approval(&target_approval).unwrap();
            let verified_target = verify_campaign_root_grant(
                &signed_target,
                &BTreeMap::from([("target-operator".into(), target_key.verifying_key())]),
                now,
            )
            .unwrap();
            store
                .register_campaign_root(&verified_target, &target_approval.approval_id, now)
                .unwrap();
            let study_id = "dispatch-study-budget".to_string();
            let target_horizon_sha256 = CampaignLabelHorizonV1::canonical().content_hash().unwrap();
            let study_grant = CampaignStudyGrantV1 {
                schema_version: STUDY_GRANT_SCHEMA.into(),
                study_id: study_id.clone(),
                members: vec![
                    CampaignStudyMemberV1 {
                        family_id: signed.grant.family.family_id.clone(),
                        root_grant_sha256: signed.content_sha256.clone(),
                        family_definition_sha256: signed.grant.family.definition_sha256.clone(),
                        family_max_trials: signed.grant.family.max_trials,
                        execution_scope: signed.grant.execution_scope.clone(),
                        execution: signed.grant.execution.clone(),
                        label_horizon_sha256: target_horizon_sha256.clone(),
                    },
                    CampaignStudyMemberV1 {
                        family_id: signed_target.grant.family.family_id.clone(),
                        root_grant_sha256: signed_target.content_sha256.clone(),
                        family_definition_sha256: signed_target
                            .grant
                            .family
                            .definition_sha256
                            .clone(),
                        family_max_trials: signed_target.grant.family.max_trials,
                        execution_scope: signed_target.grant.execution_scope.clone(),
                        execution: signed_target.grant.execution.clone(),
                        label_horizon_sha256: target_horizon_sha256,
                    },
                ],
                budget: CampaignStudyBudgetV1 {
                    max_trials: 1000,
                    max_job_attempts: 2,
                    max_job_seconds: 100_000,
                    max_llm_tokens: 0,
                },
                valid_from: signed.grant.valid_from,
                expires_at: signed.grant.expires_at,
            };
            let study_key = SigningKey::from_bytes(&[23; 32]);
            let signed_study =
                sign_campaign_study_grant(study_grant, "study-operator".into(), &study_key)
                    .unwrap();
            let verified_study = verify_campaign_study_grant(
                &signed_study,
                &BTreeMap::from([("study-operator".into(), study_key.verifying_key())]),
                now,
            )
            .unwrap();
            let study_approval = ApprovalRecord {
                approval_id: "dispatch-study-approval".into(),
                approval_class: "campaign_study".into(),
                subject_id: study_id.clone(),
                payload: json!({
                    "grant_sha256": verified_study.content_sha256(),
                    "study_id": study_id,
                }),
                signer_id: Some("study-operator".into()),
                valid_from: Some(signed.grant.valid_from),
                expires_at: Some(signed.grant.expires_at),
                revoked_at: None,
                revoked_by: None,
                revocation_reason: None,
                created_at: signed.grant.valid_from,
            };
            store.record_approval(&study_approval).unwrap();
            store
                .register_campaign_study(&verified_study, &study_approval.approval_id, now)
                .unwrap();
            drop(store);
            let origin =
                reqwest::Url::parse(&validated.submission.request.campaign_result_readback_url)
                    .unwrap()
                    .origin()
                    .ascii_serialization();
            let mut access = (1..=12).map(|sequence| {
                let key = format!("research/campaign-ledger/family-id=dispatch-study/sequence={sequence:020}/receipt.json");
                let url = format!("{origin}/{key}?signature=fixture-only");
                (key, json!({"put_url": url, "readback_url": url}))
            }).collect::<serde_json::Map<String, Value>>();
            for sequence in 1..=12 {
                let key = format!(
                    "research/campaign-ledger/study-id=dispatch-study-budget/sequence={sequence:020}/receipt.json"
                );
                let url = format!("{origin}/{key}?signature=fixture-only");
                access.insert(key, json!({"put_url": url, "readback_url": url}));
                let key = format!(
                    "research/campaign-ledger/family-id=dispatch-target/sequence={sequence:020}/receipt.json"
                );
                let url = format!("{origin}/{key}?signature=fixture-only");
                access.insert(key, json!({"put_url": url, "readback_url": url}));
            }
            let control = root.join("control.json");
            std::fs::write(&control, serde_json::to_vec(&json!({
                "schema_version": "monday.campaign_dispatch_control.v1",
                "ledger_path": "ledger.duckdb", "signed_root_grant_path": "grant.json",
                "trusted_keys_path": "keys.json", "materialization_path": "materialization.json",
                "approval_id": "dispatch-approval", "controller_image": controller_image,
                "attempt_ordinal": 0, "receipt_access": access,
            })).unwrap()).unwrap();
            Self {
                inputs,
                control,
                validated,
                manifest,
            }
        }

        fn open(&self) -> admission::Admission {
            admission::Admission::open(
                &self.control,
                &self.validated,
                &self.manifest,
                "research-context",
                "monday-research",
            )
            .unwrap()
        }

        fn usage(&self) -> alpha_store::campaign_ledger::CampaignBudgetUsageV1 {
            alpha_store::AlphaStore::open(self.inputs._root.path().join("ledger.duckdb"))
                .unwrap()
                .campaign_family_usage("dispatch-study")
                .unwrap()
        }
    }

    #[test]
    fn dispatch_inspection_uses_the_same_supported_materialization_scope_as_the_renderer() {
        let fixture = AdmissionFixture::new();
        let mut metadata: Value =
            serde_json::from_slice(&std::fs::read(&fixture.inputs.materialization_path).unwrap())
                .unwrap();
        metadata["label_horizon_buckets"] = json!(6);
        metadata["snapshot"]["label_horizon_buckets"] = json!(6);
        let snapshot: hft_research_manifest::CexReplaySnapshotV5 =
            serde_json::from_value(metadata["snapshot"].clone()).unwrap();
        metadata["snapshot_sha256"] = json!(snapshot.sha256());
        std::fs::write(
            &fixture.inputs.materialization_path,
            serde_json::to_vec(&metadata).unwrap(),
        )
        .unwrap();
        let mut submission = valid_submission();
        submission.request = crate::mission_campaign::request_for_materialization_for_tests(
            &fixture.inputs.materialization_path,
        );
        let validated = validate_submission(submission).unwrap();
        let manifest = render_manifest(&validated, "monday-research").unwrap();
        let error = admission::inspect_binding(
            &validated,
            &manifest,
            &fixture.inputs.materialization_path,
            &format!("registry/controller@sha256:{}", "e".repeat(64)),
            0,
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("approved Binance Spot or USD-M BTCUSDT"));
    }

    #[test]
    fn controller_handoff_binds_existing_volume_authority_and_read_only_cluster_access() {
        let fixture = AdmissionFixture::new();
        let root = fixture.inputs._root.path();
        let work_dir = root.join("cycles/study");
        std::fs::create_dir_all(&work_dir).unwrap();
        std::fs::write(work_dir.join("controller-inputs.json"), serde_json::to_vec(&json!({
            "context":"monday-research-apne1", "namespace":"monday-research",
            "source_revision":fixture.validated.submission.request.build_source_revision,
            "image":fixture.validated.submission.image,
            "campaign_inputs_sha256":fixture.validated.submission.request.campaign_inputs_sha256,
        })).unwrap()).unwrap();
        let generation = work_dir.join(format!(
            "generation-{}",
            fixture
                .validated
                .submission
                .request
                .research_plan
                .generation
        ));
        std::fs::create_dir_all(&generation).unwrap();
        std::fs::write(generation.join("finalize-report.json"), serde_json::to_vec(&json!({
            "request_sha256":fixture.validated.request_sha256, "job_name":fixture.validated.job_name,
        })).unwrap()).unwrap();
        for marker in ["finalized", "dispatched"] {
            std::fs::write(generation.join(marker), b"").unwrap();
        }
        std::fs::write(
            generation.join("submission.json"),
            serde_json::to_vec(&fixture.validated.submission).unwrap(),
        )
        .unwrap();
        let args = crate::cli::CampaignControllerHandoffArgs {
            submission: root.join("submission.json"),
            control: fixture.control.clone(),
            volume_root: root.to_path_buf(),
            work_dir,
            pvc: "study-ledger".into(),
            service_account: "approved-oss-operator".into(),
            trusted_keys_configmap: "operator-trusted-keys".into(),
            campaign_pod: "worker-pod".into(),
            context: "monday-research-apne1".into(),
            namespace: "monday-research".into(),
            output: root.join("handoff.json"),
        };
        std::fs::write(
            &args.submission,
            serde_json::to_vec(&fixture.validated.submission).unwrap(),
        )
        .unwrap();
        controller::render(args.clone()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&args.output)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        let value: Value = serde_json::from_slice(&std::fs::read(&args.output).unwrap()).unwrap();
        let items = value["items"].as_array().unwrap();
        let secret = items.iter().find(|x| x["kind"] == "Secret").unwrap();
        let control: Value =
            serde_json::from_str(secret["stringData"]["control.json"].as_str().unwrap()).unwrap();
        assert_eq!(control["ledger_path"], "/campaign-root/ledger.duckdb");
        assert_eq!(
            control["materialization_path"],
            "/campaign-root/materialization.json"
        );
        assert_eq!(
            control["trusted_keys_path"],
            "/trusted-keys/root-public-keys.json"
        );
        let job = items.iter().find(|x| x["kind"] == "Job").unwrap();
        assert!(
            secret["stringData"].get("root-public-keys.json").is_none(),
            "trusted keys must not be frozen into per-attempt authority"
        );
        let pod = &job["spec"]["template"]["spec"];
        assert_eq!(pod["serviceAccountName"], args.service_account);
        assert_eq!(
            pod["securityContext"]["fsGroupChangePolicy"],
            "OnRootMismatch"
        );
        assert_eq!(
            pod["containers"][0]["env"][0]["value"],
            "/authority/control.json"
        );
        assert!(pod["initContainers"][0]["command"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x == "prepare-controller"));
        let role = items.iter().find(|x| x["kind"] == "Role").unwrap();
        assert!(role["rules"].as_array().unwrap().iter().all(|x| x["verbs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| ["get", "watch", "list"].iter().any(|allowed| v == allowed))));
        assert!(
            controller::render(args.clone()).is_err(),
            "private output cannot be overwritten"
        );
        assert_eq!(
            fixture.usage().job_attempts,
            0,
            "rendering does not reserve a trial or Job"
        );
        let mut wrong_context = args.clone();
        wrong_context.context = "other-cluster".into();
        assert!(controller::render_value(&wrong_context, &fixture.validated).is_err());
        std::fs::remove_file(generation.join("dispatched")).unwrap();
        assert!(controller::render_value(&args, &fixture.validated)
            .unwrap_err()
            .to_string()
            .contains("finalized and dispatched"));
        std::fs::write(generation.join("dispatched"), b"").unwrap();
        let mut escaping = args;
        escaping.volume_root = escaping.work_dir.clone();
        assert!(controller::render_value(&escaping, &fixture.validated).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn controller_preparation_only_restricts_the_owned_integrity_key() {
        use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
        let fixture = AdmissionFixture::new();
        let ledger = fixture.inputs._root.path().join("ledger.duckdb");
        let key = fixture
            .inputs
            ._root
            .path()
            .join("ledger.duckdb.integrity-key");
        let bytes = std::fs::read(&key).unwrap();
        let uid = std::fs::metadata(&key).unwrap().uid();
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o660)).unwrap();
        assert!(controller::restrict_integrity_key(&ledger, uid + 1).is_err());
        assert_eq!(
            std::fs::metadata(&key).unwrap().permissions().mode() & 0o777,
            0o660
        );
        controller::restrict_integrity_key(&ledger, uid).unwrap();
        assert_eq!(
            std::fs::metadata(&key).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(std::fs::read(&key).unwrap(), bytes);
        let linked_ledger = fixture.inputs._root.path().join("linked.duckdb");
        symlink(
            &key,
            fixture
                .inputs
                ._root
                .path()
                .join("linked.duckdb.integrity-key"),
        )
        .unwrap();
        assert!(controller::restrict_integrity_key(&linked_ledger, uid).is_err());
    }

    #[test]
    fn dispatch_reserves_real_request_once_and_requires_independent_receipt_bytes_before_actions() {
        let fixture = AdmissionFixture::new();
        let mut gate = fixture.open();
        gate.prepare().unwrap();
        assert_eq!(
            gate.reservation.declared_trials,
            fixture.validated.submission.request.declared_total_trials as u64
        );
        assert_eq!(gate.reservation.execution.job_memory_mib, 12 * 1024);
        assert_eq!(gate.reservation.execution.job_cpu_millis, 3500);
        assert_eq!(
            gate.reservation.reserved_job_seconds,
            ACTIVE_DEADLINE_SECONDS
        );
        assert_eq!(gate.reservation.reserved_llm_tokens, 0);
        assert_ne!(
            gate.reservation
                .execution
                .evaluation_views
                .search_view_sha256,
            gate.reservation
                .execution
                .evaluation_views
                .selection_view_sha256
        );
        assert!(
            gate.claim().is_err(),
            "reservation without archived receipts must not create a Job"
        );
        assert!(gate
            .publish_receipts_with(|_, bytes| {
                let mut corrupted = bytes.to_vec();
                corrupted.push(b' ');
                Ok(corrupted)
            })
            .is_err());
        assert!(gate.claim().is_err());
        let mut published_receipt_count = 0;
        gate.publish_receipts_with(|_, bytes| {
            published_receipt_count += 1;
            Ok(bytes.to_vec())
        })
        .unwrap();
        assert_eq!(published_receipt_count, 5);
        let (_, first) = gate.claim().unwrap();
        assert!(first);
        let mut called = false;
        assert!(gate
            .guarded(None, || {
                called = true;
                Ok(())
            })
            .is_err());
        assert!(
            !called,
            "dispatch claim itself must be independently archived"
        );
        gate.publish_receipts_with(|_, bytes| Ok(bytes.to_vec()))
            .unwrap();
        gate.guarded(None, || {
            called = true;
            Ok(())
        })
        .unwrap();
        assert!(called);
        gate.bind_job("job-uid-1").unwrap();
        gate.publish_receipts_with(|_, bytes| Ok(bytes.to_vec()))
            .unwrap();
        drop(gate);
        let before = fixture.usage();
        let mut gate = fixture.open();
        gate.prepare().unwrap();
        gate.publish_receipts_with(|_, bytes| Ok(bytes.to_vec()))
            .unwrap();
        let (claim, first) = gate.claim().unwrap();
        assert!(!first, "reopen must never permit a second create");
        assert_eq!(claim.job_uid.as_deref(), Some("job-uid-1"));
        gate.guarded(Some("job-uid-1"), || Ok(())).unwrap();
        drop(gate);
        assert_eq!(fixture.usage(), before);
    }

    #[test]
    fn dispatch_rejects_execution_drift_renamed_jobs_and_revoked_trust_without_spending_again() {
        let fixture = AdmissionFixture::new();
        let mut gate = fixture.open();
        gate.prepare().unwrap();
        gate.publish_receipts_with(|_, bytes| Ok(bytes.to_vec()))
            .unwrap();
        gate.claim().unwrap();
        gate.publish_receipts_with(|_, bytes| Ok(bytes.to_vec()))
            .unwrap();
        drop(gate);
        let before = fixture.usage();
        let mut changed = fixture.manifest.clone();
        changed["items"][1]["spec"]["template"]["spec"]["containers"][0]["resources"]["limits"]
            ["memory"] = json!("24Gi");
        assert!(admission::Admission::open(
            &fixture.control,
            &fixture.validated,
            &changed,
            "research-context",
            "monday-research"
        )
        .is_err());
        let mut changed = fixture.manifest.clone();
        changed["items"][1]["spec"]["template"]["spec"]["containers"][0]["args"]
            .as_array_mut()
            .unwrap()
            .retain(|arg| arg != "--pre-holdout");
        assert!(admission::Admission::open(
            &fixture.control,
            &fixture.validated,
            &changed,
            "research-context",
            "monday-research"
        )
        .is_err());
        let mut renamed = valid_submission();
        renamed.request = fixture.validated.submission.request.clone();
        renamed.attempt_id = "another-job-name".into();
        let renamed = validate_submission(renamed).unwrap();
        let manifest = render_manifest(&renamed, "monday-research").unwrap();
        let mut gate = admission::Admission::open(
            &fixture.control,
            &renamed,
            &manifest,
            "research-context",
            "monday-research",
        )
        .unwrap();
        gate.prepare().unwrap();
        assert!(
            gate.claim().is_err(),
            "a second name must not bypass the original dispatch claim"
        );
        drop(gate);
        assert_eq!(fixture.usage(), before);
        let keys_path = fixture.inputs._root.path().join("keys.json");
        #[cfg(unix)]
        let original_keys = std::fs::read(&keys_path).unwrap();
        let mut gate = fixture.open();
        std::fs::write(&keys_path, b"{}").unwrap();
        let mut called = false;
        assert!(gate
            .guarded(None, || {
                called = true;
                Ok(())
            })
            .is_err());
        assert!(!called, "release must reread the current trusted key set");
        drop(gate);
        #[cfg(unix)]
        {
            // Projected Kubernetes files and operator key rotations can replace
            // a symlink while leaving the old key file available for audit.
            let old_keys = fixture.inputs._root.path().join("keys-old.json");
            let new_keys = fixture.inputs._root.path().join("keys-new.json");
            std::fs::write(&old_keys, original_keys).unwrap();
            std::fs::write(&new_keys, b"{}").unwrap();
            std::fs::remove_file(&keys_path).unwrap();
            std::os::unix::fs::symlink(&old_keys, &keys_path).unwrap();
            let mut gate = fixture.open();
            std::fs::remove_file(&keys_path).unwrap();
            std::os::unix::fs::symlink(&new_keys, &keys_path).unwrap();
            let mut called = false;
            assert!(
                gate.guarded(None, || {
                    called = true;
                    Ok(())
                })
                .is_err(),
                "a rotated trust symlink must not retain the old signing authority"
            );
            assert!(!called);
        }
    }

    #[test]
    fn dispatched_terminal_accounting_uses_registered_authority_after_trust_rotation() {
        use alpha_domain::campaign_control::{
            CampaignAttemptOutcomeV1, CampaignAttemptSettlementV1,
        };
        use alpha_store::campaign_ledger::CampaignDispatchSettlementV1;
        let fixture = AdmissionFixture::new();
        let mut gate = fixture.open();
        gate.prepare().unwrap();
        gate.publish_receipts_with(|_, bytes| Ok(bytes.to_vec()))
            .unwrap();
        gate.claim().unwrap();
        gate.publish_receipts_with(|_, bytes| Ok(bytes.to_vec()))
            .unwrap();
        gate.bind_job("job-uid-1").unwrap();
        gate.publish_receipts_with(|_, bytes| Ok(bytes.to_vec()))
            .unwrap();
        let attempt = gate.reservation.clone();
        drop(gate);
        std::fs::remove_file(fixture.inputs._root.path().join("keys.json")).unwrap();
        assert!(admission::Admission::open(
            &fixture.control,
            &fixture.validated,
            &fixture.manifest,
            "research-context",
            "monday-research"
        )
        .is_err());
        let mut gate = admission::Admission::open_for_settlement(
            &fixture.control,
            &fixture.validated,
            &fixture.manifest,
            "research-context",
            "monday-research",
        )
        .unwrap();
        gate.settle(&CampaignDispatchSettlementV1 {
            job_uid: "job-uid-1".into(),
            pod_uid: "pod-uid-1".into(),
            settlement: CampaignAttemptSettlementV1 {
                operation_id: attempt.operation_id().unwrap(),
                reservation_sha256: attempt.content_hash().unwrap(),
                evidence_sha256: "b".repeat(64),
                outcome: CampaignAttemptOutcomeV1::NoCandidate,
                consumed_trials: Some(20),
            },
        })
        .unwrap();
        assert!(gate
            .publish_receipts_with(|_, _| anyhow::bail!("lost PUT response"))
            .is_err());
        drop(gate);
        let mut gate = admission::Admission::open_for_settlement(
            &fixture.control,
            &fixture.validated,
            &fixture.manifest,
            "research-context",
            "monday-research",
        )
        .unwrap();
        gate.publish_receipts_with(|_, bytes| Ok(bytes.to_vec()))
            .unwrap();
        assert_eq!(
            gate.record().unwrap().terminal_pod_uid,
            Some("pod-uid-1".into())
        );
        drop(gate);
        assert_eq!(fixture.usage().consumed_trials, 20);
        assert_eq!(fixture.usage().job_attempts, 1);
    }

    #[test]
    fn study_admission_accepts_distinct_target_and_rejects_tampered_parent_evidence() {
        use alpha_domain::campaign_control::{
            CampaignAttemptOutcomeV1, CampaignAttemptSettlementV1,
        };
        use alpha_domain::campaign_horizon::{
            CampaignLabelHorizonV1, CampaignNextFamilyInputWindowV1, CampaignNextFamilyParentV1,
            CampaignNextFamilyProposalV1, CAMPAIGN_NEXT_FAMILY_PROPOSAL_SCHEMA_V1,
        };
        use alpha_store::campaign_ledger::CampaignDispatchSettlementV1;

        let fixture = AdmissionFixture::new();
        let mut gate = fixture.open();
        gate.prepare().unwrap();
        gate.publish_receipts_with(|_, bytes| Ok(bytes.to_vec()))
            .unwrap();
        gate.claim().unwrap();
        gate.publish_receipts_with(|_, bytes| Ok(bytes.to_vec()))
            .unwrap();
        gate.bind_job("job-uid-1").unwrap();
        gate.publish_receipts_with(|_, bytes| Ok(bytes.to_vec()))
            .unwrap();
        let attempt = gate.reservation.clone();
        drop(gate);
        let mut gate = admission::Admission::open_for_settlement(
            &fixture.control,
            &fixture.validated,
            &fixture.manifest,
            "research-context",
            "monday-research",
        )
        .unwrap();
        gate.settle(&CampaignDispatchSettlementV1 {
            job_uid: "job-uid-1".into(),
            pod_uid: "pod-uid-1".into(),
            settlement: CampaignAttemptSettlementV1 {
                operation_id: attempt.operation_id().unwrap(),
                reservation_sha256: attempt.content_hash().unwrap(),
                evidence_sha256: "b".repeat(64),
                outcome: CampaignAttemptOutcomeV1::NoCandidate,
                consumed_trials: Some(20),
            },
        })
        .unwrap();
        gate.publish_receipts_with(|_, bytes| Ok(bytes.to_vec()))
            .unwrap();
        drop(gate);

        let control = admission::read_control(&fixture.control).unwrap();
        let signed_root: alpha_domain::campaign_control::SignedCampaignRootGrantV1 =
            admission::read_json(&control.signed_root_grant_path).unwrap();
        let store = alpha_store::AlphaStore::open(&control.ledger_path).unwrap();
        let family_receipt = store
            .campaign_family_receipts(&attempt.family_id)
            .unwrap()
            .into_iter()
            .find(|receipt| {
                matches!(
                    &receipt.receipt.event,
                    alpha_store::campaign_ledger::CampaignLedgerEventV1::DispatchSettled {
                        evidence
                    } if evidence.settlement == store
                        .campaign_dispatch_record(&attempt.family_id, &attempt.operation_id().unwrap())
                        .unwrap()
                        .settlement
                        .clone()
                        .unwrap()
                )
            })
            .unwrap();
        let study_snapshot = store
            .campaign_study_snapshot("dispatch-study-budget")
            .unwrap();
        let study_receipt = study_snapshot
            .receipts
            .iter()
            .find(|receipt| {
                matches!(
                    &receipt.receipt.event,
                    alpha_store::campaign_ledger::CampaignStudyLedgerEventV1::AttemptSettled {
                        family_id, settlement, ..
                    } if family_id == &attempt.family_id && settlement.operation_id == attempt.operation_id().unwrap()
                )
            })
            .unwrap();
        let signed_study = store
            .campaign_study_grant("dispatch-study-budget")
            .unwrap()
            .unwrap();
        let horizon = CampaignLabelHorizonV1::canonical();
        let parent = CampaignNextFamilyParentV1 {
            campaign_id: attempt.campaign_id.clone(),
            family_id: attempt.family_id.clone(),
            root_grant_sha256: signed_root.content_sha256.clone(),
            request_sha256: attempt.request_sha256.clone(),
            campaign_result_sha256: "b".repeat(64),
            family_settlement_receipt_sha256: family_receipt.content_sha256.clone(),
            study_settlement_receipt_sha256: study_receipt.content_sha256.clone(),
            study_snapshot_sha256: admission::study_prefix_identity(
                &study_snapshot,
                &study_receipt.content_sha256,
            )
            .unwrap(),
            terminal_job_uid: "job-uid-1".into(),
            terminal_pod_uid: "pod-uid-1".into(),
        };
        let proposal = CampaignNextFamilyProposalV1 {
            schema_version: CAMPAIGN_NEXT_FAMILY_PROPOSAL_SCHEMA_V1.into(),
            study_id: "dispatch-study-budget".into(),
            study_grant_sha256: signed_study.content_sha256.clone(),
            parent,
            target_family_id: "target-family".into(),
            target_root_grant_sha256: "c".repeat(64),
            target_member_sha256: "d".repeat(64),
            target_execution: attempt.execution.clone(),
            target_horizon_sha256: horizon.content_hash().unwrap(),
            target_horizon: horizon,
            target_window: CampaignNextFamilyInputWindowV1 {
                mission_id: "target-mission".into(),
                output_prefix: "target".into(),
                start_received_at_ns: 1,
                end_received_at_ns: 2,
                bucket_ms: 1_000,
                top_depth: 5,
            },
            target_research_plan_sha256: "e".repeat(64),
        };
        assert_ne!(proposal.parent.family_id, proposal.target_family_id);
        admission::validate_parent_settlement_binding(&store, &signed_study, &proposal).unwrap();
        let parent_record = store
            .campaign_dispatch_record(&attempt.family_id, &attempt.operation_id().unwrap())
            .unwrap();
        drop(store);
        let mut advanced_store = alpha_store::AlphaStore::open(&control.ledger_path).unwrap();
        let mut target_attempt = attempt.clone();
        target_attempt.campaign_id = "target-campaign".into();
        target_attempt.generation = 1;
        target_attempt.parent_result_sha256 = Some("b".repeat(64));
        target_attempt.attempt_ordinal = 0;
        target_attempt.request_sha256 = "c".repeat(64);
        target_attempt.policy_revision_id = format!("cex-search-policy-{}", "f".repeat(64));
        advanced_store
            .reserve_campaign_attempt(&parent_record.root, &target_attempt, chrono::Utc::now())
            .unwrap();
        admission::validate_parent_settlement_binding(&advanced_store, &signed_study, &proposal)
            .unwrap();
        let mut substituted_root = proposal.clone();
        substituted_root.parent.root_grant_sha256 = "a".repeat(64);
        assert!(admission::validate_parent_settlement_binding(
            &advanced_store,
            &signed_study,
            &substituted_root,
        )
        .is_err());
        let mut tampered = proposal.clone();
        tampered.parent.campaign_result_sha256 = "c".repeat(64);
        assert!(admission::validate_parent_settlement_binding(
            &advanced_store,
            &signed_study,
            &tampered,
        )
        .is_err());
    }

    #[test]
    fn historical_parent_root_auth_accepts_expired_grants_but_rejects_signature_tamper() {
        use alpha_domain::campaign_control::{
            sign_campaign_root_grant, verify_campaign_root_grant, CampaignExecutionScope,
            CampaignFamilyPolicyV1, CampaignRootBudgetV1, CampaignRootGrantV1, ROOT_GRANT_SCHEMA,
        };
        use chrono::{Duration, Utc};
        use ed25519_dalek::SigningKey;
        use std::collections::BTreeSet;

        let now = Utc::now();
        let key = SigningKey::from_bytes(&[61; 32]);
        let grant = CampaignRootGrantV1 {
            schema_version: ROOT_GRANT_SCHEMA.into(),
            root_id: "expired-parent-root".into(),
            family: CampaignFamilyPolicyV1 {
                family_id: "expired-parent-family".into(),
                definition_sha256: "a".repeat(64),
                max_trials: 100,
            },
            execution_scope: CampaignExecutionScope::PreHoldout,
            execution: alpha_domain::campaign_control::CampaignExecutionBindingV1 {
                campaign_inputs_sha256: "b".repeat(64),
                evaluation_protocol_sha256: "c".repeat(64),
                evaluation_views: alpha_domain::campaign_control::CampaignEvaluationViewsV1 {
                    search_view_sha256: "d".repeat(64),
                    selection_view_sha256: "e".repeat(64),
                    selection_feedback:
                        alpha_domain::campaign_control::CampaignSelectionFeedbackV1::IndependentSelectionWithheld,
                },
                source_revision: "f".repeat(40),
                runner_image: format!("registry/runner@sha256:{}", "1".repeat(64)),
                controller_image: format!("registry/controller@sha256:{}", "2".repeat(64)),
                job_cpu_millis: 1,
                job_memory_mib: 1,
            },
            allowed_policy_revision_ids: BTreeSet::from([format!(
                "cex-search-policy-{}",
                "3".repeat(64)
            )]),
            max_follow_ups: 1,
            budget: CampaignRootBudgetV1 {
                max_trials: 100,
                max_job_attempts: 1,
                max_job_seconds: 1,
                max_llm_tokens: 0,
            },
            valid_from: now - Duration::hours(2),
            expires_at: now - Duration::hours(1),
        };
        let signed = sign_campaign_root_grant(grant, "expired-operator".into(), &key).unwrap();
        let trusted =
            std::collections::BTreeMap::from([("expired-operator".into(), key.verifying_key())]);
        assert!(verify_campaign_root_grant(
            &signed,
            &trusted,
            signed.grant.expires_at - Duration::seconds(1),
        )
        .is_ok());
        let mut tampered = signed.clone();
        tampered.signature_hex = "00".into();
        assert!(verify_campaign_root_grant(
            &tampered,
            &trusted,
            tampered.grant.expires_at - Duration::seconds(1),
        )
        .is_err());
    }

    #[test]
    fn target_admission_accepts_distinct_study_member_and_rejects_parent_root_substitution() {
        use alpha_domain::campaign_control::{
            CampaignAttemptOutcomeV1, CampaignAttemptSettlementV1,
        };
        use alpha_domain::campaign_horizon::{
            CampaignNextFamilyInputWindowV1, CampaignNextFamilyParentV1,
            CampaignNextFamilyProposalV1, CAMPAIGN_NEXT_FAMILY_PROPOSAL_SCHEMA_V1,
        };
        use alpha_store::campaign_ledger::CampaignDispatchSettlementV1;

        let fixture = AdmissionFixture::new();
        let mut parent_gate = fixture.open();
        parent_gate.prepare().unwrap();
        parent_gate
            .publish_receipts_with(|_, bytes| Ok(bytes.to_vec()))
            .unwrap();
        parent_gate.claim().unwrap();
        parent_gate
            .publish_receipts_with(|_, bytes| Ok(bytes.to_vec()))
            .unwrap();
        parent_gate.bind_job("parent-job").unwrap();
        parent_gate
            .publish_receipts_with(|_, bytes| Ok(bytes.to_vec()))
            .unwrap();
        let parent_attempt = parent_gate.reservation.clone();
        drop(parent_gate);
        let mut parent_settlement = admission::Admission::open_for_settlement(
            &fixture.control,
            &fixture.validated,
            &fixture.manifest,
            "research-context",
            "monday-research",
        )
        .unwrap();
        parent_settlement
            .settle(&CampaignDispatchSettlementV1 {
                job_uid: "parent-job".into(),
                pod_uid: "parent-pod".into(),
                settlement: CampaignAttemptSettlementV1 {
                    operation_id: parent_attempt.operation_id().unwrap(),
                    reservation_sha256: parent_attempt.content_hash().unwrap(),
                    evidence_sha256: "b".repeat(64),
                    outcome: CampaignAttemptOutcomeV1::NoCandidate,
                    consumed_trials: Some(20),
                },
            })
            .unwrap();
        parent_settlement
            .publish_receipts_with(|_, bytes| Ok(bytes.to_vec()))
            .unwrap();
        drop(parent_settlement);

        let control = admission::read_control(&fixture.control).unwrap();
        let store = alpha_store::AlphaStore::open(&control.ledger_path).unwrap();
        let signed_root: alpha_domain::campaign_control::SignedCampaignRootGrantV1 =
            admission::read_json(&control.signed_root_grant_path).unwrap();
        let signed_study = store
            .campaign_study_grant("dispatch-study-budget")
            .unwrap()
            .unwrap();
        let target_member = signed_study
            .grant
            .members
            .iter()
            .find(|member| member.family_id == "dispatch-target")
            .unwrap();
        let parent_family_receipt = store
            .campaign_family_receipts(&parent_attempt.family_id)
            .unwrap()
            .into_iter()
            .find(|receipt| {
                matches!(
                    &receipt.receipt.event,
                    alpha_store::campaign_ledger::CampaignLedgerEventV1::DispatchSettled {
                        evidence
                    } if evidence.settlement.evidence_sha256 == "b".repeat(64)
                )
            })
            .unwrap();
        let snapshot = store
            .campaign_study_snapshot("dispatch-study-budget")
            .unwrap();
        let study_settlement = snapshot
            .receipts
            .iter()
            .find(|receipt| {
                matches!(
                    &receipt.receipt.event,
                    alpha_store::campaign_ledger::CampaignStudyLedgerEventV1::AttemptSettled {
                        family_id, ..
                    } if family_id == &parent_attempt.family_id
                )
            })
            .unwrap();
        let parent = CampaignNextFamilyParentV1 {
            campaign_id: parent_attempt.campaign_id.clone(),
            family_id: parent_attempt.family_id.clone(),
            root_grant_sha256: signed_root.content_sha256.clone(),
            request_sha256: parent_attempt.request_sha256.clone(),
            campaign_result_sha256: "b".repeat(64),
            family_settlement_receipt_sha256: parent_family_receipt.content_sha256.clone(),
            study_settlement_receipt_sha256: study_settlement.content_sha256.clone(),
            study_snapshot_sha256: admission::study_prefix_identity(
                &snapshot,
                &study_settlement.content_sha256,
            )
            .unwrap(),
            terminal_job_uid: "parent-job".into(),
            terminal_pod_uid: "parent-pod".into(),
        };
        let materialization: Value =
            serde_json::from_slice(&std::fs::read(&fixture.inputs.materialization_path).unwrap())
                .unwrap();
        let segments = materialization["source_segments"].as_array().unwrap();
        let target_window = CampaignNextFamilyInputWindowV1 {
            mission_id: materialization["mission_id"].as_str().unwrap().into(),
            output_prefix: "target".into(),
            start_received_at_ns: segments
                .iter()
                .filter_map(|segment| segment["start_received_at_ns"].as_u64())
                .min()
                .unwrap(),
            end_received_at_ns: segments
                .iter()
                .filter_map(|segment| segment["end_received_at_ns"].as_u64())
                .max()
                .unwrap(),
            bucket_ms: materialization["bucket_ms"].as_u64().unwrap(),
            top_depth: materialization["top_depth"].as_u64().unwrap() as usize,
        };
        let plan = fixture.validated.submission.request.research_plan.clone();
        let horizon = plan.label_horizon.clone().unwrap();
        let proposal = CampaignNextFamilyProposalV1 {
            schema_version: CAMPAIGN_NEXT_FAMILY_PROPOSAL_SCHEMA_V1.into(),
            study_id: "dispatch-study-budget".into(),
            study_grant_sha256: signed_study.content_sha256.clone(),
            parent,
            target_family_id: target_member.family_id.clone(),
            target_root_grant_sha256: target_member.root_grant_sha256.clone(),
            target_member_sha256: target_member.content_hash().unwrap(),
            target_execution: target_member.execution.clone(),
            target_horizon_sha256: horizon.content_hash().unwrap(),
            target_horizon: horizon,
            target_window,
            target_research_plan_sha256: plan.content_hash().unwrap(),
        };
        proposal.validate().unwrap();
        let mut target_submission = fixture.validated.submission.clone();
        target_submission.request.study_proposal = Some(proposal.clone());
        target_submission.request.campaign_id =
            crate::mission_campaign::expected_campaign_id(&target_submission.request).unwrap();
        let target_validated = validate_submission(target_submission).unwrap();
        let target_manifest = render_manifest(&target_validated, "monday-research").unwrap();
        let mut target_control: Value =
            serde_json::from_slice(&std::fs::read(&fixture.control).unwrap()).unwrap();
        target_control["signed_root_grant_path"] = json!("target-grant.json");
        target_control["approval_id"] = json!("dispatch-target-approval");
        target_control["campaign_inputs_path"] = json!("campaign-inputs.json");
        let target_control_path = fixture.inputs._root.path().join("target-control.json");
        std::fs::write(
            &target_control_path,
            serde_json::to_vec(&target_control).unwrap(),
        )
        .unwrap();

        let mut target_gate = admission::Admission::open(
            &target_control_path,
            &target_validated,
            &target_manifest,
            "research-context",
            "monday-research",
        )
        .unwrap();
        target_gate.prepare().unwrap();
        target_gate
            .publish_receipts_with(|_, bytes| Ok(bytes.to_vec()))
            .unwrap();
        target_gate.claim().unwrap();
        target_gate
            .publish_receipts_with(|_, bytes| Ok(bytes.to_vec()))
            .unwrap();
        target_gate.bind_job("target-job").unwrap();
        target_gate
            .publish_receipts_with(|_, bytes| Ok(bytes.to_vec()))
            .unwrap();
        drop(target_gate);
        let mut target_settlement = admission::Admission::open_for_settlement(
            &target_control_path,
            &target_validated,
            &target_manifest,
            "research-context",
            "monday-research",
        )
        .unwrap();
        target_settlement
            .settle(&CampaignDispatchSettlementV1 {
                job_uid: "target-job".into(),
                pod_uid: "target-pod".into(),
                settlement: CampaignAttemptSettlementV1 {
                    operation_id: target_settlement.reservation.operation_id().unwrap(),
                    reservation_sha256: target_settlement.reservation.content_hash().unwrap(),
                    evidence_sha256: "c".repeat(64),
                    outcome: CampaignAttemptOutcomeV1::NoCandidate,
                    consumed_trials: Some(20),
                },
            })
            .unwrap();
        target_settlement
            .publish_receipts_with(|_, bytes| Ok(bytes.to_vec()))
            .unwrap();
        drop(target_settlement);

        let tampered_control_path = fixture
            .inputs
            ._root
            .path()
            .join("tampered-target-control.json");
        let mut tampered_control = target_control.clone();
        tampered_control["campaign_inputs_path"] = json!("tampered-inputs.json");
        std::fs::write(
            &tampered_control_path,
            serde_json::to_vec(&tampered_control).unwrap(),
        )
        .unwrap();
        assert!(admission::Admission::open(
            &tampered_control_path,
            &target_validated,
            &target_manifest,
            "research-context",
            "monday-research",
        )
        .is_err());

        let mut tampered_submission = target_validated.submission.clone();
        tampered_submission
            .request
            .study_proposal
            .as_mut()
            .unwrap()
            .parent
            .root_grant_sha256 = target_member.root_grant_sha256.clone();
        let tampered_validated = validate_submission(tampered_submission).unwrap();
        let tampered_manifest = render_manifest(&tampered_validated, "monday-research").unwrap();
        assert!(admission::Admission::open(
            &target_control_path,
            &tampered_validated,
            &tampered_manifest,
            "research-context",
            "monday-research",
        )
        .is_err());
    }

    #[test]
    fn dispatch_receipt_transport_failure_and_wrong_object_do_not_acknowledge_or_start() {
        let fixture = AdmissionFixture::new();
        let mut gate = fixture.open();
        gate.prepare().unwrap();
        assert!(gate
            .publish_receipts_with(|_, _| anyhow::bail!("lost readback response"))
            .is_err());
        assert!(gate.claim().is_err());
        drop(gate);
        let mut control: Value =
            serde_json::from_slice(&std::fs::read(&fixture.control).unwrap()).unwrap();
        let first = control["receipt_access"]
            .as_object_mut()
            .unwrap()
            .values_mut()
            .next()
            .unwrap();
        first["readback_url"] =
            json!("https://other.oss-ap-northeast-1-internal.aliyuncs.com/receipt.json");
        std::fs::write(&fixture.control, serde_json::to_vec(&control).unwrap()).unwrap();
        let mut gate = fixture.open();
        let mut transferred = false;
        assert!(gate
            .publish_receipts_with(|_, bytes| {
                transferred = true;
                Ok(bytes.to_vec())
            })
            .is_err());
        assert!(
            !transferred,
            "foreign bucket/key must fail before any transport"
        );
        assert!(gate.claim().is_err());
    }

    #[test]
    fn study_receipt_publication_requires_exact_control_mapping() {
        let fixture = AdmissionFixture::new();
        let mut gate = fixture.open();
        gate.prepare().unwrap();
        let study_key = "research/campaign-ledger/study-id=dispatch-study-budget/sequence=00000000000000000001/receipt.json";
        let mut control: Value =
            serde_json::from_slice(&std::fs::read(&fixture.control).unwrap()).unwrap();
        control["receipt_access"]
            .as_object_mut()
            .unwrap()
            .remove(study_key)
            .expect("fixture must provide Study registration access");
        std::fs::write(&fixture.control, serde_json::to_vec(&control).unwrap()).unwrap();
        drop(gate);
        let mut gate = fixture.open();
        let mut transferred = 0;
        let error = gate
            .publish_receipts_with(|_, bytes| {
                transferred += 1;
                Ok(bytes.to_vec())
            })
            .expect_err("missing Study receipt access must retain the reservation");
        assert!(error.to_string().contains("Study receipt access"));
        assert_eq!(transferred, 3, "family receipts are published first");
        assert!(gate.claim().is_err());

        let fixture = AdmissionFixture::new();
        let mut gate = fixture.open();
        gate.prepare().unwrap();
        let mut control: Value =
            serde_json::from_slice(&std::fs::read(&fixture.control).unwrap()).unwrap();
        control["receipt_access"][study_key]["readback_url"] =
            json!("https://other.oss-ap-northeast-1-internal.aliyuncs.com/receipt.json");
        std::fs::write(&fixture.control, serde_json::to_vec(&control).unwrap()).unwrap();
        drop(gate);
        let mut gate = fixture.open();
        let mut transferred = 0;
        let error = gate
            .publish_receipts_with(|_, bytes| {
                transferred += 1;
                Ok(bytes.to_vec())
            })
            .expect_err("Study receipt URL drift must retain the reservation");
        assert!(error.to_string().contains("Campaign receipt URL"));
        assert_eq!(
            transferred, 3,
            "foreign Study URL is rejected before transport"
        );
        assert!(gate.claim().is_err());
    }

    fn valid_submission() -> MissionDispatchSubmission {
        MissionDispatchSubmission {
            attempt_id: "attempt1".to_string(),
            image: "registry/research-runner@sha256:1111111111111111111111111111111111111111111111111111111111111111".to_string(),
            request: valid_request_for_tests(),
        }
    }
}
