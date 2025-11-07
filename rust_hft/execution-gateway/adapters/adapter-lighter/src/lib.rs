//! Lighter 執行適配器（REST 送單 + FFI 簽名，可選）
//! 注意：目前 Live 僅實作下單（place_order）。撤單/改單/查單因需要額外授權與索引，暫標記為未實作。

use async_trait::async_trait;
use hft_core::{HftError, HftResult, OrderId, Price, Quantity};
use integration::http::{HttpClient, HttpClientConfig};
use ports::{BoxStream, ExecutionClient, ExecutionEvent, OpenOrder};
use tokio::sync::broadcast;
use tokio_stream::{wrappers::BroadcastStream, StreamExt};
use tracing::info;
use urlencoding::encode;

use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_longlong};

#[derive(Debug, Clone, PartialEq)]
pub enum ExecutionMode {
    Live,
    Paper,
}

#[derive(Debug, Clone)]
pub struct LighterExecutionConfig {
    pub rest_base_url: String,
    pub timeout_ms: u64,
    pub mode: ExecutionMode,
    // 簽名庫（可選）：指定 .so/.dylib 路徑以啟用 Live 下單
    pub signer_lib_path: Option<String>,
    pub api_key_private_key: Option<String>,
    pub api_key_index: Option<i32>,
    pub account_index: Option<i64>,
}

#[repr(C)]
struct StrOrErr {
    s: *const c_char,
    err: *const c_char,
}

struct FfiSigner {
    _lib: libloading::Library,
    create_client: unsafe extern "C" fn(
        *const c_char,
        *const c_char,
        c_int,
        c_int,
        c_longlong,
    ) -> *const c_char,
    switch_api_key: unsafe extern "C" fn(c_int) -> *const c_char,
    sign_create_order: unsafe extern "C" fn(
        c_int,
        c_longlong,
        c_longlong,
        c_int,
        c_int,
        c_int,
        c_int,
        c_int,
        c_int,
        c_longlong,
        c_longlong,
    ) -> StrOrErr,
    sign_cancel_order: unsafe extern "C" fn(c_int, c_longlong, c_longlong) -> StrOrErr,
    sign_modify_order: unsafe extern "C" fn(
        c_int,
        c_longlong,
        c_longlong,
        c_longlong,
        c_longlong,
        c_longlong,
    ) -> StrOrErr,
    create_auth_token: unsafe extern "C" fn(c_longlong) -> StrOrErr,
}

impl FfiSigner {
    unsafe fn load(lib_path: &str) -> HftResult<Self> {
        let lib = libloading::Library::new(lib_path)
            .map_err(|e| HftError::Config(format!("無法載入 signer 庫: {}", e)))?;
        let create_client = *lib
            .get::<unsafe extern "C" fn(
                *const c_char,
                *const c_char,
                c_int,
                c_int,
                c_longlong,
            ) -> *const c_char>(b"CreateClient\0")
            .map_err(|e| HftError::Config(format!("缺少 CreateClient: {}", e)))?;
        let switch_api_key = *lib
            .get::<unsafe extern "C" fn(c_int) -> *const c_char>(b"SwitchAPIKey\0")
            .map_err(|e| HftError::Config(format!("缺少 SwitchAPIKey: {}", e)))?;
        let sign_create_order = *lib
            .get::<unsafe extern "C" fn(
                c_int,
                c_longlong,
                c_longlong,
                c_int,
                c_int,
                c_int,
                c_int,
                c_int,
                c_int,
                c_longlong,
                c_longlong,
            ) -> StrOrErr>(b"SignCreateOrder\0")
            .map_err(|e| HftError::Config(format!("缺少 SignCreateOrder: {}", e)))?;
        let sign_cancel_order = *lib
            .get::<unsafe extern "C" fn(c_int, c_longlong, c_longlong) -> StrOrErr>(
                b"SignCancelOrder\0",
            )
            .map_err(|e| HftError::Config(format!("缺少 SignCancelOrder: {}", e)))?;
        let sign_modify_order = *lib
            .get::<unsafe extern "C" fn(
                c_int,
                c_longlong,
                c_longlong,
                c_longlong,
                c_longlong,
                c_longlong,
            ) -> StrOrErr>(b"SignModifyOrder\0")
            .map_err(|e| HftError::Config(format!("缺少 SignModifyOrder: {}", e)))?;
        let create_auth_token = *lib
            .get::<unsafe extern "C" fn(c_longlong) -> StrOrErr>(b"CreateAuthToken\0")
            .map_err(|e| HftError::Config(format!("缺少 CreateAuthToken: {}", e)))?;
        Ok(Self {
            _lib: lib,
            create_client,
            switch_api_key,
            sign_create_order,
            sign_cancel_order,
            sign_modify_order,
            create_auth_token,
        })
    }

