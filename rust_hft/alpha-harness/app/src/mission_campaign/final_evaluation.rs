//! The final phase of the canonical Campaign seam; never a search/root-grant mode.
use super::*;
use alpha_domain::campaign_finalization::SignedCampaignFinalEvaluationGrantV1;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const REQUEST_SCHEMA: &str = "monday.campaign_final_request.v1";
const FREEZE_SCHEMA: &str = "monday.campaign_final_freeze.v1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FinalRequest {
    pub schema_version: String,
    pub campaign_id: String,
    pub output_root: String,
    pub build_source_revision: String,
    pub image_identity: String,
    pub grant: SignedCampaignFinalEvaluationGrantV1,
    /// Original request bytes remain identity evidence; fresh read capabilities
    /// are a separate map and cannot rewrite the settled request identities.
    pub sources: BTreeMap<String, CampaignRequest>,
    pub read_urls: BTreeMap<String, String>,
    pub result_put_url: String,
    pub result_readback_url: String,
    pub bundle_put_url: String,
    pub bundle_readback_url: String,
    pub holdout_claim_put_url: String,
    pub holdout_claim_readback_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FinalFreeze {
    schema_version: String,
    canonical_request: FinalRequest,
    signing_plan: CampaignSigningPlan,
}

impl FinalRequest {
    pub(crate) fn new(
        grant: SignedCampaignFinalEvaluationGrantV1,
        sources: BTreeMap<String, CampaignRequest>,
        output_root: String,
    ) -> anyhow::Result<Self> {
        let first = sources
            .values()
            .next()
            .context("final request has no sources")?;
        let holdout = canonical_final_object("global holdout", &first.holdout_claim_put_url)?;
        let image_identity = mission_dispatch::image_digest(&grant.grant.execution.runner_image)?;
        let mut request = Self {
            schema_version: REQUEST_SCHEMA.into(),
            campaign_id: String::new(),
            output_root,
            build_source_revision: grant.grant.execution.source_revision.clone(),
            image_identity,
            grant,
            sources,
            read_urls: BTreeMap::new(),
            result_put_url: String::new(),
            result_readback_url: String::new(),
            bundle_put_url: String::new(),
            bundle_readback_url: String::new(),
            holdout_claim_put_url: holdout.clone(),
            holdout_claim_readback_url: holdout,
        };
        request.campaign_id = request.expected_id()?;
        request.read_urls = request
            .read_objects()?
            .into_iter()
            .map(|url| (url.clone(), url))
            .collect();
        let root = format!("{}/{}", request.output_root, request.campaign_id);
        request.result_put_url = format!("{root}/final-result.json");
        request.result_readback_url = request.result_put_url.clone();
        request.bundle_put_url = format!("{root}/final-result.zip");
        request.bundle_readback_url = request.bundle_put_url.clone();
        request.validate()?;
        Ok(request)
    }
    pub(crate) fn first_source(&self) -> anyhow::Result<&CampaignRequest> {
        self.sources
            .values()
            .next()
            .context("final evaluation has no settled sources")
    }

