//! Two-stage SOL market encoder payload on the canonical Campaign entrypoints.
pub(crate) mod cohort;
pub(crate) mod inputs;
pub(crate) mod readback;
pub(crate) mod stage_permit;
pub(crate) mod worker;
use super::*;
use alpha_domain::market_encoder_study::{MarketEncoderStudyV1, MARKET_ENCODER_STUDY_SCHEMA};
use inputs::MarketCampaignInputs;

pub(crate) const REQUEST_SCHEMA: &str = "monday.sol_market_encoder_campaign_request.v1";
pub(crate) const FREEZE_SCHEMA: &str = "monday.sol_market_encoder_campaign_freeze.v1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MarketRequest {
    pub schema_version: String,
    pub campaign_id: String,
    pub build_source_revision: String,
    pub image: String,
    pub image_identity: String,
    pub plan: MarketEncoderStudyV1,
    pub stage_authority: alpha_domain::campaign_stage::CampaignStageAuthorityV1,
    pub inputs: MarketCampaignInputs,
    pub campaign_inputs_sha256: String,
    pub output_root: String,
    pub result_put_url: String,
    pub result_readback_url: String,
    pub bundle_put_url: String,
    pub bundle_readback_url: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MarketFreeze {
    schema_version: String,
    canonical_request: MarketRequest,
    signing_plan: CampaignSigningPlan,
}

impl MarketRequest {
    pub(crate) fn expected_id(&self) -> anyhow::Result<String> {
        let digest = canonical_json_hash(&serde_json::json!({
            "schema":REQUEST_SCHEMA, "source":self.build_source_revision, "image":self.image,
            "plan":self.plan.content_hash().map_err(anyhow::Error::msg)?, "inputs":self.campaign_inputs_sha256,
            "root":self.output_root,"stage_authority":self.stage_authority,
        }))?;
        Ok(format!("cex-campaign-{}", &digest[..32]))
    }

    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        self.plan.validate().map_err(anyhow::Error::msg)?;
        self.stage_authority
            .validate()
            .map_err(anyhow::Error::msg)?;
        self.inputs.validate()?;
        if self.stage_authority.work_pvc_name == self.inputs.pvc_name
            && self.stage_authority.work_pvc_uid != self.inputs.pvc_uid
        {
            bail!("market input/work references disagree on the same PVC UID");
        }
        let fold = self
            .plan
            .folds
            .iter()
            .find(|fold| fold.fold_id == self.inputs.fold_id)
            .context("market encoder Campaign has no registered fold")?;
        if self.schema_version != REQUEST_SCHEMA
            || self.campaign_id != self.expected_id()?
            || !valid_git_revision(&self.build_source_revision)
            || self
                .image
                .rsplit_once("@sha256:")
                .is_none_or(|(name, digest)| name.is_empty() || digest != self.image_identity)
            || normalized_sha256("market encoder image", &self.image_identity)?
                != self.image_identity
            || self.campaign_inputs_sha256 != canonical_json_hash(&self.inputs)?
            || self.inputs.train.features.sha256 != fold.train.features_sha256
            || self.inputs.train.targets.sha256 != fold.train.targets_sha256
            || self
                .inputs
                .train
                .qualified_anchors
                .as_ref()
                .map(|a| &a.sha256)
                != fold.train.qualified_anchors_sha256.as_ref()
            || self.inputs.validation.features.sha256 != fold.validation.data.features_sha256
            || self.inputs.validation.targets.sha256 != fold.validation.data.targets_sha256
            || self.inputs.validation.qualified_anchors.is_some()
            || self.inputs.replay_manifest.sha256 != fold.validation.replay_manifest_sha256
        {
            bail!("market encoder Campaign identity, dataset or image changed");
        }
        let root =
            canonical_tokyo_oss_internal_object("market encoder output root", &self.output_root)?;
        if root != self.output_root || !root.contains("/research/campaigns/") || root.ends_with('/')
        {
            bail!("market encoder output root must be a canonical Campaign object prefix");
        }
        for (url, suffix) in [
            (&self.result_put_url, "result.json"),
            (&self.result_readback_url, "result.json"),
            (&self.bundle_put_url, "results.zip"),
            (&self.bundle_readback_url, "results.zip"),
        ] {
            let expected = format!("{}/{}/{}", self.output_root, self.campaign_id, suffix);
            if canonical_tokyo_oss_internal_object("market encoder result", url)? != expected {
                bail!("market encoder output capability points outside its Campaign");
            }
        }
        Ok(())
    }

    /// One-second observations, 30-second forward-mid target, and the
    /// protocol's 30-second separation at each training/evaluation boundary.
    pub(crate) fn label_horizon(
        &self,
    ) -> anyhow::Result<alpha_domain::campaign_horizon::CampaignLabelHorizonV1> {
        self.plan.validate().map_err(anyhow::Error::msg)?;
        alpha_domain::campaign_horizon::CampaignLabelHorizonV1::new(30, 1_000, 30, 30)
            .map_err(anyhow::Error::msg)
    }

    /// Reserve the registered primary and verification stages for this fold.
    pub(crate) fn declared_trials(&self) -> anyhow::Result<u64> {
        let stages = self.plan.development_stages().map_err(anyhow::Error::msg)?;
        let count = stages
            .iter()
            .filter(|stage| stage.key.fold_id == self.inputs.fold_id)
            .count();
        if count == 0 {
            bail!("market encoder request has no development stages");
        }
        u64::try_from(count).context("market encoder trial count overflow")
    }

    /// A native Root generation admits one immutable Campaign request. Each
    /// development fold therefore owns its own family; the signed Study retains
    /// their shared original budget. The full protocol hash avoids name truncation.
    pub(crate) fn family_id(&self) -> anyhow::Result<String> {
        Ok(format!(
            "sol-market-fold-{}-{}",
            self.inputs.fold_id,
            self.plan.content_hash().map_err(anyhow::Error::msg)?
        ))
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
        bail!("market encoder request exceeds byte limit");
    }
    let mut bytes = Vec::new();
    file.take(MAX_REQUEST_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_REQUEST_BYTES {
        bail!("market encoder request grew beyond byte limit");
    }
    Ok(serde_json::from_slice(&bytes)?)
}

