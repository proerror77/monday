//! Operator-owned root authority is separate from the finalized worker request.
//! Every charge is derived from the request and the actual rendered Job.

use super::{image_digest, ValidatedSubmission};
use crate::{
    cli::BUILD_SOURCE_REVISION,
    mission_render::{
        approved_evaluation_protocol_for_horizon, approved_validation,
        validate_render_materialization_scope_for_horizon,
    },
    mission_runner::{decode_materialization, validate_materialization, MAX_MATERIALIZATION_BYTES},
    prediction_dispatch::canonical_tokyo_oss_internal_object,
};
use alpha_domain::{
    campaign_control::{
        verify_campaign_root_grant, CampaignAttemptReservationV1, CampaignEvaluationViewsV1,
        CampaignExecutionBindingV1, CampaignSelectionFeedbackV1, SignedCampaignRootGrantV1,
        VerifiedCampaignRootGrant, ATTEMPT_SCHEMA,
    },
    canonical_json_hash,
};
use alpha_store::{
    campaign_ledger::{
        CampaignDispatchClaimV1, CampaignDispatchRecord, CampaignDispatchSettlementV1,
        CampaignDispatchTargetV1, CampaignLedgerEventV1, CampaignStudyLedgerEventV1,
        CampaignStudySnapshotV1,
    },
    AlphaStore,
};
use anyhow::{bail, Context};
use chrono::Utc;
use ed25519_dalek::VerifyingKey;
use reqwest::blocking::Client;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

