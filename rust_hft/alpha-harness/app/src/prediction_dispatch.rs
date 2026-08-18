use crate::{
    cli::{
        print_json, PredictionDispatchRenderArgs, PredictionDispatchStatusArgs,
        PredictionDispatchSubmitArgs,
    },
    mission_runner::{configured_sibling_binary, fetch_to_file, normalized_sha256},
};
use anyhow::{bail, Context};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

const MAX_SUBMISSION_BYTES: u64 = 1024 * 1024;
const MAX_ADMISSION_RESPONSE_BYTES: u64 = 16 * 1024;
const MAX_ADMISSION_REQUEST_BYTES: usize = 32 * 1024;
const MAX_ADMISSION_MISSION_BYTES: u64 = 8 * 1024;
const SNAPSHOT_ADMISSION_TIMEOUT: Duration = Duration::from_secs(30);
const RESOURCE_PROFILE: &str = "standard-v1";
const ACTIVE_DEADLINE_SECONDS: u64 = 1800;
const SNAPSHOT_ADMISSION_SCHEMA_VERSION: &str = "monday.prediction.snapshot_admission.v2";

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum MissionTaskKind {
    SettlementProbability,
    UpExecution,
    DownExecution,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum MissionTokenSide {
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum MissionRunMode {
    PipelineSmoke,
    ResearchTrial,
}

impl MissionRunMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::PipelineSmoke => "pipeline_smoke",
            Self::ResearchTrial => "research_trial",
        }
    }
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct MissionTaskIdentity {
    kind: MissionTaskKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    side: Option<MissionTokenSide>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prediction_horizon_secs: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AuthenticatedPartitionView {
    common_time_boundary_ms: i64,
    train_market_ids: Vec<String>,
    crossing_excluded_market_ids: Vec<String>,
    held_out_market_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PredictionSubmission {
    attempt_id: String,
    mission_id: String,
    image: String,
    evaluator_version: String,
    resource_profile: String,
    mission_url: String,
    mission_sha256: String,
    snapshot_url: String,
    snapshot_sha256: String,
    snapshot_contract_id: String,
    result_put_url: String,
    result_readback_url: String,
    #[serde(default)]
    resume_url: Option<String>,
    #[serde(default)]
    resume_sha256: Option<String>,
    catalog_partition_artifact: CatalogPartitionArtifactRef,
    compiler_source_identity: String,
    build_input_identity: String,
    task_capability: String,
    task: MissionTaskIdentity,
    cohort_partition_id: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CatalogPartitionArtifactRef {
    path: String,
    artifact_sha256: String,
    payload_sha256: String,
}

#[derive(Serialize)]
struct SnapshotAdmissionRequest<'a> {
    schema_version: &'static str,
    catalog_partition_artifact: &'a CatalogPartitionArtifactRef,
    compiler_source_identity: &'a str,
    compiler_image_identity: String,
    build_input_identity: &'a str,
    task_capability: &'a str,
    task: &'a MissionTaskIdentity,
    cohort_partition_id: &'a str,
    mission_id: &'a str,
    snapshot_contract_id: &'a str,
    mission_json: &'a str,
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "lowercase", deny_unknown_fields)]
// ponytail: parsed once per dispatch; box variants only if admission throughput matters.
#[allow(clippy::large_enum_variant)]
enum SnapshotAdmissionResponse {
    Admitted {
        schema_version: String,
        snapshot_contract_id: String,
        snapshot_digest: String,
        partition_digest: String,
        policy_identity: String,
        task_capability: String,
        task: MissionTaskIdentity,
        cohort_partition_id: String,
        cohort_manifest_id: String,
        partition_view: AuthenticatedPartitionView,
        immutable_image_identity: String,
    },
    Rejected {
        schema_version: String,
        rejection: String,
    },
}

struct SnapshotAdmission {
    snapshot_contract_id: String,
    snapshot_digest: String,
    partition_digest: String,
    policy_identity: String,
    task_capability: String,
    task: MissionTaskIdentity,
    cohort_partition_id: String,
    cohort_manifest_id: String,
    partition_view: AuthenticatedPartitionView,
    immutable_image_identity: String,
}

struct ValidatedSubmission {
    submission: PredictionSubmission,
    mission_sha256: String,
    snapshot_sha256: String,
    resume_sha256: Option<String>,
    image_digest: String,
    mission_object: String,
    snapshot_object: String,
    result_object: String,
    result_readback_object: String,
    result_identity_sha256: String,
    result_identity_label: String,
    job_name: String,
    secret_name: String,
}

struct AdmittedSubmission {
    validated: ValidatedSubmission,
    admission: SnapshotAdmission,
    run_mode: MissionRunMode,
}

enum AdmissionDecision {
    Admitted(Box<AdmittedSubmission>),
    Rejected(String),
}

// ponytail: created once per dispatch; box variants only if admission throughput matters.
#[allow(clippy::large_enum_variant)]
enum SnapshotAdmissionDecision {
    Admitted(SnapshotAdmission),
    Rejected(String),
}

#[derive(Debug)]
struct RenderedSubmission {
    manifest: Value,
    job_name: String,
    secret_name: String,
    result_identity_sha256: String,
}

#[derive(Deserialize)]
struct StatusJob {
    metadata: StatusMetadata,
    #[serde(default)]
    status: StatusJobState,
}

#[derive(Deserialize)]
struct StatusMetadata {
    name: String,
    namespace: String,
    uid: String,
    #[serde(default)]
    annotations: BTreeMap<String, String>,
}

#[derive(Default, Deserialize)]
struct StatusJobState {
    #[serde(default)]
    conditions: Vec<StatusCondition>,
}

#[derive(Deserialize)]
struct StatusPodList {
    items: Vec<StatusPod>,
}

#[derive(Deserialize)]
struct StatusPod {
    metadata: StatusPodMetadata,
    #[serde(default)]
    status: StatusPodState,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StatusPodMetadata {
    #[serde(default)]
    owner_references: Vec<StatusOwnerReference>,
}

#[derive(Deserialize)]
struct StatusOwnerReference {
    uid: String,
    name: String,
    kind: String,
    #[serde(default)]
    controller: bool,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StatusPodState {
    #[serde(default)]
    conditions: Vec<StatusCondition>,
    #[serde(default)]
    container_statuses: Vec<StatusContainer>,
}

#[derive(Deserialize)]
struct StatusCondition {
    #[serde(rename = "type")]
    kind: String,
    status: String,
}

#[derive(Deserialize)]
struct StatusContainer {
    state: StatusContainerState,
}

#[derive(Deserialize)]
struct StatusContainerState {
    running: Option<Value>,
    terminated: Option<Value>,
}

#[derive(Deserialize)]
struct StatusEvidence {
    lane: String,
    mission_id: String,
    mission_sha256: String,
    snapshot_archive_sha256: String,
    partition_digest: String,
    policy_identity: String,
    task_capability: String,
    image_identity: String,
    run_mode: String,
    bundle_sha256: String,
    readback_bundle_sha256: String,
}

#[derive(Serialize)]
struct PredictionStatus {
    job_name: String,
    namespace: String,
    mission_id: String,
    mission_sha256: String,
    snapshot_sha256: String,
    submitted: bool,
    scheduled: bool,
    image_ready: bool,
    snapshot_ready: Option<bool>,
    evaluator_started: Option<bool>,
    completed: bool,
}

pub fn render(args: PredictionDispatchRenderArgs) -> anyhow::Result<()> {
    let submission = load_submission(&args.submission)?;
    let validated = validate_submission(submission)?;
    let sibling = configured_sibling_binary(
        "MONDAY_PREDICTION_SNAPSHOT_BIN",
        "monday-prediction-snapshot",
    )?;
    let admitted = match admit_submission(validated, &sibling)? {
        AdmissionDecision::Admitted(admitted) => *admitted,
        AdmissionDecision::Rejected(rejection) => return report_admission_rejection(&rejection),
    };
    let rendered = render_admitted_submission(admitted, &args.namespace)?;
    print_json(&json!({
        "job_name": rendered.job_name,
        "secret_name": rendered.secret_name,
        "result_identity_sha256": rendered.result_identity_sha256,
        "manifest": rendered.manifest,
    }))
}

pub fn submit(args: PredictionDispatchSubmitArgs) -> anyhow::Result<()> {
    validate_cluster_target(&args.context, &args.namespace)?;
    let submission = load_submission(&args.submission)?;
    let validated = validate_submission(submission)?;
    let sibling = configured_sibling_binary(
        "MONDAY_PREDICTION_SNAPSHOT_BIN",
        "monday-prediction-snapshot",
    )?;
    submit_validated_submission(args, validated, &sibling, &kubectl_binary())
}

#[cfg(test)]
fn submit_with_binaries(
    args: PredictionDispatchSubmitArgs,
    sibling: &Path,
    kubectl: &Path,
) -> anyhow::Result<()> {
    validate_cluster_target(&args.context, &args.namespace)?;
    let submission = load_submission(&args.submission)?;
    let validated = validate_submission(submission)?;
    submit_validated_submission(args, validated, sibling, kubectl)
}

fn submit_validated_submission(
    args: PredictionDispatchSubmitArgs,
    validated: ValidatedSubmission,
    sibling: &Path,
    kubectl: &Path,
) -> anyhow::Result<()> {
    let admitted = match admit_submission(validated, sibling)? {
        AdmissionDecision::Admitted(admitted) => *admitted,
        AdmissionDecision::Rejected(rejection) => return report_admission_rejection(&rejection),
    };
    let existing = existing_result_jobs(
        kubectl,
        &args.context,
        &args.namespace,
        &admitted.validated.result_identity_label,
    )?;
    ensure_result_available(&existing)?;
    let rendered = render_admitted_submission(admitted, &args.namespace)?;
    let secret_body = serde_json::to_vec(&rendered.manifest["items"][0])?;
    let output = kubectl_with_input(
        kubectl,
        &args.context,
        &args.namespace,
        ["create", "-f", "-"],
        &secret_body,
    )?;
    ensure_kubectl_success(output, "create immutable prediction input Secret")?;
    let job_body = serde_json::to_vec(&rendered.manifest["items"][1])?;
    let output = kubectl_with_input(
        kubectl,
        &args.context,
        &args.namespace,
        ["create", "-f", "-"],
        &job_body,
    )?;
    let recovered_after_create_error = if let Err(error) =
        ensure_kubectl_success(output, "create immutable prediction research Job")
    {
        let readback = existing_result_jobs(
            kubectl,
            &args.context,
            &args.namespace,
            &rendered.result_identity_sha256[..32],
        );
        match create_failure_recovered(
            readback.as_deref().ok(),
            &rendered.job_name,
            &rendered.manifest["items"][1]["metadata"]["annotations"],
        ) {
            Some(true) => true,
            Some(false) => {
                delete_input_secret(kubectl, &args.context, &args.namespace, &rendered.secret_name)?;
                return Err(error);
            }
            None => return Err(error.context(match readback {
                Ok(_) => "Job create outcome is conflicting; input Secret retained for reconciliation".to_owned(),
                Err(readback) => format!("Job create outcome is unknown; input Secret retained for reconciliation: {readback:#}"),
            })),
        }
    } else {
        false
    };
    print_json(&json!({
        "status": "submitted",
        "context": args.context,
        "namespace": args.namespace,
        "job_name": rendered.job_name,
        "secret_name": rendered.secret_name,
        "result_identity_sha256": rendered.result_identity_sha256,
        "recovered_after_create_error": recovered_after_create_error,
    }))
}

pub fn status(args: PredictionDispatchStatusArgs) -> anyhow::Result<()> {
    let kubectl = kubectl_binary();
    validate_cluster_target(&args.context, &args.namespace)?;
    validate_dns_label("prediction Job name", &args.job_name)?;
    let job = kubectl_json(
        &kubectl,
        &args.context,
        &args.namespace,
        ["get", "job", &args.job_name, "-o", "json"],
        "read prediction Job status",
    )?;
    let job_uid = status_job_uid(&job)?;
    let selector = format!("batch.kubernetes.io/controller-uid={job_uid}");
    let pods = kubectl_json(
        &kubectl,
        &args.context,
        &args.namespace,
        ["get", "pods", "-l", &selector, "-o", "json"],
        "read prediction Pod status",
    )?;
    let evidence = args
        .evidence
        .as_deref()
        .map(load_status_evidence)
        .transpose()?;
    let derived = derive_status(&job, &pods, evidence.as_ref())?;
    if derived.job_name != args.job_name || derived.namespace != args.namespace {
        bail!("Kubernetes Job readback does not match the requested immutable identity");
    }
    print_json(&json!({"context": args.context, "status": derived}))
}

fn status_job_uid(job: &Value) -> anyhow::Result<String> {
    let job: StatusJob =
        serde_json::from_value(job.clone()).context("parse prediction Job readback")?;
    validate_identifier("prediction Job UID", &job.metadata.uid)?;
    Ok(job.metadata.uid)
}

fn load_status_evidence(path: &Path) -> anyhow::Result<Value> {
    let file = File::open(path)
        .with_context(|| format!("open prediction execution evidence {}", path.display()))?;
    if file.metadata()?.len() > MAX_SUBMISSION_BYTES {
        bail!("prediction execution evidence exceeds {MAX_SUBMISSION_BYTES} bytes");
    }
    let mut bytes = Vec::new();
    file.take(MAX_SUBMISSION_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SUBMISSION_BYTES {
        bail!("prediction execution evidence exceeds {MAX_SUBMISSION_BYTES} bytes");
    }
    serde_json::from_slice(&bytes)
        .with_context(|| format!("parse prediction execution evidence {}", path.display()))
}

fn derive_status(
    job: &Value,
    pods: &Value,
    evidence: Option<&Value>,
) -> anyhow::Result<PredictionStatus> {
    let job: StatusJob =
        serde_json::from_value(job.clone()).context("parse prediction Job readback")?;
    let pods: StatusPodList =
        serde_json::from_value(pods.clone()).context("parse prediction Pod readback")?;
    let annotation = |key: &str| {
        job.metadata
            .annotations
            .get(key)
            .map(String::as_str)
            .with_context(|| format!("prediction Job is missing {key} annotation"))
    };
    if annotation("research.monday/lane")? != "prediction_market" {
        bail!("Kubernetes Job is not a prediction research Job");
    }
    let mission_id = annotation("research.monday/mission-id")?.to_owned();
    validate_identifier("Job mission id", &mission_id)?;
    let mission_sha256 =
        normalized_sha256("Job mission", annotation("research.monday/mission-sha256")?)?;
    let snapshot_sha256 = normalized_sha256(
        "Job snapshot",
        annotation("research.monday/snapshot-sha256")?,
    )?;
    let research_ready = if let Some(evidence) = evidence {
        let evidence: StatusEvidence = serde_json::from_value(evidence.clone())
            .context("parse prediction execution evidence")?;
        if evidence.lane != "prediction_market"
            || evidence.mission_id != mission_id
            || normalized_sha256("evidence mission", &evidence.mission_sha256)? != mission_sha256
            || normalized_sha256("evidence snapshot", &evidence.snapshot_archive_sha256)?
                != snapshot_sha256
            || evidence.partition_digest != annotation("research.monday/partition-digest")?
            || evidence.policy_identity != annotation("research.monday/policy-identity")?
            || evidence.task_capability != annotation("research.monday/task-capability")?
            || evidence.image_identity != annotation("research.monday/admitted-image-identity")?
            || evidence.run_mode != annotation("research.monday/run-mode")?
            || normalized_sha256("evidence output", &evidence.bundle_sha256)?
                != normalized_sha256("evidence readback", &evidence.readback_bundle_sha256)?
        {
            bail!("prediction execution evidence does not match the immutable Job identity");
        }
        Some(true)
    } else {
        None
    };
    let condition = |conditions: &[StatusCondition], kind: &str| {
        conditions
            .iter()
            .any(|condition| condition.kind == kind && condition.status == "True")
    };
    let owned_pods = pods.items.iter().filter(|pod| {
        pod.metadata.owner_references.iter().any(|owner| {
            owner.controller
                && owner.kind == "Job"
                && owner.name == job.metadata.name
                && owner.uid == job.metadata.uid
        })
    });
    let scheduled = owned_pods
        .clone()
        .any(|pod| condition(&pod.status.conditions, "PodScheduled"));
    let image_ready = owned_pods.clone().any(|pod| {
        pod.status.container_statuses.iter().any(|container| {
            container.state.running.is_some() || container.state.terminated.is_some()
        })
    });
    Ok(PredictionStatus {
        job_name: job.metadata.name,
        namespace: job.metadata.namespace,
        mission_id,
        mission_sha256,
        snapshot_sha256,
        submitted: true,
        scheduled,
        image_ready,
        snapshot_ready: research_ready,
        evaluator_started: research_ready,
        completed: condition(&job.status.conditions, "Complete"),
    })
}

fn load_submission(path: &Path) -> anyhow::Result<PredictionSubmission> {
    let file = File::open(path)
        .with_context(|| format!("open prediction submission {}", path.display()))?;
    if file.metadata()?.len() > MAX_SUBMISSION_BYTES {
        bail!("prediction submission exceeds {MAX_SUBMISSION_BYTES} bytes");
    }
    let mut bytes = Vec::new();
    file.take(MAX_SUBMISSION_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SUBMISSION_BYTES {
        bail!("prediction submission exceeds {MAX_SUBMISSION_BYTES} bytes");
    }
    serde_json::from_slice(&bytes)
        .with_context(|| format!("parse prediction submission {}", path.display()))
}

fn validate_submission(submission: PredictionSubmission) -> anyhow::Result<ValidatedSubmission> {
    validate_dns_label("attempt id", &submission.attempt_id)?;
    validate_identifier("mission id", &submission.mission_id)?;
    validate_identifier("evaluator version", &submission.evaluator_version)?;
    validate_task_capability(&submission.task_capability)?;
    validate_admitted_task(&submission.task)?;
    immutable_sha256_identity("cohort partition", &submission.cohort_partition_id)?;
    if submission.resource_profile != RESOURCE_PROFILE {
        bail!("prediction resource profile must be {RESOURCE_PROFILE}");
    }
    let mission_sha256 = normalized_sha256("mission", &submission.mission_sha256)?;
    let snapshot_sha256 = normalized_sha256("snapshot", &submission.snapshot_sha256)?;
    immutable_sha256_identity("snapshot contract", &submission.snapshot_contract_id)?;
    let image_digest = image_digest(&submission.image)?;
    let mission_object = canonical_https_object("mission", &submission.mission_url)?;
    let snapshot_object = canonical_https_object("snapshot", &submission.snapshot_url)?;
    let result_object = canonical_https_object("result", &submission.result_put_url)?;
    let result_readback_object =
        canonical_https_object("result readback", &submission.result_readback_url)?;
    if result_readback_object != result_object {
        bail!("result readback URL must identify the same immutable result object");
    }
    if !result_object_binds_attempt(&result_object, &submission.attempt_id)? {
        bail!("result object path must contain the exact attempt id as an immutable path segment");
    }
    let resume_sha256 = match (
        submission
            .resume_url
            .as_deref()
            .filter(|value| !value.is_empty()),
        submission
            .resume_sha256
            .as_deref()
            .filter(|value| !value.is_empty()),
    ) {
        (None, None) => None,
        (Some(url), Some(sha256)) => {
            canonical_https_object("resume", url)?;
            Some(normalized_sha256("resume", sha256)?)
        }
        _ => bail!("prediction resume URL and SHA256 must be supplied together"),
    };
    let result_identity_sha256 = sha256_text(&result_object);
    let result_identity_label = result_identity_sha256[..32].to_string();
    let job_name = format!("prediction-{result_identity_label}");
    let secret_name = format!("{job_name}-inputs");
    Ok(ValidatedSubmission {
        submission,
        mission_sha256,
        snapshot_sha256,
        resume_sha256,
        image_digest,
        mission_object,
        snapshot_object,
        result_object,
        result_readback_object,
        result_identity_sha256,
        result_identity_label,
        job_name,
        secret_name,
    })
}

fn admit_submission(
    validated: ValidatedSubmission,
    sibling: &Path,
) -> anyhow::Result<AdmissionDecision> {
    admit_submission_with_timeout(validated, sibling, SNAPSHOT_ADMISSION_TIMEOUT)
}

fn admit_submission_with_timeout(
    validated: ValidatedSubmission,
    sibling: &Path,
    timeout: Duration,
) -> anyhow::Result<AdmissionDecision> {
    let deadline = Instant::now() + timeout;
    let mission_json = read_verified_mission_for_admission(
        &validated,
        remaining_admission_time(deadline, "immutable Mission v4 fetch")?,
    )?;
    admit_submission_before_deadline(validated, sibling, deadline, &mission_json)
}

#[cfg(test)]
fn admit_submission_with_timeout_and_mission(
    validated: ValidatedSubmission,
    sibling: &Path,
    timeout: Duration,
    mission_json: &str,
) -> anyhow::Result<AdmissionDecision> {
    admit_submission_before_deadline(validated, sibling, Instant::now() + timeout, mission_json)
}

fn admit_submission_before_deadline(
    validated: ValidatedSubmission,
    sibling: &Path,
    deadline: Instant,
    mission_json: &str,
) -> anyhow::Result<AdmissionDecision> {
    let compiler_source_identity = immutable_sha256_identity(
        "compiler source identity",
        &validated.submission.compiler_source_identity,
    )?;
    let build_input_identity = immutable_sha256_identity(
        "build input identity",
        &validated.submission.build_input_identity,
    )?;
    let artifact_sha256 = immutable_sha256_identity(
        "catalog partition artifact",
        &validated
            .submission
            .catalog_partition_artifact
            .artifact_sha256,
    )?;
    let payload_sha256 = immutable_sha256_identity(
        "catalog partition payload",
        &validated
            .submission
            .catalog_partition_artifact
            .payload_sha256,
    )?;
    validate_catalog_partition_artifact_path(
        &validated.submission.catalog_partition_artifact.path,
    )?;
    let request = SnapshotAdmissionRequest {
        schema_version: SNAPSHOT_ADMISSION_SCHEMA_VERSION,
        catalog_partition_artifact: &CatalogPartitionArtifactRef {
            path: validated.submission.catalog_partition_artifact.path.clone(),
            artifact_sha256,
            payload_sha256,
        },
        compiler_source_identity: &compiler_source_identity,
        compiler_image_identity: format!("sha256:{}", validated.image_digest),
        build_input_identity: &build_input_identity,
        task_capability: &validated.submission.task_capability,
        task: &validated.submission.task,
        cohort_partition_id: &validated.submission.cohort_partition_id,
        mission_id: &validated.submission.mission_id,
        snapshot_contract_id: &validated.submission.snapshot_contract_id,
        mission_json,
    };
    let request = serde_json::to_vec(&request).context("serialize snapshot admission request")?;
    if request.len() > MAX_ADMISSION_REQUEST_BYTES {
        bail!("snapshot admission request exceeds {MAX_ADMISSION_REQUEST_BYTES} bytes");
    }
    let mut child = Command::new(sibling)
        .arg("--admit-authenticated-snapshot")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("start snapshot admission sibling {}", sibling.display()))?;
    let mut stdin = child
        .stdin
        .take()
        .context("snapshot admission sibling stdin is unavailable")?;
    let (write_sender, write_receiver) = mpsc::sync_channel(1);
    let writer = thread::spawn(move || {
        let _ = write_sender.send(
            stdin
                .write_all(&request)
                .context("write snapshot admission request"),
        );
    });
    let write_result = match write_receiver.recv_timeout(remaining_admission_time_or_reap(
        &mut child,
        deadline,
        "snapshot admission sibling request",
    )?) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            terminate_admission_child(&mut child);
            bail!("snapshot admission sibling timed out before the admission deadline");
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            terminate_admission_child(&mut child);
            bail!("snapshot admission sibling request writer failed");
        }
    };
    writer
        .join()
        .map_err(|_| anyhow::anyhow!("snapshot admission sibling request writer panicked"))?;
    if let Err(error) = write_result {
        terminate_admission_child(&mut child);
        return Err(error);
    }
    let stdout = child
        .stdout
        .take()
        .context("snapshot admission sibling stdout is unavailable")?;
    let (sender, receiver) = mpsc::sync_channel(1);
    let reader = thread::spawn(move || {
        let _ = sender.send(read_bounded_admission_response(stdout));
    });
    let output = match receiver.recv_timeout(remaining_admission_time_or_reap(
        &mut child,
        deadline,
        "snapshot admission sibling response",
    )?) {
        Ok(output) => output,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            terminate_admission_child(&mut child);
            bail!("snapshot admission sibling timed out before the admission deadline");
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            terminate_admission_child(&mut child);
            bail!("snapshot admission sibling response reader failed");
        }
    };
    reader
        .join()
        .map_err(|_| anyhow::anyhow!("snapshot admission sibling response reader panicked"))?;
    let status = wait_for_admission_child_exit_until(&mut child, deadline)?;
    let output = output?;
    if !status.success() {
        bail!("snapshot admission sibling exited unsuccessfully");
    }
    match parse_snapshot_admission_response(&output, &validated)? {
        SnapshotAdmissionDecision::Admitted(admission) => {
            let run_mode = mission_run_mode(mission_json)?;
            Ok(AdmissionDecision::Admitted(Box::new(AdmittedSubmission {
                validated,
                admission,
                run_mode,
            })))
        }
        SnapshotAdmissionDecision::Rejected(rejection) => {
            Ok(AdmissionDecision::Rejected(rejection))
        }
    }
}

fn mission_run_mode(mission_json: &str) -> anyhow::Result<MissionRunMode> {
    let mission: Value = serde_json::from_str(mission_json)
        .context("parse SHA-verified Mission v4 execution mode")?;
    if mission.get("schema_version").and_then(Value::as_str)
        != Some("prediction_research_mission.v4")
    {
        bail!("prediction execution requires Mission v4");
    }
    match mission.get("run_mode").and_then(Value::as_str) {
        Some("pipeline_smoke") => Ok(MissionRunMode::PipelineSmoke),
        Some("research_trial") => Ok(MissionRunMode::ResearchTrial),
        _ => bail!("prediction Mission v4 run mode is unsupported"),
    }
}

fn read_verified_mission_for_admission(
    validated: &ValidatedSubmission,
    timeout: Duration,
) -> anyhow::Result<String> {
    let root = tempfile::tempdir().context("create bounded mission admission directory")?;
    let mission_path = root.path().join("mission.json");
    let client = Client::builder()
        .timeout(timeout)
        .build()
        .context("build mission admission client")?;
    let (_, mission_sha256) = fetch_to_file(
        &client,
        &validated.submission.mission_url,
        &mission_path,
        MAX_ADMISSION_MISSION_BYTES,
    )
    .context("fetch bounded immutable Mission v4 for admission")?;
    if mission_sha256 != validated.mission_sha256 {
        bail!("prediction Mission v4 SHA256 mismatch before snapshot admission");
    }
    std::fs::read_to_string(&mission_path)
        .context("Mission v4 must be valid UTF-8 JSON for snapshot admission")
}

fn terminate_admission_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
fn wait_for_admission_child_exit(
    child: &mut Child,
    timeout: Duration,
) -> anyhow::Result<std::process::ExitStatus> {
    wait_for_admission_child_exit_until(child, Instant::now() + timeout)
}

fn wait_for_admission_child_exit_until(
    child: &mut Child,
    deadline: Instant,
) -> anyhow::Result<std::process::ExitStatus> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                terminate_admission_child(child);
                bail!("snapshot admission sibling did not exit before the admission deadline");
            }
            Err(error) => {
                terminate_admission_child(child);
                return Err(error).context("poll snapshot admission sibling exit");
            }
        }
    }
}