    unsafe fn cstr_to_string(ptr: *const c_char) -> Option<String> {
        if ptr.is_null() {
            None
        } else {
            CStr::from_ptr(ptr).to_str().ok().map(|s| s.to_string())
        }
    }
}

pub struct LighterExecutionClient {
    cfg: LighterExecutionConfig,
    http: HttpClient,
    event_tx: Option<broadcast::Sender<ExecutionEvent>>,
    connected: bool,
    signer: Option<FfiSigner>,
    // 符號映射與精度
    market_meta: HashMap<String, (i64, i32, i32)>, // symbol -> (market_id, size_decimals, price_decimals)
    // tx_hash -> (symbol, market_id, client_order_index)
    order_map: HashMap<String, (String, i64, i64)>,
}

impl LighterExecutionClient {
    pub fn new(cfg: LighterExecutionConfig) -> HftResult<Self> {
        let http = HttpClient::new(HttpClientConfig {
            base_url: cfg.rest_base_url.clone(),
            timeout_ms: cfg.timeout_ms,
            user_agent: "hft-lighter-exec/1.0".to_string(),
        })
        .map_err(|e| HftError::Network(e.to_string()))?;

        Ok(Self {
            cfg,
            http,
            event_tx: None,
            connected: false,
            signer: None,
            market_meta: HashMap::new(),
            order_map: HashMap::new(),
        })
    }

    async fn ensure_market_meta(&mut self, symbol: &str) -> HftResult<(i64, i32, i32)> {
        if let Some(v) = self.market_meta.get(symbol) {
            return Ok(*v);
        }
        // GET /api/v1/orderBooks
        let resp = self
            .http
            .signed_request(reqwest::Method::GET, "/api/v1/orderBooks", None, None)
            .await
            .map_err(|e| HftError::Network(e.to_string()))?;
        let v: serde_json::Value = HttpClient::parse_json(resp)
            .await
            .map_err(|e| HftError::Serialization(e.to_string()))?;
        let mut found = None;
        if let Some(arr) = v.get("order_books").and_then(|x| x.as_array()) {
            for it in arr {
                let sym_ok = it.get("symbol").and_then(|x| x.as_str()).unwrap_or("") == symbol;
                if !sym_ok {
                    continue;
                }
                let market_id = it.get("market_id").and_then(|x| x.as_i64()).unwrap_or(0);
                let size_dec = it
                    .get("supported_size_decimals")
                    .and_then(|x| x.as_i64())
                    .unwrap_or(0) as i32;
                let price_dec = it
                    .get("supported_price_decimals")
                    .and_then(|x| x.as_i64())
                    .unwrap_or(0) as i32;
                found = Some((market_id, size_dec, price_dec));
                break;
            }
        }
        if let Some(m) = found {
            self.market_meta.insert(symbol.to_string(), m);
            Ok(m)
        } else {
            Err(HftError::Config(format!(
                "Lighter: 無法找到符號 {} 的 market_id",
                symbol
            )))
        }
    }

    async fn next_nonce(&self, account_index: i64, api_key_index: i32) -> HftResult<i64> {
        let path = format!(
            "/api/v1/nextNonce?account_index={}&api_key_index={}",
            account_index, api_key_index
        );
        let resp = self
            .http
            .signed_request(reqwest::Method::GET, &path, None, None)
            .await
            .map_err(|e| HftError::Network(e.to_string()))?;
        let v: serde_json::Value = HttpClient::parse_json(resp)
            .await
            .map_err(|e| HftError::Serialization(e.to_string()))?;
        if let Some(n) = v.get("next_nonce").and_then(|x| x.as_i64()) {
            Ok(n)
        } else {
            Err(HftError::Exchange(format!("無法獲取 nextNonce: {}", v)))
        }
    }

