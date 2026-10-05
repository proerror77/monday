//! Development-only native Campaign collections bound to an admitted fixed Run.
use crate::{admission::VerifiedNativeAdmission, orchestrator::TaskKind};
#[cfg(feature = "control")]
use anyhow::Context;
use anyhow::{ensure, Result};
use hft_cex_research_input::{
    campaign::{CampaignPreparedInputsV1, VerifiedCampaignPreparedInputsV1},
    data::BlockSource,
};

/// The complete collection hash is authenticated by the native source witness.
/// Its producer must first verify the finalized native request and actual data.
/// At this import boundary we independently decode the exact published blocks.
pub fn verify_inputs(
    native: &VerifiedNativeAdmission,
    manifest: CampaignPreparedInputsV1,
    source: &mut impl BlockSource,
    max_bytes: u64,
) -> Result<VerifiedCampaignPreparedInputsV1> {
    let evidence = native.evidence();
    ensure!(
        evidence.run.kind == TaskKind::CexCampaign
            && evidence.run.data_manifest_sha256 == manifest.id()?
            && evidence.run.evaluation_protocol_sha256 == manifest.original.protocol_sha256,
        "native witness changed Campaign collection or evaluation protocol"
    );
    // This expectation comes from the complete signed collection identity,
    // never from an independent caller-supplied source/protocol override.
    let expected = manifest.expected_native()?;
    let verified = manifest.verify(
        &evidence.run.data_manifest_sha256,
        &expected,
        source,
        max_bytes,
    )?;
    validate_binding(native, &verified)?;
    Ok(verified)
}

pub fn validate_binding(
    native: &VerifiedNativeAdmission,
    inputs: &VerifiedCampaignPreparedInputsV1,
) -> Result<()> {
    let evidence = native.evidence();
    ensure!(
        evidence.run.kind == TaskKind::CexCampaign
            && evidence.admission.task_spec.kind == TaskKind::CexCampaign
            && evidence.run.data_manifest_sha256 == inputs.id()
            && evidence.admission.task_spec.view_manifest_sha256 == inputs.id()
            && evidence.run.evaluation_protocol_sha256 == inputs.native_protocol_sha256(),
        "fixed Run changed verified native Campaign input roles"
    );
    Ok(())
}

#[cfg(feature = "control")]
impl crate::postgres::Ledger {
    pub(crate) async fn validate_campaign_result(
        &self,
        task: &crate::orchestrator::Task,
        result: &crate::campaign_result::CexCampaignResultReceipt,
    ) -> Result<()> {
        use sqlx_core::{query::query, row::Row};
        // Resolve immutable native scope from PG. A caller's receipt cannot
        // supply a different protocol, grant, source preparation, or budget.
        let row = query("SELECT n.document AS native,i.document AS inputs FROM research.native_admission_imports n JOIN research.native_campaign_inputs c USING(request_sha256) JOIN research.inputs i ON i.manifest_sha256=c.manifest_sha256 JOIN research.tasks t ON t.task_id=n.request_sha256 AND t.tenant=n.tenant WHERE n.request_sha256=$1 AND c.tenant=n.tenant AND c.manifest_sha256=$2")
            .bind(&task.id).bind(&task.spec.view_manifest_sha256).fetch_one(&self.pool).await?;
        let native: crate::admission::SignedNativeAdmission =
            serde_json::from_value(row.get("native"))?;
        let inputs: CampaignPreparedInputsV1 = serde_json::from_value(row.get("inputs"))?;
        ensure!(
            result.runner.source_revision == native.evidence.run.code_commit
                && result.evaluation_protocol_sha256 == inputs.original.protocol_sha256
                && result.native_request_sha256 == native.evidence.native_request_sha256
                && result.declared_trials == native.evidence.declared_trials,
            "Campaign result changed native protocol, request or budget"
        );
        Ok(())
    }
    /// Stages independently verified, development-only bytes. The native
    /// admission and its immutable Run must already be imported for this tenant.
    /// This does not activate a backend or create a grant, charge, or Attempt.
    pub async fn register_campaign_inputs(
        &self,
        inputs: &VerifiedCampaignPreparedInputsV1,
        native: &VerifiedNativeAdmission,
    ) -> Result<String> {
        use sqlx_core::{query::query, query_scalar::query_scalar, row::Row};
        validate_binding(native, inputs)?;
        let evidence = native.evidence();
        let request = &evidence.admission.request_sha256;
        let mut tx = self.pool.begin().await?;
        let stored = query("SELECT n.document FROM research.native_admission_imports n JOIN research.admissions a USING(request_sha256) WHERE n.request_sha256=$1 AND n.tenant=$2 FOR UPDATE OF a")
            .bind(request).bind(&evidence.tenant).fetch_one(&mut *tx).await?;
        ensure!(
            serde_json::from_value::<crate::admission::SignedNativeAdmission>(
                stored.get("document")
            )? == *native.signed(),
            "Campaign input publication changed imported native witness"
        );
        let now: i64 =
            query_scalar("SELECT floor(extract(epoch FROM clock_timestamp())*1000)::bigint")
                .fetch_one(&mut *tx)
                .await?;
        evidence.admits_launch_at(now)?;
        let cap: Option<i64> = query_scalar("SELECT research.native_request_deadline_ms($1)")
            .bind(request)
            .fetch_one(&mut *tx)
            .await?;
        let finish = now
            .checked_add(evidence.admission.task_spec.timeout_ms)
            .context("Campaign deadline overflow")?;
        ensure!(
            cap.is_some_and(|deadline| finish <= deadline),
            "Campaign input publication crosses native revocation or expiry"
        );
        let revoked: bool = query_scalar(
            "SELECT EXISTS(SELECT 1 FROM research.revocations WHERE request_sha256=$1)",
        )
        .bind(request)
        .fetch_one(&mut *tx)
        .await?;
        ensure!(!revoked, "Campaign input admission was revoked");
        let document = serde_json::to_value(inputs.manifest())?;
        query("INSERT INTO research.inputs(manifest_sha256,kind,document) VALUES($1,'cex_campaign',$2) ON CONFLICT DO NOTHING")
            .bind(inputs.id()).bind(&document).execute(&mut *tx).await?;
        let row = query("SELECT kind,document FROM research.inputs WHERE manifest_sha256=$1")
            .bind(inputs.id())
            .fetch_one(&mut *tx)
            .await?;
        ensure!(
            row.get::<String, _>("kind") == "cex_campaign"
                && row.get::<serde_json::Value, _>("document") == document,
            "Campaign input conflicts with another input kind or bytes"
        );
        let verification = crate::identity(&(
            "monday.native_campaign_input_readback.v1",
            inputs.id(),
            inputs.expected_native(),
            inputs.view_ids(),
        ))?;
        query("INSERT INTO research.native_campaign_inputs(request_sha256,manifest_sha256,tenant,verification_sha256) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING")
            .bind(request).bind(inputs.id()).bind(&evidence.tenant).bind(&verification).execute(&mut *tx).await?;
        let row = query("SELECT manifest_sha256,tenant,verification_sha256 FROM research.native_campaign_inputs WHERE request_sha256=$1")
            .bind(request).fetch_one(&mut *tx).await?;
        ensure!(
            row.get::<String, _>("manifest_sha256") == inputs.id()
                && row.get::<String, _>("tenant") == evidence.tenant
                && row.get::<String, _>("verification_sha256") == verification,
            "Campaign request already has another input binding"
        );
        tx.commit().await?;
        Ok(inputs.id().to_owned())
    }
}
