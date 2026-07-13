//! Binance REST API 客戶端

use crate::message_types::*;
use hft_core::{HftError, HftResult, Symbol};
use serde::de::DeserializeOwned;
use tracing::{debug, info};

pub const REST_BASE_URL: &str = "https://api.binance.com";

fn classify_http_error(status: u16, retry_after: Option<&str>, body: &str) -> HftError {
    let retry = retry_after
        .filter(|value| !value.is_empty())
        .map(|value| format!("; retry-after={value}s"))
        .unwrap_or_default();
    let message = format!("Binance public REST {status}{retry}: {body}");
    match status {
        418 | 429 => HftError::RateLimit(message),
        400..=499 => HftError::Exchange(message),
        _ => HftError::Network(message),
    }
}

#[derive(Clone)]
pub struct BinanceRestClient {
    client: integration::http::HttpClient,
}

impl Default for BinanceRestClient {
    fn default() -> Self {
        Self::new()
    }
}

impl BinanceRestClient {
    pub fn new() -> Self {
        Self::with_base_url(REST_BASE_URL.to_string())
    }

    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        let config = integration::http::HttpClientConfig {
            base_url: base_url.into(),
            timeout_ms: 10000,
            user_agent: "HFT-Binance-Adapter/1.0".to_string(),
        };
        let client =
            integration::http::HttpClient::new(config).expect("Failed to create HTTP client");

        Self { client }
    }

    /// 獲取訂單簿快照
    pub async fn get_depth(&self, symbol: &Symbol, limit: Option<u16>) -> HftResult<DepthSnapshot> {
        let limit = limit.unwrap_or(100);
        let symbol_str = symbol.to_string();

        let path = format!("/api/v3/depth?symbol={}&limit={}", symbol_str, limit);

        debug!("獲取 {} 深度快照，限制: {}", symbol, limit);

        let response = self
            .client
            .get(&path, None)
            .await
            .map_err(|e| HftError::Network(format!("REST API 請求失敗: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let body = response.text().await.unwrap_or_default();

            return Err(classify_http_error(status, retry_after.as_deref(), &body));
        }

        let text = response
            .text()
            .await
            .map_err(|e| HftError::Network(format!("讀取響應失敗: {}", e)))?;

        let snapshot: DepthSnapshot = Self::parse_json(&text)
            .map_err(|e| HftError::Parse(format!("解析深度快照失敗: {}", e)))?;

        info!(
            "獲取 {} 深度快照成功，序號: {}, 買檔: {}, 賣檔: {}",
            symbol,
            snapshot.last_update_id,
            snapshot.bids.len(),
            snapshot.asks.len()
        );

        Ok(snapshot)
    }

    /// 測試連通性
    pub async fn ping(&self) -> HftResult<()> {
        let path = "/api/v3/ping";

        let response = self
            .client
            .get(path, None)
            .await
            .map_err(|e| HftError::Network(format!("ping 請求失敗: {}", e)))?;

        if response.status().is_success() {
            info!("Binance REST API ping 成功");
            Ok(())
        } else {
            Err(HftError::Network(format!(
                "ping 失敗: {}",
                response.status()
            )))
        }
    }

    /// 使用共用的 feature-gated JSON 解析
    #[inline]
    fn parse_json<T: DeserializeOwned>(text: &str) -> HftResult<T> {
        adapters_common::parse_json(text).map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_response_preserves_retry_after() {
        let error = classify_http_error(
            429,
            Some("60"),
            r#"{"code":-1003,"msg":"Too many requests"}"#,
        );
        assert!(
            matches!(error, HftError::RateLimit(message) if message.contains("retry-after=60s"))
        );
    }

    #[tokio::test]
    #[ignore = "requires live Binance network"]
    async fn test_ping() {
        let client = BinanceRestClient::new();
        let result = client.ping().await;
        assert!(result.is_ok(), "Binance ping 應該成功");
    }

    #[tokio::test]
    #[ignore = "requires live Binance network"]
    async fn test_get_depth() {
        let client = BinanceRestClient::new();
        let symbol = Symbol::new("BTCUSDT");
        let result = client.get_depth(&symbol, Some(10)).await;

        assert!(result.is_ok(), "獲取深度快照應該成功");

        if let Ok(depth) = result {
            assert!(!depth.bids.is_empty(), "買檔應該不為空");
            assert!(!depth.asks.is_empty(), "賣檔應該不為空");
            assert!(depth.last_update_id > 0, "更新 ID 應該大於 0");
        }
    }
}
