//! Read-only Binance USD-M reference transport.
//!
//! This module owns the official REST origin, request construction, response
//! receipt clock, and bounded rate-limit classification. Typed observation
//! validation remains in hft-data::binance_usdm_reference; callers from the
//! collector and prediction-market module share this transport without making
//! either module depend on the other.

use async_trait::async_trait;
use reqwest::{redirect::Policy, Client};
use serde_json::Value;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use url::Url;

pub const OFFICIAL_USDM_SOURCE_ORIGIN: &str = "https://fapi.binance.com";
pub const OFFICIAL_SPOT_SOURCE_ORIGIN: &str = "https://api.binance.com";
pub const EXCHANGE_INFO_ENDPOINT: &str = "/fapi/v1/exchangeInfo";
pub const SERVER_TIME_ENDPOINT: &str = "/fapi/v1/time";
pub const PREMIUM_INDEX_ENDPOINT: &str = "/fapi/v1/premiumIndex";
pub const OPEN_INTEREST_ENDPOINT: &str = "/fapi/v1/openInterest";
pub const BASIS_ENDPOINT: &str = "/futures/data/basis";
pub const SPOT_EXCHANGE_INFO_ENDPOINT: &str = "/api/v3/exchangeInfo";
pub const SPOT_SERVER_TIME_ENDPOINT: &str = "/api/v3/time";

#[derive(Debug, Error)]
pub enum BinanceReferenceError {
    #[error("Binance reference source must be an official Binance origin")]
    InvalidOrigin,
    #[error("build USD-M reference HTTP client: {0}")]
    Client(String),
    #[error("USD-M reference request for {endpoint} failed: {message}")]
    Request { endpoint: String, message: String },
    #[error("USD-M reference endpoint {endpoint} returned HTTP 429 Too Many Requests")]
    RateLimited {
        endpoint: String,
        retry_after_seconds: u64,
    },
    #[error("USD-M reference endpoint {endpoint} returned HTTP {status}: {body}")]
    Http {
        endpoint: String,
        status: u16,
        body: String,
    },
    #[error("USD-M reference endpoint {endpoint} response body failed: {message}")]
    ResponseBody { endpoint: String, message: String },
    #[error("USD-M reference endpoint {endpoint} returned invalid JSON: {message}")]
    Json { endpoint: String, message: String },
    #[error("read local USD-M reference receipt clock: {0}")]
    Clock(String),
    #[error("reference endpoint {endpoint} is unavailable on the selected Binance origin")]
    Unsupported { endpoint: String },
}

pub type ReferenceResult<T> = Result<T, BinanceReferenceError>;

/// JSON body plus the local time at which the complete HTTP body was received.
#[derive(Debug, Clone)]
pub struct TimedJson {
    pub value: Value,
    pub received_at_ns: u64,
}

/// Common read-only source interface shared by the collector and prediction
/// CEX reference consumer.
#[async_trait]
pub trait ReferenceSource: Sync {
    fn source_origin(&self) -> &str;
    async fn server_time(&self) -> ReferenceResult<TimedJson>;
    async fn exchange_info(&self) -> ReferenceResult<TimedJson>;
    async fn premium_index(&self) -> ReferenceResult<TimedJson>;
    async fn basis(&self, pair: &str, period: &str) -> ReferenceResult<TimedJson>;
    async fn open_interest(&self, symbol: &str) -> ReferenceResult<TimedJson>;
}

#[derive(Debug, Clone)]
pub struct HttpReferenceSource {
    client: Client,
    source_origin: String,
    request_origin: String,
}

