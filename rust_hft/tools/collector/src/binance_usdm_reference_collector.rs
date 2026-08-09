use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use data::binance_usdm_reference::{
    active_perpetual_contracts, mark_index_funding_observations, open_interest_observation,
    CompleteReferenceBatch, ReferenceClockValidator, ReferenceKind, EXCHANGE_INFO_ENDPOINT,
    OPEN_INTEREST_ENDPOINT, PREMIUM_INDEX_ENDPOINT, SERVER_TIME_ENDPOINT,
};
use futures::{stream, StreamExt, TryStreamExt};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::warn;

pub const OFFICIAL_USDM_SOURCE_ORIGIN: &str = "https://fapi.binance.com";

#[derive(Debug, Clone)]
pub struct TimedJson {
    pub value: Value,
    pub received_at_ns: u64,
}

#[derive(Debug)]
struct RateLimited {
    endpoint: String,
    retry_after_seconds: u64,
}

impl fmt::Display for RateLimited {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "USD-M reference endpoint {} returned HTTP 429 Too Many Requests",
            self.endpoint
        )
    }
}

impl std::error::Error for RateLimited {}

#[async_trait]
pub trait ReferenceSource: Sync {
    fn source_origin(&self) -> &str;
    async fn server_time(&self) -> Result<TimedJson>;
    async fn exchange_info(&self) -> Result<TimedJson>;
    async fn premium_index(&self) -> Result<TimedJson>;
    async fn open_interest(&self, symbol: &str) -> Result<TimedJson>;
}

#[derive(Debug, Clone)]
pub struct HttpReferenceSource {
    client: reqwest::Client,
}

impl HttpReferenceSource {
    pub fn new(source_origin: &str, timeout: Duration) -> Result<Self> {
        if source_origin.trim_end_matches('/') != OFFICIAL_USDM_SOURCE_ORIGIN {
            bail!("USD-M reference source must be the official Binance origin");
        }
        let parsed = reqwest::Url::parse(source_origin).context("invalid USD-M REST origin")?;
        if parsed.scheme() != "https"
            || parsed.host_str() != Some("fapi.binance.com")
            || parsed.port().is_some()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.path() != "/"
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            bail!("USD-M reference source must not include credentials, port, path, or query");
        }
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(timeout)
                .build()
                .context("build USD-M reference HTTP client")?,
        })
    }

    async fn get(&self, endpoint: &str, symbol: Option<&str>) -> Result<TimedJson> {
        self.get_url(
            &format!("{OFFICIAL_USDM_SOURCE_ORIGIN}{endpoint}"),
            endpoint,
            symbol,
        )
        .await
    }

    async fn get_url(&self, url: &str, endpoint: &str, symbol: Option<&str>) -> Result<TimedJson> {
        let mut request = self.client.get(url);
        if let Some(symbol) = symbol {
            request = request.query(&[("symbol", symbol)]);
        }
        let response = request
            .send()
            .await
            .context("USD-M reference request failed")?;
        let status = response.status();
        let retry_after_seconds = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(30)
            .min(60);
        let bytes = response
            .bytes()
            .await
            .context("USD-M reference response body failed")?;
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(RateLimited {
                endpoint: endpoint.to_owned(),
                retry_after_seconds,
            }
            .into());
        }
        let received_at_ns = now_ns()?;
        if !status.is_success() {
            bail!("USD-M reference endpoint {endpoint} returned HTTP {status}");
        }
        let value = serde_json::from_slice(&bytes).with_context(|| {
            format!("USD-M reference endpoint {endpoint} returned invalid JSON")
        })?;
        Ok(TimedJson {
            value,
            received_at_ns,
        })
    }
}

#[async_trait]
impl ReferenceSource for HttpReferenceSource {
    fn source_origin(&self) -> &str {
        OFFICIAL_USDM_SOURCE_ORIGIN
    }

    async fn server_time(&self) -> Result<TimedJson> {
        self.get(SERVER_TIME_ENDPOINT, None).await
    }

    async fn exchange_info(&self) -> Result<TimedJson> {
        self.get(EXCHANGE_INFO_ENDPOINT, None).await
    }

    async fn premium_index(&self) -> Result<TimedJson> {
        self.get(PREMIUM_INDEX_ENDPOINT, None).await
    }

