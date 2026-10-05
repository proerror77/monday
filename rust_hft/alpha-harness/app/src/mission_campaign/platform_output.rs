//! Exact native evidence published through the admitted platform Attempt writer.
use super::*;
use hft_research_platform::{
    artifact_io::Writer,
    campaign_result::{
        ArchiveEntry, CampaignRoundEvidence, CexCampaignResultReceipt, PublishedNativeArtifact,
        ScientificStatus,
    },
    orchestrator::{AttemptContext, ResultReceipt, TaskKind},
};
#[cfg(test)]
mod transport_fixture;
#[cfg(test)]
pub(super) use transport_fixture::assert_publication;

const MAX_PLATFORM_ARCHIVE_EXPANDED_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactConfig {
    schema_version: String,
    artifact_gateway: String,
    artifact_token_file: PathBuf,
    artifact_tls: hft_research_platform::transport::TlsConfig,
}

pub(super) struct BoundOutput {
    context: AttemptContext,
    writer: Writer,
}
impl BoundOutput {
    pub(super) fn from_environment(
        loaded: &LoadedRequest,
        native: &prepared_inputs::VerifiedNativeCampaignPreparedInputs,
    ) -> anyhow::Result<Option<Self>> {
        if std::env::var_os("MONDAY_ATTEMPT_CONTEXT").is_none() {
            if std::env::var_os("MONDAY_TASK_ID").is_some() {
                bail!("platform task lacks admitted Attempt context");
            }
            return Ok(None);
        }
        let context = AttemptContext::from_environment()?;
        Self::from_context_at(
            loaded,
            native,
            context,
            Path::new("/config"),
            Path::new("/identity"),
        )
        .map(Some)
    }
    fn from_context_at(
        loaded: &LoadedRequest,
        native: &prepared_inputs::VerifiedNativeCampaignPreparedInputs,
        context: AttemptContext,
        configuration: &Path,
        identity: &Path,
    ) -> anyhow::Result<Self> {
        validate_context(&context, loaded, native)?;
        let manifest = native.prepared().manifest();
        let archived_blocks = manifest
            .features
            .manifest
            .blocks
            .iter()
            .chain(&manifest.future_marks.manifest.blocks)
            .chain(&manifest.replay.manifest.blocks)
            .try_fold(0_u64, |sum, block| {
                sum.checked_add(block.bytes)
                    .context("native archive block byte overflow")
            })?;
        if archived_blocks >= MAX_PLATFORM_ARCHIVE_EXPANDED_BYTES {
            bail!("native input blocks exceed the platform result archive budget");
        }
        let expected = context
            .spec
            .worker_configuration
            .as_ref()
            .context("native static configuration proof missing")?;
        use base64::{engine::general_purpose::STANDARD, Engine};
        let mut encoded = std::collections::BTreeMap::new();
        let mut raw = std::collections::BTreeMap::new();
        for name in [
            "campaign.json",
            "artifact-io.json",
            "ca.pem",
            "native-trust.json",
        ] {
            let path = configuration.join(name);
            if path.try_exists()? {
                let bytes = read_private_bounded(&path, 1024 * 1024)?;
                encoded.insert(name.to_string(), STANDARD.encode(&bytes));
                raw.insert(name, bytes);
            }
        }
        let actual = hft_research_platform::orchestrator::worker_configuration_reference(
            &context.spec.profile.namespace,
            &expected.secret_name,
            &expected.secret_uid,
            &encoded,
        )?;
        if &actual != expected {
            bail!("native staged static configuration changed its signed bytes");
        }
        if hft_cex_research_input::sha256(
            raw.get("campaign.json")
                .context("native static request missing")?,
        ) != loaded.sha256
        {
            bail!("native static campaign bytes differ from the admitted request");
        }
        let config_bytes = raw
            .get("artifact-io.json")
            .context("native artifact configuration missing")?;
        if config_bytes.len() > 64 * 1024 {
            bail!("native artifact configuration exceeds bound");
        }
        let config: ArtifactConfig = serde_json::from_slice(config_bytes)?;
        if config.schema_version != "monday.cex_campaign_artifact_io.v1"
            || config.artifact_token_file != Path::new("/identity/artifact.token")
            || config.artifact_tls.ca_file.as_deref() != Some(Path::new("/config/ca.pem"))
            || config.artifact_tls.identity_file.as_deref() != Some(Path::new("/identity/tls.pem"))
        {
            bail!("native artifact transport changed its fixed private configuration");
        }
        let trust_bytes = raw
            .get("native-trust.json")
            .context("native issuer trust missing")?;
        if trust_bytes.len() > 64 * 1024 {
            bail!("native issuer trust exceeds bound");
        }
        let trust: hft_research_platform::admission::NativeAdmissionTrust =
            serde_json::from_slice(trust_bytes)?;
        let signed: hft_research_platform::admission::SignedNativeAdmission =
            serde_json::from_slice(&read_private_bounded(
                &identity.join("native-admission.json"),
                64 * 1024,
            )?)?;
        let verified = trust.verify(&signed)?;
        let evidence = verified.evidence();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis();
        evidence.active_at(i64::try_from(now)?)?;
        if evidence.admission.task_spec != context.spec
            || evidence.run.id()? != context.spec.run_manifest_sha256
            || evidence.run.code_commit != loaded.request.build_source_revision
            || evidence.run.evaluation_protocol_sha256 != native.evaluation_protocol_sha256()
            || evidence.native_request_sha256 != native.request_sha256()
            || evidence.declared_trials != native.declared_trials() as u64
            || evidence.expires_ms <= i64::try_from(now)?
            || context.lease.expires_ms <= i64::try_from(now)?
        {
            bail!("native signed transfer/Attempt does not cover the exact live request/budget");
        }
        let token = String::from_utf8(read_private_bounded(
            &identity.join("artifact.token"),
            64 * 1024,
        )?)?;
        let writer = Writer::with_tls(
            &config.artifact_gateway,
            token.trim().to_string(),
            &context,
            &hft_research_platform::transport::TlsConfig {
                ca_file: Some(configuration.join("ca.pem")),
                identity_file: Some(identity.join("tls.pem")),
            },
        )?;
        Ok(Self { context, writer })
    }
    pub(super) async fn publish(
        &self,
        loaded: &LoadedRequest,
        native: &prepared_inputs::VerifiedNativeCampaignPreparedInputs,
        result: &CampaignResultV1,
        sha: &str,
        dir: &Path,
    ) -> anyhow::Result<()> {
        publish(
            &self.context,
            &self.writer,
            loaded,
            native,
            result,
            sha,
            dir,
        )
        .await?;
        Ok(())
    }
}

