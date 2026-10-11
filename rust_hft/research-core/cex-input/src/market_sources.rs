//! Pure market-feature provenance shared by preparation and independent readback.
use anyhow::{bail, Context};
use hft_research_manifest::market_encoder::MarketFeatureDatasetV1;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeatureSources {
    pub schema_version: String,
    pub market: String,
    pub symbol: String,
    pub replay_clock: String,
    pub source_revision: String,
    pub source_segments: Vec<serde_json::Value>,
    pub feature_start_received_at_ns: Option<u64>,
    pub feature_end_received_at_ns: Option<u64>,
    pub first_feature_observed_at_ms: i64,
    pub last_feature_observed_at_ms: i64,
    pub first_dependency_received_at_ns: u64,
    pub last_dependency_received_at_ns: u64,
}

/// Shared pure provenance check for native preparation, cohort admission and
/// independent readback. The label-bound PIT snapshot is checked separately;
/// the full feature source identity covers warmup and unlabeled tail frames.
pub fn verify_feature_sources(
    report: &serde_json::Value,
    source_bytes: &[u8],
    expected_sha256: &str,
    features: &MarketFeatureDatasetV1,
) -> anyhow::Result<()> {
    if source_bytes.len() > 16 * 1024 * 1024 {
        bail!("market feature source exceeds byte bound");
    }
    features.validate().map_err(anyhow::Error::msg)?;
    let sources: FeatureSources = serde_json::from_slice(source_bytes)?;
    let exported = &report["market_encoder"];
    let optional_clock = |value: &serde_json::Value| -> anyhow::Result<Option<u64>> {
        if value.is_null() {
            Ok(None)
        } else {
            Ok(Some(
                value
                    .as_u64()
                    .context("invalid feature source window clock")?,
            ))
        }
    };
    let raw_start = optional_clock(&report["output_start_received_at_ns"])?;
    let feature_start = optional_clock(&exported["feature_start_received_at_ns"])?;
    let expected_start = feature_start.or(raw_start);
    let label_end = optional_clock(&report["output_end_received_at_ns"])?;
    let expected_end = optional_clock(&exported["feature_end_received_at_ns"])?.or(label_end);
    if let Some(start) = feature_start {
        let raw_start =
            raw_start.context("explicit feature start has no admitted raw/PIT window")?;
        let end = expected_end.context("explicit feature start has no admitted end")?;
        if start % 1_000_000_000 != 0
            || start < raw_start
            || start - raw_start > 60_000_000_000
            || start >= end
        {
            bail!("market feature start escaped its 60-second raw/PIT warmup");
        }
    }
    let first = features
        .shards
        .first()
        .context("feature source has no shards")?
        .first_observed_at_ms;
    let last = features
        .shards
        .last()
        .context("feature source has no shards")?
        .last_observed_at_ms;
    let raw = report["source_segments"]
        .as_array()
        .context("missing feature source segments")?;
    let hashes = sources
        .source_segments
        .iter()
        .map(|s| s["sha256"].as_str().context("feature source has no digest"))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let revision = hft_research_manifest::source::source_revision(hashes);
    let first_source = sources
        .source_segments
        .iter()
        .map(|s| {
            s["start_received_at_ns"]
                .as_u64()
                .context("missing source start")
        })
        .collect::<anyhow::Result<Vec<_>>>()?
        .into_iter()
        .min()
        .context("feature source has no segments")?;
    let last_source = sources
        .source_segments
        .iter()
        .map(|s| {
            s["end_received_at_ns"]
                .as_u64()
                .context("missing source end")
        })
        .collect::<anyhow::Result<Vec<_>>>()?
        .into_iter()
        .max()
        .context("feature source has no segments")?;
    let first_ns = u64::try_from(first)?
        .checked_mul(1_000_000)
        .context("feature source clock overflow")?;
    let last_ns = u64::try_from(last)?
        .checked_mul(1_000_000)
        .context("feature source clock overflow")?;
    if hft_research_manifest::market_encoder::bytes_digest(source_bytes) != expected_sha256
        || exported["schema_version"] != "monday.market_encoder_export.v1"
        || exported["features"]["sha256"] != features.digest().map_err(anyhow::Error::msg)?
        || features.source_manifest_sha256 != expected_sha256
        || exported["feature_sources"]["sha256"] != expected_sha256
        || exported["feature_sources"]["file"]
            != format!("{expected_sha256}.market-feature-sources.json")
        || sources.schema_version != "monday.market_feature_sources.v1"
        || sources.market != "usdm"
        || sources.symbol != "SOLUSDT"
        || sources.replay_clock != hft_research_manifest::CEX_REPLAY_CLOCK_RECEIVED_AT_NS
        || report["source_revision"] != sources.source_revision
        || sources.source_revision != revision
        || sources.source_segments != *raw
        || sources.feature_start_received_at_ns != expected_start
        || sources.feature_end_received_at_ns != expected_end
        || sources.first_feature_observed_at_ms != first
        || sources.last_feature_observed_at_ms != last
        || first_ns
            .checked_sub(1_000_000_000)
            .is_none_or(|t| sources.first_dependency_received_at_ns > t)
        || sources.first_dependency_received_at_ns < first_source
        || sources.last_dependency_received_at_ns != last_ns
        || last_ns > last_source
        || sources
            .feature_start_received_at_ns
            .is_some_and(|t| t > first_ns)
        || sources
            .feature_end_received_at_ns
            .is_some_and(|t| t <= last_ns)
    {
        bail!("market feature source manifest differs from original raw sources or full feature bounds");
    }
    Ok(())
}
