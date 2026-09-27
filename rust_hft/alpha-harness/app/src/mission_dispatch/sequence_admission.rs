//! Typed development studies share one native dispatch/ledger controller.
use super::*;
use crate::mission_campaign::{market_encoder::MarketRequest, sequence::SequenceRequest};
use alpha_domain::campaign_control::{
    CampaignEvaluationViewsV1, CampaignExecutionBindingV1, CampaignSelectionFeedbackV1,
    SignedCampaignRootGrantV1,
};
use alpha_domain::canonical_json_hash;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Purpose {
    SequenceStudy,
    MarketEncoderStudy,
}

/// Untagged serialization retains each request's canonical wire identity.
/// Its own schema plus the outer purpose must agree before any admission.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum StudyRequest {
    Sequence(Box<SequenceRequest>),
    MarketEncoder(Box<MarketRequest>),
}
impl From<&SequenceRequest> for StudyRequest {
    fn from(request: &SequenceRequest) -> Self {
        Self::Sequence(Box::new(request.clone()))
    }
}
impl From<&MarketRequest> for StudyRequest {
    fn from(request: &MarketRequest) -> Self {
        Self::MarketEncoder(Box::new(request.clone()))
    }
}
impl From<&StudyRequest> for StudyRequest {
    fn from(request: &StudyRequest) -> Self {
        request.clone()
    }
}
impl StudyRequest {
    fn purpose(&self) -> Purpose {
        match self {
            Self::Sequence(_) => Purpose::SequenceStudy,
            Self::MarketEncoder(_) => Purpose::MarketEncoderStudy,
        }
    }
    fn validate(&self) -> anyhow::Result<()> {
        match self {
            Self::Sequence(r) => r.validate(),
            Self::MarketEncoder(r) => r.validate(),
        }
    }
    fn policy_id(&self) -> anyhow::Result<String> {
        match self {
            Self::Sequence(r) => r.policy_id(),
            Self::MarketEncoder(r) => r.policy_id(),
        }
    }
    fn plan_hash(&self) -> anyhow::Result<String> {
        match self {
            Self::Sequence(r) => r.plan.content_hash(),
            Self::MarketEncoder(r) => r.plan.content_hash(),
        }
        .map_err(anyhow::Error::msg)
    }
    fn declared_trials(&self) -> anyhow::Result<u64> {
        match self {
            Self::Sequence(_) => Ok(14),
            Self::MarketEncoder(r) => r.declared_trials(),
        }
    }
    fn cpu_millis(&self) -> u32 {
        match self {
            Self::MarketEncoder(_) => 3000,
            Self::Sequence(_) => {
                alpha_domain::research_accelerator::ADMITTED_CAMPAIGN_JOB_CPU_MILLIS
            }
        }
    }
    fn resources(&self) -> Value {
        let mut value = cpu_research_container_resources();
        if matches!(self, Self::MarketEncoder(_)) {
            value["requests"]["cpu"] = json!("3");
            value["limits"]["cpu"] = json!("3");
        }
        value
    }
    fn root_job_count(&self) -> usize {
        match self {
            Self::Sequence(r) => r.plan.folds.len(),
            Self::MarketEncoder(_) => 1,
        }
    }
    fn root_trial_ceiling(&self) -> anyhow::Result<u64> {
        match self {
            Self::Sequence(_) => Ok(self.max_trials()),
            Self::MarketEncoder(r) => r.declared_trials(),
        }
    }
    fn family_id(&self) -> anyhow::Result<String> {
        match self {
            Self::Sequence(r) => Ok(r.plan.study_id.clone()),
            Self::MarketEncoder(r) => r.family_id(),
        }
    }

