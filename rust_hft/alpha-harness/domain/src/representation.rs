//! Read-only representation proposals. Serialized declarations grant no data or compute authority.
use crate::{
    canonical_json_hash, CexResearchContentRefV1, CexResearchHypothesisV1, EvaluationLabelSpecV1,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const CAPABILITY_SCHEMA: &str = "monday.cex_data_capability.v1";
pub const REPRESENTATION_PLAN_SCHEMA: &str = "monday.cex_representation_plan.v1";
pub const MAX_CAPABILITY_SERIES: usize = 256;
pub const MAX_CAPABILITY_SOURCES: usize = 4096;

// Existing canonical Campaign partitions, shared with the actual renderer.
pub const CAMPAIGN_INITIAL_TRAIN_ROWS: usize = 7_200;
pub const CAMPAIGN_VALIDATION_ROWS: usize = 3_600;
pub const CAMPAIGN_FOLD_COUNT: usize = 3;
pub const CAMPAIGN_HOLDOUT_ROWS: usize = 3_600;
pub const CAMPAIGN_SELECTION_ROWS: usize = 3_600;
pub const CAMPAIGN_DEFAULT_PURGE_ROWS: usize = 10;
pub const CAMPAIGN_DEFAULT_EMBARGO_ROWS: usize = 5;
pub const CAMPAIGN_DEFAULT_MIN_ROWS: usize = CAMPAIGN_INITIAL_TRAIN_ROWS
    + CAMPAIGN_FOLD_COUNT * (CAMPAIGN_VALIDATION_ROWS + CAMPAIGN_DEFAULT_EMBARGO_ROWS)
    + CAMPAIGN_DEFAULT_PURGE_ROWS
    + CAMPAIGN_SELECTION_ROWS
    + 2 * CAMPAIGN_DEFAULT_PURGE_ROWS
    + CAMPAIGN_HOLDOUT_ROWS;

pub fn campaign_minimum_rows(purge_rows: usize, embargo_rows: usize) -> Result<usize, String> {
    let fold_rows = CAMPAIGN_VALIDATION_ROWS
        .checked_add(embargo_rows)
        .and_then(|rows| CAMPAIGN_FOLD_COUNT.checked_mul(rows))
        .ok_or("typed Campaign horizon row budget overflowed")?;
    CAMPAIGN_INITIAL_TRAIN_ROWS
        .checked_add(fold_rows)
        .and_then(|rows| rows.checked_add(purge_rows))
        .and_then(|rows| rows.checked_add(CAMPAIGN_SELECTION_ROWS))
        .and_then(|rows| rows.checked_add(purge_rows.checked_mul(2)?))
        .and_then(|rows| rows.checked_add(CAMPAIGN_HOLDOUT_ROWS))
        .ok_or_else(|| "typed Campaign horizon row budget overflowed".into())
}

pub fn registered_materialization_minimum_rows(
    labels: &EvaluationLabelSpecV1,
) -> Result<usize, String> {
    if labels.observation_frequency_millis != 1_000
        || ![5, 10, 30].contains(&labels.horizon_buckets)
    {
        return Err("unregistered Campaign cadence or horizon".into());
    }
    let horizon = labels.horizon_buckets;
    campaign_minimum_rows(
        horizon.checked_mul(2).ok_or("purge rows overflow")?,
        horizon,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BookContinuityV1 {
    SnapshotOnly,
    Unseeded,
    Gap,
    SequenceChecked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanningVisibilityV1 {
    Training,
    Development,
    IndependentValidation,
    StrategySealed,
    MetaCertification,
    ExposedTerminal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanningViewV1 {
    pub view: CexResearchContentRefV1,
    pub family_id: String,
    pub visibility: PlanningVisibilityV1,
    /// A permission binding, not proof that a current grant permits reading this view.
    pub permission: CexResearchContentRefV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BookSeriesCapabilityV1 {
    pub session_id: String,
    pub start_available_ns: u64,
    pub end_available_ns: u64,
    /// Label-only coverage in this same recovery series; never a feature clock.
    pub label_available_through_ns: u64,
    pub snapshots: u64,
    pub diffs: u64,
    /// Observed snapshot depth; this does not certify every future replay state.
    pub captured_seed_depth: u16,
    pub continuity: BookContinuityV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldClockV1 {
    pub field: String,
    pub unit: String,
    pub event_ns: Option<u64>,
    pub received_ns: u64,
    pub available_ns: u64,
    pub decision_ns: u64,
}

/// A source/window declaration. Only original artifact verifiers establish its facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstrumentRuleCoverageV1 {
    pub market: String,
    pub symbol: String,
    pub sources: Vec<CexResearchContentRefV1>,
    pub rules_identity_sha256: String,
    pub first_available_ns: u64,
    pub last_available_ns: u64,
    pub max_gap_ns: u64,
}

impl InstrumentRuleCoverageV1 {
    pub fn validate(&self) -> Result<(), String> {
        CexResearchContentRefV1 {
            id: "instrument-rules".into(),
            content_sha256: self.rules_identity_sha256.clone(),
        }
        .validate()
        .map_err(|error| error.to_string())?;
        if !matches!(self.market.as_str(), "usdm" | "spot")
            || self.symbol.is_empty()
            || self.sources.is_empty()
            || self.sources.len() > MAX_CAPABILITY_SOURCES
            || self.first_available_ns == 0
            || self.last_available_ns < self.first_available_ns
            || self.max_gap_ns > hft_research_manifest::CEX_DERIVATIVES_MAX_GAP_NS
        {
            return Err("invalid instrument-rule coverage declaration".into());
        }
        let mut identities = BTreeSet::new();
        for source in &self.sources {
            source.validate().map_err(|error| error.to_string())?;
            if !identities.insert(&source.content_sha256) {
                return Err("repeated instrument-rule source identity".into());
            }
        }
        Ok(())
    }
}

impl FieldClockV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.field.is_empty()
            || self.unit.is_empty()
            || self.received_ns == 0
            || self.available_ns < self.received_ns
            || self.available_ns > self.decision_ns
            || self
                .event_ns
                .is_some_and(|v| v == 0 || v > self.available_ns)
        {
            return Err("field is unavailable at its decision clock".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataCapabilityV1 {
    pub schema: String,
    pub venue: String,
    pub market: String,
    pub symbol: String,
    pub sources: Vec<CexResearchContentRefV1>,
    pub normalizer: CexResearchContentRefV1,
    pub series: Vec<BookSeriesCapabilityV1>,
    pub fields: Vec<FieldClockV1>,
    pub aggregate_trade_direction: bool,
    pub instrument_rules: Option<InstrumentRuleCoverageV1>,
    pub view: PlanningViewV1,
}

impl DataCapabilityV1 {
    /// Validates a declaration. Neither this check nor its hash verifies raw data.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != CAPABILITY_SCHEMA
            || self.venue != "binance"
            || !matches!(self.market.as_str(), "usdm" | "spot")
            || self.symbol.is_empty()
            || self.symbol.len() > 32
            || !self
                .symbol
                .bytes()
                .all(|v| v.is_ascii_uppercase() || v.is_ascii_digit())
            || self.sources.is_empty()
            || self.sources.len() > MAX_CAPABILITY_SOURCES
            || self.series.is_empty()
            || self.series.len() > MAX_CAPABILITY_SERIES
            || self.fields.is_empty()
            || self.fields.len() > 64
            || self.view.family_id.is_empty()
        {
            return Err("invalid data capability declaration".into());
        }
        self.normalizer.validate().map_err(|e| e.to_string())?;
        self.view.view.validate().map_err(|e| e.to_string())?;
        self.view.permission.validate().map_err(|e| e.to_string())?;
        let mut sources = BTreeSet::new();
        for source in &self.sources {
            source.validate().map_err(|e| e.to_string())?;
            if !sources.insert(&source.content_sha256) {
                return Err("repeated source identity".into());
            }
        }
        if let Some(rules) = &self.instrument_rules {
            rules.validate()?;
            if rules.market != self.market
                || rules.symbol != self.symbol
                || rules
                    .sources
                    .iter()
                    .any(|reference| !self.sources.iter().any(|source| source == reference))
            {
                return Err(
                    "instrument-rule scope or sources differ from the data capability".into(),
                );
            }
        }
        let mut sessions = BTreeSet::new();
        for series in &self.series {
            if series.session_id.is_empty()
                || !sessions.insert(&series.session_id)
                || series.start_available_ns == 0
                || series.end_available_ns < series.start_available_ns
                || series.label_available_through_ns < series.end_available_ns
                || series.captured_seed_depth > 4096
                || (series.continuity == BookContinuityV1::SequenceChecked
                    && (series.snapshots == 0 || series.diffs == 0))
                || (series.continuity == BookContinuityV1::SnapshotOnly
                    && (series.snapshots == 0 || series.diffs != 0))
            {
                return Err("invalid book series declaration".into());
            }
        }
        let mut fields = BTreeSet::new();
        for field in &self.fields {
            field.validate()?;
            if !fields.insert(&field.field) {
                return Err("repeated field".into());
            }
        }
        Ok(())
    }
    pub fn digest(&self) -> Result<String, String> {
        self.validate()?;
        canonical_json_hash(self).map_err(|e| e.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanningResourcesV1 {
    pub cpu_millis: u32,
    pub memory_mib: u32,
    pub wall_seconds: u32,
    pub trials: u32,
}
impl PlanningResourcesV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.cpu_millis == 0
            || self.memory_mib == 0
            || self.wall_seconds == 0
            || self.trials == 0
        {
            return Err("positive finite planning limits required".into());
        }
        Ok(())
    }
    pub fn fits(&self, limit: &Self) -> bool {
        self.cpu_millis <= limit.cpu_millis
            && self.memory_mib <= limit.memory_mib
            && self.wall_seconds <= limit.wall_seconds
            && self.trials <= limit.trials
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepresentationGoalV1 {
    pub goal: CexResearchContentRefV1,
    pub family_id: String,
    pub venue: String,
    pub market: String,
    pub symbol: String,
    pub target_name: String,
    pub labels: EvaluationLabelSpecV1,
    pub window_start_ns: u64,
    pub window_end_ns: u64,
    pub model: CexResearchContentRefV1,
    pub scaling: CexResearchContentRefV1,
    pub costs: CexResearchContentRefV1,
    pub partition: CexResearchContentRefV1,
    /// Both arms require their own future admission. This is not a budget reservation.
    pub resource_limit: PlanningResourcesV1,
}
impl RepresentationGoalV1 {
    pub fn label_end_ns(&self) -> Result<u64, String> {
        let horizon_ns = self
            .labels
            .observation_frequency_millis
            .checked_mul(self.labels.horizon_buckets as u64)
            .and_then(|millis| millis.checked_mul(1_000_000))
            .ok_or("label availability window overflow")?;
        self.window_end_ns
            .checked_add(horizon_ns)
            .ok_or_else(|| "label availability window overflow".into())
    }

    pub fn validate(&self) -> Result<(), String> {
        for reference in [
            &self.goal,
            &self.model,
            &self.scaling,
            &self.costs,
            &self.partition,
        ] {
            reference.validate().map_err(|e| e.to_string())?;
        }
        self.resource_limit.validate()?;
        self.label_end_ns()?;
        if self.family_id.is_empty()
            || self.target_name.is_empty()
            || self.window_start_ns == 0
            || self.window_end_ns <= self.window_start_ns
            || self.labels.horizon_buckets == 0
            || self.labels.observation_frequency_millis == 0
            || self
                .labels
                .observation_frequency_millis
                .checked_mul(self.labels.horizon_buckets as u64)
                .is_none()
        {
            return Err("invalid frozen representation goal".into());
        }
        Ok(())
    }
    pub fn digest(&self) -> Result<String, String> {
        self.validate()?;
        canonical_json_hash(self).map_err(|e| e.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepresentationToolV1 {
    CapturedBookReplay,
    StaticTop5,
    LaggedContinuousOfi,
    AggregateTradeFlow,
    SolSequence,
    SolMarketEncoder,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolMatchV1 {
    pub tool: RepresentationToolV1,
    pub implementation: CexResearchContentRefV1,
    pub history_ms: u64,
    pub supported: bool,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepresentationArmV1 {
    pub name: String,
    pub fields: Vec<String>,
    pub history_ms: u64,
    pub tools: Vec<RepresentationToolV1>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepresentationPlanStatusV1 {
    NoExecutableComparison,
    NoFeasibleComparison,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepresentationPlanV1 {
    pub schema: String,
    pub capability_sha256: String,
    pub goal_sha256: String,
    pub registry_sha256: String,
    pub status: RepresentationPlanStatusV1,
    pub goal: RepresentationGoalV1,
    pub matches: Vec<ToolMatchV1>,
    /// Unfunded representation materials; these are not scientific trials.
    pub materializations: Vec<RepresentationArmV1>,
    /// Reserved for a future native execution contract. This planner emits none.
    pub arms: Vec<RepresentationArmV1>,
    pub hypothesis: Option<CexResearchHypothesisV1>,
    /// Unknown until an actual execution template and accounting contract are bound.
    pub requested_resources: Option<PlanningResourcesV1>,
    pub limitations: Vec<String>,
}
impl RepresentationPlanV1 {
    pub fn digest(&self) -> Result<String, String> {
        canonical_json_hash(self).map_err(|e| e.to_string())
    }
    pub fn validate_binding(
        &self,
        data: &DataCapabilityV1,
        goal: &RepresentationGoalV1,
    ) -> Result<(), String> {
        if self.schema != REPRESENTATION_PLAN_SCHEMA
            || self.capability_sha256 != data.digest()?
            || self.goal_sha256 != goal.digest()?
            || &self.goal != goal
        {
            return Err("representation source or frozen goal drift".into());
        }
        Ok(())
    }
}

/// Reuse of existing native columns. This binds a request, not a raw qualification or grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepresentationCampaignBindingV1 {
    pub schema: String,
    /// Prepared reuse binds the native development window with an exclusive end.
    /// This does not change raw-declaration goal semantics or prove raw label maturity.
    pub goal: RepresentationGoalV1,
    pub planning_view: PlanningViewV1,
    pub selected_arm: String,
    pub runner_source_revision: String,
    pub collection_sha256: String,
    pub development_rows_sha256: String,
    pub producer_source_revision: String,
    pub producer_image_identity: String,
    pub preparation_run_id: String,
    pub preparation_receipt_sha256: String,
    pub feature_sha256: String,
    pub materialization_sha256: String,
    pub replay_artifact_sha256: String,
    pub replay_manifest_sha256: String,
    pub protocol_sha256: String,
    pub materialized_columns: Vec<String>,
    pub seeds: Vec<u64>,
    pub declared_total_trials: usize,
}
impl RepresentationCampaignBindingV1 {
    pub fn validate(&self) -> Result<(), String> {
        let git = |value: &str| {
            value.len() == 40
                && value
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        };
        let digest = |value: &str| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        };
        self.goal.validate()?;
        self.planning_view
            .view
            .validate()
            .map_err(|e| e.to_string())?;
        self.planning_view
            .permission
            .validate()
            .map_err(|e| e.to_string())?;
        if self.schema != "monday.representation_campaign_binding.v1"
            || !git(&self.runner_source_revision)
            || !git(&self.producer_source_revision)
            || !self
                .producer_image_identity
                .rsplit_once("@sha256:")
                .is_some_and(|(_, sha)| digest(sha))
            || !matches!(
                self.selected_arm.as_str(),
                "registered_h1_snapshot_family" | "registered_h2_lagged_ofi_family"
            )
            || self.planning_view.visibility != PlanningVisibilityV1::Development
            || self.planning_view.family_id != self.goal.family_id
            || self.preparation_run_id.is_empty()
            || self.seeds.len() < 2
            || self.seeds.len() > 16
            || self.seeds.iter().collect::<BTreeSet<_>>().len() != self.seeds.len()
            || self.declared_total_trials == 0
            || self.declared_total_trials > self.goal.resource_limit.trials as usize
            || self.materialized_columns.is_empty()
            || self.materialized_columns.len() > 4096
            || self
                .materialized_columns
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
            || self.materialized_columns.iter().any(|name| name.is_empty())
            || [
                &self.collection_sha256,
                &self.development_rows_sha256,
                &self.preparation_receipt_sha256,
                &self.feature_sha256,
                &self.materialization_sha256,
                &self.replay_artifact_sha256,
                &self.replay_manifest_sha256,
                &self.protocol_sha256,
            ]
            .into_iter()
            .any(|sha| !digest(sha))
        {
            return Err("invalid prepared-column Campaign binding".into());
        }
        Ok(())
    }
}

/// Preview of the actual renderer contracts. It grants no data or execution authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepresentationCampaignContractRefsV1 {
    pub model: CexResearchContentRefV1,
    pub scaling: CexResearchContentRefV1,
    pub costs: CexResearchContentRefV1,
    pub partition: CexResearchContentRefV1,
    pub planning_view: CexResearchContentRefV1,
    pub window_start_ns: u64,
    pub window_end_ns: u64,
    pub declared_total_trials: usize,
}
