//! Sequence input binding on the shared native dispatch/ledger controller.
use super::*;
use crate::mission_campaign::sequence::{inputs::SequenceCampaignInputs, SequenceRequest};
use alpha_domain::campaign_control::{
    CampaignEvaluationViewsV1, CampaignExecutionBindingV1, CampaignSelectionFeedbackV1,
    SignedCampaignRootGrantV1,
};
use alpha_domain::canonical_json_hash;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Purpose {
    SequenceStudy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Submission {
    purpose: Purpose,
    attempt_id: String,
    image: String,
    request: SequenceRequest,
}

struct Validated {
    submission: Submission,
    request_json: String,
    request_sha256: String,
    identity: String,
    job_name: String,
    secret_name: String,
}

pub(crate) fn is_sequence_submission(path: &Path) -> anyhow::Result<bool> {
    let value: Value = admission::read_json(path)?;
    Ok(value["purpose"] == "sequence_study")
}

fn validate(submission: Submission) -> anyhow::Result<Validated> {
    validate_dns_label("sequence attempt", &submission.attempt_id)?;
    submission.request.validate()?;
    if submission.image != submission.request.image
        || image_digest(&submission.image)? != submission.request.image_identity
    {
        bail!("sequence submission image changed");
    }
    let request_json = serde_json::to_string_pretty(&submission.request)?;
    if request_json.len() as u64 > MAX_SUBMISSION_BYTES {
        bail!("sequence submission exceeds byte limit");
    }
    let request_sha256 = sha256_text(&request_json);
    let identity = sha256_text(&format!(
        "{}:{}",
        submission.attempt_id, submission.request.campaign_id
    ));
    let job_name = format!("alpha-campaign-{}", &identity[..32]);
    Ok(Validated {
        secret_name: format!("{job_name}-inputs"),
        submission,
        request_json,
        request_sha256,
        identity,
        job_name,
    })
}

pub(crate) fn write_submission(
    path: &Path,
    attempt_id: &str,
    image: &str,
    request: SequenceRequest,
) -> anyhow::Result<SubmissionRenderReport> {
    let validated = validate(Submission {
        purpose: Purpose::SequenceStudy,
        attempt_id: attempt_id.into(),
        image: image.into(),
        request,
    })?;
    data_mission::write_json_atomic(path, &validated.submission)?;
    Ok(SubmissionRenderReport {
        request_sha256: validated.request_sha256,
        submission_identity_sha256: validated.identity,
        job_name: validated.job_name,
        secret_name: validated.secret_name,
    })
}

pub(crate) fn execution_binding(
    request: &SequenceRequest,
    controller_image: &str,
) -> anyhow::Result<CampaignExecutionBindingV1> {
    request.validate()?;
    image_digest(controller_image)?;
    // A shared Root binds the full declared data cohort, while each request and
    // claim separately bind its particular input receipt, fold and mount.
    let search = canonical_json_hash(
        &serde_json::json!({"schema":"sol-sequence-development-views-v1", "folds":request.plan.folds}),
    )?;
    let selection = canonical_json_hash(
        &serde_json::json!({"schema":"sol-sequence-withheld-view-v1",
        "dataset":request.plan.sealed_dataset_sha256,"view":request.plan.sealed_view}),
    )?;
    let binding = CampaignExecutionBindingV1 {
        campaign_inputs_sha256: canonical_json_hash(
            &serde_json::json!({"schema":"sol-sequence-cohort-v1", "input":request.plan.input,
            "development":search,"withheld":selection}),
        )?,
        evaluation_protocol_sha256: request.plan.content_hash().map_err(anyhow::Error::msg)?,
        evaluation_views: CampaignEvaluationViewsV1 {
            search_view_sha256: search,
            selection_view_sha256: selection,
            selection_feedback: CampaignSelectionFeedbackV1::IndependentSelectionWithheld,
        },
        source_revision: request.build_source_revision.clone(),
        runner_image: request.image.clone(),
        controller_image: controller_image.into(),
        job_cpu_millis: alpha_domain::research_accelerator::ADMITTED_CAMPAIGN_JOB_CPU_MILLIS,
        job_memory_mib: alpha_domain::research_accelerator::ADMITTED_CAMPAIGN_JOB_MEMORY_LIMIT
            .strip_suffix("Gi")
            .context("unsupported sequence memory unit")?
            .parse::<u32>()?
            .checked_mul(1024)
            .context("sequence memory overflow")?,
        accelerator: Default::default(),
    };
    binding.validate()?;
    Ok(binding)
}

fn render(
    validated: &Validated,
    control: &admission::DispatchControl,
    namespace: &str,
) -> anyhow::Result<Value> {
    let signed: SignedCampaignRootGrantV1 = admission::read_json(&control.signed_root_grant_path)?;
    signed.grant.validate()?;
    let request = &validated.submission.request;
    if signed.grant.execution != execution_binding(request, &control.controller_image)?
        || signed.grant.family.family_id != request.plan.study_id
        || signed.grant.family.definition_sha256
            != request.plan.content_hash().map_err(anyhow::Error::msg)?
        || signed.grant.allowed_policy_revision_ids
            != std::collections::BTreeSet::from([request.policy_id()?])
        || signed.grant.max_follow_ups != 0
        || signed.grant.budget.max_llm_tokens != 0
        || signed.grant.budget.max_trials > 60
        || signed.grant.family.max_trials > 60
        || signed.grant.budget.max_job_attempts > 4
    {
        bail!("sequence Root scope differs from fixed comparison");
    }
    // Authenticate prospective/historical authority without mistaking its total
    // cumulative budget for the allocation to each of the four fold Jobs.
    let _ = admission::root_job_deadline(control)?;
    let deadline = ACTIVE_DEADLINE_SECONDS
        .min(signed.grant.budget.max_job_seconds / request.plan.folds.len() as u64);
    if deadline == 0 {
        bail!("sequence Root has no per-fold time budget");
    }
    let store = alpha_store::AlphaStore::open_read_only(&control.ledger_path)?;
    let historic = store
        .campaign_family_receipts(&signed.grant.family.family_id)?
        .into_iter()
        .find_map(|receipt| match receipt.receipt.event {
            alpha_store::campaign_ledger::CampaignLedgerEventV1::RootRegistered {
                signed: registered,
                verifying_key_hex,
                ..
            } if registered.as_ref() == &signed => Some(verifying_key_hex),
            _ => None,
        });
    let key_hex = match historic {
        Some(key) => key,
        None => hex::encode(
            admission::read_trusted_keys(&control.trusted_keys_path)?
                .get(&signed.key_id)
                .context("sequence signing key is not currently trusted")?
                .as_bytes(),
        ),
    };
    let trusted = serde_json::to_string(&BTreeMap::from([(signed.key_id.clone(), key_hex)]))?;
    let mut manifest = render_campaign_manifest(
        DispatchManifestInput {
            attempt_id: &validated.submission.attempt_id,
            campaign_id: &request.campaign_id,
            job_name: &validated.job_name,
            secret_name: &validated.secret_name,
            image: &validated.submission.image,
            image_digest: &request.image_identity,
            request_sha256: &validated.request_sha256,
            request_json: &validated.request_json,
            submission_identity_sha256: &validated.identity,
            active_deadline_seconds: deadline,
            trusted_keys_json: None,
            args: vec![
                "mission".into(),
                "campaign-execute".into(),
                "--pre-holdout".into(),
                "--work-dir".into(),
                "/work".into(),
                "--campaign-id".into(),
                request.campaign_id.clone(),
                "--image-identity".into(),
                request.image_identity.clone(),
                "--request".into(),
                "/inputs/campaign.json".into(),
                "--request-sha256".into(),
                validated.request_sha256.clone(),
            ],
        },
        namespace,
    )?;
    for (name, value) in [
        ("sequence-root-grant.json", serde_json::to_string(&signed)?),
        ("sequence-trusted-keys.json", trusted),
    ] {
        manifest["items"][0]["stringData"][name] = Value::String(value);
        manifest["items"][1]["spec"]["template"]["spec"]["volumes"][2]["secret"]["items"]
            .as_array_mut()
            .context("missing input secret items")?
            .push(json!({"key":name,"path":name}));
    }
    let reservation = alpha_domain::campaign_control::CampaignAttemptReservationV1 {
        schema_version: alpha_domain::campaign_control::ATTEMPT_SCHEMA.into(),
        root_grant_sha256: signed.content_sha256.clone(),
        family_id: signed.grant.family.family_id.clone(),
        campaign_id: request.campaign_id.clone(),
        execution: execution_binding(request, &control.controller_image)?,
        generation: 0,
        parent_result_sha256: None,
        policy_revision_id: request.policy_id()?,
        request_sha256: validated.request_sha256.clone(),
        attempt_ordinal: control.attempt_ordinal,
        declared_trials: 14,
        reserved_job_seconds: deadline,
        reserved_llm_tokens: 0,
    };
    manifest["items"][0]["stringData"]["sequence-attempt.json"] =
        Value::String(serde_json::to_string(&reservation)?);
    manifest["items"][1]["spec"]["template"]["spec"]["volumes"][2]["secret"]["items"]
        .as_array_mut()
        .context("missing sequence secret items")?
        .push(json!({"key":"sequence-attempt.json","path":"sequence-attempt.json"}));
    let pod = &mut manifest["items"][1]["spec"]["template"]["spec"];
    pod["volumes"].as_array_mut().context("missing sequence volumes")?.push(json!({"name":"sequence-data", "persistentVolumeClaim":{"claimName":request.inputs.pvc_name,"readOnly":true}}));
    pod["containers"][0]["volumeMounts"].as_array_mut().context("missing sequence mounts")?.push(json!({"name":"sequence-data","mountPath":"/sequence-inputs","subPath":request.inputs.sub_path,"readOnly":true}));
    Ok(manifest)
}

pub(crate) const SEQUENCE_INPUT_MOUNT: &str = "/sequence-inputs";

/// Settlement runs in its own Job. The training worker mount is not visible
/// to the Campaign controller, so readback mounts the same PVC and subPath.
pub(crate) fn render_readback_job(
    request: &SequenceRequest,
    controller_image: &str,
    namespace: &str,
    attempt_job_name: &str,
) -> anyhow::Result<Value> {
    image_digest(controller_image)?;
    request.inputs.validate()?;
    let name = format!("{attempt_job_name}-readback");
    if name.len() > 63 {
        bail!("sequence readback job name exceeds the DNS label limit");
    }
    Ok(json!({
        "apiVersion":"batch/v1",
        "kind":"Job",
        "metadata":{"name":name,"namespace":namespace,"labels":{"app.kubernetes.io/name":"monday-sequence-readback"}},
        "spec":{
            "backoffLimit":0,
            "ttlSecondsAfterFinished":86_400,
            "template":{"spec":{
                "restartPolicy":"Never",
                "nodeSelector":cpu_research_node_selector(),
                "containers":[{
                    "name":"sequence-readback",
                    "image":controller_image,
                    "args":["mission","dispatch","settle","--input-root",SEQUENCE_INPUT_MOUNT],
                    "volumeMounts":[{
                        "name":"sequence-data",
                        "mountPath":SEQUENCE_INPUT_MOUNT,
                        "subPath":request.inputs.sub_path,
                        "readOnly":true
                    }],
                    "resources":cpu_research_container_resources()
                }],
                "volumes":[{
                    "name":"sequence-data",
                    "persistentVolumeClaim":{"claimName":request.inputs.pvc_name,"readOnly":true}
                }]
            }}
        }
    }))
}

fn bind_readback_mount(
    job: &Value,
    request: &SequenceRequest,
    input_root: &Path,
) -> anyhow::Result<()> {
    let mount_path = input_root
        .to_str()
        .context("sequence input root must be UTF-8")?;
    let container = &job["spec"]["template"]["spec"]["containers"][0];
    let mount = &container["volumeMounts"][0];
    let volume = &job["spec"]["template"]["spec"]["volumes"][0];
    let args = container["args"]
        .as_array()
        .context("sequence readback args missing")?;
    if volume["persistentVolumeClaim"]["claimName"] != request.inputs.pvc_name
        || mount["subPath"] != request.inputs.sub_path
        || mount["mountPath"] != mount_path
        || mount["readOnly"] != true
        || !args
            .windows(2)
            .any(|pair| pair[0] == "--input-root" && pair[1] == mount_path)
    {
        bail!("sequence settlement controller does not mount the requested cohort");
    }
    Ok(())
}

fn inspection(
    validated: &Validated,
    control: &admission::DispatchControl,
    manifest: &Value,
) -> anyhow::Result<admission::DispatchInspection> {
    let inputs: SequenceCampaignInputs = admission::read_json(&control.materialization_path)?;
    if inputs != validated.submission.request.inputs
        || canonical_json_hash(&inputs)? != validated.submission.request.campaign_inputs_sha256
    {
        bail!("sequence operator input receipt differs from the worker request");
    }
    let pod = &manifest["items"][1]["spec"]["template"]["spec"];
    if pod["containers"][0]["resources"] != cpu_research_container_resources()
        || pod["nodeSelector"] != cpu_research_node_selector()
        || !alpha_domain::research_accelerator::bind_pod_spec_accelerator(pod)
            .map_err(anyhow::Error::msg)?
            .is_cpu()
    {
        bail!("sequence rendered resources differ from CPU admission");
    }
    Ok(admission::DispatchInspection {
        execution: execution_binding(&validated.submission.request, &control.controller_image)?,
        campaign_id: validated.submission.request.campaign_id.clone(),
        generation: 0,
        parent_result_sha256: None,
        policy_revision_id: validated.submission.request.policy_id()?,
        request_sha256: validated.request_sha256.clone(),
        attempt_ordinal: control.attempt_ordinal,
        declared_trials: 14,
        reserved_job_seconds: manifest["items"][1]["spec"]["activeDeadlineSeconds"]
            .as_u64()
            .context("missing sequence deadline")?,
        reserved_llm_tokens: 0,
    })
}

fn verify_pvc(request: &SequenceRequest, context: &str, namespace: &str) -> anyhow::Result<()> {
    let pvc = kubectl_json(
        &kubectl_binary(),
        context,
        namespace,
        [
            "--request-timeout=30s",
            "get",
            "pvc",
            &request.inputs.pvc_name,
            "-o",
            "json",
        ],
        "verify sequence input PVC",
    )?;
    if pvc["metadata"]["uid"] != request.inputs.pvc_uid || pvc["status"]["phase"] != "Bound" {
        bail!("sequence input PVC identity or state changed");
    }
    Ok(())
}

struct SequenceAdmission<'a> {
    inner: admission::Admission,
    request: &'a SequenceRequest,
    context: &'a str,
    namespace: &'a str,
}
impl DispatchAdmission for SequenceAdmission<'_> {
    fn prepare(&mut self) -> anyhow::Result<()> {
        verify_pvc(self.request, self.context, self.namespace)?;
        self.inner.prepare()
    }
    fn publish_receipts(&mut self) -> anyhow::Result<()> {
        self.inner.publish_receipts()
    }
    fn claim(
        &mut self,
    ) -> anyhow::Result<(alpha_store::campaign_ledger::CampaignDispatchClaimV1, bool)> {
        self.inner.claim()
    }
    fn bind_job(&mut self, uid: &str) -> anyhow::Result<()> {
        self.inner.bind_job(uid)
    }
    fn guarded<T>(
        &mut self,
        uid: Option<&str>,
        action: impl FnOnce() -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let request = self.request;
        let context = self.context;
        let namespace = self.namespace;
        self.inner.guarded(uid, || {
            verify_pvc(request, context, namespace)?;
            action()
        })
    }
}