    fn max_trials(&self) -> u64 {
        match self {
            Self::Sequence(r) => {
                u64::from(r.plan.max_primary_fits) + u64::from(r.plan.max_verification_fits)
            }
            Self::MarketEncoder(r) => {
                u64::from(r.plan.max_primary_fits) + u64::from(r.plan.max_verification_fits)
            }
        }
    }
    fn verify_input_receipt(&self, path: &Path) -> anyhow::Result<()> {
        fn check<T: serde::de::DeserializeOwned + Serialize + PartialEq>(
            path: &Path,
            expected: &T,
            hash: &str,
        ) -> anyhow::Result<()> {
            let actual: T = admission::read_json(path)?;
            // Preserve the typed wire identity: JSON Value may reorder fields.
            if actual != *expected || canonical_json_hash(&actual)? != hash {
                bail!("study operator input receipt differs from the worker request");
            }
            Ok(())
        }
        match self {
            Self::Sequence(r) => check(path, &r.inputs, self.campaign_inputs_sha256()),
            Self::MarketEncoder(r) => check(path, &r.inputs, self.campaign_inputs_sha256()),
        }
    }
    #[cfg(test)]
    fn inputs_json(&self) -> anyhow::Result<Value> {
        match self {
            Self::Sequence(r) => serde_json::to_value(&r.inputs),
            Self::MarketEncoder(r) => serde_json::to_value(&r.inputs),
        }
        .map_err(Into::into)
    }
    fn campaign_id(&self) -> &str {
        match self {
            Self::Sequence(r) => &r.campaign_id,
            Self::MarketEncoder(r) => &r.campaign_id,
        }
    }
    fn image(&self) -> &str {
        match self {
            Self::Sequence(r) => &r.image,
            Self::MarketEncoder(r) => &r.image,
        }
    }
    fn image_identity(&self) -> &str {
        match self {
            Self::Sequence(r) => &r.image_identity,
            Self::MarketEncoder(r) => &r.image_identity,
        }
    }
    fn build_source_revision(&self) -> &str {
        match self {
            Self::Sequence(r) => &r.build_source_revision,
            Self::MarketEncoder(r) => &r.build_source_revision,
        }
    }
    fn campaign_inputs_sha256(&self) -> &str {
        match self {
            Self::Sequence(r) => &r.campaign_inputs_sha256,
            Self::MarketEncoder(r) => &r.campaign_inputs_sha256,
        }
    }
    fn result_readback_url(&self) -> &str {
        match self {
            Self::Sequence(r) => &r.result_readback_url,
            Self::MarketEncoder(r) => &r.result_readback_url,
        }
    }
    fn study_id(&self) -> &str {
        match self {
            Self::Sequence(r) => &r.plan.study_id,
            Self::MarketEncoder(r) => &r.plan.study_id,
        }
    }
    fn pvc_name(&self) -> &str {
        match self {
            Self::Sequence(r) => &r.inputs.pvc_name,
            Self::MarketEncoder(r) => &r.inputs.pvc_name,
        }
    }
    fn pvc_uid(&self) -> &str {
        match self {
            Self::Sequence(r) => &r.inputs.pvc_uid,
            Self::MarketEncoder(r) => &r.inputs.pvc_uid,
        }
    }
    fn sub_path(&self) -> &str {
        match self {
            Self::Sequence(r) => &r.inputs.sub_path,
            Self::MarketEncoder(r) => &r.inputs.sub_path,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Submission {
    pub(super) purpose: Purpose,
    pub(super) attempt_id: String,
    pub(super) image: String,
    pub(super) request: StudyRequest,
}

pub(super) struct Validated {
    pub(super) submission: Submission,
    pub(super) request_json: String,
    pub(super) request_sha256: String,
    pub(super) identity: String,
    pub(super) job_name: String,
    pub(super) secret_name: String,
}

pub(crate) fn is_study_submission(path: &Path) -> anyhow::Result<bool> {
    let value: Value = admission::read_json(path)?;
    Ok(value["purpose"] == "sequence_study" || value["purpose"] == "market_encoder_study")
}

pub(super) fn validate(submission: Submission) -> anyhow::Result<Validated> {
    validate_dns_label("sequence attempt", &submission.attempt_id)?;
    submission.request.validate()?;
    if submission.purpose != submission.request.purpose() {
        bail!("study submission purpose differs from its typed request");
    }
    if submission.image != submission.request.image()
        || image_digest(&submission.image)? != submission.request.image_identity()
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
        submission.attempt_id,
        submission.request.campaign_id()
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
        request: (&request).into(),
    })?;
    data_mission::write_json_atomic(path, &validated.submission)?;
    Ok(SubmissionRenderReport {
        request_sha256: validated.request_sha256,
        submission_identity_sha256: validated.identity,
        job_name: validated.job_name,
        secret_name: validated.secret_name,
    })
}

pub(crate) fn write_market_submission(
    path: &Path,
    attempt_id: &str,
    image: &str,
    request: MarketRequest,
) -> anyhow::Result<SubmissionRenderReport> {
    let validated = validate(Submission {
        purpose: Purpose::MarketEncoderStudy,
        attempt_id: attempt_id.into(),
        image: image.into(),
        request: (&request).into(),
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
    request: impl Into<StudyRequest>,
    controller_image: &str,
) -> anyhow::Result<CampaignExecutionBindingV1> {
    let request = request.into();
    request.validate()?;
    image_digest(controller_image)?;
    // A Root binds the full protocol and declared development/withheld cohort.
    // Individual reservations additionally bind the exact fold receipt and mount.
    let (search, selection, cohort) = match &request {
        StudyRequest::Sequence(r) => {
            let search = canonical_json_hash(
                &json!({"schema":"sol-sequence-development-views-v1", "folds":r.plan.folds}),
            )?;
            let selection = canonical_json_hash(
                &json!({"schema":"sol-sequence-withheld-view-v1", "dataset":r.plan.sealed_dataset_sha256,"view":r.plan.sealed_view}),
            )?;
            let cohort = canonical_json_hash(
                &json!({"schema":"sol-sequence-cohort-v1", "input":r.plan.input,"development":search,"withheld":selection}),
            )?;
            (search, selection, cohort)
        }
        StudyRequest::MarketEncoder(r) => {
            let search = canonical_json_hash(
                &json!({"schema":"sol-market-encoder-development-views-v1", "folds":r.plan.folds}),
            )?;
            let selection = canonical_json_hash(
                &json!({"schema":"sol-market-encoder-withheld-views-v1", "independent_selection":r.plan.independent_selection,"sealed":r.plan.sealed}),
            )?;
            let cohort = canonical_json_hash(
                &json!({"schema":"sol-market-encoder-cohort-v1", "input":r.plan.input,"development":search,"withheld":selection,"stage_authority":r.stage_authority}),
            )?;
            (search, selection, cohort)
        }
    };
    let binding = CampaignExecutionBindingV1 {
        campaign_inputs_sha256: cohort,
        evaluation_protocol_sha256: request.plan_hash()?,
        evaluation_views: CampaignEvaluationViewsV1 {
            search_view_sha256: search,
            selection_view_sha256: selection,
            selection_feedback: CampaignSelectionFeedbackV1::IndependentSelectionWithheld,
        },
        source_revision: request.build_source_revision().to_owned(),
        runner_image: request.image().to_owned(),
        controller_image: controller_image.into(),
        job_cpu_millis: request.cpu_millis(),
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

/// A new market protocol cannot reset the original experiment's quota by
/// introducing a new family or Root. Its Root must already be a member of a
/// signed cumulative Study, whose transactional checks also guard every write.
fn require_cumulative_study(
    store: &alpha_store::AlphaStore,
    request: &StudyRequest,
    signed: &SignedCampaignRootGrantV1,
) -> anyhow::Result<()> {
    let StudyRequest::MarketEncoder(market) = request else {
        return Ok(());
    };
    let horizon = market
        .label_horizon()?
        .content_hash()
        .map_err(anyhow::Error::msg)?;
    let study_id = store
        .campaign_study_id_for_family(&request.family_id()?)?
        .context("market encoder requires an existing cumulative Campaign Study binding")?;
    let study = store
        .campaign_study_grant(&study_id)?
        .context("market encoder cumulative Study authority is missing")?;
    if study.grant.study_id != market.plan.study_id
        || study.grant.budget.max_trials > request.max_trials()
        || study.grant.budget.max_llm_tokens != 0
        || !study.grant.members.iter().any(|member| {
            member.matches_root(&signed.grant, &signed.content_sha256)
                && member.label_horizon_sha256 == horizon
        })
    {
        bail!("market encoder Study changes the scientific Study identity, cumulative fit ceiling or Root");
    }
    Ok(())
}

pub(super) fn render(
    validated: &Validated,
    control: &admission::DispatchControl,
    namespace: &str,
) -> anyhow::Result<Value> {
    let signed: SignedCampaignRootGrantV1 = admission::read_json(&control.signed_root_grant_path)?;
    signed.grant.validate()?;
    let request = &validated.submission.request;
    if signed.grant.execution != execution_binding(request, &control.controller_image)?
        || signed.grant.family.family_id != request.family_id()?
        || signed.grant.family.definition_sha256 != request.plan_hash()?
        || signed.grant.allowed_policy_revision_ids
            != std::collections::BTreeSet::from([request.policy_id()?])
        || signed.grant.max_follow_ups != 0
        || signed.grant.budget.max_llm_tokens != 0
        || signed.grant.budget.max_trials > request.root_trial_ceiling()?
        || signed.grant.family.max_trials > request.root_trial_ceiling()?
        || signed.grant.budget.max_job_attempts > request.root_job_count() as u64
    {
        bail!("sequence Root scope differs from fixed comparison");
    }
    // Authenticate prospective/historical authority without mistaking its total
    // cumulative budget for the allocation to each registered fold Job.
    let _ = admission::root_job_deadline(control)?;
    let deadline = ACTIVE_DEADLINE_SECONDS
        .min(signed.grant.budget.max_job_seconds / request.root_job_count() as u64);
    if deadline == 0 {
        bail!("sequence Root has no per-fold time budget");
    }
    let store = alpha_store::AlphaStore::open_read_only(&control.ledger_path)?;
    require_cumulative_study(&store, request, &signed)?;
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
            campaign_id: request.campaign_id(),
            job_name: &validated.job_name,
            secret_name: &validated.secret_name,
            image: &validated.submission.image,
            image_digest: request.image_identity(),
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
                request.campaign_id().to_owned(),
                "--image-identity".into(),
                request.image_identity().to_owned(),
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
        campaign_id: request.campaign_id().to_owned(),
        execution: execution_binding(request, &control.controller_image)?,
        generation: 0,
        parent_result_sha256: None,
        policy_revision_id: request.policy_id()?,
        request_sha256: validated.request_sha256.clone(),
        attempt_ordinal: control.attempt_ordinal,
        declared_trials: request.declared_trials()?,
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
    pod["volumes"].as_array_mut().context("missing sequence volumes")?.push(json!({"name":"sequence-data", "persistentVolumeClaim":{"claimName":request.pvc_name(),"readOnly":true}}));
    pod["containers"][0]["volumeMounts"].as_array_mut().context("missing sequence mounts")?.push(json!({"name":"sequence-data","mountPath":"/sequence-inputs","subPath":request.sub_path(),"readOnly":true}));
    if let StudyRequest::MarketEncoder(market) = request {
        pod["containers"][0]["resources"] = request.resources();
        pod["affinity"] = controller_affinity(&validated.identity[..32], namespace);
        let work_path = work_sub_path(&reservation)?;
        if market.stage_authority.work_pvc_name == market.inputs.pvc_name {
            // Same task PVC, disjoint subPaths. The input bind mount stays read-only.
            let volumes = pod["volumes"].as_array_mut().context("market volumes")?;
            for volume in volumes.iter_mut().filter(|v| v["name"] == "sequence-data") {
                volume["persistentVolumeClaim"]
                    .as_object_mut()
                    .context("input PVC source")?
                    .remove("readOnly");
            }
        }
        pod["volumes"][0] = json!({"name":"work","persistentVolumeClaim":{"claimName":market.stage_authority.work_pvc_name}});
        pod["containers"][0]["volumeMounts"][0] =
            json!({"name":"work","mountPath":"/work","subPath":work_path});
        pod["containers"][0]["env"] = json!([
            {"name":"MONDAY_CAMPAIGN_JOB_NAME","value":validated.job_name},
            {"name":"MONDAY_CAMPAIGN_POD_UID","valueFrom":{"fieldRef":{"apiVersion":"v1","fieldPath":"metadata.uid"}}}
        ]);
    }
    Ok(manifest)
}

pub(super) fn controller_affinity(identity: &str, namespace: &str) -> Value {
    json!({"podAffinity":{"requiredDuringSchedulingIgnoredDuringExecution":[{"labelSelector":{"matchLabels":{"research.monday/stage-controller":identity}},"namespaces":[namespace],"topologyKey":"kubernetes.io/hostname"}]}})
}

pub(super) fn work_sub_path(
    reservation: &alpha_domain::campaign_control::CampaignAttemptReservationV1,
) -> anyhow::Result<String> {
    Ok(format!(
        "sol-market-encoder-attempts/{}",
        reservation.operation_id()?
    ))
}

pub(crate) const SEQUENCE_INPUT_MOUNT: &str = "/sequence-inputs";

/// Settlement runs in its own Job. The training worker mount is not visible
/// to the Campaign controller, so readback mounts the same PVC and subPath.
pub(crate) fn render_readback_job(
    request: impl Into<StudyRequest>,
    controller_image: &str,
    namespace: &str,
    attempt_job_name: &str,
) -> anyhow::Result<Value> {
    let request = request.into();
    image_digest(controller_image)?;
    request.validate()?;
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
                        "subPath":request.sub_path(),
                        "readOnly":true
                    }],
                    "resources":request.resources()
                }],
                "volumes":[{
                    "name":"sequence-data",
                    "persistentVolumeClaim":{"claimName":request.pvc_name(),"readOnly":true}
                }]
            }}
        }
    }))
}