fn remaining_admission_time(deadline: Instant, phase: &str) -> anyhow::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .with_context(|| format!("{phase} exhausted snapshot admission timeout"))
}

fn remaining_admission_time_or_reap(
    child: &mut Child,
    deadline: Instant,
    phase: &str,
) -> anyhow::Result<Duration> {
    match remaining_admission_time(deadline, phase) {
        Ok(remaining) => Ok(remaining),
        Err(error) => {
            terminate_admission_child(child);
            Err(error)
        }
    }
}

fn read_bounded_admission_response(mut reader: impl Read) -> anyhow::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 4096];
    let mut exceeded = false;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let remaining = MAX_ADMISSION_RESPONSE_BYTES.saturating_add(1) as usize - output.len();
        let copied = read.min(remaining);
        output.extend_from_slice(&buffer[..copied]);
        exceeded |= copied < read || output.len() as u64 > MAX_ADMISSION_RESPONSE_BYTES;
    }
    if exceeded {
        bail!("snapshot admission response exceeds {MAX_ADMISSION_RESPONSE_BYTES} bytes");
    }
    Ok(output)
}

fn parse_snapshot_admission_response(
    output: &[u8],
    validated: &ValidatedSubmission,
) -> anyhow::Result<SnapshotAdmissionDecision> {
    let output = std::str::from_utf8(output).context("snapshot admission response is not UTF-8")?;
    let response = output
        .strip_suffix('\n')
        .filter(|line| !line.is_empty() && !line.contains('\n'))
        .context("snapshot admission response must be exactly one non-empty JSON line")?;
    let response: SnapshotAdmissionResponse =
        serde_json::from_str(response).context("snapshot admission response is invalid")?;
    match response {
        SnapshotAdmissionResponse::Rejected {
            schema_version,
            rejection,
        } => {
            if schema_version != SNAPSHOT_ADMISSION_SCHEMA_VERSION || rejection.is_empty() {
                bail!("snapshot admission response is invalid");
            }
            Ok(SnapshotAdmissionDecision::Rejected(rejection))
        }
        SnapshotAdmissionResponse::Admitted {
            schema_version,
            snapshot_contract_id,
            snapshot_digest,
            partition_digest,
            policy_identity,
            task_capability,
            task,
            cohort_partition_id,
            cohort_manifest_id,
            partition_view,
            immutable_image_identity,
        } => {
            if schema_version != SNAPSHOT_ADMISSION_SCHEMA_VERSION
                || task_capability != validated.submission.task_capability
                || task != validated.submission.task
                || cohort_partition_id != validated.submission.cohort_partition_id
                || snapshot_contract_id != validated.submission.snapshot_contract_id
                || immutable_image_identity != format!("sha256:{}", validated.image_digest)
                || validate_snapshot_digest(&snapshot_digest).is_err()
            {
                bail!("snapshot admission response does not bind the submitted immutable identity");
            }
            Ok(SnapshotAdmissionDecision::Admitted(SnapshotAdmission {
                snapshot_contract_id: immutable_sha256_identity(
                    "admitted snapshot contract",
                    &snapshot_contract_id,
                )?,
                snapshot_digest,
                partition_digest: immutable_sha256_identity(
                    "admitted partition",
                    &partition_digest,
                )?,
                policy_identity: immutable_sha256_identity("admitted policy", &policy_identity)?,
                task_capability,
                task,
                cohort_partition_id: immutable_sha256_identity(
                    "admitted cohort partition",
                    &cohort_partition_id,
                )?,
                cohort_manifest_id: immutable_sha256_identity(
                    "admitted cohort manifest",
                    &cohort_manifest_id,
                )?,
                partition_view,
                immutable_image_identity,
            }))
        }
    }
}

