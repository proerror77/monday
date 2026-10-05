//! Controlled source export. The host first inspects actual software, data and
//! immutable configuration, transfers the existing debit, then signs under guards.
use super::{
    fixed_campaign, native_witness, released_build, worker_configuration, PlatformPrepareArgs,
};
use alpha_store::campaign_ledger::{CampaignPlatformTransferV1, VerifiedCampaignPlatformExport};
use anyhow::{ensure, Context};
use hft_research_platform::{
    admission::{sign, NativeAdmission, NativeAdmissionTrust},
    build::BuildArtifact,
    execution::Profile,
    orchestrator::Admission,
    release::{BuildReleaseTrust, SignedBuildRelease},
};
use reqwest::blocking::Client;
use serde::{de::DeserializeOwned, Deserialize};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug, Clone, clap::Args)]
pub struct PlatformExportArgs {
    #[command(flatten)]
    pub prepare: PlatformPrepareArgs,
    /// Operator-owned software/target/witness configuration. It contains no key bytes.
    #[arg(long)]
    pub projection: PathBuf,
    /// Create-once public signed export directory, not a signing-key directory.
    #[arg(long)]
    pub output: PathBuf,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Projection {
    schema: String,
    tenant: String,
    profile: Profile,
    build_artifact: PathBuf,
    signed_build_release: PathBuf,
    release_trust: PathBuf,
    native_trust: PathBuf,
    native_witness_key: PathBuf,
    native_witness_key_id: String,
    artifact_readback: BTreeMap<String, String>,
    #[serde(default)]
    release_tls: HostTls,
    signed_admission_put_url: String,
    signed_admission_readback_url: String,
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HostTls {
    ca_file: Option<PathBuf>,
    identity_file: Option<PathBuf>,
}

pub(super) fn read<T: DeserializeOwned>(path: &Path) -> anyhow::Result<T> {
    let bytes = file_bytes(path, 1024 * 1024, false)?;
    serde_json::from_slice(&bytes).context("invalid typed native export metadata")
}
pub(super) fn file_bytes(path: &Path, limit: u64, private: bool) -> anyhow::Result<Vec<u8>> {
    use rustix::fs::{open, Mode, OFlags};
    use std::os::unix::fs::PermissionsExt;
    ensure!(
        path.is_absolute()
            && path
                .parent()
                .context("metadata parent is missing")?
                .canonicalize()?
                == path.parent().unwrap(),
        "native export input requires an absolute canonical parent"
    );
    let file = File::from(open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )?);
    let meta = file.metadata()?;
    ensure!(
        meta.is_file() && meta.len() <= limit,
        "native export input is not bounded regular bytes"
    );
    if private {
        ensure!(
            meta.permissions().mode() & 0o077 == 0
                && path.parent().unwrap().metadata()?.permissions().mode() & 0o077 == 0,
            "native transport credential must be private"
        );
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        !bytes.is_empty() && bytes.len() as u64 <= limit,
        "native export input exceeds bound"
    );
    Ok(bytes)
}
pub(super) fn client(tls: &HostTls) -> anyhow::Result<Client> {
    let mut builder = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30));
    if let Some(path) = &tls.ca_file {
        let certificates =
            reqwest::Certificate::from_pem_bundle(&file_bytes(path, 64 * 1024, false)?)?;
        ensure!(!certificates.is_empty(), "private release CA is absent");
        builder = builder.tls_built_in_root_certs(false);
        for certificate in certificates {
            builder = builder.add_root_certificate(certificate);
        }
    }
    if let Some(path) = &tls.identity_file {
        builder = builder.identity(reqwest::Identity::from_pem(&file_bytes(
            path,
            64 * 1024,
            true,
        )?)?);
    }
    Ok(builder.build()?)
}
pub(super) fn resolve(base: &Path, path: &mut PathBuf) {
    if !path.is_absolute() {
        *path = base.join(&*path);
    }
}
fn projection(path: &Path) -> anyhow::Result<Projection> {
    let path = path.canonicalize()?;
    let base = path.parent().context("projection parent is absent")?;
    let mut value: Projection = serde_json::from_slice(&file_bytes(&path, 1024 * 1024, true)?)?;
    ensure!(
        value.schema == "monday.native_campaign_platform_export.v1"
            && !value.tenant.is_empty()
            && value.tenant.len() <= 128
            && value.tenant.trim() == value.tenant
            && !value.native_witness_key_id.is_empty()
            && value.native_witness_key_id.len() <= 128,
        "invalid native export scope"
    );
    for path in [
        &mut value.build_artifact,
        &mut value.signed_build_release,
        &mut value.release_trust,
        &mut value.native_trust,
        &mut value.native_witness_key,
    ] {
        resolve(base, path);
    }
    for path in [
        &mut value.release_tls.ca_file,
        &mut value.release_tls.identity_file,
    ]
    .into_iter()
    .flatten()
    {
        resolve(base, path);
    }
    value.profile.validate()?;
    Ok(value)
}

