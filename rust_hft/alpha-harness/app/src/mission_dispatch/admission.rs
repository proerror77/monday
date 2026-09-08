//! Operator-owned root authority is separate from the finalized worker request.
//! Every charge is derived from the request and the actual rendered Job.

use super::{image_digest, ValidatedSubmission};
use crate::{
    cli::BUILD_SOURCE_REVISION,
    mission_render::{
        approved_evaluation_protocol, approved_validation, validate_render_materialization_scope,
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
        CampaignDispatchTargetV1,
    },
    AlphaStore,
};
use anyhow::{bail, Context};
use chrono::Utc;
use ed25519_dalek::VerifyingKey;
use reqwest::blocking::Client;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

const CONTROL_SCHEMA: &str = "monday.campaign_dispatch_control.v1";
const MAX_CONTROL_BYTES: u64 = 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DispatchControl {
    schema_version: String,
    ledger_path: PathBuf,
    signed_root_grant_path: PathBuf,
    trusted_keys_path: PathBuf,
    materialization_path: PathBuf,
    approval_id: String,
    controller_image: String,
    attempt_ordinal: u32,
    receipt_access: BTreeMap<String, ReceiptAccess>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReceiptAccess {
    put_url: String,
    readback_url: String,
}

#[derive(Debug, Serialize)]
pub(super) struct DispatchInspection {
    execution: CampaignExecutionBindingV1,
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
    fn reservation(&self, grant: &VerifiedCampaignRootGrant) -> CampaignAttemptReservationV1 {
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
    validate_render_materialization_scope(&materialization)?;
    validate_materialization(
        &materialization,
        &request.feature_sha256,
        &approved_validation(&materialization)?,
    )?;
    let protocol = approved_evaluation_protocol(&materialization)?;
    let protocol_sha256 = protocol.content_hash()?;
    // This identifies the existing walk-forward view, not an independent
    // selection dataset. Changing its materialization or split changes the hash.
    let view_sha256 = canonical_json_hash(&serde_json::json!({
        "schema_version": "monday.campaign_search_visible_view.v1",
        "materialization_sha256": request.materialization_sha256,
        "feature_sha256": request.feature_sha256,
        "snapshot_sha256": materialization.snapshot.sha256(),
        "walk_forward": protocol.walk_forward,
    }))?;
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
            search_view_sha256: view_sha256.clone(),
            selection_view_sha256: view_sha256,
            selection_feedback: CampaignSelectionFeedbackV1::SearchAndLearningVisibleWalkForward,
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
        // Preserve the operator's logical trust locator, including projected
        // file/directory symlinks. Freezing a symlink target would retain an old
        // key set after rotation. Immutable inputs and the database are resolved
        // physically below; all relative paths are anchored once, never to a
        // later working directory.
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .context("resolve Campaign control directory")?
                .join(path)
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
        let receipts = self
            .store
            .campaign_family_receipts(&self.reservation.family_id)?;
        for receipt in receipts {
            let key = receipt.object_key();
            let access = self.control.receipt_access.get(&key).with_context(|| {
                format!("missing signed receipt access for {key}; reservation is retained")
            })?;
            validate_receipt_access(access, &self.receipt_origin, &key)?;
            let bytes = receipt.publication_bytes()?;
            let observed =
                transfer(access, &bytes).with_context(|| format!("Campaign receipt {key}"))?;
            if observed != bytes {
                bail!("immutable receipt readback differs from the local authenticated bytes");
            }
            self.store.acknowledge_campaign_receipt_readback(
                &self.reservation.family_id,
                receipt.receipt.sequence,
                &key,
                &hex::encode(Sha256::digest(&observed)),
            )?;
            crate::mission_runner::research_event(
                "alpha-harness",
                "campaign_ledger_receipt_readback_completed",
                serde_json::json!({
                    "family_id": self.reservation.family_id, "sequence": receipt.receipt.sequence,
                    "object_sha256": receipt.object_sha256()?,
                }),
            );
        }
        Ok(())
    }
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

fn verify(
    signed: &SignedCampaignRootGrantV1,
    path: &Path,
) -> anyhow::Result<VerifiedCampaignRootGrant> {
    let encoded: BTreeMap<String, String> = read_json(path)?;
    let keys = encoded
        .into_iter()
        .map(|(id, hex_key)| {
            let bytes: [u8; 32] = hex::decode(hex_key)?
                .try_into()
                .map_err(|_| anyhow::anyhow!("Campaign verifying key must contain 32 bytes"))?;
            Ok((id, VerifyingKey::from_bytes(&bytes)?))
        })
        .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
    Ok(verify_campaign_root_grant(signed, &keys, Utc::now())?)
}

fn read_bounded(path: &Path, max: u64) -> anyhow::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(max + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        bail!("Campaign control input exceeds size limit");
    }
    Ok(bytes)
}

fn read_json<T: DeserializeOwned>(path: &Path) -> anyhow::Result<T> {
    serde_json::from_slice(&read_bounded(path, MAX_CONTROL_BYTES)?)
        .context("decode Campaign control input")
}