impl HttpReferenceSource {
    pub fn new(source_origin: &str, timeout: Duration) -> ReferenceResult<Self> {
        let origin = source_origin.trim_end_matches('/');
        if !matches!(
            origin,
            OFFICIAL_USDM_SOURCE_ORIGIN | OFFICIAL_SPOT_SOURCE_ORIGIN
        ) {
            return Err(BinanceReferenceError::InvalidOrigin);
        }
        let parsed = Url::parse(source_origin).map_err(|error| {
            BinanceReferenceError::Client(format!("invalid USD-M REST origin: {error}"))
        })?;
        if parsed.scheme() != "https"
            || !matches!(
                parsed.host_str(),
                Some("fapi.binance.com") | Some("api.binance.com")
            )
            || parsed.port().is_some()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.path() != "/"
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err(BinanceReferenceError::InvalidOrigin);
        }
        let client = Client::builder()
            .timeout(timeout)
            .user_agent("HFT-Binance-USD-M-Reference/1.0")
            // Reference responses are accepted only from the pinned origin;
            // following a Location would move the trust boundary.
            .redirect(Policy::none())
            .build()
            .map_err(|error| BinanceReferenceError::Client(error.to_string()))?;
        Ok(Self {
            client,
            source_origin: origin.to_owned(),
            request_origin: origin.to_owned(),
        })
    }

    pub fn official(timeout: Duration) -> ReferenceResult<Self> {
        Self::new(OFFICIAL_USDM_SOURCE_ORIGIN, timeout)
    }

    pub fn source_origin(&self) -> &str {
        &self.source_origin
    }

    /// Read one fixed, no-query public endpoint. Dynamic symbol/period
    /// requests use the typed methods below so an arbitrary caller cannot
    /// smuggle a host, path, or query into the reference client.
    pub async fn get_public(&self, endpoint: &str) -> ReferenceResult<TimedJson> {
        let endpoint = match (self.is_usdm(), endpoint) {
            (true, SERVER_TIME_ENDPOINT) => SERVER_TIME_ENDPOINT,
            (true, EXCHANGE_INFO_ENDPOINT) => EXCHANGE_INFO_ENDPOINT,
            (true, PREMIUM_INDEX_ENDPOINT) => PREMIUM_INDEX_ENDPOINT,
            (true, OPEN_INTEREST_ENDPOINT) => OPEN_INTEREST_ENDPOINT,
            (true, BASIS_ENDPOINT) => BASIS_ENDPOINT,
            (false, SPOT_SERVER_TIME_ENDPOINT) => SPOT_SERVER_TIME_ENDPOINT,
            (false, SPOT_EXCHANGE_INFO_ENDPOINT) => SPOT_EXCHANGE_INFO_ENDPOINT,
            _ => {
                return Err(BinanceReferenceError::Unsupported {
                    endpoint: endpoint.to_owned(),
                })
            }
        };
        self.get(endpoint, &[]).await
    }

    fn is_usdm(&self) -> bool {
        self.source_origin == OFFICIAL_USDM_SOURCE_ORIGIN
    }

    async fn get(
        &self,
        endpoint: &'static str,
        query: &[(&str, &str)],
    ) -> ReferenceResult<TimedJson> {
        let mut url = Url::parse(&self.request_origin).map_err(|error| {
            BinanceReferenceError::Client(format!("invalid request origin: {error}"))
        })?;
        url.set_path(endpoint);
        {
            let mut pairs = url.query_pairs_mut();
            pairs.clear();
            for (key, value) in query {
                pairs.append_pair(key, value);
            }
        }
        let response =
            self.client
                .get(url)
                .send()
                .await
                .map_err(|error| BinanceReferenceError::Request {
                    endpoint: endpoint.to_owned(),
                    message: error.to_string(),
                })?;
        let status = response.status();
        let retry_after_seconds = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(30)
            .min(60);
        let body = response
            .text()
            .await
            .map_err(|error| BinanceReferenceError::ResponseBody {
                endpoint: endpoint.to_owned(),
                message: error.to_string(),
            })?;
        let received_at_ns = now_ns()?;
        if status.as_u16() == 429 {
            return Err(BinanceReferenceError::RateLimited {
                endpoint: endpoint.to_owned(),
                retry_after_seconds,
            });
        }
        if !status.is_success() {
            return Err(BinanceReferenceError::Http {
                endpoint: endpoint.to_owned(),
                status: status.as_u16(),
                body,
            });
        }
        let value = serde_json::from_str(&body).map_err(|error| BinanceReferenceError::Json {
            endpoint: endpoint.to_owned(),
            message: error.to_string(),
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
        &self.source_origin
    }

    async fn server_time(&self) -> ReferenceResult<TimedJson> {
        let endpoint = if self.is_usdm() {
            SERVER_TIME_ENDPOINT
        } else {
            SPOT_SERVER_TIME_ENDPOINT
        };
        self.get(endpoint, &[]).await
    }

    async fn exchange_info(&self) -> ReferenceResult<TimedJson> {
        let endpoint = if self.is_usdm() {
            EXCHANGE_INFO_ENDPOINT
        } else {
            SPOT_EXCHANGE_INFO_ENDPOINT
        };
        self.get(endpoint, &[]).await
    }

    async fn premium_index(&self) -> ReferenceResult<TimedJson> {
        if !self.is_usdm() {
            return Err(BinanceReferenceError::Unsupported {
                endpoint: PREMIUM_INDEX_ENDPOINT.to_owned(),
            });
        }
        self.get(PREMIUM_INDEX_ENDPOINT, &[]).await
    }

    async fn basis(&self, pair: &str, period: &str) -> ReferenceResult<TimedJson> {
        if !self.is_usdm() {
            return Err(BinanceReferenceError::Unsupported {
                endpoint: BASIS_ENDPOINT.to_owned(),
            });
        }
        if !matches!(
            period,
            "5m" | "15m" | "30m" | "1h" | "2h" | "4h" | "6h" | "12h" | "1d" | "3d"
        ) {
            return Err(BinanceReferenceError::Unsupported {
                endpoint: BASIS_ENDPOINT.to_owned(),
            });
        }
        self.get(
            BASIS_ENDPOINT,
            &[
                ("pair", pair),
                ("contractType", "PERPETUAL"),
                ("period", period),
                ("limit", "1"),
            ],
        )
        .await
    }

    async fn open_interest(&self, symbol: &str) -> ReferenceResult<TimedJson> {
        if !self.is_usdm() {
            return Err(BinanceReferenceError::Unsupported {
                endpoint: OPEN_INTEREST_ENDPOINT.to_owned(),
            });
        }
        self.get(OPEN_INTEREST_ENDPOINT, &[("symbol", symbol)])
            .await
    }
}

fn now_ns() -> ReferenceResult<u64> {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| BinanceReferenceError::Clock(error.to_string()))?
            .as_nanos(),
    )
    .map_err(|error| BinanceReferenceError::Clock(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    fn test_source(base_url: String) -> HttpReferenceSource {
        let client = Client::builder()
            .timeout(Duration::from_secs(1))
            .user_agent("reference-test")
            .redirect(Policy::none())
            .build()
            .unwrap();
        HttpReferenceSource {
            client,
            source_origin: base_url.clone(),
            request_origin: base_url,
        }
    }

    #[test]
    fn official_origin_is_pinned_without_credentials_or_path() {
        for origin in [
            "http://fapi.binance.com",
            "https://example.com",
            "https://fapi.binance.com.evil.example",
            "https://fapi.binance.com/path",
            "https://fapi.binance.com///",
            "https://fapi.binance.com?redirect=evil",
            "https://fapi.binance.com#fragment",
            "https://user:pass@fapi.binance.com",
        ] {
            assert!(HttpReferenceSource::new(origin, Duration::from_secs(1)).is_err());
        }
        assert!(
            HttpReferenceSource::new(OFFICIAL_USDM_SOURCE_ORIGIN, Duration::from_secs(1)).is_ok()
        );
        let spot =
            HttpReferenceSource::new(OFFICIAL_SPOT_SOURCE_ORIGIN, Duration::from_secs(1)).unwrap();
        assert_eq!(spot.source_origin(), OFFICIAL_SPOT_SOURCE_ORIGIN);
    }

    #[tokio::test]
    async fn public_endpoint_is_pinned_to_the_selected_market() {
        let usdm = HttpReferenceSource::official(Duration::from_secs(1)).unwrap();
        let error = usdm.get_public("@evil.example/path").await.unwrap_err();
        assert!(matches!(
            error,
            BinanceReferenceError::Unsupported { endpoint } if endpoint == "@evil.example/path"
        ));

        let spot =
            HttpReferenceSource::new(OFFICIAL_SPOT_SOURCE_ORIGIN, Duration::from_secs(1)).unwrap();
        let error = spot.get_public(PREMIUM_INDEX_ENDPOINT).await.unwrap_err();
        assert!(matches!(
            error,
            BinanceReferenceError::Unsupported { endpoint } if endpoint == PREMIUM_INDEX_ENDPOINT
        ));
    }

    #[tokio::test]
    async fn shared_transport_records_complete_body_receive_time() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1_024];
            let _ = stream.read(&mut request);
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 18\r\nConnection: close\r\n\r\n{\"serverTime\":123}",
                )
                .unwrap();
        });
        let source = test_source(format!("http://{address}"));
        let timed = source.get(SERVER_TIME_ENDPOINT, &[]).await.unwrap();
        assert_eq!(timed.value["serverTime"], 123);
        assert!(timed.received_at_ns > 0);
        server.join().unwrap();
    }

    #[tokio::test]
    async fn shared_transport_preserves_bounded_rate_limit_metadata() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1_024];
            let _ = stream.read(&mut request);
            stream
                .write_all(
                    b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
        });
        let source = test_source(format!("http://{address}"));
        let error = source.get(SERVER_TIME_ENDPOINT, &[]).await.unwrap_err();
        assert!(matches!(
            error,
            BinanceReferenceError::RateLimited {
                retry_after_seconds: 0,
                ..
            }
        ));
        server.join().unwrap();
    }

    #[tokio::test]
    async fn shared_transport_percent_encodes_reference_query_values() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1_024];
            let count = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..count]);
            assert!(request.starts_with("GET /futures/data/basis?pair=BTC%2FUSDT&period=5m "));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]")
                .unwrap();
        });
        let source = test_source(format!("http://{address}"));
        source
            .get(BASIS_ENDPOINT, &[("pair", "BTC/USDT"), ("period", "5m")])
            .await
            .unwrap();
        server.join().unwrap();
    }

    #[tokio::test]
    async fn shared_transport_does_not_follow_a_redirect_to_another_server() {
        let second_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let second_address = second_listener.local_addr().unwrap();
        second_listener.set_nonblocking(true).unwrap();
        let second = thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_millis(250);
            loop {
                match second_listener.accept() {
                    Ok(_) => return true,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if std::time::Instant::now() >= deadline {
                            return false;
                        }
                        thread::yield_now();
                    }
                    Err(_) => return false,
                }
            }
        });

        let first_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let first_address = first_listener.local_addr().unwrap();
        let first = thread::spawn(move || {
            let (mut stream, _) = first_listener.accept().unwrap();
            let mut request = [0_u8; 1_024];
            let _ = stream.read(&mut request);
            let response = format!(
                "HTTP/1.1 302 Found\r\nLocation: http://{second_address}/redirected\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let source = test_source(format!("http://{first_address}"));
        let error = source.get(SERVER_TIME_ENDPOINT, &[]).await.unwrap_err();
        assert!(matches!(
            error,
            BinanceReferenceError::Http { status: 302, .. }
        ));
        first.join().unwrap();
        assert!(!second.join().unwrap());
    }
}
