//! GRVT 執行適配器（REST + 私有 WS 框架）
//!
//! - 登入：POST {auth_endpoint}/auth/api_key/login，Body: {"api_key": "..."}
//!   - 從回應 header 取得 Set-Cookie: gravity=... 與 x-grvt-account-id
//! - 後續 REST：必須帶 Cookie 與 X-Grvt-Account-Id header
//! - 下單需要 signature；本版先提供框架，回傳需要簽名錯誤，待整合 EIP-712 細節後實作。

use async_trait::async_trait;
use hft_core::{HftError, HftResult, OrderId, Price, Quantity};
use ports::{BoxStream, ConnectionHealth, ExecutionClient, ExecutionEvent, OpenOrder, OrderIntent};
use reqwest::{header::HeaderMap, Client, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use tracing::info;

fn parse_json<T: DeserializeOwned>(text: &str) -> Result<T, HftError> {
    let mut bytes = text.as_bytes().to_vec();
    simd_json::serde::from_slice(bytes.as_mut_slice())
        .map_err(|e| HftError::Serialization(e.to_string()))
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExecutionMode {
    Live,
    Testnet,
}

#[derive(Debug, Clone)]
pub struct GrvtExecutionConfig {
    pub auth_endpoint: Option<String>, // 例如 https://edge.testnet.grvt.io/auth/api_key/login
    pub rest_base_url: String,         // 例如 https://api.testnet.grvt.io 或 team 提供的 base
    pub ws_private_url: Option<String>,
    pub api_key: Option<String>,
    pub timeout_ms: u64,
    pub mode: ExecutionMode,
}

impl Default for GrvtExecutionConfig {
    fn default() -> Self {
        Self {
            auth_endpoint: std::env::var("GRVT_AUTH_ENDPOINT").ok(),
            rest_base_url: std::env::var("GRVT_REST")
                .unwrap_or_else(|_| "https://api.testnet.grvt.io".to_string()),
            ws_private_url: std::env::var("GRVT_WS_PRIVATE").ok(),
            api_key: std::env::var("GRVT_API_KEY").ok(),
            timeout_ms: 5000,
            mode: ExecutionMode::Testnet,
        }
    }
}

#[derive(Clone)]
pub struct GrvtExecutionClient {
    cfg: GrvtExecutionConfig,
    http: Client,
    state: Arc<Mutex<AuthState>>, // 持有 cookie 與 account_id
    _exec_tx: mpsc::UnboundedSender<HftResult<ExecutionEvent>>, // 供回報流使用（WS 尚未接）
}

#[derive(Debug, Default, Clone)]
struct AuthState {
    cookie: Option<String>,
    account_id: Option<String>,
}

impl GrvtExecutionClient {
    pub fn new(cfg: GrvtExecutionConfig) -> Self {
        let http = Client::builder()
            .use_rustls_tls()
            .cookie_store(true)
            .build()
            .expect("build reqwest client");
        let (tx, _rx) = mpsc::unbounded_channel();
        Self {
            cfg,
            http,
            state: Arc::new(Mutex::new(AuthState::default())),
            _exec_tx: tx,
        }
    }

    async fn login(&self) -> HftResult<()> {
        // 若已有 cookie，略過
        {
            let st = self.state.lock().await;
            if st.cookie.is_some() && st.account_id.is_some() {
                return Ok(());
            }
        }

        // 優先使用環境變量直接注入（CI/密鑰代理）
        if let (Ok(cookie), Ok(acc)) = (
            std::env::var("GRVT_COOKIE"),
            std::env::var("GRVT_ACCOUNT_ID"),
        ) {
            let mut st = self.state.lock().await;
            st.cookie = Some(cookie);
            st.account_id = Some(acc);
            info!("GRVT: 使用環境變量注入的 Cookie/AccountId");
            return Ok(());
        }

        let endpoint = if let Some(ep) = &self.cfg.auth_endpoint {
            ep.clone()
        } else {
            return Err(HftError::new(
                "GRVT: 未提供 auth_endpoint 或 GRVT_COOKIE/GRVT_ACCOUNT_ID",
            ));
        };
        let api_key = if let Some(k) = &self.cfg.api_key {
            k.clone()
        } else {
            return Err(HftError::new(
                "GRVT: 未提供 API Key (cfg.api_key 或 GRVT_API_KEY)",
            ));
        };

        let body = serde_json::json!({"api_key": api_key});
        let resp = self
            .http
            .post(&endpoint)
            .header("Content-Type", "application/json")
            .header("Cookie", "rm=true;")
            .json(&body)
            .send()
            .await
            .map_err(|e| HftError::Network(e.to_string()))?;
        let status = resp.status();
        let headers = resp.headers().clone();
        let _ = resp.text().await; // 釋放連線（不解析 body）
        if status != StatusCode::OK && status != StatusCode::ACCEPTED {
            return Err(HftError::Network(format!("GRVT auth HTTP {}", status)));
        }
        let (cookie, acc) = extract_cookie_and_account_id(&headers)?;
        let mut st = self.state.lock().await;
        st.cookie = Some(cookie);
        st.account_id = Some(acc);
        info!("GRVT: 登入成功，已保存 Cookie 與 AccountId");
        Ok(())
    }

    fn auth_headers(&self, st: &AuthState) -> HftResult<HeaderMap> {
        let mut hm = HeaderMap::new();
        let cookie = st
            .cookie
            .as_ref()
            .ok_or_else(|| HftError::new("GRVT: 尚未登入（缺少 Cookie）"))?;
        let acc = st
            .account_id
            .as_ref()
            .ok_or_else(|| HftError::new("GRVT: 尚未登入（缺少 AccountId）"))?;
        let cookie_val = reqwest::header::HeaderValue::from_str(cookie.as_str())
            .map_err(|e| HftError::new(&e.to_string()))?;
        let acc_val = reqwest::header::HeaderValue::from_str(acc.as_str())
            .map_err(|e| HftError::new(&e.to_string()))?;
        hm.insert("Cookie", cookie_val);
        hm.insert("X-Grvt-Account-Id", acc_val);
        Ok(hm)
    }
}

fn extract_cookie_and_account_id(headers: &HeaderMap) -> HftResult<(String, String)> {
    // Set-Cookie: gravity=...
    let mut cookie_val: Option<String> = None;
    for (k, v) in headers.iter() {
        if k.as_str().eq_ignore_ascii_case("set-cookie") {
            if let Ok(s) = v.to_str() {
                if let Some(idx) = s.find("gravity=") {
                    // 擷取到 ; 結束
                    let rest = &s[idx..];
                    let cookie = rest.split(';').next().unwrap_or(rest).to_string();
                    cookie_val = Some(cookie);
                    break;
                }
            }
        }
    }
    let acc = headers
        .get("x-grvt-account-id")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| HftError::new("GRVT: 登入回應缺少 x-grvt-account-id"))?
        .to_string();
    let cookie = cookie_val.ok_or_else(|| HftError::new("GRVT: 登入回應缺少 gravity cookie"))?;
    Ok((cookie, acc))
}

#[async_trait]
impl ExecutionClient for GrvtExecutionClient {
    async fn place_order(&mut self, _intent: OrderIntent) -> HftResult<OrderId> {
        Err(HftError::new("GRVT place_order 尚未實作：需要 EIP-712 簽名細節（signature/nonce/expiration）。目前請使用外部簽名後的 REST 流程，或提供簽名規範以整合。"))
    }

    async fn cancel_order(&mut self, _order_id: &OrderId) -> HftResult<()> {
        Err(HftError::new(
            "GRVT cancel_order 尚未實作（等待完整回應欄位與簽名/規則）",
        ))
    }

    async fn modify_order(
        &mut self,
        _order_id: &OrderId,
        _new_quantity: Option<Quantity>,
        _new_price: Option<Price>,
    ) -> HftResult<()> {
        Err(HftError::new("GRVT modify_order 尚未實作"))
    }

    async fn execution_stream(&self) -> HftResult<BoxStream<ExecutionEvent>> {
        // 私有 WS 回報需 Cookie + X-Grvt-Account-Id；未提供明確 stream 名稱，此處先回傳空流
        let (_tx, rx) = mpsc::unbounded_channel::<HftResult<ExecutionEvent>>();
        let stream = tokio_stream::wrappers::UnboundedReceiverStream::new(rx);
        Ok(Box::pin(stream))
    }

    async fn list_open_orders(&self) -> HftResult<Vec<OpenOrder>> {
        // 先完成登入以獲取 Cookie/header
        self.login().await?;
        let st = self.state.lock().await.clone();
        let headers = self.auth_headers(&st)?;

        let url = format!(
            "{}/full/v1/open_orders",
            self.cfg.rest_base_url.trim_end_matches('/')
        );
        let body = serde_json::json!({
            "sub_account_id": st.account_id.as_deref().unwrap_or("")
        });
        let resp = self
            .http
            .post(&url)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|e| HftError::Network(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(HftError::Network(format!(
                "GRVT open_orders HTTP {}",
                resp.status()
            )));
        }
        let txt = resp
            .text()
            .await
            .map_err(|e| HftError::Network(e.to_string()))?;
        // 由於回應 schema 未提供，暫時不嘗試映射，返回空集合，避免 schema 演進破壞
        let _raw: Value = parse_json(&txt).unwrap_or(Value::Null);
        Ok(Vec::new())
    }

    async fn connect(&mut self) -> HftResult<()> {
        self.login().await
    }

    async fn disconnect(&mut self) -> HftResult<()> {
        Ok(())
    }

    async fn health(&self) -> ConnectionHealth {
        ConnectionHealth {
            connected: true,
            latency_ms: None,
            last_heartbeat: 0,
        }
    }
}
