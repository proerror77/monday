//! HTTP 基元 - 低延遲 REST 請求
use reqwest::{Client, Method, Response};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tracing::{error, info};

#[derive(Clone, Debug)]
pub struct HttpClientConfig {
    pub base_url: String,
    pub timeout_ms: u64,
    pub user_agent: String,
}

impl Default for HttpClientConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.bitget.com".to_string(),
            timeout_ms: 5000,
            user_agent: "hft-client/1.0".to_string(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct HttpClient {
    pub cfg: HttpClientConfig,
    client: Client,
}

impl HttpClient {
    pub fn new(cfg: HttpClientConfig) -> Result<Self, reqwest::Error> {
        let client = Client::builder()
            .timeout(Duration::from_millis(cfg.timeout_ms))
            .user_agent(&cfg.user_agent)
            .build()?;

        Ok(Self { cfg, client })
    }

    /// 發送帶簽名的請求
    pub async fn signed_request(
        &self,
        method: Method,
        path: &str,
        headers: Option<HashMap<String, String>>,
        body: Option<String>,
    ) -> Result<Response, reqwest::Error> {
        let url = format!("{}{}", self.cfg.base_url, path);

        let mut request = self.client.request(method.clone(), &url);

        // 添加自定義標頭
        if let Some(headers) = headers {
            for (key, value) in headers {
                request = request.header(&key, &value);
            }
        }

        // 添加請求體
        if let Some(body) = body {
            request = request
                .header("Content-Type", "application/json")
                .body(body);
        }

        info!("HTTP request: {} {}", method, url);

        match request.send().await {
            Ok(response) => {
                info!("HTTP response: {} {}", response.status(), url);
                Ok(response)
            }
            Err(e) => {
                error!("HTTP request failed: {} - {}", url, e);
                Err(e)
            }
        }
    }

    /// GET 請求
    pub async fn get(
        &self,
        path: &str,
        headers: Option<HashMap<String, String>>,
    ) -> Result<Response, reqwest::Error> {
        self.signed_request(Method::GET, path, headers, None).await
    }

    /// POST 請求
    pub async fn post<T: Serialize>(
        &self,
        path: &str,
        headers: Option<HashMap<String, String>>,
        payload: &T,
    ) -> Result<Response, Box<dyn std::error::Error + Send + Sync>> {
        let body = serde_json::to_string(payload)?;
        let response = self
            .signed_request(Method::POST, path, headers, Some(body))
            .await?;
        Ok(response)
    }

    /// 解析 JSON 響應
    pub async fn parse_json<T: for<'de> Deserialize<'de>>(
        response: Response,
    ) -> Result<T, Box<dyn std::error::Error + Send + Sync>> {
        let text = response.text().await?;
        let parsed: T = serde_json::from_str(&text)?;
        Ok(parsed)
    }
}
