//! Bounded metadata for a develop-only check before Campaign preparation.
use crate::{
    cli::{CampaignPrecheckArgs, BUILD_SOURCE_REVISION},
    data_mission,
    mission_render::{CexCampaignResearchPlanV1, PreparedCexInputs},
};
use alpha_domain::{EvaluationCalendarBindingV1, EvaluationProtocolV1};
use alpha_engine::label_precheck::LabelSpacePrecheckV1;
use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use std::io::Read;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DevelopmentPrecheckReceiptV1 {
    pub schema_version: String,
    pub source_revision: String,
    pub feature_sha256: String,
    pub materialization_sha256: String,
    pub protocol: EvaluationProtocolV1,
    pub decision_policy_sha256: String,
    pub report: LabelSpacePrecheckV1,
}

impl DevelopmentPrecheckReceiptV1 {
    pub(crate) fn binding(&self) -> anyhow::Result<&EvaluationCalendarBindingV1> {
        if self.schema_version != "monday.development_precheck.v1"
            || self.source_revision != BUILD_SOURCE_REVISION
            || self.report.scope != "calendar_development_only_overlapping_labels"
            || self.report.cancels_experiment
        {
            bail!("invalid calendar development precheck identity or scope");
        }
        self.protocol.validate()?;
        self.protocol
            .calendar
            .as_ref()
            .context("precheck lacks calendar binding")
    }
}

pub(crate) fn precheck(args: CampaignPrecheckArgs) -> anyhow::Result<()> {
    #[cfg(not(test))]
    {
        crate::cli::require_cloud_data_host(std::env::consts::OS)?;
        if std::env::var("MONDAY_EXECUTION_HOST").as_deref() != Ok("ack") {
            bail!("calendar precheck belongs in ACK");
        }
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&args.research_plan)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        bail!("precheck plan exceeds metadata limit");
    }
    let mut plan: CexCampaignResearchPlanV1 = serde_json::from_slice(&bytes)?;
    plan.validate()?;
    if plan.development_precheck.is_some() {
        bail!("precheck requires an unbound plan; retained reports are immutable");
    }
    let inputs = PreparedCexInputs::load(&args.feature, &args.materialization, true)?;
    let receipt = inputs.development_precheck(&plan)?;
    data_mission::write_json_atomic(&args.output, &receipt)?;
    plan.development_precheck = Some(receipt);
    plan.validate()?;
    data_mission::write_json_atomic(&args.research_plan_out, &plan)?;
    crate::cli::print_json(&serde_json::json!({
        "status":"development_precheck_complete", "report":args.output,
        "research_plan":args.research_plan_out, "report_sha256":crate::mission_runner::sha256_file(&args.output)?,
        "research_plan_sha256":crate::mission_runner::sha256_file(&args.research_plan_out)?,
        "training_performed":false, "campaign_frozen":false,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn precheck_command_publishes_bound_plan_before_any_freeze() {
        let fixture = crate::mission_render::tests::Fixture::new(28_770);
        let rows = crate::mission_render::tests::read_feature_rows(&fixture.feature_path);
        let start = rows[0].feature_available_time;
        let mut plan = CexCampaignResearchPlanV1::canonical();
        plan.calendar = Some(alpha_domain::EvaluationCalendarV1 {
            start,
            develop_end: start + chrono::TimeDelta::hours(4),
            validation_end: start + chrono::TimeDelta::hours(6),
            end: start + chrono::TimeDelta::hours(8),
        });
        plan.supervised_model_scope = alpha_domain::CexSupervisedModelScopeV1::RidgeOnly;
        plan.holding = Some(hft_research_manifest::model::HorizonHoldingPolicyV1 {
            horizon_millis: 5000,
        });
        plan.label_horizon =
            Some(alpha_domain::campaign_horizon::CampaignLabelHorizonV1::canonical());
        plan.comparison_family_trials = Some(138);
        let root = tempfile::tempdir().unwrap();
        let input = root.path().join("plan.json");
        let output = root.path().join("precheck.json");
        let checked = root.path().join("checked-plan.json");
        data_mission::write_json_atomic(&input, &plan).unwrap();
        precheck(CampaignPrecheckArgs {
            feature: fixture.feature_path.clone(),
            materialization: fixture.materialization_path.clone(),
            research_plan: input,
            output: output.clone(),
            research_plan_out: checked.clone(),
        })
        .unwrap();
        let receipt: DevelopmentPrecheckReceiptV1 =
            serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap();
        let checked: CexCampaignResearchPlanV1 =
            serde_json::from_slice(&std::fs::read(checked).unwrap()).unwrap();
        assert_eq!(checked.development_precheck.as_ref(), Some(&receipt));
        assert_eq!(receipt.report.observations, 14_395);
        assert_eq!(receipt.binding().unwrap().calendar, plan.calendar.unwrap());
        let inputs =
            PreparedCexInputs::load(&fixture.feature_path, &fixture.materialization_path, true)
                .unwrap();
        inputs.verify_development_precheck(&checked).unwrap();
    }
}
