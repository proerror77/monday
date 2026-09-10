//! Typed label-horizon and next-family Campaign proposal contracts.
//!
//! A proposal is derived research evidence.  It can select only a member that
//! is already present in an authenticated Campaign Study; it never signs a
//! root grant, changes a Study, or authorizes dispatch on its own.

use crate::{
    campaign_control::CampaignExecutionBindingV1, canonical_json_hash, EvaluationLabelSpecV1,
};
use serde::{Deserialize, Serialize};

pub const CAMPAIGN_LABEL_HORIZON_SCHEMA_V1: &str = "monday.campaign_label_horizon.v1";
pub const CAMPAIGN_NEXT_FAMILY_PROPOSAL_SCHEMA_V1: &str = "monday.campaign_next_family_proposal.v1";
pub const CAMPAIGN_LABEL_TARGET_FORWARD_MID_RETURN: &str = "forward_mid_return";
pub const CAMPAIGN_FEATURE_AVAILABILITY_POLICY_V1: &str =
    "feature_available_time_equals_event_time";
pub const MAX_CAMPAIGN_LABEL_HORIZON_BUCKETS: usize = 3_600;
pub const MAX_CAMPAIGN_LABEL_FREQUENCY_MILLIS: u64 = 86_400_000;
pub const MAX_CAMPAIGN_PURGE_ROWS: usize = 100_000;
pub const MAX_CAMPAIGN_EMBARGO_ROWS: usize = 100_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignLabelHorizonV1 {
    pub schema_version: String,
    pub target_name: String,
    pub labels: EvaluationLabelSpecV1,
    pub feature_availability_policy: String,
    pub purge_rows: usize,
    pub embargo_rows: usize,
}

