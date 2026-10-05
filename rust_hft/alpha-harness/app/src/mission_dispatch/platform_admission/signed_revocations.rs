//! Constraint-only source export. Reasons and operation bindings are derived
//! from authenticated, published Root/Study history, never caller statements.
use super::{
    native_witness, released_build,
    signed_export::{self, HostTls},
};
use alpha_store::{campaign_ledger::VerifiedCampaignPlatformRevocation, AlphaStore};
use anyhow::{ensure, Context};
use hft_research_platform::{
    admission::NativeAdmissionTrust,
    release::BuildReleaseTrust,
    revocation::{sign_revocation, NativeRequestRevocation, SignedNativeRequestRevocation},
};
use serde::Deserialize;
use std::{collections::BTreeMap, io::Read, path::PathBuf};

#[derive(Debug, Clone, clap::Args)]
pub struct PlatformRevocationsArgs {
    #[arg(long)]
    pub control: PathBuf,
    #[arg(long)]
    pub family_id: String,
    #[arg(long)]
    pub projection: PathBuf,
    /// Existing canonical private directory. Signed public files are create-once.
    #[arg(long)]
    pub output: PathBuf,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Projection {
    schema: String,
    tenant: String,
    native_trust: PathBuf,
    release_trust: PathBuf,
    native_witness_key: PathBuf,
    native_witness_key_id: String,
    /// Exact operation/reason paths; no wildcard or arbitrary reason statement.
    publications: BTreeMap<String, Publication>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Publication {
    put_url: String,
    readback_url: String,
}

pub(super) fn export(args: PlatformRevocationsArgs) -> anyhow::Result<()> {
    crate::cli::require_cloud_data_host(std::env::consts::OS)?;
    ensure!(
        std::env::var("MONDAY_EXECUTION_HOST").as_deref() == Ok("ack"),
        "native revocation export requires the controlled ACK source host"
    );
    let path = args.projection.canonicalize()?;
    let mut projection: Projection =
        serde_json::from_slice(&signed_export::file_bytes(&path, 1024 * 1024, true)?)?;
    ensure!(
        projection.schema == "monday.native_campaign_platform_revocations.v1"
            && !projection.tenant.is_empty()
            && projection.tenant.len() <= 128
            && projection.tenant.trim() == projection.tenant
            && projection.publications.len() <= 1024,
        "invalid bounded source revocation projection"
    );
    let base = path
        .parent()
        .context("source revocation projection parent is absent")?;
    for path in [
        &mut projection.native_trust,
        &mut projection.release_trust,
        &mut projection.native_witness_key,
    ] {
        signed_export::resolve(base, path);
    }
    let trust: NativeAdmissionTrust = signed_export::read(&projection.native_trust)?;
    let release: BuildReleaseTrust = signed_export::read(&projection.release_trust)?;
    let forbidden_release_keys = released_build::public_keys(&release)?;
    let control = super::super::admission::read_control(&args.control)?;
    let store = AlphaStore::open_read_only(&control.ledger_path)?;
    // Historical constraints remain exportable after grant expiration. This
    // read never registers a grant, reserves a run, claims or refunds budget.
    let reasons = store.campaign_platform_revocations(&args.family_id)?;
    ensure!(
        reasons.len() <= 1024,
        "source revocation export exceeds bounded count"
    );
    let output = args.output.canonicalize()?;
    let client = signed_export::client(&HostTls::default())?;
    let mut identities = Vec::new();
    for reason in &reasons {
        ensure!(
            reason.transfer().tenant == projection.tenant,
            "source revocation belongs to another tenant"
        );
        let operation = reason.operation_sha256()?;
        let filename = format!("{operation}-{}.json", reason.reason_receipt_sha256());
        let object_key = format!(
            "research/native-request-revocations/{operation}/{}.json",
            reason.reason_receipt_sha256()
        );
        let publication = projection
            .publications
            .get(&object_key)
            .context("source reason lacks exact controlled publication access")?;
        ensure!(
            hft_research_dispatch_io::canonical_tokyo_oss_internal_object(
                "source revocation",
                &publication.put_url
            )? == hft_research_dispatch_io::canonical_tokyo_oss_internal_object(
                "source revocation",
                &publication.readback_url
            )?,
            "source revocation PUT and readback buckets differ"
        );
        for url in [&publication.put_url, &publication.readback_url] {
            let canonical = hft_research_dispatch_io::canonical_tokyo_oss_internal_object(
                "source revocation",
                url,
            )?;
            let parsed = reqwest::Url::parse(&canonical)?;
            ensure!(
                parsed.path() == format!("/{object_key}"),
                "source revocation publication changed the operation or reason"
            );
        }
        let retained = output.join(filename);
        native_witness::check_public_role(
            &projection.native_witness_key_id,
            &trust,
            reason.authority_public_keys(),
            &forbidden_release_keys,
        )?;
        let signed = if retained.exists() {
            let signed: SignedNativeRequestRevocation = signed_export::read(&retained)?;
            trust.verify_revocation(&signed)?;
            ensure!(
                signed.key_id == projection.native_witness_key_id
                    && signed.evidence == statement(reason, signed.evidence.issued_ms)?,
                "retained revocation differs from actual source history"
            );
            signed
        } else {
            let evidence = statement(reason, chrono::Utc::now().timestamp_millis())?;
            let key = native_witness::load(
                &projection.native_witness_key,
                &projection.native_witness_key_id,
                &trust,
                reason.authority_public_keys(),
                &forbidden_release_keys,
            )?;
            let signed = sign_revocation(evidence, projection.native_witness_key_id.clone(), &key)?;
            trust.verify_revocation(&signed)?;
            signed_export::retain(&retained, &serde_json::to_vec_pretty(&signed)?)?;
            signed
        };
        // Stable retained issued_ms preserves retries after an ambiguous PUT.
        let bytes = serde_json::to_vec_pretty(&signed)?;
        let put = client
            .put(&publication.put_url)
            .header("Content-Type", "application/json")
            .header("x-oss-forbid-overwrite", "true")
            .body(bytes.clone())
            .send()
            .map_err(reqwest::Error::without_url)?;
        ensure!(
            put.status().is_success() || put.status() == reqwest::StatusCode::CONFLICT,
            "source revocation publication rejected"
        );
        let response = client
            .get(&publication.readback_url)
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(reqwest::Error::without_url)?;
        let mut observed = Vec::new();
        response
            .take(bytes.len() as u64 + 1)
            .read_to_end(&mut observed)?;
        ensure!(
            observed == bytes,
            "source revocation independent readback changed bytes"
        );
        identities.push(serde_json::json!({"request_sha256":signed.evidence.request_sha256,"operation_sha256":operation,"reason_receipt_sha256":reason.reason_receipt_sha256(),"effective_ms":signed.evidence.effective_ms,"evidence_sha256":signed.evidence_sha256}));
    }
    crate::cli::print_json(
        &serde_json::json!({"schema":"monday.native_campaign_platform_revocations_result.v1","family_id":args.family_id,"constraints":identities,"pg_import_stage":"not_performed_by_export","budget_changed":false}),
    )
}

pub(super) fn statement(
    source: &VerifiedCampaignPlatformRevocation,
    issued_ms: i64,
) -> anyhow::Result<NativeRequestRevocation> {
    let value = NativeRequestRevocation {
        schema: "monday.native_request_revocation.v1".into(),
        tenant: source.transfer().tenant.clone(),
        request_sha256: source.transfer().request_sha256.clone(),
        operation_sha256: source.operation_sha256()?,
        family_id: source.family_id().into(),
        root_grant_sha256: source.root_grant_sha256().into(),
        reason_receipt_sha256: source.reason_receipt_sha256().into(),
        effective_ms: source.effective_at().timestamp_millis(),
        issued_ms,
    };
    value.validate()?;
    ensure!(
        issued_ms <= chrono::Utc::now().timestamp_millis(),
        "source revocation issuance is in the future"
    );
    Ok(value)
}
