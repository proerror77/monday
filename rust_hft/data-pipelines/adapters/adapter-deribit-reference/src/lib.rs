//! Deribit public reference transport.
//!
//! This adapter owns only the public REST origin, request construction, JSON-RPC
//! validation, raw response receipt clocks, and delegation to hft-data typed
//! Deribit observations. It never creates MarketEvent trade/quote events.

use data::deribit_reference::{
    parse_greeks_result, parse_iv_summary_row, DeribitGreeksObservation, DeribitIvObservation,
};
use reqwest::{Client, StatusCode};
use serde_json::Value;
use std::time::Duration;
use thiserror::Error;
use url::Url;

const PROD_ORIGIN: &str = "https://www.deribit.com";
#[cfg(test)]
const TEST_ORIGIN: &str = "https://test.deribit.com";
const IV_PATH: &str = "/api/v2/public/get_book_summary_by_currency";
const GREEKS_PATH: &str = "/api/v2/public/get_order_book";

#[derive(Debug, Error)]
pub enum DeribitReferenceError {
    #[error("Deribit reference origin is not official: {0}")]
    UnsupportedOrigin(String),
    #[error("build Deribit reference client: {0}")]
    Client(String),
    #[error("Deribit request {path} failed: {message}")]
    Request { path: String, message: String },
    #[error("Deribit request {path} returned HTTP {status}: {body}")]
    Http {
        path: String,
        status: StatusCode,
        body: String,
    },
    #[error("Deribit response {path} is invalid JSON: {message}")]
    Json { path: String, message: String },
    #[error("Deribit JSON-RPC response {path} has an error: {error}")]
    Rpc { path: String, error: String },
    #[error("Deribit source response has no result: {path}")]
    MissingResult { path: String },
    #[error(transparent)]
    Typed(#[from] anyhow::Error),
}

pub type DeribitReferenceResult<T> = Result<T, DeribitReferenceError>;

#[derive(Debug, Clone)]
pub struct TimedDeribitIvBatch {
    pub observations: Vec<DeribitIvObservation>,
    pub received_at_us: u64,
    pub raw: Value,
}

#[derive(Debug, Clone)]
pub struct TimedDeribitGreeks {
    pub observation: DeribitGreeksObservation,
    pub received_at_us: u64,
    pub raw: Value,
}

#[derive(Debug, Clone)]
pub struct DeribitReferenceSource {
    client: Client,
    origin: Url,
}

impl DeribitReferenceSource {
    pub fn new(origin: &str, timeout: Duration) -> DeribitReferenceResult<Self> {
        validate_origin(origin)?;
        let parsed = Url::parse(origin.trim_end_matches('/'))
            .map_err(|error| DeribitReferenceError::Client(error.to_string()))?;
        Self::build(parsed, timeout)
    }

    pub fn production(timeout: Duration) -> DeribitReferenceResult<Self> {
        Self::new(PROD_ORIGIN, timeout)
    }

    fn build(origin: Url, timeout: Duration) -> DeribitReferenceResult<Self> {
        let client = Client::builder()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("monday-deribit-reference/1.0")
            .build()
            .map_err(|error| DeribitReferenceError::Client(error.to_string()))?;
        Ok(Self { client, origin })
    }

    #[cfg(test)]
    fn for_test(origin: &str) -> Self {
        Self::build(Url::parse(origin).unwrap(), Duration::from_secs(2)).unwrap()
    }

    async fn get_json(
        &self,
        path: &'static str,
        query: &[(&str, &str)],
    ) -> DeribitReferenceResult<(Value, u64)> {
        let mut url = self.origin.clone();
        url.set_path(path);
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
                .map_err(|error| DeribitReferenceError::Request {
                    path: path.to_owned(),
                    message: error.to_string(),
                })?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|error| DeribitReferenceError::Request {
                path: path.to_owned(),
                message: error.to_string(),
            })?;
        let received_at_us = hft_core::now_micros();
        if !status.is_success() {
            return Err(DeribitReferenceError::Http {
                path: path.to_owned(),
                status,
                body: String::from_utf8_lossy(&body).into_owned(),
            });
        }
        let value: Value =
            serde_json::from_slice(&body).map_err(|error| DeribitReferenceError::Json {
                path: path.to_owned(),
                message: error.to_string(),
            })?;
        if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err(DeribitReferenceError::Json {
                path: path.to_owned(),
                message: "jsonrpc must be 2.0".to_owned(),
            });
        }
        if let Some(error) = value.get("error") {
            return Err(DeribitReferenceError::Rpc {
                path: path.to_owned(),
                error: error.to_string(),
            });
        }
        if !value.get("result").is_some_and(|result| !result.is_null()) {
            return Err(DeribitReferenceError::MissingResult {
                path: path.to_owned(),
            });
        }
        if received_at_us == 0 {
            return Err(DeribitReferenceError::Typed(anyhow::anyhow!(
                "Deribit receive clock is zero"
            )));
        }
        Ok((value, received_at_us))
    }

    pub async fn option_iv(&self, currency: &str) -> DeribitReferenceResult<TimedDeribitIvBatch> {
        let currency = currency.trim().to_ascii_uppercase();
        if currency.is_empty() {
            return Err(DeribitReferenceError::Typed(anyhow::anyhow!(
                "Deribit currency is empty"
            )));
        }
        let (raw, received_at_us) = self
            .get_json(IV_PATH, &[("currency", &currency), ("kind", "option")])
            .await?;
        let rows = raw.get("result").and_then(Value::as_array).ok_or_else(|| {
            DeribitReferenceError::MissingResult {
                path: IV_PATH.to_owned(),
            }
        })?;
        let observations = rows
            .iter()
            .map(|row| parse_iv_summary_row(row, &currency, received_at_us))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(TimedDeribitIvBatch {
            observations,
            received_at_us,
            raw,
        })
    }

    pub async fn option_greeks(
        &self,
        currency: &str,
        instrument_name: &str,
    ) -> DeribitReferenceResult<TimedDeribitGreeks> {
        let (raw, received_at_us) = self
            .get_json(
                GREEKS_PATH,
                &[("instrument_name", instrument_name), ("depth", "1")],
            )
            .await?;
        let result = raw
            .get("result")
            .ok_or_else(|| DeribitReferenceError::MissingResult {
                path: GREEKS_PATH.to_owned(),
            })?;
        let observation = parse_greeks_result(result, currency, instrument_name, received_at_us)?;
        Ok(TimedDeribitGreeks {
            observation,
            received_at_us,
            raw,
        })
    }
}