    pub(crate) fn expected_id(&self) -> anyhow::Result<String> {
        let sources = self
            .sources
            .iter()
            .map(|(operation, request)| {
                Ok((
                    operation,
                    hex::encode(Sha256::digest(serialize_request(request)?)),
                ))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let hash = canonical_json_hash(&(
            &self.schema_version,
            &self.grant.content_sha256,
            sources,
            &self.output_root,
        ))?;
        Ok(format!("cex-final-{}", &hash[..32]))
    }

    fn read_objects(&self) -> anyhow::Result<BTreeSet<String>> {
        let mut objects = BTreeSet::new();
        for source in self.sources.values() {
            for url in [
                &source.feature_url,
                &source.materialization_url,
                &source.replay_artifact_url,
                &source.replay_manifest_url,
                &source.campaign_result_readback_url,
            ] {
                objects.insert(canonical_final_object("final source", url)?);
            }
            for round in &source.rounds {
                for url in [&round.mission_readback_url, &round.result_readback_url] {
                    objects.insert(canonical_final_object("final source round", url)?);
                }
            }
        }
        Ok(objects)
    }

    pub(crate) fn read_url(&self, original: &str) -> anyhow::Result<&str> {
        let object = canonical_final_object("final source read", original)?;
        self.read_urls
            .get(&object)
            .map(String::as_str)
            .context("final request lacks source read capability")
    }

    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        self.grant.grant.validate()?;
        let grant = &self.grant.grant;
        let first = self.first_source()?;
        if self.schema_version != REQUEST_SCHEMA
            || self.grant.content_sha256 != self.grant.grant.content_hash()?
            || self.campaign_id != self.expected_id()?
            || self.sources.keys().collect::<Vec<_>>()
                != grant.selected_results.keys().collect::<Vec<_>>()
            || self.build_source_revision != grant.execution.source_revision
            || self.image_identity != mission_dispatch::image_digest(&grant.execution.runner_image)?
            || self.output_root != canonical_final_object("final output root", &self.output_root)?
            || self.read_urls.keys().cloned().collect::<BTreeSet<_>>() != self.read_objects()?
        {
            bail!("final Campaign identity, source set or execution binding changed");
        }
        for (object, access) in &self.read_urls {
            if *object != canonical_final_object("final source read", access)? {
                bail!("final read capability changed its object");
            }
        }
        for source in self.sources.values() {
            validate_request_for_execute(source)?;
            if source.campaign_inputs_sha256 != grant.execution.campaign_inputs_sha256
                || source.feature_sha256 != first.feature_sha256
                || source.materialization_sha256 != first.materialization_sha256
                || source.replay_artifact_sha256 != first.replay_artifact_sha256
                || source.replay_manifest_sha256 != first.replay_manifest_sha256
                || source.holdout_id != first.holdout_id
                || source.build_source_revision != self.build_source_revision
                || source.image_identity != self.image_identity
            {
                bail!("final source data, holdout cohort or code differs from the closed family");
            }
        }
        let root = format!("{}/{}", self.output_root, self.campaign_id);
        for (url, expected) in [
            (&self.result_put_url, format!("{root}/final-result.json")),
            (
                &self.result_readback_url,
                format!("{root}/final-result.json"),
            ),
            (&self.bundle_put_url, format!("{root}/final-result.zip")),
            (
                &self.bundle_readback_url,
                format!("{root}/final-result.zip"),
            ),
            (
                &self.holdout_claim_put_url,
                canonical_final_object("global holdout", &first.holdout_claim_put_url)?,
            ),
            (
                &self.holdout_claim_readback_url,
                canonical_final_object("global holdout", &first.holdout_claim_readback_url)?,
            ),
        ] {
            if canonical_final_object("final output", url)? != expected {
                bail!("final output changed its canonical object");
            }
        }
        if serde_json::to_vec(self)?.len() as u64 > MAX_REQUEST_BYTES {
            bail!("final request exceeds byte budget");
        }
        Ok(())
    }

    fn canonical(&self) -> anyhow::Result<Self> {
        let mut request = self.clone();
        for access in request.read_urls.values_mut() {
            *access = canonical_final_object("final read", access)?;
        }
        for access in [
            &mut request.result_put_url,
            &mut request.result_readback_url,
            &mut request.bundle_put_url,
            &mut request.bundle_readback_url,
            &mut request.holdout_claim_put_url,
            &mut request.holdout_claim_readback_url,
        ] {
            *access = canonical_final_object("final output", access)?;
        }
        Ok(request)
    }

    fn signing_plan(&self) -> anyhow::Result<CampaignSigningPlan> {
        let mut actions = self
            .read_urls
            .keys()
            .enumerate()
            .map(|(index, object)| {
                signing_action_get(&format!("source_read_{index}"), object.clone())
            })
            .collect::<Vec<_>>();
        actions.extend([
            signing_action_put_json("final_result_put", self.result_put_url.clone()),
            signing_action_get("final_result_readback", self.result_readback_url.clone()),
            signing_action_put_zip("final_bundle_put", self.bundle_put_url.clone()),
            signing_action_get("final_bundle_readback", self.bundle_readback_url.clone()),
            signing_action_put_json("holdout_claim_put", self.holdout_claim_put_url.clone()),
            signing_action_get(
                "holdout_claim_readback",
                self.holdout_claim_readback_url.clone(),
            ),
        ]);
        Ok(CampaignSigningPlan { actions })
    }
}

pub(crate) fn freeze(args: CampaignFreezeArgs) -> anyhow::Result<()> {
    if !args.seeds.is_empty() || args.research_plan.is_some() {
        bail!("final evaluation cannot add seeds or a research plan");
    }
    let path = args
        .final_evaluation_control
        .as_deref()
        .context("missing final evaluation control")?;
    let (control, grant) = mission_dispatch::final_admission::read_active_control(path)?;
    let sources = mission_dispatch::final_admission::verified_source_requests(&control, &grant)?;
    let input = validated_campaign_inputs(&args)?;
    let first = sources
        .values()
        .next()
        .context("closed family has no selected results")?;
    if input.campaign_inputs_sha256 != grant.grant().execution.campaign_inputs_sha256
        || input.feature_sha256 != first.feature_sha256
        || input.materialization_sha256 != first.materialization_sha256
        || input.replay_artifact_sha256 != first.replay_artifact_sha256
        || input.replay_manifest_sha256 != first.replay_manifest_sha256
        || input.build_source_revision != grant.grant().execution.source_revision
        || input.image_identity
            != mission_dispatch::image_digest(&grant.grant().execution.runner_image)?
    {
        bail!("final freeze input identities differ from the closed family");
    }
    let request = FinalRequest::new(grant.signed_grant().clone(), sources, input.campaign_root)?;
    let plan = FinalFreeze {
        schema_version: FREEZE_SCHEMA.into(),
        signing_plan: request.signing_plan()?,
        canonical_request: request,
    };
    data_mission::write_json_atomic(&args.output, &plan)?;
    print_json(
        &serde_json::json!({"phase":"final_evaluation", "campaign_id":plan.canonical_request.campaign_id,
        "family_id":grant.grant().family_id, "sources":plan.canonical_request.sources.len(), "max_candidates":grant.grant().max_candidates,
        "max_job_seconds":grant.grant().max_job_seconds, "evaluation_started":false, "output":args.output}),
    )
}

pub(crate) fn finalize(args: CampaignFinalizeArgs) -> anyhow::Result<()> {
    let plan: FinalFreeze = mission_dispatch::final_admission::read_bounded_json(&args.freeze)?;
    plan.canonical_request.validate()?;
    if plan.schema_version != FREEZE_SCHEMA
        || plan.signing_plan != plan.canonical_request.signing_plan()?
    {
        bail!("final freeze signing plan changed");
    }
    let request: FinalRequest =
        mission_dispatch::final_admission::read_bounded_json(&args.signed_request)?;
    request.validate()?;
    if request.canonical()? != plan.canonical_request
        || args.image != request.grant.grant.execution.runner_image
    {
        bail!("finalized request differs from its frozen inputs");
    }
    data_mission::write_json_atomic(&args.request_out, &request)?;
    let report = mission_dispatch::final_admission::write_submission(
        &args.submission_out,
        &args.attempt_id,
        &args.image,
        request,
    )?;
    print_json(&report)
}

pub(crate) fn is_final_freeze(path: &Path) -> anyhow::Result<bool> {
    let value: serde_json::Value = mission_dispatch::final_admission::read_bounded_json(path)?;
    Ok(value["schema_version"] == FREEZE_SCHEMA)
}

use alpha_domain::frozen_model::{
    FrozenModelStrategyV1, FrozenSupervisedCandidateV1, ModelFinalPrecommitV1,
    ModelSelectionEntryV1, ModelSelectionReportV1, FROZEN_FORMULA_SELECTION_PREFIX,
};
use alpha_domain::{
    CandidateArtifact, CexBaselineArtifactV1, CexBaselineModelKindV1, CexFinalPrecommitV1,
    CexResearchContentRefV1, CexSealedHoldoutClaimV1, EngineKind, IterationVerdict,
    ResearchIteration,
};
use alpha_engine::{
    engines::CexCombinationResearchArtifactV1,
    evaluation::prepare_dataset,
    final_models::{
        evaluate_frozen_holdout, evaluate_frozen_selection, freeze_supervised_candidate,
    },
    formula_evaluator::FormulaEvaluator,
    EngineProposal,
};
use alpha_store::{
    campaign_ledger::CampaignFinalOutcomeV1, AlphaStore, EvaluationRecord, RegistryRevision,
};
use chrono::Utc;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FinalResult {
    pub schema_version: String,
    pub campaign_id: String,
    pub family_id: String,
    pub request_sha256: String,
    pub final_grant_sha256: String,
    pub build_source_revision: String,
    pub image_identity: String,
    pub outcome: CampaignFinalOutcomeV1,
    pub candidates_considered: u32,
    pub candidates_evaluated: u32,
    pub selection_report: CexResearchContentRefV1,
    pub selected_candidate: Option<CexResearchContentRefV1>,
    pub precommit: Option<CexResearchContentRefV1>,
    pub sealed_receipt: Option<CexResearchContentRefV1>,
    pub strategy_bundle: Option<CexResearchContentRefV1>,
    pub promotion: Option<CexResearchContentRefV1>,
    pub bundle_sha256: String,
    pub bundle_bytes: u64,
    pub elapsed_to_result_seconds: u64,
}

#[derive(Debug)]
enum WinnerLane {
    Supervised,
    Formula,
}

fn winner_lane(
    supervised_replay_gate_passed: Option<bool>,
    replay_gate_passed: Option<bool>,
) -> anyhow::Result<Option<WinnerLane>> {
    match (supervised_replay_gate_passed, replay_gate_passed) {
        (Some(true), Some(true)) => {
            bail!("round mixed supervised and formula replay evidence")
        }
        (Some(true), _) => Ok(Some(WinnerLane::Supervised)),
        (_, Some(true)) => Ok(Some(WinnerLane::Formula)),
        _ => Ok(None),
    }
}

enum ClosedFamily {
    Supervised(Vec<SupervisedWinner>),
    Formula(Vec<FormulaWinner>),
}

struct SupervisedWinner {
    operation: String,
    result_sha256: String,
    results_dir: PathBuf,
    mission: alpha_domain::CexResearchMissionArtifactV1,
    research_mission: alpha_domain::ResearchMission,
    candidate: CexSupervisedModelCandidateV2,
    bank: CexFactorBankRevisionV2,
    baseline: CexBaselineArtifactV1,
}

struct FormulaWinner {
    operation: String,
    result_sha256: String,
    results_dir: PathBuf,
    mission: alpha_domain::CexResearchMissionArtifactV1,
    research_mission: alpha_domain::ResearchMission,
    strategy: CexCombinationResearchArtifactV1,
    factor_bank: CexFactorBankRevisionV2,
}

impl ClosedFamily {
    fn first_mission(&self) -> &alpha_domain::CexResearchMissionArtifactV1 {
        match self {
            Self::Supervised(sources) => &sources[0].mission,
            Self::Formula(sources) => &sources[0].mission,
        }
    }

