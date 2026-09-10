pub use adapter_binance_data::reference::{
    BinanceReferenceError, HttpReferenceSource, ReferenceResult, ReferenceSource, TimedJson,
    OFFICIAL_USDM_SOURCE_ORIGIN,
};
use anyhow::{bail, Context, Result};
use data::binance_usdm_reference::{
    active_perpetual_contracts, mark_index_funding_observations, open_interest_observation,
    CompleteReferenceBatch, ReferenceClockValidator, ReferenceKind, ReferenceMarket,
};
use futures::{stream, StreamExt, TryStreamExt};
use serde_json::Value;
use std::collections::BTreeSet;
use std::time::Duration;
use tracing::warn;

#[derive(Debug)]
pub struct CollectedReferenceBatch {
    source_origin: String,
    batch: CompleteReferenceBatch,
}

impl CollectedReferenceBatch {
    pub fn source_origin(&self) -> &str {
        &self.source_origin
    }

    pub fn batch(&self) -> &CompleteReferenceBatch {
        &self.batch
    }
}

pub async fn collect_complete_reference_batch(
    source: &dyn ReferenceSource,
    oi_concurrency: usize,
    clocks: &mut ReferenceClockValidator,
) -> Result<CollectedReferenceBatch> {
    if source.source_origin() != OFFICIAL_USDM_SOURCE_ORIGIN {
        bail!("USD-M reference source origin is not official Binance");
    }
    if oi_concurrency == 0 {
        bail!("OI concurrency must be positive");
    }
    match collect_complete_reference_batch_once(source, oi_concurrency, clocks).await {
        Err(error) => {
            let Some(BinanceReferenceError::RateLimited {
                endpoint,
                retry_after_seconds,
            }) = error.downcast_ref::<BinanceReferenceError>()
            else {
                return Err(error);
            };
            warn!(
                endpoint,
                retry_after_seconds,
                "Binance rate limited; restarting the complete reference batch once"
            );
            tokio::time::sleep(Duration::from_secs(*retry_after_seconds)).await;
            collect_complete_reference_batch_once(source, oi_concurrency, clocks).await
        }
        result => result,
    }
}