    async fn open_interest(&self, symbol: &str) -> Result<TimedJson> {
        self.get(OPEN_INTEREST_ENDPOINT, Some(symbol)).await
    }
}

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
            let Some(rate_limit) = error.downcast_ref::<RateLimited>() else {
                return Err(error);
            };
            warn!(
                endpoint = rate_limit.endpoint,
                retry_after_seconds = rate_limit.retry_after_seconds,
                "Binance rate limited; restarting the complete reference batch once"
            );
            tokio::time::sleep(Duration::from_secs(rate_limit.retry_after_seconds)).await;
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
            ReferenceKind::Metadata,
            &row.symbol,
            row.source_time_ms,
            row.received_at_ns,
        )?;
    }
    for row in batch.mark_index_funding() {
        clocks.observe(
            ReferenceKind::MarkIndexFunding,
            &row.symbol,
            row.source_time_ms,
            row.received_at_ns,
        )?;
    }
    for row in batch.open_interest() {
        clocks.observe(
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

fn now_ns() -> Result<u64> {
    Ok(u64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use async_trait::async_trait;
    use rust_decimal::Decimal;
    use serde_json::{json, Value};
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

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

        async fn server_time(&self) -> Result<TimedJson> {
            Ok(Self::timed(json!({"serverTime": SOURCE_MS}), RECEIVED_NS))
        }

        async fn exchange_info(&self) -> Result<TimedJson> {
            Ok(Self::timed(
                json!({"symbols":[
                    {"symbol":"BTCUSDT","pair":"BTCUSDT","contractType":"PERPETUAL","deliveryDate":4133404800000_u64,"onboardDate":1598252400000_u64,"status":"TRADING","baseAsset":"BTC","quoteAsset":"USDT","marginAsset":"USDT","filters":[{"filterType":"PRICE_FILTER","tickSize":"0.10"},{"filterType":"LOT_SIZE","stepSize":"0.001"},{"filterType":"MIN_NOTIONAL","notional":"5"}]},
                    {"symbol":"ETHUSDT","pair":"ETHUSDT","contractType":"PERPETUAL","deliveryDate":4133404800000_u64,"onboardDate":1598252400000_u64,"status":"TRADING","baseAsset":"ETH","quoteAsset":"USDT","marginAsset":"USDT","filters":[{"filterType":"PRICE_FILTER","tickSize":"0.01"},{"filterType":"LOT_SIZE","stepSize":"0.001"},{"filterType":"MIN_NOTIONAL","notional":"5"}]}
                ]}),
                RECEIVED_NS + 10,
            ))
        }

        async fn premium_index(&self) -> Result<TimedJson> {
            Ok(Self::timed(
                json!([
                    {"symbol":"BTCUSDT","markPrice":"101.0","indexPrice":"100.0","lastFundingRate":"0.0001","interestRate":"0.0001","nextFundingTime":SOURCE_MS + 28_800_000,"time":SOURCE_MS},
                    {"symbol":"ETHUSDT","markPrice":"2001","indexPrice":"2000","lastFundingRate":"-0.0002","interestRate":"0.0001","nextFundingTime":SOURCE_MS + 28_800_000,"time":SOURCE_MS}
                ]),
                RECEIVED_NS + 20,
            ))
        }

        async fn open_interest(&self, symbol: &str) -> Result<TimedJson> {
            let value = self
                .open_interest
                .get(symbol)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("missing fake OI for {symbol}"))?;
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

        async fn server_time(&self) -> Result<TimedJson> {
            self.server_time_calls.fetch_add(1, Ordering::SeqCst);
            self.inner.server_time().await
        }

        async fn exchange_info(&self) -> Result<TimedJson> {
            self.inner.exchange_info().await
        }

        async fn premium_index(&self) -> Result<TimedJson> {
            if self.server_time_calls.load(Ordering::SeqCst) == 1 {
                return Err(RateLimited {
                    endpoint: PREMIUM_INDEX_ENDPOINT.to_owned(),
                    retry_after_seconds: 0,
                }
                .into());
            }
            self.inner.premium_index().await
        }

        async fn open_interest(&self, symbol: &str) -> Result<TimedJson> {
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
    fn http_source_pins_the_exact_official_origin() {
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
    async fn http_source_preserves_bounded_retry_after_on_rate_limit() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0_u8; 1024];
            let _ = stream.read(&mut buffer);
            stream
                .write_all(b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .unwrap();
        });
        let source = HttpReferenceSource {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(1))
                .build()
                .unwrap(),
        };

        let error = source
            .get_url(
                &format!("http://{address}/time"),
                SERVER_TIME_ENDPOINT,
                None,
            )
            .await
            .unwrap_err();

        assert_eq!(
            error
                .downcast_ref::<RateLimited>()
                .unwrap()
                .retry_after_seconds,
            0
        );
        server.join().unwrap();
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
