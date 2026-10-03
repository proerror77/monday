//! SOL sequence payload on the existing Campaign entrypoints.
pub(crate) mod cohort;
pub(crate) mod inputs;
pub(crate) mod readback;
pub(crate) mod worker;
use super::*;
use alpha_domain::sequence_study::{SolSequenceStudyV1, SOL_SEQUENCE_STUDY_SCHEMA};
use inputs::SequenceCampaignInputs;

pub(crate) const REQUEST_SCHEMA: &str = "monday.sol_sequence_campaign_request.v1";
pub(crate) const FREEZE_SCHEMA: &str = "monday.sol_sequence_campaign_freeze.v1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SequenceRequest {
    pub schema_version: String,
    pub campaign_id: String,
    pub build_source_revision: String,
    pub image: String,
    pub image_identity: String,
    pub plan: SolSequenceStudyV1,
    pub inputs: SequenceCampaignInputs,
    pub campaign_inputs_sha256: String,
    pub output_root: String,
    pub result_put_url: String,
    pub result_readback_url: String,
    pub bundle_put_url: String,
    pub bundle_readback_url: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SequenceFreeze {
    schema_version: String,
    canonical_request: SequenceRequest,
    signing_plan: CampaignSigningPlan,
}

impl SequenceRequest {
    pub(crate) fn expected_id(&self) -> anyhow::Result<String> {
        let digest = canonical_json_hash(&serde_json::json!({
            "schema":REQUEST_SCHEMA, "source":self.build_source_revision, "image":self.image,
            "plan":self.plan.content_hash().map_err(anyhow::Error::msg)?, "inputs":self.campaign_inputs_sha256,
            "root":self.output_root,
        }))?;
        Ok(format!("cex-campaign-{}", &digest[..32]))
    }

    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        self.plan.validate().map_err(anyhow::Error::msg)?;
        self.inputs.validate()?;
        let fold = self
            .plan
            .folds
            .iter()
            .find(|fold| {
                fold.fold_id == self.inputs.fold_id
                    && fold.training_window_days == self.inputs.training_window_days
            })
            .context("sequence Campaign has no registered fold")?;
        if self.schema_version != REQUEST_SCHEMA
            || self.campaign_id != self.expected_id()?
            || !valid_git_revision(&self.build_source_revision)
            || self.image.rsplit_once("@sha256:").map(|(_, digest)| digest)
                != Some(self.image_identity.as_str())
            || normalized_sha256("sequence image", &self.image_identity)? != self.image_identity
            || self.campaign_inputs_sha256 != canonical_json_hash(&self.inputs)?
            || self.inputs.train.manifest.sha256 != fold.train_dataset_sha256
            || self.inputs.validation.manifest.sha256 != fold.validation_dataset_sha256
            || self.inputs.replay_manifest.sha256 != fold.replay_manifest_sha256
        {
            bail!("sequence Campaign identity, dataset or image changed");
        }
        let root = canonical_tokyo_oss_internal_object("sequence output root", &self.output_root)?;
        if root != self.output_root || !root.contains("/research/campaigns/") || root.ends_with('/')
        {
            bail!("sequence output root must be a canonical Campaign object prefix");
        }
        for (url, suffix) in [
            (&self.result_put_url, "result.json"),
            (&self.result_readback_url, "result.json"),
            (&self.bundle_put_url, "results.zip"),
            (&self.bundle_readback_url, "results.zip"),
        ] {
            let expected = format!("{}/{}/{}", self.output_root, self.campaign_id, suffix);
            if canonical_tokyo_oss_internal_object("sequence result", url)? != expected {
                bail!("sequence output capability points outside its Campaign");
            }
        }
        Ok(())
    }

    pub(crate) fn policy_id(&self) -> anyhow::Result<String> {
        Ok(format!(
            "cex-search-policy-{}",
            self.plan.content_hash().map_err(anyhow::Error::msg)?
        ))
    }
}