fn validate_cohort_mount(
    pod: &Value,
    request: &StudyRequest,
    mount_path: &str,
) -> anyhow::Result<()> {
    let volumes = pod["volumes"].as_array().context("missing study volumes")?;
    let mounts = pod["containers"][0]["volumeMounts"]
        .as_array()
        .context("missing study mounts")?;
    let shared_work = matches!(request,StudyRequest::MarketEncoder(r) if r.stage_authority.work_pvc_name==r.inputs.pvc_name)
        && volumes
            .iter()
            .any(|v| v["name"] == "work" && !v["persistentVolumeClaim"].is_null());
    let mut expected_volume = json!({"name":"sequence-data", "persistentVolumeClaim":{"claimName":request.pvc_name(),"readOnly":true}});
    if shared_work {
        expected_volume["persistentVolumeClaim"]
            .as_object_mut()
            .context("shared input PVC")?
            .remove("readOnly");
    }
    let expected_mount = json!({"name":"sequence-data","mountPath":mount_path,"subPath":request.sub_path(),"readOnly":true});
    if volumes
        .iter()
        .filter(|v| v["name"] == "sequence-data")
        .count()
        != 1
        || !volumes.contains(&expected_volume)
        || volumes.iter().any(|v| !v["persistentVolumeClaim"].is_null() && *v != expected_volume
            && !matches!(request,StudyRequest::MarketEncoder(r) if *v==json!({"name":"work","persistentVolumeClaim":{"claimName":r.stage_authority.work_pvc_name}})))
        || mounts
            .iter()
            .filter(|m| m["name"] == "sequence-data" || m["mountPath"] == mount_path)
            .count()
            != 1
        || !mounts.contains(&expected_mount)
    {
        bail!("study input mount differs from its exact read-only cohort");
    }
    Ok(())
}

fn bind_readback_mount(
    job: &Value,
    request: impl Into<StudyRequest>,
    input_root: &Path,
) -> anyhow::Result<()> {
    let request = request.into();
    let mount_path = input_root
        .to_str()
        .context("sequence input root must be UTF-8")?;
    let pod = &job["spec"]["template"]["spec"];
    validate_cohort_mount(pod, &request, mount_path)?;
    let container = &pod["containers"][0];
    let args = container["args"]
        .as_array()
        .context("sequence readback args missing")?;
    if args.iter().filter(|arg| *arg == "--input-root").count() != 1
        || pod["nodeSelector"] != cpu_research_node_selector()
        || container["resources"] != request.resources()
        || !args
            .windows(2)
            .any(|pair| pair[0] == "--input-root" && pair[1] == mount_path)
    {
        bail!("sequence settlement controller does not mount the requested cohort");
    }
    Ok(())
}

pub(super) fn inspection(
    validated: &Validated,
    control: &admission::DispatchControl,
    manifest: &Value,
) -> anyhow::Result<admission::DispatchInspection> {
    validated
        .submission
        .request
        .verify_input_receipt(&control.materialization_path)?;
    let pod = &manifest["items"][1]["spec"]["template"]["spec"];
    validate_cohort_mount(pod, &validated.submission.request, SEQUENCE_INPUT_MOUNT)?;
    if pod["containers"][0]["resources"] != validated.submission.request.resources()
        || pod["nodeSelector"] != cpu_research_node_selector()
        || !alpha_domain::research_accelerator::bind_pod_spec_accelerator(pod)
            .map_err(anyhow::Error::msg)?
            .is_cpu()
    {
        bail!("sequence rendered resources differ from CPU admission");
    }
    if let StudyRequest::MarketEncoder(r) = &validated.submission.request {
        let reservation: alpha_domain::campaign_control::CampaignAttemptReservationV1 =
            serde_json::from_str(
                manifest["items"][0]["stringData"]["sequence-attempt.json"]
                    .as_str()
                    .context("missing stage attempt")?,
            )?;
        let expected =
            json!({"name":"work","mountPath":"/work","subPath":work_sub_path(&reservation)?});
        let namespace = manifest["items"][1]["metadata"]["namespace"]
            .as_str()
            .context("market Job namespace is missing")?;
        if pod["affinity"] != controller_affinity(&validated.identity[..32], namespace)
            || pod["containers"][0]["volumeMounts"][0] != expected
            || pod["volumes"][0]
                != json!({"name":"work","persistentVolumeClaim":{"claimName":r.stage_authority.work_pvc_name}})
        {
            bail!("market durable work volume differs from its charged attempt");
        }
    }
    Ok(admission::DispatchInspection {
        execution: execution_binding(&validated.submission.request, &control.controller_image)?,
        campaign_id: validated.submission.request.campaign_id().to_owned(),
        generation: 0,
        parent_result_sha256: None,
        policy_revision_id: validated.submission.request.policy_id()?,
        request_sha256: validated.request_sha256.clone(),
        attempt_ordinal: control.attempt_ordinal,
        declared_trials: validated.submission.request.declared_trials()?,
        reserved_job_seconds: manifest["items"][1]["spec"]["activeDeadlineSeconds"]
            .as_u64()
            .context("missing sequence deadline")?,
        reserved_llm_tokens: 0,
    })
}

fn verify_pvc(request: &StudyRequest, context: &str, namespace: &str) -> anyhow::Result<()> {
    let pvc = kubectl_json(
        &kubectl_binary(),
        context,
        namespace,
        [
            "--request-timeout=30s",
            "get",
            "pvc",
            request.pvc_name(),
            "-o",
            "json",
        ],
        "verify sequence input PVC",
    )?;
    if pvc["metadata"]["uid"] != request.pvc_uid() || pvc["status"]["phase"] != "Bound" {
        bail!("sequence input PVC identity or state changed");
    }
    if let StudyRequest::MarketEncoder(r) = request {
        let work = kubectl_json(
            &kubectl_binary(),
            context,
            namespace,
            [
                "--request-timeout=30s",
                "get",
                "pvc",
                r.stage_authority.work_pvc_name.as_str(),
                "-o",
                "json",
            ],
            "verify market work PVC",
        )?;
        if work["metadata"]["uid"] != r.stage_authority.work_pvc_uid
            || work["status"]["phase"] != "Bound"
        {
            bail!("market work PVC identity changed");
        }
    }
    Ok(())
}

struct StudyAdmission<'a> {
    inner: admission::Admission,
    request: &'a StudyRequest,
    context: &'a str,
    namespace: &'a str,
}
impl DispatchAdmission for StudyAdmission<'_> {
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

