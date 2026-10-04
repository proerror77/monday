//! Independent restored-model prediction and native replay verification in ACK.
use super::*;
use alpha_domain::campaign_control::CampaignAttemptOutcomeV1;
use alpha_engine::sequence_study::{SequenceEnsemble, SequenceFit, SOL_SEQUENCE_POSITION_POLICY};
use std::collections::{BTreeMap, BTreeSet};
use worker::FoldResult;

pub(crate) fn cached_download(
    client: &Client,
    url: &str,
    path: &Path,
    limit: u64,
) -> anyhow::Result<()> {
    if path.try_exists()? {
        if !path.is_file() || path.metadata()?.len() > limit {
            bail!("invalid sequence cache file");
        }
        return Ok(());
    }
    let temporary = tempfile::NamedTempFile::new_in(path.parent().context("cache parent")?)?;
    fetch_to_file(client, url, temporary.path(), limit)?;
    temporary.persist_noclobber(path)?;
    Ok(())
}

pub(crate) fn readback(
    request: &SequenceRequest,
    request_hash: &str,
    root_hash: &str,
    input_root: &Path,
    cache: &Path,
    settled_hash: Option<&str>,
) -> anyhow::Result<(CampaignAttemptOutcomeV1, String, serde_json::Value)> {
    request.validate()?;
    std::fs::create_dir_all(cache)?;
    let result_path = cache.join("result.json");
    let client = Client::builder()
        .timeout(Duration::from_secs(300))
        .redirect(Policy::none())
        .build()?;
    cached_download(
        &client,
        &request.result_readback_url,
        &result_path,
        MAX_REQUEST_BYTES,
    )?;
    let hash = hft_research_artifacts::sha256_file(&result_path)?;
    if settled_hash.is_some_and(|expected| expected != hash) {
        bail!("settled sequence result changed");
    }
    let result: FoldResult = read_json(&result_path)?;
    validate_result(request, request_hash, root_hash, &result)?;
    let outcome = outcome(&result);
    if settled_hash.is_none() {
        let bundle = cache.join("results.zip");
        cached_download(
            &client,
            &request.bundle_readback_url,
            &bundle,
            MAX_RESULT_BUNDLE_BYTES,
        )?;
        let temporary = tempfile::tempdir_in(cache)?;
        let extracted = temporary.path().join("extracted");
        extract_bundle_with_file_limit(&bundle, &extracted, 64)?;
        if hft_research_artifacts::sha256_file(&extracted.join("result.json"))? != hash {
            bail!("sequence archive result differs from published result");
        }
        verify_artifact_set(&bundle, &extracted, &result)?;
        verify_restored(request, input_root, &extracted, temporary.path(), &result)?;
    }
    let report = serde_json::json!({
        "schema_version":"monday.sol_sequence_model_report.v1", "campaign_id":request.campaign_id,
        "request_sha256":request_hash, "result_sha256":hash, "root_grant_sha256":root_hash,
        "study_sha256":result.study_sha256, "fold_id":result.fold_id,
        "training_window_days":result.training_window_days, "groups":result.groups,
        "primary_fits_attempted":result.primary_fits_attempted,
        "verification_fits_attempted":result.verification_fits_attempted,
        "charged_trials":14, "outcome":outcome, "sealed_holdout_opened":false,
        "deployment_authority":false, "independent_prediction_and_replay_readback":true,
    });
    Ok((outcome, hash, report))
}

fn validate_result(
    request: &SequenceRequest,
    request_hash: &str,
    root_hash: &str,
    result: &FoldResult,
) -> anyhow::Result<()> {
    if result.schema_version != "monday.sol_sequence_fold_result.v1"
        || result.campaign_id != request.campaign_id
        || result.request_sha256 != request_hash
        || result.root_grant_sha256 != root_hash
        || result.study_sha256 != request.plan.content_hash().map_err(anyhow::Error::msg)?
        || result.fold_id != request.inputs.fold_id
        || result.training_window_days != request.inputs.training_window_days
        || result.position_policy != SOL_SEQUENCE_POSITION_POLICY
        || result.sealed_holdout_opened
        || result.deployment_authority
        || result.primary_fits_attempted != 7
        || result.verification_fits_attempted > 7
        || result
            .groups
            .iter()
            .map(|g| g.model_kind)
            .collect::<Vec<_>>()
            != request.plan.models
        || result
            .groups
            .iter()
            .map(|g| g.verified_members as u64)
            .sum::<u64>()
            > result.verification_fits_attempted
        || result.artifacts.len() > 40
    {
        bail!("sequence result identity, authority or trial accounting changed");
    }
    for (name, hash) in &result.artifacts {
        if name.contains('/')
            || !inputs::safe_relative_path(name)
            || !hft_research_manifest::sequence::valid_sha256(hash)
        {
            bail!("invalid sequence result artifact identity");
        }
    }
    Ok(())
}