    fn scale_qty(q: Quantity, size_decimals: i32) -> i64 {
        let f = q.to_f64().unwrap_or(0.0);
        let scale = 10_i64.pow(size_decimals as u32) as f64;
        (f * scale).round() as i64
    }
    fn scale_price(p: Option<Price>, price_decimals: i32) -> i32 {
        let f = p.and_then(|x| x.to_f64()).unwrap_or(0.0);
        let scale = 10_i64.pow(price_decimals as u32) as f64;
        ((f * scale).round() as i64) as i32
    }

    async fn send_tx(&self, tx_type: i32, tx_info: &str) -> HftResult<String> {
        // POST /api/v1/sendTx with form/json params
        let url = format!("{}/api/v1/sendTx", self.http.cfg.base_url);
        let client = reqwest::Client::new();
        let body = serde_json::json!({
            "tx_type": tx_type,
            "tx_info": tx_info,
        });
        let resp = client
            .post(url)
            .json(&body)
            .send()
            .await
            .map_err(|e| HftError::Network(e.to_string()))?;
        let status = resp.status();
        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| HftError::Serialization(e.to_string()))?;
        if !status.is_success() {
            return Err(HftError::Exchange(format!("sendTx HTTP {}: {}", status, v)));
        }
        let code = v.get("code").and_then(|x| x.as_i64()).unwrap_or_default();
        if code != 200 {
            return Err(HftError::Exchange(format!(
                "sendTx ret code {}: {}",
                code, v
            )));
        }
        let txh = v
            .get("tx_hash")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        Ok(txh)
    }

    async fn create_auth_token(&self, deadline_secs: i64) -> HftResult<String> {
        let signer = self
            .signer
            .as_ref()
            .ok_or_else(|| HftError::Config("未載入 signer".into()))?;
        let ret = unsafe { (signer.create_auth_token)(deadline_secs as c_longlong) };
        if let Some(e) = unsafe { FfiSigner::cstr_to_string(ret.err) } {
            return Err(HftError::Exchange(format!("CreateAuthToken 失敗: {}", e)));
        }
        unsafe { FfiSigner::cstr_to_string(ret.s) }
            .ok_or_else(|| HftError::Exchange("empty auth token".into()))
    }

    async fn list_active_orders(
        &self,
        account_index: i64,
        market_id: i64,
        auth: &str,
    ) -> HftResult<serde_json::Value> {
        let path = format!(
            "/api/v1/accountActiveOrders?account_index={}&market_id={}&auth={}",
            account_index,
            market_id,
            encode(auth)
        );
        let resp = self
            .http
            .signed_request(reqwest::Method::GET, &path, None, None)
            .await
            .map_err(|e| HftError::Network(e.to_string()))?;
        let v: serde_json::Value = HttpClient::parse_json(resp)
            .await
            .map_err(|e| HftError::Serialization(e.to_string()))?;
        Ok(v)
    }
}

