//! Closing search is a ledger operation, never an alternate evaluation path.
use super::admission::{read_json, read_trusted_keys};
use crate::cli::{print_json, CampaignCloseFamilyArgs};
use alpha_domain::campaign_finalization::{
    verify_campaign_final_evaluation_grant, SignedCampaignFinalEvaluationGrantV1,
};
use alpha_store::AlphaStore;
use anyhow::bail;
use chrono::Utc;

pub fn close_family(args: CampaignCloseFamilyArgs) -> anyhow::Result<()> {
    if !args.ledger.is_file() {
        bail!("closing a Campaign family requires an existing local ledger file");
    }
    let signed: SignedCampaignFinalEvaluationGrantV1 = read_json(&args.signed_grant)?;
    let keys = read_trusted_keys(&args.trusted_keys)?;
    let now = Utc::now();
    let verified = verify_campaign_final_evaluation_grant(&signed, &keys, now)?;
    let mut store = AlphaStore::open(&args.ledger)?;
    let receipt =
        store.close_campaign_family_for_final_evaluation(&verified, &args.approval_id, now)?;
    if store
        .campaign_final_evaluation_grant(&signed.grant.family_id)?
        .as_ref()
        != Some(&signed)
    {
        bail!("closed family grant readback differs from the verified authority");
    }
    print_json(&serde_json::json!({
        "action": "family_closed_for_final_evaluation",
        "family_id": signed.grant.family_id,
        "grant_sha256": signed.content_sha256,
        "closure_receipt": receipt,
        "pending_receipt_readbacks": store.pending_campaign_receipts(&signed.grant.family_id)?.len(),
        "evaluation_started": false,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_close_does_not_bootstrap_a_missing_ledger() {
        let root = tempfile::tempdir().unwrap();
        let ledger = root.path().join("mistyped-ledger.duckdb");
        let result = close_family(CampaignCloseFamilyArgs {
            ledger: ledger.clone(),
            signed_grant: root.path().join("grant.json"),
            trusted_keys: root.path().join("keys.json"),
            approval_id: "approval".into(),
        });
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("existing local ledger"));
        assert!(!ledger.exists());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
}
