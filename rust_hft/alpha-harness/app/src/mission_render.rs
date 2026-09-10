use crate::{
    cli::ValidationArgs,
    data_mission,
    mission_runner::{
        decode_materialization, normalized_sha256, sha256_file, validate_materialization,
        MAX_FEATURE_BYTES, MAX_MATERIALIZATION_BYTES,
    },
};
use alpha_domain::{
    campaign_horizon::CampaignLabelHorizonV1, canonical_json_hash, CexBaselinePolicyV1,
    CexEqualAbsoluteWeightPolicyV1, CexEventReplayPolicyV1, CexGpPolicyV1, CexResearchContentRefV1,
    CexResearchDeltaConfigV1, CexResearchEvidenceKindV1, CexResearchEvidenceRefV1,
    CexResearchFalsificationTestV1, CexResearchHoldoutStateV1, CexResearchHoldoutV1,
    CexResearchHypothesisTargetV1, CexResearchHypothesisV1, CexResearchInputBindingsV1,
    CexResearchInstrumentV1, CexResearchMarketV1, CexResearchMissionArtifactV1,
    CexResearchMissionSpecV1, CexResearchOperationalMetadataV1, CexResearchPolicyBindingsV1,
    CexResearchSearchPlanV1, CexResearchVenueV1, EvaluationCostsV1, EvaluationLabelSpecV1,
    EvaluationProtocolV1, EvaluationWalkForwardV1, SearchBudget,
    CEX_RESEARCH_AGGREGATE_TRADE_FLOW_IMBALANCE_FIELD, CEX_RESEARCH_MISSION_SCHEMA_V1,
};
use alpha_engine::baselines::CexSupervisedDecisionPolicyV2;
use anyhow::{bail, Context};
use hft_collector::{import_feature_dataset, FeatureDatasetManifest};
use hft_factor_dsl::FactorOperator;
use hft_research_manifest::CexReplayDatasetManifestV5;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::Path};

const STABLE_VERSION: &str = "binance-btcusdt-usdm-1s-h5-top5-factor-plan-v5";
const STABLE_HYPOTHESIS_ID: &str = "l2-microstructure-factor-plan-v5";
const RESEARCH_PLAN_SCHEMA_V2: &str = "cex-campaign-research-plan-v2";
const SEARCH_POLICY_REVISION_SCHEMA_V1: &str = "cex-campaign-search-policy-revision-v1";
const SEARCH_POLICY_REVISION_SCHEMA_V2: &str = "cex-campaign-search-policy-revision-v2";
const LEARNING_DIRECTIVE_SCHEMA_V1: &str = "cex-campaign-learning-directive-v1";
const RESEARCH_EVIDENCE_SIGNATURE_SCHEMA_V2: &str = "cex-campaign-research-evidence-signature-v2";
pub(crate) const MAX_RESEARCH_PLAN_GENERATION: u8 = 3;
const INITIAL_TRAIN_ROWS: usize = 7_200;
const VALIDATION_ROWS: usize = 3_600;
const FOLD_COUNT: usize = 3;
// Labels mature five seconds after observation. Leave another full horizon
// before validation so the bound trainer's purge/embargo contract is real.
const PURGE_ROWS: usize = 10;
// Keep the last validation label strictly before the sealed holdout.
const EMBARGO_ROWS: usize = 5;
const HOLDOUT_ROWS: usize = 3_600;
const SELECTION_ROWS: usize = 3_600;
const MIN_ROWS: usize = INITIAL_TRAIN_ROWS
    + FOLD_COUNT * (VALIDATION_ROWS + EMBARGO_ROWS)
    + PURGE_ROWS
    + SELECTION_ROWS
    + 2 * PURGE_ROWS
    + HOLDOUT_ROWS;