#[async_trait]
impl ExecutionClient for LighterExecutionClient {
    async fn place_order(&mut self, intent: ports::OrderIntent) -> HftResult<OrderId> {
        match self.cfg.mode {
            ExecutionMode::Paper => {
                let oid = OrderId(format!("LIGHTER_PAPER_{}", hft_core::now_micros()));
                if let Some(ref tx) = self.event_tx {
                    let _ = tx.send(ExecutionEvent::OrderAck {
                        order_id: oid.clone(),
                        timestamp: hft_core::now_micros(),
                    });
                    if let Some(p) = intent.price {
                        let _ = tx.send(ExecutionEvent::Fill {
                            order_id: oid.clone(),
                            price: p,
                            quantity: intent.quantity,
                            timestamp: hft_core::now_micros(),
                            fill_id: format!("LGFILL-{}", hft_core::now_micros()),
                        });
                    }
                }
                return Ok(oid);
            }
            ExecutionMode::Live => {}
        }

        if self.signer.is_none() {
            return Err(HftError::Config(
                "未載入 Lighter signer 庫或未 connect()".to_string(),
            ));
        }
        let api_key_index = self
            .cfg
            .api_key_index
            .ok_or_else(|| HftError::Config("缺少 api_key_index".into()))?;
        let account_index = self
            .cfg
            .account_index
            .ok_or_else(|| HftError::Config("缺少 account_index".into()))?;

        let (market_id, size_dec, price_dec) =
            self.ensure_market_meta(intent.symbol.as_str()).await?;

        // 取得 nonce
        let nonce = self.next_nonce(account_index, api_key_index).await?;
        let signer = self.signer.as_ref().unwrap();

        // 映射欄位
        let base_amount = Self::scale_qty(intent.quantity, size_dec);
        let price_i32 = Self::scale_price(intent.price, price_dec);
        let is_ask = match intent.side {
            hft_core::Side::Buy => 0,
            hft_core::Side::Sell => 1,
        };
        let order_type = match intent.order_type {
            hft_core::OrderType::Limit => 0,
            hft_core::OrderType::Market => 1,
        };
        let tif = match intent.time_in_force {
            hft_core::TimeInForce::IOC => 0,
            hft_core::TimeInForce::GTC => 1,
            hft_core::TimeInForce::FOK => 0,
        };
        let reduce_only = 0;
        let trigger_price = 0; // 非觸發單
        let order_expiry: i64 = if tif == 0 { 0 } else { -1 }; // IOC=0、其他 28天

        // 生成客戶端訂單索引（任意唯一即可）
        let client_order_index = hft_core::now_micros() as i64;

        // 調用 signer 產生 tx_info（JSON 字串）
        let tx_info = unsafe {
            let ret = (signer.sign_create_order)(
                market_id as c_int,
                client_order_index as c_longlong,
                base_amount as c_longlong,
                price_i32 as c_int,
                is_ask as c_int,
                order_type as c_int,
                tif as c_int,
                reduce_only as c_int,
                trigger_price as c_int,
                order_expiry as c_longlong,
                nonce as c_longlong,
            );
            if !ret.err.is_null() {
                if let Some(e) = FfiSigner::cstr_to_string(ret.err) {
                    return Err(HftError::Exchange(format!("sign error: {}", e)));
                }
            }
            FfiSigner::cstr_to_string(ret.s)
                .ok_or_else(|| HftError::Exchange("empty sign result".into()))?
        };

        // 送出交易
        let tx_hash = self.send_tx(14, &tx_info).await?; // 14 = CREATE_ORDER

        let oid = OrderId(tx_hash.clone());
        // record mapping for later cancel/modify
        self.order_map.insert(
            tx_hash.clone(),
            (
                intent.symbol.as_str().to_string(),
                market_id,
                client_order_index,
            ),
        );
        if let Some(ref tx) = self.event_tx {
            let _ = tx.send(ExecutionEvent::OrderAck {
                order_id: oid.clone(),
                timestamp: hft_core::now_micros(),
            });
        }
        Ok(oid)
    }

    async fn cancel_order(&mut self, order_id: &OrderId) -> HftResult<()> {
        match self.cfg.mode {
            ExecutionMode::Paper => return Ok(()),
            ExecutionMode::Live => {}
        }
        let (symbol, market_id, client_order_index) = self
            .order_map
            .get(&order_id.0)
            .cloned()
            .ok_or_else(|| HftError::Config("未知的 order_id（缺少本地映射）".into()))?;
        let account_index = self
            .cfg
            .account_index
            .ok_or_else(|| HftError::Config("缺少 account_index".into()))?;
        let api_key_index = self
            .cfg
            .api_key_index
            .ok_or_else(|| HftError::Config("缺少 api_key_index".into()))?;
        // auth
        let _ = unsafe { (self.signer.as_ref().unwrap().switch_api_key)(api_key_index as c_int) };
        let auth = self.create_auth_token(0).await?;
        let v = self
            .list_active_orders(account_index, market_id, &auth)
            .await?;
        let mut order_index: Option<i64> = None;
        if let Some(arr) = v.get("orders").and_then(|x| x.as_array()) {
            for it in arr {
                let coi = it.get("client_order_index").and_then(|x| x.as_i64());
                if coi == Some(client_order_index) {
                    order_index = it.get("order_index").and_then(|x| x.as_i64());
                    break;
                }
            }
        }
        let order_index = order_index.ok_or_else(|| {
            HftError::Exchange(format!(
                "找不到訂單（symbol={}, client_order_index={}）",
                symbol, client_order_index
            ))
        })?;
        let nonce = self.next_nonce(account_index, api_key_index).await?;
        let signer = self.signer.as_ref().unwrap();
        let ret = unsafe {
            (signer.sign_cancel_order)(
                market_id as c_int,
                order_index as c_longlong,
                nonce as c_longlong,
            )
        };
        if let Some(e) = unsafe { FfiSigner::cstr_to_string(ret.err) } {
            return Err(HftError::Exchange(format!("sign cancel error: {}", e)));
        }
        let tx_info = unsafe { FfiSigner::cstr_to_string(ret.s) }
            .ok_or_else(|| HftError::Exchange("empty cancel tx_info".into()))?;
        let _txh = self.send_tx(15, &tx_info).await?;
        if let Some(ref tx) = self.event_tx {
            let _ = tx.send(ExecutionEvent::OrderCanceled {
                order_id: order_id.clone(),
                timestamp: hft_core::now_micros(),
            });
        }
        Ok(())
    }