impl CampaignLabelHorizonV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != CAMPAIGN_LABEL_HORIZON_SCHEMA_V1
            || self.target_name != CAMPAIGN_LABEL_TARGET_FORWARD_MID_RETURN
            || self.labels.horizon_buckets == 0
            || self.labels.horizon_buckets > MAX_CAMPAIGN_LABEL_HORIZON_BUCKETS
            || self.labels.observation_frequency_millis == 0
            || self.labels.observation_frequency_millis > MAX_CAMPAIGN_LABEL_FREQUENCY_MILLIS
            || self.feature_availability_policy != CAMPAIGN_FEATURE_AVAILABILITY_POLICY_V1
            || self.purge_rows < self.labels.horizon_buckets
            || self.purge_rows > MAX_CAMPAIGN_PURGE_ROWS
            || self.embargo_rows == 0
            || self.embargo_rows > MAX_CAMPAIGN_EMBARGO_ROWS
        {
            return Err("typed Campaign label horizon is invalid".into());
        }
        Ok(())
    }

    pub fn new(
        horizon_buckets: usize,
        observation_frequency_millis: u64,
        purge_rows: usize,
        embargo_rows: usize,
    ) -> Result<Self, String> {
        let horizon = Self {
            schema_version: CAMPAIGN_LABEL_HORIZON_SCHEMA_V1.into(),
            target_name: CAMPAIGN_LABEL_TARGET_FORWARD_MID_RETURN.into(),
            labels: EvaluationLabelSpecV1 {
                horizon_buckets,
                observation_frequency_millis,
            },
            feature_availability_policy: CAMPAIGN_FEATURE_AVAILABILITY_POLICY_V1.into(),
            purge_rows,
            embargo_rows,
        };
        horizon.validate()?;
        Ok(horizon)
    }

    pub fn canonical() -> Self {
        Self::new(5, 1_000, 10, 5).expect("canonical Campaign label horizon is valid")
    }

    pub fn content_hash(&self) -> Result<String, String> {
        self.validate()?;
        canonical_json_hash(self).map_err(|error| error.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignNextFamilyInputWindowV1 {
    pub mission_id: String,
    pub output_prefix: String,
    pub start_received_at_ns: u64,
    pub end_received_at_ns: u64,
    pub bucket_ms: u64,
    pub top_depth: usize,
}

impl CampaignNextFamilyInputWindowV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.mission_id.is_empty()
            || self.mission_id.len() > 256
            || self.mission_id.chars().any(char::is_control)
            || self.output_prefix.is_empty()
            || self.output_prefix.len() > 512
            || self.output_prefix.starts_with('/')
            || self.output_prefix.split('/').any(|part| {
                part.is_empty() || part == "." || part == ".." || part.chars().any(char::is_control)
            })
            || self.start_received_at_ns >= self.end_received_at_ns
            || self.bucket_ms == 0
            || self.top_depth == 0
        {
            return Err("next-family input window is invalid".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignNextFamilyParentV1 {
    pub campaign_id: String,
    pub family_id: String,
    pub root_grant_sha256: String,
    pub request_sha256: String,
    /// The authenticated parent Campaign input receipt.  A Study handoff may
    /// intentionally select a different target receipt, so this remains
    /// separate from the target member's execution binding.
    pub campaign_inputs_sha256: String,
    pub campaign_result_sha256: String,
    pub family_settlement_receipt_sha256: String,
    pub study_settlement_receipt_sha256: String,
    pub study_snapshot_sha256: String,
    pub terminal_job_uid: String,
    pub terminal_pod_uid: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignNextFamilyProposalV1 {
    pub schema_version: String,
    pub study_id: String,
    pub study_grant_sha256: String,
    pub parent: CampaignNextFamilyParentV1,
    pub target_family_id: String,
    pub target_root_grant_sha256: String,
    pub target_member_sha256: String,
    pub target_execution: CampaignExecutionBindingV1,
    pub target_horizon: CampaignLabelHorizonV1,
    pub target_horizon_sha256: String,
    pub target_window: CampaignNextFamilyInputWindowV1,
    pub target_research_plan_sha256: String,
}

impl CampaignNextFamilyProposalV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != CAMPAIGN_NEXT_FAMILY_PROPOSAL_SCHEMA_V1
            || !valid_identifier(&self.study_id)
            || !valid_digest(&self.study_grant_sha256)
            || self.parent.family_id == self.target_family_id
            || !valid_identifier(&self.parent.campaign_id)
            || !valid_identifier(&self.parent.family_id)
            || !valid_digest(&self.parent.root_grant_sha256)
            || !valid_digest(&self.parent.request_sha256)
            || !valid_digest(&self.parent.campaign_inputs_sha256)
            || !valid_digest(&self.parent.campaign_result_sha256)
            || !valid_digest(&self.parent.family_settlement_receipt_sha256)
            || !valid_digest(&self.parent.study_settlement_receipt_sha256)
            || !valid_digest(&self.parent.study_snapshot_sha256)
            || !valid_identifier(&self.parent.terminal_job_uid)
            || !valid_identifier(&self.parent.terminal_pod_uid)
            || !valid_identifier(&self.target_family_id)
            || !valid_digest(&self.target_root_grant_sha256)
            || !valid_digest(&self.target_member_sha256)
            || !valid_digest(&self.target_horizon_sha256)
            || !valid_digest(&self.target_research_plan_sha256)
        {
            return Err("next-family proposal is invalid".into());
        }
        self.target_execution
            .validate()
            .map_err(|_| "next-family proposal is invalid".to_string())?;
        self.target_horizon.validate()?;
        if self.target_horizon.content_hash()? != self.target_horizon_sha256 {
            return Err("next-family proposal is invalid".into());
        }
        self.target_window.validate()?;
        Ok(())
    }

    pub fn content_hash(&self) -> Result<String, String> {
        self.validate()?;
        canonical_json_hash(self).map_err(|error| error.to_string())
    }
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::campaign_control::{CampaignEvaluationViewsV1, CampaignSelectionFeedbackV1};

    fn execution() -> CampaignExecutionBindingV1 {
        CampaignExecutionBindingV1 {
            campaign_inputs_sha256: "1".repeat(64),
            evaluation_protocol_sha256: "2".repeat(64),
            evaluation_views: CampaignEvaluationViewsV1 {
                search_view_sha256: "3".repeat(64),
                selection_view_sha256: "4".repeat(64),
                selection_feedback: CampaignSelectionFeedbackV1::IndependentSelectionWithheld,
            },
            source_revision: "a".repeat(40),
            runner_image: format!("registry/runner@sha256:{}", "5".repeat(64)),
            controller_image: format!("registry/controller@sha256:{}", "6".repeat(64)),
            job_cpu_millis: 1,
            job_memory_mib: 1,
        }
    }

    fn proposal() -> CampaignNextFamilyProposalV1 {
        let horizon = CampaignLabelHorizonV1::canonical();
        CampaignNextFamilyProposalV1 {
            schema_version: CAMPAIGN_NEXT_FAMILY_PROPOSAL_SCHEMA_V1.into(),
            study_id: "study-1".into(),
            study_grant_sha256: "7".repeat(64),
            parent: CampaignNextFamilyParentV1 {
                campaign_id: "cex-campaign-parent".into(),
                family_id: "family-parent".into(),
                root_grant_sha256: "8".repeat(64),
                request_sha256: "9".repeat(64),
                campaign_inputs_sha256: "1".repeat(64),
                campaign_result_sha256: "a".repeat(64),
                family_settlement_receipt_sha256: "b".repeat(64),
                study_settlement_receipt_sha256: "c".repeat(64),
                study_snapshot_sha256: "d".repeat(64),
                terminal_job_uid: "job-uid".into(),
                terminal_pod_uid: "pod-uid".into(),
            },
            target_family_id: "family-target".into(),
            target_root_grant_sha256: "e".repeat(64),
            target_member_sha256: "f".repeat(64),
            target_execution: execution(),
            target_horizon_sha256: horizon.content_hash().unwrap(),
            target_horizon: horizon,
            target_window: CampaignNextFamilyInputWindowV1 {
                mission_id: "target-window".into(),
                output_prefix: "target/output".into(),
                start_received_at_ns: 1,
                end_received_at_ns: 2,
                bucket_ms: 1_000,
                top_depth: 5,
            },
            target_research_plan_sha256: "0".repeat(64),
        }
    }

    #[test]
    fn horizon_requires_purge_and_has_stable_hash() {
        let horizon = CampaignLabelHorizonV1::new(20, 1_000, 20, 5).unwrap();
        let same = serde_json::from_slice::<CampaignLabelHorizonV1>(
            &serde_json::to_vec(&horizon).unwrap(),
        )
        .unwrap();
        assert_eq!(horizon.content_hash(), same.content_hash());
        assert!(CampaignLabelHorizonV1::new(20, 1_000, 19, 5).is_err());
    }

    #[test]
    fn proposal_rejects_parent_or_target_identity_drift() {
        let mut changed_parent = proposal();
        changed_parent.parent.campaign_result_sha256 = "z".repeat(64);
        assert!(changed_parent.validate().is_err());
        let mut changed_horizon = proposal();
        changed_horizon.target_horizon.labels.horizon_buckets = 10;
        assert!(changed_horizon.validate().is_err());
    }

    #[test]
    fn input_window_rejects_latest_or_unsafe_paths() {
        let mut window = CampaignNextFamilyInputWindowV1 {
            mission_id: "target".into(),
            output_prefix: "target".into(),
            start_received_at_ns: 1,
            end_received_at_ns: 2,
            bucket_ms: 1_000,
            top_depth: 5,
        };
        window.output_prefix = "../target".into();
        assert!(window.validate().is_err());
    }
}
