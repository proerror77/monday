use crate::{
    cli::{print_json, MissionDispatchRenderArgs},
    mission_runner::normalized_sha256,
    prediction_dispatch::{
        canonical_https_object, result_object_binds_attempt, validate_dns_label,
    },
};
use anyhow::{bail, Context};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const MAX_SUBMISSION_BYTES: u64 = 1024 * 1024;
const ACTIVE_DEADLINE_SECONDS: u64 = 900;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MissionDispatchSubmission {
    attempt_id: String,
    image: String,
    mission_url: String,
    mission_sha256: String,
    feature_url: String,
    materialization_url: String,
    replay_artifact_url: String,
    replay_artifact_sha256: String,
    replay_manifest_url: String,
    replay_manifest_sha256: String,
    result_put_url: String,
    result_readback_url: String,
    #[serde(default)]
    resume_url: Option<String>,
    #[serde(default)]
    resume_sha256: Option<String>,
}

#[derive(Debug)]
struct ValidatedSubmission {
    submission: MissionDispatchSubmission,
    image_digest: String,
    mission_sha256: String,
    replay_artifact_sha256: String,
    replay_manifest_sha256: String,
    mission_object: String,
    feature_object: String,
    materialization_object: String,
    replay_artifact_object: String,
    replay_manifest_object: String,
    result_object: String,
    result_readback_object: String,
    resume_sha256: Option<String>,
    result_identity_sha256: String,
    result_identity_label: String,
    job_name: String,
    secret_name: String,
}

pub fn render(args: MissionDispatchRenderArgs) -> anyhow::Result<()> {
    let submission = load_submission(&args.submission)?;
    let validated = validate_submission(submission)?;
    let manifest = render_manifest(validated, &args.namespace)?;
    print_json(&manifest)
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
    let image_digest = image_digest(&submission.image)?;
    let mission_sha256 = normalized_sha256("mission", &submission.mission_sha256)?;
    let replay_artifact_sha256 =
        normalized_sha256("replay artifact", &submission.replay_artifact_sha256)?;
    let replay_manifest_sha256 =
        normalized_sha256("replay manifest", &submission.replay_manifest_sha256)?;
    let mission_object = canonical_https_object("mission", &submission.mission_url)?;
    let feature_object = canonical_https_object("feature", &submission.feature_url)?;
    let materialization_object =
        canonical_https_object("materialization", &submission.materialization_url)?;
    let replay_artifact_object =
        canonical_https_object("replay artifact", &submission.replay_artifact_url)?;
    let replay_manifest_object =
        canonical_https_object("replay manifest", &submission.replay_manifest_url)?;
    let result_object = canonical_https_object("result", &submission.result_put_url)?;
    let result_readback_object =
        canonical_https_object("result readback", &submission.result_readback_url)?;
    if result_object != result_readback_object {
        bail!("result readback URL must identify the same immutable result object");
    }
    if !result_object_binds_attempt(&result_object, &submission.attempt_id)? {
        bail!("result object must bind the exact attempt id");
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
        _ => bail!("mission resume URL and SHA256 must be supplied together"),
    };
    let result_identity_sha256 = sha256_text(&result_object);
    let result_identity_label = result_identity_sha256[..32].to_string();
    let job_name = format!("alpha-mission-{result_identity_label}");
    let secret_name = format!("{job_name}-inputs");
    Ok(ValidatedSubmission {
        submission,
        image_digest,
        mission_sha256,
        replay_artifact_sha256,
        replay_manifest_sha256,
        mission_object,
        feature_object,
        materialization_object,
        replay_artifact_object,
        replay_manifest_object,
        result_object,
        result_readback_object,
        resume_sha256,
        result_identity_sha256,
        result_identity_label,
        job_name,
        secret_name,
    })
}