    async fn modify_order(
        &mut self,
        order_id: &OrderId,
        new_quantity: Option<Quantity>,
        new_price: Option<Price>,
    ) -> HftResult<()> {
        match self.cfg.mode {
            ExecutionMode::Paper => return Ok(()),
            ExecutionMode::Live => {}
        }
        let (symbol, market_id, client_order_index) = self
            .order_map
            .get(&order_id.0)
            .cloned()
            .ok_or_else(|| HftError::Config("未知的 order_id（缺少本地映射）".into()))?;
        let account_index = self
            .cfg
            .account_index
            .ok_or_else(|| HftError::Config("缺少 account_index".into()))?;
        let api_key_index = self
            .cfg
            .api_key_index
            .ok_or_else(|| HftError::Config("缺少 api_key_index".into()))?;
        let _ = unsafe { (self.signer.as_ref().unwrap().switch_api_key)(api_key_index as c_int) };
        let auth = self.create_auth_token(0).await?;
        let v = self
            .list_active_orders(account_index, market_id, &auth)
            .await?;
        let mut order_index: Option<i64> = None;
        let mut curr_base_amount: Option<i64> = None;
        let mut curr_price: Option<i64> = None;
        if let Some(arr) = v.get("orders").and_then(|x| x.as_array()) {
            for it in arr {
                let coi = it.get("client_order_index").and_then(|x| x.as_i64());
                if coi == Some(client_order_index) {
                    order_index = it.get("order_index").and_then(|x| x.as_i64());
                    curr_base_amount = it.get("base_size").and_then(|x| x.as_i64());
                    curr_price = it.get("base_price").and_then(|x| x.as_i64());
                    break;
                }
            }
        }
        let order_index = order_index.ok_or_else(|| {
            HftError::Exchange(format!(
                "找不到訂單（symbol={}, client_order_index={}）",
                symbol, client_order_index
            ))
        })?;
        let (_mid, size_dec, price_dec) = self.ensure_market_meta(&symbol).await?;
        let base_amount_i64 = if let Some(q) = new_quantity {
            Self::scale_qty(q, size_dec)
        } else {
            curr_base_amount.unwrap_or(0)
        };
        let price_i64 = if let Some(p) = new_price {
            Self::scale_price(Some(p), price_dec) as i64
        } else {
            curr_price.unwrap_or(0)
        };
        let nonce = self.next_nonce(account_index, api_key_index).await?;
        let signer = self.signer.as_ref().unwrap();
        let ret = unsafe {
            (signer.sign_modify_order)(
                market_id as c_int,
                order_index as c_longlong,
                base_amount_i64 as c_longlong,
                price_i64 as c_longlong,
                0 as c_longlong,
                nonce as c_longlong,
            )
        };
        if let Some(e) = unsafe { FfiSigner::cstr_to_string(ret.err) } {
            return Err(HftError::Exchange(format!("sign modify error: {}", e)));
        }
        let tx_info = unsafe { FfiSigner::cstr_to_string(ret.s) }
            .ok_or_else(|| HftError::Exchange("empty modify tx_info".into()))?;
        let _txh = self.send_tx(17, &tx_info).await?;
        if let Some(ref tx) = self.event_tx {
            let _ = tx.send(ExecutionEvent::OrderModified {
                order_id: order_id.clone(),
                new_quantity,
                new_price,
                timestamp: hft_core::now_micros(),
            });
        }
        Ok(())
    }

