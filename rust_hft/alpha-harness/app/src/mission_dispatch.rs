mod admission;
pub(crate) mod final_authority;
pub(crate) mod controller;
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
use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;

const MAX_SUBMISSION_BYTES: u64 = 1024 * 1024;
const ACTIVE_DEADLINE_SECONDS: u64 = 21_608;

#[derive(Debug, Deserialize, Serialize)]
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
    terminal::settle(args)
}

pub fn submit(args: MissionDispatchSubmitArgs) -> anyhow::Result<()> {
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
        .or_else(|| std::env::var_os("MONDAY_CAMPAIGN_CONTROL").map(Into::into))
        .context("Campaign dispatch requires --control or MONDAY_CAMPAIGN_CONTROL")?;
    let mut admission = admission::Admission::open(
        &control,
        &validated,
        &manifest,
        &args.context,
        &args.namespace,
    )?;
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
    validate_dns_label("attempt id", &submission.attempt_id)?;
    validate_request(&submission.request)?;
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

fn render_manifest(validated: &ValidatedSubmission, namespace: &str) -> anyhow::Result<Value> {
    validate_dns_label("namespace", namespace)?;
    let attempt_id = validated.submission.attempt_id.clone();
    let campaign_id = validated.submission.request.campaign_id.clone();
    let labels = json!({
        "app.kubernetes.io/name": "monday-alpha-campaign",
        "app.kubernetes.io/part-of": "monday",
        "research.monday/campaign-id": &campaign_id,
    });
    let annotations = json!({
        "research.monday/attempt-id": &attempt_id,
        "research.monday/campaign-id": &campaign_id,
        "research.monday/request-sha256": &validated.request_sha256,
        "research.monday/submission-identity-sha256": &validated.submission_identity_sha256,
        "research.monday/image-digest": &validated.image_digest,
        "research.monday/lane": "cex_research_campaign",
    });
    Ok(json!({
        "apiVersion": "v1",
        "kind": "List",
        "items": [
            {
                "apiVersion": "v1",
                "kind": "Secret",
                "metadata": {
                    "name": validated.secret_name,
                    "namespace": namespace,
                    "labels": labels,
                    "annotations": {
                        "research.monday/attempt-id": &attempt_id,
                        "research.monday/campaign-id": &campaign_id,
                        "research.monday/request-sha256": &validated.request_sha256,
                    }
                },
                "type": "Opaque",
                "immutable": true,
                "stringData": {
                    "campaign.json": validated.request_json,
                },
            },
            {
                "apiVersion": "batch/v1",
                "kind": "Job",
                "metadata": {
                    "name": validated.job_name,
                    "namespace": namespace,
                    "labels": labels,
                    "annotations": annotations,
                },
                "spec": {
                    "suspend": true,
                    "parallelism": 1,
                    "completions": 1,
                    "backoffLimit": 0,
                    "activeDeadlineSeconds": ACTIVE_DEADLINE_SECONDS,
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
                                "image": validated.submission.image,
                                "imagePullPolicy": "IfNotPresent",
                                "command": ["/usr/local/bin/alpha-harness"],
                                "args": [
                                    "mission",
                                    "campaign-execute",
                                    "--pre-holdout",
                                    "--work-dir", "/work",
                                    "--campaign-id", &campaign_id,
                                    "--image-identity", &validated.image_digest,
                                    "--request", "/inputs/campaign.json",
                                    "--request-sha256", &validated.request_sha256
                                ],
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
                                        "secretName": validated.secret_name,
                                        "items": [{ "key": "campaign.json", "path": "campaign.json" }]
                                    }
                                }
                            ]
                        }
                    }
                }
            }
        ]
    }))
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
            use alpha_store::{AlphaStore, ApprovalRecord};
            use chrono::{TimeDelta, Utc};
            use ed25519_dalek::SigningKey;
            use std::collections::BTreeSet;
            let inputs = crate::mission_render::tests::Fixture::canonical();
            let mut submission = valid_submission();
            submission.request = crate::mission_campaign::request_for_materialization_for_tests(
                &inputs.materialization_path,
            );
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
                allowed_policy_revision_ids: BTreeSet::from([inspection["policy_revision_id"]
                    .as_str()
                    .unwrap()
                    .into()]),
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
            let root = inputs._root.path();
            std::fs::write(
                root.join("grant.json"),
                serde_json::to_vec(&signed).unwrap(),
            )
            .unwrap();
            std::fs::write(
                root.join("keys.json"),
                serde_json::to_vec(
                    &json!({ "operator": hex::encode(signing_key.verifying_key().as_bytes()) }),
                )
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
            drop(store);
            let origin =
                reqwest::Url::parse(&validated.submission.request.campaign_result_readback_url)
                    .unwrap()
                    .origin()
                    .ascii_serialization();
            let access = (1..=12).map(|sequence| {
                let key = format!("research/campaign-ledger/family-id=dispatch-study/sequence={sequence:020}/receipt.json");
                let url = format!("{origin}/{key}?signature=fixture-only");
                (key, json!({"put_url": url, "readback_url": url}))
            }).collect::<serde_json::Map<String, Value>>();
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
        assert!(error.to_string().contains("approved Binance USD-M BTCUSDT"));
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
        gate.publish_receipts_with(|_, bytes| Ok(bytes.to_vec()))
            .unwrap();
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

    fn valid_submission() -> MissionDispatchSubmission {
        MissionDispatchSubmission {
            attempt_id: "attempt1".to_string(),
            image: "registry/research-runner@sha256:1111111111111111111111111111111111111111111111111111111111111111".to_string(),
            request: valid_request_for_tests(),
        }
    }
}
