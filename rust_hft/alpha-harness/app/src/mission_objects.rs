//! CEX Campaign result and sealed holdout object identities.
use anyhow::{bail, Context};
use hft_research_dispatch_io::{
    canonical_tokyo_oss_internal_object, sha256_text, validate_dns_label,
};

pub(crate) const CEX_GLOBAL_HOLDOUT_CLAIM_ROOT: &str =
    "https://monday-lob-apne1-1045353359.oss-ap-northeast-1-internal.aliyuncs.com/artifacts/alpha-results/cex-holdout-claims";

pub(crate) fn cex_result_attempt_and_holdout_claim(
    result_object: &str,
    mission_id: &str,
    holdout_id: &str,
) -> anyhow::Result<(String, String)> {
    let segment = format!("/mission-id={mission_id}/");
    let mut matches = result_object.match_indices(&segment);
    let (index, _) = matches
        .next()
        .context("result object must bind the exact Mission ID")?;
    if matches.next().is_some() || result_object[..index].contains("/mission-id=") {
        bail!("result object contains duplicate Mission ID bindings");
    }
    let attempt_id = result_object[index + segment.len()..]
        .strip_prefix("attempt=")
        .and_then(|value| value.strip_suffix("/results.zip"))
        .context("CEX result object must end with mission-id=<id>/attempt=<id>/results.zip")?;
    validate_dns_label("attempt id", attempt_id)?;
    Ok((
        attempt_id.to_string(),
        format!(
            "{}/holdout-id-sha256={}/sealed-holdout-claim.json",
            result_object[..index].trim_end_matches('/'),
            sha256_text(holdout_id),
        ),
    ))
}

pub(crate) fn cex_campaign_round_root(
    object: &str,
    campaign_id: &str,
    round_id: &str,
    file_name: &str,
) -> anyhow::Result<String> {
    let suffix = format!("/campaign-id={campaign_id}/round={round_id}/{file_name}");
    let root = object.strip_suffix(&suffix).with_context(|| {
        format!("CEX campaign object must end with campaign-id=<id>/round=<id>/{file_name}")
    })?;
    if root.is_empty() || root.contains("/campaign-id=") || root.contains("/round=") {
        bail!("CEX campaign object contains duplicate Campaign ID bindings");
    }
    Ok(root.trim_end_matches('/').to_string())
}

pub(crate) fn cex_campaign_round_result_and_holdout_claim(
    result_object: &str,
    campaign_id: &str,
    round_id: &str,
    holdout_id: &str,
) -> anyhow::Result<String> {
    Ok(format!(
        "{}/holdout-id-sha256={}/sealed-holdout-claim.json",
        cex_campaign_round_root(result_object, campaign_id, round_id, "results.zip")?,
        sha256_text(holdout_id),
    ))
}

pub(crate) fn cex_global_holdout_claim_object(holdout_id: &str) -> anyhow::Result<String> {
    Ok(format!(
        "{}/holdout-id-sha256={}/sealed-holdout-claim.json",
        canonical_tokyo_oss_internal_object(
            "CEX sealed holdout claim root",
            CEX_GLOBAL_HOLDOUT_CLAIM_ROOT,
        )?,
        sha256_text(holdout_id),
    ))
}