    fn source_identities(&self) -> anyhow::Result<BTreeSet<(String, String, String, String)>> {
        match self {
            Self::Supervised(sources) => sources
                .iter()
                .map(|source| {
                    Ok((
                        source.operation.clone(),
                        source.result_sha256.clone(),
                        source.candidate.artifact_id.clone(),
                        canonical_json_hash(&source.candidate)?,
                    ))
                })
                .collect(),
            Self::Formula(sources) => sources
                .iter()
                .map(|source| {
                    Ok((
                        source.operation.clone(),
                        source.result_sha256.clone(),
                        source.strategy.artifact_id.clone(),
                        canonical_json_hash(&source.strategy)?,
                    ))
                })
                .collect(),
        }
    }
}

fn content_ref<T: Serialize>(id: &str, value: &T) -> anyhow::Result<CexResearchContentRefV1> {
    Ok(CexResearchContentRefV1 {
        id: id.into(),
        content_sha256: canonical_json_hash(value)?,
    })
}

fn access_overlay(
    request: &FinalRequest,
    source: &CampaignRequest,
) -> anyhow::Result<CampaignRequest> {
    let mut source = source.clone();
    source.feature_url = request.read_url(&source.feature_url)?.into();
    source.materialization_url = request.read_url(&source.materialization_url)?.into();
    source.replay_artifact_url = request.read_url(&source.replay_artifact_url)?.into();
    source.replay_manifest_url = request.read_url(&source.replay_manifest_url)?.into();
    source.campaign_result_readback_url = request
        .read_url(&source.campaign_result_readback_url)?
        .into();
    for round in &mut source.rounds {
        round.mission_readback_url = request.read_url(&round.mission_readback_url)?.into();
        round.result_readback_url = request.read_url(&round.result_readback_url)?.into();
    }
    Ok(source)
}

fn collect_sources(
    client: &Client,
    request: &FinalRequest,
    work: &Path,
) -> anyhow::Result<ClosedFamily> {
    let mut supervised = Vec::new();
    let mut formula = Vec::new();
    let mut total_bundle_bytes = 0u64;
    for (source_index, (operation, original)) in request.sources.iter().enumerate() {
        let root = work.join(format!("source-{source_index}"));
        std::fs::create_dir(&root)?;
        let source = access_overlay(request, original)?;
        let request_sha256 = hex::encode(Sha256::digest(serialize_request(original)?));
        let (_, _, result_sha256) = readback_pre_holdout_terminal_into(
            client,
            &source,
            &request_sha256,
            &request.grant.grant.execution.evaluation_protocol_sha256,
            &root,
        )?;
        if request.grant.grant.selected_results.get(operation) != Some(&result_sha256) {
            bail!("final source result digest differs from closed family");
        }
        let result = load_campaign_result(&root.join("campaign-result.json"))?;
        for (index, ledger) in result.rounds.iter().enumerate() {
            let round_dir = root.join(format!("round-{index}"));
            let zip = round_dir.join("result.zip");
            total_bundle_bytes = total_bundle_bytes
                .checked_add(std::fs::metadata(&zip)?.len())
                .context("final source size overflow")?;
            if total_bundle_bytes > 8 * 1024 * 1024 * 1024 {
                bail!("final source bundles exceed the bounded input budget");
            }
            // The immutable remote digest is recorded above; keep the verified
            // extracted source, not an extra compressed copy on the Job disk.
            std::fs::remove_file(zip)?;
            let Some(lane) = winner_lane(
                ledger.supervised_replay_gate_passed,
                ledger.replay_gate_passed,
            )?
            else {
                continue;
            };
            if supervised.len() + formula.len() >= request.grant.grant.max_candidates as usize {
                bail!("complete final shortlist exceeds signed candidate budget");
            }
            let results_dir = round_dir.join("extracted/results");
            let mission: alpha_domain::CexResearchMissionArtifactV1 =
                serde_json::from_slice(&std::fs::read(round_dir.join("mission.json"))?)?;
            let research_mission = AlphaStore::open_read_only(results_dir.join("alpha.duckdb"))?
                .get_mission(&mission.semantic_id()?)?;
            match lane {
                WinnerLane::Supervised => {
                    let selection: CexSupervisedModelSelectionV1 = serde_json::from_slice(
                        &std::fs::read(results_dir.join("supervised-model-selection.json"))?,
                    )?;
                    let mut selected = None;
                    for name in ["ridge", "cart", "burn_mlp"] {
                        let candidate: CexSupervisedModelCandidateV2 =
                            serde_json::from_slice(&std::fs::read(
                                results_dir.join(format!("{name}-supervised-candidate.json")),
                            )?)?;
                        if content_ref(&candidate.artifact_id, &candidate)?
                            == selection.selected_candidate
                        {
                            selected = Some(candidate);
                        }
                    }
                    let candidate = selected.context("final source selected model is missing")?;
                    if !candidate.evaluation.passed
                        || Some(&candidate.artifact_id) != ledger.supervised_candidate_id.as_ref()
                    {
                        bail!("final shortlist differs from passing source evidence");
                    }
                    let bank: CexFactorBankRevisionV2 = serde_json::from_slice(&std::fs::read(
                        results_dir.join("factor-bank.json"),
                    )?)?;
                    let baseline_name = match candidate.model_kind {
                        CexBaselineModelKindV1::Ridge => "ridge-baseline.json",
                        CexBaselineModelKindV1::ShallowCart => "cart-baseline.json",
                        CexBaselineModelKindV1::BurnMlp => "burn-mlp-baseline.json",
                    };
                    let baseline: CexBaselineArtifactV1 =
                        serde_json::from_slice(&std::fs::read(results_dir.join(baseline_name))?)?;
                    validate_supervised_candidate_binding(&candidate, &mission, &bank, &baseline)?;
                    supervised.push(SupervisedWinner {
                        operation: operation.clone(),
                        result_sha256: result_sha256.clone(),
                        results_dir,
                        mission,
                        research_mission,
                        candidate,
                        bank,
                        baseline,
                    });
                }
                WinnerLane::Formula => {
                    if ledger.supervised_candidate_id.is_some() {
                        bail!("formula shortlist mixed with supervised evidence");
                    }
                    let strategy: CexCombinationResearchArtifactV1 = serde_json::from_slice(
                        &std::fs::read(results_dir.join("combination-walk-forward.json"))?,
                    )?;
                    let factor_bank: CexFactorBankRevisionV2 = serde_json::from_slice(
                        &std::fs::read(results_dir.join("factor-bank.json"))?,
                    )?;
                    if Some(&strategy.artifact_id) != ledger.selected_candidate_id.as_ref() {
                        bail!("final formula shortlist differs from passing source evidence");
                    }
                    formula.push(FormulaWinner {
                        operation: operation.clone(),
                        result_sha256: result_sha256.clone(),
                        results_dir,
                        mission,
                        research_mission,
                        strategy,
                        factor_bank,
                    });
                }
            }
        }
    }
    match (supervised.is_empty(), formula.is_empty()) {
        (false, false) => bail!("closed family mixed supervised and formula winners"),
        (false, true) => Ok(ClosedFamily::Supervised(supervised)),
        (true, false) => Ok(ClosedFamily::Formula(formula)),
        (true, true) => bail!("closed family has no replay-qualified round winners"),
    }
}

pub(crate) fn execute(args: CampaignExecuteArgs) -> anyhow::Result<()> {
    let started = std::time::Instant::now();
    if args.pre_holdout || !args.final_evaluation {
        bail!("final evaluation requires its explicit execution mode");
    }
    let bytes = std::fs::read(&args.request)?;
    if bytes.len() as u64 > MAX_REQUEST_BYTES
        || hex::encode(Sha256::digest(&bytes)) != args.request_sha256
    {
        bail!("final request byte identity mismatch");
    }
    let request: FinalRequest = serde_json::from_slice(&bytes)?;
    request.validate()?;
    if request.campaign_id != args.campaign_id
        || request.image_identity != args.image_identity
        || request.build_source_revision != BUILD_SOURCE_REVISION
    {
        bail!("final worker identity mismatch");
    }
    let grant = mission_dispatch::final_admission::verify_worker_grant(
        &request.grant,
        args.final_trusted_keys
            .as_deref()
            .context("missing final trust keys")?,
    )?;
    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .redirect(Policy::none())
        .build()?;
    // A consumed global cohort can never become another final attempt.
    crate::mission_runner::ensure_holdout_claim_absent(
        &client,
        &request.holdout_claim_readback_url,
    )?;
    std::fs::create_dir_all(&args.work_dir)?;
    let work = args.work_dir.join(&request.campaign_id);
    std::fs::create_dir(&work).context(
        "final worker directory already exists; do not replay a partial final evaluation",
    )?;
    let results = work.join("results");
    let inputs = work.join("inputs");
    std::fs::create_dir(&results)?;
    std::fs::create_dir(&inputs)?;
    let first = request.first_source()?;
    let feature_path = inputs.join("features.jsonl");
    let materialization_path = inputs.join("materialization.json");
    let replay_path = inputs.join(format!("{}.parquet", first.replay_artifact_sha256));
    let replay_manifest_path = inputs.join("replay-manifest.json");
    for (label, url, path, hash, limit) in [
        (
            "final features",
            &first.feature_url,
            &feature_path,
            &first.feature_sha256,
            crate::mission_runner::MAX_FEATURE_BYTES,
        ),
        (
            "final materialization",
            &first.materialization_url,
            &materialization_path,
            &first.materialization_sha256,
            crate::mission_runner::MAX_MATERIALIZATION_BYTES,
        ),
        (
            "final replay",
            &first.replay_artifact_url,
            &replay_path,
            &first.replay_artifact_sha256,
            1024 * 1024 * 1024,
        ),
        (
            "final replay manifest",
            &first.replay_manifest_url,
            &replay_manifest_path,
            &first.replay_manifest_sha256,
            16 * 1024 * 1024,
        ),
    ] {
        fetch_verified(&client, label, request.read_url(url)?, path, hash, limit)?;
    }
    mission_dispatch::final_admission::validate_worker_dataset_binding(
        &request,
        &materialization_path,
    )?;
    let materialization =
        crate::mission_runner::decode_materialization(&std::fs::read(&materialization_path)?)?;
    let family = collect_sources(&client, &request, &work)?;
    let mission = family.first_mission().clone();
    crate::mission_runner::validate_mission_materialization_binding(
        &mission,
        &materialization,
        &first.materialization_sha256,
        &first.feature_sha256,
    )?;
    // Re-admit content-addressed features at this Job's local path; archived
    // source databases keep their historical paths and bytes unchanged.
    let mut data_store = AlphaStore::open_in_memory()?;
    let artifacts = work.join("artifacts");
    let feature_manifest = data_mission::import_and_register_features(
        &mut data_store,
        &mission.spec.data_mission_id,
        &feature_path,
        &artifacts,
    )?;
    let dataset_manifest = data_mission::admit_cex_replay_dataset(
        &mut data_store,
        &feature_manifest,
        &materialization.snapshot,
    )?;
    crate::mission_runner::validate_mission_dataset_binding(
        &mission,
        &feature_manifest,
        &dataset_manifest,
    )?;
    let dataset_path = inputs.join("dataset-manifest.json");
    data_mission::write_json_atomic(&dataset_path, &dataset_manifest)?;
    let registered = data_mission::read_registered_research_dataset(&data_store, &dataset_path)?;
    let dataset = prepare_dataset(
        registered.load_rows(&mission.spec.evaluation_protocol.costs)?,
        &mission.spec.evaluation_protocol,
    )?;
    let clocks = data_mission::feature_decision_clocks(&feature_manifest)?;
    let (selection, evaluated, outcome, precommit_ref, sealed_ref, bundle_ref, promotion_ref) =
        match family {
            ClosedFamily::Supervised(sources) => finalize_supervised_family(
                &grant,
                &sources,
                &dataset,
                &clocks,
                &results,
                &materialization,
                first,
                &replay_path,
                &replay_manifest_path,
                &client,
                &request,
            )?,
            ClosedFamily::Formula(sources) => {
                finalize_formula_family(&grant, &sources, &dataset, &results, &client, &request)?
            }
        };
    let bundle_path = work.join("final-result.zip");
    crate::mission_runner::create_bundle(&work, &bundle_path, [&results])?;
    let bundle_bytes = std::fs::metadata(&bundle_path)?.len();
    if bundle_bytes > MAX_RESULT_BUNDLE_BYTES {
        bail!("final result bundle exceeds publication limit");
    }
    let bundle_sha256 = crate::mission_runner::sha256_file(&bundle_path)?;
    let result = FinalResult {
        schema_version: "monday.campaign_final_result.v1".into(),
        campaign_id: request.campaign_id.clone(),
        family_id: grant.grant().family_id.clone(),
        request_sha256: args.request_sha256,
        final_grant_sha256: grant.content_sha256().into(),
        build_source_revision: BUILD_SOURCE_REVISION.into(),
        image_identity: args.image_identity,
        outcome,
        candidates_considered: u32::try_from(selection.entries.len())?,
        candidates_evaluated: evaluated,
        selection_report: content_ref(&selection.artifact_id, &selection)?,
        selected_candidate: selection.selected_candidate.clone(),
        precommit: precommit_ref,
        sealed_receipt: sealed_ref,
        strategy_bundle: bundle_ref,
        promotion: promotion_ref,
        bundle_sha256,
        bundle_bytes,
        elapsed_to_result_seconds: started.elapsed().as_secs(),
    };
    result.validate_identity(&request, &result.request_sha256)?;
    let result_path = work.join("final-result.json");
    data_mission::write_json_atomic(&result_path, &result)?;
    publish_immutable_file(
        &client,
        &request.bundle_put_url,
        &bundle_path,
        "application/zip",
    )?;
    fetch_verified(
        &client,
        "final bundle readback",
        &request.bundle_readback_url,
        &work.join("final-result-readback.zip"),
        &result.bundle_sha256,
        MAX_RESULT_BUNDLE_BYTES,
    )?;
    publish_immutable_file(
        &client,
        &request.result_put_url,
        &result_path,
        "application/json",
    )?;
    let result_hash = crate::mission_runner::sha256_file(&result_path)?;
    fetch_verified(
        &client,
        "final result readback",
        &request.result_readback_url,
        &work.join("final-result-readback.json"),
        &result_hash,
        MAX_CAMPAIGN_RESULT_BYTES,
    )?;
    print_json(
        &serde_json::json!({"campaign_id":request.campaign_id,"family_id":grant.grant().family_id,"outcome":result.outcome,
        "result_sha256":result_hash,"bundle_sha256":result.bundle_sha256,"candidates_considered":result.candidates_considered,
        "candidates_evaluated":result.candidates_evaluated,"holdout_opened":result.precommit.is_some(),"promotion":result.promotion}),
    )
}

type FamilyOutcome = (
    ModelSelectionReportV1,
    u32,
    CampaignFinalOutcomeV1,
    Option<CexResearchContentRefV1>,
    Option<CexResearchContentRefV1>,
    Option<CexResearchContentRefV1>,
    Option<CexResearchContentRefV1>,
);

fn copy_source_store(source_results: &Path, results: &Path) -> anyhow::Result<()> {
    for name in [
        "alpha.duckdb",
        "alpha.duckdb.integrity-key",
        "alpha.duckdb.wal",
    ] {
        let original = source_results.join(name);
        if original.try_exists()? {
            std::fs::copy(original, results.join(name))?;
        }
    }
    Ok(())
}

fn formula_frozen_ref(
    strategy: &CexCombinationResearchArtifactV1,
) -> anyhow::Result<CexResearchContentRefV1> {
    let hash = canonical_json_hash(strategy)?;
    content_ref(
        &format!("{FROZEN_FORMULA_SELECTION_PREFIX}{hash}"),
        strategy,
    )
}

#[allow(clippy::too_many_arguments)]
fn finalize_supervised_family(
    grant: &alpha_domain::campaign_finalization::VerifiedCampaignFinalEvaluationGrant,
    sources: &[SupervisedWinner],
    dataset: &alpha_engine::evaluation::PreparedDataset,
    clocks: &[crate::data_mission::FeatureDecisionClock],
    results: &Path,
    materialization: &crate::mission_runner::Materialization,
    first: &CampaignRequest,
    replay_path: &Path,
    replay_manifest_path: &Path,
    client: &Client,
    request: &FinalRequest,
) -> anyhow::Result<FamilyOutcome> {
    let mut entries = Vec::new();
    let mut fitted = BTreeMap::new();
    let mut evaluated = 0u32;
    for (index, source) in sources.iter().enumerate() {
        grant.validate_active_at(Utc::now())?;
        let source_ref = content_ref(&source.candidate.artifact_id, &source.candidate)?;
        let attempt = freeze_supervised_candidate(
            &dataset.engine_context(),
            &source.bank,
            &source.baseline,
            &source.candidate,
            source.mission.spec.instrument.venue.as_str(),
            source.mission.spec.instrument.market.as_str(),
            &source.mission.spec.instrument.symbol,
            &source.research_mission,
            grant.grant().max_candidates,
        )
        .and_then(|frozen| {
            evaluate_frozen_selection(&frozen, dataset).map(|report| (frozen, report))
        });
        match attempt {
            Ok((frozen, report)) => {
                evaluated += 1;
                let reference = content_ref(&frozen.artifact_id, &frozen)?;
                data_mission::write_json_atomic(
                    &results.join(format!("{}-model.json", frozen.artifact_id)),
                    &frozen,
                )?;
                data_mission::write_json_atomic(
                    &results.join(format!("{}-selection.json", frozen.artifact_id)),
                    &report,
                )?;
                entries.push(ModelSelectionEntryV1 {
                    source_operation_id: source.operation.clone(),
                    source_result_sha256: source.result_sha256.clone(),
                    source_candidate: source_ref,
                    frozen_candidate: Some(reference),
                    evaluation: Some(report.evaluation.clone()),
                    rejection_reason: None,
                });
                if fitted
                    .insert(frozen.artifact_id.clone(), (index, frozen, report))
                    .is_some()
                {
                    bail!("duplicate frozen candidate identity");
                }
            }
            Err(reason) => entries.push(ModelSelectionEntryV1 {
                source_operation_id: source.operation.clone(),
                source_result_sha256: source.result_sha256.clone(),
                source_candidate: source_ref,
                frozen_candidate: None,
                evaluation: None,
                rejection_reason: Some(reason),
            }),
        }
    }
    let selection = ModelSelectionReportV1::new(grant, entries).map_err(anyhow::Error::msg)?;
    data_mission::write_json_atomic(&results.join("independent-selection.json"), &selection)?;
    let mut outcome = CampaignFinalOutcomeV1::NoSelectionCandidate;
    let mut precommit_ref = None;
    let mut sealed_ref = None;
    let mut bundle_ref = None;
    let mut promotion_ref = None;
    if let Some(selected) = &selection.selected_candidate {
        let (index, frozen, evaluation) = fitted
            .get(&selected.id)
            .context("selected model is missing")?;
        let source = &sources[*index];
        grant.validate_active_at(Utc::now())?;
        let replay_policy: alpha_domain::CexEventReplayPolicyV1 = serde_json::from_slice(
            &std::fs::read(source.results_dir.join("replay-policy.json"))?,
        )?;
        let replay = crate::mission_runner::run_frozen_model_event_replay(
            results,
            &source.mission,
            materialization,
            &first.materialization_sha256,
            clocks,
            frozen,
            evaluation,
            &replay_policy,
            replay_path,
            &first.replay_artifact_sha256,
            replay_manifest_path,
            &first.replay_manifest_sha256,
        )?;
        outcome = CampaignFinalOutcomeV1::ReplayRejected;
        if replay.gate.passed {
            grant.validate_active_at(Utc::now())?;
            {
                let original = AlphaStore::open_read_only(source.results_dir.join("alpha.duckdb"))?;
                original.get_mission(&source.mission.semantic_id()?)?;
            }
            copy_source_store(&source.results_dir, results)?;
            let mut store = AlphaStore::open(results.join("alpha.duckdb"))?;
            let lineage = store.mission_lineage(&source.mission.semantic_id()?)?;
            let now = Utc::now();
            let (authority_ref, selection_ref) =
                store.put_model_final_authority(grant, &selection, now)?;
            data_mission::write_json_atomic(
                &results.join("final-authority.json"),
                &store.get_registry_revision(&authority_ref.id)?,
            )?;
            data_mission::write_json_atomic(&results.join("source-mission.json"), &source.mission)?;
            let replay_reference =
                content_ref(&replay.receipt_id, &serde_json::to_value(&replay)?)?;
            store.put_registry_revision(&RegistryRevision {
                revision_id: replay.receipt_id.clone(),
                registry_kind: "cex_frozen_model_event_replay_receipt".into(),
                asset_id: lineage.mission.mission_id.clone(),
                parent_revision_id: Some(frozen.artifact_id.clone()),
                payload: serde_json::to_value(&replay)?,
                created_at: now,
            })?;
            let strategy = FrozenModelStrategyV1 {
                schema_version: "monday.frozen_model_strategy.v1".into(),
                mission_id: lineage.mission.mission_id.clone(),
                precommit_id: format!("cex-final-precommit:{}", lineage.mission.mission_id),
                frozen: frozen.clone(),
                evaluation_protocol: source.mission.spec.evaluation_protocol.clone(),
                instrument_rules: materialization.snapshot.instrument_rules.clone(),
            };
            let candidate = CandidateArtifact::FrozenModel(Box::new(strategy));
            let candidate_hash = canonical_json_hash(&candidate)?;
            let candidate_id = format!("cex-final-model-{candidate_hash}");
            let evaluation_id = format!("cex-independent-selection:{}", frozen.artifact_id);
            let record = EvaluationRecord {
                evaluation_id: evaluation_id.clone(),
                mission_id: lineage.mission.mission_id.clone(),
                candidate_id: candidate_id.clone(),
                dataset_manifest_id: lineage.mission.dataset_manifest_id.as_str().into(),
                evaluation_protocol_hash: frozen.evaluation_protocol_sha256.clone(),
                payload: serde_json::to_value(&evaluation.evaluation)?,
                created_at: now,
            };
            let iteration = ResearchIteration {
                iteration_id: format!("cex-final-model-iteration-{candidate_hash}"),
                mission_id: lineage.mission.mission_id.clone(),
                parent_candidate_ids: source
                    .bank
                    .entries
                    .iter()
                    .map(|entry| entry.candidate_id.clone())
                    .collect(),
                engine: EngineKind::FinalEvaluation,
                hypothesis: format!(
                    "frozen final evaluation of {}",
                    source.candidate.artifact_id
                ),
                candidate_artifact_id: Some(candidate_id.clone()),
                evaluation_artifact_id: Some(evaluation_id),
                budget_usage: lineage
                    .iterations
                    .last()
                    .map(|iteration| iteration.budget_usage.clone())
                    .unwrap_or_default(),
                verdict: IterationVerdict::Keep,
                failure_class: None,
                failure_explanation: None,
                created_at: now,
            };
            let precommit = ModelFinalPrecommitV1 {
                schema_version: "monday.model_final_precommit.v1".into(),
                precommit_id: format!("cex-final-precommit:{}", lineage.mission.mission_id),
                mission_id: lineage.mission.mission_id.clone(),
                family_id: grant.grant().family_id.clone(),
                final_grant: authority_ref,
                selection_report: selection_ref,
                source_result: CexResearchContentRefV1 {
                    id: source.operation.clone(),
                    content_sha256: source.result_sha256.clone(),
                },
                final_candidate: CexResearchContentRefV1 {
                    id: candidate_id.clone(),
                    content_sha256: candidate_hash.clone(),
                },
                frozen_candidate: selected.clone(),
                replay_receipt: replay_reference,
                evaluation_protocol: source.mission.spec.policies.evaluation.clone(),
                dataset_manifest_id: lineage.mission.dataset_manifest_id.clone(),
                holdout_id: first.holdout_id.clone(),
                implementation_source_revision: BUILD_SOURCE_REVISION.into(),
            };
            store.put_model_final_precommit(
                &iteration,
                &candidate_id,
                &candidate,
                &record,
                &precommit,
            )?;
            data_mission::write_json_atomic(&results.join("final-precommit.json"), &precommit)?;
            data_mission::write_json_atomic(&results.join("final-candidate.json"), &candidate)?;
            let claim = CexSealedHoldoutClaimV1::from_model_precommit(&precommit)?;
            grant.validate_active_at(Utc::now())?;
            let sealed = crate::mission_runner::open_cex_holdout(
                &mut store,
                results,
                client,
                &claim,
                &request.holdout_claim_put_url,
                &request.holdout_claim_readback_url,
                || {
                    evaluate_frozen_holdout(frozen, dataset)
                        .map(|report| report.evaluation)
                        .map_err(anyhow::Error::msg)
                },
            )?;
            data_mission::write_json_atomic(&results.join("sealed-holdout-receipt.json"), &sealed)?;
            let (bundle_id, promotion_id) = crate::mission_runner::promote_sealed_candidate(
                &mut store,
                results,
                &lineage.mission,
                &candidate,
                &candidate_id,
                &candidate_hash,
                &frozen.evaluation_protocol_sha256,
                &sealed,
            )?;
            precommit_ref = Some(precommit.content_reference()?);
            sealed_ref = Some(content_ref(&sealed.revision_id, &sealed)?);
            outcome = CampaignFinalOutcomeV1::HoldoutRejected;
            if let (Some(bundle_id), Some(promotion_id)) = (bundle_id, promotion_id) {
                let bundle = store.get_strategy_bundle(&bundle_id)?;
                let promotion = store.get_promotion(&promotion_id)?;
                bundle_ref = Some(content_ref(&bundle_id, &bundle)?);
                promotion_ref = Some(content_ref(&promotion_id, &promotion.record)?);
                outcome = CampaignFinalOutcomeV1::PromotionReady;
            }
        }
    }
    Ok((
        selection,
        evaluated,
        outcome,
        precommit_ref,
        sealed_ref,
        bundle_ref,
        promotion_ref,
    ))
}

fn finalize_formula_family(
    grant: &alpha_domain::campaign_finalization::VerifiedCampaignFinalEvaluationGrant,
    sources: &[FormulaWinner],
    dataset: &alpha_engine::evaluation::PreparedDataset,
    results: &Path,
    client: &Client,
    request: &FinalRequest,
) -> anyhow::Result<FamilyOutcome> {
    let mut entries = Vec::new();
    let mut fitted = BTreeMap::new();
    let mut evaluated = 0u32;
    for (index, source) in sources.iter().enumerate() {
        grant.validate_active_at(Utc::now())?;
        let source_ref = content_ref(&source.strategy.artifact_id, &source.strategy)?;
        let frozen_ref = formula_frozen_ref(&source.strategy)?;
        let attempt = source
            .strategy
            .executable_formula(&source.factor_bank)
            .and_then(|ast| {
                FormulaEvaluator::for_mission(&source.research_mission).and_then(|evaluator| {
                    evaluator.evaluate_independent_selection(
                        &EngineProposal {
                            candidate_id: source.strategy.artifact_id.clone(),
                            hypothesis: format!(
                                "independent formula selection of {}",
                                source.strategy.artifact_id
                            ),
                            artifact: CandidateArtifact::Formula(ast),
                            expansions: 0,
                            tokens: 0,
                            elapsed_ms: 0,
                        },
                        dataset,
                    )
                })
            });
        match attempt {
            Ok(report) => {
                evaluated += 1;
                data_mission::write_json_atomic(
                    &results.join(format!("{}-model.json", frozen_ref.id)),
                    &source.strategy,
                )?;
                data_mission::write_json_atomic(
                    &results.join(format!("{}-selection.json", frozen_ref.id)),
                    &report,
                )?;
                entries.push(ModelSelectionEntryV1 {
                    source_operation_id: source.operation.clone(),
                    source_result_sha256: source.result_sha256.clone(),
                    source_candidate: source_ref,
                    frozen_candidate: Some(frozen_ref.clone()),
                    evaluation: Some(report.evaluation.clone()),
                    rejection_reason: None,
                });
                if fitted.insert(frozen_ref.id.clone(), index).is_some() {
                    bail!("duplicate frozen formula identity");
                }
            }
            Err(reason) => entries.push(ModelSelectionEntryV1 {
                source_operation_id: source.operation.clone(),
                source_result_sha256: source.result_sha256.clone(),
                source_candidate: source_ref,
                frozen_candidate: None,
                evaluation: None,
                rejection_reason: Some(reason),
            }),
        }
    }
    let selection = ModelSelectionReportV1::new(grant, entries).map_err(anyhow::Error::msg)?;
    data_mission::write_json_atomic(&results.join("independent-selection.json"), &selection)?;
    let mut outcome = CampaignFinalOutcomeV1::NoSelectionCandidate;
    let mut precommit_ref = None;
    let mut sealed_ref = None;
    let mut bundle_ref = None;
    let mut promotion_ref = None;
    if let Some(selected) = &selection.selected_candidate {
        let index = fitted
            .get(&selected.id)
            .context("selected formula is missing")?;
        let source = &sources[*index];
        grant.validate_active_at(Utc::now())?;
        {
            let original = AlphaStore::open_read_only(source.results_dir.join("alpha.duckdb"))?;
            original.get_mission(&source.mission.semantic_id()?)?;
        }
        copy_source_store(&source.results_dir, results)?;
        let mut store = AlphaStore::open(results.join("alpha.duckdb"))?;
        let report = crate::mission_runner::finalize_formula_search_round(
            &source.results_dir,
            results,
            client,
            &request.holdout_claim_put_url,
            &request.holdout_claim_readback_url,
            &source.mission,
            &mut store,
            dataset,
        )?;
        let replay_src = source.results_dir.join("cex-event-replay-receipt.json");
        if replay_src.try_exists()? {
            std::fs::copy(&replay_src, results.join("cex-event-replay-receipt.json"))?;
        }
        let precommit: CexFinalPrecommitV1 =
            serde_json::from_slice(&std::fs::read(results.join("final-precommit.json"))?)?;
        let sealed: RegistryRevision =
            serde_json::from_slice(&std::fs::read(results.join("sealed-holdout-receipt.json"))?)?;
        precommit_ref = Some(content_ref(&precommit.precommit_id, &precommit)?);
        sealed_ref = Some(content_ref(&sealed.revision_id, &sealed)?);
        outcome = CampaignFinalOutcomeV1::HoldoutRejected;
        if let (Some(bundle_id), Some(promotion_id)) =
            (report.strategy_bundle_id, report.promotion_id)
        {
            let bundle = store.get_strategy_bundle(&bundle_id)?;
            let promotion = store.get_promotion(&promotion_id)?;
            bundle_ref = Some(content_ref(&bundle_id, &bundle)?);
            promotion_ref = Some(content_ref(&promotion_id, &promotion.record)?);
            outcome = CampaignFinalOutcomeV1::PromotionReady;
        }
    }
    Ok((
        selection,
        evaluated,
        outcome,
        precommit_ref,
        sealed_ref,
        bundle_ref,
        promotion_ref,
    ))
}

impl FinalResult {
    fn validate_identity(
        &self,
        request: &FinalRequest,
        expected_request_sha256: &str,
    ) -> anyhow::Result<()> {
        if self.schema_version != "monday.campaign_final_result.v1"
            || self.campaign_id != request.campaign_id
            || self.family_id != request.grant.grant.family_id
            || self.request_sha256 != expected_request_sha256
            || self.final_grant_sha256 != request.grant.content_sha256
            || self.build_source_revision != request.build_source_revision
            || self.image_identity != request.image_identity
            || self.candidates_considered == 0
            || self.candidates_considered > request.grant.grant.max_candidates
            || self.candidates_evaluated > self.candidates_considered
            || self.bundle_bytes == 0
            || self.bundle_bytes > MAX_RESULT_BUNDLE_BYTES
            || self.elapsed_to_result_seconds > request.grant.grant.max_job_seconds
            || normalized_sha256("final bundle", &self.bundle_sha256)? != self.bundle_sha256
        {
            bail!("final result identity or consumption differs from its admitted request");
        }
        self.selection_report.validate()?;
        for reference in [
            &self.selected_candidate,
            &self.precommit,
            &self.sealed_receipt,
            &self.strategy_bundle,
            &self.promotion,
        ]
        .into_iter()
        .flatten()
        {
            reference.validate()?;
        }
        let shape = (
            self.selected_candidate.is_some(),
            self.precommit.is_some(),
            self.sealed_receipt.is_some(),
            self.strategy_bundle.is_some(),
            self.promotion.is_some(),
        );
        let expected = match self.outcome {
            CampaignFinalOutcomeV1::NoSelectionCandidate => (false, false, false, false, false),
            CampaignFinalOutcomeV1::ReplayRejected => (true, false, false, false, false),
            CampaignFinalOutcomeV1::HoldoutRejected => (true, true, true, false, false),
            CampaignFinalOutcomeV1::PromotionReady => (true, true, true, true, true),
            CampaignFinalOutcomeV1::Failed => {
                bail!("failed final Job requires independent infrastructure evidence")
            }
        };
        if shape != expected {
            bail!("final result evidence does not match its outcome");
        }
        Ok(())
    }
}

pub(crate) fn readback_terminal(
    client: &Client,
    request: &FinalRequest,
    request_sha256: &str,
    grant: &alpha_domain::campaign_finalization::VerifiedCampaignFinalEvaluationGrant,
) -> anyhow::Result<(FinalResult, String)> {
    request.validate()?;
    if request.grant != *grant.signed_grant() {
        bail!("final readback grant differs from closed family");
    }
    let root = tempfile::tempdir()?;
    let result_path = root.path().join("final-result.json");
    let (_, result_sha256) = fetch_to_file(
        client,
        &request.result_readback_url,
        &result_path,
        MAX_CAMPAIGN_RESULT_BYTES,
    )
    .map_err(terminal_readback_error)?;
    let result: FinalResult = serde_json::from_slice(&std::fs::read(&result_path)?)?;
    result.validate_identity(request, request_sha256)?;
    let bundle_path = root.path().join("final-result.zip");
    fetch_verified(
        client,
        "final bundle",
        &request.bundle_readback_url,
        &bundle_path,
        &result.bundle_sha256,
        MAX_RESULT_BUNDLE_BYTES,
    )
    .map_err(terminal_readback_error)?;
    if std::fs::metadata(&bundle_path)?.len() != result.bundle_bytes {
        bail!("final bundle size differs from result");
    }
    let extracted = root.path().join("extracted");
    extract_bundle_with_file_limit(
        &bundle_path,
        &extracted,
        2 * alpha_domain::campaign_finalization::MAX_FINAL_CANDIDATES as usize + 16,
    )?;
    let results = extracted.join("results");
    let selection: ModelSelectionReportV1 =
        serde_json::from_slice(&std::fs::read(results.join("independent-selection.json"))?)?;
    selection
        .validate_against(grant)
        .map_err(anyhow::Error::msg)?;
    if content_ref(&selection.artifact_id, &selection)? != result.selection_report
        || selection.selected_candidate != result.selected_candidate
        || selection.entries.len() != result.candidates_considered as usize
        || selection
            .entries
            .iter()
            .filter(|entry| entry.evaluation.is_some())
            .count()
            != result.candidates_evaluated as usize
    {
        bail!("final selection report differs from terminal result");
    }
    // Reconstruct only immutable pre-holdout provenance. Do not reopen either
    // evaluation window or rerun training during terminal readback.
    let source_root = root.path().join("sources");
    std::fs::create_dir(&source_root)?;
    let sources = collect_sources(client, request, &source_root)?;
    let expected = sources.source_identities()?;
    let observed: BTreeSet<_> = selection
        .entries
        .iter()
        .map(|entry| {
            (
                entry.source_operation_id.clone(),
                entry.source_result_sha256.clone(),
                entry.source_candidate.id.clone(),
                entry.source_candidate.content_sha256.clone(),
            )
        })
        .collect();
    if expected != observed {
        bail!("final selection omitted or added a replay-qualified source winner");
    }
    match &sources {
        ClosedFamily::Supervised(sources) => {
            for entry in &selection.entries {
                if let Some(reference) = &entry.frozen_candidate {
                    let source = sources
                        .iter()
                        .find(|source| {
                            source.operation == entry.source_operation_id
                                && source.candidate.artifact_id == entry.source_candidate.id
                        })
                        .context("final source missing")?;
                    let frozen: FrozenSupervisedCandidateV1 = serde_json::from_slice(
                        &std::fs::read(results.join(format!("{}-model.json", reference.id)))?,
                    )?;
                    frozen
                        .validate_against_protocol(&source.mission.spec.evaluation_protocol)
                        .map_err(anyhow::Error::msg)?;
                    frozen
                        .validate_fitted_origin(&source.baseline, &source.bank)
                        .map_err(anyhow::Error::msg)?;
                    if content_ref(&frozen.artifact_id, &frozen)? != *reference
                        || frozen.source_candidate != entry.source_candidate
                        || frozen.program.decision_policy != source.candidate.decision_policy
                        || frozen.evaluator_config
                            != alpha_domain::frozen_model::final_evaluator_config(
                                &source.research_mission,
                                grant.grant().max_candidates,
                            )?
                    {
                        bail!("final frozen model source changed");
                    }
                    let report: alpha_engine::formula_evaluator::PositionEvaluationReport =
                        serde_json::from_slice(&std::fs::read(
                            results.join(format!("{}-selection.json", reference.id)),
                        )?)?;
                    if Some(&report.evaluation) != entry.evaluation.as_ref()
                        || report.ledger.len() != report.evaluation.metrics.row_count
                    {
                        bail!("final selection ledger differs from evaluation");
                    }
                }
            }
            if let Some(selected) = &result.selected_candidate {
                let replay: CexEventReplayReceiptV1 = serde_json::from_slice(&std::fs::read(
                    results.join("frozen-model-event-replay-receipt.json"),
                )?)?;
                replay.validate()?;
                if replay.strategy != *selected
                    || replay.gate.passed
                        == (result.outcome == CampaignFinalOutcomeV1::ReplayRejected)
                {
                    bail!("frozen replay candidate or gate differs from result");
                }
            }
            if let Some(precommit_reference) = &result.precommit {
                let store = AlphaStore::open_read_only(results.join("alpha.duckdb"))?;
                let evidence = store.read_model_finalization_evidence(&precommit_reference.id)?;
                let file_precommit: ModelFinalPrecommitV1 =
                    serde_json::from_slice(&std::fs::read(results.join("final-precommit.json"))?)?;
                if evidence.precommit != file_precommit
                    || evidence.precommit.content_reference()? != *precommit_reference
                    || evidence.precommit.frozen_candidate
                        != *result.selected_candidate.as_ref().unwrap()
                    || evidence.precommit.family_id != result.family_id
                    || evidence.precommit.selection_report != result.selection_report
                    || Some(content_ref(&evidence.sealed.revision_id, &evidence.sealed)?)
                        != result.sealed_receipt
                {
                    bail!("model finalization database and published files differ");
                }
                readback_holdout_and_promotion(
                    client,
                    request,
                    root.path(),
                    &results,
                    &result,
                    &store,
                    &evidence.claim,
                    &evidence.sealed,
                    &evidence.precommit.final_candidate.id,
                )?;
            }
        }
        ClosedFamily::Formula(sources) => {
            for entry in &selection.entries {
                if let Some(reference) = &entry.frozen_candidate {
                    let source = sources
                        .iter()
                        .find(|source| {
                            source.operation == entry.source_operation_id
                                && source.strategy.artifact_id == entry.source_candidate.id
                        })
                        .context("final source missing")?;
                    let strategy: CexCombinationResearchArtifactV1 = serde_json::from_slice(
                        &std::fs::read(results.join(format!("{}-model.json", reference.id)))?,
                    )?;
                    if formula_frozen_ref(&strategy)? != *reference
                        || content_ref(&strategy.artifact_id, &strategy)? != entry.source_candidate
                        || strategy.artifact_id != source.strategy.artifact_id
                    {
                        bail!("final frozen formula source changed");
                    }
                    let report: alpha_engine::formula_evaluator::PositionEvaluationReport =
                        serde_json::from_slice(&std::fs::read(
                            results.join(format!("{}-selection.json", reference.id)),
                        )?)?;
                    if Some(&report.evaluation) != entry.evaluation.as_ref()
                        || report.ledger.len() != report.evaluation.metrics.row_count
                    {
                        bail!("final selection ledger differs from evaluation");
                    }
                }
            }
            if result.selected_candidate.is_some() {
                let replay: CexEventReplayReceiptV1 = serde_json::from_slice(&std::fs::read(
                    results.join("cex-event-replay-receipt.json"),
                )?)?;
                replay.validate()?;
                if !replay.gate.passed || result.outcome == CampaignFinalOutcomeV1::ReplayRejected {
                    bail!("formula replay candidate or gate differs from result");
                }
            }
            if let Some(precommit_reference) = &result.precommit {
                let store = AlphaStore::open_read_only(results.join("alpha.duckdb"))?;
                let file_precommit: CexFinalPrecommitV1 =
                    serde_json::from_slice(&std::fs::read(results.join("final-precommit.json"))?)?;
                let stored = store.get_registry_revision(&file_precommit.precommit_id)?;
                let stored_precommit: CexFinalPrecommitV1 = serde_json::from_value(stored.payload)?;
                let sealed: RegistryRevision = serde_json::from_slice(&std::fs::read(
                    results.join("sealed-holdout-receipt.json"),
                )?)?;
                if stored_precommit != file_precommit
                    || content_ref(&file_precommit.precommit_id, &file_precommit)?
                        != *precommit_reference
                    || Some(content_ref(&sealed.revision_id, &sealed)?) != result.sealed_receipt
                {
                    bail!("formula finalization database and published files differ");
                }
                let claim = CexSealedHoldoutClaimV1::from_precommit(&file_precommit)?;
                readback_holdout_and_promotion(
                    client,
                    request,
                    root.path(),
                    &results,
                    &result,
                    &store,
                    &claim,
                    &sealed,
                    &file_precommit.final_candidate.id,
                )?;
            }
        }
    }
    Ok((result, result_sha256))
}

#[allow(clippy::too_many_arguments)]
fn readback_holdout_and_promotion(
    client: &Client,
    request: &FinalRequest,
    root: &Path,
    results: &Path,
    result: &FinalResult,
    store: &AlphaStore,
    claim: &CexSealedHoldoutClaimV1,
    sealed_revision: &RegistryRevision,
    final_candidate_id: &str,
) -> anyhow::Result<()> {
    let global = root.join("global-claim.json");
    fetch_verified(
        client,
        "global holdout claim",
        &request.holdout_claim_readback_url,
        &global,
        &crate::mission_runner::sha256_file(&results.join("sealed-holdout-claim.json"))?,
        64 * 1024,
    )
    .map_err(terminal_readback_error)?;
    let published: CexSealedHoldoutClaimV1 = serde_json::from_slice(&std::fs::read(global)?)?;
    if published != *claim {
        bail!("global holdout claim differs from finalization evidence");
    }
    let sealed: CandidateEvaluation =
        serde_json::from_value(sealed_revision.payload["evaluation"].clone())?;
    if sealed.passed != (result.outcome == CampaignFinalOutcomeV1::PromotionReady) {
        bail!("sealed verdict differs from final outcome");
    }
    if let (Some(bundle_ref), Some(promotion_ref)) = (&result.strategy_bundle, &result.promotion) {
        let bundle = store.get_strategy_bundle(&bundle_ref.id)?;
        let promotion = store.get_promotion(&promotion_ref.id)?;
        if content_ref(&bundle.bundle_id, &bundle)? != *bundle_ref
            || content_ref(&promotion.record.promotion_id, &promotion.record)? != *promotion_ref
            || promotion.record.bundle_hash != bundle.bundle_hash
            || promotion.record.candidate_id != final_candidate_id
        {
            bail!("final promotion identities differ");
        }
        let bundle_file: alpha_domain::StrategyBundle =
            serde_json::from_slice(&std::fs::read(results.join("strategy-bundle.json"))?)?;
        let promotion_file: alpha_domain::PromotionRecord =
            serde_json::from_slice(&std::fs::read(results.join("promotion-record.json"))?)?;
        if bundle != bundle_file || promotion.record != promotion_file {
            bail!("final promotion files differ from authenticated database");
        }
    }
    Ok(())
}

fn canonical_final_object(label: &str, value: &str) -> anyhow::Result<String> {
    #[cfg(test)]
    if Path::new(value).is_absolute() {
        if Path::new(value)
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            bail!("local final test object escapes its root");
        }
        return Ok(value.to_string());
    }
    canonical_tokyo_oss_internal_object(label, value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alpha_domain::campaign_control::{
        CampaignEvaluationViewsV1, CampaignExecutionBindingV1, CampaignSelectionFeedbackV1,
    };
    use alpha_domain::campaign_finalization::{
        sign_campaign_final_evaluation_grant, CampaignFinalEvaluationGrantV1,
        FINAL_EVALUATION_GRANT_SCHEMA,
    };

    fn request() -> FinalRequest {
        let source = super::super::valid_request_for_tests();
        let operation = format!("campaign-attempt-{}", "a".repeat(64));
        let key = ed25519_dalek::SigningKey::from_bytes(&[17; 32]);
        let now = Utc::now();
        let grant = CampaignFinalEvaluationGrantV1 {
            schema_version: FINAL_EVALUATION_GRANT_SCHEMA.into(),
            grant_id: "test-final".into(),
            family_id: "test-family".into(),
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
                job_memory_mib: 12288,
            },
            selected_results: BTreeMap::from([(operation.clone(), "f".repeat(64))]),
            max_candidates: 4,
            max_job_seconds: 3600,
            valid_from: now - chrono::TimeDelta::minutes(1),
            expires_at: now + chrono::TimeDelta::hours(2),
        };
        FinalRequest::new(sign_campaign_final_evaluation_grant(grant,"test-key".into(),&key).unwrap(), BTreeMap::from([(operation,source)]),
            "https://monday-lob-apne1-1045353359.oss-ap-northeast-1-internal.aliyuncs.com/research/final-tests".into()).unwrap()
    }

