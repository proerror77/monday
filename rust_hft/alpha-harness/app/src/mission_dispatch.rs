use crate::{
    cli::{print_json, MissionDispatchSubmitArgs},
    data_mission,
    mission_campaign::{serialize_request, validate_request, CampaignRequest},
    mission_runner::normalized_sha256,
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
const ACTIVE_DEADLINE_SECONDS: u64 = 3600;

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

impl SubmissionObjectState {
    fn should_cleanup(self) -> bool {
        matches!(self, Self::Created)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CleanupState {
    job: SubmissionObjectState,
    secret: SubmissionObjectState,
    release_patch_sent: bool,
}

impl CleanupState {
    fn should_cleanup_job(self) -> bool {
        !self.release_patch_sent && self.job.should_cleanup()
    }

    fn should_cleanup_secret(self) -> bool {
        !self.release_patch_sent && self.secret.should_cleanup()
    }
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
    let manifest = render_manifest(validated, &args.namespace)?;
    let kubectl = kubectl_binary();
    let mut cleanup_state = CleanupState::default();

    let result = (|| -> anyhow::Result<()> {
        let expected_job = &manifest["items"][1];
        let job = create_or_adopt_job(
            &kubectl,
            &args.context,
            &args.namespace,
            expected_job,
            &job_name,
            &request_sha256,
            &mut cleanup_state.job,
        )?;
        let job_uid = job["metadata"]["uid"]
            .as_str()
            .context("CEX Campaign Job readback is missing its UID")?;
        let secret = secret_with_owner(&manifest["items"][0], &job_name, job_uid)?;
        let secret = create_or_adopt_secret(
            &kubectl,
            &args.context,
            &args.namespace,
            &secret,
            &secret_name,
            &request_json,
            &job_name,
            job_uid,
            &mut cleanup_state.secret,
        )?;
        validate_secret_readback(
            &secret,
            &secret_name,
            &request_sha256,
            request_json.as_bytes(),
            &job_name,
            job_uid,
        )?;
        let release_patch_json = serde_json::to_string(&release_job_patch())?;
        cleanup_state.release_patch_sent = true;
        let release_output = kubectl_with_input(
            &kubectl,
            &args.context,
            &args.namespace,
            [
                "patch",
                "job",
                &job_name,
                "--type=merge",
                "--patch",
                &release_patch_json,
                "-o",
                "json",
            ],
            &[],
        )?;
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
        let cleanup = delete_created_campaign_objects(
            &kubectl,
            &args.context,
            &args.namespace,
            &job_name,
            &secret_name,
            cleanup_state,
        );
        return match cleanup {
            Ok(()) => Err(error),
            Err(cleanup_error) => Err(error.context(format!(
                "incomplete submission retained for reconciliation: {cleanup_error:#}"
            ))),
        };
    }

    print_json(&json!({
        "status": "submitted",
        "context": args.context,
        "namespace": args.namespace,
        "campaign_id": campaign_id,
        "request_sha256": request_sha256,
        "job_name": job_name,
    }))
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
        ["create", "-f", "-"],
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
                ["get", "job", job_name, "-o", "json"],
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
        ["create", "-f", "-"],
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
        ["get", "secret", secret_name, "-o", "json"],
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
        ["get", "job", job_name, "-o", "json"],
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

fn release_job_patch() -> Value {
    json!({
        "spec": {
            "suspend": false,
        }
    })
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
    for chunk in value.as_bytes().chunks_exact(4) {
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

fn delete_campaign_secret(
    kubectl: &std::path::Path,
    context: &str,
    namespace: &str,
    secret_name: &str,
) -> anyhow::Result<()> {
    let output = kubectl_with_input(
        kubectl,
        context,
        namespace,
        ["delete", "secret", secret_name, "--ignore-not-found=true"],
        &[],
    )?;
    ensure_kubectl_success(output, "clean up incomplete CEX Campaign input Secret")?;
    Ok(())
}

fn delete_created_campaign_objects(
    kubectl: &std::path::Path,
    context: &str,
    namespace: &str,
    job_name: &str,
    secret_name: &str,
    cleanup_state: CleanupState,
) -> anyhow::Result<()> {
    if cleanup_state.should_cleanup_job() {
        let output = kubectl_with_input(
            kubectl,
            context,
            namespace,
            ["delete", "job", job_name, "--ignore-not-found=true"],
            &[],
        )?;
        ensure_kubectl_success(output, "clean up incomplete CEX Campaign submission")?;
    }
    if cleanup_state.should_cleanup_secret() {
        delete_campaign_secret(kubectl, context, namespace, secret_name)?;
    }
    Ok(())
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

fn render_manifest(validated: ValidatedSubmission, namespace: &str) -> anyhow::Result<Value> {
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
    if image != image.trim() || image.chars().any(char::is_control) {
        bail!("mission image must not contain surrounding whitespace or control characters");
    }
    let (repository, digest) = image
        .split_once('@')
        .context("mission image must be pinned by @sha256 digest")?;
    if digest.is_empty() || digest.contains('@') {
        bail!("mission image must be pinned by a canonical @sha256 digest");
    }
    let digest = digest
        .strip_prefix("sha256:")
        .context("mission image must be pinned by @sha256 digest")?;
    validate_image_repository(repository)?;
    normalized_sha256("mission image", digest)
}

fn validate_image_repository(repository: &str) -> anyhow::Result<()> {
    if repository.is_empty()
        || repository.starts_with('/')
        || repository.ends_with('/')
        || repository.contains("//")
        || repository.chars().any(char::is_whitespace)
        || repository.chars().any(|ch| ch.is_ascii_uppercase())
    {
        bail!("mission image repository must be a canonical OCI reference");
    }
    let segments = repository.split('/').collect::<Vec<_>>();
    if segments.iter().any(|segment| segment.is_empty()) {
        bail!("mission image repository must be a canonical OCI reference");
    }
    let host_prefix = segments.len() > 1
        && (segments[0].contains('.') || segments[0].contains(':') || segments[0] == "localhost");
    if host_prefix && !segments[0].chars().all(is_registry_host_char) {
        bail!("mission image repository must be a canonical OCI reference");
    }
    let name_segments = if host_prefix {
        &segments[1..]
    } else {
        &segments[..]
    };
    if name_segments.is_empty()
        || name_segments
            .iter()
            .any(|segment| !is_repository_segment(segment))
    {
        bail!("mission image repository must be a canonical OCI reference");
    }
    Ok(())
}

fn is_registry_host_char(ch: char) -> bool {
    ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '.' | ':' | '-')
}

fn is_repository_segment(segment: &str) -> bool {
    !segment.is_empty()
        && !segment.starts_with(['.', '-', '_'])
        && !segment.ends_with(['.', '-', '_'])
        && segment.chars().all(|ch| {
            ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '.' | '_' | '-')
        })
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
            render_manifest(validate_submission(valid_submission()).unwrap(), "monday").unwrap();
        let job = &rendered["items"][1];

        assert_eq!(job["spec"]["suspend"], true);
        assert_eq!(job["spec"]["backoffLimit"], 0);
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
            render_manifest(validate_submission(valid_submission()).unwrap(), "monday").unwrap();
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

    #[test]
    fn release_patch_clears_job_suspend() {
        assert_eq!(release_job_patch()["spec"]["suspend"], false);
    }

    #[test]
    fn job_readback_requires_expected_suspend_state() {
        let expected_job =
            render_manifest(validate_submission(valid_submission()).unwrap(), "monday").unwrap();
        let job = json!({
            "metadata": {
                "name": expected_job["items"][1]["metadata"]["name"].clone(),
                "annotations": { "research.monday/request-sha256": "request-sha" },
            },
            "spec": {
                "suspend": true,
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
            render_manifest(validate_submission(valid_submission()).unwrap(), "monday").unwrap();
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
    fn cleanup_only_targets_created_objects() {
        assert!(!SubmissionObjectState::Unknown.should_cleanup());
        assert!(!SubmissionObjectState::Adopted.should_cleanup());
        assert!(SubmissionObjectState::Created.should_cleanup());
    }

    #[test]
    fn release_phase_disables_cleanup_even_for_created_objects() {
        let cleanup_state = CleanupState {
            job: SubmissionObjectState::Created,
            secret: SubmissionObjectState::Created,
            release_patch_sent: true,
        };

        assert!(!cleanup_state.should_cleanup_job());
        assert!(!cleanup_state.should_cleanup_secret());
    }

    #[test]
    fn released_and_mismatched_jobs_do_not_become_cleanup_targets() {
        let expected_job =
            render_manifest(validate_submission(valid_submission()).unwrap(), "monday").unwrap();
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
        assert!(!adopted_state.should_cleanup());

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
        assert!(!mismatched_state.should_cleanup());

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
        assert!(!mismatched_released_state.should_cleanup());
    }

    #[test]
    fn released_exact_job_is_adoptable_for_retry() {
        let expected_job =
            render_manifest(validate_submission(valid_submission()).unwrap(), "monday").unwrap();
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
        assert!(!released_state.should_cleanup());
    }

    #[test]
    fn matching_secret_conflict_is_adoptable_without_cleanup() {
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

        let adopted_state = SubmissionObjectState::Adopted;
        assert!(!adopted_state.should_cleanup());
    }

    fn valid_submission() -> MissionDispatchSubmission {
        MissionDispatchSubmission {
            attempt_id: "attempt1".to_string(),
            image: "registry/research-runner@sha256:1111111111111111111111111111111111111111111111111111111111111111".to_string(),
            request: valid_request_for_tests(),
        }
    }
}