pub(crate) fn inspect(args: MissionDispatchInspectArgs) -> anyhow::Result<()> {
    let validated = validate(admission::read_json(&args.submission)?)?;
    let control = admission::read_control(
        args.control
            .as_deref()
            .context("sequence inspect requires its operator control")?,
    )?;
    let manifest = render(&validated, &control, "monday-research")?;
    print_json(&inspection(&validated, &control, &manifest)?)
}

pub(crate) fn submit(args: MissionDispatchSubmitArgs) -> anyhow::Result<()> {
    validate_cluster_target(&args.context, &args.namespace)?;
    let validated = validate(admission::read_json(&args.submission)?)?;
    if validated.submission.request.build_source_revision != crate::cli::BUILD_SOURCE_REVISION {
        bail!("sequence dispatcher source differs from request");
    }
    let path = args
        .control
        .clone()
        .or_else(|| std::env::var_os("MONDAY_CAMPAIGN_CONTROL").map(Into::into))
        .context("sequence dispatch requires operator control")?;
    let control = admission::read_control(&path)?;
    let manifest = render(&validated, &control, &args.namespace)?;
    let inspected = inspection(&validated, &control, &manifest)?;
    let inner = admission::Admission::from_inspection(
        control,
        admission::InspectedDispatch {
            inspection: inspected,
            manifest: &manifest,
            job_name: &validated.job_name,
            result_readback_url: &validated.submission.request.result_readback_url,
            study_proposal: None,
            context: &args.context,
            namespace: &args.namespace,
        },
        admission::Purpose::Dispatch,
        false,
    )?;
    let mut admitted = SequenceAdmission {
        inner,
        request: &validated.submission.request,
        context: &args.context,
        namespace: &args.namespace,
    };
    submit_rendered_job(
        &args,
        RenderedDispatch {
            job_name: validated.job_name.clone(),
            secret_name: validated.secret_name,
            request_sha256: validated.request_sha256.clone(),
            request_json: validated.request_json,
            manifest,
        },
        &mut admitted,
    )?;
    print_json(
        &json!({"status":"submitted","execution_scope":"pre_holdout","campaign_id":validated.submission.request.campaign_id,
        "request_sha256":validated.request_sha256,"job_name":validated.job_name,"operation_id":admitted.inner.reservation.operation_id()?,"reserved_trials":14}),
    )
}