    #[test]
    fn winner_lane_admits_formula_and_supervised_replay_and_rejects_mixed_evidence() {
        assert!(matches!(
            winner_lane(Some(true), None).unwrap(),
            Some(WinnerLane::Supervised)
        ));
        assert!(matches!(
            winner_lane(None, Some(true)).unwrap(),
            Some(WinnerLane::Formula)
        ));
        assert!(winner_lane(Some(false), Some(false)).unwrap().is_none());
        assert!(winner_lane(None, None).unwrap().is_none());
        assert!(winner_lane(Some(true), Some(true))
            .unwrap_err()
            .to_string()
            .contains("mixed supervised and formula"));
    }

    #[test]
    fn final_request_allows_presign_refresh_without_rewriting_sources() {
        let original = request();
        let mut signed = original.clone();
        for value in signed.read_urls.values_mut() {
            value.push_str("?signature=test");
        }
        signed.result_put_url.push_str("?signature=result-put");
        signed.result_readback_url.push_str("?signature=result-get");
        signed.validate().unwrap();
        assert_eq!(signed.canonical().unwrap(), original);
        let mut changed = signed.clone();
        changed
            .sources
            .values_mut()
            .next()
            .unwrap()
            .declared_total_trials += 1;
        assert!(changed.validate().is_err());
        changed = signed.clone();
        changed.grant.grant.max_candidates += 1;
        assert!(changed.validate().is_err());
        changed = signed;
        changed.read_urls.pop_first();
        assert!(changed.validate().is_err());
    }