fn verify_artifact_set(bundle: &Path, extracted: &Path, result: &FoldResult) -> anyhow::Result<()> {
    verify_artifact_inventory(bundle, extracted, "sequence-results", &result.artifacts)
}

pub(crate) fn verify_artifact_inventory(
    bundle: &Path,
    extracted: &Path,
    directory: &str,
    artifacts: &BTreeMap<String, String>,
) -> anyhow::Result<()> {
    let expected = artifacts
        .keys()
        .map(|name| format!("{directory}/{name}"))
        .chain(std::iter::once("result.json".into()))
        .collect::<BTreeSet<_>>();
    let mut archive = zip::ZipArchive::new(File::open(bundle)?)?;
    let mut actual = BTreeSet::new();
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        if entry.is_dir() || !actual.insert(entry.name().to_owned()) {
            bail!("unexpected sequence archive entry");
        }
    }
    if actual != expected {
        bail!("sequence archive inventory differs from result");
    }
    for (name, hash) in artifacts {
        if hft_research_artifacts::sha256_file(&extracted.join(directory).join(name))? != *hash {
            bail!("sequence result artifact bytes changed");
        }
    }
    Ok(())
}

fn verify_restored(
    request: &SequenceRequest,
    root: &Path,
    extracted: &Path,
    work: &Path,
    result: &FoldResult,
) -> anyhow::Result<()> {
    request.inputs.verify_mount(root)?;
    let fold = request
        .plan
        .folds
        .iter()
        .find(|fold| {
            fold.fold_id == request.inputs.fold_id
                && fold.training_window_days == request.inputs.training_window_days
        })
        .context("missing fold")?;
    let train = request
        .inputs
        .verify_view(root, &request.inputs.train, fold.train)?;
    let mut reader = worker::open_reader(root, &request.inputs.train, &train, fold.train)?;
    reader.finish_pass().map_err(anyhow::Error::msg)?;
    request.inputs.verify_replay(root, fold.validation)?;
    let output = work.join("independent");
    std::fs::create_dir(&output)?;
    let mut accounted = BTreeSet::new();
    for group in &result.groups {
        let name = serde_json::to_value(group.model_kind)?
            .as_str()
            .context("model name")?
            .to_owned();
        let seeds = if group.model_kind == alpha_domain::sequence_study::SequenceStudyModelV1::Ridge
        {
            vec![0]
        } else {
            request.plan.neural_seeds.clone()
        };
        let mut members = Vec::new();
        for seed in seeds {
            let meta = format!("{name}-{seed}.json");
            let weights = format!("{name}-{seed}.weights");
            match (result.artifacts.get(&meta), result.artifacts.get(&weights)) {
                (Some(hash), Some(_)) => {
                    let metadata = inputs::Artifact {
                        file: format!("sequence-results/{meta}"),
                        sha256: hash.clone(),
                    }
                    .read(extracted, 4 * 1024 * 1024)?;
                    let parameters = inputs::Artifact {
                        file: format!("sequence-results/{weights}"),
                        sha256: result.artifacts[&weights].clone(),
                    }
                    .read(extracted, 16 * 1024 * 1024)?;
                    let fitted = SequenceFit::restore(&request.plan, &metadata, hash, parameters)
                        .map_err(anyhow::Error::msg)?;
                    if fitted.identity().fold_id != result.fold_id
                        || fitted.identity().training_window_days != result.training_window_days
                        || fitted.identity().model_kind != group.model_kind
                        || fitted.identity().seed != seed
                    {
                        bail!("restored sequence model mislabeled");
                    }
                    accounted.insert(meta);
                    accounted.insert(weights);
                    members.push(fitted);
                }
                (None, None) => (),
                _ => bail!("incomplete sequence model bundle"),
            }
        }
        if members.len() != group.verified_members {
            bail!("sequence verified member count differs");
        }
        if group.state == "fit_failed" {
            if group.coverage.is_some()
                || group.replay.is_some()
                || group
                    .diagnostic
                    .as_ref()
                    .is_none_or(|d| d.is_empty() || d.len() > 8192)
            {
                bail!("invalid sequence failed-fit evidence");
            }
            continue;
        }
        let ensemble = SequenceEnsemble::new(&request.plan, members).map_err(anyhow::Error::msg)?;
        let mut artifacts = BTreeMap::new();
        let checked = worker::evaluate_group(
            request,
            root,
            &output,
            &ensemble,
            group.model_kind,
            &mut artifacts,
        )?;
        if checked != *group {
            bail!("independent sequence coverage or replay differs");
        }
        for (name, hash) in artifacts {
            if result.artifacts.get(&name) != Some(&hash) {
                bail!("independent sequence predictions or trace differ");
            }
            accounted.insert(name);
        }
    }
    if accounted != result.artifacts.keys().cloned().collect() {
        bail!("unaccounted sequence result artifacts");
    }
    Ok(())
}