pub(super) fn export(args: PlatformExportArgs) -> anyhow::Result<()> {
    crate::cli::require_cloud_data_host(std::env::consts::OS)?;
    ensure!(
        std::env::var("MONDAY_EXECUTION_HOST").as_deref() == Ok("ack"),
        "native signed export requires the controlled ACK source host"
    );
    let projection = projection(&args.projection)?;
    let output = args
        .output
        .canonicalize()
        .context("create the private export directory before running the prebuilt CLI")?;
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            output.is_dir() && output.metadata()?.permissions().mode() & 0o077 == 0,
            "native export output must be a private directory"
        );
    }
    let artifact: BuildArtifact = read(&projection.build_artifact)?;
    let signed_release: SignedBuildRelease = read(&projection.signed_build_release)?;
    let release_trust: BuildReleaseTrust = read(&projection.release_trust)?;
    let native_trust: NativeAdmissionTrust = read(&projection.native_trust)?;
    let release_client = client(&projection.release_tls)?;
    let source_client = client(&HostTls::default())?;
    let directory = tempfile::tempdir()?;
    let build = released_build::verify_and_readback(
        &release_trust,
        &artifact,
        &signed_release,
        &projection.artifact_readback,
        &release_client,
    )?;
    let release_public_keys = released_build::public_keys(&release_trust)?;
    let mut prepared = super::prepare_budget(&args.prepare)?;
    let budget = prepared.budget()?;
    let data = crate::mission_campaign::prepared_inputs::acquire_native_prepared(
        &prepared.validated.submission.request,
        &budget.reservation().request_sha256,
        &source_client,
        directory.path(),
    )?;
    let configuration = worker_configuration::readback(
        &args.prepare.context,
        &args.prepare.namespace,
        projection
            .profile
            .worker_secret
            .as_deref()
            .context("fixed Campaign worker configuration is absent")?,
        prepared.validated.request_json.as_bytes(),
        &budget.reservation().request_sha256,
        &native_trust,
    )?;
    let fixed = fixed_campaign::construct(fixed_campaign::Inputs {
        budget: &budget,
        data: &data,
        build: &build,
        configuration: &configuration,
        profile: projection.profile.clone(),
        validated: &prepared.validated,
        manifest: &prepared.manifest,
        context: &args.prepare.context,
        namespace: &args.prepare.namespace,
    })?;
    let transfer = CampaignPlatformTransferV1 {
        operation_id: budget.reservation().operation_id()?,
        tenant: projection.tenant.clone(),
        run_sha256: fixed.run.id()?,
        request_sha256: fixed.spec.id()?,
    };
    // Nothing is signed or loaded from the private witness before all actual
    // software, development data, resources and configuration gates pass.
    prepared.admission.transfer_to_platform(&transfer)?;
    prepared
        .admission
        .publish_receipts()
        .context("transfer charge retained; receipt readback is incomplete")?;
    validate_export_urls(
        &projection,
        &prepared
            .validated
            .submission
            .request
            .campaign_result_readback_url,
        &budget.operation_sha256()?,
    )?;
    let signed_path = output.join("signed-native-admission.json");
    let signed = prepared
        .admission
        .with_platform_export(&transfer, |source| {
            let evidence = statement(source, &fixed, &artifact)?;
            let signed = if signed_path.exists() {
                let old: hft_research_platform::admission::SignedNativeAdmission =
                    read(&signed_path)?;
                native_trust.verify(&old)?;
                ensure!(
                    old.evidence == evidence && old.key_id == projection.native_witness_key_id,
                    "retained signed export differs from current guarded source evidence"
                );
                old
            } else {
                let key = native_witness::load(
                    &projection.native_witness_key,
                    &projection.native_witness_key_id,
                    &native_trust,
                    source.budget().authority_public_keys(),
                    &release_public_keys,
                )?;
                let signed = sign(evidence, projection.native_witness_key_id.clone(), &key)?;
                native_trust.verify(&signed)?;
                retain(&signed_path, &serde_json::to_vec_pretty(&signed)?)?;
                signed
            };
            let bytes = serde_json::to_vec_pretty(&signed)?;
            publish_and_readback(&source_client, &projection, &bytes)?;
            Ok(signed)
        })?;
    for (name, bytes) in [
        (
            "experiment.json",
            serde_json::to_vec_pretty(&fixed.experiment)?,
        ),
        ("run.json", serde_json::to_vec_pretty(&fixed.run)?),
        ("task.json", serde_json::to_vec_pretty(&fixed.spec)?),
        (
            "campaign-inputs.json",
            serde_json::to_vec_pretty(data.prepared().manifest())?,
        ),
    ] {
        retain(&output.join(name), &bytes)?;
    }
    // Preserve the verified immutable transport bytes for the independent PG
    // input importer. Dropping this temporary cache without them would leave a
    // signed collection that its controlled consumer could not actually read.
    let manifest = data.prepared().manifest();
    let mut copied = std::collections::BTreeSet::new();
    for block in manifest
        .features
        .manifest
        .blocks
        .iter()
        .chain(&manifest.future_marks.manifest.blocks)
        .chain(&manifest.replay.manifest.blocks)
    {
        if copied.insert(&block.sha256) {
            let name = format!("{}.mondaybin", block.sha256);
            let bytes = file_bytes(&directory.path().join(&name), 16 * 1024 * 1024, false)?;
            ensure!(
                bytes.len() as u64 == block.bytes,
                "verified immutable block changed size before export retention"
            );
            retain(&output.join(name), &bytes)?;
        }
    }
    crate::cli::print_json(&serde_json::json!({
        "schema":"monday.native_campaign_platform_export_result.v1", "tenant":signed.evidence.tenant,
        "operation_sha256":signed.evidence.operation_sha256, "run_sha256":fixed.run.id()?,
        "request_sha256":fixed.spec.id()?, "native_request_sha256":signed.evidence.native_request_sha256,
        "evidence_sha256":signed.evidence_sha256, "signed_export_path":signed_path,
        "pg_import_stage":"not_performed_by_export", "terminal_settlement_stage":"not_performed_by_export"
    }))
}