fn read_private_bounded(path: &Path, bound: u64) -> anyhow::Result<Vec<u8>> {
    use rustix::fs::{open, Mode, OFlags};
    let parent = path.parent().context("private file parent absent")?;
    if !path.is_absolute() || parent.canonicalize()? != parent {
        bail!("private input path is not canonical");
    }
    let file = File::from(open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )?);
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > bound {
        bail!("private input exceeds regular file bound");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0
            || parent.metadata()?.permissions().mode() & 0o077 != 0
        {
            bail!("private inputs must retain owner-only permissions");
        }
    }
    let mut bytes = Vec::new();
    file.take(bound + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > bound {
        bail!("private input changed during read");
    }
    Ok(bytes)
}

pub(super) fn validate_context(
    context: &AttemptContext,
    loaded: &LoadedRequest,
    native: &prepared_inputs::VerifiedNativeCampaignPreparedInputs,
) -> anyhow::Result<()> {
    context.validate()?;
    if context.spec.kind != TaskKind::CexCampaign
        || context.spec.max_attempts != 1
        || context.spec.view_manifest_sha256 != native.collection_id()
        || mission_dispatch::image_digest(&context.spec.image)? != loaded.request.image_identity
        || native.request_sha256() != loaded.sha256
    {
        bail!("platform Attempt changed the native Campaign/data/runner binding");
    }
    let expected = context
        .spec
        .command
        .windows(2)
        .filter(|p| p[0] == "--request-sha256")
        .map(|p| p[1].as_str())
        .collect::<Vec<_>>();
    if expected != [loaded.sha256.as_str()]
        || !context.spec.command.iter().any(|p| p == "--pre-holdout")
    {
        bail!("platform Attempt does not bind exact pre-holdout native execution");
    }
    Ok(())
}

fn actual_archive_entries(path: &Path) -> anyhow::Result<Vec<ArchiveEntry>> {
    actual_archive_entries_with_limit(path, MAX_PLATFORM_ARCHIVE_EXPANDED_BYTES)
}