fn historical_admission(
    validated: &Validated,
    control: admission::DispatchControl,
    manifest: &Value,
    context: &str,
    namespace: &str,
    read_only: bool,
) -> anyhow::Result<admission::Admission> {
    let inspected = inspection(validated, &control, manifest)?;
    admission::Admission::from_inspection(
        control,
        admission::InspectedDispatch {
            inspection: inspected,
            manifest,
            job_name: &validated.job_name,
            result_readback_url: &validated.submission.request.result_readback_url,
            study_proposal: None,
            context,
            namespace,
        },
        admission::Purpose::Settlement,
        read_only,
    )
}

pub(crate) fn status_report(args: &crate::cli::MissionDispatchStatusArgs) -> anyhow::Result<Value> {
    let validated = validate(admission::read_json(&args.submission)?)?;
    let control = admission::read_control(&args.control)?;
    let manifest = render(&validated, &control, &args.namespace)?;
    let admitted = historical_admission(
        &validated,
        control,
        &manifest,
        &args.context,
        &args.namespace,
        true,
    )?;
    let record = admitted.record()?;
    Ok(
        json!({"schema_version":"monday.campaign_dispatch_status.v1", "operation_id":record.reservation.operation_id()?,
        "request_sha256":validated.request_sha256,"campaign_id":validated.submission.request.campaign_id,
        "job_name":record.claim.target.job_name,"job_uid":record.claim.job_uid,
        "authority_deadline_epoch":record.root.grant().expires_at.timestamp(),"reserved_trials":record.reservation.declared_trials,
        "settlement":record.settlement,"accounting_changed":false}),
    )
}

