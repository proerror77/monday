use crate::{
    cli::{print_json, MissionDispatchSubmitArgs},
    mission_campaign::{serialize_request, validate_request, CampaignRequest},
    mission_runner::normalized_sha256,
    prediction_dispatch::{
        ensure_kubectl_success, kubectl_binary, kubectl_json, kubectl_with_input,
        validate_cluster_target, validate_dns_label,
    },
};
use anyhow::{bail, Context};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const MAX_SUBMISSION_BYTES: u64 = 1024 * 1024;
const ACTIVE_DEADLINE_SECONDS: u64 = 3600;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MissionDispatchSubmission {
    attempt_id: String,
    image: String,
    request: CampaignRequest,
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

pub fn submit(args: MissionDispatchSubmitArgs) -> anyhow::Result<()> {
    validate_cluster_target(&args.context, &args.namespace)?;
    let submission = load_submission(&args.submission)?;
    let validated = validate_submission(submission)?;
    let job_name = validated.job_name.clone();
    let secret_name = validated.secret_name.clone();
    let campaign_id = validated.submission.request.campaign_id.clone();
    let request_sha256 = validated.request_sha256.clone();
    let manifest = render_manifest(validated, &args.namespace)?;
    let kubectl = kubectl_binary();

    let secret_body = serde_json::to_vec(&manifest["items"][0])?;
    let output = kubectl_with_input(
        &kubectl,
        &args.context,
        &args.namespace,
        ["create", "-f", "-"],
        &secret_body,
    )?;
    ensure_kubectl_success(output, "create immutable CEX Campaign input Secret")?;

    let job_body = serde_json::to_vec(&manifest["items"][1])?;
    let job_create = kubectl_with_input(
        &kubectl,
        &args.context,
        &args.namespace,
        ["create", "-f", "-"],
        &job_body,
    )
    .and_then(|output| {
        ensure_kubectl_success(output, "create immutable CEX Campaign Job").map(|_| ())
    });
    if let Err(error) = job_create {
        let cleanup =
            delete_campaign_secret(&kubectl, &args.context, &args.namespace, &secret_name);
        return match cleanup {
            Ok(()) => Err(error),
            Err(cleanup_error) => Err(error.context(format!(
                "input Secret {secret_name} retained for reconciliation: {cleanup_error:#}"
            ))),
        };
    }

    let result = (|| -> anyhow::Result<()> {
        let job = kubectl_json(
            &kubectl,
            &args.context,
            &args.namespace,
            ["get", "job", &job_name, "-o", "json"],
            "read back immutable CEX Campaign Job",
        )?;
        validate_job_readback(&job, &job_name, &request_sha256, true)?;
        let job_uid = job["metadata"]["uid"]
            .as_str()
            .context("CEX Campaign Job readback is missing its UID")?;
        let owner_patch = job_owner_patch(&job_name, job_uid)?;
        let owner_patch_json = serde_json::to_string(&owner_patch)?;
        let output = kubectl_with_input(
            &kubectl,
            &args.context,
            &args.namespace,
            [
                "patch",
                "secret",
                &secret_name,
                "--type=merge",
                "--patch",
                &owner_patch_json,
            ],
            &[],
        )?;
        ensure_kubectl_success(output, "bind CEX Campaign input Secret to its TTL Job")?;
        let secret = kubectl_json(
            &kubectl,
            &args.context,
            &args.namespace,
            ["get", "secret", &secret_name, "-o", "json"],
            "read back immutable CEX Campaign input Secret",
        )?;
        validate_secret_readback(&secret, &secret_name, &request_sha256, &job_name, job_uid)?;
        let release_patch_json = serde_json::to_string(&release_job_patch())?;
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
        validate_job_readback(&released_job, &job_name, &request_sha256, false)?;
        if released_job["metadata"]["uid"] != job_uid {
            bail!("released CEX Campaign Job readback does not match the created Job UID");
        }
        Ok(())
    })();
    if let Err(error) = result {
        let cleanup = delete_campaign_objects(
            &kubectl,
            &args.context,
            &args.namespace,
            &job_name,
            &secret_name,
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

fn job_owner_patch(job_name: &str, job_uid: &str) -> anyhow::Result<Value> {
    if job_uid.trim().is_empty() || job_uid.chars().any(char::is_control) {
        bail!("CEX Campaign Job UID is invalid");
    }
    Ok(json!({
        "metadata": {
            "ownerReferences": [{
                "apiVersion": "batch/v1",
                "kind": "Job",
                "name": job_name,
                "uid": job_uid,
                "controller": false,
                "blockOwnerDeletion": false,
            }]
        }
    }))
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
    job_name: &str,
    request_sha256: &str,
    suspended: bool,
) -> anyhow::Result<()> {
    if job["metadata"]["name"] != job_name
        || job["metadata"]["annotations"]["research.monday/request-sha256"] != request_sha256
        || job["spec"]["suspend"] != suspended
    {
        bail!("CEX Campaign Job readback does not match the submitted identity");
    }
    Ok(())
}

fn validate_secret_readback(
    secret: &Value,
    secret_name: &str,
    request_sha256: &str,
    job_name: &str,
    job_uid: &str,
) -> anyhow::Result<()> {
    let owner = &secret["metadata"]["ownerReferences"][0];
    if secret["metadata"]["name"] != secret_name
        || secret["immutable"] != true
        || secret["metadata"]["annotations"]["research.monday/request-sha256"] != request_sha256
        || secret["data"]["campaign.json"]
            .as_str()
            .is_none_or(str::is_empty)
        || owner["apiVersion"] != "batch/v1"
        || owner["kind"] != "Job"
        || owner["name"] != job_name
        || owner["uid"] != job_uid
    {
        bail!("CEX Campaign input Secret readback does not match the submitted identity");
    }
    Ok(())
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

fn delete_campaign_objects(
    kubectl: &std::path::Path,
    context: &str,
    namespace: &str,
    job_name: &str,
    secret_name: &str,
) -> anyhow::Result<()> {
    let output = kubectl_with_input(
        kubectl,
        context,
        namespace,
        ["delete", "job", job_name, "--ignore-not-found=true"],
        &[],
    )?;
    ensure_kubectl_success(output, "clean up incomplete CEX Campaign submission")?;
    delete_campaign_secret(kubectl, context, namespace, secret_name)
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
        let owner = job_owner_patch("alpha-campaign-test", "job-uid").unwrap();

        assert_eq!(
            owner["metadata"]["ownerReferences"][0]["name"],
            "alpha-campaign-test"
        );
        assert_eq!(owner["metadata"]["ownerReferences"][0]["uid"], "job-uid");
    }

    #[test]
    fn campaign_secret_readback_binds_request_and_job() {
        let secret = json!({
            "metadata": {
                "name": "alpha-campaign-test-inputs",
                "annotations": { "research.monday/request-sha256": "request-sha" },
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
            "request-sha",
            "alpha-campaign-test",
            "job-uid",
        )
        .unwrap();
    }

    #[test]
    fn release_patch_clears_job_suspend() {
        assert_eq!(release_job_patch()["spec"]["suspend"], false);
    }

    #[test]
    fn job_readback_requires_expected_suspend_state() {
        let job = json!({
            "metadata": {
                "name": "alpha-campaign-test",
                "annotations": { "research.monday/request-sha256": "request-sha" },
            },
            "spec": {
                "suspend": true,
            },
        });

        validate_job_readback(&job, "alpha-campaign-test", "request-sha", true).unwrap();
        assert!(validate_job_readback(&job, "alpha-campaign-test", "request-sha", false).is_err());
    }

    fn valid_submission() -> MissionDispatchSubmission {
        MissionDispatchSubmission {
            attempt_id: "attempt1".to_string(),
            image: "registry/research-runner@sha256:1111111111111111111111111111111111111111111111111111111111111111".to_string(),
            request: valid_request_for_tests(),
        }
    }
}