async fn collect_complete_reference_batch_once(
    source: &dyn ReferenceSource,
    oi_concurrency: usize,
    clocks: &mut ReferenceClockValidator,
) -> Result<CollectedReferenceBatch> {
    let server_time = source.server_time().await?;
    let source_time_ms = server_time
        .value
        .get("serverTime")
        .and_then(Value::as_u64)
        .context("server time response has invalid serverTime")?;
    let exchange_info = source.exchange_info().await?;
    let contracts = active_perpetual_contracts(
        &exchange_info.value,
        source_time_ms,
        server_time.received_at_ns,
        exchange_info.received_at_ns,
    )?;
    let expected = contracts
        .iter()
        .map(|row| row.symbol.clone())
        .collect::<BTreeSet<_>>();
    let premium_index = source.premium_index().await?;
    let marks = mark_index_funding_observations(
        &premium_index.value,
        &expected,
        premium_index.received_at_ns,
    )?;
    let open_interest = stream::iter(expected.iter().cloned())
        .map(|symbol| async move {
            let response = source.open_interest(&symbol).await?;
            open_interest_observation(&response.value, &symbol, response.received_at_ns)
        })
        .buffer_unordered(oi_concurrency)
        .try_collect::<Vec<_>>()
        .await?;
    let batch = CompleteReferenceBatch::new(contracts, marks, open_interest)?;
    for row in batch.contracts() {
        clocks.observe(
            ReferenceMarket::Usdm,
            ReferenceKind::Metadata,
            &row.symbol,
            row.source_time_ms,
            row.received_at_ns,
        )?;
    }
    for row in batch.mark_index_funding() {
        clocks.observe(
            ReferenceMarket::Usdm,
            ReferenceKind::MarkIndexFunding,
            &row.symbol,
            row.source_time_ms,
            row.received_at_ns,
        )?;
    }
    for row in batch.open_interest() {
        clocks.observe(
            ReferenceMarket::Usdm,
            ReferenceKind::OpenInterest,
            &row.symbol,
            row.source_time_ms,
            row.received_at_ns,
        )?;
    }
    Ok(CollectedReferenceBatch {
        source_origin: source.source_origin().to_owned(),
        batch,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use data::binance_usdm_reference::{OPEN_INTEREST_ENDPOINT, PREMIUM_INDEX_ENDPOINT};
    use rust_decimal::Decimal;
    use serde_json::{json, Value};
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const SOURCE_MS: u64 = 1_700_000_000_000;
    const RECEIVED_NS: u64 = 1_700_000_000_500_000_000;

    struct FakeSource {
        origin: &'static str,
        open_interest: BTreeMap<String, Value>,
    }

    impl FakeSource {
        fn complete() -> Self {
            Self {
                origin: OFFICIAL_USDM_SOURCE_ORIGIN,
                open_interest: BTreeMap::from([
                    (
                        "BTCUSDT".to_owned(),
                        json!({"symbol":"BTCUSDT","openInterest":"10659.509","time":SOURCE_MS + 100}),
                    ),
                    (
                        "ETHUSDT".to_owned(),
                        json!({"symbol":"ETHUSDT","openInterest":"50200","time":SOURCE_MS + 100}),
                    ),
                ]),
            }
        }

        fn timed(value: Value, received_at_ns: u64) -> TimedJson {
            TimedJson {
                value,
                received_at_ns,
            }
        }
    }

    #[async_trait]
    impl ReferenceSource for FakeSource {
        fn source_origin(&self) -> &str {
            self.origin
        }

        async fn server_time(&self) -> ReferenceResult<TimedJson> {
            Ok(Self::timed(json!({"serverTime": SOURCE_MS}), RECEIVED_NS))
        }

        async fn exchange_info(&self) -> ReferenceResult<TimedJson> {
            Ok(Self::timed(
                json!({"symbols":[
                    {"symbol":"BTCUSDT","pair":"BTCUSDT","contractType":"PERPETUAL","deliveryDate":4133404800000_u64,"onboardDate":1598252400000_u64,"status":"TRADING","baseAsset":"BTC","quoteAsset":"USDT","marginAsset":"USDT","filters":[{"filterType":"PRICE_FILTER","tickSize":"0.10"},{"filterType":"LOT_SIZE","stepSize":"0.001"},{"filterType":"MIN_NOTIONAL","notional":"5"}]},
                    {"symbol":"ETHUSDT","pair":"ETHUSDT","contractType":"PERPETUAL","deliveryDate":4133404800000_u64,"onboardDate":1598252400000_u64,"status":"TRADING","baseAsset":"ETH","quoteAsset":"USDT","marginAsset":"USDT","filters":[{"filterType":"PRICE_FILTER","tickSize":"0.01"},{"filterType":"LOT_SIZE","stepSize":"0.001"},{"filterType":"MIN_NOTIONAL","notional":"5"}]}
                ]}),
                RECEIVED_NS + 10,
            ))
        }

        async fn premium_index(&self) -> ReferenceResult<TimedJson> {
            Ok(Self::timed(
                json!([
                    {"symbol":"BTCUSDT","markPrice":"101.0","indexPrice":"100.0","lastFundingRate":"0.0001","interestRate":"0.0001","nextFundingTime":SOURCE_MS + 28_800_000,"time":SOURCE_MS},
                    {"symbol":"ETHUSDT","markPrice":"2001","indexPrice":"2000","lastFundingRate":"-0.0002","interestRate":"0.0001","nextFundingTime":SOURCE_MS + 28_800_000,"time":SOURCE_MS}
                ]),
                RECEIVED_NS + 20,
            ))
        }

        async fn basis(&self, _pair: &str, _period: &str) -> ReferenceResult<TimedJson> {
            Ok(Self::timed(json!([]), RECEIVED_NS + 25))
        }

        async fn open_interest(&self, symbol: &str) -> ReferenceResult<TimedJson> {
            let value = self.open_interest.get(symbol).cloned().ok_or_else(|| {
                BinanceReferenceError::Request {
                    endpoint: OPEN_INTEREST_ENDPOINT.to_owned(),
                    message: format!("missing fake OI for {symbol}"),
                }
            })?;
            Ok(Self::timed(value, RECEIVED_NS + 30))
        }
    }

    struct RateLimitedOnceSource {
        inner: FakeSource,
        server_time_calls: AtomicUsize,
    }

    #[async_trait]
    impl ReferenceSource for RateLimitedOnceSource {
        fn source_origin(&self) -> &str {
            self.inner.source_origin()
        }

        async fn server_time(&self) -> ReferenceResult<TimedJson> {
            self.server_time_calls.fetch_add(1, Ordering::SeqCst);
            self.inner.server_time().await
        }

        async fn exchange_info(&self) -> ReferenceResult<TimedJson> {
            self.inner.exchange_info().await
        }

        async fn premium_index(&self) -> ReferenceResult<TimedJson> {
            if self.server_time_calls.load(Ordering::SeqCst) == 1 {
                return Err(BinanceReferenceError::RateLimited {
                    endpoint: PREMIUM_INDEX_ENDPOINT.to_owned(),
                    retry_after_seconds: 0,
                });
            }
            self.inner.premium_index().await
        }

        async fn basis(&self, pair: &str, period: &str) -> ReferenceResult<TimedJson> {
            self.inner.basis(pair, period).await
        }

        async fn open_interest(&self, symbol: &str) -> ReferenceResult<TimedJson> {
            self.inner.open_interest(symbol).await
        }
    }

    #[tokio::test]
    async fn collects_a_complete_official_batch_with_every_source_clock() {
        let collected = collect_complete_reference_batch(
            &FakeSource::complete(),
            2,
            &mut ReferenceClockValidator::default(),
        )
        .await
        .unwrap();
        assert_eq!(collected.source_origin(), OFFICIAL_USDM_SOURCE_ORIGIN);
        assert_eq!(collected.batch().contracts().len(), 2);
        assert_eq!(
            collected.batch().contracts()[0].source_clock_received_at_ns,
            RECEIVED_NS
        );
        assert_eq!(
            collected.batch().mark_index_funding()[0].basis,
            Decimal::ONE
        );
        assert_eq!(collected.batch().open_interest().len(), 2);
    }

    #[tokio::test]
    async fn missing_oi_and_non_official_origins_fail_closed() {
        assert!(collect_complete_reference_batch(
            &FakeSource::complete(),
            0,
            &mut ReferenceClockValidator::default(),
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("OI concurrency must be positive"));

        let mut missing = FakeSource::complete();
        missing.open_interest.remove("ETHUSDT");
        assert!(collect_complete_reference_batch(
            &missing,
            2,
            &mut ReferenceClockValidator::default(),
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("missing fake OI"));

        let mut wrong_origin = FakeSource::complete();
        wrong_origin.origin = "https://example.com";
        assert!(collect_complete_reference_batch(
            &wrong_origin,
            2,
            &mut ReferenceClockValidator::default(),
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("not official Binance"));
    }

    #[test]
    fn shared_http_source_pins_the_exact_official_origin() {
        for origin in [
            "http://fapi.binance.com",
            "https://example.com",
            "https://fapi.binance.com.evil.example",
            "https://fapi.binance.com/path",
            "https://fapi.binance.com///",
        ] {
            assert!(HttpReferenceSource::new(origin, Duration::from_secs(1)).is_err());
        }
        HttpReferenceSource::new(OFFICIAL_USDM_SOURCE_ORIGIN, Duration::from_secs(1)).unwrap();
    }

    #[tokio::test]
    async fn rate_limit_restarts_the_complete_batch_once() {
        let source = RateLimitedOnceSource {
            inner: FakeSource::complete(),
            server_time_calls: AtomicUsize::new(0),
        };

        let collected =
            collect_complete_reference_batch(&source, 2, &mut ReferenceClockValidator::default())
                .await
                .unwrap();

        assert_eq!(source.server_time_calls.load(Ordering::SeqCst), 2);
        assert_eq!(collected.batch().open_interest().len(), 2);
    }
}
