//! Public Binance Spot exchangeInfo collector.

use crate::binance_spot_reference_artifact::{
    publish_spot_reference, verify_spot_reference_artifact, PublishedSpotReferenceArtifact,
    SpotReferenceArtifactConfig,
};
use async_trait::async_trait;
use data::binance_reference_common::{
    required_u64, validate_receive_clock, ReferenceClockValidator,
};
use data::binance_spot_reference::{
    active_spot_instrument_rules, observe_reference_clocks, SpotReferenceBatch,
    EXCHANGE_INFO_ENDPOINT, OFFICIAL_SOURCE_ORIGIN, SERVER_TIME_ENDPOINT,
};
use std::collections::BTreeSet;

pub use crate::binance_usdm_reference_collector::{HttpReferenceSource, TimedJson};

pub const OFFICIAL_SPOT_SOURCE_ORIGIN: &str = OFFICIAL_SOURCE_ORIGIN;

#[async_trait]
pub trait SpotReferenceSource: Sync {
    fn source_origin(&self) -> &str;
    async fn server_time(&self) -> anyhow::Result<TimedJson>;
    async fn exchange_info(&self) -> anyhow::Result<TimedJson>;
}

#[async_trait]
impl SpotReferenceSource for HttpReferenceSource {
    fn source_origin(&self) -> &str {
        self.source_origin()
    }

    async fn server_time(&self) -> anyhow::Result<TimedJson> {
        self.get_public(SERVER_TIME_ENDPOINT).await
    }

    async fn exchange_info(&self) -> anyhow::Result<TimedJson> {
        self.get_public(EXCHANGE_INFO_ENDPOINT).await
    }
}

#[derive(Debug, Clone)]
pub struct CollectedSpotReference {
    source_origin: String,
    server_time_ms: u64,
    source_clock_received_at_ns: u64,
    exchange_info_received_at_ns: u64,
    batch: SpotReferenceBatch,
}

impl CollectedSpotReference {
    pub fn source_origin(&self) -> &str {
        &self.source_origin
    }

    pub fn server_time_ms(&self) -> u64 {
        self.server_time_ms
    }

    pub fn source_clock_received_at_ns(&self) -> u64 {
        self.source_clock_received_at_ns
    }

    pub fn exchange_info_received_at_ns(&self) -> u64 {
        self.exchange_info_received_at_ns
    }

    pub fn batch(&self) -> &SpotReferenceBatch {
        &self.batch
    }
}

pub async fn collect_spot_reference(
    source: &dyn SpotReferenceSource,
    requested_symbols: Option<&BTreeSet<String>>,
    clocks: &mut ReferenceClockValidator,
) -> anyhow::Result<CollectedSpotReference> {
    if source.source_origin() != OFFICIAL_SPOT_SOURCE_ORIGIN {
        anyhow::bail!("Spot reference source origin is not official Binance");
    }
    if requested_symbols.is_some_and(BTreeSet::is_empty) {
        anyhow::bail!("requested Spot symbol set is empty");
    }
    let server_time = source.server_time().await?;
    let server_time_ms = required_u64(&server_time.value, "serverTime", SERVER_TIME_ENDPOINT)?;
    validate_receive_clock(server_time_ms, server_time.received_at_ns)?;
    let exchange_info = source.exchange_info().await?;
    let rules = active_spot_instrument_rules(
        &exchange_info.value,
        server_time_ms,
        server_time.received_at_ns,
        exchange_info.received_at_ns,
        requested_symbols,
    )?;
    let batch = SpotReferenceBatch::new(rules)?;
    observe_reference_clocks(clocks, batch.rules())?;
    Ok(CollectedSpotReference {
        source_origin: source.source_origin().to_owned(),
        server_time_ms,
        source_clock_received_at_ns: server_time.received_at_ns,
        exchange_info_received_at_ns: exchange_info.received_at_ns,
        batch,
    })
}

