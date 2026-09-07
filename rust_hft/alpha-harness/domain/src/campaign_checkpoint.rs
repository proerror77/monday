//! Wire contracts emitted by the Campaign controller's recovery checkpoints.
//!
//! These types validate structure and intrinsic consistency. Admission still
//! requires independently verifying the referenced file bytes, parent identity,
//! authorization, and published receipts. Deserialization alone grants no authority.
//! Hash and republish original bytes; serialization does not preserve byte identity.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CampaignLearningCheckpointSchemaV1 {
    #[serde(rename = "monday.campaign_learning_checkpoint.v1")]
    V1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CampaignGenerationCompletionSchemaV1 {
    #[serde(rename = "monday.campaign_generation_completion.v1")]
    V1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CampaignLearningOutcomeV1 {
    FollowUp,
    NoImprovement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CampaignGenerationOutcomeV1 {
    FollowUp,
    Complete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignLearningCheckpointV1 {
    pub schema_version: CampaignLearningCheckpointSchemaV1,
    pub request_sha256: String,
    pub campaign_result_sha256: String,
    pub learn_report_sha256: String,
    /// The existing v1 wire format uses an empty string when no plan exists.
    pub research_plan_file_sha256: String,
    pub outcome: CampaignLearningOutcomeV1,
}

impl CampaignLearningCheckpointV1 {
    pub fn validate(&self) -> anyhow::Result<()> {
        require_sha(&self.request_sha256)?;
        require_sha(&self.campaign_result_sha256)?;
        require_sha(&self.learn_report_sha256)?;
        match self.outcome {
            CampaignLearningOutcomeV1::FollowUp => require_sha(&self.research_plan_file_sha256)?,
            CampaignLearningOutcomeV1::NoImprovement => anyhow::ensure!(
                self.research_plan_file_sha256.is_empty(),
                "no-improvement checkpoint must not bind a child plan"
            ),
        }
        Ok(())
    }
}

/// The terminal projection embedded in a generation completion, not a new
/// Campaign result schema. Its evidence must be matched to generation-report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignCycleResultV1 {
    pub generation: u32,
    pub campaign_id: String,
    pub request_sha256: String,
    pub job_name: String,
    pub campaign_result_sha256: String,
    pub termination_reason: String,
    pub observed_image_id: String,
    pub learning_directive_sha256: String,
    pub search_policy_revision_id: String,
    pub round_readback_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounded_loop_exhausted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub learning_outcome: Option<CampaignLearningOutcomeV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub learn_report_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub learn_report_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignGenerationCompletionV1 {
    pub schema_version: CampaignGenerationCompletionSchemaV1,
    pub generation: u32,
    pub outcome: CampaignGenerationOutcomeV1,
    pub campaign_pod_name: String,
    pub generation_report_sha256: String,
    /// Empty when this generation terminated without invoking learn.
    pub learning_checkpoint_sha256: String,
    pub cycle_result: Option<CampaignCycleResultV1>,
}

impl CampaignGenerationCompletionV1 {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.campaign_pod_name.trim().is_empty(),
            "missing Campaign Pod identity"
        );
        require_sha(&self.generation_report_sha256)?;
        if !self.learning_checkpoint_sha256.is_empty() {
            require_sha(&self.learning_checkpoint_sha256)?;
        }
        match self.outcome {
            CampaignGenerationOutcomeV1::FollowUp => {
                require_sha(&self.learning_checkpoint_sha256)?;
                anyhow::ensure!(
                    self.cycle_result.is_none(),
                    "follow-up cannot contain a terminal projection"
                );
            }
            CampaignGenerationOutcomeV1::Complete => {
                let result = self
                    .cycle_result
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("completion lacks its terminal projection"))?;
                anyhow::ensure!(
                    result.generation == self.generation,
                    "terminal projection generation mismatch"
                );
                require_sha(&result.request_sha256)?;
                require_sha(&result.campaign_result_sha256)?;
                match result.learning_outcome {
                    Some(CampaignLearningOutcomeV1::FollowUp) => {
                        anyhow::bail!("follow-up learning cannot terminate a generation");
                    }
                    Some(CampaignLearningOutcomeV1::NoImprovement) => {
                        require_sha(&self.learning_checkpoint_sha256)?;
                        anyhow::ensure!(
                            result.termination_reason == "no_improvement",
                            "learning termination reason mismatch"
                        );
                        anyhow::ensure!(
                            result.bounded_loop_exhausted.is_none(),
                            "learned completion cannot also exhaust the generation bound"
                        );
                        anyhow::ensure!(
                            result
                                .learn_report_url
                                .as_ref()
                                .is_some_and(|url| !url.trim().is_empty()),
                            "learned completion lacks its report location"
                        );
                        require_sha(result.learn_report_sha256.as_deref().unwrap_or(""))?;
                    }
                    None => {
                        anyhow::ensure!(
                            self.learning_checkpoint_sha256.is_empty()
                                && result.learn_report_url.is_none()
                                && result.learn_report_sha256.is_none(),
                            "unlearned completion contains learning evidence"
                        );
                        anyhow::ensure!(
                            result.termination_reason != "no_improvement",
                            "no-improvement completion lacks learning evidence"
                        );
                        if result.termination_reason == "campaign_no_candidate" {
                            anyhow::ensure!(
                                result.bounded_loop_exhausted == Some(true),
                                "negative unlearned completion must exhaust the generation bound"
                            );
                        } else {
                            anyhow::ensure!(
                                result.bounded_loop_exhausted.is_none(),
                                "nonnegative completion cannot exhaust the generation bound"
                            );
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

fn require_sha(value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        value.len() == 64
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "invalid lowercase SHA-256"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const LEARN: [&str; 2] = [
        include_str!("../tests/fixtures/campaign-checkpoints/learning-follow-up.json"),
        include_str!("../tests/fixtures/campaign-checkpoints/learning-no-improvement.json"),
    ];
    const COMPLETE: [&str; 2] = [
        include_str!("../tests/fixtures/campaign-checkpoints/completion-follow-up.json"),
        include_str!("../tests/fixtures/campaign-checkpoints/completion-no-improvement.json"),
    ];

    #[test]
    fn consumes_actual_shell_controller_checkpoints_without_losing_fields() {
        for json in LEARN {
            let checkpoint: CampaignLearningCheckpointV1 = serde_json::from_str(json).unwrap();
            checkpoint.validate().unwrap();
            assert_eq!(
                serde_json::to_value(checkpoint).unwrap(),
                serde_json::from_str::<serde_json::Value>(json).unwrap()
            );
        }
        for json in COMPLETE {
            let checkpoint: CampaignGenerationCompletionV1 = serde_json::from_str(json).unwrap();
            checkpoint.validate().unwrap();
            assert_eq!(
                serde_json::to_value(checkpoint).unwrap(),
                serde_json::from_str::<serde_json::Value>(json).unwrap()
            );
        }
    }

    #[test]
    fn completion_binds_original_learning_checkpoint_bytes() {
        use sha2::{Digest, Sha256};
        for (learn_json, completion_json) in LEARN.into_iter().zip(COMPLETE) {
            let completion: CampaignGenerationCompletionV1 =
                serde_json::from_str(completion_json).unwrap();
            assert_eq!(
                completion.learning_checkpoint_sha256,
                hex::encode(Sha256::digest(learn_json.as_bytes()))
            );
        }
    }

    #[test]
    fn terminal_learning_fields_form_coherent_variants() {
        let learned: serde_json::Value = serde_json::from_str(COMPLETE[1]).unwrap();
        let mut ordinary = learned.clone();
        ordinary["learning_checkpoint_sha256"] = "".into();
        let result = ordinary["cycle_result"].as_object_mut().unwrap();
        for key in [
            "learning_outcome",
            "learn_report_url",
            "learn_report_sha256",
        ] {
            result.remove(key);
        }
        result.insert("termination_reason".into(), "campaign_finalized".into());
        let mut bounded = ordinary.clone();
        bounded["cycle_result"]["termination_reason"] = "campaign_no_candidate".into();
        bounded["cycle_result"]["bounded_loop_exhausted"] = true.into();
        for valid in [&learned, &ordinary, &bounded] {
            serde_json::from_value::<CampaignGenerationCompletionV1>(valid.clone())
                .unwrap()
                .validate()
                .unwrap();
        }
        let cases = [
            (
                &learned,
                "/learning_checkpoint_sha256",
                serde_json::json!(""),
            ),
            (
                &learned,
                "/cycle_result/learning_outcome",
                serde_json::json!("follow_up"),
            ),
            (
                &learned,
                "/cycle_result/learn_report_url",
                serde_json::Value::Null,
            ),
            (
                &learned,
                "/cycle_result/learn_report_url",
                serde_json::json!(" "),
            ),
            (
                &learned,
                "/cycle_result/learn_report_sha256",
                serde_json::Value::Null,
            ),
            (
                &learned,
                "/cycle_result/learn_report_sha256",
                serde_json::json!("bad"),
            ),
            (
                &learned,
                "/cycle_result/termination_reason",
                serde_json::json!("campaign_finalized"),
            ),
            (
                &learned,
                "/cycle_result/learning_outcome",
                serde_json::Value::Null,
            ),
            (
                &ordinary,
                "/learning_checkpoint_sha256",
                serde_json::json!("a".repeat(64)),
            ),
            (
                &ordinary,
                "/cycle_result/termination_reason",
                serde_json::json!("no_improvement"),
            ),
            (
                &ordinary,
                "/cycle_result/termination_reason",
                serde_json::json!("campaign_no_candidate"),
            ),
            (
                &bounded,
                "/cycle_result/bounded_loop_exhausted",
                serde_json::json!(false),
            ),
            (
                &bounded,
                "/cycle_result/termination_reason",
                serde_json::json!("campaign_finalized"),
            ),
        ];
        for (source, path, value) in cases {
            let mut invalid = source.clone();
            *invalid.pointer_mut(path).unwrap() = value;
            let checkpoint: CampaignGenerationCompletionV1 =
                serde_json::from_value(invalid).unwrap();
            assert!(
                checkpoint.validate().is_err(),
                "accepted invalid {path}: {checkpoint:?}"
            );
        }
        for source in [&learned, &ordinary] {
            let mut invalid = source.clone();
            invalid["cycle_result"]["bounded_loop_exhausted"] = true.into();
            assert!(
                serde_json::from_value::<CampaignGenerationCompletionV1>(invalid)
                    .unwrap()
                    .validate()
                    .is_err()
            );
        }
        for field in ["learn_report_url", "learn_report_sha256"] {
            let mut invalid = ordinary.clone();
            invalid["cycle_result"][field] = learned["cycle_result"][field].clone();
            assert!(
                serde_json::from_value::<CampaignGenerationCompletionV1>(invalid)
                    .unwrap()
                    .validate()
                    .is_err()
            );
        }
    }

    #[test]
    fn rejects_schema_drift_and_inconsistent_transitions() {
        assert!(serde_json::from_str::<CampaignLearningCheckpointV1>(
            &LEARN[0].replace("checkpoint.v1", "checkpoint.v2")
        )
        .is_err());
        let mut value: serde_json::Value = serde_json::from_str(COMPLETE[0]).unwrap();
        value["unknown_authority"] = true.into();
        assert!(serde_json::from_value::<CampaignGenerationCompletionV1>(value).is_err());
        assert!(serde_json::from_str::<CampaignGenerationCompletionV1>("").is_err());
        let mut learn: CampaignLearningCheckpointV1 = serde_json::from_str(LEARN[0]).unwrap();
        learn.outcome = CampaignLearningOutcomeV1::NoImprovement;
        assert!(learn.validate().is_err());
        learn.research_plan_file_sha256.clear();
        learn.request_sha256 = "bad".into();
        assert!(learn.validate().is_err());
        let mut complete: CampaignGenerationCompletionV1 =
            serde_json::from_str(COMPLETE[1]).unwrap();
        complete.generation += 1;
        assert!(complete.validate().is_err());
        complete.generation -= 1;
        complete.outcome = CampaignGenerationOutcomeV1::FollowUp;
        assert!(complete.validate().is_err());
        complete.cycle_result = None;
        complete.learning_checkpoint_sha256.clear();
        assert!(complete.validate().is_err());
    }
}
