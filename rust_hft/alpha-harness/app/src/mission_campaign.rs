use crate::{
    cli::{
        print_json, CampaignExecuteArgs, CampaignIdArgs, ExecuteMissionArgs, BUILD_SOURCE_REVISION,
    },
    data_mission,
    mission_render::render_cex_bundle,
    mission_runner::{
        ensure_holdout_claim_absent, execute_report, fetch_to_file, normalized_sha256,
        publish_immutable_file, valid_git_revision, validate_cex_holdout_id, ExecutionBinding,
    },
    prediction_dispatch::{
        canonical_https_object, canonical_tokyo_oss_internal_object,
        cex_campaign_round_result_and_holdout_claim, cex_campaign_round_root, sha256_text,
        validate_dns_label,
    },
};
use alpha_domain::canonical_json_hash;
use anyhow::{bail, Context};
use reqwest::{blocking::Client, redirect::Policy, StatusCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{path::Path, time::Duration};

const CAMPAIGN_REQUEST_SCHEMA_V1: &str = "cex-campaign-request-v1";
const CAMPAIGN_RESULT_SCHEMA_V1: &str = "cex-campaign-result-v1";
const CAMPAIGN_IDENTITY_SCHEMA_V1: &str = "cex-campaign-identity-v1";
const STOP_RULE_V1: &str = "single-mission-terminal";
const MAX_REQUEST_BYTES: u64 = 1024 * 1024;
const MAX_CAMPAIGN_RESULT_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CampaignRequest {
    pub(crate) schema_version: String,
    pub(crate) campaign_id: String,
    pub(crate) build_source_revision: String,
    pub(crate) image_identity: String,
    pub(crate) feature_url: String,
    pub(crate) feature_sha256: String,
    pub(crate) materialization_url: String,
    pub(crate) materialization_sha256: String,
    pub(crate) replay_artifact_url: String,
    pub(crate) replay_artifact_sha256: String,
    pub(crate) replay_manifest_url: String,
    pub(crate) replay_manifest_sha256: String,
    pub(crate) holdout_id: String,
    pub(crate) round: CampaignRoundRequest,
    pub(crate) holdout_claim_put_url: String,
    pub(crate) holdout_claim_readback_url: String,
    pub(crate) campaign_result_put_url: String,
    pub(crate) campaign_result_readback_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CampaignRoundRequest {
    pub(crate) round_id: String,
    pub(crate) seed: u64,
    pub(crate) mission_put_url: String,
    pub(crate) mission_readback_url: String,
    pub(crate) result_put_url: String,
    pub(crate) result_readback_url: String,
}

#[derive(Debug, Serialize)]
struct CampaignIdReport {
    campaign_id: String,
    matches_request: bool,
}

#[derive(Debug, Serialize)]
struct CampaignMissionLedgerV1 {
    round_id: String,
    seed: u64,
    mission_id: String,
    mission_sha256: String,
    result_bundle_sha256: String,
    result_readback_bundle_sha256: String,
    replay_receipt_id: Option<String>,
    replay_gate_passed: Option<bool>,
    final_precommit_id: Option<String>,
    sealed_receipt_id: Option<String>,
    sealed_passed: Option<bool>,
    strategy_bundle_id: Option<String>,
    promotion_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct CampaignResultV1 {
    schema_version: &'static str,
    campaign_id: String,
    request_sha256: String,
    build_source_revision: String,
    image_identity: String,
    holdout_id: String,
    stop_rule: &'static str,
    termination_reason: String,
    mission: CampaignMissionLedgerV1,
    sealed_passed: Option<bool>,
    promotion_id: Option<String>,
}

#[derive(Debug)]
struct LoadedRequest {
    request: CampaignRequest,
    sha256: String,
}

pub fn execute(args: CampaignExecuteArgs) -> anyhow::Result<()> {
    let loaded = load_request(&args.request)?;
    if loaded.sha256 != normalized_sha256("campaign request", &args.request_sha256)? {
        bail!("campaign request SHA256 mismatch");
    }
    validate_request(&loaded.request)?;
    if loaded.request.campaign_id != args.campaign_id {
        bail!("campaign request does not match the requested Campaign ID");
    }
    if loaded.request.image_identity
        != normalized_sha256("campaign image identity", &args.image_identity)?
    {
        bail!("campaign request image identity does not match the requested image identity");
    }
    if loaded.request.build_source_revision != BUILD_SOURCE_REVISION {
        bail!("campaign request source revision does not match this build");
    }
    if !valid_git_revision(BUILD_SOURCE_REVISION) {
        bail!("alpha-harness was built without an exact source revision");
    }
    let shared_input_dir = args.work_dir.join("shared-inputs");
    let mission_dir = args.work_dir.join("mission");
    let local_request_path = args.work_dir.join("campaign-request.json");
    let local_result_path = args.work_dir.join("campaign-result.json");
    let local_result_readback_path = args.work_dir.join("campaign-result-readback.json");
    for path in [
        &shared_input_dir,
        &mission_dir,
        &local_request_path,
        &local_result_path,
        &local_result_readback_path,
    ] {
        if path.try_exists()? {
            bail!(
                "Campaign execution requires empty campaign paths; existing path: {}",
                path.display()
            );
        }
    }
    std::fs::create_dir_all(&args.work_dir)?;
    std::fs::create_dir_all(&shared_input_dir)?;
    std::fs::create_dir_all(&mission_dir)?;
    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .redirect(Policy::none())
        .build()?;
    ensure_holdout_claim_absent(&client, &loaded.request.holdout_claim_readback_url)?;
    data_mission::write_json_atomic(&local_request_path, &loaded.request)?;

    let feature_path = shared_input_dir.join("features.jsonl");
    fetch_verified(
        &client,
        "campaign feature",
        &loaded.request.feature_url,
        &feature_path,
        &loaded.request.feature_sha256,
        crate::mission_runner::MAX_FEATURE_BYTES,
    )?;
    let materialization_path = shared_input_dir.join("materialization.json");
    fetch_verified(
        &client,
        "campaign materialization",
        &loaded.request.materialization_url,
        &materialization_path,
        &loaded.request.materialization_sha256,
        crate::mission_runner::MAX_MATERIALIZATION_BYTES,
    )?;
    let replay_artifact_path = shared_input_dir.join(format!(
        "{}.parquet",
        normalized_sha256(
            "campaign replay artifact",
            &loaded.request.replay_artifact_sha256
        )?
    ));
    fetch_verified(
        &client,
        "campaign replay artifact",
        &loaded.request.replay_artifact_url,
        &replay_artifact_path,
        &loaded.request.replay_artifact_sha256,
        1024 * 1024 * 1024,
    )?;
    let replay_manifest_path = shared_input_dir.join("replay-manifest.json");
    fetch_verified(
        &client,
        "campaign replay manifest",
        &loaded.request.replay_manifest_url,
        &replay_manifest_path,
        &loaded.request.replay_manifest_sha256,
        16 * 1024 * 1024,
    )?;

    let round = &loaded.request.round;
    let rendered = render_cex_bundle(&feature_path, &materialization_path, round.seed)?;
    if rendered.mission.spec.holdout.holdout_id != loaded.request.holdout_id {
        bail!("rendered Mission holdout ID drifted from the Campaign request");
    }
    let round_dir = mission_dir.join(&round.round_id);
    let mission_publish_dir = round_dir.join("admission");
    std::fs::create_dir_all(&mission_publish_dir)?;
    let mission_local_path = mission_publish_dir.join("mission.json");
    data_mission::write_json_atomic(&mission_local_path, &rendered.mission)?;
    let mission_readback_path = mission_publish_dir.join("mission-readback.json");
    let mission_sha256 = publish_round_mission(
        &client,
        &round.mission_put_url,
        &round.mission_readback_url,
        &mission_local_path,
        &mission_readback_path,
    )?;

    let report = execute_report(
        ExecuteMissionArgs {
            work_dir: round_dir.join("execute"),
            mission_id: rendered.mission_id.clone(),
            holdout_id: loaded.request.holdout_id.clone(),
            mission_url: mission_readback_path.to_string_lossy().into_owned(),
            mission_sha256: mission_sha256.clone(),
            feature_url: feature_path.to_string_lossy().into_owned(),
            materialization_url: materialization_path.to_string_lossy().into_owned(),
            replay_artifact_url: replay_artifact_path.to_string_lossy().into_owned(),
            replay_artifact_sha256: loaded.request.replay_artifact_sha256.clone(),
            replay_manifest_url: replay_manifest_path.to_string_lossy().into_owned(),
            replay_manifest_sha256: loaded.request.replay_manifest_sha256.clone(),
            resume_url: None,
            resume_sha256: None,
            result_put_url: round.result_put_url.clone(),
            result_readback_url: round.result_readback_url.clone(),
            holdout_claim_put_url: loaded.request.holdout_claim_put_url.clone(),
            holdout_claim_readback_url: loaded.request.holdout_claim_readback_url.clone(),
        },
        ExecutionBinding::Campaign {
            campaign_id: loaded.request.campaign_id.clone(),
            round_id: round.round_id.clone(),
            request_sha256: loaded.sha256.clone(),
        },
    )?;
    let mission = CampaignMissionLedgerV1 {
        round_id: round.round_id.clone(),
        seed: round.seed,
        mission_id: report.mission_id.clone(),
        mission_sha256: report.mission_sha256.clone(),
        result_bundle_sha256: report.bundle_sha256.clone(),
        result_readback_bundle_sha256: report.readback_bundle_sha256.clone(),
        replay_receipt_id: report.replay_receipt_id.clone(),
        replay_gate_passed: report.replay_gate_passed,
        final_precommit_id: report.final_precommit_id.clone(),
        sealed_receipt_id: report.sealed_receipt_id.clone(),
        sealed_passed: report.sealed_passed,
        strategy_bundle_id: report.strategy_bundle_id.clone(),
        promotion_id: report.promotion_id.clone(),
    };
    let termination_reason =
        if report.final_precommit_id.is_some() || report.sealed_receipt_id.is_some() {
            "single_mission_finalized".to_string()
        } else {
            "single_mission_completed_without_finalization".to_string()
        };

    let sealed_passed = mission.sealed_passed;
    let promotion_id = mission.promotion_id.clone();
    let result = CampaignResultV1 {
        schema_version: CAMPAIGN_RESULT_SCHEMA_V1,
        campaign_id: loaded.request.campaign_id.clone(),
        request_sha256: loaded.sha256.clone(),
        build_source_revision: loaded.request.build_source_revision.clone(),
        image_identity: loaded.request.image_identity.clone(),
        holdout_id: loaded.request.holdout_id.clone(),
        stop_rule: STOP_RULE_V1,
        termination_reason,
        mission,
        sealed_passed,
        promotion_id,
    };
    data_mission::write_json_atomic(&local_result_path, &result)?;
    let result_sha256 = crate::mission_runner::sha256_file(&local_result_path)?;
    publish_immutable_file(
        &client,
        &loaded.request.campaign_result_put_url,
        &local_result_path,
        "application/json",
    )?;
    let (_, readback_sha256) = fetch_to_file(
        &client,
        &loaded.request.campaign_result_readback_url,
        &local_result_readback_path,
        MAX_CAMPAIGN_RESULT_BYTES,
    )?;
    if readback_sha256 != result_sha256 {
        bail!("published campaign result readback SHA256 mismatch");
    }
    print_json(&serde_json::json!({
        "campaign_id": result.campaign_id,
        "request_sha256": result.request_sha256,
        "campaign_result_sha256": result_sha256,
        "campaign_result_readback_sha256": readback_sha256,
        "termination_reason": result.termination_reason,
        "sealed_passed": result.sealed_passed,
        "promotion_id": result.promotion_id,
        "mission": result.mission,
    }))
}

pub fn print_expected_id(args: CampaignIdArgs) -> anyhow::Result<()> {
    let loaded = load_request(&args.request)?;
    let expected = expected_campaign_id(&loaded.request)?;
    print_json(&CampaignIdReport {
        campaign_id: expected.clone(),
        matches_request: loaded.request.campaign_id == expected,
    })
}

fn load_request(path: &Path) -> anyhow::Result<LoadedRequest> {
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("open campaign request {}", path.display()))?;
    if file.metadata()?.len() > MAX_REQUEST_BYTES {
        bail!("campaign request exceeds {MAX_REQUEST_BYTES} bytes");
    }
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut bytes)?;
    let request: CampaignRequest = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse campaign request {}", path.display()))?;
    Ok(LoadedRequest {
        request,
        sha256: hex::encode(Sha256::digest(&bytes)),
    })
}

pub(crate) fn serialize_request(request: &CampaignRequest) -> anyhow::Result<Vec<u8>> {
    serde_json::to_vec_pretty(request).map_err(anyhow::Error::new)
}

#[cfg(test)]
pub(crate) fn valid_request_for_tests() -> CampaignRequest {
    tests::valid_request_for_other_modules()
}

pub(crate) fn validate_request(request: &CampaignRequest) -> anyhow::Result<()> {
    if request.schema_version != CAMPAIGN_REQUEST_SCHEMA_V1 {
        bail!("campaign request schema_version must be {CAMPAIGN_REQUEST_SCHEMA_V1}");
    }
    validate_campaign_id(&request.campaign_id)?;
    if request.image_identity
        != normalized_sha256("campaign image identity", &request.image_identity)?
    {
        bail!("campaign image identity must be a normalized SHA256");
    }
    if request.build_source_revision != request.build_source_revision.trim() {
        bail!("campaign source revision must not contain surrounding whitespace");
    }
    if !valid_git_revision(&request.build_source_revision) {
        bail!("campaign source revision must be an exact git revision");
    }
    validate_cex_holdout_id(&request.holdout_id)?;
    canonical_input_object("campaign feature", &request.feature_url)?;
    normalized_sha256("campaign feature", &request.feature_sha256)?;
    canonical_input_object("campaign materialization", &request.materialization_url)?;
    normalized_sha256("campaign materialization", &request.materialization_sha256)?;
    canonical_input_object("campaign replay artifact", &request.replay_artifact_url)?;
    normalized_sha256("campaign replay artifact", &request.replay_artifact_sha256)?;
    canonical_input_object("campaign replay manifest", &request.replay_manifest_url)?;
    normalized_sha256("campaign replay manifest", &request.replay_manifest_sha256)?;

    let claim_object =
        canonical_output_object("campaign holdout claim", &request.holdout_claim_put_url)?;
    let claim_readback_object = canonical_output_object(
        "campaign holdout claim readback",
        &request.holdout_claim_readback_url,
    )?;
    if claim_object != claim_readback_object {
        bail!("campaign holdout claim readback URL must identify the same immutable object");
    }
    let campaign_result_object =
        canonical_output_object("campaign result", &request.campaign_result_put_url)?;
    let campaign_result_readback_object = canonical_output_object(
        "campaign result readback",
        &request.campaign_result_readback_url,
    )?;
    if campaign_result_object != campaign_result_readback_object {
        bail!("campaign result readback URL must identify the same immutable object");
    }
    let campaign_root = campaign_output_root(&campaign_result_object)?;
    let expected_campaign_result_object = format!(
        "{campaign_root}/campaign-id={}/campaign-result.json",
        request.campaign_id
    );
    if campaign_result_object != expected_campaign_result_object {
        bail!("campaign result object must bind the exact Campaign ID");
    }
    let expected_claim_object = format!(
        "{}/holdout-id-sha256={}/sealed-holdout-claim.json",
        campaign_root,
        sha256_text(&request.holdout_id),
    );
    if claim_object != expected_claim_object {
        bail!("campaign holdout claim object must live at the Campaign root");
    }

    let round = &request.round;
    validate_dns_label("campaign round id", &round.round_id)?;
    let mission_object = canonical_output_object("campaign mission", &round.mission_put_url)?;
    let mission_readback_object =
        canonical_output_object("campaign mission readback", &round.mission_readback_url)?;
    if mission_object != mission_readback_object {
        bail!("campaign Mission readback URL must identify the same immutable object");
    }
    let expected_mission_object = format!(
        "{campaign_root}/campaign-id={}/round={}/mission.json",
        request.campaign_id, round.round_id,
    );
    if mission_object != expected_mission_object {
        bail!("campaign Mission object must live at campaign-id=<id>/round=<id>/mission.json");
    }
    let result_object = canonical_output_object("campaign result", &round.result_put_url)?;
    let result_readback_object =
        canonical_output_object("campaign result readback", &round.result_readback_url)?;
    if result_object != result_readback_object {
        bail!("campaign result readback URL must identify the same immutable object");
    }
    let root = cex_campaign_round_root(
        &result_object,
        &request.campaign_id,
        &round.round_id,
        "results.zip",
    )?;
    if root != campaign_root {
        bail!("campaign result must share one Campaign root");
    }
    let expected_claim = cex_campaign_round_result_and_holdout_claim(
        &result_object,
        &request.campaign_id,
        &round.round_id,
        &request.holdout_id,
    )?;
    if expected_claim != claim_object {
        bail!("campaign result and holdout claim must share one Campaign root");
    }
    let expected_id = expected_campaign_id(request)?;
    if request.campaign_id != expected_id {
        bail!("campaign request campaign_id does not match its semantic identity");
    }
    Ok(())
}

pub(crate) fn expected_campaign_id(request: &CampaignRequest) -> anyhow::Result<String> {
    let identity = serde_json::json!({
        "identity_schema_version": CAMPAIGN_IDENTITY_SCHEMA_V1,
        "request_schema_version": request.schema_version,
        "build_source_revision": request.build_source_revision,
        "image_identity": normalized_sha256("campaign image identity", &request.image_identity)?,
        "feature": {
            "object": canonical_input_object("campaign feature", &request.feature_url)?,
            "sha256": normalized_sha256("campaign feature", &request.feature_sha256)?,
        },
        "materialization": {
            "object": canonical_input_object("campaign materialization", &request.materialization_url)?,
            "sha256": normalized_sha256("campaign materialization", &request.materialization_sha256)?,
        },
        "replay_artifact": {
            "object": canonical_input_object("campaign replay artifact", &request.replay_artifact_url)?,
            "sha256": normalized_sha256("campaign replay artifact", &request.replay_artifact_sha256)?,
        },
        "replay_manifest": {
            "object": canonical_input_object("campaign replay manifest", &request.replay_manifest_url)?,
            "sha256": normalized_sha256("campaign replay manifest", &request.replay_manifest_sha256)?,
        },
        "holdout_id": request.holdout_id,
        "output_root": campaign_output_root(&canonical_output_object(
            "campaign result",
            &request.campaign_result_put_url,
        )?)?,
        "round": {
            "round_id": request.round.round_id,
            "seed": request.round.seed,
        },
        "stop_rule": STOP_RULE_V1,
    });
    Ok(format!(
        "cex-campaign-{}",
        &canonical_json_hash(&identity)?[..32]
    ))
}

fn validate_campaign_id(value: &str) -> anyhow::Result<()> {
    validate_dns_label("campaign id", value)?;
    if !value.starts_with("cex-campaign-") {
        bail!("campaign id must start with cex-campaign-");
    }
    Ok(())
}

fn campaign_output_root(result_object: &str) -> anyhow::Result<String> {
    const SEGMENT: &str = "/campaign-id=";
    let mut matches = result_object.match_indices(SEGMENT);
    let (index, _) = matches
        .next()
        .context("campaign result object must contain one Campaign ID binding")?;
    if matches.next().is_some() {
        bail!("campaign result object contains duplicate Campaign ID bindings");
    }
    let suffix = &result_object[index + SEGMENT.len()..];
    let (_, file_name) = suffix
        .split_once('/')
        .context("campaign result object must end with campaign-id=<id>/campaign-result.json")?;
    if file_name != "campaign-result.json" {
        bail!("campaign result object must end with campaign-id=<id>/campaign-result.json");
    }
    let root = result_object[..index].trim_end_matches('/');
    if root.is_empty() {
        bail!("campaign result object requires a Campaign root");
    }
    Ok(root.to_string())
}

fn fetch_verified(
    client: &Client,
    label: &str,
    source: &str,
    destination: &Path,
    expected_sha256: &str,
    max_bytes: u64,
) -> anyhow::Result<()> {
    let (_, sha256) = fetch_to_file(client, source, destination, max_bytes)?;
    if sha256 != normalized_sha256(label, expected_sha256)? {
        bail!("{label} SHA256 mismatch");
    }
    Ok(())
}

fn publish_round_mission(
    client: &Client,
    destination: &str,
    readback_url: &str,
    source: &Path,
    readback_path: &Path,
) -> anyhow::Result<String> {
    let mission_sha256 = crate::mission_runner::sha256_file(source)?;
    let already_exists =
        match publish_immutable_file(client, destination, source, "application/json") {
            Ok(()) => false,
            Err(error) if immutable_publish_conflict(&error) => true,
            Err(error) => return Err(error).context("publish Mission"),
        };
    let (_, mission_readback_sha256) = fetch_to_file(
        client,
        readback_url,
        readback_path,
        source.metadata()?.len(),
    )?;
    if mission_readback_sha256 != mission_sha256 {
        if already_exists {
            bail!("published Mission already exists with different bytes");
        }
        bail!("published Mission readback SHA256 mismatch");
    }
    Ok(mission_sha256)
}

fn immutable_publish_conflict(error: &anyhow::Error) -> bool {
    error
        .to_string()
        .starts_with("result destination already exists:")
        || error.chain().any(|cause| {
            cause
                .downcast_ref::<reqwest::Error>()
                .and_then(reqwest::Error::status)
                .is_some_and(|status| status == StatusCode::CONFLICT)
        })
}

fn canonical_input_object(label: &str, value: &str) -> anyhow::Result<String> {
    canonical_object(label, value)
}

fn canonical_output_object(label: &str, value: &str) -> anyhow::Result<String> {
    canonical_tokyo_oss_internal_object(label, value)
}

fn canonical_object(label: &str, value: &str) -> anyhow::Result<String> {
    canonical_https_object(label, value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn expected_campaign_id_ignores_signed_queries_and_output_transports() {
        let mut request = valid_request();
        let original = expected_campaign_id(&request).unwrap();
        request.feature_url.push_str("?signature=feature");
        request
            .materialization_url
            .push_str("?signature=materialization");
        request.replay_artifact_url.push_str("?signature=replay");
        request.replay_manifest_url.push_str("?signature=manifest");
        request
            .campaign_result_put_url
            .push_str("?signature=ignored");
        request
            .campaign_result_readback_url
            .push_str("?signature=ignored");
        request.round.mission_put_url.push_str("?signature=ignored");
        request
            .round
            .mission_readback_url
            .push_str("?signature=ignored");
        request.round.result_put_url.push_str("?signature=ignored");
        request
            .round
            .result_readback_url
            .push_str("?signature=ignored");
        request.holdout_claim_put_url.push_str("?signature=ignored");
        request
            .holdout_claim_readback_url
            .push_str("?signature=ignored");

        assert_eq!(expected_campaign_id(&request).unwrap(), original);
    }

    #[test]
    fn expected_campaign_id_binds_the_output_root() {
        let mut request = valid_request();
        let original = expected_campaign_id(&request).unwrap();
        request.campaign_result_put_url = request
            .campaign_result_put_url
            .replace("/research/", "/other-root/");

        assert_ne!(expected_campaign_id(&request).unwrap(), original);
    }

    #[test]
    fn validate_request_rejects_http_transport() {
        let mut request = valid_request();
        request.feature_url = request.feature_url.replacen("https://", "http://", 1);

        assert!(validate_request(&request).is_err());
    }

    #[test]
    fn round_result_path_binds_the_shared_holdout_claim() {
        let request = valid_request();
        let claim = cex_campaign_round_result_and_holdout_claim(
            &canonical_output_object("result", &request.round.result_put_url).unwrap(),
            &request.campaign_id,
            &request.round.round_id,
            &request.holdout_id,
        )
        .unwrap();

        assert_eq!(
            claim,
            canonical_output_object("holdout", &request.holdout_claim_put_url).unwrap()
        );
    }

    #[test]
    fn validate_request_rejects_non_oss_campaign_outputs() {
        let mut request = valid_request();
        request.holdout_claim_put_url =
            "https://example.com/research/sealed-holdout-claim.json".to_string();
        request.holdout_claim_readback_url = request.holdout_claim_put_url.clone();
        assert!(validate_request(&request).is_err());

        let mut request = valid_request();
        request.campaign_result_put_url =
            "https://example.com/research/campaign-id=placeholder/campaign-result.json".to_string();
        request.campaign_result_readback_url = request.campaign_result_put_url.clone();
        assert!(validate_request(&request).is_err());

        let mut request = valid_request();
        request.round.mission_put_url =
            "https://example.com/research/campaign-id=placeholder/round=r1/mission.json"
                .to_string();
        request.round.mission_readback_url = request.round.mission_put_url.clone();
        assert!(validate_request(&request).is_err());
    }

    #[test]
    fn publish_round_mission_accepts_an_existing_identical_object() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("rendered-mission.json");
        let destination = root.path().join("published-mission.json");
        let readback = root.path().join("mission-readback.json");
        std::fs::write(&source, br#"{"mission":"same"}"#).unwrap();
        std::fs::write(&destination, br#"{"mission":"same"}"#).unwrap();

        let client = Client::builder().redirect(Policy::none()).build().unwrap();
        let mission_sha256 = publish_round_mission(
            &client,
            &destination.to_string_lossy(),
            &destination.to_string_lossy(),
            &source,
            &readback,
        )
        .unwrap();

        assert_eq!(
            mission_sha256,
            crate::mission_runner::sha256_file(&destination).unwrap()
        );
        assert_eq!(
            crate::mission_runner::sha256_file(&readback).unwrap(),
            mission_sha256
        );
    }

    #[test]
    fn publish_round_mission_rejects_an_existing_different_object() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("rendered-mission.json");
        let destination = root.path().join("published-mission.json");
        let readback = root.path().join("mission-readback.json");
        std::fs::write(&source, br#"{"mission":"new"}"#).unwrap();
        let mut file = std::fs::File::create(&destination).unwrap();
        file.write_all(br#"{"mission":"old"}"#).unwrap();
        file.flush().unwrap();

        let client = Client::builder().redirect(Policy::none()).build().unwrap();
        let error = publish_round_mission(
            &client,
            &destination.to_string_lossy(),
            &destination.to_string_lossy(),
            &source,
            &readback,
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("published Mission already exists with different bytes"));
    }

    #[test]
    fn immutable_publish_conflict_recognizes_http_conflict() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 409 Conflict\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
        });
        let error = Client::new()
            .put(format!("http://{address}/mission.json"))
            .body("mission")
            .send()
            .unwrap()
            .error_for_status()
            .unwrap_err();
        server.join().unwrap();

        assert!(immutable_publish_conflict(&error.into()));
    }

    pub(crate) fn valid_request_for_other_modules() -> CampaignRequest {
        valid_request()
    }

    fn valid_request() -> CampaignRequest {
        const TEST_ROOT: &str =
            "https://monday-lob-apne1-1045353359.oss-ap-northeast-1-internal.aliyuncs.com/research";
        let mut request = CampaignRequest {
            schema_version: CAMPAIGN_REQUEST_SCHEMA_V1.to_string(),
            campaign_id: String::new(),
            build_source_revision: "a".repeat(40),
            image_identity: "1".repeat(64),
            feature_url: "https://oss-internal/research/features.jsonl".to_string(),
            feature_sha256: "1".repeat(64),
            materialization_url: "https://oss-internal/research/materialization.json".to_string(),
            materialization_sha256: "2".repeat(64),
            replay_artifact_url: "https://oss-internal/research/replay.parquet".to_string(),
            replay_artifact_sha256: "3".repeat(64),
            replay_manifest_url: "https://oss-internal/research/replay-manifest.json".to_string(),
            replay_manifest_sha256: "4".repeat(64),
            holdout_id: "cex-holdout-test".to_string(),
            round: CampaignRoundRequest {
                round_id: "r1".to_string(),
                seed: 11,
                mission_put_url: format!(
                    "{TEST_ROOT}/campaign-id=placeholder/round=r1/mission.json"
                ),
                mission_readback_url: format!(
                    "{TEST_ROOT}/campaign-id=placeholder/round=r1/mission.json?readback=1"
                ),
                result_put_url: format!("{TEST_ROOT}/campaign-id=placeholder/round=r1/results.zip"),
                result_readback_url: format!(
                    "{TEST_ROOT}/campaign-id=placeholder/round=r1/results.zip?readback=1"
                ),
            },
            holdout_claim_put_url: String::new(),
            holdout_claim_readback_url: String::new(),
            campaign_result_put_url: format!(
                "{TEST_ROOT}/campaign-id=placeholder/campaign-result.json"
            ),
            campaign_result_readback_url: format!(
                "{TEST_ROOT}/campaign-id=placeholder/campaign-result.json?readback=1"
            ),
        };
        request.campaign_id = expected_campaign_id(&request).unwrap();
        request.round.mission_put_url = format!(
            "{TEST_ROOT}/campaign-id={}/round={}/mission.json",
            request.campaign_id, request.round.round_id
        );
        request.round.mission_readback_url = format!(
            "{TEST_ROOT}/campaign-id={}/round={}/mission.json?readback=1",
            request.campaign_id, request.round.round_id
        );
        request.round.result_put_url = format!(
            "{TEST_ROOT}/campaign-id={}/round={}/results.zip",
            request.campaign_id, request.round.round_id
        );
        request.round.result_readback_url = format!(
            "{TEST_ROOT}/campaign-id={}/round={}/results.zip?readback=1",
            request.campaign_id, request.round.round_id
        );
        request.holdout_claim_put_url = format!(
            "{TEST_ROOT}/holdout-id-sha256={}/sealed-holdout-claim.json",
            sha256_text(&request.holdout_id)
        );
        request.holdout_claim_readback_url = format!(
            "{TEST_ROOT}/holdout-id-sha256={}/sealed-holdout-claim.json?readback=1",
            sha256_text(&request.holdout_id)
        );
        request.campaign_result_put_url = format!(
            "{TEST_ROOT}/campaign-id={}/campaign-result.json",
            request.campaign_id
        );
        request.campaign_result_readback_url = format!(
            "{TEST_ROOT}/campaign-id={}/campaign-result.json?readback=1",
            request.campaign_id
        );
        request
    }
}