fn render_manifest(validated: ValidatedSubmission, namespace: &str) -> anyhow::Result<Value> {
    validate_dns_label("namespace", namespace)?;
    let resume_url = validated.submission.resume_url.as_deref().unwrap_or("");
    let resume_sha256 = validated.resume_sha256.as_deref().unwrap_or("");
    let labels = json!({
        "app.kubernetes.io/name": "monday-alpha-mission",
        "app.kubernetes.io/part-of": "monday",
        "research.monday/result-id": validated.result_identity_label,
    });
    let annotations = json!({
        "research.monday/attempt-id": validated.submission.attempt_id,
        "research.monday/mission-sha256": validated.mission_sha256,
        "research.monday/mission-object": validated.mission_object,
        "research.monday/feature-object": validated.feature_object,
        "research.monday/materialization-object": validated.materialization_object,
        "research.monday/replay-artifact-object": validated.replay_artifact_object,
        "research.monday/replay-artifact-sha256": validated.replay_artifact_sha256,
        "research.monday/replay-manifest-object": validated.replay_manifest_object,
        "research.monday/replay-manifest-sha256": validated.replay_manifest_sha256,
        "research.monday/result-object": validated.result_object,
        "research.monday/result-readback-object": validated.result_readback_object,
        "research.monday/result-identity-sha256": validated.result_identity_sha256,
        "research.monday/image-digest": validated.image_digest,
        "research.monday/lane": "cex_research",
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
                        "research.monday/result-identity-sha256": validated.result_identity_sha256,
                        "research.monday/attempt-id": validated.submission.attempt_id,
                    }
                },
                "type": "Opaque",
                "immutable": true,
                "stringData": {
                    "mission-url": validated.submission.mission_url,
                    "feature-url": validated.submission.feature_url,
                    "materialization-url": validated.submission.materialization_url,
                    "replay-artifact-url": validated.submission.replay_artifact_url,
                    "replay-manifest-url": validated.submission.replay_manifest_url,
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
                                "seccompProfile": { "type": "RuntimeDefault" }
                            },
                            "containers": [{
                                "name": "alpha-mission",
                                "image": validated.submission.image,
                                "imagePullPolicy": "IfNotPresent",
                                "command": ["/usr/local/bin/alpha-harness"],
                                "args": [
                                    "mission",
                                    "execute",
                                    "--work-dir", "/work",
                                    "--mission-url", "$(MISSION_URL)",
                                    "--mission-sha256", "$(MISSION_SHA256)",
                                    "--feature-url", "$(FEATURE_URL)",
                                    "--materialization-url", "$(MATERIALIZATION_URL)",
                                    "--replay-artifact-url", "$(REPLAY_ARTIFACT_URL)",
                                    "--replay-artifact-sha256", "$(REPLAY_ARTIFACT_SHA256)",
                                    "--replay-manifest-url", "$(REPLAY_MANIFEST_URL)",
                                    "--replay-manifest-sha256", "$(REPLAY_MANIFEST_SHA256)",
                                    "--resume-url", "$(RESUME_URL)",
                                    "--resume-sha256", "$(RESUME_SHA256)",
                                    "--result-put-url", "$(RESULT_PUT_URL)",
                                    "--result-readback-url", "$(RESULT_READBACK_URL)"
                                ],
                                "env": [
                                    secret_env("MISSION_URL", &validated.secret_name, "mission-url"),
                                    json!({ "name": "MISSION_SHA256", "value": validated.mission_sha256 }),
                                    secret_env("FEATURE_URL", &validated.secret_name, "feature-url"),
                                    secret_env("MATERIALIZATION_URL", &validated.secret_name, "materialization-url"),
                                    secret_env("REPLAY_ARTIFACT_URL", &validated.secret_name, "replay-artifact-url"),
                                    json!({ "name": "REPLAY_ARTIFACT_SHA256", "value": validated.replay_artifact_sha256 }),
                                    secret_env("REPLAY_MANIFEST_URL", &validated.secret_name, "replay-manifest-url"),
                                    json!({ "name": "REPLAY_MANIFEST_SHA256", "value": validated.replay_manifest_sha256 }),
                                    secret_env("RESUME_URL", &validated.secret_name, "resume-url"),
                                    secret_env("RESUME_SHA256", &validated.secret_name, "resume-sha256"),
                                    secret_env("RESULT_PUT_URL", &validated.secret_name, "result-put-url"),
                                    secret_env("RESULT_READBACK_URL", &validated.secret_name, "result-readback-url")
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
                                    { "name": "tmp", "mountPath": "/tmp" }
                                ]
                            }],
                            "volumes": [
                                { "name": "work", "emptyDir": { "sizeLimit": "20Gi" } },
                                { "name": "tmp", "emptyDir": {} }
                            ]
                        }
                    }
                }
            }
        ]
    }))
}