fn report_admission_rejection(rejection: &str) -> anyhow::Result<()> {
    print_json(&json!({
        "schema_version": SNAPSHOT_ADMISSION_SCHEMA_VERSION,
        "status": "rejected",
        "rejection": rejection,
    }))?;
    bail!("snapshot admission rejected: {rejection}")
}

fn immutable_sha256_identity(label: &str, value: &str) -> anyhow::Result<String> {
    let digest = value
        .strip_prefix("sha256:")
        .with_context(|| format!("{label} must use sha256:<64 lowercase hex>"))?;
    if value != format!("sha256:{digest}") || digest != normalized_sha256(label, digest)? {
        bail!("{label} must use sha256:<64 lowercase hex>");
    }
    Ok(value.to_owned())
}

fn validate_snapshot_digest(value: &str) -> anyhow::Result<()> {
    if value.len() != 16
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        bail!("admitted snapshot digest must be exactly 16 lowercase ASCII hex characters");
    }
    Ok(())
}

fn render_admitted_submission(
    admitted: AdmittedSubmission,
    namespace: &str,
) -> anyhow::Result<RenderedSubmission> {
    validate_dns_label("namespace", namespace)?;
    render_validated_submission(admitted, namespace)
}

fn render_validated_submission(
    admitted: AdmittedSubmission,
    namespace: &str,
) -> anyhow::Result<RenderedSubmission> {
    let admission = admitted.admission;
    let validated = admitted.validated;
    let run_mode = admitted.run_mode;
    let resume_url = validated.submission.resume_url.as_deref().unwrap_or("");
    let resume_sha256 = validated.resume_sha256.as_deref().unwrap_or("");
    let labels = json!({
        "app.kubernetes.io/name": "monday-prediction-research",
        "app.kubernetes.io/part-of": "monday",
        "research.monday/result-id": validated.result_identity_label,
    });
    let annotations = json!({
        "research.monday/attempt-id": validated.submission.attempt_id,
        "research.monday/mission-id": validated.submission.mission_id,
        "research.monday/mission-sha256": validated.mission_sha256,
        "research.monday/mission-object": validated.mission_object,
        "research.monday/snapshot-sha256": validated.snapshot_sha256,
        "research.monday/snapshot-object": validated.snapshot_object,
        "research.monday/snapshot-contract-id": admission.snapshot_contract_id,
        "research.monday/admitted-snapshot-digest": admission.snapshot_digest,
        "research.monday/partition-digest": admission.partition_digest,
        "research.monday/cohort-partition-id": admission.cohort_partition_id,
        "research.monday/cohort-manifest-id": admission.cohort_manifest_id,
        "research.monday/policy-identity": admission.policy_identity,
        "research.monday/task-capability": admission.task_capability,
        "research.monday/task-kind": admitted_task_kind(&admission.task),
        "research.monday/run-mode": run_mode.as_str(),
        "research.monday/admitted-image-identity": admission.immutable_image_identity,
        "research.monday/result-object": validated.result_object,
        "research.monday/result-readback-object": validated.result_readback_object,
        "research.monday/result-identity-sha256": validated.result_identity_sha256,
        "research.monday/image-digest": validated.image_digest,
        "research.monday/evaluator-version": validated.submission.evaluator_version,
        "research.monday/resource-profile": RESOURCE_PROFILE,
        "research.monday/lane": "prediction_market",
    });
    let container_args = json!([
        "prediction",
        "execute",
        "--work-dir",
        "/work",
        "--mission-url",
        "$(MISSION_URL)",
        "--mission-sha256",
        "$(MISSION_SHA256)",
        "--snapshot-url",
        "$(SNAPSHOT_URL)",
        "--snapshot-sha256",
        "$(SNAPSHOT_SHA256)",
        "--snapshot-contract-id",
        "$(SNAPSHOT_CONTRACT_ID)",
        "--snapshot-digest",
        "$(SNAPSHOT_DIGEST)",
        "--cohort-manifest-id",
        "$(COHORT_MANIFEST_ID)",
        "--partition-digest",
        "$(PARTITION_DIGEST)",
        "--policy-identity",
        "$(POLICY_IDENTITY)",
        "--task-capability",
        "$(TASK_CAPABILITY)",
        "--image-identity",
        "$(IMAGE_IDENTITY)",
        "--partition-view-json",
        "$(PARTITION_VIEW_JSON)",
        "--resume-url",
        "$(RESUME_URL)",
        "--resume-sha256",
        "$(RESUME_SHA256)",
        "--result-put-url",
        "$(RESULT_PUT_URL)",
        "--result-readback-url",
        "$(RESULT_READBACK_URL)"
    ]);
    let manifest = json!({
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
                        "research.monday/result-identity-sha256": validated.result_identity_sha256,
                        "research.monday/attempt-id": validated.submission.attempt_id,
                    },
                },
                "type": "Opaque",
                "immutable": true,
                "stringData": {
                    "mission-url": validated.submission.mission_url,
                    "snapshot-url": validated.submission.snapshot_url,
                    "result-put-url": validated.submission.result_put_url,
                    "result-readback-url": validated.submission.result_readback_url,
                    "resume-url": resume_url,
                    "resume-sha256": resume_sha256,
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
                                "seccompProfile": { "type": "RuntimeDefault" },
                            },
                            "containers": [{
                                "name": "prediction-mission",
                                "image": validated.submission.image,
                                "imagePullPolicy": "IfNotPresent",
                                "command": ["/usr/local/bin/alpha-harness"],
                                "args": container_args,
                                "env": prediction_environment(&validated, &admission),
                                "resources": {
                                    "requests": { "cpu": "3500m", "memory": "8Gi" },
                                    "limits": { "cpu": "3500m", "memory": "12Gi" },
                                },
                                "securityContext": {
                                    "allowPrivilegeEscalation": false,
                                    "capabilities": { "drop": ["ALL"] },
                                    "readOnlyRootFilesystem": true,
                                },
                                "volumeMounts": [
                                    { "name": "work", "mountPath": "/work" },
                                    { "name": "tmp", "mountPath": "/tmp" }
                                ],
                            }],
                            "volumes": [
                                { "name": "work", "emptyDir": { "sizeLimit": "40Gi" } },
                                { "name": "tmp", "emptyDir": {} }
                            ],
                        },
                    },
                },
            }
        ]
    });
    Ok(RenderedSubmission {
        manifest,
        job_name: validated.job_name,
        secret_name: validated.secret_name,
        result_identity_sha256: validated.result_identity_sha256,
    })
}