/// Collect and seal one public Spot reference batch.  The returned triplet has
/// already passed local hash, marker, path, schema, and coverage readback.
pub async fn collect_and_publish_spot_reference(
    source: &dyn SpotReferenceSource,
    requested_symbols: Option<&BTreeSet<String>>,
    clocks: &mut ReferenceClockValidator,
    artifact_config: &SpotReferenceArtifactConfig,
) -> anyhow::Result<PublishedSpotReferenceArtifact> {
    let collected = collect_spot_reference(source, requested_symbols, clocks).await?;
    let artifact = publish_spot_reference(
        artifact_config,
        collected.source_origin(),
        collected.source_clock_received_at_ns(),
        collected.exchange_info_received_at_ns(),
        collected.batch(),
    )?;
    verify_spot_reference_artifact(&artifact, &artifact.data_sha256, &artifact.manifest_sha256)?;
    Ok(artifact)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use async_trait::async_trait;
    use serde_json::{json, Value};
    use std::fs;
    use tempfile::tempdir;

    const SOURCE_MS: u64 = 1_700_000_000_000;
    const CLOCK_RECEIVED_NS: u64 = 1_700_000_000_100_000_000;
    const EXCHANGE_RECEIVED_NS: u64 = 1_700_000_000_200_000_000;

    struct FakeSource {
        origin: &'static str,
        server_time: Value,
        exchange_info: Value,
        server_received_at_ns: u64,
        exchange_received_at_ns: u64,
    }

    #[async_trait]
    impl SpotReferenceSource for FakeSource {
        fn source_origin(&self) -> &str {
            self.origin
        }

        async fn server_time(&self) -> Result<TimedJson> {
            Ok(TimedJson {
                value: self.server_time.clone(),
                received_at_ns: self.server_received_at_ns,
            })
        }

        async fn exchange_info(&self) -> Result<TimedJson> {
            Ok(TimedJson {
                value: self.exchange_info.clone(),
                received_at_ns: self.exchange_received_at_ns,
            })
        }
    }

    fn symbol(name: &str) -> Value {
        json!({
            "symbol": name,
            "status": "TRADING",
            "isSpotTradingAllowed": true,
            "permissionSets": [["SPOT"]],
            "baseAsset": "BTC",
            "quoteAsset": "USDT",
            "baseAssetPrecision": 8,
            "quoteAssetPrecision": 8,
            "filters": [
                {"filterType":"PRICE_FILTER","minPrice":"0.01","maxPrice":"1000000","tickSize":"0.01"},
                {"filterType":"LOT_SIZE","minQty":"0.00001","maxQty":"9000","stepSize":"0.00001"},
                {"filterType":"MIN_NOTIONAL","minNotional":"5","applyToMarket":true,"avgPriceMins":5}
            ]
        })
    }

    fn source() -> FakeSource {
        FakeSource {
            origin: OFFICIAL_SPOT_SOURCE_ORIGIN,
            server_time: json!({"serverTime": SOURCE_MS}),
            exchange_info: json!({"symbols": [symbol("BTCUSDT")]}),
            server_received_at_ns: CLOCK_RECEIVED_NS,
            exchange_received_at_ns: EXCHANGE_RECEIVED_NS,
        }
    }

    #[tokio::test]
    async fn collects_public_spot_rules_with_the_server_clock() {
        let collected =
            collect_spot_reference(&source(), None, &mut ReferenceClockValidator::default())
                .await
                .unwrap();
        assert_eq!(collected.source_origin(), OFFICIAL_SPOT_SOURCE_ORIGIN);
        assert_eq!(collected.server_time_ms(), SOURCE_MS);
        assert_eq!(collected.batch().rules().len(), 1);
        assert_eq!(
            collected.batch().rules()[0].source_clock_received_at_ns,
            CLOCK_RECEIVED_NS
        );
    }

    #[tokio::test]
    async fn collect_and_publish_seals_a_public_spot_triplet() {
        let temp = tempdir().unwrap();
        let artifact = collect_and_publish_spot_reference(
            &source(),
            None,
            &mut ReferenceClockValidator::default(),
            &SpotReferenceArtifactConfig {
                output_root: fs::canonicalize(temp.path()).unwrap(),
                observed_at_ns: EXCHANGE_RECEIVED_NS + 100_000_000,
                max_staleness_ms: 1_000,
            },
        )
        .await
        .unwrap();

        assert!(artifact.data_path.is_file());
        assert!(artifact.manifest_path.is_file());
        assert!(artifact.success_path.is_file());
        assert_eq!(
            verify_spot_reference_artifact(
                &artifact,
                &artifact.data_sha256,
                &artifact.manifest_sha256,
            )
            .unwrap()
            .rules()
            .len(),
            1
        );
    }

    #[tokio::test]
    async fn requested_missing_or_non_official_spot_inputs_fail_closed() {
        let requested = BTreeSet::from(["ETHUSDT".to_owned()]);
        assert!(collect_spot_reference(
            &source(),
            Some(&requested),
            &mut ReferenceClockValidator::default(),
        )
        .await
        .is_err());

        let wrong = FakeSource {
            origin: "https://example.com",
            ..source()
        };
        assert!(
            collect_spot_reference(&wrong, None, &mut ReferenceClockValidator::default(),)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn source_clock_regression_is_rejected_without_advancing_validator() {
        let mut clocks = ReferenceClockValidator::default();
        collect_spot_reference(&source(), None, &mut clocks)
            .await
            .unwrap();
        let regressed = FakeSource {
            server_time: json!({"serverTime": SOURCE_MS - 1}),
            ..source()
        };
        assert!(collect_spot_reference(&regressed, None, &mut clocks)
            .await
            .is_err());
        let current = collect_spot_reference(&source(), None, &mut clocks)
            .await
            .unwrap();
        assert_eq!(current.server_time_ms(), SOURCE_MS);
    }
}