fn secret_env(name: &str, secret_name: &str, key: &str) -> Value {
    json!({
        "name": name,
        "valueFrom": {
            "secretKeyRef": {
                "name": secret_name,
                "key": key
            }
        }
    })
}

fn image_digest(image: &str) -> anyhow::Result<String> {
    if image != image.trim() || image.chars().any(char::is_control) {
        bail!("mission image must not contain surrounding whitespace or control characters");
    }
    let (repository, digest) = image
        .rsplit_once("@sha256:")
        .context("mission image must be pinned by @sha256 digest")?;
    if repository.is_empty() {
        bail!("mission image repository is required");
    }
    normalized_sha256("mission image", digest)
}

fn sha256_text(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_is_stable_for_the_same_query_free_result_identity() {
        let first = validate_submission(valid_submission(
            "https://oss-internal/results/attempt-001/results.zip?signature=first",
            "https://oss-internal/results/attempt-001/results.zip?signature=second",
        ))
        .unwrap();
        let second = validate_submission(valid_submission(
            "https://oss-internal/results/attempt-001/results.zip?signature=third",
            "https://oss-internal/results/attempt-001/results.zip?signature=fourth",
        ))
        .unwrap();

        assert_eq!(first.job_name, second.job_name);
        assert_eq!(first.secret_name, second.secret_name);
        assert_eq!(first.result_identity_sha256, second.result_identity_sha256);
    }

    #[test]
    fn render_rejects_result_identity_drift() {
        let error = validate_submission(valid_submission(
            "https://oss-internal/results/attempt-001/results.zip",
            "https://oss-internal/results/attempt-001/other.zip",
        ))
        .unwrap_err();
        assert!(format!("{error:#}").contains("same immutable result object"));
    }

    #[test]
    fn render_rejects_attempt_identity_drift() {
        let mut submission = valid_submission(
            "https://oss-internal/results/attempt-001/results.zip",
            "https://oss-internal/results/attempt-001/results.zip",
        );
        submission.attempt_id = "attempt-002".to_string();
        let error = validate_submission(submission).unwrap_err();
        assert!(format!("{error:#}").contains("exact attempt id"));
    }

    fn valid_submission(
        result_put_url: &str,
        result_readback_url: &str,
    ) -> MissionDispatchSubmission {
        MissionDispatchSubmission {
            attempt_id: "attempt-001".to_string(),
            image: "registry/research-runner@sha256:1111111111111111111111111111111111111111111111111111111111111111".to_string(),
            mission_url: "https://oss-internal/missions/mission.json?signature=x".to_string(),
            mission_sha256: "2".repeat(64),
            feature_url: "https://oss-internal/features/features.jsonl?signature=x".to_string(),
            materialization_url: "https://oss-internal/materialization/report.json?signature=x".to_string(),
            replay_artifact_url: "https://oss-internal/replay/data.parquet?signature=x".to_string(),
            replay_artifact_sha256: "3".repeat(64),
            replay_manifest_url: "https://oss-internal/replay/manifest.json?signature=x".to_string(),
            replay_manifest_sha256: "4".repeat(64),
            result_put_url: result_put_url.to_string(),
            result_readback_url: result_readback_url.to_string(),
            resume_url: None,
            resume_sha256: None,
        }
    }
}