fn prediction_environment(validated: &ValidatedSubmission, admission: &SnapshotAdmission) -> Value {
    let secret_ref = |key: &str| {
        json!({
            "name": validated.secret_name,
            "key": key,
        })
    };
    let environment = vec![
        json!({ "name": "MISSION_URL", "valueFrom": { "secretKeyRef": secret_ref("mission-url") } }),
        json!({ "name": "SNAPSHOT_URL", "valueFrom": { "secretKeyRef": secret_ref("snapshot-url") } }),
        json!({ "name": "RESULT_PUT_URL", "valueFrom": { "secretKeyRef": secret_ref("result-put-url") } }),
        json!({ "name": "RESULT_READBACK_URL", "valueFrom": { "secretKeyRef": secret_ref("result-readback-url") } }),
        json!({ "name": "RESUME_URL", "valueFrom": { "secretKeyRef": secret_ref("resume-url") } }),
        json!({ "name": "RESUME_SHA256", "valueFrom": { "secretKeyRef": secret_ref("resume-sha256") } }),
        json!({ "name": "MISSION_SHA256", "value": validated.mission_sha256 }),
        json!({ "name": "SNAPSHOT_SHA256", "value": validated.snapshot_sha256 }),
        json!({ "name": "SNAPSHOT_CONTRACT_ID", "value": admission.snapshot_contract_id }),
        json!({ "name": "SNAPSHOT_DIGEST", "value": admission.snapshot_digest }),
        json!({ "name": "COHORT_MANIFEST_ID", "value": admission.cohort_manifest_id }),
        json!({ "name": "PARTITION_DIGEST", "value": admission.partition_digest }),
        json!({ "name": "POLICY_IDENTITY", "value": admission.policy_identity }),
        json!({ "name": "TASK_CAPABILITY", "value": admission.task_capability }),
        json!({ "name": "IMAGE_IDENTITY", "value": admission.immutable_image_identity }),
        json!({ "name": "PARTITION_VIEW_JSON", "value": serde_json::to_string(&admission.partition_view).expect("serialize admitted partition view") }),
    ];
    Value::Array(environment)
}