    async fn execution_stream(&self) -> HftResult<BoxStream<ExecutionEvent>> {
        if let Some(ref tx) = self.event_tx {
            let rx = tx.subscribe();
            let s = BroadcastStream::new(rx).filter_map(|e| e.ok().map(Ok));
            return Ok(Box::pin(s));
        }
        Ok(Box::pin(futures::stream::empty()))
    }

    async fn list_open_orders(&self) -> HftResult<Vec<OpenOrder>> {
        match self.cfg.mode {
            ExecutionMode::Paper => Ok(Vec::new()),
            ExecutionMode::Live => Err(HftError::Config(
                "Lighter list_open_orders 尚未實作".into(),
            )),
        }
    }

    async fn connect(&mut self) -> HftResult<()> {
        let (tx, _) = broadcast::channel(1000);
        self.event_tx = Some(tx);
        self.connected = true;

        if matches!(self.cfg.mode, ExecutionMode::Live) {
            // 載入 signer
            let lib_path = self
                .cfg
                .signer_lib_path
                .clone()
                .or_else(|| std::env::var("LIGHTER_SIGNER_LIB_PATH").ok())
                .ok_or_else(|| HftError::Config("請設定 LIGHTER_SIGNER_LIB_PATH".into()))?;
            let api_key = self
                .cfg
                .api_key_private_key
                .clone()
                .or_else(|| std::env::var("LIGHTER_API_KEY_PRIVATE_KEY").ok())
                .ok_or_else(|| HftError::Config("請設定 LIGHTER_API_KEY_PRIVATE_KEY".into()))?;
            let api_key_index = self
                .cfg
                .api_key_index
                .or_else(|| {
                    std::env::var("LIGHTER_API_KEY_INDEX")
                        .ok()
                        .and_then(|s| s.parse::<i32>().ok())
                })
                .ok_or_else(|| HftError::Config("請設定 LIGHTER_API_KEY_INDEX".into()))?;
            let account_index = self
                .cfg
                .account_index
                .or_else(|| {
                    std::env::var("LIGHTER_ACCOUNT_INDEX")
                        .ok()
                        .and_then(|s| s.parse::<i64>().ok())
                })
                .ok_or_else(|| HftError::Config("請設定 LIGHTER_ACCOUNT_INDEX".into()))?;

            unsafe {
                let signer = FfiSigner::load(&lib_path)?;
                // CreateClient(url, apiKeyHex, chainId, api_key_index, account_index)
                let url_c = CString::new(self.http.cfg.base_url.clone()).unwrap();
                let key_c = CString::new(api_key).unwrap();
                let chain_id: i32 = if self.http.cfg.base_url.contains("mainnet") {
                    304
                } else {
                    300
                };
                let err_ptr = (signer.create_client)(
                    url_c.as_ptr(),
                    key_c.as_ptr(),
                    chain_id as c_int,
                    api_key_index as c_int,
                    account_index as c_longlong,
                );
                if let Some(e) = FfiSigner::cstr_to_string(err_ptr) {
                    return Err(HftError::Config(format!("CreateClient 失敗: {}", e)));
                }
                // 切換當前 API key（可選，但保持一致性）
                let err2 = (signer.switch_api_key)(api_key_index as c_int);
                if let Some(e) = FfiSigner::cstr_to_string(err2) {
                    return Err(HftError::Config(format!("SwitchAPIKey 失敗: {}", e)));
                }
                self.signer = Some(signer);
            }
        }

        info!("Lighter 執行客戶端就緒");
        Ok(())
    }

    async fn disconnect(&mut self) -> HftResult<()> {
        self.event_tx = None;
        self.connected = false;
        Ok(())
    }

    async fn health(&self) -> ports::ConnectionHealth {
        ports::ConnectionHealth {
            connected: self.connected,
            latency_ms: None,
            last_heartbeat: hft_core::now_micros(),
        }
    }
}