fn validate_origin(origin: &str) -> DeribitReferenceResult<()> {
    let parsed = Url::parse(origin)
        .map_err(|_| DeribitReferenceError::UnsupportedOrigin(origin.to_owned()))?;
    let origin_only = parsed.scheme() == "https"
        && parsed.port_or_known_default() == Some(443)
        && matches!(parsed.path(), "" | "/")
        && parsed.query().is_none()
        && parsed.fragment().is_none()
        && parsed.username().is_empty()
        && parsed.password().is_none();
    if !origin_only
        || !matches!(
            parsed.host_str(),
            Some("www.deribit.com") | Some("test.deribit.com")
        )
    {
        return Err(DeribitReferenceError::UnsupportedOrigin(origin.to_owned()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    fn response(value: Value) -> Vec<u8> {
        let body = value.to_string();
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    #[test]
    fn only_official_origins_are_allowed_without_path_or_redirect_surface() {
        assert!(DeribitReferenceSource::new(PROD_ORIGIN, Duration::from_secs(1)).is_ok());
        assert!(DeribitReferenceSource::new(TEST_ORIGIN, Duration::from_secs(1)).is_ok());
        for origin in [
            "http://www.deribit.com",
            "https://evil.example",
            "https://www.deribit.com/path",
            "https://www.deribit.com?x=1",
            "https://user:pass@www.deribit.com",
        ] {
            assert!(DeribitReferenceSource::new(origin, Duration::from_secs(1)).is_err());
        }
    }

    #[tokio::test]
    async fn transport_preserves_raw_json_and_actual_receipt_clock() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            stream
                .write_all(&response(json!({
                    "jsonrpc":"2.0",
                    "id":1,
                    "result": [{
                        "instrument_name":"BTC-6SEP24-50000-C",
                        "creation_timestamp":1700000000000_i64,
                        "mark_iv":77.72,
                        "unknown":"preserved"
                    }]
                })))
                .unwrap();
        });
        let source = DeribitReferenceSource::for_test(&format!("http://{address}"));
        let batch = source.option_iv("BTC").await.unwrap();
        assert!(batch.received_at_us > 0);
        assert_eq!(batch.raw["result"][0]["unknown"], "preserved");
        assert_eq!(batch.observations.len(), 1);
        assert_eq!(batch.observations[0].identity.creation_timestamp_ms, None);
        assert_eq!(
            batch.observations[0].source_timestamp_ms,
            Some(1700000000000_i64)
        );
        assert_eq!(batch.observations[0].mark_iv.unwrap().to_string(), "0.7772");
        assert_eq!(batch.observations[0].source_iv_unit, "percent_points");
        assert_eq!(batch.observations[0].iv_unit, "decimal_fraction");
        server.join().unwrap();
    }

    #[tokio::test]
    async fn order_book_fixture_uses_timestamp_and_does_not_require_creation_metadata() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            stream
                .write_all(&response(json!({
                    "jsonrpc":"2.0",
                    "id":1,
                    "result": {
                        "instrument_name":"BTC-6SEP24-50000-C",
                        "timestamp":1700000002000_i64,
                        "mark_iv":77.72,
                        "greeks":{"delta":0.5,"gamma":null}
                    }
                })))
                .unwrap();
        });
        let source = DeribitReferenceSource::for_test(&format!("http://{address}"));
        let response = source
            .option_greeks("BTC", "BTC-6SEP24-50000-C")
            .await
            .unwrap();
        assert_eq!(response.observation.identity.creation_timestamp_ms, None);
        assert_eq!(response.observation.source_timestamp_ms, 1700000002000_i64);
        assert_eq!(response.observation.delta.unwrap().to_string(), "0.5");
        assert_eq!(response.observation.gamma, None);
        server.join().unwrap();
    }
}
