use crate::{
    cli::{print_json, PredictionDispatchRenderArgs, PredictionDispatchSubmitArgs},
    mission_runner::normalized_sha256,
};
use anyhow::{bail, Context};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs::File,
    io::{Read, Write},
    path::Path,
    process::{Command, Output, Stdio},
};

const MAX_SUBMISSION_BYTES: u64 = 1024 * 1024;
const RESOURCE_PROFILE: &str = "standard-v1";
const ACTIVE_DEADLINE_SECONDS: u64 = 1800;

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
    result_put_url: String,
    llm_secret_name: String,
    #[serde(default)]
    resume_url: Option<String>,
    #[serde(default)]
    resume_sha256: Option<String>,
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
    result_identity_sha256: String,
    result_identity_label: String,
    job_name: String,
    secret_name: String,
}

#[derive(Debug)]
struct RenderedSubmission {
    manifest: Value,
    job_name: String,
    secret_name: String,
    result_identity_sha256: String,
}

pub fn render(args: PredictionDispatchRenderArgs) -> anyhow::Result<()> {
    let submission = load_submission(&args.submission)?;
    let rendered = render_submission(submission, &args.namespace)?;
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
    let existing = existing_result_jobs(
        &args.context,
        &args.namespace,
        &validated.result_identity_label,
    )?;
    ensure_result_available(&existing)?;
    let rendered = render_validated_submission(validated, &args.namespace)?;
    let secret_body = serde_json::to_vec(&rendered.manifest["items"][0])?;
    let output = kubectl_with_input(
        &args.context,
        &args.namespace,
        ["create", "-f", "-"],
        &secret_body,
    )?;
    ensure_kubectl_success(output, "create immutable prediction input Secret")?;
    let job_body = serde_json::to_vec(&rendered.manifest["items"][1])?;
    let output = kubectl_with_input(
        &args.context,
        &args.namespace,
        ["create", "-f", "-"],
        &job_body,
    )?;
    let recovered_after_create_error = if let Err(error) =
        ensure_kubectl_success(output, "create immutable prediction research Job")
    {
        let readback = existing_result_jobs(
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
                delete_input_secret(&args.context, &args.namespace, &rendered.secret_name)?;
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
    validate_dns_label("LLM secret name", &submission.llm_secret_name)?;
    if submission.resource_profile != RESOURCE_PROFILE {
        bail!("prediction resource profile must be {RESOURCE_PROFILE}");
    }
    let mission_sha256 = normalized_sha256("mission", &submission.mission_sha256)?;
    let snapshot_sha256 = normalized_sha256("snapshot", &submission.snapshot_sha256)?;
    let image_digest = image_digest(&submission.image)?;
    let mission_object = canonical_https_object("mission", &submission.mission_url)?;
    let snapshot_object = canonical_https_object("snapshot", &submission.snapshot_url)?;
    let result_object = canonical_https_object("result", &submission.result_put_url)?;
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
        result_identity_sha256,
        result_identity_label,
        job_name,
        secret_name,
    })
}

fn render_submission(
    submission: PredictionSubmission,
    namespace: &str,
) -> anyhow::Result<RenderedSubmission> {
    validate_dns_label("namespace", namespace)?;
    render_validated_submission(validate_submission(submission)?, namespace)
}

fn render_validated_submission(
    validated: ValidatedSubmission,
    namespace: &str,
) -> anyhow::Result<RenderedSubmission> {
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
        "research.monday/result-object": validated.result_object,
        "research.monday/result-identity-sha256": validated.result_identity_sha256,
        "research.monday/image-digest": validated.image_digest,
        "research.monday/evaluator-version": validated.submission.evaluator_version,
        "research.monday/resource-profile": RESOURCE_PROFILE,
        "research.monday/lane": "prediction_market",
    });
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
                                "args": [
                                    "prediction", "execute", "--work-dir", "/work",
                                    "--mission-url", "$(MISSION_URL)",
                                    "--mission-sha256", "$(MISSION_SHA256)",
                                    "--snapshot-url", "$(SNAPSHOT_URL)",
                                    "--snapshot-sha256", "$(SNAPSHOT_SHA256)",
                                    "--resume-url", "$(RESUME_URL)",
                                    "--resume-sha256", "$(RESUME_SHA256)",
                                    "--result-put-url", "$(RESULT_PUT_URL)"
                                ],
                                "env": prediction_environment(&validated),
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