    #[test]
    fn final_result_cannot_claim_promotion_or_exceed_accounting_bounds() {
        let request = request();
        let result = FinalResult {
            schema_version: "monday.campaign_final_result.v1".into(),
            campaign_id: request.campaign_id.clone(),
            family_id: request.grant.grant.family_id.clone(),
            request_sha256: "1".repeat(64),
            final_grant_sha256: request.grant.content_sha256.clone(),
            build_source_revision: request.build_source_revision.clone(),
            image_identity: request.image_identity.clone(),
            outcome: CampaignFinalOutcomeV1::NoSelectionCandidate,
            candidates_considered: 2,
            candidates_evaluated: 0,
            selection_report: CexResearchContentRefV1 {
                id: "selection".into(),
                content_sha256: "2".repeat(64),
            },
            selected_candidate: None,
            precommit: None,
            sealed_receipt: None,
            strategy_bundle: None,
            promotion: None,
            bundle_sha256: "3".repeat(64),
            bundle_bytes: 1,
            elapsed_to_result_seconds: 1,
        };
        result.validate_identity(&request, &"1".repeat(64)).unwrap();
        let mut changed = result.clone();
        changed.outcome = CampaignFinalOutcomeV1::PromotionReady;
        assert!(changed
            .validate_identity(&request, &"1".repeat(64))
            .is_err());
        changed = result.clone();
        changed.candidates_considered = 5;
        assert!(changed
            .validate_identity(&request, &"1".repeat(64))
            .is_err());
        changed = result;
        changed.elapsed_to_result_seconds = 3601;
        assert!(changed
            .validate_identity(&request, &"1".repeat(64))
            .is_err());
    }
}
