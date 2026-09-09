//! Final evaluation reuses canonical Job creation, identity readback and receipts.
use super::*;
use crate::mission_campaign::final_evaluation::FinalRequest;
use alpha_domain::campaign_finalization::{
    verify_campaign_final_evaluation_grant, SignedCampaignFinalEvaluationGrantV1,
    VerifiedCampaignFinalEvaluationGrant,
};
use alpha_domain::canonical_json_hash;
use alpha_store::{
    campaign_ledger::{
        CampaignDispatchClaimV1, CampaignDispatchTargetV1, CampaignFinalDispatchRecord,
        CampaignFinalDispatchSettlementV1,
    },
    AlphaStore,
};
use chrono::Utc;
use std::{collections::BTreeMap, path::PathBuf, time::Duration};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FinalControl {
    schema_version: String,
    ledger_path: PathBuf,
    signed_final_grant_path: PathBuf,
    trusted_keys_path: PathBuf,
    materialization_path: PathBuf,
    controller_image: String,
    source_submissions: BTreeMap<String, PathBuf>,
    receipt_access: BTreeMap<String, admission::ReceiptAccess>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Purpose {
    FinalEvaluation,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FinalSubmission {
    purpose: Purpose,
    attempt_id: String,
    image: String,
    request: FinalRequest,
}

struct ValidatedFinalSubmission {
    submission: FinalSubmission,
    request_sha256: String,
    request_json: String,
    submission_identity_sha256: String,
    job_name: String,
    secret_name: String,
}

pub(crate) fn read_bounded_json<T: serde::de::DeserializeOwned>(path: &Path) -> anyhow::Result<T> {
    admission::read_json(path)
}

fn read_control(path: &Path) -> anyhow::Result<FinalControl> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let base = path.parent().context("final control lacks parent")?;
    let mut control: FinalControl = read_bounded_json(&path)?;
    if control.schema_version != "monday.campaign_final_dispatch_control.v1" {
        bail!("unsupported final dispatch control");
    }
    for location in [
        &mut control.ledger_path,
        &mut control.signed_final_grant_path,
        &mut control.materialization_path,
    ]
    .into_iter()
    .chain(control.source_submissions.values_mut())
    {
        if location.is_relative() {
            *location = base.join(&*location);
        }
        *location = location
            .canonicalize()
            .context("resolve existing final control input")?;
    }
    if !control.ledger_path.is_file() {
        bail!("final control requires an existing ledger");
    }
    if control.trusted_keys_path.is_relative() {
        control.trusted_keys_path = base.join(&control.trusted_keys_path);
    }
    Ok(control)
}

fn verified(control: &FinalControl) -> anyhow::Result<VerifiedCampaignFinalEvaluationGrant> {
    let signed: SignedCampaignFinalEvaluationGrantV1 =
        read_bounded_json(&control.signed_final_grant_path)?;
    Ok(verify_campaign_final_evaluation_grant(
        &signed,
        &admission::read_trusted_keys(&control.trusted_keys_path)?,
        Utc::now(),
    )?)
}

pub(crate) fn read_active_control(
    path: &Path,
) -> anyhow::Result<(FinalControl, VerifiedCampaignFinalEvaluationGrant)> {
    let control = read_control(path)?;
    let grant = verified(&control)?;
    let store = AlphaStore::open(&control.ledger_path)?;
    store.inspect_campaign_final_authority(&grant, Utc::now())?;
    if control.controller_image != grant.grant().execution.controller_image {
        bail!("final controller image changed");
    }
    Ok((control, grant))
}

pub(crate) fn verified_source_requests(
    control: &FinalControl,
    grant: &VerifiedCampaignFinalEvaluationGrant,
) -> anyhow::Result<BTreeMap<String, CampaignRequest>> {
    let store = AlphaStore::open(&control.ledger_path)?;
    if control.source_submissions.keys().collect::<Vec<_>>()
        != grant.grant().selected_results.keys().collect::<Vec<_>>()
    {
        bail!("final sources differ from complete closed family");
    }
    let mut requests = BTreeMap::new();
    for (index, (operation, path)) in control.source_submissions.iter().enumerate() {
        let source = validate_submission(load_submission(path)?)?;
        let record = store.campaign_dispatch_record(&grant.grant().family_id, operation)?;
        let settlement = record.settlement.context("final source is unsettled")?;
        if record.reservation.request_sha256 != source.request_sha256
            || record.terminal_pod_uid.is_none()
            || record.reservation.execution != grant.grant().execution
            || grant.grant().selected_results.get(operation) != Some(&settlement.evidence_sha256)
            || settlement.outcome
                != alpha_domain::campaign_control::CampaignAttemptOutcomeV1::SelectedPreHoldout
        {
            bail!("final source differs from canonical settled evidence");
        }
        if index == 0 {
            let inspection = admission::inspect_binding(
                &source,
                &render_manifest(&source, "monday-research")?,
                &control.materialization_path,
                &control.controller_image,
                record.reservation.attempt_ordinal,
            )?;
            if inspection.execution != grant.grant().execution {
                bail!("final materialization or evaluation view changed");
            }
        }
        requests.insert(operation.clone(), source.submission.request);
    }
    Ok(requests)
}

fn validate(submission: FinalSubmission) -> anyhow::Result<ValidatedFinalSubmission> {
    validate_dns_label("final attempt id", &submission.attempt_id)?;
    submission.request.validate()?;
    if submission.image != submission.request.grant.grant.execution.runner_image
        || image_digest(&submission.image)? != submission.request.image_identity
    {
        bail!("final runner image differs from authority");
    }
    let request_json = serde_json::to_string_pretty(&submission.request)?;
    if request_json.len() as u64 > MAX_SUBMISSION_BYTES {
        bail!("final submission exceeds byte budget");
    }
    let request_sha256 = sha256_text(&request_json);
    let submission_identity_sha256 = sha256_text(&format!(
        "final:{}:{}",
        submission.attempt_id, submission.request.campaign_id
    ));
    let job_name = format!("alpha-final-{}", &submission_identity_sha256[..32]);
    let secret_name = format!("{job_name}-inputs");
    Ok(ValidatedFinalSubmission {
        submission,
        request_sha256,
        request_json,
        submission_identity_sha256,
        job_name,
        secret_name,
    })
}

pub(crate) fn write_submission(
    path: &Path,
    attempt: &str,
    image: &str,
    request: FinalRequest,
) -> anyhow::Result<SubmissionRenderReport> {
    let validated = validate(FinalSubmission {
        purpose: Purpose::FinalEvaluation,
        attempt_id: attempt.into(),
        image: image.into(),
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

pub(super) fn is_final_submission(path: &Path) -> anyhow::Result<bool> {
    let value: Value = read_bounded_json(path)?;
    Ok(value["purpose"] == "final_evaluation")
}

fn render(
    validated: &ValidatedFinalSubmission,
    grant: &VerifiedCampaignFinalEvaluationGrant,
    namespace: &str,
) -> anyhow::Result<Value> {
    let keys = serde_json::to_string(&BTreeMap::from([(
        grant.signed_grant().key_id.clone(),
        hex::encode(grant.verifying_key().as_bytes()),
    )]))?;
    render_campaign_manifest(
        DispatchManifestInput {
            attempt_id: &validated.submission.attempt_id,
            campaign_id: &validated.submission.request.campaign_id,
            job_name: &validated.job_name,
            secret_name: &validated.secret_name,
            image: &validated.submission.image,
            image_digest: &validated.submission.request.image_identity,
            request_sha256: &validated.request_sha256,
            request_json: &validated.request_json,
            submission_identity_sha256: &validated.submission_identity_sha256,
            trusted_keys_json: Some(&keys),
            active_deadline_seconds: grant.grant().max_job_seconds,
            args: vec![
                "mission".into(),
                "campaign-execute".into(),
                "--final-evaluation".into(),
                "--final-trusted-keys".into(),
                "/inputs/final-trusted-keys.json".into(),
                "--work-dir".into(),
                "/work".into(),
                "--campaign-id".into(),
                validated.submission.request.campaign_id.clone(),
                "--image-identity".into(),
                validated.submission.request.image_identity.clone(),
                "--request".into(),
                "/inputs/campaign.json".into(),
                "--request-sha256".into(),
                validated.request_sha256.clone(),
            ],
        },
        namespace,
    )
}

struct FinalAdmission {
    control: FinalControl,
    grant: VerifiedCampaignFinalEvaluationGrant,
    store: AlphaStore,
    request_sha256: String,
    target: CampaignDispatchTargetV1,
    receipt_origin: String,
    historical: bool,
}

impl FinalAdmission {
    fn open(
        control_path: &Path,
        validated: &ValidatedFinalSubmission,
        context: &str,
        namespace: &str,
        historical: bool,
    ) -> anyhow::Result<(Self, Value)> {
        let control = read_control(control_path)?;
        let store = AlphaStore::open(&control.ledger_path)?;
        let record = store
            .campaign_final_dispatch_record(&validated.submission.request.grant.grant.family_id)?;
        let grant = if historical {
            record.grant
        } else {
            verified(&control)?
        };
        if grant.signed_grant() != &validated.submission.request.grant
            || grant.grant().execution.controller_image != control.controller_image
        {
            bail!("final request authority differs from closed family");
        }
        if !historical {
            store.inspect_campaign_final_authority(&grant, Utc::now())?;
            if verified_source_requests(&control, &grant)? != validated.submission.request.sources {
                bail!("final request rewrote its settled sources");
            }
        }
        let manifest = render(validated, &grant, namespace)?;
        let container = &manifest["items"][1]["spec"]["template"]["spec"]["containers"][0];
        if grant.grant().execution.job_cpu_millis != 3500
            || grant.grant().execution.job_memory_mib != 12 * 1024
            || container["image"] != grant.grant().execution.runner_image
        {
            bail!("final Job resources differ from closed-family binding");
        }
        let target = CampaignDispatchTargetV1 {
            context: context.into(),
            namespace: namespace.into(),
            job_name: validated.job_name.clone(),
            manifest_sha256: canonical_json_hash(&manifest)?,
        };
        target.validate()?;
        let receipt_origin = reqwest::Url::parse(&validated.submission.request.output_root)?
            .origin()
            .ascii_serialization();
        let admission = Self {
            control,
            grant,
            store,
            request_sha256: validated.request_sha256.clone(),
            target,
            receipt_origin,
            historical,
        };
        if historical {
            admission.record()?;
        }
        Ok((admission, manifest))
    }

    fn record(&self) -> anyhow::Result<CampaignFinalDispatchRecord> {
        let record = self
            .store
            .campaign_final_dispatch_record(&self.grant.grant().family_id)?;
        if record.claim.as_ref().is_none_or(|claim| {
            claim.request_sha256 != self.request_sha256 || claim.dispatch.target != self.target
        }) {
            bail!("final dispatch record changed");
        }
        Ok(record)
    }
}

impl DispatchAdmission for FinalAdmission {
    fn prepare(&mut self) -> anyhow::Result<()> {
        if self.historical {
            bail!("settlement authority cannot dispatch");
        }
        self.grant = verified(&self.control)?;
        self.store
            .inspect_campaign_final_authority(&self.grant, Utc::now())?;
        Ok(())
    }
    fn publish_receipts(&mut self) -> anyhow::Result<()> {
        if self.historical {
            self.record()?;
        } else {
            self.prepare()?;
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        admission::publish_family_receipts_with(
            &mut self.store,
            &self.grant.grant().family_id,
            &self.receipt_origin,
            &self.control.receipt_access,
            |access, bytes| admission::publish_and_readback(&client, access, bytes),
        )
    }
    fn claim(&mut self) -> anyhow::Result<(CampaignDispatchClaimV1, bool)> {
        self.prepare()?;
        let (claim, first) = self.store.claim_campaign_final_dispatch(
            &self.grant,
            &self.request_sha256,
            &self.target,
            Utc::now(),
        )?;
        Ok((claim.dispatch, first))
    }
    fn bind_job(&mut self, uid: &str) -> anyhow::Result<()> {
        self.prepare()?;
        self.store.bind_campaign_final_dispatch_job(
            &self.grant,
            &self.request_sha256,
            &self.target,
            uid,
            Utc::now(),
        )?;
        Ok(())
    }
    fn guarded<T>(
        &mut self,
        uid: Option<&str>,
        action: impl FnOnce() -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        self.prepare()?;
        self.store.with_campaign_final_dispatch_admission(
            &self.grant,
            &self.request_sha256,
            &self.target,
            uid,
            || Utc::now() + chrono::TimeDelta::seconds(30),
            action,
        )
    }
}

pub(super) fn submit(args: MissionDispatchSubmitArgs) -> anyhow::Result<()> {
    validate_cluster_target(&args.context, &args.namespace)?;
    let validated = validate(read_bounded_json(&args.submission)?)?;
    let control = args
        .control
        .clone()
        .or_else(|| std::env::var_os("MONDAY_CAMPAIGN_CONTROL").map(Into::into))
        .context("final dispatch requires control")?;
    let (mut admission, manifest) =
        FinalAdmission::open(&control, &validated, &args.context, &args.namespace, false)?;
    submit_rendered_job(
        &args,
        RenderedDispatch {
            job_name: validated.job_name.clone(),
            secret_name: validated.secret_name,
            request_sha256: validated.request_sha256.clone(),
            request_json: validated.request_json,
            manifest,
        },
        &mut admission,
    )?;
    print_json(
        &json!({"status":"submitted", "execution_scope":"final_evaluation", "campaign_id":validated.submission.request.campaign_id,
        "family_id":admission.grant.grant().family_id, "request_sha256":validated.request_sha256, "job_name":validated.job_name,
        "reserved_job_seconds":admission.grant.grant().max_job_seconds, "max_candidates":admission.grant.grant().max_candidates}),
    )
}

pub(super) fn inspect(args: MissionDispatchInspectArgs) -> anyhow::Result<()> {
    let path = args
        .control
        .as_deref()
        .context("final inspection requires --control")?;
    let validated = validate(read_bounded_json(&args.submission)?)?;
    let (admission, _) =
        FinalAdmission::open(path, &validated, "inspection", "monday-research", false)?;
    if args.materialization.canonicalize()? != admission.control.materialization_path
        || args.controller_image != admission.control.controller_image
        || args.attempt_ordinal != 0
    {
        bail!("final inspection input, image or attempt scope changed");
    }
    print_json(
        &json!({"execution_scope":"final_evaluation", "execution":admission.grant.grant().execution,
        "family_id":admission.grant.grant().family_id, "final_grant_sha256":admission.grant.content_sha256(),
        "request_sha256":validated.request_sha256, "max_candidates":admission.grant.grant().max_candidates,
        "reserved_job_seconds":admission.grant.grant().max_job_seconds, "job_name":validated.job_name}),
    )
}

pub(crate) fn verify_worker_grant(
    signed: &SignedCampaignFinalEvaluationGrantV1,
    keys: &Path,
) -> anyhow::Result<VerifiedCampaignFinalEvaluationGrant> {
    Ok(verify_campaign_final_evaluation_grant(
        signed,
        &admission::read_trusted_keys(keys)?,
        Utc::now(),
    )?)
}

pub(crate) fn source_execution_binding(
    source: &CampaignRequest,
    materialization: &Path,
    image: &str,
    controller_image: &str,
) -> anyhow::Result<alpha_domain::campaign_control::CampaignExecutionBindingV1> {
    let source = validate_submission_with_request_check(
        MissionDispatchSubmission {
            attempt_id: "final-source-binding".into(),
            image: image.into(),
            request: source.clone(),
        },
        crate::mission_campaign::validate_request_for_execute,
    )?;
    Ok(admission::inspect_binding(
        &source,
        &render_manifest(&source, "monday-research")?,
        materialization,
        controller_image,
        0,
    )?
    .execution)
}

pub(crate) fn validate_worker_dataset_binding(
    request: &FinalRequest,
    materialization: &Path,
) -> anyhow::Result<()> {
    if source_execution_binding(
        request.first_source()?,
        materialization,
        &request.grant.grant.execution.runner_image,
        &request.grant.grant.execution.controller_image,
    )? != request.grant.grant.execution
    {
        bail!("final worker input views differ from signed authority");
    }
    Ok(())
}

pub(super) fn settle(args: MissionDispatchSubmitArgs) -> anyhow::Result<()> {
    validate_cluster_target(&args.context, &args.namespace)?;
    let validated = validate(read_bounded_json(&args.submission)?)?;
    let control = args
        .control
        .clone()
        .or_else(|| std::env::var_os("MONDAY_CAMPAIGN_CONTROL").map(Into::into))
        .context("final settlement requires control")?;
    let (mut admission, manifest) =
        FinalAdmission::open(&control, &validated, &args.context, &args.namespace, true)?;
    let record = admission.record()?;
    if record.settlement.is_none() {
        let claim = record.claim.context("final Job claim is missing")?;
        let uid = claim
            .dispatch
            .job_uid
            .as_deref()
            .context("final Job UID is not bound")?;
        let readback = terminal::read_terminal_job(
            &args.context,
            &args.namespace,
            &manifest["items"][1],
            &validated.job_name,
            uid,
        )?;
        let termination = &readback.pod["status"]["containerStatuses"][0]["state"]["terminated"];
        let started = chrono::DateTime::parse_from_rfc3339(
            termination["startedAt"]
                .as_str()
                .context("final Pod start time missing")?,
        )?;
        let finished = chrono::DateTime::parse_from_rfc3339(
            termination["finishedAt"]
                .as_str()
                .context("final Pod finish time missing")?,
        )?;
        let nanoseconds = finished
            .signed_duration_since(started)
            .num_nanoseconds()
            .context("final Pod duration overflow")?;
        let consumed_job_seconds = u64::try_from(nanoseconds)
            .context("final Pod finished before it started")?
            .div_ceil(1_000_000_000);
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(120))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let (result, result_sha256) = crate::mission_campaign::final_evaluation::readback_terminal(
            &client,
            &validated.submission.request,
            &validated.request_sha256,
            &admission.grant,
        )?;
        admission.store.settle_campaign_final_dispatch(
            &admission.grant.grant().family_id,
            &CampaignFinalDispatchSettlementV1 {
                request_sha256: validated.request_sha256.clone(),
                job_uid: readback.job_uid,
                pod_uid: readback.pod_uid,
                result_sha256,
                outcome: result.outcome,
                candidates_evaluated: Some(result.candidates_evaluated),
                consumed_job_seconds: Some(consumed_job_seconds),
            },
            Utc::now(),
        )?;
    }
    let record = admission.record()?;
    let settlement = record
        .settlement
        .context("final settlement was not persisted")?;
    admission.publish_receipts()?;
    print_json(
        &json!({"status":"settled", "execution_scope":"final_evaluation", "family_id":admission.grant.grant().family_id,
        "campaign_id":validated.submission.request.campaign_id, "request_sha256":settlement.request_sha256,
        "job_name":validated.job_name, "job_uid":settlement.job_uid, "pod_uid":settlement.pod_uid,
        "result_sha256":settlement.result_sha256, "outcome":settlement.outcome,
        "candidates_evaluated":settlement.candidates_evaluated, "consumed_job_seconds":settlement.consumed_job_seconds}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use alpha_domain::{
        campaign_control::{
            CampaignEvaluationViewsV1, CampaignExecutionBindingV1, CampaignSelectionFeedbackV1,
        },
        campaign_finalization::{
            sign_campaign_final_evaluation_grant, CampaignFinalEvaluationGrantV1,
            FINAL_EVALUATION_GRANT_SCHEMA,
        },
    };
    use chrono::{TimeDelta, Utc};
    use ed25519_dalek::SigningKey;

    fn final_request_fixture() -> (FinalRequest, SigningKey) {
        let source = crate::mission_campaign::valid_request_for_tests();
        let operation = format!("campaign-attempt-{}", "a".repeat(64));
        let key = SigningKey::from_bytes(&[17; 32]);
        let now = Utc::now();
        let grant = CampaignFinalEvaluationGrantV1 {
            schema_version: FINAL_EVALUATION_GRANT_SCHEMA.into(),
            grant_id: "final-admission-test".into(),
            family_id: "final-admission-family".into(),
            family_definition_sha256: "a".repeat(64),
            family_head_sha256: "b".repeat(64),
            execution: CampaignExecutionBindingV1 {
                campaign_inputs_sha256: source.campaign_inputs_sha256.clone(),
                evaluation_protocol_sha256: "c".repeat(64),
                evaluation_views: CampaignEvaluationViewsV1 {
                    search_view_sha256: "d".repeat(64),
                    selection_view_sha256: "e".repeat(64),
                    selection_feedback: CampaignSelectionFeedbackV1::IndependentSelectionWithheld,
                },
                source_revision: source.build_source_revision.clone(),
                runner_image: format!("registry/runner@sha256:{}", source.image_identity),
                controller_image: format!("registry/controller@sha256:{}", "f".repeat(64)),
                job_cpu_millis: 3500,
                job_memory_mib: 12 * 1024,
            },
            selected_results: BTreeMap::from([(operation.clone(), "f".repeat(64))]),
            max_candidates: 4,
            max_job_seconds: 3600,
            valid_from: now - TimeDelta::minutes(1),
            expires_at: now + TimeDelta::hours(2),
        };
        let signed = sign_campaign_final_evaluation_grant(grant, "test-key".into(), &key).unwrap();
        (
            FinalRequest::new(
                signed,
                BTreeMap::from([(operation, source)]),
                "https://monday-lob-apne1-1045353359.oss-ap-northeast-1-internal.aliyuncs.com/research/final-admission-tests".into(),
            )
            .unwrap(),
            key,
        )
    }

    fn validated_fixture() -> (FinalRequest, SigningKey, ValidatedFinalSubmission) {
        let (request, key) = final_request_fixture();
        let image = request.grant.grant.execution.runner_image.clone();
        let validated = validate(FinalSubmission {
            purpose: Purpose::FinalEvaluation,
            attempt_id: "final-admission-attempt".into(),
            image,
            request: request.clone(),
        })
        .unwrap();
        (request, key, validated)
    }

    #[test]
    fn final_manifest_binds_job_parameters_deadline_and_trusted_key_input() {
        let (request, key, validated) = validated_fixture();
        let verified = verify_campaign_final_evaluation_grant(
            &request.grant,
            &BTreeMap::from([(String::from("test-key"), key.verifying_key())]),
            Utc::now(),
        )
        .unwrap();
        let manifest = render(&validated, &verified, "monday-research").unwrap();
        let job = &manifest["items"][1];
        let container = &job["spec"]["template"]["spec"]["containers"][0];

        assert_eq!(job["spec"]["suspend"], json!(true));
        assert_eq!(job["spec"]["parallelism"], json!(1));
        assert_eq!(job["spec"]["completions"], json!(1));
        assert_eq!(job["spec"]["backoffLimit"], json!(0));
        assert_eq!(
            job["spec"]["activeDeadlineSeconds"],
            json!(verified.grant().max_job_seconds)
        );
        assert_eq!(
            container["resources"],
            json!({
                "requests": {"cpu": "3500m", "memory": "8Gi"},
                "limits": {"cpu": "3500m", "memory": "12Gi"}
            })
        );
        assert_eq!(
            container["image"],
            json!(request.grant.grant.execution.runner_image.clone())
        );
        assert_eq!(
            container["args"].as_array().unwrap()[2],
            json!("--final-evaluation")
        );
        assert_eq!(
            container["args"].as_array().unwrap()[4],
            json!("/inputs/final-trusted-keys.json")
        );

        let expected_key = hex::encode(key.verifying_key().as_bytes());
        let trusted_keys: BTreeMap<String, String> = serde_json::from_str(
            manifest["items"][0]["stringData"]["final-trusted-keys.json"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            trusted_keys,
            BTreeMap::from([(String::from("test-key"), expected_key)])
        );
        assert!(
            manifest["items"][1]["spec"]["template"]["spec"]["volumes"][2]["secret"]["items"]
                .as_array()
                .unwrap()
                .contains(&json!({
                    "key": "final-trusted-keys.json",
                    "path": "final-trusted-keys.json"
                }))
        );
    }

    #[test]
    fn final_job_deadline_rejects_a_start_that_cannot_finish_before_expiry() {
        let (request, key, _) = validated_fixture();
        let verified = verify_campaign_final_evaluation_grant(
            &request.grant,
            &BTreeMap::from([(String::from("test-key"), key.verifying_key())]),
            Utc::now(),
        )
        .unwrap();

        verified
            .validate_job_deadline_at(Utc::now())
            .expect("fixture deadline is active");
        assert!(verified
            .validate_job_deadline_at(verified.grant().expires_at - TimeDelta::seconds(1))
            .is_err());
    }

    #[test]
    fn final_dispatch_rejects_control_schema_and_authority_drift() {
        let root = tempfile::tempdir().unwrap();
        let control_path = root.path().join("control.json");
        let control = FinalControl {
            schema_version: "monday.campaign_final_dispatch_control.v999".into(),
            ledger_path: root.path().join("ledger.duckdb"),
            signed_final_grant_path: root.path().join("grant.json"),
            trusted_keys_path: root.path().join("keys.json"),
            materialization_path: root.path().join("materialization.json"),
            controller_image: format!("registry/controller@sha256:{}", "f".repeat(64)),
            source_submissions: BTreeMap::new(),
            receipt_access: BTreeMap::new(),
        };
        data_mission::write_json_atomic(&control_path, &control).unwrap();
        let control_error = read_control(&control_path).err().unwrap();
        assert!(control_error
            .to_string()
            .contains("unsupported final dispatch control"));

        let (request, _, _) = validated_fixture();
        let mut schema_drift = request.clone();
        schema_drift.schema_version = "monday.campaign_final_request.v999".into();
        assert!(validate(FinalSubmission {
            purpose: Purpose::FinalEvaluation,
            attempt_id: "final-admission-attempt".into(),
            image: request.grant.grant.execution.runner_image.clone(),
            request: schema_drift,
        })
        .is_err());

        let wrong_image = format!("registry/runner@sha256:{}", "9".repeat(64));
        assert!(validate(FinalSubmission {
            purpose: Purpose::FinalEvaluation,
            attempt_id: "final-admission-attempt".into(),
            image: wrong_image,
            request,
        })
        .is_err());
    }

    #[test]
    fn final_dispatch_rejects_an_untrusted_grant_signer() {
        let root = tempfile::tempdir().unwrap();
        let keys = root.path().join("keys.json");
        let wrong_key = SigningKey::from_bytes(&[18; 32]);
        data_mission::write_json_atomic(
            &keys,
            &BTreeMap::from([(
                "different-key",
                hex::encode(wrong_key.verifying_key().as_bytes()),
            )]),
        )
        .unwrap();
        let (request, _, _) = validated_fixture();

        assert!(verify_worker_grant(&request.grant, &keys).is_err());
    }
}
