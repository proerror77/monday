//! Reuse the original native Campaign and ZIP scientific validators. Platform
//! trial totals and worker entry claims cannot establish actual consumption.
use super::platform_facts::VerifiedPlatformFacts;
use alpha_store::campaign_ledger::CampaignPlatformScientificStatusV1;
use anyhow::{ensure, Context};
use hft_research_platform::{
    campaign_result::{CexCampaignResultReceipt, ScientificStatus},
    orchestrator::Artifact,
};
use reqwest::blocking::Client;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Write},
    path::Path,
};

pub(super) fn read(
    facts: &VerifiedPlatformFacts,
    request: &crate::mission_campaign::CampaignRequest,
    request_sha256: &str,
    artifact_client: &Client,
    source_client: &Client,
    urls: &BTreeMap<String, String>,
    cache: &Path,
) -> anyhow::Result<(
    CampaignPlatformScientificStatusV1,
    Option<u64>,
    Option<String>,
)> {
    let receipt = facts
        .snapshot
        .result
        .as_ref()
        .context("successful native compute lacks immutable output receipt")?;
    receipt.validate(&facts.snapshot.task.spec, &facts.lease)?;
    ensure!(
        urls.len() == receipt.artifacts.len()
            && receipt
                .artifacts
                .iter()
                .all(|artifact| urls.contains_key(&artifact.key)),
        "terminal transport changed exact PG output coverage"
    );
    let find = |name: &str| -> anyhow::Result<&Artifact> {
        let suffix = format!("/{name}");
        let found = receipt
            .artifacts
            .iter()
            .filter(|a| a.key.ends_with(&suffix))
            .collect::<Vec<_>>();
        ensure!(
            found.len() == 1,
            "ambiguous or absent native result artifact {name}"
        );
        Ok(found[0])
    };
    let metadata = find("cex-campaign.json")?;
    ensure!(
        metadata.bytes <= 1024 * 1024,
        "native evidence metadata exceeds bound"
    );
    fetch_artifact(
        artifact_client,
        metadata,
        &urls[&metadata.key],
        &cache.join("cex-campaign.json"),
    )?;
    let evidence: CexCampaignResultReceipt =
        serde_json::from_slice(&std::fs::read(cache.join("cex-campaign.json"))?)?;
    evidence.validate(&facts.snapshot.task.spec, &receipt.artifacts)?;
    ensure!(
        evidence.native_request_sha256 == request_sha256
            && evidence.native_campaign_inputs_sha256 == request.campaign_inputs_sha256
            && evidence.native_campaign_id == request.campaign_id
            && evidence.collection_sha256 == facts.snapshot.run.data_manifest_sha256
            && evidence.evaluation_protocol_sha256 == facts.snapshot.run.evaluation_protocol_sha256
            && evidence.runner.source_revision == request.build_source_revision
            && evidence.runner.image_identity == facts.snapshot.task.spec.image
            && evidence.declared_trials == request.declared_total_trials as u64
            && evidence.rounds.len() == request.rounds.len()
            && evidence.scientific_status == ScientificStatus::InsufficientEvidence
            && receipt.artifacts.len() == evidence.rounds.len() + 2,
        "native receipt changed original Campaign source, protocol or development scope"
    );
    ensure!(
        &evidence.campaign_result.artifact == find("native-campaign-result.json")?
            && evidence.campaign_result.native_publication_readback_sha256
                == evidence.campaign_result.artifact.sha256,
        "PG campaign result differs from original native publication"
    );
    fetch_artifact(
        artifact_client,
        &evidence.campaign_result.artifact,
        &urls[&evidence.campaign_result.artifact.key],
        &cache.join("campaign-result.json"),
    )?;
    let rounds = cache.join("round-readback");
    std::fs::create_dir(&rounds)?;
    for (index, (round, planned)) in evidence.rounds.iter().zip(&request.rounds).enumerate() {
        ensure!(
            round.round_id == planned.round_id
                && round.seed == planned.seed
                && round.result_zip.native_publication_readback_sha256
                    == round.result_zip.artifact.sha256,
            "native round changed original plan or publication"
        );
        let bundle = rounds.join(format!("round-{index}-results.zip"));
        fetch_artifact(
            artifact_client,
            &round.result_zip.artifact,
            &urls[&round.result_zip.artifact.key],
            &bundle,
        )?;
        hft_research_platform::campaign_result::verify_archive_entries(
            File::open(&bundle)?,
            &round.entries,
            512 * 1024 * 1024,
        )?;
        let mission = rounds.join(format!("round-{index}-mission.json"));
        let url = hft_research_dispatch_io::canonical_tokyo_oss_internal_object(
            "native terminal Mission",
            &planned.mission_readback_url,
        )?;
        fetch(
            source_client,
            &url,
            &mission,
            &round.native_mission_sha256,
            None,
            1024 * 1024,
        )?;
    }
    let (_outcome, actual, result_sha256) =
        crate::mission_campaign::readback_pre_holdout_terminal_cached(
            source_client,
            request,
            request_sha256,
            &facts.snapshot.run.evaluation_protocol_sha256,
            cache,
        )?;
    ensure!(
        actual == evidence.actual_consumed_trials
            && result_sha256 == evidence.campaign_result.artifact.sha256,
        "independent native ZIP/ledger validation differs from platform output"
    );
    // Development evidence remains insufficient for scientific promotion even
    // when its original ledger proves exact actual consumption.
    Ok((
        CampaignPlatformScientificStatusV1::InsufficientEvidence,
        Some(actual),
        Some(result_sha256),
    ))
}

fn fetch_artifact(
    client: &Client,
    artifact: &Artifact,
    url: &str,
    target: &Path,
) -> anyhow::Result<()> {
    ensure!(
        artifact.bytes > 0 && artifact.bytes <= 512 * 1024 * 1024,
        "native terminal artifact exceeds bound"
    );
    let parsed = reqwest::Url::parse(url)?;
    ensure!(
        parsed.path().ends_with(&format!("/{}", artifact.key)),
        "terminal artifact transport changed signed task/attempt/fence object key"
    );
    fetch(
        client,
        url,
        target,
        &artifact.sha256,
        Some(artifact.bytes),
        artifact.bytes,
    )
}

fn fetch(
    client: &Client,
    url: &str,
    target: &Path,
    digest: &str,
    expected_bytes: Option<u64>,
    limit: u64,
) -> anyhow::Result<()> {
    let parsed = reqwest::Url::parse(url)?;
    ensure!(
        parsed.scheme() == "https"
            && parsed.host_str().is_some()
            && parsed.username().is_empty()
            && parsed.password().is_none()
            && parsed.fragment().is_none(),
        "terminal artifact requires controlled HTTPS readback"
    );
    let mut response = client
        .get(url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(reqwest::Error::without_url)?;
    let mut file =
        tempfile::NamedTempFile::new_in(target.parent().context("artifact cache parent absent")?)?;
    let mut hash = Sha256::new();
    let mut count = 0_u64;
    let mut buffer = [0; 64 * 1024];
    loop {
        let n = response.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        count = count
            .checked_add(n as u64)
            .context("terminal artifact size overflow")?;
        ensure!(count <= limit, "terminal artifact exceeds its signed bound");
        file.write_all(&buffer[..n])?;
        hash.update(&buffer[..n]);
    }
    ensure!(
        count > 0
            && expected_bytes.is_none_or(|bytes| count == bytes)
            && hex::encode(hash.finalize()) == digest,
        "actual native terminal artifact bytes changed"
    );
    file.as_file().sync_all()?;
    file.persist_noclobber(target).map_err(|e| e.error)?;
    Ok(())
}