#[cfg(any(feature = "scientific", test))]
pub(crate) fn is_market_encoder_request(path: &Path) -> anyhow::Result<bool> {
    let value: serde_json::Value = read_json(path)?;
    Ok(value["schema_version"] == REQUEST_SCHEMA)
}

#[cfg(feature = "scientific")]
pub(crate) fn execute(args: CampaignExecuteArgs) -> anyhow::Result<()> {
    worker::execute(args)
}

pub(crate) fn is_market_encoder_plan(path: Option<&Path>) -> anyhow::Result<bool> {
    let Some(path) = path else {
        return Ok(false);
    };
    let value: serde_json::Value = read_json(path)?;
    Ok(value["schema_version"] == MARKET_ENCODER_STUDY_SCHEMA)
}

pub(crate) fn is_market_encoder_freeze(path: &Path) -> anyhow::Result<bool> {
    let value: serde_json::Value = read_json(path)?;
    Ok(value["schema_version"] == FREEZE_SCHEMA)
}

pub(crate) fn finalize(args: CampaignFinalizeArgs) -> anyhow::Result<()> {
    let frozen: MarketFreeze = read_json(&args.freeze)?;
    if frozen.schema_version != FREEZE_SCHEMA {
        bail!("invalid market encoder freeze schema");
    }
    frozen.canonical_request.validate()?;
    let signed: MarketRequest = read_json(&args.signed_request)?;
    signed.validate()?;
    let mut canonical = signed.clone();
    canonical.result_put_url =
        canonical_tokyo_oss_internal_object("market encoder result PUT", &signed.result_put_url)?;
    canonical.result_readback_url = canonical_tokyo_oss_internal_object(
        "market encoder result GET",
        &signed.result_readback_url,
    )?;
    canonical.bundle_put_url =
        canonical_tokyo_oss_internal_object("market encoder bundle PUT", &signed.bundle_put_url)?;
    canonical.bundle_readback_url = canonical_tokyo_oss_internal_object(
        "market encoder bundle GET",
        &signed.bundle_readback_url,
    )?;
    if canonical != frozen.canonical_request {
        bail!("signed market encoder request changed its frozen semantics");
    }
    hft_research_artifacts::write_json_atomic(&args.request_out, &signed)?;
    let rendered = crate::mission_dispatch::sequence_admission::write_market_submission(
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
        || args.reuse_sha256.is_some()
        || args.final_evaluation_control.is_some()
        || args.study_proposal.is_some()
    {
        bail!("market encoder freeze does not accept legacy reuse, follow-up or final-evaluation modes");
    }
    let plan: MarketEncoderStudyV1 = read_json(
        args.research_plan
            .as_deref()
            .context("market encoder freeze requires its plan")?,
    )?;
    plan.validate().map_err(anyhow::Error::msg)?;
    if args.seeds != plan.seeds || args.source_revision != BUILD_SOURCE_REVISION {
        bail!("market encoder freeze seed set or exact source changed");
    }
    let inputs: MarketCampaignInputs = read_json(&args.campaign_inputs)?;
    inputs.validate()?;
    inputs.verify_mount(&args.input_root)?;
    let fold = plan
        .folds
        .iter()
        .find(|fold| fold.fold_id == inputs.fold_id)
        .context("unregistered market encoder input fold")?;
    // verify_view performs independent byte, row, source and anchor verification.
    inputs.verify_view(&args.input_root, &inputs.train, &fold.train)?;
    inputs.verify_view(&args.input_root, &inputs.validation, &fold.validation.data)?;
    inputs.verify_replay(&args.input_root, fold.validation.data.view)?;
    let image_identity = args
        .image
        .rsplit_once("@sha256:")
        .context("market encoder runner image is not pinned")?
        .1
        .to_string();
    let root = canonical_tokyo_oss_internal_object(
        "market encoder Campaign root",
        args.campaign_root.trim_end_matches('/'),
    )?;
    let mut request = MarketRequest {
        schema_version: REQUEST_SCHEMA.into(),
        campaign_id: String::new(),
        build_source_revision: args.source_revision,
        image: args.image,
        image_identity,
        plan,
        stage_authority: read_json(
            args.stage_authority
                .as_deref()
                .context("market freeze requires --stage-authority")?,
        )?,
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
    let frozen = MarketFreeze {
        schema_version: FREEZE_SCHEMA.into(),
        canonical_request: request,
        signing_plan: CampaignSigningPlan { actions },
    };
    hft_research_artifacts::write_json_atomic(&args.output, &frozen)?;
    print_json(
        &serde_json::json!({"schema_version":FREEZE_SCHEMA,"campaign_id":frozen.canonical_request.campaign_id,
        "policy_id":frozen.canonical_request.policy_id()?,"declared_primary_fits":frozen.canonical_request.plan.development_stages().map_err(anyhow::Error::msg)?.iter()
            .filter(|stage| stage.key.fold_id == frozen.canonical_request.inputs.fold_id
                && stage.key.purpose == alpha_domain::market_encoder_study::MarketTrainingStagePurposeV1::Primary).count(),
        "declared_verification_fits":frozen.canonical_request.plan.development_stages().map_err(anyhow::Error::msg)?.iter()
            .filter(|stage| stage.key.fold_id == frozen.canonical_request.inputs.fold_id
                && stage.key.purpose == alpha_domain::market_encoder_study::MarketTrainingStagePurposeV1::Verification).count(),
        "declared_total_trials":frozen.canonical_request.declared_trials()?,
        "output":args.output,"execution_started":false}),
    )
}

#[cfg(all(test, feature = "scientific"))]
pub(crate) mod tests;

#[cfg(test)]
pub(crate) mod request_tests {
    use super::*;
    use crate::mission_campaign::sequence::inputs::Artifact;
    use alpha_domain::{market_encoder_study::*, EvaluationCostsV1};
    use hft_research_manifest::sequence::{SequenceInputSpecV1, SequenceViewV1};
    use inputs::{MarketDatasetLocation, INPUTS_SCHEMA};
    const DAY_MS: i64 = 86_400_000;
    fn hash(id: u8) -> String {
        format!("{id:064x}")
    }
    fn evaluation(day: i64, id: u8) -> MarketEvaluationViewV1 {
        let start = day * DAY_MS;
        MarketEvaluationViewV1 {
            data: MarketDataViewV1 {
                features_sha256: hash(id),
                targets_sha256: hash(id + 20),
                qualified_anchors_sha256: None,
                view: SequenceViewV1 {
                    history_start_ms: start,
                    decision_start_ms: start + 59000,
                    end_ms: start + 59000 + DAY_MS + 30000,
                    decision_stride_ms: 1000,
                },
            },
            replay_manifest_sha256: hash(id + 40),
        }
    }
    fn plan() -> MarketEncoderStudyV1 {
        MarketEncoderStudyV1 {
            schema_version: MARKET_ENCODER_STUDY_SCHEMA.into(),
            study_id: "sol-market-encoder-controlled-test".into(),
            input: SequenceInputSpecV1::sol_lob(),
            seeds: vec![7, 11],
            folds: [(1, 15, 1, 3), (2, 18, 2, 4)]
                .into_iter()
                .map(|(fold_id, day, id, val)| {
                    let end = day * DAY_MS;
                    MarketEncoderFoldV1 {
                        fold_id,
                        train: MarketDataViewV1 {
                            features_sha256: hash(id),
                            targets_sha256: hash(id + 20),
                            qualified_anchors_sha256: Some(hash(id + 60)),
                            view: SequenceViewV1 {
                                history_start_ms: end - 14 * DAY_MS,
                                decision_start_ms: end - 14 * DAY_MS + 59000,
                                end_ms: end,
                                decision_stride_ms: 60000,
                            },
                        },
                        validation: evaluation(day + 1, val),
                    }
                })
                .collect(),
            independent_selection: evaluation(22, 5),
            sealed: evaluation(25, 6),
            training: MarketEncoderTrainingV1 {
                hidden_channels: 16,
                batch_size: 64,
                pretraining_updates: 1024,
                task_updates: 512,
                compute_control_updates: 1536,
                learning_rate: 0.001,
                max_training_examples: 32768,
            },
            max_primary_fits: 30,
            max_verification_fits: 30,
            max_cost_fen: 10000,
            costs: EvaluationCostsV1 {
                fee_bps: 2.0,
                rebate_bps: 0.0,
                funding_bps: 0.0,
                latency_bps: 0.5,
                slippage_bps: 0.0,
                cross_spread: true,
                position_notional_usd: 100.0,
                capacity_depth_levels: 5,
                max_book_depth_fraction: 0.05,
            },
        }
    }

    pub(crate) fn request() -> MarketRequest {
        let plan = plan();
        let fold = &plan.folds[0];
        let artifact = |file: &str, hash: &str| Artifact {
            file: file.into(),
            sha256: hash.into(),
        };
        let location = |prefix: &str, data: &MarketDataViewV1| MarketDatasetLocation {
            features: artifact(&format!("{prefix}/features.json"), &data.features_sha256),
            targets: artifact(&format!("{prefix}/targets.json"), &data.targets_sha256),
            sources: artifact(&format!("{prefix}/sources.json"), &hash(90)),
            qualified_anchors: data
                .qualified_anchors_sha256
                .as_ref()
                .map(|h| artifact(&format!("{prefix}/{h}.market-anchors.json"), h)),
        };
        let image = format!("registry/runner@sha256:{}", "e".repeat(64));
        let inputs = MarketCampaignInputs {
            schema_version: INPUTS_SCHEMA.into(),
            producer_source_revision: BUILD_SOURCE_REVISION.into(),
            producer_image: image.clone(),
            pvc_name: "sol-inputs".into(),
            pvc_uid: "pvc-uid".into(),
            sub_path: "sol-market-encoder/fold-1".into(),
            fold_id: 1,
            train: location("train", &fold.train),
            validation: location("validation", &fold.validation.data),
            replay_artifact: artifact("replay.parquet", &hash(91)),
            replay_manifest: artifact("replay.json", &fold.validation.replay_manifest_sha256),
        };
        let mut request = MarketRequest {
            schema_version: REQUEST_SCHEMA.into(), campaign_id: String::new(),
            build_source_revision: BUILD_SOURCE_REVISION.into(), image,
            image_identity: "e".repeat(64), plan,
            stage_authority: alpha_domain::campaign_stage::CampaignStageAuthorityV1 {
                schema_version: alpha_domain::campaign_stage::AUTHORITY_SCHEMA.into(),
                public_key_hex: hex::encode(ed25519_dalek::SigningKey::from_bytes(&[73;32]).verifying_key().as_bytes()),
                work_pvc_name: "market-work".into(), work_pvc_uid: "market-work-uid".into(),
            },
            campaign_inputs_sha256: canonical_json_hash(&inputs).unwrap(), inputs,
            output_root: "https://monday-lob-apne1-1045353359.oss-ap-northeast-1-internal.aliyuncs.com/research/campaigns/sol-market-test".into(),
            result_put_url: String::new(), result_readback_url: String::new(),
            bundle_put_url: String::new(), bundle_readback_url: String::new(),
        };
        rebind(&mut request);
        request
    }
    pub(crate) fn rebind(request: &mut MarketRequest) {
        request.campaign_inputs_sha256 = canonical_json_hash(&request.inputs).unwrap();
        request.campaign_id = request.expected_id().unwrap();
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
    }
    #[test]
    fn market_request_binds_fold_features_targets_anchors_image_and_capabilities() {
        let original = request();
        original.validate().unwrap();
        assert_eq!(original.declared_trials().unwrap(), 22);
        let mut signed = original.clone();
        signed.result_put_url.push_str("?signature=fixture");
        signed.validate().unwrap();
        assert_eq!(
            signed.expected_id().unwrap(),
            original.expected_id().unwrap()
        );
        for changed in [
            {
                let mut r = original.clone();
                r.inputs.pvc_uid = "replacement".into();
                r
            },
            {
                let mut r = original.clone();
                r.inputs.train.targets.sha256 = hash(92);
                rebind(&mut r);
                r
            },
            {
                let mut r = original.clone();
                r.inputs.train.qualified_anchors = None;
                rebind(&mut r);
                r
            },
            {
                let mut r = original.clone();
                r.inputs.validation.qualified_anchors = r.inputs.train.qualified_anchors.clone();
                rebind(&mut r);
                r
            },
            {
                let mut r = original.clone();
                r.image_identity = hash(93);
                r
            },
            {
                let mut r = original.clone();
                r.bundle_readback_url = r.result_readback_url.clone();
                r
            },
        ] {
            assert!(changed.validate().is_err());
        }
        let mut changed = original.clone();
        changed.plan.training.compute_control_updates += 1;
        assert_ne!(
            changed.expected_id().unwrap(),
            original.expected_id().unwrap()
        );
        assert!(changed.validate().is_err());
    }
    #[test]
    fn market_request_routing_recognizes_only_registered_schemas() {
        let root = tempfile::tempdir().unwrap();
        let request = request();
        let path = root.path().join("request.json");
        hft_research_artifacts::write_json_atomic(&path, &request).unwrap();
        assert!(is_market_encoder_request(&path).unwrap());
        assert!(!sequence::is_sequence_request(&path).unwrap());
        hft_research_artifacts::write_json_atomic(&path, &request.plan).unwrap();
        assert!(is_market_encoder_plan(Some(&path)).unwrap());
        assert!(!sequence::is_sequence_plan(Some(&path)).unwrap());
        assert!(!is_market_encoder_plan(None).unwrap());
    }
    #[test]
    fn market_request_finalize_preserves_frozen_semantics_and_native_bytes() {
        let root = tempfile::tempdir().unwrap();
        let original = request();
        let frozen = MarketFreeze {
            schema_version: FREEZE_SCHEMA.into(),
            canonical_request: original.clone(),
            signing_plan: CampaignSigningPlan {
                actions: Vec::new(),
            },
        };
        let args = CampaignFinalizeArgs {
            freeze: root.path().join("freeze.json"),
            signed_request: root.path().join("signed.json"),
            attempt_id: "market-finalize-test".into(),
            image: original.image.clone(),
            request_out: root.path().join("request.json"),
            submission_out: root.path().join("submission.json"),
        };
        hft_research_artifacts::write_json_atomic(&args.freeze, &frozen).unwrap();
        assert!(is_market_encoder_freeze(&args.freeze).unwrap());
        let mut signed = original.clone();
        for url in [
            &mut signed.result_put_url,
            &mut signed.result_readback_url,
            &mut signed.bundle_put_url,
            &mut signed.bundle_readback_url,
        ] {
            url.push_str("?signature=controlled-fixture");
        }
        hft_research_artifacts::write_json_atomic(&args.signed_request, &signed).unwrap();
        crate::mission_campaign::finalize(args.clone()).unwrap();
        let emitted: MarketRequest = read_json(&args.request_out).unwrap();
        assert_eq!(emitted, signed);
        let submission: serde_json::Value = read_json(&args.submission_out).unwrap();
        assert_eq!(submission["purpose"], "market_encoder_study");
        assert_eq!(
            submission["request"],
            serde_json::to_value(&signed).unwrap()
        );
        assert_eq!(
            std::fs::read_to_string(&args.request_out).unwrap(),
            serde_json::to_string_pretty(&signed).unwrap()
        );
        let mut changed = signed;
        changed.plan.costs.fee_bps += 1.0;
        rebind(&mut changed);
        hft_research_artifacts::write_json_atomic(&args.signed_request, &changed).unwrap();
        assert!(crate::mission_campaign::finalize(args.clone()).is_err());
        assert_eq!(
            read_json::<MarketRequest>(&args.request_out).unwrap(),
            emitted
        );
    }
}