const CONTROL_SCHEMA: &str = "monday.campaign_dispatch_control.v1";
const MAX_CONTROL_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DispatchControl {
    pub(super) schema_version: String,
    pub(super) ledger_path: PathBuf,
    pub(super) signed_root_grant_path: PathBuf,
    pub(super) trusted_keys_path: PathBuf,
    pub(super) materialization_path: PathBuf,
    #[serde(default)]
    pub(super) campaign_inputs_path: Option<PathBuf>,
    pub(super) approval_id: String,
    pub(super) controller_image: String,
    pub(super) attempt_ordinal: u32,
    pub(super) receipt_access: BTreeMap<String, ReceiptAccess>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReceiptAccess {
    put_url: String,
    readback_url: String,
}

#[derive(Debug, Serialize)]
pub(super) struct DispatchInspection {
    pub(super) execution: CampaignExecutionBindingV1,
    campaign_id: String,
    generation: u8,
    parent_result_sha256: Option<String>,
    policy_revision_id: String,
    request_sha256: String,
    attempt_ordinal: u32,
    declared_trials: u64,
    reserved_job_seconds: u64,
    reserved_llm_tokens: u64,
}

impl DispatchInspection {
    pub(super) fn reservation(
        &self,
        grant: &VerifiedCampaignRootGrant,
    ) -> CampaignAttemptReservationV1 {
        self.reservation_for(grant.content_sha256(), &grant.grant().family.family_id)
    }

    fn reservation_for(&self, grant_sha256: &str, family_id: &str) -> CampaignAttemptReservationV1 {
        CampaignAttemptReservationV1 {
            schema_version: ATTEMPT_SCHEMA.into(),
            root_grant_sha256: grant_sha256.into(),
            family_id: family_id.into(),
            campaign_id: self.campaign_id.clone(),
            execution: self.execution.clone(),
            generation: self.generation,
            parent_result_sha256: self.parent_result_sha256.clone(),
            policy_revision_id: self.policy_revision_id.clone(),
            request_sha256: self.request_sha256.clone(),
            attempt_ordinal: self.attempt_ordinal,
            declared_trials: self.declared_trials,
            reserved_job_seconds: self.reserved_job_seconds,
            reserved_llm_tokens: self.reserved_llm_tokens,
        }
    }

    pub(super) fn historical_reservation_for(
        &self,
        grant_sha256: &str,
        family_id: &str,
    ) -> CampaignAttemptReservationV1 {
        self.reservation_for(grant_sha256, family_id)
    }
}

pub(super) fn inspect_binding(
    validated: &ValidatedSubmission,
    manifest: &Value,
    materialization_path: &Path,
    controller_image: &str,
    attempt_ordinal: u32,
) -> anyhow::Result<DispatchInspection> {
    let request = &validated.submission.request;
    if request.build_source_revision != BUILD_SOURCE_REVISION {
        bail!("Campaign dispatcher source revision differs from the execution request");
    }
    image_digest(controller_image)?;
    let bytes = read_bounded(materialization_path, MAX_MATERIALIZATION_BYTES)?;
    if hex::encode(Sha256::digest(&bytes)) != request.materialization_sha256 {
        bail!("dispatch materialization SHA256 differs from the execution request");
    }
    let materialization = decode_materialization(&bytes)?;
    validate_render_materialization_scope_for_horizon(
        &materialization,
        request.research_plan.label_horizon.as_ref(),
    )?;
    validate_materialization(
        &materialization,
        &request.feature_sha256,
        &approved_validation(&materialization)?,
    )?;
    let protocol = approved_evaluation_protocol_for_horizon(
        &materialization,
        request.research_plan.label_horizon.as_ref(),
    )?;
    let protocol_sha256 = protocol.content_hash()?;
    // The same partition function is used by PreparedDataset readers. Bind the
    // complete protocol, exact data identity and source, not just two view labels.
    let partitions = protocol.row_partitions(materialization.rows)?;
    let selection = partitions
        .selection
        .as_ref()
        .context("canonical Campaign requires a withheld independent selection window")?;
    let view_hash = |purpose: &str, range: &std::ops::Range<usize>| {
        canonical_json_hash(&serde_json::json!({
            "schema_version": "monday.campaign_evaluation_view.v2",
            "purpose": purpose,
            "materialization_sha256": request.materialization_sha256,
            "feature_sha256": request.feature_sha256,
            "snapshot_sha256": materialization.snapshot.sha256(),
            "evaluation_protocol_sha256": protocol_sha256,
            "source_revision": request.build_source_revision,
            "rows": range,
        }))
    };
    let search_view_sha256 = view_hash("search_and_learning", &partitions.search)?;
    let selection_view_sha256 = view_hash("independent_selection_withheld", selection)?;
    let job = &manifest["items"][1];
    let container = &job["spec"]["template"]["spec"]["containers"][0];
    if !container["args"]
        .as_array()
        .is_some_and(|args| args.iter().any(|arg| arg == "--pre-holdout"))
    {
        bail!("root-authorized Campaign Job must stop before sealed holdout");
    }
    let cpu = container["resources"]["limits"]["cpu"]
        .as_str()
        .and_then(|s| s.strip_suffix('m'))
        .context("Job CPU must be expressed in millicores")?
        .parse()?;
    let memory_gib: u32 = container["resources"]["limits"]["memory"]
        .as_str()
        .and_then(|s| s.strip_suffix("Gi"))
        .context("Job memory must be expressed in GiB")?
        .parse()?;
    let execution = CampaignExecutionBindingV1 {
        campaign_inputs_sha256: request.campaign_inputs_sha256.clone(),
        evaluation_protocol_sha256: protocol_sha256,
        evaluation_views: CampaignEvaluationViewsV1 {
            search_view_sha256,
            selection_view_sha256,
            selection_feedback: CampaignSelectionFeedbackV1::IndependentSelectionWithheld,
        },
        source_revision: request.build_source_revision.clone(),
        runner_image: container["image"]
            .as_str()
            .context("missing Job image")?
            .into(),
        controller_image: controller_image.into(),
        job_cpu_millis: cpu,
        job_memory_mib: memory_gib
            .checked_mul(1024)
            .context("Job memory overflow")?,
    };
    execution.validate()?;
    if let Some(proposal) = request.study_proposal.as_ref() {
        proposal.validate().map_err(anyhow::Error::msg)?;
        if proposal.target_execution != execution {
            bail!("next-family proposal execution binding differs from the rendered Job");
        }
        if proposal.target_horizon.labels.horizon_buckets
            != materialization.label_horizon_buckets
            || proposal.target_horizon.labels.observation_frequency_millis
                != materialization.bucket_ms
            || proposal.target_window.mission_id != materialization.mission_id
            || proposal.target_window.bucket_ms != materialization.bucket_ms
            || proposal.target_window.top_depth != materialization.top_depth
        {
            bail!("next-family proposal horizon or materialization identity differs");
        }
    }
    Ok(DispatchInspection {
        execution,
        campaign_id: request.campaign_id.clone(),
        generation: request.research_plan.generation,
        parent_result_sha256: request
            .research_plan
            .parent
            .as_ref()
            .map(|parent| parent.campaign_result_sha256.clone()),
        policy_revision_id: request
            .research_plan
            .search_policy_revision
            .revision_id
            .clone(),
        request_sha256: validated.request_sha256.clone(),
        attempt_ordinal,
        declared_trials: u64::try_from(request.declared_total_trials)?,
        reserved_job_seconds: job["spec"]["activeDeadlineSeconds"]
            .as_u64()
            .context("missing Job deadline")?,
        // Campaign learning is deterministic. The worker has no provider call
        // or LLM credential; prior optional provenance is not a new token charge.
        reserved_llm_tokens: 0,
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Purpose {
    Dispatch,
    Settlement,
}

pub(super) struct Admission {
    control: DispatchControl,
    signed: SignedCampaignRootGrantV1,
    store: AlphaStore,
    pub(super) reservation: CampaignAttemptReservationV1,
    target: CampaignDispatchTargetV1,
    receipt_origin: String,
    purpose: Purpose,
}

impl Admission {
    pub(super) fn open(
        path: &Path,
        validated: &ValidatedSubmission,
        manifest: &Value,
        context: &str,
        namespace: &str,
    ) -> anyhow::Result<Self> {
        Self::load(
            path,
            validated,
            manifest,
            context,
            namespace,
            Purpose::Dispatch,
        )
    }

    pub(super) fn open_for_settlement(
        path: &Path,
        validated: &ValidatedSubmission,
        manifest: &Value,
        context: &str,
        namespace: &str,
    ) -> anyhow::Result<Self> {
        Self::load(
            path,
            validated,
            manifest,
            context,
            namespace,
            Purpose::Settlement,
        )
    }

    fn load(
        path: &Path,
        validated: &ValidatedSubmission,
        manifest: &Value,
        context: &str,
        namespace: &str,
        purpose: Purpose,
    ) -> anyhow::Result<Self> {
        let control = read_control(path)?;
        let signed: SignedCampaignRootGrantV1 = read_json(&control.signed_root_grant_path)?;
        let inspection = inspect_binding(
            validated,
            manifest,
            &control.materialization_path,
            &control.controller_image,
            control.attempt_ordinal,
        )?;
        // Opening the existing database read/write retains DuckDB's process
        // exclusion. Never create an empty replacement database.
        let store = AlphaStore::open(&control.ledger_path)?;
        let verified = match purpose {
            Purpose::Dispatch => verify(&signed, &control.trusted_keys_path)?,
            Purpose::Settlement => {
                let operation_id = inspection
                    .reservation_for(&signed.content_sha256, &signed.grant.family.family_id)
                    .operation_id()?;
                let record = store
                    .campaign_dispatch_record(&signed.grant.family.family_id, &operation_id)?;
                if record.root.signed_grant() != &signed {
                    bail!("settlement root differs from registered authority");
                }
                record.root
            }
        };
        let reservation = inspection.reservation(&verified);
        if let Some(proposal) = validated.submission.request.study_proposal.as_ref() {
            validate_study_member_binding(
                &store,
                &signed,
                &verified,
                &inspection.execution,
                &control.campaign_inputs_path,
                &control.materialization_path,
                proposal,
            )?;
        }
        if purpose == Purpose::Dispatch {
            verified.validate_attempt_scope(&reservation, Utc::now())?;
        }
        let result_object = canonical_tokyo_oss_internal_object(
            "Campaign result",
            &validated.submission.request.campaign_result_readback_url,
        )?;
        let receipt_origin = reqwest::Url::parse(&result_object)?
            .origin()
            .ascii_serialization();
        let target = CampaignDispatchTargetV1 {
            context: context.into(),
            namespace: namespace.into(),
            job_name: validated.job_name.clone(),
            manifest_sha256: canonical_json_hash(manifest)?,
        };
        target.validate()?;
        let admission = Self {
            control,
            signed,
            store,
            reservation,
            target,
            receipt_origin,
            purpose,
        };
        if purpose == Purpose::Settlement {
            admission.record()?;
        }
        Ok(admission)
    }

    pub(super) fn record(&self) -> anyhow::Result<CampaignDispatchRecord> {
        let record = self.store.campaign_dispatch_record(
            &self.reservation.family_id,
            &self.reservation.operation_id()?,
        )?;
        if record.reservation != self.reservation || record.claim.target != self.target {
            bail!("stored dispatch differs from the submitted request or target");
        }
        Ok(record)
    }

    pub(super) fn settle(&mut self, evidence: &CampaignDispatchSettlementV1) -> anyhow::Result<()> {
        if self.purpose != Purpose::Settlement {
            bail!("settlement requires historical evidence mode");
        }
        self.record()?;
        self.store
            .settle_campaign_dispatch(&self.reservation, evidence, Utc::now())?;
        crate::mission_runner::research_event(
            "alpha-harness",
            "campaign_terminal_recorded",
            serde_json::json!({
                "operation_id": evidence.settlement.operation_id,
                "job_uid": evidence.job_uid, "pod_uid": evidence.pod_uid,
                "campaign_result_sha256": evidence.settlement.evidence_sha256,
                "consumed_trials": evidence.settlement.consumed_trials,
                "outcome": evidence.settlement.outcome,
            }),
        );
        Ok(())
    }

    pub(super) fn prepare(&mut self) -> anyhow::Result<()> {
        if self.purpose != Purpose::Dispatch {
            bail!("historical settlement authority cannot dispatch");
        }
        let verified = verify(&self.signed, &self.control.trusted_keys_path)?;
        self.store
            .register_campaign_root(&verified, &self.control.approval_id, Utc::now())?;
        self.store
            .reserve_campaign_attempt(&verified, &self.reservation, Utc::now())?;
        self.store
            .inspect_campaign_reservation(&verified, &self.reservation, Utc::now())?;
        crate::mission_runner::research_event(
            "alpha-harness",
            "campaign_reservation_ready",
            serde_json::json!({
                "operation_id": self.reservation.operation_id()?,
                "root_grant_sha256": self.reservation.root_grant_sha256,
                "family_id": self.reservation.family_id,
                "generation": self.reservation.generation,
                "campaign_id": self.reservation.campaign_id,
                "request_sha256": self.reservation.request_sha256,
                "declared_trials": self.reservation.declared_trials,
                "reserved_job_seconds": self.reservation.reserved_job_seconds,
            }),
        );
        Ok(())
    }

    pub(super) fn claim(&mut self) -> anyhow::Result<(CampaignDispatchClaimV1, bool)> {
        if self.purpose != Purpose::Dispatch {
            bail!("historical settlement authority cannot dispatch");
        }
        let verified = verify(&self.signed, &self.control.trusted_keys_path)?;
        Ok(self.store.claim_campaign_dispatch(
            &verified,
            &self.reservation,
            &self.target,
            Utc::now(),
        )?)
    }

    pub(super) fn bind_job(&mut self, uid: &str) -> anyhow::Result<()> {
        if self.purpose != Purpose::Dispatch {
            bail!("historical settlement authority cannot dispatch");
        }
        let verified = verify(&self.signed, &self.control.trusted_keys_path)?;
        self.store.bind_campaign_dispatch_job(
            &verified,
            &self.reservation,
            &self.target,
            uid,
            Utc::now(),
        )?;
        Ok(())
    }

    pub(super) fn guarded<T>(
        &mut self,
        uid: Option<&str>,
        action: impl FnOnce() -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        if self.purpose != Purpose::Dispatch {
            bail!("historical settlement authority cannot dispatch");
        }
        let verified = verify(&self.signed, &self.control.trusted_keys_path)?;
        self.store.with_campaign_dispatch_admission(
            &verified,
            &self.reservation,
            &self.target,
            uid,
            || Utc::now() + chrono::TimeDelta::seconds(30),
            action,
        )
    }

    pub(super) fn publish_receipts(&mut self) -> anyhow::Result<()> {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        self.publish_receipts_with(|access, bytes| publish_and_readback(&client, access, bytes))
    }

    pub(super) fn publish_receipts_with(
        &mut self,
        mut transfer: impl FnMut(&ReceiptAccess, &[u8]) -> anyhow::Result<Vec<u8>>,
    ) -> anyhow::Result<()> {
        if self.purpose == Purpose::Dispatch {
            let verified = verify(&self.signed, &self.control.trusted_keys_path)?;
            self.store
                .inspect_campaign_reservation(&verified, &self.reservation, Utc::now())?;
        } else {
            self.record()?;
        }
        publish_family_receipts_with(
            &mut self.store,
            &self.reservation.family_id,
            &self.receipt_origin,
            &self.control.receipt_access,
            &mut transfer,
        )?;
        if let Some(study_id) = self
            .store
            .campaign_study_id_for_family(&self.reservation.family_id)?
        {
            publish_study_receipts_with(
                &mut self.store,
                &study_id,
                &self.receipt_origin,
                &self.control.receipt_access,
                &mut transfer,
            )?;
        }
        Ok(())
    }
}

fn validate_study_member_binding(
    store: &AlphaStore,
    signed_root: &SignedCampaignRootGrantV1,
    verified_root: &VerifiedCampaignRootGrant,
    execution: &CampaignExecutionBindingV1,
    campaign_inputs_path: &Option<PathBuf>,
    materialization_path: &Path,
    proposal: &alpha_domain::campaign_horizon::CampaignNextFamilyProposalV1,
) -> anyhow::Result<()> {
    if proposal.target_family_id != verified_root.grant().family.family_id
        || proposal.target_root_grant_sha256 != signed_root.content_sha256
        || proposal.target_execution != *execution
    {
        bail!("next-family proposal does not bind the authenticated root execution");
    }
    let mapped_study = store
        .campaign_study_id_for_family(&verified_root.grant().family.family_id)?
        .context("next-family proposal root has no authenticated Study mapping")?;
    if mapped_study != proposal.study_id {
        bail!("next-family proposal Study mapping differs from the ledger");
    }
    let signed_study = store
        .campaign_study_grant(&proposal.study_id)?
        .context("next-family proposal Study grant is missing from the ledger")?;
    if signed_study.content_sha256 != proposal.study_grant_sha256 {
        bail!("next-family proposal Study grant hash differs from the ledger");
    }
    let member = signed_study
        .grant
        .members
        .iter()
        .find(|member| member.family_id == proposal.target_family_id)
        .context("next-family proposal target is not a Study member")?;
    if member.root_grant_sha256 != signed_root.content_sha256
        || member.content_hash().map_err(anyhow::Error::msg)? != proposal.target_member_sha256
        || member.execution != proposal.target_execution
        || member.label_horizon_sha256 != proposal.target_horizon_sha256
    {
        bail!("next-family proposal Study member binding differs from the ledger");
    }
    validate_study_target_window(campaign_inputs_path, materialization_path, proposal)?;
    validate_parent_settlement_binding(store, &signed_study, proposal)?;
    Ok(())
}

fn validate_study_target_window(
    campaign_inputs_path: &Option<PathBuf>,
    materialization_path: &Path,
    proposal: &alpha_domain::campaign_horizon::CampaignNextFamilyProposalV1,
) -> anyhow::Result<()> {
    let campaign_inputs_path = campaign_inputs_path
        .as_deref()
        .context("next-family proposal requires an authenticated campaign-inputs receipt path")?;
    let input_bytes = read_bounded(campaign_inputs_path, MAX_CONTROL_BYTES)?;
    if hex::encode(Sha256::digest(&input_bytes))
        != proposal.target_execution.campaign_inputs_sha256
    {
        bail!("next-family proposal campaign-inputs receipt hash differs from the Study member");
    }
    let receipt: Value = serde_json::from_slice(&input_bytes)
        .context("decode next-family campaign-inputs receipt")?;
    if receipt["mission_id"] != proposal.target_window.mission_id
        || receipt["output_prefix"]
            .as_str()
            .is_none_or(|prefix| prefix.trim_matches('/') != proposal.target_window.output_prefix)
    {
        bail!("next-family proposal target window differs from the campaign-inputs receipt");
    }
    let materialization_bytes = read_bounded(materialization_path, MAX_MATERIALIZATION_BYTES)?;
    let materialization: Value = serde_json::from_slice(&materialization_bytes)
        .context("decode next-family materialization")?;
    if materialization["mission_id"] != proposal.target_window.mission_id
        || materialization["bucket_ms"] != proposal.target_window.bucket_ms
        || materialization["top_depth"] != proposal.target_window.top_depth
        || materialization["label_horizon_buckets"]
            != proposal.target_horizon.labels.horizon_buckets
    {
        bail!("next-family proposal target window differs from the materialization");
    }
    let segments = materialization["source_segments"]
        .as_array()
        .context("next-family materialization source segments are missing")?;
    let start = segments
        .iter()
        .filter_map(|segment| segment["start_received_at_ns"].as_u64())
        .min()
        .context("next-family materialization start is missing")?;
    let end = segments
        .iter()
        .filter_map(|segment| segment["end_received_at_ns"].as_u64())
        .max()
        .context("next-family materialization end is missing")?;
    if start != proposal.target_window.start_received_at_ns
        || end != proposal.target_window.end_received_at_ns
    {
        bail!("next-family proposal target window differs from source segments");
    }
    Ok(())
}

pub(super) fn validate_parent_settlement_binding(
    store: &AlphaStore,
    signed_study: &alpha_domain::campaign_study::SignedCampaignStudyGrantV1,
    proposal: &alpha_domain::campaign_horizon::CampaignNextFamilyProposalV1,
) -> anyhow::Result<()> {
    let parent = &proposal.parent;
    let family_receipt_sha256 = store
        .campaign_family_receipts(&parent.family_id)?
        .into_iter()
        .find_map(|receipt| match &receipt.receipt.event {
            CampaignLedgerEventV1::DispatchSettled { evidence }
                if evidence.settlement.evidence_sha256 == parent.campaign_result_sha256 =>
            {
                Some((receipt.content_sha256, evidence.clone()))
            }
            _ => None,
        })
        .context("next-family proposal parent settlement is absent from the family ledger")?;
    let (family_receipt_sha256, evidence) = family_receipt_sha256;
    let operation_id = evidence.settlement.operation_id.clone();
    let record = store.campaign_dispatch_record(&parent.family_id, &operation_id)?;
    let parent_member = signed_study
        .grant
        .members
        .iter()
        .find(|member| member.family_id == parent.family_id)
        .context("parent family is not a member of the authenticated Study")?;
    if record.root.signed_grant().content_sha256 != parent.root_grant_sha256
        || record.root.grant().family.family_id != parent.family_id
        || !parent_member.matches_root(
            record.root.grant(),
            &record.root.signed_grant().content_sha256,
        )
        || record.reservation.campaign_id != parent.campaign_id
        || record.reservation.root_grant_sha256 != parent.root_grant_sha256
        || record.reservation.request_sha256 != parent.request_sha256
        || record.settlement.as_ref() != Some(&evidence.settlement)
        || record.claim.job_uid.as_deref() != Some(parent.terminal_job_uid.as_str())
        || record.terminal_pod_uid.as_deref() != Some(parent.terminal_pod_uid.as_str())
        || family_receipt_sha256 != parent.family_settlement_receipt_sha256
    {
        bail!("next-family proposal parent settlement does not match the authenticated dispatch ledger");
    }
    let snapshot = store.campaign_study_snapshot(&proposal.study_id)?;
    if study_prefix_identity(&snapshot, &parent.study_settlement_receipt_sha256)?
            != parent.study_snapshot_sha256
        || !snapshot.receipts.iter().any(|receipt| {
            receipt.content_sha256 == parent.study_settlement_receipt_sha256
                && matches!(
                    &receipt.receipt.event,
                    CampaignStudyLedgerEventV1::AttemptSettled {
                        family_id,
                        settlement,
                        family_receipt_sha256,
                    } if family_id == &parent.family_id
                        && settlement == &evidence.settlement
                        && family_receipt_sha256 == &parent.family_settlement_receipt_sha256
                )
        })
    {
        bail!("next-family proposal parent settlement does not match the authenticated Study ledger");
    }
    Ok(())
}

/// Hash the immutable Study prefix through the parent's settlement receipt.
/// Later target-family receipts remain append-only evidence and do not change
/// the historical parent identity.
pub(super) fn study_prefix_identity(
    snapshot: &CampaignStudySnapshotV1,
    settlement_receipt_sha256: &str,
) -> anyhow::Result<String> {
    let end = snapshot
        .receipts
        .iter()
        .position(|receipt| receipt.content_sha256 == settlement_receipt_sha256)
        .context("parent Study settlement receipt is absent from the current Study ledger")?;
    let prefix = &snapshot.receipts[..=end];
    let mut linked_heads: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for receipt in prefix {
        match &receipt.receipt.event {
            CampaignStudyLedgerEventV1::StudyRegistered { member_heads, .. } => {
                for (family_id, head) in member_heads {
                    linked_heads
                        .entry(family_id.clone())
                        .or_default()
                        .push(head.last_receipt_sha256.clone());
                }
            }
            CampaignStudyLedgerEventV1::AttemptReserved {
                family_id,
                family_receipt_sha256,
                ..
            }
            | CampaignStudyLedgerEventV1::AttemptSettled {
                family_id,
                family_receipt_sha256,
                ..
            } => {
                linked_heads
                    .entry(family_id.clone())
                    .or_default()
                    .push(family_receipt_sha256.clone());
            }
            CampaignStudyLedgerEventV1::ApprovalRevoked { .. } => {}
        }
    }
    let mut member_heads = BTreeMap::new();
    for family in &snapshot.member_snapshots {
        let linked = linked_heads
            .get(&family.family_id)
            .context("parent Study prefix omits a member head")?;
        let head = family
            .receipts
            .iter()
            .filter(|receipt| linked.iter().any(|hash| hash == &receipt.content_sha256))
            .max_by_key(|receipt| receipt.receipt.sequence)
            .context("parent Study prefix omits a family receipt head")?;
        member_heads.insert(
            family.family_id.clone(),
            json!({
                "sequence": head.receipt.sequence,
                "last_receipt_sha256": head.content_sha256,
                "auth_tag": head.auth_tag,
            }),
        );
    }
    Ok(canonical_json_hash(&json!({
        "schema_version": "monday.campaign_study_prefix.v1",
        "study_id": snapshot.study_id,
        "sequence": end + 1,
        "last_receipt_sha256": settlement_receipt_sha256,
        "receipts": prefix,
        "member_heads": member_heads,
    }))?)
}

pub(super) fn publish_family_receipts_with(
    store: &mut AlphaStore,
    family: &str,
    origin: &str,
    access_map: &BTreeMap<String, ReceiptAccess>,
    mut transfer: impl FnMut(&ReceiptAccess, &[u8]) -> anyhow::Result<Vec<u8>>,
) -> anyhow::Result<()> {
    let receipts = store.campaign_family_receipts(family)?;
    for receipt in receipts {
        let key = receipt.object_key();
        let access = access_map.get(&key).with_context(|| {
            format!("missing signed receipt access for {key}; reservation is retained")
        })?;
        validate_receipt_access(access, origin, &key)?;
        let bytes = receipt.publication_bytes()?;
        let observed =
            transfer(access, &bytes).with_context(|| format!("Campaign receipt {key}"))?;
        if observed != bytes {
            bail!("immutable receipt readback differs from the local authenticated bytes");
        }
        store.acknowledge_campaign_receipt_readback(
            family,
            receipt.receipt.sequence,
            &key,
            &hex::encode(Sha256::digest(&observed)),
        )?;
        crate::mission_runner::research_event(
            "alpha-harness",
            "campaign_ledger_receipt_readback_completed",
            serde_json::json!({
                "family_id": family, "sequence": receipt.receipt.sequence,
                "object_sha256": receipt.object_sha256()?,
            }),
        );
    }
    Ok(())
}

pub(super) fn publish_study_receipts_with(
    store: &mut AlphaStore,
    study_id: &str,
    origin: &str,
    access_map: &BTreeMap<String, ReceiptAccess>,
    mut transfer: impl FnMut(&ReceiptAccess, &[u8]) -> anyhow::Result<Vec<u8>>,
) -> anyhow::Result<()> {
    let receipts = store.campaign_study_receipts(study_id)?;
    for receipt in receipts {
        let key = receipt.object_key();
        let access = access_map.get(&key).with_context(|| {
            format!("missing signed Study receipt access for {key}; reservation is retained")
        })?;
        validate_receipt_access(access, origin, &key)?;
        let bytes = receipt.publication_bytes()?;
        let observed =
            transfer(access, &bytes).with_context(|| format!("Campaign Study receipt {key}"))?;
        if observed != bytes {
            bail!("immutable Study receipt readback differs from the local authenticated bytes");
        }
        store.acknowledge_campaign_study_receipt_readback(
            study_id,
            receipt.receipt.sequence,
            &key,
            &hex::encode(Sha256::digest(&observed)),
        )?;
        crate::mission_runner::research_event(
            "alpha-harness",
            "campaign_study_receipt_readback_completed",
            serde_json::json!({
                "study_id": study_id,
                "sequence": receipt.receipt.sequence,
                "object_sha256": receipt.object_sha256()?,
            }),
        );
    }
    Ok(())
}

fn validate_receipt_access(access: &ReceiptAccess, origin: &str, key: &str) -> anyhow::Result<()> {
    let expected = format!("{origin}/{key}");
    for url in [&access.put_url, &access.readback_url] {
        if canonical_tokyo_oss_internal_object("Campaign ledger receipt", url)? != expected {
            bail!("Campaign receipt URL differs from the exact sequence key or result bucket");
        }
    }
    Ok(())
}

pub(super) fn publish_and_readback(
    client: &Client,
    access: &ReceiptAccess,
    bytes: &[u8],
) -> anyhow::Result<Vec<u8>> {
    let put = client
        .put(&access.put_url)
        .header("Content-Type", "application/json")
        .header("x-oss-forbid-overwrite", "true")
        .body(bytes.to_vec())
        .send()
        .map_err(reqwest::Error::without_url)?;
    if !put.status().is_success() && put.status() != reqwest::StatusCode::CONFLICT {
        bail!("receipt create-once PUT failed with HTTP {}", put.status());
    }
    let response = client
        .get(&access.readback_url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(reqwest::Error::without_url)?;
    let mut observed = Vec::new();
    response
        .take(u64::try_from(bytes.len())? + 1)
        .read_to_end(&mut observed)?;
    Ok(observed)
}

pub(super) fn read_control(path: &Path) -> anyhow::Result<DispatchControl> {
    // Trust paths remain logical so projected-key rotation is visible. Immutable
    // input/ledger paths are physical and all relative paths are control-relative.
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let base = path
        .parent()
        .context("Campaign control file has no parent")?;
    let mut control: DispatchControl = read_json(&path)?;
    if control.schema_version != CONTROL_SCHEMA {
        bail!("unsupported Campaign dispatch control schema");
    }
    for location in [
        &mut control.ledger_path,
        &mut control.signed_root_grant_path,
        &mut control.materialization_path,
    ] {
        if location.is_relative() {
            *location = base.join(&*location);
        }
        *location = location
            .canonicalize()
            .context("resolve existing Campaign control input")?;
    }
    if control.trusted_keys_path.is_relative() {
        control.trusted_keys_path = base.join(&control.trusted_keys_path);
    }
    if let Some(campaign_inputs_path) = &mut control.campaign_inputs_path {
        if campaign_inputs_path.is_relative() {
            *campaign_inputs_path = base.join(&*campaign_inputs_path);
        }
        *campaign_inputs_path = campaign_inputs_path
            .canonicalize()
            .context("resolve existing Campaign inputs receipt")?;
    }
    Ok(control)
}

fn verify(
    signed: &SignedCampaignRootGrantV1,
    path: &Path,
) -> anyhow::Result<VerifiedCampaignRootGrant> {
    Ok(verify_campaign_root_grant(
        signed,
        &read_trusted_keys(path)?,
        Utc::now(),
    )?)
}

pub(super) fn read_trusted_keys(path: &Path) -> anyhow::Result<BTreeMap<String, VerifyingKey>> {
    let encoded: BTreeMap<String, String> = read_json(path)?;
    encoded
        .into_iter()
        .map(|(id, hex_key)| {
            let bytes: [u8; 32] = hex::decode(hex_key)?
                .try_into()
                .map_err(|_| anyhow::anyhow!("Campaign verifying key must contain 32 bytes"))?;
            Ok((id, VerifyingKey::from_bytes(&bytes)?))
        })
        .collect()
}

pub(super) fn read_bounded(path: &Path, max: u64) -> anyhow::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(max + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        bail!("Campaign control input exceeds size limit");
    }
    Ok(bytes)
}

pub(super) fn read_json<T: DeserializeOwned>(path: &Path) -> anyhow::Result<T> {
    serde_json::from_slice(&read_bounded(path, MAX_CONTROL_BYTES)?)
        .context("decode Campaign control input")
}