fn outcome(result: &FoldResult) -> CampaignAttemptOutcomeV1 {
    if result
        .groups
        .iter()
        .any(|g| g.state == "development_candidate")
    {
        CampaignAttemptOutcomeV1::SelectedPreHoldout
    } else if result.groups.iter().any(|g| {
        matches!(
            g.state.as_str(),
            "fit_failed"
                | "replay_failed"
                | "incomplete_decision_grid"
                | "invalid_accounting"
                | "unverified_holding"
                | "execution_gate_failed"
        )
    }) {
        CampaignAttemptOutcomeV1::Failed
    } else {
        CampaignAttemptOutcomeV1::NoCandidate
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;
    use std::io::Write;

    fn empty_result() -> FoldResult {
        FoldResult {
            schema_version: "monday.sol_sequence_fold_result.v1".into(),
            campaign_id: "campaign".into(),
            request_sha256: "a".repeat(64),
            root_grant_sha256: "b".repeat(64),
            study_sha256: "c".repeat(64),
            fold_id: 1,
            training_window_days: 7,
            position_policy: SOL_SEQUENCE_POSITION_POLICY.into(),
            primary_fits_attempted: 7,
            verification_fits_attempted: 0,
            groups: Vec::new(),
            artifacts: BTreeMap::new(),
            sealed_holdout_opened: false,
            deployment_authority: false,
        }
    }

    #[test]
    fn sequence_readback_rejects_corrupt_cache_without_fetching() {
        let cache = tempfile::tempdir().unwrap();
        let path = cache.path().join("result.json");
        std::fs::write(&path, b"not-json").unwrap();
        let request = crate::mission_campaign::sequence::tests::request();
        let error = readback(
            &request,
            &"d".repeat(64),
            &"e".repeat(64),
            cache.path(),
            cache.path(),
            None,
        )
        .unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains("result.json") || message.contains("expected"),
            "{message}"
        );
        assert!(!message.to_ascii_lowercase().contains("dns"));
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        let client = Client::builder().build().unwrap();
        assert!(
            cached_download(&client, "https://example.invalid/result.json", &path, 100).is_err()
        );
    }

    #[test]
    fn sequence_archive_rejects_extra_and_tampered_artifacts() {
        let root = tempfile::tempdir().unwrap();
        let bundle = root.path().join("results.zip");
        let extracted = root.path().join("extracted");
        std::fs::create_dir(&extracted).unwrap();
        let mut archive = zip::ZipWriter::new(std::fs::File::create(&bundle).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        archive.start_file("result.json", options).unwrap();
        archive.write_all(b"{}").unwrap();
        archive
            .start_file("sequence-results/note.json", options)
            .unwrap();
        archive.write_all(b"model").unwrap();
        archive
            .start_file("sequence-results/sealed.json", options)
            .unwrap();
        archive.write_all(b"holdout").unwrap();
        archive.finish().unwrap();
        let mut result = empty_result();
        result.artifacts.insert("note.json".into(), "a".repeat(64));
        assert!(verify_artifact_set(&bundle, &extracted, &result).is_err());

        let bundle = root.path().join("exact.zip");
        let mut archive = zip::ZipWriter::new(std::fs::File::create(&bundle).unwrap());
        archive.start_file("result.json", options).unwrap();
        archive.write_all(b"{}").unwrap();
        archive
            .start_file("sequence-results/note.json", options)
            .unwrap();
        archive.write_all(b"model").unwrap();
        archive.finish().unwrap();
        let results = extracted.join("sequence-results");
        std::fs::create_dir(&results).unwrap();
        std::fs::write(results.join("note.json"), b"model").unwrap();
        assert!(verify_artifact_set(&bundle, &extracted, &result).is_err());
        result.artifacts.insert(
            "note.json".into(),
            format!("{:x}", sha2::Sha256::digest(b"model")),
        );
        verify_artifact_set(&bundle, &extracted, &result).unwrap();
    }

    #[test]
    fn sequence_execution_gate_failure_is_a_failed_attempt() {
        let mut result = empty_result();
        result.groups.push(worker::GroupResult {
            model_kind: alpha_domain::sequence_study::SequenceStudyModelV1::Ridge,
            state: "execution_gate_failed".into(),
            verified_members: 1,
            coverage: None,
            replay: None,
            report: None,
            diagnostic: Some("displayed depth exceeded the declared gate".into()),
        });
        assert_eq!(outcome(&result), CampaignAttemptOutcomeV1::Failed);
        result.groups[0].state = "no_trades_after_costs".into();
        assert_eq!(outcome(&result), CampaignAttemptOutcomeV1::NoCandidate);
    }
}