pub(crate) fn settle(args: MissionDispatchSubmitArgs) -> anyhow::Result<()> {
    validate_cluster_target(&args.context, &args.namespace)?;
    let cache = args
        .readback_cache
        .as_deref()
        .context("sequence settlement requires its ACK readback cache")?;
    let report_path = args
        .model_report
        .clone()
        .unwrap_or_else(|| cache.join("model-report.json"));
    let validated = validate(admission::read_json(&args.submission)?)?;
    let control_path = args
        .control
        .clone()
        .or_else(|| std::env::var_os("MONDAY_CAMPAIGN_CONTROL").map(Into::into))
        .context("sequence settlement requires operator control")?;
    let control = admission::read_control(&control_path)?;
    let controller_image = control.controller_image.clone();
    let manifest = render(&validated, &control, &args.namespace)?;
    let mut admitted = historical_admission(
        &validated,
        control,
        &manifest,
        &args.context,
        &args.namespace,
        false,
    )?;
    let record = admitted.record()?;
    let terminal = if record.settlement.is_none() {
        verify_pvc(
            &validated.submission.request,
            &args.context,
            &args.namespace,
        )?;
        Some(super::terminal::read_terminal_job(
            &args.context,
            &args.namespace,
            &manifest["items"][1],
            &validated.job_name,
            record
                .claim
                .job_uid
                .as_deref()
                .context("sequence Job lacks durable UID")?,
        )?)
    } else {
        if record.terminal_pod_uid.is_none() {
            bail!("sequence settlement lacks terminal provenance");
        }
        None
    };
    let input_root = args
        .input_root
        .as_deref()
        .context("sequence settlement requires its mounted cohort root")?;
    let readback_job = render_readback_job(
        &validated.submission.request,
        &controller_image,
        &args.namespace,
        &validated.job_name,
    )?;
    bind_readback_mount(&readback_job, &validated.submission.request, input_root)?;
    let (outcome, hash, report) = crate::mission_campaign::sequence::readback::readback(
        &validated.submission.request,
        &validated.request_sha256,
        &admitted.reservation.root_grant_sha256,
        input_root,
        cache,
        record
            .settlement
            .as_ref()
            .map(|s| s.evidence_sha256.as_str()),
    )?;
    data_mission::write_json_atomic(&report_path, &report)?;
    if let Some(terminal) = terminal {
        admitted.settle(
            &alpha_store::campaign_ledger::CampaignDispatchSettlementV1 {
                job_uid: terminal.job_uid,
                pod_uid: terminal.pod_uid,
                settlement: alpha_domain::campaign_control::CampaignAttemptSettlementV1 {
                    operation_id: admitted.reservation.operation_id()?,
                    reservation_sha256: admitted.reservation.content_hash()?,
                    evidence_sha256: hash.clone(),
                    outcome, // Charge the entire attempt; failed fits are never refunded.
                    consumed_trials: Some(admitted.reservation.declared_trials),
                },
            },
        )?;
    }
    admitted.publish_receipts()?;
    let record = admitted.record()?;
    print_json(
        &json!({"status":"settled","operation_id":admitted.reservation.operation_id()?,
        "campaign_id":validated.submission.request.campaign_id,"job_name":validated.job_name,
        "job_uid":record.claim.job_uid,"pod_uid":record.terminal_pod_uid,"result_sha256":hash,
        "settlement":record.settlement,"model_report":report_path,"sealed_holdout_opened":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use alpha_domain::campaign_control::*;
    use chrono::{TimeDelta, Utc};
    use std::collections::BTreeSet;

    #[test]
    fn sequence_dispatch_reuses_cumulative_ledger_and_historical_readback() {
        let root = tempfile::tempdir().unwrap();
        let request = crate::mission_campaign::sequence::tests::request();
        let validated = validate(Submission {
            purpose: Purpose::SequenceStudy,
            attempt_id: "sequence-test".into(),
            image: request.image.clone(),
            request: request.clone(),
        })
        .unwrap();
        let controller = format!("registry/controller@sha256:{}", "f".repeat(64));
        let now = Utc::now();
        let signing = ed25519_dalek::SigningKey::from_bytes(&[33; 32]);
        let signed = sign_campaign_root_grant(
            CampaignRootGrantV1 {
                schema_version: ROOT_GRANT_SCHEMA.into(),
                root_id: "sequence-root".into(),
                family: CampaignFamilyPolicyV1 {
                    family_id: request.plan.study_id.clone(),
                    definition_sha256: request.plan.content_hash().unwrap(),
                    max_trials: 60,
                },
                execution_scope: CampaignExecutionScope::PreHoldout,
                execution: execution_binding(&request, &controller).unwrap(),
                allowed_policy_revision_ids: BTreeSet::from([request.policy_id().unwrap()]),
                max_follow_ups: 0,
                budget: CampaignRootBudgetV1 {
                    max_trials: 56,
                    max_job_attempts: 4,
                    max_job_seconds: 2400,
                    max_llm_tokens: 0,
                },
                valid_from: now - TimeDelta::minutes(1),
                expires_at: now + TimeDelta::hours(2),
            },
            "operator".into(),
            &signing,
        )
        .unwrap();
        let mut store = alpha_store::AlphaStore::open(root.path().join("ledger.duckdb")).unwrap();
        store.record_approval(&alpha_store::ApprovalRecord {
            approval_id:"sequence-approval".into(),approval_class:"campaign_root".into(),subject_id:signed.grant.root_id.clone(),
            payload:json!({"grant_sha256":signed.content_sha256,"family_id":signed.grant.family.family_id}),signer_id:Some("operator".into()),
            valid_from:Some(signed.grant.valid_from),expires_at:Some(signed.grant.expires_at),revoked_at:None,revoked_by:None,revocation_reason:None,created_at:signed.grant.valid_from,
        }).unwrap();
        drop(store);
        for (name, value) in [
            ("grant.json", serde_json::to_value(&signed).unwrap()),
            (
                "keys.json",
                json!({"operator":hex::encode(signing.verifying_key().as_bytes())}),
            ),
            (
                "inputs.json",
                serde_json::to_value(&request.inputs).unwrap(),
            ),
        ] {
            std::fs::write(root.path().join(name), serde_json::to_vec(&value).unwrap()).unwrap();
        }
        let control = admission::DispatchControl {
            schema_version: "monday.campaign_dispatch_control.v1".into(),
            ledger_path: root.path().join("ledger.duckdb"),
            signed_root_grant_path: root.path().join("grant.json"),
            trusted_keys_path: root.path().join("keys.json"),
            materialization_path: root.path().join("inputs.json"),
            campaign_inputs_path: None,
            approval_id: "sequence-approval".into(),
            controller_image: controller,
            attempt_ordinal: 0,
            receipt_access: (1..=12).map(|sequence| {
                let key = format!("research/campaign-ledger/family-id={}/sequence={sequence:020}/receipt.json", request.plan.study_id);
                let url = format!("https://monday-lob-apne1-1045353359.oss-ap-northeast-1-internal.aliyuncs.com/{key}?signature=fixture");
                (key, serde_json::from_value(json!({"put_url":url,"readback_url":url})).unwrap())
            }).collect(),
        };
        let manifest = render(&validated, &control, "monday-research").unwrap();
        assert_eq!(manifest["items"][1]["spec"]["activeDeadlineSeconds"], 600);
        let reserved: CampaignAttemptReservationV1 = serde_json::from_str(
            manifest["items"][0]["stringData"]["sequence-attempt.json"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        let inspected = inspection(&validated, &control, &manifest).unwrap();
        let mut admitted = admission::Admission::from_inspection(
            control.clone(),
            admission::InspectedDispatch {
                inspection: inspected,
                manifest: &manifest,
                job_name: &validated.job_name,
                result_readback_url: &request.result_readback_url,
                study_proposal: None,
                context: "monday-research-apne1",
                namespace: "monday-research",
            },
            admission::Purpose::Dispatch,
            false,
        )
        .unwrap();
        assert_eq!(admitted.reservation, reserved);
        admitted.prepare().unwrap();
        assert!(
            admitted.claim().is_err(),
            "unpublished reservation must not dispatch"
        );
        admitted
            .publish_receipts_with(|_, bytes| Ok(bytes.to_vec()))
            .unwrap();
        let (_, created) = admitted.claim().unwrap();
        assert!(created);
        admitted
            .publish_receipts_with(|_, bytes| Ok(bytes.to_vec()))
            .unwrap();
        admitted.bind_job("sequence-job-uid").unwrap();
        admitted
            .publish_receipts_with(|_, bytes| Ok(bytes.to_vec()))
            .unwrap();
        let record = admitted.record().unwrap();
        assert_eq!(record.reservation.declared_trials, 14);
        drop(admitted);
        // Current trust may be revoked after execution; historical evidence must
        // still reconstruct identical bytes without granting another dispatch.
        std::fs::write(&control.trusted_keys_path, b"{}").unwrap();
        let historical = render(&validated, &control, "monday-research").unwrap();
        assert_eq!(manifest, historical);
        let mut reader = historical_admission(
            &validated,
            control.clone(),
            &manifest,
            "monday-research-apne1",
            "monday-research",
            true,
        )
        .unwrap();
        assert!(reader.prepare().is_err());
        assert!(reader.claim().is_err());
        assert_eq!(
            reader.record().unwrap().claim.job_uid.as_deref(),
            Some("sequence-job-uid")
        );
        drop(reader);
        let mut changed = manifest["items"][1].clone();
        changed["spec"]["template"]["spec"]["volumes"][3]["persistentVolumeClaim"]["claimName"] =
            json!("broader-data");
        assert!(validate_job_readback(
            &changed,
            &manifest["items"][1],
            &validated.job_name,
            &validated.request_sha256,
            false
        )
        .is_err());
        let mut widened = signed.grant.clone();
        widened.budget.max_trials = 61;
        widened.family.max_trials = 61;
        let widened = sign_campaign_root_grant(widened, "operator".into(), &signing).unwrap();
        std::fs::write(
            &control.signed_root_grant_path,
            serde_json::to_vec(&widened).unwrap(),
        )
        .unwrap();
        assert!(render(&validated, &control, "monday-research").is_err());
    }

    #[test]
    fn sequence_readback_job_mounts_the_request_cohort() {
        let request = crate::mission_campaign::sequence::tests::request();
        let controller = format!("registry/controller@sha256:{}", "f".repeat(64));
        let job = render_readback_job(
            &request,
            &controller,
            "monday-research",
            "alpha-campaign-test",
        )
        .unwrap();
        bind_readback_mount(&job, &request, Path::new(SEQUENCE_INPUT_MOUNT)).unwrap();
        assert!(bind_readback_mount(&job, &request, Path::new("/tmp/unmounted")).is_err());
        let mount = &job["spec"]["template"]["spec"]["containers"][0]["volumeMounts"][0];
        assert_eq!(mount["subPath"], request.inputs.sub_path);
        assert_eq!(
            job["spec"]["template"]["spec"]["volumes"][0]["persistentVolumeClaim"]["claimName"],
            request.inputs.pvc_name
        );
    }
}