fn actual_archive_entries_with_limit(
    path: &Path,
    expanded_limit: u64,
) -> anyhow::Result<Vec<ArchiveEntry>> {
    let file = File::open(path)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > MAX_PLATFORM_ARCHIVE_EXPANDED_BYTES {
        bail!("native result archive exceeds its bound");
    }
    let mut archive = ZipArchive::new(file)?;
    if archive.len() > 256 {
        bail!("native archive has too many entries");
    }
    let mut entries = Vec::new();
    let mut paths = std::collections::BTreeSet::new();
    let mut total = 0u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        if entry.is_dir() {
            continue;
        }
        let path = entry.name().to_string();
        if entry.enclosed_name().is_none()
            || path.len() > 256
            || path.contains("..")
            || path
                .bytes()
                .any(|b| !(b.is_ascii_alphanumeric() || b"/-_.".contains(&b)))
            || !paths.insert(path.clone())
            || entry.size() == 0
            || entry.size() > 512 * 1024 * 1024
        {
            bail!("native archive entry is unsafe, missing or unbounded");
        }
        total = total
            .checked_add(entry.size())
            .context("native archive size overflow")?;
        if total > expanded_limit {
            bail!("native archive expanded bytes exceed bound");
        }
        let mut hash = Sha256::new();
        let mut bytes = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let count = entry.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            bytes = bytes
                .checked_add(count as u64)
                .context("native archive entry size overflow")?;
            if bytes > entry.size() {
                bail!("native archive entry size changed");
            }
            hash.update(&buffer[..count]);
        }
        if bytes != entry.size() {
            bail!("native archive entry incomplete");
        }
        entries.push(ArchiveEntry {
            path,
            sha256: hex::encode(hash.finalize()),
            bytes,
        });
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    for required in [
        "results/native-prepared-admission.json",
        "results/factor-bank.json",
        "results/mission-run.json",
        "results/mission-admission.json",
    ] {
        if !entries.iter().any(|entry| entry.path == required) {
            bail!("native result lacks actual {required}");
        }
    }
    Ok(entries)
}

