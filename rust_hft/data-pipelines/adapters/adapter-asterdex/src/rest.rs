use crate::message_types::DepthSnapshot;
use hft_core::{HftError, HftResult, Symbol};
use integration::http::{HttpClient, HttpClientConfig};
use std::collections::HashMap;

const BASE_URL: &str = "https://fapi.asterdex.com";

pub struct AsterdexRestClient {
    http: HttpClient,
}

impl AsterdexRestClient {
    pub fn new() -> HftResult<Self> {
        let cfg = HttpClientConfig {
            base_url: BASE_URL.to_string(),
            timeout_ms: 5_000,
            user_agent: "hft-asterdex-adapter/0.1".to_string(),
        };

        let http = HttpClient::new(cfg)
            .map_err(|e| HftError::Network(format!("建立 HTTP 客戶端失敗: {}", e)))?;
        Ok(Self { http })
    }

    pub async fn get_depth(&self, symbol: &Symbol, limit: Option<u32>) -> HftResult<DepthSnapshot> {
        let limit = limit.unwrap_or(100);
        let path = format!(
            "/fapi/v1/depth?symbol={}&limit={}",
            symbol,
            limit
        );

        let response = self
            .http
            .get(&path, Option::<HashMap<String, String>>::None)
            .await
            .map_err(|e| HftError::Network(format!("獲取深度失敗: {}", e)))?;

        integration::http::HttpClient::parse_json::<DepthSnapshot>(response)
            .await
            .map_err(|e| HftError::Parse(format!("解析深度快照失敗: {}", e)))
    }
}