fn image_digest(image: &str) -> anyhow::Result<String> {
    if image != image.trim() || image.chars().any(char::is_control) {
        bail!("prediction image must not contain surrounding whitespace or control characters");
    }
    let (repository, digest) = image
        .rsplit_once("@sha256:")
        .context("prediction image must be pinned by @sha256 digest")?;
    if repository.is_empty() {
        bail!("prediction image repository is required");
    }
    normalized_sha256("prediction image", digest)
}

pub(crate) fn canonical_https_object(label: &str, value: &str) -> anyhow::Result<String> {
    if value != value.trim() || value.chars().any(char::is_control) {
        bail!("{label} URL must not contain surrounding whitespace or control characters");
    }
    let mut url = reqwest::Url::parse(value).with_context(|| format!("{label} URL is invalid"))?;
    if url.scheme() != "https" || url.host_str().is_none() {
        bail!("{label} URL must be HTTPS with a host");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("{label} URL must not contain userinfo credentials");
    }
    if url.fragment().is_some() || url.path() == "/" || url.path().ends_with('/') {
        bail!("{label} URL must identify one object without a fragment");
    }
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string())
}

pub(crate) fn result_object_binds_attempt(object: &str, attempt_id: &str) -> anyhow::Result<bool> {
    let url = reqwest::Url::parse(object)?;
    Ok(url.path_segments().into_iter().flatten().any(|segment| {
        segment == attempt_id
            || segment.strip_prefix("attempt=") == Some(attempt_id)
            || segment.strip_suffix(".zip") == Some(attempt_id)
    }))
}

pub(crate) fn cex_result_attempt_and_holdout_claim(
    result_object: &str,
    mission_id: &str,
) -> anyhow::Result<(String, String)> {
    let segment = format!("/mission-id={mission_id}/");
    let mut matches = result_object.match_indices(&segment);
    let (index, _) = matches
        .next()
        .context("result object must bind the exact Mission ID")?;
    if matches.next().is_some() || result_object[..index].contains("/mission-id=") {
        bail!("result object contains duplicate Mission ID bindings");
    }
    let attempt_id = result_object[index + segment.len()..]
        .strip_prefix("attempt=")
        .and_then(|value| value.strip_suffix("/results.zip"))
        .context("CEX result object must end with mission-id=<id>/attempt=<id>/results.zip")?;
    validate_dns_label("attempt id", attempt_id)?;
    Ok((
        attempt_id.to_string(),
        format!(
            "{}{segment}sealed-holdout-claim.json",
            &result_object[..index]
        ),
    ))
}

fn validate_identifier(label: &str, value: &str) -> anyhow::Result<()> {
    let value = value.trim();
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        bail!("{label} is invalid");
    }
    Ok(())
}

fn validate_task_capability(value: &str) -> anyhow::Result<()> {
    let bytes = value.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 63
        || !bytes[0].is_ascii_lowercase()
        || !bytes[bytes.len() - 1].is_ascii_alphanumeric()
        || !bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
    {
        bail!("task capability must be a lowercase safe identifier");
    }
    Ok(())
}

fn validate_admitted_task(task: &MissionTaskIdentity) -> anyhow::Result<()> {
    if matches!(task.kind, MissionTaskKind::SettlementProbability)
        && task.side.is_none()
        && task.prediction_horizon_secs.is_none()
    {
        return Ok(());
    }
    bail!(
        "btc_5m_backtest only admits settlement_probability without token side or prediction horizon"
    )
}

fn admitted_task_kind(task: &MissionTaskIdentity) -> &'static str {
    match task.kind {
        MissionTaskKind::SettlementProbability => "settlement_probability",
        MissionTaskKind::UpExecution => "up_execution",
        MissionTaskKind::DownExecution => "down_execution",
    }
}

fn validate_catalog_partition_artifact_path(value: &str) -> anyhow::Result<()> {
    let path = Path::new(value);
    let has_windows_prefix = value.as_bytes().get(1) == Some(&b':')
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphabetic);
    if value.is_empty()
        || value.chars().any(char::is_control)
        || path.is_absolute()
        || has_windows_prefix
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        bail!("catalog partition artifact path is invalid");
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("catalog partition artifact path must end in a UTF-8 filename")?;
    let suffix = name
        .strip_prefix("catalog-partition-")
        .and_then(|name| name.strip_suffix(".json"));
    if suffix.is_none_or(str::is_empty) {
        bail!("catalog partition artifact path must name catalog-partition-*.json");
    }
    Ok(())
}

pub(crate) fn validate_dns_label(label: &str, value: &str) -> anyhow::Result<()> {
    let bytes = value.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 63
        || !bytes[0].is_ascii_alphanumeric()
        || !bytes[bytes.len() - 1].is_ascii_alphanumeric()
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
    {
        bail!("{label} must be a lowercase Kubernetes DNS label");
    }
    Ok(())
}

fn validate_cluster_target(context: &str, namespace: &str) -> anyhow::Result<()> {
    validate_identifier("Kubernetes context", context)?;
    validate_dns_label("namespace", namespace)
}