const MAX_EXPANSIONS: u64 = 256;
const GP_POLICY_ID: &str = "binance-btcusdt-usdm-1s-h5-top5-factor-plan-v5-gp-policy";
const BASELINE_POLICY_ID: &str = "binance-btcusdt-usdm-1s-h5-top5-baseline-policy";
const WEIGHT_POLICY_ID: &str = "binance-btcusdt-usdm-1s-h5-top5-weight-policy";
const REPLAY_POLICY_ID: &str = "binance-btcusdt-usdm-1s-h5-top5-replay-policy";
const SCREENING_POLICY_ID: &str = "binance-btcusdt-usdm-1s-h5-top5-screening-policy";
const SUBSET_POLICY_ID: &str = "binance-btcusdt-usdm-1s-h5-top5-subset-policy";
const EVALUATION_POLICY_ID: &str = "binance-btcusdt-usdm-1s-h5-top5-evaluation-policy";
const HOLDOUT_POLICY_ID: &str = "binance-btcusdt-usdm-1s-h5-top5-holdout-policy";
const FEATURE_FIELDS: [&str; 9] = [
    CEX_RESEARCH_AGGREGATE_TRADE_FLOW_IMBALANCE_FIELD,
    "ask_depth_top5",
    "bid_depth_top5",
    "book_imbalance",
    "book_imbalance_top5",
    "near_depth_concentration_skew_top5",
    "spread_bps",
    "vwap_center_deviation_top5_bps",
    "weighted_book_imbalance_top5",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CexCampaignFailureClassV1 {
    NoTradesAfterCosts,
    OvertradeCapacity,
    PositiveIcNegativeNet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CexCampaignPositionPolicyV1 {
    CostAware,
    PredictionIdentity,
    HystereticCostAware,
}

impl CexCampaignPositionPolicyV1 {
    pub(crate) fn decision_policy(self) -> CexSupervisedDecisionPolicyV2 {
        self.decision_policy_for_market(CexResearchMarketV1::Usdm)
    }

    pub(crate) fn decision_policy_for_market(
        self,
        market: CexResearchMarketV1,
    ) -> CexSupervisedDecisionPolicyV2 {
        let policy = match self {
            Self::CostAware => CexSupervisedDecisionPolicyV2::controlled_v2(),
            Self::PredictionIdentity => CexSupervisedDecisionPolicyV2::prediction_identity_v2(),
            Self::HystereticCostAware => CexSupervisedDecisionPolicyV2::hysteretic_cost_aware_v2(),
        };
        if market == CexResearchMarketV1::Spot {
            policy.with_long_only(true)
        } else {
            policy
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CexCampaignResearchDeltaV1 {
    pub(crate) feature_fields: Vec<String>,
    pub(crate) operators: Vec<FactorOperator>,
    pub(crate) windows: Vec<usize>,
    pub(crate) ridge_l2: f64,
    pub(crate) cart_max_depth: usize,
    pub(crate) cart_min_leaf: usize,
}

impl CexCampaignResearchDeltaV1 {
    pub(crate) fn canonical() -> Self {
        Self {
            feature_fields: FEATURE_FIELDS.into_iter().map(str::to_string).collect(),
            operators: vec![
                FactorOperator::Add,
                FactorOperator::Delta,
                FactorOperator::Mul,
                FactorOperator::Sub,
                FactorOperator::ZScore,
            ],
            windows: vec![5, 20],
            ridge_l2: 1.0e-6,
            cart_max_depth: 3,
            cart_min_leaf: 5,
        }
    }

    pub(crate) fn bounded_variants() -> Vec<Self> {
        let canonical = Self::canonical();
        vec![
            Self {
                feature_fields: canonical
                    .feature_fields
                    .iter()
                    .filter(|field| *field != "ask_depth_top5" && *field != "bid_depth_top5")
                    .cloned()
                    .collect(),
                ..canonical.clone()
            },
            Self {
                operators: vec![
                    FactorOperator::Add,
                    FactorOperator::Delta,
                    FactorOperator::Sub,
                    FactorOperator::ZScore,
                ],
                ..canonical.clone()
            },
            Self {
                windows: vec![5, 40],
                ..canonical.clone()
            },
            Self {
                ridge_l2: 1.0e-4,
                ..canonical.clone()
            },
            Self {
                cart_max_depth: 2,
                ..canonical.clone()
            },
            Self {
                cart_min_leaf: 10,
                ..canonical
            },
        ]
    }

    pub(crate) fn gp_template_count(&self) -> anyhow::Result<usize> {
        self.clone()
            .into_domain()
            .gp_template_count()
            .map_err(anyhow::Error::msg)
    }

    fn into_domain(self) -> CexResearchDeltaConfigV1 {
        CexResearchDeltaConfigV1 {
            feature_fields: self.feature_fields,
            operators: self.operators,
            windows: self.windows,
            ridge_l2: self.ridge_l2,
            cart_max_depth: self.cart_max_depth,
            cart_min_leaf: self.cart_min_leaf,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CexCampaignSearchPolicyRevisionV1 {
    pub(crate) schema_version: String,
    pub(crate) revision_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) parent_revision_id: Option<String>,
    pub(crate) position_policy: CexCampaignPositionPolicyV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) research_delta: Option<CexResearchDeltaConfigV1>,
}

impl CexCampaignSearchPolicyRevisionV1 {
    pub(crate) fn canonical() -> Self {
        Self {
            schema_version: SEARCH_POLICY_REVISION_SCHEMA_V1.to_string(),
            revision_id: String::new(),
            parent_revision_id: None,
            position_policy: CexCampaignPositionPolicyV1::CostAware,
            research_delta: None,
        }
        .with_revision_id()
        .expect("canonical CEX Campaign search policy is valid")
    }

    pub(crate) fn new_typed(
        parent_revision_id: Option<String>,
        position_policy: CexCampaignPositionPolicyV1,
        research_delta: CexCampaignResearchDeltaV1,
    ) -> anyhow::Result<Self> {
        let revision = Self {
            schema_version: SEARCH_POLICY_REVISION_SCHEMA_V2.to_string(),
            revision_id: String::new(),
            parent_revision_id,
            position_policy,
            research_delta: Some(research_delta.into_domain()),
        };
        revision.with_revision_id()
    }

    fn with_revision_id(mut self) -> anyhow::Result<Self> {
        self.revision_id = self.expected_revision_id()?;
        self.validate()?;
        Ok(self)
    }

    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        if !matches!(
            self.schema_version.as_str(),
            SEARCH_POLICY_REVISION_SCHEMA_V1 | SEARCH_POLICY_REVISION_SCHEMA_V2
        ) || self.revision_id != self.expected_revision_id()?
            || self
                .parent_revision_id
                .as_ref()
                .is_some_and(|parent| parent.trim().is_empty() || parent == &self.revision_id)
        {
            bail!("CEX Campaign search policy revision is invalid");
        }
        if self.schema_version == SEARCH_POLICY_REVISION_SCHEMA_V1 && self.research_delta.is_some()
        {
            bail!("legacy CEX Campaign search policy cannot carry a research delta");
        }
        if self.schema_version == SEARCH_POLICY_REVISION_SCHEMA_V2 {
            let delta = self
                .research_delta
                .as_ref()
                .context("typed CEX Campaign search policy is missing its research delta")?;
            delta.validate().map_err(anyhow::Error::msg)?;
        }
        self.position_policy
            .decision_policy()
            .validate()
            .map_err(anyhow::Error::msg)
    }

    fn expected_revision_id(&self) -> anyhow::Result<String> {
        let semantic = if self.schema_version == SEARCH_POLICY_REVISION_SCHEMA_V2 {
            serde_json::json!({
                "schema_version": self.schema_version,
                "position_policy": self.position_policy,
                "research_delta": self.research_delta,
            })
        } else {
            serde_json::json!({
                "schema_version": self.schema_version,
                "parent_revision_id": self.parent_revision_id,
                "position_policy": self.position_policy,
            })
        };
        Ok(format!(
            "cex-search-policy-{}",
            canonical_json_hash(&semantic)?
        ))
    }

    fn same_declared_configuration(&self, other: &Self) -> bool {
        self.schema_version == other.schema_version
            && self.revision_id == other.revision_id
            && self.position_policy == other.position_policy
            && self.research_delta == other.research_delta
    }

    pub(crate) fn bounded_allowlist() -> Vec<Self> {
        let canonical = Self::canonical();
        let defaults = CexCampaignResearchDeltaV1::canonical();
        let mut revisions = vec![canonical];
        for position_policy in [
            CexCampaignPositionPolicyV1::PredictionIdentity,
            CexCampaignPositionPolicyV1::HystereticCostAware,
        ] {
            revisions.push(
                Self::new_typed(None, position_policy, defaults.clone())
                    .expect("bounded typed policy is valid"),
            );
        }
        for delta in CexCampaignResearchDeltaV1::bounded_variants() {
            for position_policy in [
                CexCampaignPositionPolicyV1::PredictionIdentity,
                CexCampaignPositionPolicyV1::HystereticCostAware,
            ] {
                revisions.push(
                    Self::new_typed(None, position_policy, delta.clone())
                        .expect("bounded typed policy is valid"),
                );
            }
        }
        revisions
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CexCampaignLearningDirectiveV1 {
    pub(crate) schema_version: String,
    pub(crate) directive_id: String,
    pub(crate) parent_campaign_id: String,
    pub(crate) parent_request_sha256: String,
    pub(crate) parent_campaign_result_sha256: String,
    pub(crate) failure_class: CexCampaignFailureClassV1,
    pub(crate) rollback_policy_revision_id: String,
    pub(crate) search_policy_revision_id: String,
}

impl CexCampaignLearningDirectiveV1 {
    pub(crate) fn new(
        parent: &CexCampaignResearchParentV1,
        failure_class: CexCampaignFailureClassV1,
        rollback_policy_revision_id: String,
        search_policy_revision_id: String,
    ) -> anyhow::Result<Self> {
        let mut directive = Self {
            schema_version: LEARNING_DIRECTIVE_SCHEMA_V1.to_string(),
            directive_id: String::new(),
            parent_campaign_id: parent.campaign_id.clone(),
            parent_request_sha256: parent.request_sha256.clone(),
            parent_campaign_result_sha256: parent.campaign_result_sha256.clone(),
            failure_class,
            rollback_policy_revision_id,
            search_policy_revision_id,
        };
        directive.directive_id = directive.expected_directive_id()?;
        directive.validate()?;
        Ok(directive)
    }

    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        if self.schema_version != LEARNING_DIRECTIVE_SCHEMA_V1
            || self.directive_id != self.expected_directive_id()?
            || !self.parent_campaign_id.starts_with("cex-campaign-")
            || self.rollback_policy_revision_id.trim().is_empty()
            || self.search_policy_revision_id.trim().is_empty()
            || self.rollback_policy_revision_id == self.search_policy_revision_id
            || normalized_sha256("directive parent request", &self.parent_request_sha256)?
                != self.parent_request_sha256
            || normalized_sha256(
                "directive parent Campaign result",
                &self.parent_campaign_result_sha256,
            )? != self.parent_campaign_result_sha256
        {
            bail!("CEX Campaign learning directive is invalid");
        }
        Ok(())
    }

    pub(crate) fn content_hash(&self) -> anyhow::Result<String> {
        self.validate()?;
        canonical_json_hash(self).map_err(anyhow::Error::new)
    }

    fn expected_directive_id(&self) -> anyhow::Result<String> {
        let semantic = serde_json::json!({
            "schema_version": self.schema_version,
            "parent_campaign_id": self.parent_campaign_id,
            "parent_request_sha256": self.parent_request_sha256,
            "parent_campaign_result_sha256": self.parent_campaign_result_sha256,
            "failure_class": self.failure_class,
            "rollback_policy_revision_id": self.rollback_policy_revision_id,
            "search_policy_revision_id": self.search_policy_revision_id,
        });
        Ok(format!(
            "cex-learning-directive-{}",
            canonical_json_hash(&semantic)?
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CexCampaignResearchPlanV1 {
    pub(crate) schema_version: String,
    pub(crate) generation: u8,
    pub(crate) objective: String,
    pub(crate) hypothesis: String,
    pub(crate) focus_field: String,
    pub(crate) feature_fields: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) label_horizon: Option<CampaignLabelHorizonV1>,
    pub(crate) search_policy_revision: CexCampaignSearchPolicyRevisionV1,
    pub(crate) attempted_search_policy_revision_ids: Vec<String>,
    pub(crate) allowed_search_policy_revisions: Vec<CexCampaignSearchPolicyRevisionV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) parent_evidence_signature: Option<CexCampaignResearchEvidenceSignatureV2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) parent: Option<CexCampaignResearchParentV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) learning_directive: Option<CexCampaignLearningDirectiveV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) llm: Option<CexCampaignLlmProvenanceV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CexCampaignResearchEvidenceSignatureV2 {
    pub(crate) schema_version: String,
    pub(crate) campaign_inputs_sha256: String,
    pub(crate) search_policy_revision_id: String,
    pub(crate) feature_fields_sha256: String,
    pub(crate) factor_signatures_sha256: String,
    pub(crate) evaluation_feedback_sha256: String,
}

impl CexCampaignResearchEvidenceSignatureV2 {
    pub(crate) fn new(
        campaign_inputs_sha256: String,
        search_policy_revision_id: String,
        feature_fields_sha256: String,
        factor_signatures_sha256: String,
        evaluation_feedback_sha256: String,
    ) -> anyhow::Result<Self> {
        let signature = Self {
            schema_version: RESEARCH_EVIDENCE_SIGNATURE_SCHEMA_V2.to_string(),
            campaign_inputs_sha256,
            search_policy_revision_id,
            feature_fields_sha256,
            factor_signatures_sha256,
            evaluation_feedback_sha256,
        };
        signature.validate()?;
        Ok(signature)
    }

    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        if self.schema_version != RESEARCH_EVIDENCE_SIGNATURE_SCHEMA_V2
            || normalized_sha256("Campaign inputs", &self.campaign_inputs_sha256)?
                != self.campaign_inputs_sha256
            || !self
                .search_policy_revision_id
                .starts_with("cex-search-policy-")
            || normalized_sha256("Campaign feature fields", &self.feature_fields_sha256)?
                != self.feature_fields_sha256
            || normalized_sha256("Campaign factor signatures", &self.factor_signatures_sha256)?
                != self.factor_signatures_sha256
            || normalized_sha256(
                "Campaign evaluation feedback",
                &self.evaluation_feedback_sha256,
            )? != self.evaluation_feedback_sha256
        {
            bail!("CEX Campaign research evidence signature is invalid");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CexCampaignResearchParentV1 {
    pub(crate) campaign_id: String,
    pub(crate) request_sha256: String,
    pub(crate) campaign_result_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CexCampaignLlmProvenanceV1 {
    pub(crate) provider: String,
    pub(crate) model: String,
    pub(crate) prompt_sha256: String,
    pub(crate) prompt_tokens: u64,
    pub(crate) completion_tokens: u64,
    pub(crate) total_tokens: u64,
}

impl CexCampaignResearchPlanV1 {
    pub(crate) fn canonical() -> Self {
        let search_policy_revision = CexCampaignSearchPolicyRevisionV1::canonical();
        Self {
            schema_version: RESEARCH_PLAN_SCHEMA_V2.to_string(),
            generation: 0,
            objective: "Generate and screen continuous L2 and aggregate-trade microstructure factors, including aggressive trade-flow imbalance, inverse spread, cross-depth pressure consensus, top-five depth concentration, and VWAP-center displacement, then evaluate Ridge and shallow CART with purged walk-forward OOS predictions on Binance USD-M BTCUSDT 1s/h5/top5 under governed dynamic-v4 GP"
                .to_string(),
            hypothesis: "Aggressive trade-flow imbalance, L1 pressure, top-five depth balance, inverse spread, cross-depth pressure consensus, near-touch depth concentration, linearly weighted top-five pressure, and top-five VWAP-center displacement predict the next five one-second BTCUSDT mid-price returns"
                .to_string(),
            focus_field: "book_imbalance_top5".to_string(),
            feature_fields: FEATURE_FIELDS.into_iter().map(str::to_string).collect(),
            label_horizon: None,
            attempted_search_policy_revision_ids: vec![search_policy_revision.revision_id.clone()],
            allowed_search_policy_revisions: CexCampaignSearchPolicyRevisionV1::bounded_allowlist(),
            parent_evidence_signature: None,
            search_policy_revision,
            parent: None,
            learning_directive: None,
            llm: None,
        }
    }

    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        if self.schema_version != RESEARCH_PLAN_SCHEMA_V2 {
            bail!("CEX Campaign research plan schema_version must be {RESEARCH_PLAN_SCHEMA_V2}");
        }
        if let Some(horizon) = &self.label_horizon {
            horizon.validate().map_err(anyhow::Error::msg)?;
        }
        self.search_policy_revision.validate()?;
        let attempted = self
            .attempted_search_policy_revision_ids
            .iter()
            .collect::<BTreeSet<_>>();
        let allowed_revisions = self
            .allowed_search_policy_revisions
            .iter()
            .map(|revision| revision.revision_id.as_str())
            .collect::<BTreeSet<_>>();
        if self.allowed_search_policy_revisions.is_empty()
            || self
                .allowed_search_policy_revisions
                .iter()
                .any(|revision| revision.validate().is_err())
            || self
                .allowed_search_policy_revisions
                .iter()
                .skip(1)
                .any(|revision| revision.research_delta.is_none())
            || allowed_revisions.len() != self.allowed_search_policy_revisions.len()
            || self
                .allowed_search_policy_revisions
                .first()
                .map(|revision| revision.revision_id.as_str())
                != Some(
                    CexCampaignSearchPolicyRevisionV1::canonical()
                        .revision_id
                        .as_str(),
                )
            || !allowed_revisions.contains(self.search_policy_revision.revision_id.as_str())
        {
            bail!("CEX Campaign research plan policy allowlist is invalid");
        }
        if self.attempted_search_policy_revision_ids.len() != usize::from(self.generation) + 1
            || attempted.len() != self.attempted_search_policy_revision_ids.len()
            || self.attempted_search_policy_revision_ids.first()
                != Some(&CexCampaignSearchPolicyRevisionV1::canonical().revision_id)
            || self.attempted_search_policy_revision_ids.last()
                != Some(&self.search_policy_revision.revision_id)
            || self
                .attempted_search_policy_revision_ids
                .iter()
                .any(|revision_id| !revision_id.starts_with("cex-search-policy-"))
            || self
                .attempted_search_policy_revision_ids
                .iter()
                .any(|revision_id| !allowed_revisions.contains(revision_id.as_str()))
            || self
                .search_policy_revision
                .parent_revision_id
                .as_ref()
                .is_some_and(|parent| !attempted.contains(parent))
        {
            bail!("CEX Campaign attempted search policy history is invalid");
        }
        for (label, value) in [
            ("objective", self.objective.as_str()),
            ("hypothesis", self.hypothesis.as_str()),
            ("focus_field", self.focus_field.as_str()),
        ] {
            if value.trim().is_empty() || value.len() > 4_096 || value.chars().any(char::is_control)
            {
                bail!("CEX Campaign research plan {label} is invalid");
            }
        }
        let allowed = FEATURE_FIELDS
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        if self.feature_fields.is_empty()
            || self
                .feature_fields
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
            || self
                .feature_fields
                .iter()
                .any(|field| !allowed.contains(field.as_str()))
            || !self
                .feature_fields
                .iter()
                .any(|field| field == &self.focus_field)
            || self
                .search_policy_revision
                .research_delta
                .as_ref()
                .is_some_and(|delta| {
                    delta.feature_fields != self.feature_fields || delta.validate().is_err()
                })
        {
            bail!("CEX Campaign research plan feature fields are invalid");
        }
        match (&self.parent, &self.learning_directive, &self.llm) {
            (None, None, None)
                if self.generation == 0
                    && self.parent_evidence_signature.is_none()
                    && self.search_policy_revision
                        == CexCampaignSearchPolicyRevisionV1::canonical() => {}
            (Some(parent), Some(directive), llm) => {
                if self.generation == 0 || self.generation > MAX_RESEARCH_PLAN_GENERATION {
                    bail!("CEX Campaign follow-up generation is outside the bounded loop");
                }
                if !parent.campaign_id.starts_with("cex-campaign-")
                    || parent.campaign_id.len() > 63
                    || parent.campaign_id.bytes().any(|byte| {
                        !(byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
                    })
                    || normalized_sha256("parent Campaign request", &parent.request_sha256)?
                        != parent.request_sha256
                    || normalized_sha256("parent Campaign result", &parent.campaign_result_sha256)?
                        != parent.campaign_result_sha256
                {
                    bail!("CEX Campaign research plan parent binding is invalid");
                }
                directive.validate()?;
                self.parent_evidence_signature
                    .as_ref()
                    .context("CEX Campaign follow-up is missing its parent evidence signature")?
                    .validate()?;
                let expected_position_policy = match directive.failure_class {
                    CexCampaignFailureClassV1::NoTradesAfterCosts => {
                        CexCampaignPositionPolicyV1::PredictionIdentity
                    }
                    CexCampaignFailureClassV1::OvertradeCapacity => {
                        CexCampaignPositionPolicyV1::HystereticCostAware
                    }
                    CexCampaignFailureClassV1::PositiveIcNegativeNet => {
                        CexCampaignPositionPolicyV1::HystereticCostAware
                    }
                };
                if directive.parent_campaign_id != parent.campaign_id
                    || directive.parent_request_sha256 != parent.request_sha256
                    || directive.parent_campaign_result_sha256 != parent.campaign_result_sha256
                    || directive.search_policy_revision_id
                        != self.search_policy_revision.revision_id
                    || self.search_policy_revision.parent_revision_id.as_deref()
                        != Some(directive.rollback_policy_revision_id.as_str())
                    || self.search_policy_revision.position_policy != expected_position_policy
                    || self
                        .allowed_search_policy_revisions
                        .iter()
                        .find(|revision| {
                            revision.revision_id == self.search_policy_revision.revision_id
                        })
                        .is_none_or(|revision| {
                            !revision.same_declared_configuration(&self.search_policy_revision)
                        })
                {
                    bail!("CEX Campaign learning directive does not bind its policy revision");
                }
                if let Some(llm) = llm {
                    for (label, value) in [
                        ("LLM provider", llm.provider.as_str()),
                        ("LLM model", llm.model.as_str()),
                    ] {
                        if value.trim().is_empty()
                            || value.len() > 256
                            || value.chars().any(char::is_control)
                        {
                            bail!("CEX Campaign research plan {label} is invalid");
                        }
                    }
                    if normalized_sha256("LLM prompt", &llm.prompt_sha256)? != llm.prompt_sha256
                        || llm.prompt_tokens == 0
                        || llm.completion_tokens == 0
                        || llm.total_tokens
                            != llm.prompt_tokens.saturating_add(llm.completion_tokens)
                    {
                        bail!("CEX Campaign research plan LLM provenance is invalid");
                    }
                }
            }
            _ => bail!("CEX Campaign follow-up requires parent and learning directive"),
        }
        Ok(())
    }

    pub(crate) fn content_hash(&self) -> anyhow::Result<String> {
        self.validate()?;
        canonical_json_hash(self).map_err(anyhow::Error::new)
    }

    pub(crate) fn max_candidates(&self) -> anyhow::Result<usize> {
        self.validate()?;
        self.search_policy_revision
            .research_delta
            .as_ref()
            .map_or_else(
                || {
                    self.feature_fields
                        .len()
                        .checked_mul(2)
                        .and_then(|count| count.checked_add(4))
                        .context("CEX Campaign research plan candidate budget overflowed")
                },
                |delta| {
                    CexCampaignResearchDeltaV1 {
                        feature_fields: delta.feature_fields.clone(),
                        operators: delta.operators.clone(),
                        windows: delta.windows.clone(),
                        ridge_l2: delta.ridge_l2,
                        cart_max_depth: delta.cart_max_depth,
                        cart_min_leaf: delta.cart_min_leaf,
                    }
                    .gp_template_count()
                },
            )
    }
}

pub(crate) fn allowed_research_feature_fields() -> Vec<String> {
    FEATURE_FIELDS.into_iter().map(str::to_string).collect()
}

#[derive(Debug)]
pub(crate) struct RenderedCexMission {
    pub(crate) mission: CexResearchMissionArtifactV1,
    pub(crate) mission_id: String,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct RenderedHoldoutPolicyV1 {
    schema_version: &'static str,
    holdout_id: String,
    max_opens: u8,
    state: CexResearchHoldoutStateV1,
}

fn sealed_holdout_cohort_sha256(
    materialization: &crate::mission_runner::Materialization,
) -> anyhow::Result<String> {
    Ok(canonical_json_hash(&serde_json::json!({
        "venue": materialization.snapshot.venue,
        "market": materialization.market,
        "symbol": materialization.symbol,
        "source_revision": materialization.source_revision,
        "source_segments": materialization.snapshot.source_segments,
        "bucket_ms": materialization.bucket_ms,
        "label_horizon_buckets": materialization.label_horizon_buckets,
        "rows": materialization.rows,
        "first_event_time": materialization.snapshot.first_event_time,
        "last_event_time": materialization.snapshot.last_event_time,
        "sealed_holdout_rows": HOLDOUT_ROWS,
    }))?)
}

#[allow(dead_code)]
pub(crate) fn render_cex_bundle(
    feature: &Path,
    materialization_path: &Path,
    research_plan: &CexCampaignResearchPlanV1,
    seed: u64,
    multiple_testing_trials: usize,
) -> anyhow::Result<RenderedCexMission> {
    research_plan.validate()?;
    validate_input_sizes(feature, materialization_path)?;
    let feature_sha256 = sha256_file(feature)?;
    let materialization_sha256 = sha256_file(materialization_path)?;
    let materialization = decode_materialization(
        &std::fs::read(materialization_path)
            .with_context(|| format!("read {}", materialization_path.display()))?,
    )?;
    let research_market = rendered_research_market(&materialization)?;
    let feature_artifacts = tempfile::tempdir().context("create feature validation directory")?;
    let feature_manifest = import_feature_manifest(
        &materialization.mission_id,
        feature,
        feature_artifacts.path(),
    )?;
    ensure_materialization_scope(
        &materialization,
        &feature_manifest,
        &feature_sha256,
        research_plan.label_horizon.as_ref(),
    )?;
    data_mission::validate_cex_replay_features(&materialization.snapshot, &feature_manifest)?;
    let validation = approved_validation(&materialization)?;
    validate_materialization(&materialization, &feature_sha256, &validation)?;
    let dataset = CexReplayDatasetManifestV5::new(
        feature_manifest.manifest_id.clone(),
        materialization.snapshot.clone(),
    )
    .context("construct CEX replay dataset manifest")?;
    let dataset_sha256 = canonical_json_hash(&dataset)?;
    let feature_sha256 = normalized_sha256("feature", &feature_sha256)?;
    let materialization_sha256 = normalized_sha256("materialization", &materialization_sha256)?;
    let snapshot_sha256 = materialization.snapshot.sha256();
    let frozen_input_sha256 = canonical_json_hash(&serde_json::json!({
        "stable_version": STABLE_VERSION,
        "feature_sha256": feature_sha256,
        "materialization_sha256": materialization_sha256,
        "snapshot_sha256": snapshot_sha256,
        "source_revision": materialization.source_revision,
    }))?;
    let sealed_holdout_cohort_sha256 = sealed_holdout_cohort_sha256(&materialization)?;
    let research_plan_sha256 = research_plan.content_hash()?;
    let stable_version = if research_plan == &CexCampaignResearchPlanV1::canonical() {
        STABLE_VERSION.to_string()
    } else {
        format!("{STABLE_VERSION}-plan-{}", &research_plan_sha256[..16])
    };
    let gp_policy_id = if research_plan == &CexCampaignResearchPlanV1::canonical() {
        GP_POLICY_ID.to_string()
    } else {
        format!("{GP_POLICY_ID}-{}", &research_plan_sha256[..16])
    };
    let hypothesis_id = if research_plan == &CexCampaignResearchPlanV1::canonical() {
        STABLE_HYPOTHESIS_ID.to_string()
    } else {
        format!("l2-microstructure-followup-{}", &research_plan_sha256[..16])
    };
    let search_lineage_id = format!(
        "{stable_version}-lineage-seed-{seed}-{}",
        &frozen_input_sha256[..16]
    );
    let input_lineage_id = format!("{stable_version}-input-{}", &materialization_sha256[..16]);
    let holdout_id = format!("cex-holdout-{}", &sealed_holdout_cohort_sha256[..48]);
    let evaluation_protocol = approved_evaluation_protocol_for_horizon(
        &materialization,
        research_plan.label_horizon.as_ref(),
    )?;
    let search = CexResearchSearchPlanV1 {
        seed,
        budget: SearchBudget {
            max_candidates: research_plan.max_candidates()?,
            max_expansions: MAX_EXPANSIONS,
            max_tokens: 0,
            max_seconds: 0,
        },
        max_new_iterations: research_plan.max_candidates()?,
        multiple_testing_trials,
    };
    let gp_policy = if let Some(delta) = &research_plan.search_policy_revision.research_delta {
        CexGpPolicyV1::controlled_dynamic_v5(
            gp_policy_id,
            delta.feature_fields.clone(),
            delta.operators.clone(),
            delta.windows.clone(),
            search.seed,
            &search.budget,
        )?
    } else {
        CexGpPolicyV1::controlled_dynamic_v4(
            gp_policy_id,
            research_plan.feature_fields.clone(),
            search.seed,
            &search.budget,
        )?
    };
    let gp_policy_content_sha256 = gp_policy.content_hash()?;
    let baseline_policy = research_plan
        .search_policy_revision
        .research_delta
        .as_ref()
        .map(|delta| {
            CexBaselinePolicyV1::controlled_v2(
                BASELINE_POLICY_ID,
                delta.ridge_l2,
                delta.cart_max_depth,
                delta.cart_min_leaf,
            )
        })
        .transpose()?
        .unwrap_or(CexBaselinePolicyV1::controlled_v1(BASELINE_POLICY_ID)?);
    let supervised_decision_policy = research_plan
        .search_policy_revision
        .position_policy
        .decision_policy_for_market(research_market.clone());
    let weight_policy = CexEqualAbsoluteWeightPolicyV1::controlled_v1(WEIGHT_POLICY_ID)?;
    let replay_policy = CexEventReplayPolicyV1::controlled_v2(
        REPLAY_POLICY_ID,
        materialization.top_depth,
        materialization.bucket_ms,
    )?;
    let holdout_policy = RenderedHoldoutPolicyV1 {
        schema_version: "cex-holdout-policy-v1",
        holdout_id: holdout_id.clone(),
        max_opens: 1,
        state: CexResearchHoldoutStateV1::Unopened,
    };
    let holdout_policy_content_sha256 = canonical_json_hash(&holdout_policy)?;
    let partition_sha256 = canonical_json_hash(&materialization.snapshot.source_segments)?;
    let mission = CexResearchMissionArtifactV1 {
        schema_version: CEX_RESEARCH_MISSION_SCHEMA_V1.to_string(),
        spec: CexResearchMissionSpecV1 {
            objective: research_plan.objective.clone(),
            search_lineage_id,
            data_mission_id: materialization.mission_id.clone(),
            instrument: CexResearchInstrumentV1 {
                venue: CexResearchVenueV1::Binance,
                market: research_market,
                symbol: materialization.symbol.clone(),
                horizon: EvaluationLabelSpecV1 {
                    horizon_buckets: materialization.label_horizon_buckets,
                    observation_frequency_millis: materialization.bucket_ms,
                },
            },
            hypotheses: vec![CexResearchHypothesisV1 {
                hypothesis_id,
                statement: research_plan.hypothesis.clone(),
                target: CexResearchHypothesisTargetV1 {
                    name: "forward_mid_return".to_string(),
                    horizon: EvaluationLabelSpecV1 {
                        horizon_buckets: materialization.label_horizon_buckets,
                        observation_frequency_millis: materialization.bucket_ms,
                    },
                },
                required_feature_families: research_plan.feature_fields.clone(),
                required_template_families: if research_plan
                    .search_policy_revision
                    .research_delta
                    .is_some()
                {
                    vec![
                        "parameterized_gp_atomic".to_string(),
                        "parameterized_gp_binary".to_string(),
                    ]
                } else {
                    vec![
                        "atomic_l2_microstructure".to_string(),
                        "named_composite_l2_microstructure".to_string(),
                    ]
                },
                falsification_tests: vec![
                    CexResearchFalsificationTestV1 {
                        test_id: "purged-predictive-gate".to_string(),
                        reject_when: "the governed purged walk-forward IC, RankIC, ICIR, or positive-fold thresholds fail".to_string(),
                    },
                    CexResearchFalsificationTestV1 {
                        test_id: "post-cost-capacity-gate".to_string(),
                        reject_when: "declared post-cost return, drawdown, trade-count, or top-five-depth capacity evidence fails".to_string(),
                    },
                ],
                source_evidence_ids: vec!["input-materialization-evidence".to_string()],
            }],
            inputs: CexResearchInputBindingsV1 {
                dataset: CexResearchContentRefV1 {
                    id: dataset.manifest_id.clone(),
                    content_sha256: dataset_sha256,
                },
                snapshot: CexResearchContentRefV1 {
                    id: format!("cex-replay-snapshot-{snapshot_sha256}"),
                    content_sha256: snapshot_sha256,
                },
                partition: CexResearchContentRefV1 {
                    id: format!("cex-replay-partition-{partition_sha256}"),
                    content_sha256: partition_sha256,
                },
                source: CexResearchContentRefV1 {
                    id: materialization.source_revision.clone(),
                    content_sha256: materialization.source_revision.clone(),
                },
                feature: CexResearchContentRefV1 {
                    id: feature_manifest.manifest_id.clone(),
                    content_sha256: feature_sha256.clone(),
                },
                materialization: CexResearchContentRefV1 {
                    id: materialization.mission_id.clone(),
                    content_sha256: materialization_sha256.clone(),
                },
            },
            policies: CexResearchPolicyBindingsV1 {
                gp: CexResearchContentRefV1 {
                    id: gp_policy.policy_id.clone(),
                    content_sha256: gp_policy_content_sha256.clone(),
                },
                screening: CexResearchContentRefV1 {
                    id: SCREENING_POLICY_ID.to_string(),
                    content_sha256: canonical_json_hash(
                        &alpha_domain::FormulaEvaluatorConfig::for_trials(
                            search.planned_gp_and_subset_trials()?,
                        )?,
                    )?,
                },
                baseline: CexResearchContentRefV1 {
                    id: baseline_policy.policy_id.clone(),
                    content_sha256: baseline_policy.content_hash()?,
                },
                supervised_decision: CexResearchContentRefV1 {
                    id: research_plan.search_policy_revision.revision_id.clone(),
                    content_sha256: supervised_decision_policy
                        .content_hash()
                        .map_err(anyhow::Error::msg)?,
                },
                subset_search: CexResearchContentRefV1 {
                    id: SUBSET_POLICY_ID.to_string(),
                    content_sha256: canonical_json_hash(&search)?,
                },
                weight: CexResearchContentRefV1 {
                    id: weight_policy.policy_id.clone(),
                    content_sha256: weight_policy.content_hash()?,
                },
                evaluation: CexResearchContentRefV1 {
                    id: EVALUATION_POLICY_ID.to_string(),
                    content_sha256: evaluation_protocol.content_hash()?,
                },
                replay: CexResearchContentRefV1 {
                    id: replay_policy.policy_id.clone(),
                    content_sha256: replay_policy.content_hash()?,
                },
                holdout: CexResearchContentRefV1 {
                    id: HOLDOUT_POLICY_ID.to_string(),
                    content_sha256: holdout_policy_content_sha256.clone(),
                },
            },
            evidence: vec![CexResearchEvidenceRefV1 {
                evidence_id: "input-materialization-evidence".to_string(),
                kind: CexResearchEvidenceKindV1::TrainingValidation,
                source_mission_id: materialization.mission_id.clone(),
                source_search_lineage_id: input_lineage_id,
                artifact_sha256: materialization_sha256.clone(),
                signature: None,
                holdout_id: None,
            }],
            feature_fields: research_plan.feature_fields.clone(),
            research_delta: research_plan
                .search_policy_revision
                .research_delta
                .clone(),
            search,
            evaluation_protocol,
            holdout: CexResearchHoldoutV1 {
                holdout_id,
                state: CexResearchHoldoutStateV1::Unopened,
            },
        },
        operational: CexResearchOperationalMetadataV1::default(),
    };
    mission.validate()?;
    let mission_id = mission.semantic_id()?;
    Ok(RenderedCexMission {
        mission,
        mission_id,
    })
}

fn validate_input_sizes(feature: &Path, materialization: &Path) -> anyhow::Result<()> {
    if feature.metadata()?.len() > MAX_FEATURE_BYTES
        || materialization.metadata()?.len() > MAX_MATERIALIZATION_BYTES
    {
        bail!("source exceeds the allowed size");
    }
    Ok(())
}

fn import_feature_manifest(
    mission_id: &str,
    input: &Path,
    artifact_dir: &Path,
) -> anyhow::Result<FeatureDatasetManifest> {
    import_feature_dataset(mission_id.to_string(), input, artifact_dir)
        .map_err(anyhow::Error::msg)
        .with_context(|| format!("import feature dataset from {}", input.display()))
}

pub(crate) fn approved_validation(
    materialization: &crate::mission_runner::Materialization,
) -> anyhow::Result<ValidationArgs> {
    Ok(ValidationArgs {
        initial_train_rows: INITIAL_TRAIN_ROWS,
        validation_rows: VALIDATION_ROWS,
        fold_count: FOLD_COUNT,
        purge_rows: PURGE_ROWS,
        embargo_rows: EMBARGO_ROWS,
        sealed_holdout_rows: HOLDOUT_ROWS,
        independent_selection_rows: Some(SELECTION_ROWS),
        fee_bps: 2.0,
        rebate_bps: 0.0,
        funding_bps: 0.0,
        latency_bps: 0.5,
        slippage_bps: 0.0,
        cross_spread: true,
        position_notional_usd: 1_000.0,
        capacity_depth_levels: 5,
        max_book_depth_fraction: 0.05,
        label_horizon_buckets: materialization.label_horizon_buckets,
        observation_frequency_millis: materialization.bucket_ms,
    })
}

#[cfg(test)]
pub(crate) fn approved_evaluation_protocol(
    materialization: &crate::mission_runner::Materialization,
) -> anyhow::Result<EvaluationProtocolV1> {
    approved_evaluation_protocol_for_horizon(materialization, None)
}

pub(crate) fn approved_evaluation_protocol_for_horizon(
    materialization: &crate::mission_runner::Materialization,
    expected_horizon: Option<&CampaignLabelHorizonV1>,
) -> anyhow::Result<EvaluationProtocolV1> {
    let (labels, purge_rows, embargo_rows) = if let Some(horizon) = expected_horizon {
        horizon.validate().map_err(anyhow::Error::msg)?;
        if horizon.labels.horizon_buckets != materialization.label_horizon_buckets
            || horizon.labels.observation_frequency_millis != materialization.bucket_ms
        {
            bail!("typed Campaign label horizon does not match materialization");
        }
        (
            horizon.labels.clone(),
            horizon.purge_rows,
            horizon.embargo_rows,
        )
    } else {
        (
            EvaluationLabelSpecV1 {
                horizon_buckets: materialization.label_horizon_buckets,
                observation_frequency_millis: materialization.bucket_ms,
            },
            PURGE_ROWS,
            EMBARGO_ROWS,
        )
    };
    EvaluationProtocolV1::new(
        EvaluationWalkForwardV1 {
            initial_train_rows: INITIAL_TRAIN_ROWS,
            validation_rows: VALIDATION_ROWS,
            fold_count: FOLD_COUNT,
            purge_rows,
            embargo_rows,
            sealed_holdout_rows: HOLDOUT_ROWS,
        },
        EvaluationCostsV1 {
            fee_bps: 2.0,
            rebate_bps: 0.0,
            funding_bps: 0.0,
            latency_bps: 0.5,
            slippage_bps: 0.0,
            cross_spread: true,
            position_notional_usd: 1_000.0,
            capacity_depth_levels: 5,
            max_book_depth_fraction: 0.05,
        },
        labels,
    )
    .and_then(|protocol| protocol.with_independent_selection(SELECTION_ROWS))
    .map_err(anyhow::Error::new)
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn validate_render_materialization_scope(
    materialization: &crate::mission_runner::Materialization,
) -> anyhow::Result<()> {
    validate_render_materialization_scope_for_horizon(materialization, None)?;
    Ok(())
}

pub(crate) fn validate_render_materialization_scope_for_horizon(
    materialization: &crate::mission_runner::Materialization,
    expected_horizon: Option<&CampaignLabelHorizonV1>,
) -> anyhow::Result<()> {
    if !matches!(materialization.market.as_str(), "spot" | "usdm")
        || materialization.symbol != "BTCUSDT"
        || materialization.bucket_ms != 1_000
        || materialization.top_depth != 5
    {
        bail!("only the approved Binance Spot or USD-M BTCUSDT 1s/h5/top5 materialization can render this Mission");
    }
    if let Some(horizon) = expected_horizon {
        horizon.validate().map_err(anyhow::Error::msg)?;
        if horizon.labels.horizon_buckets != materialization.label_horizon_buckets
            || horizon.labels.observation_frequency_millis != materialization.bucket_ms
        {
            bail!("typed Campaign label horizon does not match materialization");
        }
    } else if materialization.label_horizon_buckets != 5 {
        bail!("only the approved Binance Spot or USD-M BTCUSDT 1s/h5/top5 materialization can render this Mission");
    }
    let minimum_rows = expected_horizon
        .map(minimum_rows_for_horizon)
        .transpose()?
        .unwrap_or(MIN_ROWS);
    if materialization.rows < minimum_rows {
        bail!("approved Mission render requires at least {minimum_rows} point-in-time rows");
    }
    Ok(())
}

fn rendered_research_market(
    materialization: &crate::mission_runner::Materialization,
) -> anyhow::Result<CexResearchMarketV1> {
    match materialization.market.as_str() {
        "spot" => Ok(CexResearchMarketV1::Spot),
        "usdm" => Ok(CexResearchMarketV1::Usdm),
        other => bail!("unsupported CEX materialization market: {other}"),
    }
}

fn minimum_rows_for_horizon(horizon: &CampaignLabelHorizonV1) -> anyhow::Result<usize> {
    let fold_rows = VALIDATION_ROWS
        .checked_add(horizon.embargo_rows)
        .and_then(|rows| FOLD_COUNT.checked_mul(rows))
        .context("typed Campaign horizon row budget overflowed")?;
    INITIAL_TRAIN_ROWS
        .checked_add(fold_rows)
        .and_then(|rows| rows.checked_add(horizon.purge_rows))
        .and_then(|rows| rows.checked_add(SELECTION_ROWS))
        .and_then(|rows| rows.checked_add(horizon.purge_rows.checked_mul(2)?))
        .and_then(|rows| rows.checked_add(HOLDOUT_ROWS))
        .context("typed Campaign horizon row budget overflowed")
}

fn ensure_materialization_scope(
    materialization: &crate::mission_runner::Materialization,
    manifest: &FeatureDatasetManifest,
    feature_sha256: &str,
    expected_horizon: Option<&CampaignLabelHorizonV1>,
) -> anyhow::Result<()> {
    validate_render_materialization_scope_for_horizon(materialization, expected_horizon)?;
    let minimum_rows = expected_horizon
        .map(minimum_rows_for_horizon)
        .transpose()?
        .unwrap_or(MIN_ROWS);
    if manifest.rows < minimum_rows {
        bail!("approved Mission render requires at least {minimum_rows} point-in-time rows");
    }
    if materialization.rows != manifest.rows {
        bail!("materialization evidence row count does not match the feature artifact");
    }
    if materialization.series_count != manifest.series_count {
        bail!("materialization evidence series count does not match the feature artifact");
    }
    if materialization.artifact_sha256 != feature_sha256
        || materialization.snapshot.feature_artifact_sha256 != feature_sha256
    {
        bail!("materialization evidence does not bind the supplied feature artifact");
    }
    if manifest.manifest_id != format!("dataset-{feature_sha256}")
        || manifest.artifact_sha256 != feature_sha256
    {
        bail!("feature manifest does not bind the supplied feature artifact");
    }
    for field in FEATURE_FIELDS.into_iter().chain(["mid_price"]) {
        if !manifest.feature_names.iter().any(|name| name == field) {
            bail!("approved Mission render requires feature field {field}");
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use chrono::{Duration as ChronoDuration, Utc};
    use hft_collector::{DataModality, PointInTimeFeatureRow};
    use sha2::{Digest, Sha256};
    use std::{
        collections::{BTreeMap, BTreeSet},
        path::PathBuf,
    };

    fn default_trials() -> usize {
        CexCampaignResearchPlanV1::canonical()
            .max_candidates()
            .unwrap()
            * 2
    }

    #[test]
    fn cloud_renderer_builds_an_l2_factor_plan_v5_mission() {
        let fixture = Fixture::new(MIN_ROWS);
        let rendered = render_cex_bundle(
            &fixture.feature_path,
            &fixture.materialization_path,
            &CexCampaignResearchPlanV1::canonical(),
            7,
            default_trials(),
        )
        .unwrap();
        let mission = rendered.mission;
        assert_eq!(MIN_ROWS, 25_245);
        assert_eq!(mission.spec.instrument.market, CexResearchMarketV1::Usdm);
        assert_eq!(
            mission.spec.policies.supervised_decision.content_sha256,
            CexCampaignPositionPolicyV1::CostAware
                .decision_policy()
                .content_hash()
                .unwrap()
        );
        let partitions = mission
            .spec
            .evaluation_protocol
            .row_partitions(MIN_ROWS)
            .unwrap();
        assert_eq!(partitions.search.end, 18_025);
        assert_eq!(partitions.selection, Some(18_035..21_635));
        assert_eq!(partitions.sealed_holdout, 21_645..25_245);
        let reconstructed = ValidationArgs::from_protocol(&mission.spec.evaluation_protocol)
            .evaluation_protocol(&mission.spec.evaluation_protocol.labels)
            .unwrap();
        assert_eq!(reconstructed, mission.spec.evaluation_protocol);
        assert_eq!(mission.spec.search.budget.max_candidates, 22);
        assert_eq!(
            mission.spec.search.planned_gp_and_subset_trials().unwrap(),
            44
        );
        assert_eq!(mission.spec.search.budget.max_expansions, MAX_EXPANSIONS);
        assert_eq!(
            mission.spec.evaluation_protocol.walk_forward,
            EvaluationWalkForwardV1 {
                initial_train_rows: INITIAL_TRAIN_ROWS,
                validation_rows: VALIDATION_ROWS,
                fold_count: FOLD_COUNT,
                purge_rows: PURGE_ROWS,
                embargo_rows: EMBARGO_ROWS,
                sealed_holdout_rows: HOLDOUT_ROWS,
            }
        );
        assert_eq!(mission.spec.evaluation_protocol.costs.fee_bps, 2.0);
        assert_eq!(mission.spec.evaluation_protocol.costs.funding_bps, 0.0);
        assert_eq!(
            mission.spec.evaluation_protocol.costs.position_notional_usd,
            1_000.0
        );
        assert_eq!(
            mission.spec.evaluation_protocol.costs.capacity_depth_levels,
            5
        );
        assert_eq!(
            mission
                .spec
                .evaluation_protocol
                .costs
                .max_book_depth_fraction,
            0.05
        );
        assert!(mission.spec.evaluation_protocol.costs.cross_spread);
        assert_eq!(
            mission.spec.feature_fields,
            FEATURE_FIELDS.map(str::to_string)
        );
        assert_eq!(
            mission.spec.hypotheses[0].hypothesis_id,
            STABLE_HYPOTHESIS_ID
        );
        assert_eq!(
            mission.spec.hypotheses[0].required_template_families,
            vec![
                "atomic_l2_microstructure".to_string(),
                "named_composite_l2_microstructure".to_string(),
            ]
        );
        assert_eq!(mission.spec.policies.gp.id, GP_POLICY_ID);
        let expected_gp = CexGpPolicyV1::controlled_dynamic_v4(
            mission.spec.policies.gp.id.clone(),
            mission.spec.feature_fields.clone(),
            mission.spec.search.seed,
            &mission.spec.search.budget,
        )
        .unwrap();
        expected_gp
            .validate_binding(&mission.spec.policies.gp)
            .unwrap();
        assert!(mission.spec.search_lineage_id.starts_with(STABLE_VERSION));
    }

    #[test]
    fn follow_up_plan_binds_one_registered_position_policy_revision() {
        let fixture = Fixture::new(MIN_ROWS);
        let canonical = CexCampaignResearchPlanV1::canonical();
        let parent = CexCampaignResearchParentV1 {
            campaign_id: format!("cex-campaign-{}", "1".repeat(32)),
            request_sha256: "2".repeat(64),
            campaign_result_sha256: "3".repeat(64),
        };
        let search_policy_revision = CexCampaignSearchPolicyRevisionV1::new_typed(
            Some(canonical.search_policy_revision.revision_id.clone()),
            CexCampaignPositionPolicyV1::PredictionIdentity,
            CexCampaignResearchDeltaV1::canonical(),
        )
        .unwrap();
        let learning_directive = CexCampaignLearningDirectiveV1::new(
            &parent,
            CexCampaignFailureClassV1::NoTradesAfterCosts,
            canonical.search_policy_revision.revision_id.clone(),
            search_policy_revision.revision_id.clone(),
        )
        .unwrap();
        let plan = CexCampaignResearchPlanV1 {
            schema_version: RESEARCH_PLAN_SCHEMA_V2.to_string(),
            generation: 1,
            objective: "Test one bounded follow-up".to_string(),
            hypothesis: "Cost-filtered predictions should be tested through one registered mapping"
                .to_string(),
            focus_field: canonical.focus_field,
            feature_fields: canonical.feature_fields,
            label_horizon: None,
            search_policy_revision,
            attempted_search_policy_revision_ids: vec![
                canonical.search_policy_revision.revision_id.clone(),
                learning_directive.search_policy_revision_id.clone(),
            ],
            allowed_search_policy_revisions: canonical.allowed_search_policy_revisions,
            parent_evidence_signature: Some(
                CexCampaignResearchEvidenceSignatureV2::new(
                    "5".repeat(64),
                    format!("cex-search-policy-{}", "6".repeat(64)),
                    "7".repeat(64),
                    "8".repeat(64),
                    "9".repeat(64),
                )
                .unwrap(),
            ),
            parent: Some(parent),
            learning_directive: Some(learning_directive),
            llm: Some(CexCampaignLlmProvenanceV1 {
                provider: "test".to_string(),
                model: "test".to_string(),
                prompt_sha256: "4".repeat(64),
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            }),
        };
        let rendered = render_cex_bundle(
            &fixture.feature_path,
            &fixture.materialization_path,
            &plan,
            7,
            plan.max_candidates().unwrap() * 2,
        )
        .unwrap();

        assert_eq!(rendered.mission.spec.objective, plan.objective);
        assert_eq!(rendered.mission.spec.feature_fields, plan.feature_fields);
        assert_eq!(
            rendered.mission.spec.search.budget.max_candidates,
            CexCampaignResearchDeltaV1::canonical()
                .gp_template_count()
                .unwrap()
        );
        assert_eq!(
            rendered
                .mission
                .spec
                .research_delta
                .as_ref()
                .unwrap()
                .cart_max_depth,
            3
        );
        assert_ne!(rendered.mission.spec.policies.gp.id, GP_POLICY_ID);
        assert_eq!(
            rendered.mission.spec.policies.supervised_decision.id,
            plan.search_policy_revision.revision_id
        );

        let mut deterministic_plan = plan.clone();
        deterministic_plan.llm = None;
        let deterministic = render_cex_bundle(
            &fixture.feature_path,
            &fixture.materialization_path,
            &deterministic_plan,
            7,
            deterministic_plan.max_candidates().unwrap() * 2,
        )
        .unwrap();
        assert_eq!(
            deterministic.mission.spec.policies.supervised_decision,
            rendered.mission.spec.policies.supervised_decision
        );

        let mut invalid = plan.clone();
        invalid.feature_fields.push("open_interest".to_string());
        assert!(invalid.validate().is_err());

        let mut exhausted = plan;
        exhausted.generation = MAX_RESEARCH_PLAN_GENERATION + 1;
        assert!(exhausted.validate().is_err());
    }

    #[test]
    fn typed_revision_hash_binds_every_delta_dimension_but_not_parent() {
        let delta = CexCampaignResearchDeltaV1::canonical();
        let first = CexCampaignSearchPolicyRevisionV1::new_typed(
            None,
            CexCampaignPositionPolicyV1::HystereticCostAware,
            delta.clone(),
        )
        .unwrap();
        let child = CexCampaignSearchPolicyRevisionV1::new_typed(
            Some(CexCampaignSearchPolicyRevisionV1::canonical().revision_id),
            CexCampaignPositionPolicyV1::HystereticCostAware,
            delta.clone(),
        )
        .unwrap();
        assert_eq!(first.revision_id, child.revision_id);

        for changed in [
            CexCampaignResearchDeltaV1 {
                feature_fields: delta
                    .feature_fields
                    .iter()
                    .filter(|field| *field != "ask_depth_top5")
                    .cloned()
                    .collect(),
                ..delta.clone()
            },
            CexCampaignResearchDeltaV1 {
                operators: vec![
                    FactorOperator::Add,
                    FactorOperator::Delta,
                    FactorOperator::Sub,
                    FactorOperator::ZScore,
                ],
                ..delta.clone()
            },
            CexCampaignResearchDeltaV1 {
                windows: vec![5, 40],
                ..delta.clone()
            },
            CexCampaignResearchDeltaV1 {
                ridge_l2: 1.0e-4,
                ..delta.clone()
            },
            CexCampaignResearchDeltaV1 {
                cart_max_depth: 2,
                ..delta.clone()
            },
            CexCampaignResearchDeltaV1 {
                cart_min_leaf: 10,
                ..delta.clone()
            },
        ] {
            let revision = CexCampaignSearchPolicyRevisionV1::new_typed(
                None,
                CexCampaignPositionPolicyV1::HystereticCostAware,
                changed,
            )
            .unwrap();
            assert_ne!(revision.revision_id, first.revision_id);
        }
    }

    #[test]
    fn feature_subset_delta_renders_v5_mission_and_parameterized_baseline() {
        let fixture = Fixture::new(MIN_ROWS);
        let canonical = CexCampaignResearchPlanV1::canonical();
        assert_eq!(canonical.allowed_search_policy_revisions.len(), 15);
        let subset_revision = CexCampaignSearchPolicyRevisionV1::bounded_allowlist()
            .into_iter()
            .find(|revision| {
                revision.position_policy == CexCampaignPositionPolicyV1::HystereticCostAware
                    && revision
                        .research_delta
                        .as_ref()
                        .is_some_and(|delta| delta.feature_fields.len() < 9)
            })
            .unwrap();
        let subset = subset_revision.research_delta.clone().unwrap();
        let parent = CexCampaignResearchParentV1 {
            campaign_id: format!("cex-campaign-{}", "1".repeat(32)),
            request_sha256: "2".repeat(64),
            campaign_result_sha256: "3".repeat(64),
        };
        let revision = CexCampaignSearchPolicyRevisionV1::new_typed(
            Some(canonical.search_policy_revision.revision_id.clone()),
            CexCampaignPositionPolicyV1::HystereticCostAware,
            CexCampaignResearchDeltaV1 {
                feature_fields: subset.feature_fields.clone(),
                operators: subset.operators.clone(),
                windows: subset.windows.clone(),
                ridge_l2: subset.ridge_l2,
                cart_max_depth: subset.cart_max_depth,
                cart_min_leaf: subset.cart_min_leaf,
            },
        )
        .unwrap();
        let directive = CexCampaignLearningDirectiveV1::new(
            &parent,
            CexCampaignFailureClassV1::OvertradeCapacity,
            canonical.search_policy_revision.revision_id.clone(),
            revision.revision_id.clone(),
        )
        .unwrap();
        let plan = CexCampaignResearchPlanV1 {
            schema_version: RESEARCH_PLAN_SCHEMA_V2.to_string(),
            generation: 1,
            objective: "Test a declared feature subset".to_string(),
            hypothesis: "The subset remains evaluable under the same costs".to_string(),
            focus_field: canonical.focus_field.clone(),
            feature_fields: subset.feature_fields.clone(),
            label_horizon: None,
            search_policy_revision: revision.clone(),
            attempted_search_policy_revision_ids: vec![
                canonical.search_policy_revision.revision_id.clone(),
                revision.revision_id.clone(),
            ],
            allowed_search_policy_revisions: canonical.allowed_search_policy_revisions.clone(),
            parent_evidence_signature: Some(
                CexCampaignResearchEvidenceSignatureV2::new(
                    "5".repeat(64),
                    canonical.search_policy_revision.revision_id.clone(),
                    "7".repeat(64),
                    "8".repeat(64),
                    "9".repeat(64),
                )
                .unwrap(),
            ),
            parent: Some(parent),
            learning_directive: Some(directive),
            llm: None,
        };
        plan.validate().unwrap();
        let rendered = render_cex_bundle(
            &fixture.feature_path,
            &fixture.materialization_path,
            &plan,
            7,
            plan.max_candidates().unwrap() * 2,
        )
        .unwrap();
        assert_eq!(rendered.mission.spec.feature_fields, subset.feature_fields);
        assert_eq!(
            rendered.mission.spec.research_delta.as_ref().unwrap(),
            &subset
        );
        assert_eq!(
            rendered.mission.spec.policies.gp.content_sha256,
            CexGpPolicyV1::controlled_dynamic_v5(
                rendered.mission.spec.policies.gp.id.clone(),
                rendered.mission.spec.feature_fields.clone(),
                subset.operators,
                subset.windows,
                rendered.mission.spec.search.seed,
                &rendered.mission.spec.search.budget,
            )
            .unwrap()
            .content_hash()
            .unwrap()
        );
        assert_eq!(
            rendered.mission.spec.search.budget.max_candidates,
            plan.max_candidates().unwrap()
        );
        assert_eq!(
            rendered.mission.spec.policies.baseline.content_sha256,
            CexBaselinePolicyV1::controlled_v2(
                rendered.mission.spec.policies.baseline.id.clone(),
                subset.ridge_l2,
                subset.cart_max_depth,
                subset.cart_min_leaf,
            )
            .unwrap()
            .content_hash()
            .unwrap()
        );

        let mut request = crate::mission_campaign::valid_request_for_tests();
        request.research_plan = plan;
        crate::mission_campaign::validate_terminal_mission_revision_binding(
            &rendered.mission,
            &request,
        )
        .unwrap();
        let mut forged = rendered.mission.clone();
        let forged_delta = {
            let delta = forged.spec.research_delta.as_mut().unwrap();
            delta.windows = vec![5, 40];
            delta.clone()
        };
        let forged_gp = CexGpPolicyV1::controlled_dynamic_v5(
            forged.spec.policies.gp.id.clone(),
            forged.spec.feature_fields.clone(),
            forged_delta.operators.clone(),
            forged_delta.windows.clone(),
            forged.spec.search.seed,
            &forged.spec.search.budget,
        )
        .unwrap();
        forged.spec.policies.gp.content_sha256 = forged_gp.content_hash().unwrap();
        forged.validate().unwrap();
        assert!(
            crate::mission_campaign::validate_terminal_mission_revision_binding(&forged, &request,)
                .is_err()
        );
        let mut forged_decision = rendered.mission.clone();
        forged_decision
            .spec
            .policies
            .supervised_decision
            .content_sha256 = CexCampaignPositionPolicyV1::CostAware
            .decision_policy()
            .content_hash()
            .unwrap();
        forged_decision.validate().unwrap();
        assert!(
            crate::mission_campaign::validate_terminal_mission_revision_binding(
                &forged_decision,
                &request,
            )
            .is_err()
        );
    }

    #[test]
    fn render_cex_rejects_short_materializations() {
        let fixture = Fixture::new(MIN_ROWS - 1);
        let error = render_cex_bundle(
            &fixture.feature_path,
            &fixture.materialization_path,
            &CexCampaignResearchPlanV1::canonical(),
            7,
            default_trials(),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains(&format!("at least {MIN_ROWS} point-in-time rows")));
    }

    #[test]
    fn spot_renderer_binds_typed_spot_market() {
        let fixture = Fixture::spot(MIN_ROWS);
        let rendered = render_cex_bundle(
            &fixture.feature_path,
            &fixture.materialization_path,
            &CexCampaignResearchPlanV1::canonical(),
            7,
            default_trials(),
        )
        .unwrap();

        assert_eq!(
            rendered.mission.spec.instrument.market,
            CexResearchMarketV1::Spot
        );
        assert_eq!(rendered.mission.spec.instrument.symbol, "BTCUSDT");
        let spot_policy = CexCampaignPositionPolicyV1::CostAware
            .decision_policy_for_market(CexResearchMarketV1::Spot);
        assert!(spot_policy.long_only);
        assert_eq!(
            rendered
                .mission
                .spec
                .policies
                .supervised_decision
                .content_sha256,
            spot_policy.content_hash().unwrap()
        );
    }

    #[test]
    fn render_cex_rejects_feature_source_drift_without_leaving_output() {
        let fixture = Fixture::with_feature_source(MIN_ROWS, "d".repeat(64));
        let error = render_cex_bundle(
            &fixture.feature_path,
            &fixture.materialization_path,
            &CexCampaignResearchPlanV1::canonical(),
            7,
            default_trials(),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("feature source revision"));
    }

    #[test]
    fn render_cex_rejects_forbidden_current_cex_feature_fields() {
        for field in ["funding_cost_bps", "funding_rate", "open_interest"] {
            let fixture = Fixture::new(MIN_ROWS);
            let mut rows = read_feature_rows(&fixture.feature_path);
            for row in &mut rows {
                row.features.insert(field.to_string(), 0.0001);
            }
            rewrite_feature_rows(&fixture.feature_path, &rows);
            let feature_sha256 = hex::encode(Sha256::digest(
                std::fs::read(&fixture.feature_path).unwrap(),
            ));
            let mut materialization: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&fixture.materialization_path).unwrap())
                    .unwrap();
            materialization["artifact_sha256"] = serde_json::json!(feature_sha256.clone());
            materialization["snapshot"]["feature_artifact_sha256"] =
                serde_json::json!(feature_sha256);
            let snapshot: hft_research_manifest::CexReplaySnapshotV5 =
                serde_json::from_value(materialization["snapshot"].clone()).unwrap();
            materialization["snapshot_sha256"] = serde_json::json!(snapshot.sha256());
            std::fs::write(
                &fixture.materialization_path,
                serde_json::to_vec_pretty(&materialization).unwrap(),
            )
            .unwrap();
            let feature_artifacts = tempfile::tempdir().unwrap();
            let feature_manifest = import_feature_manifest(
                "data-mission-1",
                &fixture.feature_path,
                feature_artifacts.path(),
            )
            .unwrap();
            let materialization = load_materialization(&fixture.materialization_path);
            let error = data_mission::validate_cex_replay_features(
                &materialization.snapshot,
                &feature_manifest,
            )
            .unwrap_err();

            assert!(format!("{error:#}").contains(&format!(
                "current L2-only CEX replay cannot include {field}"
            )));

            assert!(render_cex_bundle(
                &fixture.feature_path,
                &fixture.materialization_path,
                &CexCampaignResearchPlanV1::canonical(),
                7,
                default_trials(),
            )
            .is_err());
        }
    }

    #[test]
    fn render_cex_rejects_execution_oversized_inputs() {
        let fixture = Fixture::new(MIN_ROWS);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&fixture.feature_path)
            .unwrap()
            .set_len(MAX_FEATURE_BYTES + 1)
            .unwrap();
        let error = render_cex_bundle(
            &fixture.feature_path,
            &fixture.materialization_path,
            &CexCampaignResearchPlanV1::canonical(),
            7,
            default_trials(),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("source exceeds the allowed size"));

        let fixture = Fixture::new(MIN_ROWS);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&fixture.materialization_path)
            .unwrap()
            .set_len(MAX_MATERIALIZATION_BYTES + 1)
            .unwrap();
        let error = render_cex_bundle(
            &fixture.feature_path,
            &fixture.materialization_path,
            &CexCampaignResearchPlanV1::canonical(),
            7,
            default_trials(),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("source exceeds the allowed size"));
    }

    #[test]
    fn render_cex_rejects_materialization_source_revision_drift() {
        let fixture = Fixture::new(MIN_ROWS);
        let mut materialization: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&fixture.materialization_path).unwrap()).unwrap();
        materialization["source_revision"] = serde_json::json!("9".repeat(64));
        std::fs::write(
            &fixture.materialization_path,
            serde_json::to_vec_pretty(&materialization).unwrap(),
        )
        .unwrap();

        let error = render_cex_bundle(
            &fixture.feature_path,
            &fixture.materialization_path,
            &CexCampaignResearchPlanV1::canonical(),
            7,
            default_trials(),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("source revision does not bind"));
    }

    #[test]
    fn render_cex_bundle_changes_search_lineage_per_seed_but_keeps_holdout() {
        let fixture = Fixture::new(MIN_ROWS);
        let first = render_cex_bundle(
            &fixture.feature_path,
            &fixture.materialization_path,
            &CexCampaignResearchPlanV1::canonical(),
            7,
            default_trials(),
        )
        .unwrap();
        let second = render_cex_bundle(
            &fixture.feature_path,
            &fixture.materialization_path,
            &CexCampaignResearchPlanV1::canonical(),
            11,
            default_trials(),
        )
        .unwrap();

        assert_ne!(first.mission_id, second.mission_id);
        assert_ne!(
            first.mission.spec.search_lineage_id,
            second.mission.spec.search_lineage_id
        );
        assert_eq!(
            first.mission.spec.holdout.holdout_id,
            second.mission.spec.holdout.holdout_id
        );
        assert_holdout_id_format(&first.mission.spec.holdout.holdout_id);
    }

    #[test]
    fn render_cex_bundle_keeps_holdout_for_rematerialized_same_cohort() {
        let fixture = Fixture::new(MIN_ROWS);
        let first = render_cex_bundle(
            &fixture.feature_path,
            &fixture.materialization_path,
            &CexCampaignResearchPlanV1::canonical(),
            7,
            default_trials(),
        )
        .unwrap();

        let shifted_ingestion_time = read_feature_rows(&fixture.feature_path)[0].ingestion_time
            + ChronoDuration::seconds(60);
        let feature_bytes = std::fs::read_to_string(&fixture.feature_path)
            .unwrap()
            .lines()
            .map(|line| format!("{line} "))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(&fixture.feature_path, feature_bytes).unwrap();
        let feature_sha256 = hex::encode(Sha256::digest(
            std::fs::read(&fixture.feature_path).unwrap(),
        ));

        let mut materialization: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&fixture.materialization_path).unwrap()).unwrap();
        materialization["mission_id"] = serde_json::json!("data-mission-rematerialized");
        materialization["artifact_sha256"] = serde_json::json!(feature_sha256);
        materialization["snapshot"]["feature_artifact_sha256"] = serde_json::json!(feature_sha256);
        let snapshot: hft_research_manifest::CexReplaySnapshotV5 =
            serde_json::from_value(materialization["snapshot"].clone()).unwrap();
        materialization["snapshot_sha256"] = serde_json::json!(snapshot.sha256());
        materialization["created_at"] = serde_json::json!(shifted_ingestion_time);
        std::fs::write(
            &fixture.materialization_path,
            serde_json::to_vec_pretty(&materialization).unwrap(),
        )
        .unwrap();

        let second = render_cex_bundle(
            &fixture.feature_path,
            &fixture.materialization_path,
            &CexCampaignResearchPlanV1::canonical(),
            7,
            default_trials(),
        )
        .unwrap();

        assert_eq!(
            first.mission.spec.holdout.holdout_id,
            second.mission.spec.holdout.holdout_id
        );
        assert_holdout_id_format(&first.mission.spec.holdout.holdout_id);
    }

    #[test]
    fn sealed_holdout_cohort_hash_changes_with_window_source_or_label_slice() {
        let fixture = Fixture::new(MIN_ROWS);
        let base = load_materialization(&fixture.materialization_path);
        let base_hash = sealed_holdout_cohort_sha256(&base).unwrap();

        let shifted_window = mutate_materialization(&fixture.materialization_path, |value| {
            let last_event_time = serde_json::from_value::<chrono::DateTime<Utc>>(
                value["snapshot"]["last_event_time"].clone(),
            )
            .unwrap()
                + ChronoDuration::seconds(1);
            value["rows"] = serde_json::json!(MIN_ROWS + 1);
            value["snapshot"]["last_event_time"] = serde_json::json!(last_event_time);
            value["last_event_time"] = serde_json::json!(last_event_time);
        });
        let shifted_window_hash = sealed_holdout_cohort_sha256(&shifted_window).unwrap();
        assert_ne!(base_hash, shifted_window_hash);

        let shifted_source = mutate_materialization(&fixture.materialization_path, |value| {
            value["source_revision"] = serde_json::json!("d".repeat(64));
            value["snapshot"]["source_segments"][0]["content_sha256"] =
                serde_json::json!("e".repeat(64));
        });
        let shifted_source_hash = sealed_holdout_cohort_sha256(&shifted_source).unwrap();
        assert_ne!(base_hash, shifted_source_hash);

        let shifted_label = mutate_materialization(&fixture.materialization_path, |value| {
            value["label_horizon_buckets"] = serde_json::json!(6);
            value["snapshot"]["label_horizon_buckets"] = serde_json::json!(6);
        });
        let shifted_label_hash = sealed_holdout_cohort_sha256(&shifted_label).unwrap();
        assert_ne!(base_hash, shifted_label_hash);
    }

    pub(crate) struct Fixture {
        pub(crate) _root: tempfile::TempDir,
        pub(crate) feature_path: PathBuf,
        pub(crate) materialization_path: PathBuf,
    }

    impl Fixture {
        pub(crate) fn canonical() -> Self {
            Self::new(MIN_ROWS)
        }

        pub(crate) fn new(rows: usize) -> Self {
            Self::with_optional_feature_source(rows, None)
        }

        pub(crate) fn spot(rows: usize) -> Self {
            Self::with_market(rows, None, "spot")
        }

        fn with_feature_source(rows: usize, feature_source_revision: String) -> Self {
            Self::with_optional_feature_source(rows, Some(feature_source_revision))
        }

        fn with_optional_feature_source(
            rows: usize,
            feature_source_revision: Option<String>,
        ) -> Self {
            Self::with_market(rows, feature_source_revision, "usdm")
        }

        fn with_market(rows: usize, feature_source_revision: Option<String>, market: &str) -> Self {
            let root = tempfile::tempdir().unwrap();
            let feature_path = root.path().join("features.jsonl");
            let materialization_path = root.path().join("materialization.json");
            let source_content_sha256 = "b".repeat(64);
            let source_revision =
                hft_collector::lob_archiver::source_revision([source_content_sha256.as_str()]);
            let feature_source_revision = feature_source_revision
                .as_deref()
                .unwrap_or(&source_revision);
            let rows = feature_rows(rows, feature_source_revision, market);
            write_feature_rows(&feature_path, &rows);
            let bytes = std::fs::read(&feature_path).unwrap();
            let feature_sha256 = hex::encode(Sha256::digest(&bytes));
            let first_event_time = rows.first().unwrap().event_time;
            let last_event_time = rows.last().unwrap().event_time;
            let ingestion_time = rows.first().unwrap().ingestion_time;
            let source_manifest_sha256 = "c".repeat(64);
            let source_start_ns = u64::try_from(
                (first_event_time - ChronoDuration::seconds(1))
                    .timestamp_nanos_opt()
                    .unwrap(),
            )
            .unwrap();
            let source_end_ns =
                u64::try_from(ingestion_time.timestamp_nanos_opt().unwrap()).unwrap();
            // Synthetic fixture coverage scales with the requested observation window.
            let reference_observations = rows.len().div_ceil(90) + 2;
            let instrument_rules_evidence = (0..reference_observations)
                .map(indexed_cex_triplet)
                .collect::<Vec<_>>();
            let snapshot = hft_research_manifest::CexReplaySnapshotV5 {
                schema_version: hft_research_manifest::CEX_REPLAY_SNAPSHOT_SCHEMA_V5.to_string(),
                venue: "binance".to_string(),
                instrument_type: market.to_string(),
                symbol: "BTCUSDT".to_string(),
                replay_clock: hft_research_manifest::CEX_REPLAY_CLOCK_RECEIVED_AT_NS.to_string(),
                required_modalities: BTreeSet::from([
                    hft_research_manifest::CEX_MODALITY_LOB.to_string(),
                    hft_research_manifest::CEX_MODALITY_AGGREGATE_TRADE.to_string(),
                ]),
                source_segments: vec![hft_research_manifest::CexReplaySegmentIdentity {
                    content_sha256: source_content_sha256.clone(),
                    manifest_sha256: source_manifest_sha256.clone(),
                    start_received_at_ns: source_start_ns,
                    end_received_at_ns: source_end_ns,
                    events: rows.len() as u64,
                }],
                first_event_time,
                last_event_time,
                feature_artifact_sha256: feature_sha256.clone(),
                feature_availability_policy: hft_research_manifest::CEX_FEATURE_AVAILABILITY_POLICY
                    .to_string(),
                bucket_ms: 1_000,
                label_horizon_buckets: 5,
                top_depth: 5,
                instrument_rules: hft_research_manifest::CexInstrumentRulesV2 {
                    tick_size: "0.1".to_string(),
                    step_size: "0.001".to_string(),
                    min_notional: "5".to_string(),
                    available_at: first_event_time - ChronoDuration::seconds(1),
                    valid_through: last_event_time + ChronoDuration::seconds(5),
                    evidence: instrument_rules_evidence.clone(),
                },
                spot_instrument_rules: (market == "spot").then(spot_instrument_rules),
                series: vec![hft_research_manifest::CexReplaySeriesV1 {
                    series_id: 1,
                    first_event_time,
                    last_event_time,
                    instrument_rules_coverage: hft_research_manifest::CexPitSeriesEvidenceV2 {
                        evidence: instrument_rules_evidence,
                        first_available_at: first_event_time - ChronoDuration::seconds(1),
                        last_available_at: last_event_time + ChronoDuration::seconds(5),
                        observations: reference_observations as u64,
                        max_gap_ns: hft_research_manifest::CEX_DERIVATIVES_MAX_GAP_NS,
                    },
                }],
            };
            let snapshot_sha256 = snapshot.sha256();
            let report = serde_json::json!({
                "dataset_kind": "lob_point_in_time_materialization",
                "schema_version": hft_research_manifest::BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V7,
                "mission_id": "data-mission-1",
                "symbol": "BTCUSDT",
                "market": market,
                "bucket_ms": 1000,
                "label_horizon_buckets": 5,
                "top_depth": 5,
                "source_revision": source_revision,
                "source_segments": [{
                    "path": "raw/segment.jsonl.zst",
                    "sha256": source_content_sha256,
                    "collector_manifest_sha256": source_manifest_sha256,
                    "success_marker_sha256": hex::encode(Sha256::digest(format!("{source_content_sha256}\n"))),
                    "start_received_at_ns": source_start_ns,
                    "end_received_at_ns": source_end_ns,
                    "events": rows.len()
                }],
                "series_count": 1,
                "rows": rows.len(),
                "first_event_time": first_event_time,
                "last_event_time": last_event_time,
                "artifact_path": feature_path,
                "artifact_sha256": feature_sha256,
                "snapshot": snapshot,
                "snapshot_sha256": snapshot_sha256,
                "created_at": ingestion_time,
            });
            std::fs::write(
                &materialization_path,
                serde_json::to_vec_pretty(&report).unwrap(),
            )
            .unwrap();
            Self {
                _root: root,
                feature_path,
                materialization_path,
            }
        }
    }

    fn feature_rows(
        count: usize,
        source_revision: &str,
        market: &str,
    ) -> Vec<PointInTimeFeatureRow> {
        let ingestion_time = Utc::now();
        let start = ingestion_time - ChronoDuration::seconds(count as i64 + 10);
        (0..count)
            .map(|index| PointInTimeFeatureRow {
                series_id: 1,
                event_time: start + ChronoDuration::seconds(index as i64),
                feature_available_time: start + ChronoDuration::seconds(index as i64),
                label_available_time: start + ChronoDuration::seconds(index as i64 + 5),
                ingestion_time,
                symbol: "BTCUSDT".to_string(),
                source_revisions: BTreeMap::from([(
                    format!("binance-{market}-lob"),
                    source_revision.to_string(),
                )]),
                modalities: BTreeSet::from([DataModality::Lob, DataModality::TradeTick]),
                features: BTreeMap::from([
                    (
                        CEX_RESEARCH_AGGREGATE_TRADE_FLOW_IMBALANCE_FIELD.to_string(),
                        -0.2,
                    ),
                    ("ask_depth_top5".to_string(), 10.0),
                    ("bid_depth_top5".to_string(), 10.0),
                    ("book_imbalance".to_string(), 0.1),
                    ("book_imbalance_top5".to_string(), 0.08),
                    ("mid_price".to_string(), 60_000.0),
                    ("near_depth_concentration_skew_top5".to_string(), 0.04),
                    ("spread_bps".to_string(), 1.2),
                    ("vwap_center_deviation_top5_bps".to_string(), 0.7),
                    ("weighted_book_imbalance_top5".to_string(), 0.09),
                ]),
                label: 0.0001,
            })
            .collect()
    }

    fn spot_instrument_rules() -> hft_research_manifest::CexSpotInstrumentRulesV1 {
        hft_research_manifest::CexSpotInstrumentRulesV1 {
            schema: "binance.spot_reference.v1".to_string(),
            venue: "binance".to_string(),
            market: "spot".to_string(),
            symbol: "BTCUSDT".to_string(),
            base_asset: "BTC".to_string(),
            quote_asset: "USDT".to_string(),
            status: "TRADING".to_string(),
            is_spot_trading_allowed: true,
            base_asset_precision: 8,
            quote_asset_precision: 8,
            price_filter: hft_research_manifest::CexSpotPriceFilterV1 {
                min_price: "0".to_string(),
                max_price: "0".to_string(),
                tick_size: "0.1".to_string(),
            },
            lot_size_filter: hft_research_manifest::CexSpotQuantityFilterV1 {
                min_quantity: "0.001".to_string(),
                max_quantity: "100".to_string(),
                step_size: "0.001".to_string(),
            },
            market_lot_size_filter: Some(hft_research_manifest::CexSpotQuantityFilterV1 {
                min_quantity: "0.001".to_string(),
                max_quantity: "100".to_string(),
                step_size: "0.001".to_string(),
            }),
            notional_filter: hft_research_manifest::CexSpotNotionalFilterV1 {
                filter_type: "MIN_NOTIONAL".to_string(),
                min_notional: "5".to_string(),
                max_notional: None,
                apply_min_to_market: false,
                apply_max_to_market: None,
                avg_price_mins: 5,
            },
            source_time_ms: 1,
            source_clock_received_at_ns: 1_000_000,
            received_at_ns: 1_000_000,
            source_endpoint: "/api/v3/exchangeInfo".to_string(),
            source_clock_endpoint: "/api/v3/time".to_string(),
        }
    }

    fn indexed_cex_triplet(index: usize) -> hft_research_manifest::CexArtifactTripletV2 {
        let data_sha256 = hex::encode(Sha256::digest(format!("render-reference-data-{index}")));
        hft_research_manifest::CexArtifactTripletV2 {
            manifest_sha256: hex::encode(Sha256::digest(format!(
                "render-reference-manifest-{index}"
            ))),
            success_sha256: data_sha256.clone(),
            data_sha256,
        }
    }

    fn write_feature_rows(path: &Path, rows: &[PointInTimeFeatureRow]) {
        let mut bytes = Vec::new();
        for row in rows {
            serde_json::to_writer(&mut bytes, row).unwrap();
            bytes.push(b'\n');
        }
        std::fs::write(path, bytes).unwrap();
    }

    pub(crate) fn rewrite_feature_rows(path: &Path, rows: &[PointInTimeFeatureRow]) {
        write_feature_rows(path, rows);
    }

    pub(crate) fn read_feature_rows(path: &Path) -> Vec<PointInTimeFeatureRow> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn load_materialization(path: &Path) -> crate::mission_runner::Materialization {
        decode_materialization(&std::fs::read(path).unwrap()).unwrap()
    }

    fn mutate_materialization(
        path: &Path,
        mutate: impl FnOnce(&mut serde_json::Value),
    ) -> crate::mission_runner::Materialization {
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        mutate(&mut value);
        decode_materialization(&serde_json::to_vec_pretty(&value).unwrap()).unwrap()
    }

    fn assert_holdout_id_format(holdout_id: &str) {
        assert!(holdout_id.starts_with("cex-holdout-"));
        assert_eq!(holdout_id.len(), 60);
        assert!(holdout_id
            .strip_prefix("cex-holdout-")
            .unwrap()
            .chars()
            .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase()));
    }
}
