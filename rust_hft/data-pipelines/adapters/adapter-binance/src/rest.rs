//! Binance REST API 客戶端

use crate::message_types::*;
use hft_core::{HftError, HftResult, Symbol};
use serde::de::DeserializeOwned;
use serde_json;
use simd_json;
use std::collections::HashMap;
use tracing::{debug, info};

const REST_BASE_URL: &str = "https://api.binance.com";

pub struct BinanceRestClient {
    client: reqwest::Client,
    base_url: String,
}

impl BinanceRestClient {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .user_agent("HFT-Binance-Adapter/1.0")
            .build()
            .expect("Failed to create HTTP client");

        Self {
            client,
            base_url: REST_BASE_URL.to_string(),
        }
    }

    /// 獲取訂單簿快照
    pub async fn get_depth(&self, symbol: &Symbol, limit: Option<u16>) -> HftResult<DepthSnapshot> {
        let limit = limit.unwrap_or(100);
        let symbol_str = symbol.to_string();

        let url = format!("{}/api/v3/depth", self.base_url);
        let limit_str = limit.to_string();
        let mut params = HashMap::new();
        params.insert("symbol", symbol_str.as_str());
        params.insert("limit", limit_str.as_str());

        debug!("獲取 {} 深度快照，限制: {}", symbol, limit);

        let response = self
            .client
            .get(&url)
            .query(&params)
            .send()
            .await
            .map_err(|e| HftError::Network(format!("REST API 請求失敗: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();

            // 嘗試解析錯誤響應
            if let Ok(error) = Self::parse_json::<BinanceError>(&body) {
                return Err(HftError::Exchange(format!(
                    "Binance API 錯誤 {}: {} ({})",
                    status, error.msg, error.code
                )));
            }

            return Err(HftError::Network(format!(
                "API 請求失敗 {}: {}",
                status, body
            )));
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
        let url = format!("{}/api/v3/ping", self.base_url);

        let response = self
            .client
            .get(&url)
            .send()
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

    /// 獲取服務器時間
    pub async fn get_server_time(&self) -> HftResult<u64> {
        let url = format!("{}/api/v3/time", self.base_url);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| HftError::Network(format!("獲取服務器時間失敗: {}", e)))?;

        if !response.status().is_success() {
            return Err(HftError::Network(format!(
                "獲取時間失敗: {}",
                response.status()
            )));
        }

        let text = response
            .text()
            .await
            .map_err(|e| HftError::Network(format!("讀取時間響應失敗: {}", e)))?;

        let json: serde_json::Value = Self::parse_json(&text)
            .map_err(|e| HftError::Parse(format!("解析時間響應失敗: {}", e)))?;

        let server_time = json["serverTime"]
            .as_u64()
            .ok_or_else(|| HftError::Parse("無效的服務器時間格式".to_string()))?;

        Ok(server_time)
    }

    /// 獲取交易對信息
    pub async fn get_exchange_info(&self) -> HftResult<serde_json::Value> {
        let url = format!("{}/api/v3/exchangeInfo", self.base_url);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| HftError::Network(format!("獲取交易所信息失敗: {}", e)))?;

        if !response.status().is_success() {
            return Err(HftError::Network(format!(
                "獲取交易所信息失敗: {}",
                response.status()
            )));
        }

        let text = response
            .text()
            .await
            .map_err(|e| HftError::Network(format!("讀取交易所信息響應失敗: {}", e)))?;

        let json: serde_json::Value = Self::parse_json(&text)
            .map_err(|e| HftError::Parse(format!("解析交易所信息失敗: {}", e)))?;

        Ok(json)
    }

    #[inline]
    fn parse_json<T: DeserializeOwned>(text: &str) -> Result<T, simd_json::Error> {
        let mut bytes = text.as_bytes().to_vec();
        simd_json::serde::from_slice(bytes.as_mut_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_ping() {
        let client = BinanceRestClient::new();
        let result = client.ping().await;
        assert!(result.is_ok(), "Binance ping 應該成功");
    }

    #[tokio::test]
    async fn test_get_server_time() {
        let client = BinanceRestClient::new();
        let result = client.get_server_time().await;
        assert!(result.is_ok(), "獲取服務器時間應該成功");

        if let Ok(time) = result {
            assert!(time > 0, "服務器時間應該大於 0");
        }
    }

    #[tokio::test]
    async fn test_get_depth() {
        let client = BinanceRestClient::new();
        let symbol = Symbol("BTCUSDT".to_string());
        let result = client.get_depth(&symbol, Some(10)).await;

        assert!(result.is_ok(), "獲取深度快照應該成功");

        if let Ok(depth) = result {
            assert!(!depth.bids.is_empty(), "買檔應該不為空");
            assert!(!depth.asks.is_empty(), "賣檔應該不為空");
            assert!(depth.last_update_id > 0, "更新 ID 應該大於 0");
        }
    }
}