fn statement(
    source: &VerifiedCampaignPlatformExport,
    fixed: &fixed_campaign::FixedCampaign,
    artifact: &BuildArtifact,
) -> anyhow::Result<NativeAdmission> {
    let budget = source.budget();
    let reservation = budget.reservation();
    ensure!(
        source.transfer().run_sha256 == fixed.run.id()?
            && source.transfer().request_sha256 == fixed.spec.id()?,
        "source ownership changed fixed platform Run/Task"
    );
    let evidence = NativeAdmission {
        schema: "monday.native_scientific_admission.v1".into(),
        tenant: source.transfer().tenant.clone(),
        run: fixed.run.clone(),
        admission: Admission {
            schema: 1,
            request_sha256: fixed.spec.id()?,
            task_spec: fixed.spec.clone(),
            resource_reservation_receipt_sha256: budget.reservation_receipt().object_sha256()?,
            scientific_grant_receipt_sha256: budget.root_receipt().object_sha256()?,
            release_admission_receipt_sha256: artifact.release_receipt_sha256.clone(),
            max_attempts: 1,
        },
        operation_sha256: budget.operation_sha256()?,
        native_request_sha256: reservation.request_sha256.clone(),
        family_id: reservation.family_id.clone(),
        root_grant_sha256: budget.root().content_sha256().into(),
        approval_sha256: budget.approval_sha256().into(),
        transfer_receipt_sha256: source.transfer_receipt().object_sha256()?,
        declared_trials: reservation.declared_trials,
        reserved_job_seconds: reservation.reserved_job_seconds,
        reserved_llm_tokens: reservation.reserved_llm_tokens,
        issued_ms: source
            .transfer_receipt()
            .receipt
            .recorded_at
            .timestamp_millis(),
        expires_ms: budget.authority_expires_at().timestamp_millis(),
    };
    evidence.admits_launch_at(chrono::Utc::now().timestamp_millis())?;
    Ok(evidence)
}
fn validate_export_urls(
    projection: &Projection,
    native_result_url: &str,
    operation: &str,
) -> anyhow::Result<()> {
    let result = crate::prediction_dispatch::canonical_tokyo_oss_internal_object(
        "native result",
        native_result_url,
    )?;
    let parsed = reqwest::Url::parse(&result)?;
    let expected = format!(
        "{}://{}/research/native-admissions/{operation}/signed-native-admission.json",
        parsed.scheme(),
        parsed
            .host_str()
            .context("native receipt bucket is absent")?
    );
    for url in [
        &projection.signed_admission_put_url,
        &projection.signed_admission_readback_url,
    ] {
        ensure!(
            crate::prediction_dispatch::canonical_tokyo_oss_internal_object(
                "native admission export",
                url
            )? == expected,
            "native export transport changed the exact existing operation or receipt bucket"
        );
    }
    Ok(())
}
fn publish_and_readback(
    client: &Client,
    projection: &Projection,
    bytes: &[u8],
) -> anyhow::Result<()> {
    ensure!(
        bytes.len() <= 1024 * 1024,
        "native signed export exceeds bound"
    );
    let put = client
        .put(&projection.signed_admission_put_url)
        .header("Content-Type", "application/json")
        .header("x-oss-forbid-overwrite", "true")
        .body(bytes.to_vec())
        .send()
        .map_err(reqwest::Error::without_url)?;
    ensure!(
        put.status().is_success() || put.status() == reqwest::StatusCode::CONFLICT,
        "native signed export publication rejected"
    );
    let response = client
        .get(&projection.signed_admission_readback_url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(reqwest::Error::without_url)?;
    let mut observed = Vec::new();
    response
        .take(bytes.len() as u64 + 1)
        .read_to_end(&mut observed)?;
    ensure!(
        observed == bytes,
        "native signed export readback changed immutable bytes"
    );
    Ok(())
}
pub(super) fn retain(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let parent = path.parent().context("export output parent is absent")?;
    ensure!(
        parent.is_absolute()
            && parent.canonicalize()? == parent
            && parent.metadata()?.permissions().mode() & 0o077 == 0
            && bytes.len() <= 128 * 1024 * 1024,
        "export output must be bounded private canonical storage"
    );
    if path.exists() {
        ensure!(
            file_bytes(path, 128 * 1024 * 1024, true)? == bytes,
            "export output already contains different immutable bytes"
        );
        return Ok(());
    }
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist_noclobber(path).map_err(|e| e.error)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}