fn sha256_text(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn kubectl_binary() -> PathBuf {
    std::env::var_os("MONDAY_KUBECTL_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("kubectl"))
}

fn existing_result_jobs(
    kubectl: &Path,
    context: &str,
    namespace: &str,
    result_identity_label: &str,
) -> anyhow::Result<Vec<(String, Value)>> {
    let selector = format!("research.monday/result-id={result_identity_label}");
    let jobs = kubectl_json(
        kubectl,
        context,
        namespace,
        ["get", "jobs", "-l", selector.as_str(), "-o", "json"],
        "check prediction result identity",
    )?;
    let mut jobs_with_identities = Vec::new();
    for job in jobs["items"]
        .as_array()
        .context("kubectl Job list is missing items")?
    {
        let name = job["metadata"]["name"]
            .as_str()
            .context("matched prediction Job is missing its name")?;
        let annotations = &job["metadata"]["annotations"];
        let identity = annotations["research.monday/result-identity-sha256"]
            .as_str()
            .context("matched prediction Job is missing its result identity annotation")?;
        normalized_sha256("result identity", identity)?;
        jobs_with_identities.push((name.to_owned(), annotations.clone()));
    }
    Ok(jobs_with_identities)
}

fn ensure_result_available(jobs: &[(String, Value)]) -> anyhow::Result<()> {
    if !jobs.is_empty() {
        bail!("result identity label is already assigned to a prediction Job");
    }
    Ok(())
}

fn create_failure_recovered(
    jobs: Option<&[(String, Value)]>,
    name: &str,
    annotations: &Value,
) -> Option<bool> {
    match jobs {
        Some(jobs)
            if jobs
                .iter()
                .any(|job| job.0 == name && job.1 == *annotations) =>
        {
            Some(true)
        }
        Some([]) => Some(false),
        _ => None,
    }
}

fn kubectl_json<const N: usize>(
    kubectl: &Path,
    context: &str,
    namespace: &str,
    args: [&str; N],
    action: &str,
) -> anyhow::Result<Value> {
    let output = Command::new(kubectl)
        .arg("--context")
        .arg(context)
        .arg("--namespace")
        .arg(namespace)
        .args(args)
        .output()
        .with_context(|| format!("start kubectl to {action}"))?;
    let stdout = ensure_kubectl_success(output, action)?;
    serde_json::from_slice(&stdout).with_context(|| format!("parse kubectl output for {action}"))
}

fn kubectl_with_input<const N: usize>(
    kubectl: &Path,
    context: &str,
    namespace: &str,
    args: [&str; N],
    input: &[u8],
) -> anyhow::Result<Output> {
    let mut child = Command::new(kubectl)
        .arg("--context")
        .arg(context)
        .arg("--namespace")
        .arg(namespace)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("start kubectl prediction Job creation")?;
    child
        .stdin
        .take()
        .context("kubectl stdin is unavailable")?
        .write_all(input)?;
    child.wait_with_output().context("wait for kubectl")
}

fn delete_input_secret(
    kubectl: &Path,
    context: &str,
    namespace: &str,
    secret_name: &str,
) -> anyhow::Result<()> {
    let output = Command::new(kubectl)
        .arg("--context")
        .arg(context)
        .arg("--namespace")
        .arg(namespace)
        .args(["delete", "secret", secret_name, "--ignore-not-found=true"])
        .output()
        .context("start prediction input Secret cleanup")?;
    ensure_kubectl_success(output, "clean up prediction input Secret")?;
    Ok(())
}

fn ensure_kubectl_success(output: Output, action: &str) -> anyhow::Result<Vec<u8>> {
    if !output.status.success() {
        bail!(
            "kubectl failed to {action}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_the_blessed_immutable_prediction_job() {
        let rendered = render_admitted_submission(
            admitted_submission_for_test(valid_submission()),
            "monday-research",
        )
        .expect("valid immutable submission must render");
        let secret = &rendered.manifest["items"][0];
        let job = &rendered.manifest["items"][1];

        assert_eq!(rendered.manifest["kind"], "List");
        assert_eq!(secret["kind"], "Secret");
        assert_eq!(secret["immutable"], true);
        assert_eq!(job["kind"], "Job");
        assert_eq!(job["metadata"]["namespace"], "monday-research");
        assert_eq!(
            job["spec"]["template"]["spec"]["automountServiceAccountToken"],
            false
        );
        assert_eq!(
            job["spec"]["template"]["spec"]["containers"][0]["image"],
            format!("registry/research-runner@sha256:{}", "a".repeat(64))
        );
        assert_eq!(
            job["metadata"]["annotations"]["research.monday/resource-profile"],
            RESOURCE_PROFILE
        );
        assert_eq!(
            job["metadata"]["annotations"]["research.monday/result-identity-sha256"],
            rendered.result_identity_sha256
        );
        assert_eq!(
            job["metadata"]["annotations"]["research.monday/partition-digest"],
            format!("sha256:{}", "e".repeat(64))
        );
        assert_eq!(
            job["metadata"]["annotations"]["research.monday/run-mode"],
            "research_trial"
        );
        let container = &job["spec"]["template"]["spec"]["containers"][0];
        assert!(container["args"].as_array().is_some_and(|args| {
            [
                "--snapshot-contract-id",
                "--partition-digest",
                "--policy-identity",
                "--task-capability",
                "--image-identity",
                "--partition-view-json",
            ]
            .iter()
            .all(|flag| args.contains(&json!(flag)))
        }));
        assert!(container["env"]
            .as_array()
            .is_some_and(|values| values.iter().any(|value| {
                value["name"] == "SNAPSHOT_CONTRACT_ID"
                    && value["value"] == format!("sha256:{}", "1".repeat(64))
            })));
        assert!(container["env"]
            .as_array()
            .is_some_and(|values| values.iter().any(|value| {
                value["name"] == "TASK_CAPABILITY" && value["value"] == "btc_5m_backtest"
            })));
        assert!(rendered.job_name.starts_with("prediction-"));
    }

    #[test]
    fn both_v4_modes_render_without_llm_credentials() {
        for admitted in [
            admitted_submission_for_test(valid_submission()),
            pipeline_smoke_admitted_submission_for_test(valid_submission()),
        ] {
            let rendered = render_admitted_submission(admitted, "monday-research").unwrap();
            let environment = rendered.manifest["items"][1]["spec"]["template"]["spec"]
                ["containers"][0]["env"]
                .as_array()
                .expect("prediction Job environment must be an array");
            assert!(environment.iter().all(|value| !value["name"]
                .as_str()
                .is_some_and(|name| name.starts_with("MONDAY_PREDICTION_LLM_"))));
        }
    }

    #[test]
    fn rejects_mutable_or_untrimmed_image_references() {
        for image in [
            "registry/research-runner:latest".to_owned(),
            format!(" registry/research-runner@sha256:{}", "a".repeat(64)),
        ] {
            let mut submission = valid_submission();
            submission.image = image;
            assert!(validate_submission(submission).is_err());
        }
    }

    #[test]
    fn rejects_an_unbound_snapshot_identity() {
        let mut submission = valid_submission();
        submission.snapshot_sha256.clear();

        let error = validate_submission(submission)
            .err()
            .expect("snapshot without an authenticated digest must fail");

        assert!(error.to_string().contains("snapshot SHA256 is invalid"));
    }

    #[test]
    fn rejects_a_duplicate_result_identity() {
        let error = ensure_result_available(&[("job".to_owned(), Value::Null)])
            .expect_err("duplicate output must fail before Job creation");

        assert!(error.to_string().contains("already assigned"));
    }

    #[test]
    fn rejects_a_mutable_result_object_without_the_attempt_identity() {
        let mut submission = valid_submission();
        submission.result_put_url =
            "https://oss-internal/results/latest/results.zip?signature=x".to_owned();
        submission.result_readback_url =
            "https://oss-internal/results/latest/results.zip?read-signature=x".to_owned();

        let error = validate_submission(submission)
            .err()
            .expect("mutable output identity must fail before Job creation");

        assert!(error.to_string().contains("exact attempt id"));
    }

    #[test]
    fn rejects_url_userinfo_or_untrimmed_values_before_job_render() {
        for url in [
            "https://token@oss-internal/missions/mission.json?signature=x",
            " https://oss-internal/missions/mission.json?signature=x",
        ] {
            let mut submission = valid_submission();
            submission.mission_url = url.to_owned();
            assert!(validate_submission(submission).is_err());
        }
    }

    #[test]
    fn rejects_an_incomplete_resume_pair() {
        let mut submission = valid_submission();
        submission.resume_url =
            Some("https://oss-internal/results/prior.zip?signature=x".to_owned());

        let error = validate_submission(submission)
            .err()
            .expect("incomplete resume pair must fail");

        assert!(error.to_string().contains("must be supplied together"));
    }

    #[test]
    fn create_failure_reconciliation_is_fail_closed() {
        let annotations = json!({"r": "identity", "m": "a"});
        let exact = [("job".to_owned(), annotations.clone())];
        let conflict = [("job".to_owned(), json!({"r": "identity", "m": "b"}))];
        for (jobs, expected) in [
            (Some(exact.as_slice()), Some(true)),
            (Some([].as_slice()), Some(false)),
            (Some(conflict.as_slice()), None),
            (None, None),
        ] {
            let actual = create_failure_recovered(jobs, "job", &annotations);
            assert_eq!(actual, expected);
        }
    }

    #[cfg(unix)]
    #[test]
    fn invalid_submission_never_reaches_kubectl() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().expect("create dispatch test root");
        let submission = root.path().join("submission.json");
        std::fs::write(
            &submission,
            serde_json::to_vec(&json!({
                "attempt_id": "btc-5m-attempt-001",
                "mission_id": "btc-5m-mission-001",
                "image": format!("registry/research-runner@sha256:{}", "a".repeat(64)),
                "evaluator_version": format!("sha256:{}", "b".repeat(64)),
                "resource_profile": RESOURCE_PROFILE,
                "mission_url": "not-an-https-url",
                "mission_sha256": "c".repeat(64),
                "snapshot_url": "https://oss-internal/snapshots/snapshot.zip?signature=x",
                "snapshot_sha256": "d".repeat(64),
                "snapshot_contract_id": format!("sha256:{}", "1".repeat(64)),
                "result_put_url": "https://oss-internal/results/btc-5m-attempt-001/results.zip?signature=x",
                "result_readback_url": "https://oss-internal/results/btc-5m-attempt-001/results.zip?read-signature=x",
                "catalog_partition_artifact": {
                    "path": "catalog/catalog-partition-deadbeef.json",
                    "artifact_sha256": format!("sha256:{}", "e".repeat(64)),
                    "payload_sha256": format!("sha256:{}", "f".repeat(64)),
                },
                "compiler_source_identity": format!("sha256:{}", "1".repeat(64)),
                "build_input_identity": format!("sha256:{}", "2".repeat(64)),
                "task_capability": "btc_5m_backtest",
                "task": {"kind": "settlement_probability"},
                "cohort_partition_id": format!("sha256:{}", "e".repeat(64)),
            }))
            .expect("serialize submission"),
        )
        .expect("write submission");
        let sibling = root.path().join("monday-prediction-snapshot");
        std::fs::write(
            &sibling,
            "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{\"schema_version\":\"monday.prediction.snapshot_admission.v2\",\"status\":\"rejected\",\"rejection\":\"unsupported_task\"}'\n",
        )
        .expect("write sibling");
        std::fs::set_permissions(&sibling, std::fs::Permissions::from_mode(0o700))
            .expect("make sibling executable");
        let kubectl = root.path().join("kubectl");
        let kubectl_log = root.path().join("kubectl-called");
        std::fs::write(
            &kubectl,
            format!("#!/bin/sh\ntouch '{}'\nexit 1\n", kubectl_log.display()),
        )
        .expect("write kubectl sentinel");
        std::fs::set_permissions(&kubectl, std::fs::Permissions::from_mode(0o700))
            .expect("make kubectl sentinel executable");

        let _error = submit_with_binaries(
            PredictionDispatchSubmitArgs {
                submission,
                context: "ack".to_owned(),
                namespace: "monday-research".to_owned(),
            },
            &sibling,
            &kubectl,
        )
        .expect_err("rejected admission must fail dispatch after reporting a typed result");

        assert!(
            !kubectl_log.exists(),
            "invalid submission must fail before any Kubernetes read or write"
        );
    }

    #[test]
    fn admission_response_must_be_one_line_and_bind_submission_identities() {
        let validated = validate_submission(valid_submission()).expect("valid submission");
        let admitted = serde_json::to_vec(&json!({
            "schema_version": SNAPSHOT_ADMISSION_SCHEMA_VERSION,
            "status": "admitted",
            "snapshot_contract_id": format!("sha256:{}", "1".repeat(64)),
            "snapshot_digest": "0123456789abcdef",
            "partition_digest": format!("sha256:{}", "e".repeat(64)),
            "policy_identity": format!("sha256:{}", "f".repeat(64)),
            "task_capability": "btc_5m_backtest",
            "task": {"kind": "settlement_probability"},
            "cohort_partition_id": format!("sha256:{}", "e".repeat(64)),
            "cohort_manifest_id": format!("sha256:{}", "d".repeat(64)),
            "partition_view": {
                "common_time_boundary_ms": 1,
                "train_market_ids": ["train"],
                "crossing_excluded_market_ids": [],
                "held_out_market_ids": ["held-out"]
            },
            "immutable_image_identity": format!("sha256:{}", "a".repeat(64)),
        }))
        .expect("serialize admitted response");
        let mut one_line = admitted.clone();
        one_line.push(b'\n');
        assert!(parse_snapshot_admission_response(&one_line, &validated).is_ok());

        let rejected = serde_json::to_vec(&json!({
            "schema_version": SNAPSHOT_ADMISSION_SCHEMA_VERSION,
            "status": "rejected",
            "rejection": "unsupported_task",
        }))
        .expect("serialize rejected response");
        let mut rejected = rejected;
        rejected.push(b'\n');
        assert!(matches!(
            parse_snapshot_admission_response(&rejected, &validated),
            Ok(SnapshotAdmissionDecision::Rejected(reason)) if reason == "unsupported_task"
        ));

        let mut extra_output = serde_json::to_vec(&json!({
            "schema_version": SNAPSHOT_ADMISSION_SCHEMA_VERSION,
            "status": "rejected",
            "rejection": "unsupported_task",
        }))
        .expect("serialize rejected response");
        extra_output.extend_from_slice(b"\nextra");
        let mut invalid_digest =
            serde_json::from_slice::<Value>(&admitted).expect("parse admitted response");
        invalid_digest["snapshot_digest"] = json!("0123456789abcdeg");
        let mut invalid_digest = serde_json::to_vec(&invalid_digest).expect("serialize mismatch");
        invalid_digest.push(b'\n');
        let mut contract_mismatch =
            serde_json::from_slice::<Value>(&admitted).expect("parse admitted response");
        contract_mismatch["snapshot_contract_id"] = json!(format!("sha256:{}", "2".repeat(64)));
        let mut contract_mismatch =
            serde_json::to_vec(&contract_mismatch).expect("serialize contract mismatch");
        contract_mismatch.push(b'\n');
        let mut task_mismatch =
            serde_json::from_slice::<Value>(&admitted).expect("parse admitted response");
        task_mismatch["task"] =
            json!({"kind": "up_execution", "side": "up", "prediction_horizon_secs": 60});
        let mut task_mismatch =
            serde_json::to_vec(&task_mismatch).expect("serialize task mismatch");
        task_mismatch.push(b'\n');
        let mut partition_mismatch =
            serde_json::from_slice::<Value>(&admitted).expect("parse admitted response");
        partition_mismatch["cohort_partition_id"] = json!(format!("sha256:{}", "9".repeat(64)));
        let mut partition_mismatch =
            serde_json::to_vec(&partition_mismatch).expect("serialize partition mismatch");
        partition_mismatch.push(b'\n');
        for output in [
            admitted,
            extra_output,
            invalid_digest,
            contract_mismatch,
            task_mismatch,
            partition_mismatch,
        ] {
            assert!(parse_snapshot_admission_response(&output, &validated).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn admission_timeout_kills_a_stalled_sibling() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().expect("create admission timeout root");
        let sibling = root.path().join("monday-prediction-snapshot");
        std::fs::write(&sibling, "#!/bin/sh\ncat >/dev/null\nsleep 1\n")
            .expect("write stalled sibling");
        std::fs::set_permissions(&sibling, std::fs::Permissions::from_mode(0o700))
            .expect("make stalled sibling executable");

        let error = admit_submission_with_timeout_and_mission(
            validate_submission(valid_submission()).expect("valid submission"),
            &sibling,
            Duration::from_millis(100),
            "{}",
        )
        .err()
        .expect("stalled sibling must time out");

        assert!(error.to_string().contains("timed out"));
    }

    #[cfg(unix)]
    #[test]
    fn admission_request_write_timeout_kills_a_nonreading_sibling() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().expect("create nonreading sibling root");
        let sibling = root.path().join("monday-prediction-snapshot");
        std::fs::write(&sibling, "#!/bin/sh\nsleep 1\n").expect("write nonreading sibling");
        std::fs::set_permissions(&sibling, std::fs::Permissions::from_mode(0o700))
            .expect("make nonreading sibling executable");

        let started = Instant::now();
        let error = admit_submission_with_timeout_and_mission(
            validate_submission(valid_submission()).expect("valid submission"),
            &sibling,
            Duration::from_millis(100),
            &format!("\"{}\"", "x".repeat(24 * 1024)),
        )
        .err()
        .expect("nonreading sibling must time out during request write");

        assert!(error.to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[cfg(unix)]
    #[test]
    fn admission_timeout_does_not_join_a_reader_held_by_a_descendant() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().expect("create inherited-stdout timeout root");
        let sibling = root.path().join("monday-prediction-snapshot");
        std::fs::write(&sibling, "#!/bin/sh\ncat >/dev/null\nsleep 1 &\nexit 0\n")
            .expect("write descendant-stalling sibling");
        std::fs::set_permissions(&sibling, std::fs::Permissions::from_mode(0o700))
            .expect("make descendant-stalling sibling executable");

        let started = Instant::now();
        let error = admit_submission_with_timeout_and_mission(
            validate_submission(valid_submission()).expect("valid submission"),
            &sibling,
            Duration::from_millis(10),
            "{}",
        )
        .err()
        .expect("inherited stdout must time out without joining its reader");

        assert!(error.to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn oversized_admission_request_is_rejected_before_spawning_a_sibling() {
        let root = tempfile::tempdir().expect("create admission request root");
        let result = admit_submission_with_timeout_and_mission(
            validate_submission(valid_submission()).expect("valid submission"),
            &root.path().join("missing-sibling"),
            Duration::from_secs(1),
            &"x".repeat(MAX_ADMISSION_REQUEST_BYTES),
        );
        let error = match result {
            Ok(_) => panic!("oversized request must fail before spawning the sibling"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("request exceeds"));
    }

    #[cfg(unix)]
    #[test]
    fn admission_exit_wait_kills_a_sibling_that_does_not_exit_after_response() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "sleep 1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start response-stalling sibling");
        let error = wait_for_admission_child_exit(&mut child, Duration::from_millis(10))
            .expect_err("response-stalling sibling must be killed and reaped");

        assert!(error.to_string().contains("did not exit"), "{error:#}");
    }

    #[cfg(unix)]
    #[test]
    fn exhausted_admission_deadline_kills_and_reaps_the_sibling() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "sleep 1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start deadline-exhausted sibling");

        let error = remaining_admission_time_or_reap(
            &mut child,
            Instant::now(),
            "snapshot admission sibling response",
        )
        .expect_err("an exhausted deadline must terminate the sibling");

        assert!(error.to_string().contains("exhausted"), "{error:#}");
        assert!(child.try_wait().expect("poll reaped sibling").is_some());
    }

    #[test]
    fn validates_catalog_partition_artifact_path_and_task_capability() {
        for path in [
            "catalog/catalog-partition-deadbeef.json",
            "nested/catalog/catalog-partition-deadbeef.json",
        ] {
            validate_catalog_partition_artifact_path(path).expect("valid relative catalog path");
        }
        for path in [
            "",
            "/catalog/catalog-partition-deadbeef.json",
            "../catalog-partition-deadbeef.json",
            "C:\\catalog\\catalog-partition-deadbeef.json",
            "catalog/other.json",
            "catalog/catalog-partition-.json",
            "catalog/catalog-partition-deadbeef.json\n",
        ] {
            assert!(
                validate_catalog_partition_artifact_path(path).is_err(),
                "{path}"
            );
        }
        validate_task_capability("btc_5m_backtest").expect("supported syntax");
        for capability in ["", "BTC_5m_backtest", "btc 5m", "btc/5m", "_btc_5m"] {
            assert!(
                validate_task_capability(capability).is_err(),
                "{capability}"
            );
        }
    }

    #[test]
    fn status_derives_only_kubernetes_milestones_without_evidence() {
        let status = derive_status(&status_job(), &status_pods(), None).unwrap();
        assert!(status.submitted && status.scheduled && status.image_ready && status.completed);
        assert_eq!(status.snapshot_ready, None);
        assert_eq!(status.evaluator_started, None);
    }

    #[test]
    fn matching_execution_evidence_asserts_research_milestones() {
        let evidence = status_evidence();
        let status = derive_status(&status_job(), &status_pods(), Some(&evidence)).unwrap();
        assert_eq!(status.snapshot_ready, Some(true));
        assert_eq!(status.evaluator_started, Some(true));
    }

    #[test]
    fn status_rejects_evidence_from_another_mission_or_snapshot() {
        for field in ["mission_id", "mission_sha256", "snapshot_archive_sha256"] {
            let mut evidence = status_evidence();
            evidence[field] = if field == "mission_id" {
                json!("mission-2")
            } else {
                json!("e".repeat(64))
            };
            assert!(derive_status(&status_job(), &status_pods(), Some(&evidence)).is_err());
        }
    }

    #[test]
    fn status_rejects_evidence_without_the_admitted_policy_or_verified_readback() {
        for field in [
            "partition_digest",
            "policy_identity",
            "task_capability",
            "image_identity",
            "run_mode",
            "readback_bundle_sha256",
        ] {
            let mut evidence = status_evidence();
            evidence[field] = if field == "run_mode" {
                json!("legacy")
            } else {
                json!(format!("sha256:{}", "9".repeat(64)))
            };
            assert!(derive_status(&status_job(), &status_pods(), Some(&evidence)).is_err());
        }
    }

    #[test]
    fn submission_rejects_a_result_readback_url_for_another_object() {
        let mut submission = valid_submission();
        submission.result_readback_url =
            "https://oss-internal/results/another-attempt/results.zip?signature=x".to_owned();

        let error = match validate_submission(submission) {
            Ok(_) => panic!("result readback must bind the published object"),
            Err(error) => error,
        };

        assert!(error
            .to_string()
            .contains("result readback URL must identify the same immutable result object"));
    }

    #[test]
    fn pod_from_another_job_cannot_advance_milestones() {
        let mut pods = status_pods();
        pods["items"][0]["metadata"]["ownerReferences"][0]["uid"] = json!("other-uid");
        let status = derive_status(&status_job(), &pods, None).unwrap();
        assert!(!status.scheduled && !status.image_ready);
    }

    fn status_job() -> Value {
        json!({
            "metadata": {"name": "prediction-job", "namespace": "monday-research", "uid": "job-uid", "annotations": {
                "research.monday/lane": "prediction_market",
                "research.monday/mission-id": "mission-1",
                "research.monday/mission-sha256": "c".repeat(64),
                "research.monday/snapshot-sha256": "d".repeat(64),
                "research.monday/partition-digest": format!("sha256:{}", "e".repeat(64)),
                "research.monday/policy-identity": format!("sha256:{}", "f".repeat(64)),
                "research.monday/task-capability": "btc_5m_backtest",
                "research.monday/admitted-image-identity": format!("sha256:{}", "a".repeat(64)),
                "research.monday/run-mode": "pipeline_smoke"
            }},
            "status": {"conditions": [{"type": "Complete", "status": "True"}]}
        })
    }

    fn status_pods() -> Value {
        json!({"items": [{"metadata": {"ownerReferences": [{
            "uid": "job-uid", "name": "prediction-job", "kind": "Job", "controller": true
        }]}, "status": {
            "conditions": [{"type": "PodScheduled", "status": "True"}],
            "containerStatuses": [{"state": {"running": {}}}]
        }}]})
    }

    fn status_evidence() -> Value {
        json!({
            "lane": "prediction_market",
            "mission_id": "mission-1",
            "mission_sha256": "c".repeat(64),
            "snapshot_archive_sha256": "d".repeat(64),
            "partition_digest": format!("sha256:{}", "e".repeat(64)),
            "policy_identity": format!("sha256:{}", "f".repeat(64)),
            "task_capability": "btc_5m_backtest",
            "image_identity": format!("sha256:{}", "a".repeat(64)),
            "run_mode": "pipeline_smoke",
            "bundle_sha256": "b".repeat(64),
            "readback_bundle_sha256": "b".repeat(64),
            "runner_exit_code": 0
        })
    }

    fn valid_submission() -> PredictionSubmission {
        PredictionSubmission {
            attempt_id: "btc-5m-attempt-001".to_owned(),
            mission_id: "btc-5m-mission-001".to_owned(),
            image: format!("registry/research-runner@sha256:{}", "a".repeat(64)),
            evaluator_version: format!("sha256:{}", "b".repeat(64)),
            resource_profile: RESOURCE_PROFILE.to_owned(),
            mission_url: "https://oss-internal/missions/mission.json?signature=x".to_owned(),
            mission_sha256: "c".repeat(64),
            snapshot_url: "https://oss-internal/snapshots/snapshot.zip?signature=x".to_owned(),
            snapshot_sha256: "d".repeat(64),
            snapshot_contract_id: format!("sha256:{}", "1".repeat(64)),
            result_put_url:
                "https://oss-internal/results/btc-5m-attempt-001/results.zip?signature=x".to_owned(),
            result_readback_url:
                "https://oss-internal/results/btc-5m-attempt-001/results.zip?read-signature=x"
                    .to_owned(),
            resume_url: None,
            resume_sha256: None,
            catalog_partition_artifact: CatalogPartitionArtifactRef {
                path: "catalog/catalog-partition-deadbeef.json".to_owned(),
                artifact_sha256: format!("sha256:{}", "e".repeat(64)),
                payload_sha256: format!("sha256:{}", "f".repeat(64)),
            },
            compiler_source_identity: format!("sha256:{}", "1".repeat(64)),
            build_input_identity: format!("sha256:{}", "2".repeat(64)),
            task_capability: "btc_5m_backtest".to_owned(),
            task: MissionTaskIdentity {
                kind: MissionTaskKind::SettlementProbability,
                side: None,
                prediction_horizon_secs: None,
            },
            cohort_partition_id: format!("sha256:{}", "e".repeat(64)),
        }
    }

    fn admitted_submission_for_test(submission: PredictionSubmission) -> AdmittedSubmission {
        AdmittedSubmission {
            validated: validate_submission(submission).expect("valid test submission"),
            admission: SnapshotAdmission {
                snapshot_contract_id: format!("sha256:{}", "1".repeat(64)),
                snapshot_digest: "0123456789abcdef".to_owned(),
                partition_digest: format!("sha256:{}", "e".repeat(64)),
                policy_identity: format!("sha256:{}", "f".repeat(64)),
                task_capability: "btc_5m_backtest".to_owned(),
                task: MissionTaskIdentity {
                    kind: MissionTaskKind::SettlementProbability,
                    side: None,
                    prediction_horizon_secs: None,
                },
                cohort_partition_id: format!("sha256:{}", "e".repeat(64)),
                cohort_manifest_id: format!("sha256:{}", "d".repeat(64)),
                partition_view: AuthenticatedPartitionView {
                    common_time_boundary_ms: 1,
                    train_market_ids: vec!["train".to_owned()],
                    crossing_excluded_market_ids: Vec::new(),
                    held_out_market_ids: vec!["held-out".to_owned()],
                },
                immutable_image_identity: format!("sha256:{}", "a".repeat(64)),
            },
            run_mode: MissionRunMode::ResearchTrial,
        }
    }

    fn pipeline_smoke_admitted_submission_for_test(
        submission: PredictionSubmission,
    ) -> AdmittedSubmission {
        let mut admitted = admitted_submission_for_test(submission);
        admitted.run_mode = MissionRunMode::PipelineSmoke;
        admitted
    }
}