pub(crate) fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> anyhow::Result<T> {
    let file = File::open(path)?;
    if file.metadata()?.len() > MAX_REQUEST_BYTES {
        bail!("sequence request exceeds byte limit");
    }
    let mut bytes = Vec::new();
    file.take(MAX_REQUEST_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_REQUEST_BYTES {
        bail!("sequence request grew beyond byte limit");
    }
    Ok(serde_json::from_slice(&bytes)?)
}

pub(crate) fn is_sequence_request(path: &Path) -> anyhow::Result<bool> {
    let value: serde_json::Value = read_json(path)?;
    Ok(value["schema_version"] == REQUEST_SCHEMA)
}

pub(crate) fn execute(args: CampaignExecuteArgs) -> anyhow::Result<()> {
    worker::execute(args)
}

pub(crate) fn is_sequence_plan(path: Option<&Path>) -> anyhow::Result<bool> {
    let Some(path) = path else {
        return Ok(false);
    };
    let value: serde_json::Value = read_json(path)?;
    Ok(value["schema_version"] == SOL_SEQUENCE_STUDY_SCHEMA)
}

pub(crate) fn is_sequence_freeze(path: &Path) -> anyhow::Result<bool> {
    let value: serde_json::Value = read_json(path)?;
    Ok(value["schema_version"] == FREEZE_SCHEMA)
}

pub(crate) fn finalize(args: CampaignFinalizeArgs) -> anyhow::Result<()> {
    let frozen: SequenceFreeze = read_json(&args.freeze)?;
    if frozen.schema_version != FREEZE_SCHEMA {
        bail!("invalid sequence freeze schema");
    }
    frozen.canonical_request.validate()?;
    let signed: SequenceRequest = read_json(&args.signed_request)?;
    signed.validate()?;
    let mut canonical = signed.clone();
    canonical.result_put_url =
        canonical_tokyo_oss_internal_object("sequence result PUT", &signed.result_put_url)?;
    canonical.result_readback_url =
        canonical_tokyo_oss_internal_object("sequence result GET", &signed.result_readback_url)?;
    canonical.bundle_put_url =
        canonical_tokyo_oss_internal_object("sequence bundle PUT", &signed.bundle_put_url)?;
    canonical.bundle_readback_url =
        canonical_tokyo_oss_internal_object("sequence bundle GET", &signed.bundle_readback_url)?;
    if canonical != frozen.canonical_request {
        bail!("signed sequence request changed its frozen semantics");
    }
    data_mission::write_json_atomic(&args.request_out, &signed)?;
    let rendered = crate::mission_dispatch::sequence_admission::write_submission(
        &args.submission_out,
        &args.attempt_id,
        &args.image,
        signed.clone(),
    )?;
    print_json(
        &serde_json::json!({"campaign_id":signed.campaign_id,"request_sha256":rendered.request_sha256,
        "submission_identity_sha256":rendered.submission_identity_sha256,"job_name":rendered.job_name,"request_out":args.request_out,
        "submission_out":args.submission_out,"execution_started":false}),
    )
}

pub(crate) fn freeze(args: CampaignFreezeArgs) -> anyhow::Result<()> {
    if args.reuse.is_some()
        || args.final_evaluation_control.is_some()
        || args.study_proposal.is_some()
    {
        bail!("sequence freeze does not accept legacy reuse, follow-up or final-evaluation modes");
    }
    let plan: SolSequenceStudyV1 = read_json(
        args.research_plan
            .as_deref()
            .context("sequence freeze requires its plan")?,
    )?;
    plan.validate().map_err(anyhow::Error::msg)?;
    if args.seeds != plan.neural_seeds || args.source_revision != BUILD_SOURCE_REVISION {
        bail!("sequence freeze seed set or exact source changed");
    }
    let inputs: SequenceCampaignInputs = read_json(&args.campaign_inputs)?;
    inputs.validate()?;
    inputs.verify_mount(&args.input_root)?;
    let fold = plan
        .folds
        .iter()
        .find(|fold| {
            fold.fold_id == inputs.fold_id
                && fold.training_window_days == inputs.training_window_days
        })
        .context("unregistered sequence input fold")?;
    for (location, view) in [
        (&inputs.train, fold.train),
        (&inputs.validation, fold.validation),
    ] {
        let dataset = inputs.verify_view(&args.input_root, location, view)?;
        let manifest_path = location.manifest.path(&args.input_root)?;
        let mut reader = hft_research_ml::sequence::SequenceReader::open(
            manifest_path
                .parent()
                .context("sequence manifest has no directory")?,
            dataset,
            &location.manifest.sha256,
            view,
        )
        .map_err(anyhow::Error::msg)?;
        reader.finish_pass().map_err(anyhow::Error::msg)?;
    }
    inputs.verify_replay(&args.input_root, fold.validation)?;
    let image_identity = args
        .image
        .rsplit_once("@sha256:")
        .context("sequence runner image is not pinned")?
        .1
        .to_string();
    let root = canonical_tokyo_oss_internal_object(
        "sequence Campaign root",
        args.campaign_root.trim_end_matches('/'),
    )?;
    let mut request = SequenceRequest {
        schema_version: REQUEST_SCHEMA.into(),
        campaign_id: String::new(),
        build_source_revision: args.source_revision,
        image: args.image,
        image_identity,
        plan,
        campaign_inputs_sha256: canonical_json_hash(&inputs)?,
        inputs,
        output_root: root,
        result_put_url: String::new(),
        result_readback_url: String::new(),
        bundle_put_url: String::new(),
        bundle_readback_url: String::new(),
    };
    request.campaign_id = request.expected_id()?;
    request.result_put_url = format!(
        "{}/{}/result.json",
        request.output_root, request.campaign_id
    );
    request.result_readback_url = request.result_put_url.clone();
    request.bundle_put_url = format!(
        "{}/{}/results.zip",
        request.output_root, request.campaign_id
    );
    request.bundle_readback_url = request.bundle_put_url.clone();
    request.validate()?;
    let actions = [
        (&request.result_put_url, "PUT", "result_put"),
        (&request.result_readback_url, "GET", "result_get"),
        (&request.bundle_put_url, "PUT", "bundle_put"),
        (&request.bundle_readback_url, "GET", "bundle_get"),
    ]
    .into_iter()
    .map(|(url, method, name)| CampaignSigningAction {
        name: name.into(),
        object: url.clone(),
        method: method.into(),
        content_type: (method == "PUT").then(|| {
            if name == "bundle_put" {
                "application/zip".into()
            } else {
                "application/json".into()
            }
        }),
        required_headers: if method == "PUT" {
            std::collections::BTreeMap::from([("x-oss-forbid-overwrite".into(), "true".into())])
        } else {
            Default::default()
        },
    })
    .collect();
    let frozen = SequenceFreeze {
        schema_version: FREEZE_SCHEMA.into(),
        canonical_request: request,
        signing_plan: CampaignSigningPlan { actions },
    };
    data_mission::write_json_atomic(&args.output, &frozen)?;
    print_json(
        &serde_json::json!({"schema_version":FREEZE_SCHEMA,"campaign_id":frozen.canonical_request.campaign_id,
        "policy_id":frozen.canonical_request.policy_id()?,"declared_primary_fits":7,"declared_verification_fits":7,
        "output":args.output,"execution_started":false}),
    )
}

#[cfg(test)]
pub(crate) mod tests;