pub(crate) fn describe(args: crate::cli::DescribeStudyArgs) -> anyhow::Result<()> {
    let validated = validate(admission::read_json(&args.submission)?)?;
    let request = &validated.submission.request;
    let label_horizon = match request {
        StudyRequest::MarketEncoder(r) => Some(r.label_horizon()?),
        StudyRequest::Sequence(_) => None,
    };
    let label_horizon_sha256 = label_horizon
        .as_ref()
        .map(|h| h.content_hash().map_err(anyhow::Error::msg))
        .transpose()?;
    print_json(&json!({
        "schema_version":"monday.typed_study_dispatch_binding.v1","campaign_id":request.campaign_id(),
        "request_sha256":validated.request_sha256,"job_name":validated.job_name,
        "family_id":request.family_id()?,"family_definition_sha256":request.plan_hash()?,"scientific_study_id":request.study_id(),
        "family_max_trials_ceiling":request.root_trial_ceiling()?,"study_max_trials_ceiling":request.max_trials(),"max_root_job_attempts":request.root_job_count(),
        "declared_trials":request.declared_trials()?,"policy_revision_id":request.policy_id()?,
        "execution":execution_binding(request,&args.controller_image)?,
        "label_horizon":label_horizon,"label_horizon_sha256":label_horizon_sha256,
        "remaining_budget_verified":false,"accounting_changed":false,"grants_authority":false,
    }))
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
    if validated.submission.request.build_source_revision() != crate::cli::BUILD_SOURCE_REVISION {
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
            result_readback_url: validated.submission.request.result_readback_url(),
            study_proposal: None,
            context: &args.context,
            namespace: &args.namespace,
        },
        admission::Purpose::Dispatch,
        false,
    )?;
    let mut admitted = StudyAdmission {
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
        &json!({"status":"submitted","execution_scope":"pre_holdout","campaign_id":validated.submission.request.campaign_id(),
        "request_sha256":validated.request_sha256,"job_name":validated.job_name,"operation_id":admitted.inner.reservation.operation_id()?,"reserved_trials":validated.submission.request.declared_trials()?}),
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
            result_readback_url: validated.submission.request.result_readback_url(),
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
        "request_sha256":validated.request_sha256,"campaign_id":validated.submission.request.campaign_id(),
        "job_name":record.claim.target.job_name,"job_uid":record.claim.job_uid,
        "authority_deadline_epoch":record.root.grant().expires_at.timestamp(),"reserved_trials":record.reservation.declared_trials,
        "settlement":record.settlement,"cancellation":record.cancellation,
        "completion_provenance":record.completion_provenance,"accounting_changed":false}),
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
    if let Some(settlement) = &record.settlement {
        if matches!(
            &validated.submission.request,
            StudyRequest::MarketEncoder(_)
        ) && report_path.try_exists()?
        {
            let report: Value = admission::read_json(&report_path)?;
            if report["schema_version"] == "monday.market_completion_authority_failure.v1" {
                if settlement.outcome
                    != alpha_domain::campaign_control::CampaignAttemptOutcomeV1::Failed
                    || settlement.consumed_trials != Some(admitted.reservation.declared_trials)
                    || canonical_json_hash(&report)? != settlement.evidence_sha256
                    || report["completion"]["job_uid"].as_str() != record.claim.job_uid.as_deref()
                    || report["completion"]["pod_uid"].as_str()
                        != record.terminal_pod_uid.as_deref()
                {
                    bail!(
                        "completion authority failure cache differs from authenticated settlement"
                    );
                }
                admitted.publish_receipts()?;
                return print_json(&report);
            }
        }
    }
    if let Some(cancellation) = &record.cancellation {
        let report = if record.settlement.is_some() {
            admission::read_json::<Value>(&report_path)?
        } else {
            super::stage_controller::read_cancelled_terminal(
                &args.context,
                &args.namespace,
                &manifest["items"][1],
                cancellation,
            )?
            .context(
                "cancellation was requested but terminal failure is not independently confirmed",
            )?
        };
        let evidence_sha256 = canonical_json_hash(&report)?;
        if record
            .settlement
            .as_ref()
            .is_some_and(|s| s.evidence_sha256 != evidence_sha256)
        {
            bail!("cancelled terminal cache differs from settled evidence");
        }
        if record.settlement.is_none() {
            data_mission::write_json_atomic(&report_path, &report)?;
            admitted.settle(
                &alpha_store::campaign_ledger::CampaignDispatchSettlementV1 {
                    completion_provenance: None,
                    job_uid: cancellation.job_uid.clone(),
                    pod_uid: cancellation.pod_uid.clone(),
                    settlement: alpha_domain::campaign_control::CampaignAttemptSettlementV1 {
                        operation_id: admitted.reservation.operation_id()?,
                        reservation_sha256: admitted.reservation.content_hash()?,
                        evidence_sha256: evidence_sha256.clone(),
                        outcome: alpha_domain::campaign_control::CampaignAttemptOutcomeV1::Failed,
                        consumed_trials: Some(admitted.reservation.declared_trials),
                    },
                },
            )?;
        }
        admitted.publish_receipts()?;
        return print_json(
            &json!({"status":"settled_cancelled","operation_id":admitted.reservation.operation_id()?,
            "job_uid":cancellation.job_uid,"pod_uid":cancellation.pod_uid,"evidence_sha256":evidence_sha256,
            "charged_trials":admitted.reservation.declared_trials,"sealed_holdout_opened":false,"model_report":report_path}),
        );
    }
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
    let completion = if matches!(
        &validated.submission.request,
        StudyRequest::MarketEncoder(_)
    ) {
        terminal
            .as_ref()
            .map(|terminal| terminal.completion(chrono::Utc::now()))
            .transpose()?
    } else {
        None
    };
    if let Some(completion) = &completion {
        if !admitted.completion_active(completion)? {
            let terminal = terminal
                .as_ref()
                .context("missing bound terminal evidence")?;
            let report = json!({
                "schema_version":"monday.market_completion_authority_failure.v1",
                "operation_id":admitted.reservation.operation_id()?,
                "request_sha256":validated.request_sha256,
                "root_grant_sha256":admitted.reservation.root_grant_sha256,
                "completion":completion,
                "job_sha256":canonical_json_hash(&terminal.job)?,
                "pod_sha256":canonical_json_hash(&terminal.pod)?,
                "kubernetes_outcome":"Complete", "research_outcome":"Failed",
                "reason":"campaign_authority_invalid_at_completion",
                "charged_trials":admitted.reservation.declared_trials,
                "sealed_holdout_opened":false,
            });
            let evidence = alpha_store::campaign_ledger::CampaignDispatchSettlementV1 {
                completion_provenance: None,
                job_uid: completion.job_uid.clone(),
                pod_uid: completion.pod_uid.clone(),
                settlement: alpha_domain::campaign_control::CampaignAttemptSettlementV1 {
                    operation_id: admitted.reservation.operation_id()?,
                    reservation_sha256: admitted.reservation.content_hash()?,
                    evidence_sha256: canonical_json_hash(&report)?,
                    outcome: alpha_domain::campaign_control::CampaignAttemptOutcomeV1::Failed,
                    consumed_trials: Some(admitted.reservation.declared_trials),
                },
            };
            data_mission::write_json_atomic(&report_path, &report)?;
            admitted.settle_at_completion(&evidence, completion)?;
            admitted.publish_receipts()?;
            return print_json(&report);
        }
    }
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
    let settled_hash = record
        .settlement
        .as_ref()
        .map(|s| s.evidence_sha256.as_str());
    let (outcome, hash, report) = match &validated.submission.request {
        StudyRequest::Sequence(request) => crate::mission_campaign::sequence::readback::readback(
            request,
            &validated.request_sha256,
            &admitted.reservation.root_grant_sha256,
            input_root,
            cache,
            settled_hash,
        )?,
        StudyRequest::MarketEncoder(request) => {
            crate::mission_campaign::market_encoder::readback::readback(
                request,
                &validated.request_sha256,
                &admitted.reservation.root_grant_sha256,
                &admitted.reservation.content_hash()?,
                record
                    .claim
                    .job_uid
                    .as_deref()
                    .context("market readback requires claimed Job UID")?,
                input_root,
                cache,
                settled_hash,
            )?
        }
    };
    data_mission::write_json_atomic(&report_path, &report)?;
    if let Some(terminal) = terminal {
        let evidence = alpha_store::campaign_ledger::CampaignDispatchSettlementV1 {
            completion_provenance: None,
            job_uid: terminal.job_uid,
            pod_uid: terminal.pod_uid,
            settlement: alpha_domain::campaign_control::CampaignAttemptSettlementV1 {
                operation_id: admitted.reservation.operation_id()?,
                reservation_sha256: admitted.reservation.content_hash()?,
                evidence_sha256: hash.clone(),
                outcome, // Charge the entire attempt; failed fits are never refunded.
                consumed_trials: Some(admitted.reservation.declared_trials),
            },
        };
        if let Some(completion) = &completion {
            admitted.settle_at_completion(&evidence, completion)?;
        } else {
            admitted.settle(&evidence)?;
        }
    }
    admitted.publish_receipts()?;
    let record = admitted.record()?;
    print_json(
        &json!({"status":"settled","operation_id":admitted.reservation.operation_id()?,
        "campaign_id":validated.submission.request.campaign_id(),"job_name":validated.job_name,
        "job_uid":record.claim.job_uid,"pod_uid":record.terminal_pod_uid,"result_sha256":hash,
        "settlement":record.settlement,"completion_provenance":record.completion_provenance,
        "model_report":report_path,"sealed_holdout_opened":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use alpha_domain::campaign_control::*;
    use chrono::{TimeDelta, Utc};
    use std::collections::BTreeSet;

    fn check_dispatch_ledger_and_historical_readback(request: StudyRequest, revoke_root: bool) {
        check_dispatch_case(request, revoke_root, None);
    }

    fn check_dispatch_case(
        request: StudyRequest,
        revoke_root: bool,
        completion_case: Option<&str>,
    ) {
        let root = tempfile::tempdir().unwrap();
        let expected_trials = request.declared_trials().unwrap();
        let is_market = matches!(&request, StudyRequest::MarketEncoder(_));
        let validated = validate(Submission {
            purpose: request.purpose(),
            attempt_id: "sequence-test".into(),
            image: request.image().to_owned(),
            request: (&request).into(),
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
                    family_id: request.family_id().unwrap(),
                    definition_sha256: request.plan_hash().unwrap(),
                    max_trials: request.root_trial_ceiling().unwrap(),
                },
                execution_scope: CampaignExecutionScope::PreHoldout,
                execution: execution_binding(&request, &controller).unwrap(),
                allowed_policy_revision_ids: BTreeSet::from([request.policy_id().unwrap()]),
                max_follow_ups: 0,
                budget: CampaignRootBudgetV1 {
                    max_trials: expected_trials * request.root_job_count() as u64,
                    max_job_attempts: request.root_job_count() as u64,
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
        if is_market {
            let keys = BTreeMap::from([("operator".to_owned(), signing.verifying_key())]);
            let verified = verify_campaign_root_grant(&signed, &keys, now).unwrap();
            store
                .register_campaign_root(&verified, "sequence-approval", now)
                .unwrap();
            assert!(require_cumulative_study(&store, &request, &signed).is_err());
            register_market_study(&mut store, &signed, &signing, 43, now);
            require_cumulative_study(&store, &request, &signed).unwrap();
        }
        drop(store);
        for (name, value) in [
            ("grant.json", serde_json::to_value(&signed).unwrap()),
            (
                "keys.json",
                json!({"operator":hex::encode(signing.verifying_key().as_bytes())}),
            ),
            ("inputs.json", request.inputs_json().unwrap()),
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
            receipt_access: (1..=16).flat_map(|sequence| {
                let mut keys = vec![format!("research/campaign-ledger/family-id={}/sequence={sequence:020}/receipt.json", request.family_id().unwrap())];
                if is_market { keys.push(format!("research/campaign-ledger/study-id=sol-market-encoder-controlled-test/sequence={sequence:020}/receipt.json")); }
                keys.into_iter().map(|key| {
                let url = format!("https://monday-lob-apne1-1045353359.oss-ap-northeast-1-internal.aliyuncs.com/{key}?signature=fixture");
                (key, serde_json::from_value(json!({"put_url":url,"readback_url":url})).unwrap())
                })
            }).collect(),
        };
        let manifest = render(&validated, &control, "monday-research").unwrap();
        assert_eq!(
            manifest["items"][1]["spec"]["activeDeadlineSeconds"],
            2400 / request.root_job_count() as u64
        );
        assert_eq!(
            manifest["items"][1]["spec"]["template"]["spec"]["containers"][0]["resources"]
                ["requests"]["cpu"],
            if is_market { "3" } else { "3500m" }
        );
        let reserved: CampaignAttemptReservationV1 = serde_json::from_str(
            manifest["items"][0]["stringData"]["sequence-attempt.json"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        if is_market {
            assert_eq!(
                manifest["items"][1]["spec"]["template"]["spec"]["containers"][0]["env"][1]
                    ["valueFrom"]["fieldRef"]["apiVersion"],
                "v1"
            );
            let mut served = manifest["items"][1].clone();
            served["spec"]["suspend"] = json!(false);
            validate_job_readback(
                &served,
                &manifest["items"][1],
                &validated.job_name,
                &validated.request_sha256,
                false,
            )
            .unwrap();
            served["spec"]["template"]["spec"]["containers"][0]["env"][1]["valueFrom"]
                ["fieldRef"]["fieldPath"] = json!("metadata.name");
            assert!(validate_job_readback(
                &served,
                &manifest["items"][1],
                &validated.job_name,
                &validated.request_sha256,
                false
            )
            .is_err());
        }
        let inspected = inspection(&validated, &control, &manifest).unwrap();
        if is_market {
            let mut custom = render(&validated, &control, "market-custom-namespace").unwrap();
            inspection(&validated, &control, &custom).unwrap();
            custom["items"][1]["spec"]["template"]["spec"]["affinity"] =
                controller_affinity(&validated.identity[..32], "monday-research");
            assert!(inspection(&validated, &control, &custom).is_err());
        }
        let mut gpu = manifest.clone();
        gpu["items"][1]["spec"]["template"]["spec"]["containers"][0]["resources"]["limits"]
            ["nvidia.com/gpu"] = json!(1);
        assert!(inspection(&validated, &control, &gpu).is_err());
        let mut gpu = manifest.clone();
        gpu["items"][1]["spec"]["template"]["spec"]["nodeSelector"] = json!({"workload":"gpu"});
        assert!(inspection(&validated, &control, &gpu).is_err());
        let mut widened_mount = manifest.clone();
        widened_mount["items"][1]["spec"]["template"]["spec"]["containers"][0]["volumeMounts"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name":"sequence-data","mountPath":"/all-inputs","readOnly":true}));
        assert!(inspection(&validated, &control, &widened_mount).is_err());
        let mut admitted = admission::Admission::from_inspection(
            control.clone(),
            admission::InspectedDispatch {
                inspection: inspected,
                manifest: &manifest,
                job_name: &validated.job_name,
                result_readback_url: request.result_readback_url(),
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
        assert_eq!(record.reservation.declared_trials, expected_trials);
        assert_eq!(record.claim.target.require_completion_authority, is_market);
        if let Some(case) = completion_case {
            drop(admitted);
            let mut store = alpha_store::AlphaStore::open(&control.ledger_path).unwrap();
            let before = store
                .campaign_study_usage("sol-market-encoder-controlled-test")
                .unwrap();
            let mut downgrade = record.claim.target.clone();
            downgrade.require_completion_authority = false;
            assert!(store
                .claim_campaign_dispatch(&record.root, &reserved, &downgrade, now)
                .unwrap_err()
                .to_string()
                .contains("dispatch target changed"));
            let mut completion = alpha_store::campaign_ledger::CampaignDispatchCompletionV1 {
                job_uid: "sequence-job-uid".into(),
                pod_uid: "successful-pod".into(),
                job_sha256: "a".repeat(64),
                pod_sha256: "b".repeat(64),
                job_started_at: now,
                completed_at: now + TimeDelta::seconds(2),
            };
            let mut observed_at = now + TimeDelta::seconds(10);
            assert!(store
                .campaign_dispatch_completion_active(&reserved, &completion, observed_at)
                .unwrap());
            let mut invalid = completion.clone();
            invalid.job_uid = "another-job".into();
            assert!(store
                .campaign_dispatch_completion_active(&reserved, &invalid, observed_at)
                .is_err());
            invalid = completion.clone();
            invalid.completed_at = now - TimeDelta::seconds(1);
            assert!(store
                .campaign_dispatch_completion_active(&reserved, &invalid, observed_at)
                .is_err());
            assert!(store
                .campaign_dispatch_completion_active(&reserved, &completion, now)
                .is_err());
            if case == "expired" {
                completion.completed_at = signed.grant.expires_at + TimeDelta::seconds(1);
                observed_at = completion.completed_at;
            } else {
                // Record revocation after the optimistic read above. Settlement
                // must independently recompute the decision inside its write.
                store
                    .revoke_approval(
                        if revoke_root {
                            "sequence-approval"
                        } else {
                            "study-approval"
                        },
                        "operator",
                        "terminal authorization regression",
                        now + TimeDelta::seconds(if case == "completed_before_revoke" {
                            3
                        } else {
                            1
                        }),
                    )
                    .unwrap();
            }
            let expected_active = case == "completed_before_revoke";
            assert_eq!(
                store
                    .campaign_dispatch_completion_active(&reserved, &completion, observed_at)
                    .unwrap(),
                expected_active
            );
            let mut evidence = alpha_store::campaign_ledger::CampaignDispatchSettlementV1 {
                completion_provenance: None,
                job_uid: completion.job_uid.clone(),
                pod_uid: completion.pod_uid.clone(),
                settlement: CampaignAttemptSettlementV1 {
                    operation_id: reserved.operation_id().unwrap(),
                    reservation_sha256: reserved.content_hash().unwrap(),
                    evidence_sha256: "f".repeat(64),
                    outcome: CampaignAttemptOutcomeV1::SelectedPreHoldout,
                    consumed_trials: Some(reserved.declared_trials),
                },
            };
            assert!(store
                .settle_campaign_dispatch(&reserved, &evidence, observed_at)
                .unwrap_err()
                .to_string()
                .contains("requires completion authority"));
            assert!(store
                .settle_campaign_attempt(&reserved.family_id, &evidence.settlement, observed_at)
                .is_err());
            let mut refund = evidence.clone();
            refund.settlement.consumed_trials = Some(0);
            assert!(store
                .settle_campaign_dispatch(&reserved, &refund, observed_at)
                .is_err());
            if !expected_active {
                assert!(store
                    .settle_campaign_dispatch_at_completion(
                        &reserved,
                        &evidence,
                        &completion,
                        observed_at
                    )
                    .unwrap_err()
                    .to_string()
                    .contains("requires Failed"));
                assert!(store
                    .campaign_dispatch_record(
                        &reserved.family_id,
                        &reserved.operation_id().unwrap()
                    )
                    .unwrap()
                    .settlement
                    .is_none());
                evidence.settlement.outcome = CampaignAttemptOutcomeV1::Failed;
                evidence.settlement.consumed_trials = Some(0);
                assert!(store
                    .settle_campaign_dispatch_at_completion(
                        &reserved,
                        &evidence,
                        &completion,
                        observed_at
                    )
                    .is_err());
                evidence.settlement.consumed_trials = Some(reserved.declared_trials);
            }
            let mut mismatched = evidence.clone();
            mismatched.completion_provenance = Some(
                alpha_store::campaign_ledger::CampaignDispatchCompletionProvenanceV1 {
                    completion: completion.clone(),
                    authority_active_at_completion: !expected_active,
                },
            );
            assert!(store
                .settle_campaign_dispatch_at_completion(
                    &reserved,
                    &mismatched,
                    &completion,
                    observed_at
                )
                .is_err());
            let receipt = store
                .settle_campaign_dispatch_at_completion(
                    &reserved,
                    &evidence,
                    &completion,
                    observed_at,
                )
                .unwrap();
            let alpha_store::campaign_ledger::CampaignLedgerEventV1::DispatchSettled {
                evidence: persisted,
            } = &receipt.receipt.event
            else {
                panic!("not native settlement");
            };
            let provenance = persisted.completion_provenance.as_ref().unwrap();
            assert_eq!(provenance.completion, completion);
            assert_eq!(provenance.authority_active_at_completion, expected_active);
            assert_eq!(
                persisted.settlement.evidence_sha256,
                "f".repeat(64),
                "remote result identity is unchanged"
            );
            let bytes = serde_json::to_vec(&receipt).unwrap();
            let decoded: alpha_store::campaign_ledger::AuthenticatedCampaignReceiptV1 =
                serde_json::from_slice(&bytes).unwrap();
            assert_eq!(decoded, receipt);
            assert_eq!(
                canonical_json_hash(&decoded.receipt).unwrap(),
                decoded.content_sha256
            );
            let snapshot = store
                .campaign_study_snapshot("sol-market-encoder-controlled-test")
                .unwrap();
            for field in ["time", "job_hash", "pod_hash", "decision", "missing"] {
                let mut tampered = snapshot.clone();
                let last = tampered.member_snapshots[0].receipts.last_mut().unwrap();
                let alpha_store::campaign_ledger::CampaignLedgerEventV1::DispatchSettled {
                    evidence,
                } = &mut last.receipt.event
                else {
                    panic!("not terminal snapshot");
                };
                if field == "missing" {
                    evidence.completion_provenance = None;
                } else {
                    let provenance = evidence.completion_provenance.as_mut().unwrap();
                    match field {
                        "time" => provenance.completion.completed_at += TimeDelta::seconds(1),
                        "job_hash" => provenance.completion.job_sha256 = "c".repeat(64),
                        "pod_hash" => provenance.completion.pod_sha256 = "d".repeat(64),
                        "decision" => provenance.authority_active_at_completion = !expected_active,
                        _ => unreachable!(),
                    }
                }
                assert!(
                    store.import_campaign_study_snapshot(&tampered).is_err(),
                    "accepted tampered {field}"
                );
            }
            drop(store);
            let store = alpha_store::AlphaStore::open(&control.ledger_path).unwrap();
            let terminal = store
                .campaign_dispatch_record(&reserved.family_id, &reserved.operation_id().unwrap())
                .unwrap();
            assert_eq!(terminal.settlement, Some(evidence.settlement));
            assert_eq!(terminal.completion_provenance.as_ref(), Some(provenance));
            assert!(
                terminal.cancellation.is_none(),
                "Kubernetes Complete was not patched or relabelled Failed"
            );
            let after = store
                .campaign_study_usage("sol-market-encoder-controlled-test")
                .unwrap();
            assert_eq!(
                before.accounted_trials().unwrap(),
                after.accounted_trials().unwrap()
            );
            assert_eq!(after.consumed_trials, reserved.declared_trials);
            return;
        }
        if is_market {
            // This fold Root has one admitted Job; a retry is not another fold.
            // Cross-fold cumulative rejection is covered by the two-Root test.
            let mut next = admitted.reservation.clone();
            next.attempt_ordinal = 1;
            drop(admitted);
            let mut store = alpha_store::AlphaStore::open(&control.ledger_path).unwrap();
            let verified = verify_campaign_root_grant(
                &signed,
                &BTreeMap::from([("operator".into(), signing.verifying_key())]),
                now,
            )
            .unwrap();
            let error = store
                .reserve_campaign_attempt(&verified, &next, now)
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("attempt outside root grant")
                    || error.to_ascii_lowercase().contains("study"),
                "{error}"
            );
            let before = store
                .campaign_study_usage("sol-market-encoder-controlled-test")
                .unwrap();
            let late_start = signed.grant.expires_at
                - TimeDelta::seconds(reserved.reserved_job_seconds as i64 + 1);
            let late = late_start + TimeDelta::seconds(reserved.reserved_job_seconds as i64 - 5);
            assert!(store
                .inspect_campaign_reservation(&verified, &reserved, late)
                .is_err());
            // This is the still-running original Job, not a new full-duration admission.
            let actual_deadline: chrono::DateTime<Utc> = store
                .with_running_campaign_admission(
                    &verified,
                    &reserved,
                    &record.claim.target,
                    "sequence-job-uid",
                    late_start,
                    || late,
                    |_, deadline| -> anyhow::Result<_> { Ok(deadline) },
                )
                .unwrap();
            assert_eq!(
                actual_deadline,
                late_start + TimeDelta::seconds(reserved.reserved_job_seconds as i64)
            );
            let wrong: Result<(), anyhow::Error> = store.with_running_campaign_admission(
                &verified,
                &reserved,
                &record.claim.target,
                "other-job",
                now,
                || now,
                |_, _| panic!("wrong writer cannot receive permission"),
            );
            assert!(wrong.is_err());
            assert_eq!(
                before,
                store
                    .campaign_study_usage("sol-market-encoder-controlled-test")
                    .unwrap()
            );
            store
                .revoke_approval(
                    if revoke_root {
                        "sequence-approval"
                    } else {
                        "study-approval"
                    },
                    "operator",
                    "stop after P",
                    Utc::now(),
                )
                .unwrap();
            let revoked: Result<(), anyhow::Error> = store.with_running_campaign_admission(
                &verified,
                &reserved,
                &record.claim.target,
                "sequence-job-uid",
                now,
                Utc::now,
                |_, _| panic!("revoked C must not receive permission"),
            );
            assert!(revoked.is_err());
            let patch = super::super::stage_controller::cancellation_patch(
                "sequence-job-uid",
                "before-patch",
            );
            let cancellation = alpha_store::campaign_ledger::CampaignDispatchCancellationV1 {
                operation_id: reserved.operation_id().unwrap(),
                job_uid: "sequence-job-uid".into(),
                pod_uid: "cancelled-pod".into(),
                original_resource_version: "before-patch".into(),
                patched_resource_version: "after-patch".into(),
                reason: "current approval revoked".into(),
                requested_at: Utc::now(),
                job_started_at: now,
                original_deadline_at: now
                    + TimeDelta::seconds(reserved.reserved_job_seconds as i64),
                patch_sha256: canonical_json_hash(&patch).unwrap(),
                patch_result_sha256: "f".repeat(64),
            };
            store
                .record_campaign_dispatch_cancellation(&reserved, &cancellation)
                .unwrap();
            store
                .record_campaign_dispatch_cancellation(&reserved, &cancellation)
                .unwrap();
            assert_eq!(
                before,
                store
                    .campaign_study_usage("sol-market-encoder-controlled-test")
                    .unwrap()
            );
            let evidence = alpha_store::campaign_ledger::CampaignDispatchSettlementV1 {
                completion_provenance: None,
                job_uid: cancellation.job_uid.clone(),
                pod_uid: cancellation.pod_uid.clone(),
                settlement: CampaignAttemptSettlementV1 {
                    operation_id: reserved.operation_id().unwrap(),
                    reservation_sha256: reserved.content_hash().unwrap(),
                    evidence_sha256: "f".repeat(64),
                    outcome: CampaignAttemptOutcomeV1::Failed,
                    consumed_trials: Some(reserved.declared_trials),
                },
            };
            let mut refunded = evidence.clone();
            refunded.settlement.consumed_trials = Some(0);
            assert!(store
                .settle_campaign_dispatch(&reserved, &refunded, Utc::now())
                .is_err());
            let mut promoted = evidence.clone();
            promoted.settlement.outcome = CampaignAttemptOutcomeV1::SelectedPreHoldout;
            assert!(store
                .settle_campaign_dispatch(&reserved, &promoted, Utc::now())
                .is_err());
            store
                .settle_campaign_dispatch(&reserved, &evidence, Utc::now())
                .unwrap();
            let after = store
                .campaign_study_usage("sol-market-encoder-controlled-test")
                .unwrap();
            assert_eq!(
                after.accounted_trials().unwrap(),
                before.accounted_trials().unwrap()
            );
            assert_eq!(after.consumed_trials, reserved.declared_trials);
            assert!(store
                .inspect_campaign_reservation(&verified, &reserved, now)
                .is_err());
        } else {
            drop(admitted);
        }
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

    fn register_market_study(
        store: &mut alpha_store::AlphaStore,
        signed: &SignedCampaignRootGrantV1,
        signing: &ed25519_dalek::SigningKey,
        ceiling: u64,
        now: chrono::DateTime<Utc>,
    ) {
        use alpha_domain::campaign_study::*;
        let signed_study = sign_campaign_study_grant(
            CampaignStudyGrantV1 {
                schema_version: STUDY_GRANT_SCHEMA.into(),
                study_id: "sol-market-encoder-controlled-test".into(),
                members: vec![CampaignStudyMemberV1 {
                    family_id: signed.grant.family.family_id.clone(),
                    root_grant_sha256: signed.content_sha256.clone(),
                    family_definition_sha256: signed.grant.family.definition_sha256.clone(),
                    family_max_trials: signed.grant.family.max_trials,
                    execution_scope: signed.grant.execution_scope.clone(),
                    execution: signed.grant.execution.clone(),
                    label_horizon_sha256:
                        crate::mission_campaign::market_encoder::request_tests::request()
                            .label_horizon()
                            .unwrap()
                            .content_hash()
                            .unwrap(),
                }],
                budget: CampaignStudyBudgetV1 {
                    max_trials: ceiling,
                    max_job_attempts: 4,
                    max_job_seconds: 2400,
                    max_llm_tokens: 0,
                },
                valid_from: signed.grant.valid_from,
                expires_at: signed.grant.expires_at,
            },
            "operator".into(),
            signing,
        )
        .unwrap();
        let verified = verify_campaign_study_grant(
            &signed_study,
            &BTreeMap::from([("operator".into(), signing.verifying_key())]),
            now,
        )
        .unwrap();
        store.record_approval(&alpha_store::ApprovalRecord {
            approval_id: "study-approval".into(), approval_class: "campaign_study".into(), subject_id: signed_study.grant.study_id.clone(),
            payload: json!({"grant_sha256": signed_study.content_sha256, "study_id": signed_study.grant.study_id}), signer_id: Some("operator".into()),
            valid_from: Some(signed.grant.valid_from), expires_at: Some(signed.grant.expires_at), revoked_at: None, revoked_by: None,
            revocation_reason: None, created_at: signed.grant.valid_from,
        }).unwrap();
        store
            .register_campaign_study(&verified, "study-approval", now)
            .unwrap();
    }
    #[test]
    fn sequence_dispatch_reuses_cumulative_ledger_and_historical_readback() {
        check_dispatch_ledger_and_historical_readback(
            (&crate::mission_campaign::sequence::tests::request()).into(),
            false,
        );
    }
    #[test]
    fn market_completion_authority_is_temporal_and_rechecked_at_settlement() {
        for revoke_root in [false, true] {
            for case in [
                "completed_before_revoke",
                "revoked_during_final_fit",
                "expired",
            ] {
                check_dispatch_case(
                    (&crate::mission_campaign::market_encoder::request_tests::request()).into(),
                    revoke_root,
                    Some(case),
                );
            }
        }
    }
    #[test]
    fn market_dispatch_reuses_cumulative_study_and_historical_readback() {
        check_dispatch_ledger_and_historical_readback(
            (&crate::mission_campaign::market_encoder::request_tests::request()).into(),
            false,
        );
    }

    #[test]
    fn market_running_root_revocation_preserves_failed_cancelled_budget() {
        check_dispatch_ledger_and_historical_readback(
            (&crate::mission_campaign::market_encoder::request_tests::request()).into(),
            true,
        );
    }

    #[test]
    fn market_shared_task_pvc_keeps_input_and_work_subpaths_isolated() {
        let mut request = crate::mission_campaign::market_encoder::request_tests::request();
        request.stage_authority.work_pvc_name = request.inputs.pvc_name.clone();
        request.stage_authority.work_pvc_uid = request.inputs.pvc_uid.clone();
        crate::mission_campaign::market_encoder::request_tests::rebind(&mut request);
        check_dispatch_ledger_and_historical_readback((&request).into(), false);
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
    #[test]
    fn market_submission_keeps_native_request_bytes_and_rejects_wrong_purpose() {
        let market = crate::mission_campaign::market_encoder::request_tests::request();
        let old = crate::mission_campaign::sequence::tests::request();
        for request in [StudyRequest::from(&old), StudyRequest::from(&market)] {
            let expected = match &request {
                StudyRequest::Sequence(r) => serde_json::to_string_pretty(r).unwrap(),
                StudyRequest::MarketEncoder(r) => serde_json::to_string_pretty(r).unwrap(),
            };
            let value = validate(Submission {
                purpose: request.purpose(),
                attempt_id: "wire-test".into(),
                image: request.image().into(),
                request: request.clone(),
            })
            .unwrap();
            assert_eq!(value.request_json, expected);
            let decoded: Submission =
                serde_json::from_slice(&serde_json::to_vec(&value.submission).unwrap()).unwrap();
            assert_eq!(
                validate(decoded).unwrap().request_sha256,
                value.request_sha256
            );
            let mut wrong = value.submission;
            wrong.purpose = match wrong.purpose {
                Purpose::SequenceStudy => Purpose::MarketEncoderStudy,
                Purpose::MarketEncoderStudy => Purpose::SequenceStudy,
            };
            assert!(validate(wrong).is_err());
        }
    }
    #[test]
    fn market_readback_keeps_cpu_cohort_and_withheld_identity() {
        let request = crate::mission_campaign::market_encoder::request_tests::request();
        let controller = format!("registry/controller@sha256:{}", "f".repeat(64));
        let binding = execution_binding(&request, &controller).unwrap();
        assert!(binding.accelerator.is_cpu());
        let job = render_readback_job(
            &request,
            &controller,
            "monday-research",
            "alpha-campaign-test",
        )
        .unwrap();
        bind_readback_mount(&job, &request, Path::new(SEQUENCE_INPUT_MOUNT)).unwrap();
        let mut changed = job.clone();
        changed["spec"]["template"]["spec"]["containers"][0]["volumeMounts"][0]["subPath"] =
            json!("sol-market-encoder/all-folds");
        assert!(bind_readback_mount(&changed, &request, Path::new(SEQUENCE_INPUT_MOUNT)).is_err());
        let mut writable = job.clone();
        writable["spec"]["template"]["spec"]["volumes"][0]["persistentVolumeClaim"]["readOnly"] =
            json!(false);
        assert!(bind_readback_mount(&writable, &request, Path::new(SEQUENCE_INPUT_MOUNT)).is_err());
        let mut changed = request.clone();
        changed.plan.independent_selection.data.targets_sha256 = "d".repeat(64);
        crate::mission_campaign::market_encoder::request_tests::rebind(&mut changed);
        assert_ne!(execution_binding(&changed, &controller).unwrap(), binding);
        let mut changed = request.clone();
        changed.plan.sealed.data.targets_sha256 = "d".repeat(64);
        crate::mission_campaign::market_encoder::request_tests::rebind(&mut changed);
        assert_ne!(execution_binding(&changed, &controller).unwrap(), binding);
    }
    #[test]
    fn stage_operator_cli_parses_without_render_registration_cycle() {
        use clap::Parser;
        for command in [
            "alpha-harness mission dispatch init-stage-authority --private-key /authority/stage.key --public-out /authority/stage.json --work-pvc-name task-data --work-pvc-uid task-uid",
            "alpha-harness mission dispatch stage-controller --submission /root/submission.json --control /root/control.json --pvc-root /research-data --private-key /root/stage.key --context monday-research-apne1 --namespace monday-research --prepare-only",
            "alpha-harness mission dispatch sign-root --grant /root/root.json --key-id operator --signing-key /root/operator.key --trusted-keys /root/keys.json --output /root/signed-root.json",
            "alpha-harness mission dispatch sign-study --grant /root/study.json --key-id operator --signing-key /root/operator.key --trusted-keys /root/keys.json --output /root/signed-study.json",
            "alpha-harness mission dispatch register-study --ledger /root/ledger.duckdb --signed-study /root/signed-study.json --trusted-keys /root/keys.json --study-approval-id original-approval --roots /root/roots.json --output /root/registered.json",
            "alpha-harness mission dispatch inspect-study --ledger /root/ledger.duckdb --study-id original-study --output /root/study-inspection.json",
        ] {assert!(crate::cli::Cli::try_parse_from(command.split_whitespace()).is_ok(),"{command}");}
        let root = tempfile::tempdir().unwrap();
        let request = crate::mission_campaign::market_encoder::request_tests::request();
        let path = root.path().join("submission.json");
        write_market_submission(&path, "describe-test", &request.image, request.clone()).unwrap();
        // No ledger, Root, approval, Kubernetes connection or raw data is needed here.
        describe(crate::cli::DescribeStudyArgs {
            submission: path,
            controller_image: format!("registry/controller@sha256:{}", "f".repeat(64)),
        })
        .unwrap();
    }
    #[test]
    fn market_two_fold_roots_share_one_cumulative_study_without_generation_drift() {
        use alpha_domain::campaign_study::*;
        let first = crate::mission_campaign::market_encoder::request_tests::request();
        let mut second = first.clone();
        let fold = &second.plan.folds[1];
        second.inputs.fold_id = fold.fold_id;
        second.inputs.sub_path = "sol-market-encoder/fold-2".into();
        second.inputs.train.features.sha256 = fold.train.features_sha256.clone();
        second.inputs.train.targets.sha256 = fold.train.targets_sha256.clone();
        let anchors = second.inputs.train.qualified_anchors.as_mut().unwrap();
        anchors.sha256 = fold.train.qualified_anchors_sha256.clone().unwrap();
        anchors.file = format!("train/{}.market-anchors.json", anchors.sha256);
        second.inputs.validation.features.sha256 = fold.validation.data.features_sha256.clone();
        second.inputs.validation.targets.sha256 = fold.validation.data.targets_sha256.clone();
        second.inputs.replay_manifest.sha256 = fold.validation.replay_manifest_sha256.clone();
        crate::mission_campaign::market_encoder::request_tests::rebind(&mut second);
        first.validate().unwrap();
        second.validate().unwrap();
        assert_eq!(
            first.plan.content_hash().unwrap(),
            second.plan.content_hash().unwrap()
        );
        assert_ne!(first.campaign_id, second.campaign_id);
        assert_ne!(first.family_id().unwrap(), second.family_id().unwrap());
        let mut long = first.clone();
        long.plan.study_id = "a".repeat(128);
        let original = long.family_id().unwrap();
        assert!(original.len() <= 128);
        long.plan.study_id.replace_range(127..128, "b");
        assert_ne!(original, long.family_id().unwrap());
        let controller = format!("registry/controller@sha256:{}", "f".repeat(64));
        let signing = ed25519_dalek::SigningKey::from_bytes(&[33; 32]);
        let keys = BTreeMap::from([("operator".into(), signing.verifying_key())]);
        for ceiling in [43, 44, 60] {
            let directory = tempfile::tempdir().unwrap();
            let mut store =
                alpha_store::AlphaStore::open(directory.path().join("ledger.duckdb")).unwrap();
            let now = Utc::now();
            let mut signed_roots = Vec::new();
            let mut verified_roots = Vec::new();
            let mut reservations = Vec::new();
            for request in [&first, &second] {
                let signed = sign_campaign_root_grant(
                    CampaignRootGrantV1 {
                        schema_version: ROOT_GRANT_SCHEMA.into(),
                        root_id: format!("root-fold-{}", request.inputs.fold_id),
                        family: CampaignFamilyPolicyV1 {
                            family_id: request.family_id().unwrap(),
                            definition_sha256: request.plan.content_hash().unwrap(),
                            max_trials: 22,
                        },
                        execution_scope: CampaignExecutionScope::PreHoldout,
                        execution: execution_binding(request, &controller).unwrap(),
                        allowed_policy_revision_ids: BTreeSet::from([request.policy_id().unwrap()]),
                        max_follow_ups: 0,
                        budget: CampaignRootBudgetV1 {
                            max_trials: 22,
                            max_job_attempts: 1,
                            max_job_seconds: 1200,
                            max_llm_tokens: 0,
                        },
                        valid_from: now - TimeDelta::minutes(1),
                        expires_at: now + TimeDelta::hours(2),
                    },
                    "operator".into(),
                    &signing,
                )
                .unwrap();
                let approval_id = format!("approval-fold-{}", request.inputs.fold_id);
                store.record_approval(&alpha_store::ApprovalRecord {approval_id:approval_id.clone(),approval_class:"campaign_root".into(),subject_id:signed.grant.root_id.clone(),payload:json!({"grant_sha256":signed.content_sha256,"family_id":signed.grant.family.family_id}),signer_id:Some("operator".into()),valid_from:Some(signed.grant.valid_from),expires_at:Some(signed.grant.expires_at),revoked_at:None,revoked_by:None,revocation_reason:None,created_at:now-TimeDelta::minutes(1)}).unwrap();
                let verified = verify_campaign_root_grant(&signed, &keys, now).unwrap();
                store
                    .register_campaign_root(&verified, &approval_id, now)
                    .unwrap();
                reservations.push(CampaignAttemptReservationV1 {
                    schema_version: ATTEMPT_SCHEMA.into(),
                    root_grant_sha256: signed.content_sha256.clone(),
                    family_id: request.family_id().unwrap(),
                    campaign_id: request.campaign_id.clone(),
                    execution: signed.grant.execution.clone(),
                    generation: 0,
                    parent_result_sha256: None,
                    policy_revision_id: request.policy_id().unwrap(),
                    request_sha256: sha256_text(&serde_json::to_string_pretty(request).unwrap()),
                    attempt_ordinal: 0,
                    declared_trials: request.declared_trials().unwrap(),
                    reserved_job_seconds: 1200,
                    reserved_llm_tokens: 0,
                });
                signed_roots.push(signed);
                verified_roots.push(verified);
            }
            let study = sign_campaign_study_grant(
                CampaignStudyGrantV1 {
                    schema_version: STUDY_GRANT_SCHEMA.into(),
                    study_id: if ceiling == 60 {
                        "separate-budget".into()
                    } else {
                        first.plan.study_id.clone()
                    },
                    members: signed_roots
                        .iter()
                        .map(|signed| CampaignStudyMemberV1 {
                            family_id: signed.grant.family.family_id.clone(),
                            root_grant_sha256: signed.content_sha256.clone(),
                            family_definition_sha256: signed.grant.family.definition_sha256.clone(),
                            family_max_trials: signed.grant.family.max_trials,
                            execution_scope: signed.grant.execution_scope.clone(),
                            execution: signed.grant.execution.clone(),
                            label_horizon_sha256: first
                                .label_horizon()
                                .unwrap()
                                .content_hash()
                                .unwrap(),
                        })
                        .collect(),
                    budget: CampaignStudyBudgetV1 {
                        max_trials: ceiling,
                        max_job_attempts: 2,
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
            let verified_study = verify_campaign_study_grant(&study, &keys, now).unwrap();
            store.record_approval(&alpha_store::ApprovalRecord {approval_id:"original-study-approval".into(),approval_class:"campaign_study".into(),subject_id:study.grant.study_id.clone(),payload:json!({"grant_sha256":study.content_sha256,"study_id":study.grant.study_id}),signer_id:Some("operator".into()),valid_from:Some(study.grant.valid_from),expires_at:Some(study.grant.expires_at),revoked_at:None,revoked_by:None,revocation_reason:None,created_at:now-TimeDelta::minutes(1)}).unwrap();
            store
                .register_campaign_study(&verified_study, "original-study-approval", now)
                .unwrap();
            for (request, signed) in [&first, &second].into_iter().zip(&signed_roots) {
                let admission = require_cumulative_study(&store, &request.into(), signed);
                if ceiling == 60 {
                    assert!(admission
                        .unwrap_err()
                        .to_string()
                        .contains("Study identity"));
                } else {
                    admission.unwrap();
                }
            }
            if ceiling == 60 {
                continue;
            }
            store
                .reserve_campaign_attempt(&verified_roots[0], &reservations[0], now)
                .unwrap();
            let second_result =
                store.reserve_campaign_attempt(&verified_roots[1], &reservations[1], now);
            if ceiling == 44 {
                second_result.unwrap();
            } else {
                assert!(second_result
                    .unwrap_err()
                    .to_string()
                    .contains("cumulative study budget"));
            }
            let usage = store
                .campaign_study_usage("sol-market-encoder-controlled-test")
                .unwrap();
            assert_eq!(
                usage.accounted_trials().unwrap(),
                if ceiling == 44 { 44 } else { 22 }
            );
            assert_eq!(usage.job_attempts, if ceiling == 44 { 2 } else { 1 });
        }
    }
}