pub(super) async fn publish(
    context: &AttemptContext,
    writer: &Writer,
    loaded: &LoadedRequest,
    native: &prepared_inputs::VerifiedNativeCampaignPreparedInputs,
    result: &CampaignResultV1,
    result_sha256: &str,
    directory: &Path,
) -> anyhow::Result<ResultReceipt> {
    validate_context(context, loaded, native)?;
    validate_campaign_result_identity(loaded, result, result_sha256)?;
    if result.finalization.is_some() || result.rounds.len() != loaded.request.rounds.len() {
        bail!("development platform worker cannot report final evaluation or partial rounds");
    }
    let campaign_result = writer
        .put_file(
            "native-campaign-result.json",
            &directory.join("campaign-result-readback.json"),
            result_sha256,
        )
        .await?;
    let mut artifacts = vec![campaign_result.clone()];
    let mut rounds = Vec::new();
    let mut consumed = 0u64;
    for (planned, round) in loaded.request.rounds.iter().zip(&result.rounds) {
        let round_dir = directory.join("mission").join(&round.round_id);
        let execute = round_dir.join("execute");
        if planned.round_id != round.round_id
            || planned.seed != round.seed
            || round.result_bundle_sha256 != round.result_readback_bundle_sha256
            || round.request_sha256.as_deref() != Some(loaded.sha256.as_str())
        {
            bail!("platform native round changed its exact request/readback identity");
        }
        crate::mission_runner::validate_native_campaign_result_binding(
            &execute.join("results"),
            native,
        )?;
        let readback = execute.join("input/published-result-readback.zip");
        let zip = if readback.try_exists()? {
            readback
        } else {
            round_dir.join("published-result-readback.zip")
        };
        let report = recover_execution_report_from_cached_result(
            &zip,
            &round.result_readback_bundle_sha256,
            &round.mission_id,
            &round.mission_sha256,
            &ExecutionBinding::Campaign {
                campaign_id: loaded.request.campaign_id.clone(),
                round_id: round.round_id.clone(),
                request_sha256: loaded.sha256.clone(),
            },
            Some((native.finalized_request(), native.request_sha256())),
        )?;
        let reconstructed = collect_round_ledger(&execute, planned, &report)?;
        if serde_json::to_value(&reconstructed)? != serde_json::to_value(round)? {
            bail!("platform trial/fit/evaluation ledger differs from actual native round readback");
        }
        let entries = actual_archive_entries(&zip)?;
        let result_zip = writer
            .put_file(
                &format!("native-{}-results.zip", round.round_id),
                &zip,
                &round.result_readback_bundle_sha256,
            )
            .await?;
        artifacts.push(result_zip.clone());
        let trials = u64::try_from(round.consumed_trials)?;
        consumed = consumed
            .checked_add(trials)
            .context("native trials overflow")?;
        rounds.push(CampaignRoundEvidence {
            round_id: round.round_id.clone(),
            seed: round.seed,
            native_mission_id: round.mission_id.clone(),
            native_mission_sha256: round.mission_sha256.clone(),
            consumed_trials: trials,
            result_zip: PublishedNativeArtifact {
                artifact: result_zip,
                native_publication_readback_sha256: round.result_readback_bundle_sha256.clone(),
            },
            entries,
        });
    }
    if consumed != u64::try_from(result.consumed_trials)? {
        bail!("native consumed trials differ from independently decoded rounds");
    }
    let evidence = CexCampaignResultReceipt {
        schema: "monday.cex_campaign_result_receipt.v1".into(),
        native_request_sha256: loaded.sha256.clone(),
        native_campaign_inputs_sha256: native.campaign_inputs_sha256().into(),
        collection_sha256: native.collection_id().into(),
        evaluation_protocol_sha256: native.evaluation_protocol_sha256().into(),
        runner: hft_cex_research_input::campaign::SourceBuildRefV1 {
            source_revision: loaded.request.build_source_revision.clone(),
            image_identity: context.spec.image.clone(),
        },
        native_campaign_id: result.campaign_id.clone(),
        declared_trials: u64::try_from(result.declared_total_trials)?,
        actual_consumed_trials: consumed,
        scientific_status: ScientificStatus::InsufficientEvidence,
        campaign_result: PublishedNativeArtifact {
            artifact: campaign_result,
            native_publication_readback_sha256: result_sha256.into(),
        },
        rounds,
    };
    evidence.validate(&context.spec, &artifacts)?;
    artifacts.push(
        writer
            .put("cex-campaign.json", serde_json::to_vec(&evidence)?)
            .await?,
    );
    let receipt = ResultReceipt {
        task_id: context.lease.task_id.clone(),
        attempt: context.lease.attempt,
        fence: context.lease.fence,
        view_manifest_sha256: context.spec.view_manifest_sha256.clone(),
        source_sha256: context.spec.source_sha256.clone(),
        image: context.spec.image.clone(),
        fit_identity_sha256: context.spec.fit_identity_sha256.clone(),
        artifacts,
        checkpoint: None,
        prepared_view: None,
    };
    receipt.validate(&context.spec, &context.lease)?;
    writer
        .put("receipt.json", serde_json::to_vec(&receipt)?)
        .await?;
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_archive_refs_are_actual_bytes_and_reject_unsafe_or_missing_entries(
    ) -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("native.zip");
        let make = |unsafe_entry: bool, missing: bool| -> anyhow::Result<()> {
            let mut zip = zip::ZipWriter::new(File::create(&path)?);
            let options = zip::write::SimpleFileOptions::default();
            for name in [
                "results/native-prepared-admission.json",
                "results/factor-bank.json",
                "results/mission-run.json",
                "results/mission-admission.json",
            ] {
                if missing && name == "results/factor-bank.json" {
                    continue;
                }
                zip.start_file(name, options)?;
                zip.write_all(b"{\"source\":\"actual-archive-fixture\"}")?;
            }
            if unsafe_entry {
                zip.start_file("../outside.json", options)?;
                zip.write_all(b"{}")?;
            }
            zip.finish()?;
            Ok(())
        };
        make(false, false)?;
        let entries = actual_archive_entries(&path)?;
        assert_eq!(entries.len(), 4);
        assert!(actual_archive_entries_with_limit(&path, entries[0].bytes * 3).is_err());
        assert!(entries.iter().all(|entry| entry.sha256
            == hft_cex_research_input::sha256(b"{\"source\":\"actual-archive-fixture\"}")
            && entry.bytes == b"{\"source\":\"actual-archive-fixture\"}".len() as u64));
        make(true, false)?;
        assert!(actual_archive_entries(&path).is_err());
        make(false, true)?;
        assert!(actual_archive_entries(&path).is_err());
        Ok(())
    }
    #[test]
    #[cfg(unix)]
    fn bounded_private_configuration_rejects_projected_links_public_permissions_and_oversize(
    ) -> anyhow::Result<()> {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let root = tempfile::tempdir()?;
        let directory = root.path().canonicalize()?;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
        let path = directory.join("config.json");
        std::fs::write(&path, b"{}")?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        assert_eq!(read_private_bounded(&path, 2)?, b"{}");
        assert!(read_private_bounded(&path, 1).is_err());
        let link = directory.join("projected.json");
        symlink(&path, &link)?;
        assert!(read_private_bounded(&link, 2).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
        assert!(read_private_bounded(&path, 2).is_err());
        Ok(())
    }
}