fn prediction_environment(validated: &ValidatedSubmission) -> Value {
    let secret_ref = |key: &str| {
        json!({
            "name": validated.secret_name,
            "key": key,
        })
    };
    json!([
        { "name": "MISSION_URL", "valueFrom": { "secretKeyRef": secret_ref("mission-url") } },
        { "name": "SNAPSHOT_URL", "valueFrom": { "secretKeyRef": secret_ref("snapshot-url") } },
        { "name": "RESULT_PUT_URL", "valueFrom": { "secretKeyRef": secret_ref("result-put-url") } },
        { "name": "RESUME_URL", "valueFrom": { "secretKeyRef": secret_ref("resume-url") } },
        { "name": "RESUME_SHA256", "valueFrom": { "secretKeyRef": secret_ref("resume-sha256") } },
        { "name": "MISSION_SHA256", "value": validated.mission_sha256 },
        { "name": "SNAPSHOT_SHA256", "value": validated.snapshot_sha256 },
        { "name": "MONDAY_PREDICTION_LLM_BASE_URL", "valueFrom": { "secretKeyRef": { "name": validated.submission.llm_secret_name, "key": "base-url" } } },
        { "name": "MONDAY_PREDICTION_LLM_MODEL", "valueFrom": { "secretKeyRef": { "name": validated.submission.llm_secret_name, "key": "model" } } },
        { "name": "MONDAY_PREDICTION_LLM_API_KEY", "valueFrom": { "secretKeyRef": { "name": validated.submission.llm_secret_name, "key": "api-key", "optional": true } } },
        { "name": "MONDAY_PREDICTION_LLM_PROVIDER", "valueFrom": { "secretKeyRef": { "name": validated.submission.llm_secret_name, "key": "provider", "optional": true } } },
    ])
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

fn canonical_https_object(label: &str, value: &str) -> anyhow::Result<String> {
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

fn result_object_binds_attempt(object: &str, attempt_id: &str) -> anyhow::Result<bool> {
    let url = reqwest::Url::parse(object)?;
    Ok(url
        .path_segments()
        .into_iter()
        .flatten()
        .any(|segment| segment == attempt_id || segment.strip_suffix(".zip") == Some(attempt_id)))
}

fn validate_identifier(label: &str, value: &str) -> anyhow::Result<()> {
    let value = value.trim();
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        bail!("{label} is invalid");
    }
    Ok(())
}

fn validate_dns_label(label: &str, value: &str) -> anyhow::Result<()> {
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

fn kubectl_binary() -> OsString {
    std::env::var_os("MONDAY_KUBECTL_BIN").unwrap_or_else(|| OsString::from("kubectl"))
}

fn existing_result_jobs(
    context: &str,
    namespace: &str,
    result_identity_label: &str,
) -> anyhow::Result<Vec<(String, Value)>> {
    let selector = format!("research.monday/result-id={result_identity_label}");
    let jobs = kubectl_json(
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
    context: &str,
    namespace: &str,
    args: [&str; N],
    action: &str,
) -> anyhow::Result<Value> {
    let output = Command::new(kubectl_binary())
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
    context: &str,
    namespace: &str,
    args: [&str; N],
    input: &[u8],
) -> anyhow::Result<Output> {
    let mut child = Command::new(kubectl_binary())
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

fn delete_input_secret(context: &str, namespace: &str, secret_name: &str) -> anyhow::Result<()> {
    let output = Command::new(kubectl_binary())
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
        let rendered = render_submission(valid_submission(), "monday-research")
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
        assert!(rendered.job_name.starts_with("prediction-"));
    }

    #[test]
    fn rejects_mutable_or_untrimmed_image_references() {
        for image in [
            "registry/research-runner:latest".to_owned(),
            format!(" registry/research-runner@sha256:{}", "a".repeat(64)),
        ] {
            let mut submission = valid_submission();
            submission.image = image;
            assert!(render_submission(submission, "monday-research").is_err());
        }
    }

    #[test]
    fn rejects_an_unbound_snapshot_identity() {
        let mut submission = valid_submission();
        submission.snapshot_sha256.clear();

        let error = render_submission(submission, "monday-research")
            .expect_err("snapshot without an authenticated digest must fail");

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

        let error = render_submission(submission, "monday-research")
            .expect_err("mutable output identity must fail before Job creation");

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
            assert!(render_submission(submission, "monday-research").is_err());
        }
    }

    #[test]
    fn rejects_an_incomplete_resume_pair() {
        let mut submission = valid_submission();
        submission.resume_url =
            Some("https://oss-internal/results/prior.zip?signature=x".to_owned());

        let error = render_submission(submission, "monday-research")
            .expect_err("incomplete resume pair must fail");

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
            result_put_url:
                "https://oss-internal/results/btc-5m-attempt-001/results.zip?signature=x".to_owned(),
            llm_secret_name: "monday-prediction-llm".to_owned(),
            resume_url: None,
            resume_sha256: None,
        }
    }
}
