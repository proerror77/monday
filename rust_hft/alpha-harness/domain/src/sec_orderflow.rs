//! Second-level orderflow experiment protocol. Research-only; no runtime authority.

use hft_research_manifest::sec_orderflow::{
    SecOrderflowAvailabilityQualityV1, SecOrderflowError, SecOrderflowExperimentManifestV1,
    SecOrderflowTargetKindV1,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const SEC_ORDERFLOW_ELIGIBILITY_SCHEMA_V1: &str = "sec-orderflow-eligibility-v1";
pub const SEC_ORDERFLOW_AUDIT_REPORT_SCHEMA_V1: &str = "sec-orderflow-audit-report-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecOrderflowInputStatusV1 {
    Explicit,
    Missing,
    Unavailable,
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecOrderflowGateStatusV1 {
    pub passed: bool,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecOrderflowEligibilityV1 {
    pub schema_version: String,
    pub research_eligible: bool,
    pub training_admitted: bool,
    pub live_admitted: bool,
    pub sealed_holdout_open: bool,
    pub jobs_dispatched: u64,
    pub rejection_reasons: Vec<String>,
    pub gates: BTreeMap<String, SecOrderflowGateStatusV1>,
}

impl SecOrderflowEligibilityV1 {
    pub fn from_audit(
        manifest: &SecOrderflowExperimentManifestV1,
        input_status: SecOrderflowInputStatusV1,
        g0_reason: &str,
        calendar_days: u32,
    ) -> Result<Self, SecOrderflowError> {
        manifest.validate()?;
        let mut gates = BTreeMap::new();
        let input_ok = matches!(input_status, SecOrderflowInputStatusV1::Explicit);
        let g0_passed = input_ok
            && g0_reason.is_empty()
            && manifest.legacy.availability_quality == SecOrderflowAvailabilityQualityV1::Verified;
        gates.insert(
            "G0_data_correctness".to_string(),
            SecOrderflowGateStatusV1 {
                passed: g0_passed,
                reason: if !input_ok {
                    input_rejection(input_status).to_string()
                } else if g0_passed {
                    "source clocks and freshness are verified".to_string()
                } else if g0_reason.is_empty() {
                    "legacy_availability_quality_unknown".to_string()
                } else {
                    g0_reason.to_string()
                },
            },
        );
        let _ = calendar_days;
        gates.insert(
            "G1_exploratory_fit".to_string(),
            fail_gate(false, "insufficient_qualified_days_and_no_fit"),
        );
        gates.insert(
            "G2_formal_comparison".to_string(),
            fail_gate(false, "insufficient_calendar_days_for_formal_comparison"),
        );
        gates.insert(
            "G3_strategy_sample".to_string(),
            fail_gate(false, "no_finished_nonoverlapping_trades"),
        );
        gates.insert(
            "G4_dl_upgrade".to_string(),
            fail_gate(false, "dl_stage_closed"),
        );
        gates.insert(
            "G5_execution_capability".to_string(),
            fail_gate(false, "queue_fill_and_fee_evidence_missing"),
        );
        gates.insert(
            "run_enabled".to_string(),
            fail_gate(false, "run_enabled_false"),
        );
        gates.insert(
            "approved_grant".to_string(),
            fail_gate(false, "approved_grant_null"),
        );
        gates.insert(
            "live_authority".to_string(),
            fail_gate(false, "orders_live_risk_resume_deployment_promotion_closed"),
        );
        gates.insert(
            "sealed_holdout".to_string(),
            fail_gate(false, "holdout_open_false"),
        );

        let mut rejection_reasons = gates
            .values()
            .filter(|gate| !gate.passed)
            .map(|gate| gate.reason.clone())
            .collect::<Vec<_>>();
        rejection_reasons.sort();
        rejection_reasons.dedup();

        Ok(Self {
            schema_version: SEC_ORDERFLOW_ELIGIBILITY_SCHEMA_V1.to_string(),
            research_eligible: g0_passed,
            training_admitted: false,
            live_admitted: false,
            sealed_holdout_open: false,
            jobs_dispatched: 0,
            rejection_reasons,
            gates,
        })
    }
}

fn fail_gate(passed: bool, reason: &str) -> SecOrderflowGateStatusV1 {
    SecOrderflowGateStatusV1 {
        passed,
        reason: if passed {
            "passed".to_string()
        } else {
            reason.to_string()
        },
    }
}

pub fn input_rejection(status: SecOrderflowInputStatusV1) -> &'static str {
    match status {
        SecOrderflowInputStatusV1::Explicit => "input_explicit",
        SecOrderflowInputStatusV1::Missing => "input_root_missing",
        SecOrderflowInputStatusV1::Unavailable => "input_root_unavailable",
        SecOrderflowInputStatusV1::Empty => "input_root_empty",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecOrderflowTargetCountV1 {
    pub symbol: String,
    pub horizon_s: u16,
    pub price: String,
    pub target: SecOrderflowTargetKindV1,
    pub valid: u64,
    pub invalid: u64,
    pub rejection_reasons: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecOrderflowFileDigestV1 {
    pub path: String,
    pub rows: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecOrderflowAuditIdentitiesV1 {
    pub config_sha256: String,
    pub input_list_sha256: Option<String>,
    pub source_files: Vec<SecOrderflowFileDigestV1>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecOrderflowAuditReportV1 {
    pub schema_version: String,
    pub diagnostic: bool,
    pub mode: String,
    pub input_status: SecOrderflowInputStatusV1,
    pub identities: SecOrderflowAuditIdentitiesV1,
    pub eligibility: SecOrderflowEligibilityV1,
    pub target_counts: Vec<SecOrderflowTargetCountV1>,
    pub applied_split: String,
    pub notes: Vec<String>,
}

impl SecOrderflowAuditReportV1 {
    pub fn unavailable(
        manifest: &SecOrderflowExperimentManifestV1,
        status: SecOrderflowInputStatusV1,
    ) -> Result<Self, SecOrderflowError> {
        let config_sha256 = manifest.sha256()?;
        let eligibility = SecOrderflowEligibilityV1::from_audit(manifest, status, "", 0)?;
        Ok(Self {
            schema_version: SEC_ORDERFLOW_AUDIT_REPORT_SCHEMA_V1.to_string(),
            diagnostic: true,
            mode: "audit_only".to_string(),
            input_status: status,
            identities: SecOrderflowAuditIdentitiesV1 {
                config_sha256,
                input_list_sha256: None,
                source_files: Vec::new(),
            },
            eligibility,
            target_counts: Vec::new(),
            applied_split: "not_applied_input_unavailable".to_string(),
            notes: vec![
                input_rejection(status).to_string(),
                "mac_collector_paths_are_never_defaulted".to_string(),
                "no_jobs_dispatched".to_string(),
                "no_training".to_string(),
                "no_live_paper_shadow".to_string(),
            ],
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecOrderflowExperimentV1 {
    pub manifest: SecOrderflowExperimentManifestV1,
    pub dataset_manifest_sha256: Option<String>,
    pub ordered_feature_hash: Option<String>,
    pub label_hash: Option<String>,
    pub split_hash: Option<String>,
    pub cost_hash: Option<String>,
}

impl SecOrderflowExperimentV1 {
    pub fn from_manifest(
        manifest: SecOrderflowExperimentManifestV1,
    ) -> Result<Self, SecOrderflowError> {
        manifest.validate()?;
        Ok(Self {
            manifest,
            dataset_manifest_sha256: None,
            ordered_feature_hash: None,
            label_hash: None,
            split_hash: None,
            cost_hash: None,
        })
    }

    pub fn validate(&self) -> Result<(), SecOrderflowError> {
        self.manifest.validate()?;
        if self.dataset_manifest_sha256.is_some()
            || self.ordered_feature_hash.is_some()
            || self.label_hash.is_some()
            || self.split_hash.is_some()
            || self.cost_hash.is_some()
        {
            return Err(SecOrderflowError::Invalid(
                "P0 audit cannot bind training hashes or admit a governed dataset",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_input_is_ineligible_and_has_no_runtime_authority() {
        let manifest = SecOrderflowExperimentManifestV1::canonical_audit();
        let report =
            SecOrderflowAuditReportV1::unavailable(&manifest, SecOrderflowInputStatusV1::Missing)
                .unwrap();
        assert!(!report.eligibility.research_eligible);
        assert!(!report.eligibility.training_admitted);
        assert!(!report.eligibility.live_admitted);
        assert!(!report.eligibility.sealed_holdout_open);
        assert_eq!(report.eligibility.jobs_dispatched, 0);
        assert!(report
            .eligibility
            .rejection_reasons
            .iter()
            .any(|reason| reason == "input_root_missing"));
        assert!(report.target_counts.is_empty());
        assert!(report.identities.input_list_sha256.is_none());
        assert!(report.diagnostic);
    }

    #[test]
    fn experiment_refuses_training_hash_placeholders() {
        let mut experiment = SecOrderflowExperimentV1::from_manifest(
            SecOrderflowExperimentManifestV1::canonical_audit(),
        )
        .unwrap();
        experiment.dataset_manifest_sha256 = Some("0".repeat(64));
        assert!(experiment.validate().is_err());
    }
}
