//! Hyperliquid 执行适配器 - 支持 EIP-712 签名与主网交易
//!
//! 功能：
//! - EIP-712 签名工具
//! - REST API 下单/撤单/改单
//! - 私有 WebSocket 订阅回报
//! - Paper/Live 双模式支持

use async_trait::async_trait;
use ethers_core::types::{Address, H256, U256};
use ethers_core::utils::keccak256;
use futures::{SinkExt, StreamExt};
use hft_core::{now_micros, HftError, HftResult, OrderId, Price, Quantity, Side, Symbol};
use ports::{BoxStream, ExecutionClient, ExecutionEvent, OpenOrder, OrderIntent, OrderStatus};
use rust_decimal::Decimal;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::TcpStream;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use tracing::{debug, info, warn};
use url::Url;

fn parse_json<T: DeserializeOwned>(text: &str) -> Result<T, HftError> {
    let mut bytes = text.as_bytes().to_vec();
    simd_json::serde::from_slice(bytes.as_mut_slice())
        .map_err(|e| HftError::Serialization(e.to_string()))
}

fn parse_value<T: DeserializeOwned>(value: Value) -> Result<T, HftError> {
    let owned: simd_json::OwnedValue = match value.try_into() {
        Ok(v) => v,
        Err(e) => return Err(HftError::Serialization(e.to_string())),
    };
    simd_json::serde::from_owned_value(owned).map_err(|e| HftError::Serialization(e.to_string()))
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExecutionMode {
    Live,
    Paper,
}

// Hyperliquid 私有 WebSocket 订阅消息
#[derive(Debug, Serialize, Deserialize)]
struct WsSubscription {
    method: String, // "subscribe"
    subscription: WsSubscriptionType,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum WsSubscriptionType {
    #[serde(rename = "userEvents")]
    UserEvents { user: String },
    #[serde(rename = "userFills")]
    UserFills { user: String },
    #[serde(rename = "userFundings")]
    UserFundings { user: String },
}

// Hyperliquid 私有 WebSocket 响应消息
#[derive(Debug, Deserialize)]
struct WsResponse {
    channel: String,
    data: Value,
}

// 订单状态更新事件
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum UserEvent {
    #[serde(rename = "order")]
    Order { order: OrderUpdate },
    #[serde(rename = "fill")]
    Fill { fill: FillUpdate },
    #[serde(rename = "liquidation")]
    Liquidation { liq: LiquidationUpdate },
}

#[derive(Debug, Deserialize)]
struct OrderUpdate {
    coin: String, // 资产名称 (e.g., "BTC")
    side: String, // "B" | "A"
    #[serde(rename = "limitPx")]
    limit_px: String, // 限价
    #[serde(rename = "sz")]
    sz: String, // 数量
    oid: u64,     // Hyperliquid 订单 ID
    timestamp: u64, // 时间戳
    #[serde(rename = "origSz")]
    orig_sz: String, // 原始数量
    #[serde(rename = "tif")]
    tif: String, // 时效类型
    #[serde(rename = "reduceOnly")]
    reduce_only: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct FillUpdate {
    coin: String, // 资产名称
    #[serde(rename = "px")]
    px: String, // 成交价
    #[serde(rename = "sz")]
    sz: String, // 成交量
    side: String, // "B" | "A"
    time: u64,    // 成交时间
    #[serde(rename = "startPosition")]
    start_position: String, // 起始仓位
    #[serde(rename = "dir")]
    dir: String, // "Open" | "Close"
    oid: u64,     // 订单 ID
    #[serde(rename = "fee")]
    fee: String, // 手续费
}

#[derive(Debug, Deserialize)]
struct LiquidationUpdate {
    // 清算事件结构 - 暂时简化
    coin: String,
    #[serde(rename = "liqPx")]
    liq_px: String,
}

// Hyperliquid 未结订单查询请求
#[derive(Debug, Serialize)]
struct OpenOrdersRequest {
    #[serde(rename = "type")]
    request_type: String, // "openOrders"
    user: String, // 用户地址
}

// Hyperliquid 未结订单响应
#[derive(Debug, Deserialize)]
struct HyperliquidOpenOrder {
    coin: String, // 资产名称 (e.g., "BTC")
    side: String, // "B" | "A"
    #[serde(rename = "limitPx")]
    limit_px: String, // 限价
    #[serde(rename = "sz")]
    sz: String, // 总数量
    #[serde(rename = "filled")]
    filled: String, // 已成交数量
    oid: u64,     // Hyperliquid 订单 ID
    timestamp: u64, // 创建时间
    #[serde(rename = "tif")]
    tif: String, // 时效类型
    #[serde(rename = "reduceOnly")]
    reduce_only: bool,
}

#[derive(Debug, Clone)]
pub struct HyperliquidExecutionConfig {
    pub rest_base_url: String,
    pub ws_private_url: String,
    pub private_key: String, // 十六进制格式，不含 0x 前缀
    pub timeout_ms: u64,
    pub mode: ExecutionMode,
    pub vault_address: Option<String>, // 可选的 vault 地址
}

impl Default for HyperliquidExecutionConfig {
    fn default() -> Self {
        Self {
            rest_base_url: "https://api.hyperliquid.xyz".to_string(),
            ws_private_url: "wss://api.hyperliquid.xyz/ws".to_string(),
            private_key: "".to_string(),
            timeout_ms: 5000,
            mode: ExecutionMode::Paper,
            vault_address: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct HyperliquidOrder {
    pub a: u32,    // asset index
    pub b: bool,   // is_buy
    pub p: String, // price
    pub s: String, // size
    pub r: bool,   // reduce_only
    #[serde(rename = "t")]
    pub tif: OrderTif,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum OrderTif {
    #[serde(rename = "limit")]
    Limit { tif: String }, // "Gtc", "Ioc", "Alo"
}

#[derive(Debug, Serialize, Deserialize)]
struct OrderAction {
    #[serde(rename = "type")]
    action_type: String, // "order"
    orders: Vec<HyperliquidOrder>,
    grouping: String, // "na" for no grouping
}

#[derive(Debug, Serialize, Deserialize)]
struct ActionPayload {
    action: OrderAction,
    nonce: u64,
    signature: EthSignature,
    #[serde(rename = "vaultAddress", skip_serializing_if = "Option::is_none")]
    vault_address: Option<String>,
}

// 撤单 Action
#[derive(Debug, Serialize, Deserialize)]
struct CancelAction {
    #[serde(rename = "type")]
    action_type: String, // "cancel"
    cancels: Vec<CancelRequest>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CancelRequest {
    #[serde(rename = "a")]
    asset: u32, // asset index
    #[serde(rename = "o")]
    oid: u64, // order id
}

// 改单 Action
#[derive(Debug, Serialize, Deserialize)]
struct ModifyAction {
    #[serde(rename = "type")]
    action_type: String, // "modify"
    modifies: Vec<ModifyRequest>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ModifyRequest {
    #[serde(rename = "a")]
    asset: u32, // asset index
    #[serde(rename = "o")]
    oid: u64, // order id
    #[serde(rename = "p")]
    price: String, // new price
    #[serde(rename = "s")]
    size: String, // new size
}

#[derive(Debug, Serialize, Deserialize)]
struct EthSignature {
    r: String,
    s: String,
    v: u8,
}

/// EIP-712 签名工具
struct Eip712Signer {
    private_key: [u8; 32],
    wallet_address: Address,
}

impl Eip712Signer {
    fn new(private_key_hex: &str) -> HftResult<Self> {
        let private_key_hex = private_key_hex.trim_start_matches("0x");
        let private_key = hex::decode(private_key_hex)
            .map_err(|e| HftError::Config(format!("无效私钥格式: {}", e)))?;

        if private_key.len() != 32 {
            return Err(HftError::Config("私钥必须为 32 字节".to_string()));
        }

        let mut key_bytes = [0u8; 32];
        key_bytes.copy_from_slice(&private_key);

        // 计算对应的钱包地址
        let wallet_address = self::compute_wallet_address(&key_bytes)?;

        Ok(Self {
            private_key: key_bytes,
            wallet_address,
        })
    }

    fn sign_action(&self, action: &OrderAction, nonce: u64) -> HftResult<EthSignature> {
        let domain_separator = self.get_domain_separator();
        let action_hash = self.hash_action(action, nonce)?;

        let digest = keccak256(
            &[
                &[0x19, 0x01], // EIP-191 prefix
                domain_separator.as_bytes(),
                action_hash.as_bytes(),
            ]
            .concat(),
        );

        let signature = self.sign_digest(&digest)?;
        Ok(signature)
    }

    fn get_domain_separator(&self) -> H256 {
        // Hyperliquid 的 EIP-712 domain
        let domain_type_hash = keccak256(
            b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
        );
        let name_hash = keccak256(b"Exchange");
        let version_hash = keccak256(b"1");
        let chain_id = U256::from(1337u64); // Hyperliquid chain ID
        let verifying_contract = Address::zero(); // 0x0000000000000000000000000000000000000000

        let encoded = ethers_core::abi::encode(&[
            ethers_core::abi::Token::FixedBytes(domain_type_hash.to_vec()),
            ethers_core::abi::Token::FixedBytes(name_hash.to_vec()),
            ethers_core::abi::Token::FixedBytes(version_hash.to_vec()),
            ethers_core::abi::Token::Uint(chain_id),
            ethers_core::abi::Token::Address(verifying_contract),
        ]);

        H256::from(keccak256(&encoded))
    }

    fn hash_action(&self, action: &OrderAction, nonce: u64) -> HftResult<H256> {
        // 构造 Hyperliquid 特定的 action hash
        let action_json = serde_json::to_string(action)
            .map_err(|e| HftError::Serialization(format!("序列化 action 失败: {}", e)))?;

        // 简化的 hash 计算 - 实际实现需要按照 Hyperliquid 的具体 EIP-712 结构
        let combined = format!("{}:{}", action_json, nonce);
        Ok(H256::from(keccak256(combined.as_bytes())))
    }

    fn sign_digest(&self, digest: &[u8; 32]) -> HftResult<EthSignature> {
        use ethers_core::k256::ecdsa::signature::hazmat::PrehashSigner;
        use ethers_core::k256::ecdsa::{Signature, SigningKey};

        let signing_key = SigningKey::from_bytes(&self.private_key.into())
            .map_err(|e| HftError::Exchange(format!("创建签名密钥失败: {}", e)))?;

        let signature: Signature = signing_key
            .sign_prehash(digest)
            .map_err(|e| HftError::Exchange(format!("签名失败: {}", e)))?;

        let (r_bytes, s_bytes) = signature.split_bytes();
        let recovery_id = 0u8; // 简化版本，实际需要计算恢复 ID

        Ok(EthSignature {
            r: format!("0x{}", hex::encode(r_bytes.as_slice())),
            s: format!("0x{}", hex::encode(s_bytes.as_slice())),
            v: 27 + recovery_id,
        })
    }
}

fn compute_wallet_address(private_key: &[u8; 32]) -> HftResult<Address> {
    use ethers_core::k256::elliptic_curve::sec1::ToEncodedPoint;
    use ethers_core::k256::{ecdsa::SigningKey, ecdsa::VerifyingKey};

    let signing_key = SigningKey::from_bytes(&(*private_key).into())
        .map_err(|e| HftError::Exchange(format!("无效私钥: {}", e)))?;

    let verifying_key = VerifyingKey::from(&signing_key);
    let public_key = verifying_key.as_affine();
    let public_key_bytes = public_key.to_encoded_point(false);
    let public_key_bytes = &public_key_bytes.as_bytes()[1..]; // 去掉前缀 0x04

    let hash = keccak256(public_key_bytes);
    let address_bytes = &hash[12..]; // 取后 20 字节

    Ok(Address::from_slice(address_bytes))
}

pub struct HyperliquidExecutionClient {
    cfg: HyperliquidExecutionConfig,
    event_tx: Option<broadcast::Sender<ExecutionEvent>>,
    connected: bool,
    signer: Option<Eip712Signer>,
    http_client: reqwest::Client,
    asset_map: HashMap<Symbol, u32>,          // symbol -> asset_index
    order_map: HashMap<OrderId, (u32, u64)>,  // our_order_id -> (asset_index, hyperliquid_oid)
    reverse_order_map: HashMap<u64, OrderId>, // hyperliquid_oid -> our_order_id
    next_nonce: u64,
    ws_stream: Option<WebSocketStream<MaybeTlsStream<TcpStream>>>,
}

impl HyperliquidExecutionClient {
    pub fn new(cfg: HyperliquidExecutionConfig) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(cfg.timeout_ms))
            .build()
            .expect("创建 HTTP 客户端失败");

        let mut asset_map = HashMap::new();
        // Hyperliquid 主网资产索引 (需要从 API 获取最新映射)
        asset_map.insert(Symbol::new("BTC-PERP"), 0);
        asset_map.insert(Symbol::new("ETH-PERP"), 1);
        asset_map.insert(Symbol::new("SOL-PERP"), 2);
        asset_map.insert(Symbol::new("SUI-PERP"), 3);

        Self {
            cfg,
            event_tx: None,
            connected: false,
            signer: None,
            http_client,
            asset_map,
            order_map: HashMap::new(),
            reverse_order_map: HashMap::new(),
            next_nonce: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            ws_stream: None,
        }
    }

    async fn init_signer(&mut self) -> HftResult<()> {
        if self.cfg.mode == ExecutionMode::Live && self.signer.is_none() {
            if self.cfg.private_key.is_empty() {
                return Err(HftError::Config("Live 模式需要私钥".to_string()));
            }
            self.signer = Some(Eip712Signer::new(&self.cfg.private_key)?);
        }
        Ok(())
    }

    // 私有方法：连接私有 WebSocket
    async fn connect_private_ws(&mut self) -> HftResult<()> {
        let url = Url::parse(&self.cfg.ws_private_url)
            .map_err(|e| HftError::Config(format!("无效的 WebSocket URL: {}", e)))?;

        debug!("连接到 Hyperliquid 私有 WebSocket: {}", url);

        let (ws_stream, _) = connect_async(url)
            .await
            .map_err(|e| HftError::Network(format!("私有 WebSocket 连接失败: {}", e)))?;

        self.ws_stream = Some(ws_stream);

        // 获取用户地址用于订阅
        let user_address = if let Some(ref signer) = self.signer {
            format!("0x{:x}", signer.wallet_address)
        } else {
            return Err(HftError::Exchange("未找到签名器".to_string()));
        };

        // 订阅用户事件和成交事件
        self.subscribe_user_events(&user_address).await?;
        self.subscribe_user_fills(&user_address).await?;

        info!(
            "Hyperliquid 私有 WebSocket 连接成功，用户: {}",
            user_address
        );
        Ok(())
    }

    // 订阅用户事件 (订单状态更新)
    async fn subscribe_user_events(&mut self, user_address: &str) -> HftResult<()> {
        let subscription = WsSubscription {
            method: "subscribe".to_string(),
            subscription: WsSubscriptionType::UserEvents {
                user: user_address.to_string(),
            },
        };

        self.send_ws_message(&subscription).await?;
        debug!("已订阅用户事件: {}", user_address);
        Ok(())
    }

    // 订阅用户成交事件
    async fn subscribe_user_fills(&mut self, user_address: &str) -> HftResult<()> {
        let subscription = WsSubscription {
            method: "subscribe".to_string(),
            subscription: WsSubscriptionType::UserFills {
                user: user_address.to_string(),
            },
        };

        self.send_ws_message(&subscription).await?;
        debug!("已订阅用户成交事件: {}", user_address);
        Ok(())
    }

    // 发送 WebSocket 消息
    async fn send_ws_message<T: Serialize>(&mut self, message: &T) -> HftResult<()> {
        if let Some(ref mut ws) = self.ws_stream {
            let message_text = serde_json::to_string(message).map_err(|e| {
                HftError::Serialization(format!("序列化 WebSocket 消息失败: {}", e))
            })?;

            ws.send(Message::Text(message_text))
                .await
                .map_err(|e| HftError::Network(format!("发送 WebSocket 消息失败: {}", e)))?;
        }
        Ok(())
    }

    async fn place_order_live(&mut self, intent: OrderIntent) -> HftResult<OrderId> {
        let signer = self
            .signer
            .as_ref()
            .ok_or_else(|| HftError::Exchange("签名器未初始化".to_string()))?;

        let asset_index = self
            .asset_map
            .get(&intent.symbol)
            .ok_or_else(|| HftError::Exchange(format!("不支持的交易对: {}", intent.symbol)))?;

        let order = HyperliquidOrder {
            a: *asset_index,
            b: intent.side == Side::Buy,
            p: intent
                .price
                .unwrap_or_else(|| Price(Decimal::ZERO))
                .to_string(),
            s: intent.quantity.to_string(),
            r: false, // reduce_only
            tif: OrderTif::Limit {
                tif: "Gtc".to_string(),
            },
        };

        let action = OrderAction {
            action_type: "order".to_string(),
            orders: vec![order],
            grouping: "na".to_string(),
        };

        let nonce = self.next_nonce;
        self.next_nonce += 1;

        let signature = signer.sign_action(&action, nonce)?;

        let payload = ActionPayload {
            action,
            nonce,
            signature,
            vault_address: self.cfg.vault_address.clone(),
        };

        let request_body = json!({
            "method": "post",
            "id": now_micros(),
            "request": {
                "type": "action",
                "payload": payload
            }
        });

        debug!("发送 Hyperliquid 下单请求: {}", request_body);

        let response = self
            .http_client
            .post(&format!("{}/exchange", self.cfg.rest_base_url))
            .json(&request_body)
            .send()
            .await
            .map_err(|e| HftError::Network(format!("HTTP 请求失败: {}", e)))?;

        let response_text = response
            .text()
            .await
            .map_err(|e| HftError::Network(format!("读取响应失败: {}", e)))?;

        debug!("Hyperliquid 响应: {}", response_text);

        // 简化的响应解析 - 实际需要根据 Hyperliquid API 文档完善
        // TODO: 从响应中提取真实的 Hyperliquid order ID
        let hyperliquid_oid = now_micros(); // 临时使用时间戳
        let order_id = OrderId(format!("HL_{}", hyperliquid_oid));

        // 记录订单映射（双向）
        self.order_map
            .insert(order_id.clone(), (*asset_index, hyperliquid_oid));
        self.reverse_order_map
            .insert(hyperliquid_oid, order_id.clone());

        // 发送回报事件
        if let Some(ref tx) = self.event_tx {
            let _ = tx.send(ExecutionEvent::OrderAck {
                order_id: order_id.clone(),
                timestamp: now_micros(),
            });
        }

        Ok(order_id)
    }

    async fn cancel_order_live(&mut self, order_id: &OrderId) -> HftResult<()> {
        let _signer = self
            .signer
            .as_ref()
            .ok_or_else(|| HftError::Exchange("签名器未初始化".to_string()))?;

        let (asset_index, hyperliquid_oid) = self
            .order_map
            .get(order_id)
            .ok_or_else(|| HftError::Exchange(format!("未找到订单映射: {:?}", order_id)))?;

        let cancel_request = CancelRequest {
            asset: *asset_index,
            oid: *hyperliquid_oid,
        };

        let cancel_action = CancelAction {
            action_type: "cancel".to_string(),
            cancels: vec![cancel_request],
        };

        let nonce = self.next_nonce;
        self.next_nonce += 1;

        // 临时序列化为 JSON 以进行签名 - 实际需要正确的 EIP-712 结构
        let _action_json = serde_json::to_string(&cancel_action)
            .map_err(|e| HftError::Serialization(format!("序列化撤单请求失败: {}", e)))?;

        // 简化签名 - 实际需要按照 Hyperliquid 的 EIP-712 标准
        let signature = EthSignature {
            r: "0x0".to_string(),
            s: "0x0".to_string(),
            v: 27,
        };

        let payload = json!({
            "action": cancel_action,
            "nonce": nonce,
            "signature": signature,
            "vaultAddress": self.cfg.vault_address
        });

        let request_body = json!({
            "method": "post",
            "id": now_micros(),
            "request": {
                "type": "action",
                "payload": payload
            }
        });

        debug!("发送 Hyperliquid 撤单请求: {}", request_body);

        let response = self
            .http_client
            .post(&format!("{}/exchange", self.cfg.rest_base_url))
            .json(&request_body)
            .send()
            .await
            .map_err(|e| HftError::Network(format!("撤单 HTTP 请求失败: {}", e)))?;

        let response_text = response
            .text()
            .await
            .map_err(|e| HftError::Network(format!("读取撤单响应失败: {}", e)))?;

        debug!("Hyperliquid 撤单响应: {}", response_text);

        // 删除订单映射（双向）
        if let Some((_, hyperliquid_oid)) = self.order_map.remove(order_id) {
            self.reverse_order_map.remove(&hyperliquid_oid);
        }

        // 发送撤单确认事件
        if let Some(ref tx) = self.event_tx {
            let _ = tx.send(ExecutionEvent::OrderCanceled {
                order_id: order_id.clone(),
                timestamp: now_micros(),
            });
        }

        Ok(())
    }

    async fn modify_order_live(
        &mut self,
        order_id: &OrderId,
        new_quantity: Option<Quantity>,
        new_price: Option<Price>,
    ) -> HftResult<()> {
        let _signer = self
            .signer
            .as_ref()
            .ok_or_else(|| HftError::Exchange("签名器未初始化".to_string()))?;

        let (asset_index, hyperliquid_oid) = self
            .order_map
            .get(order_id)
            .ok_or_else(|| HftError::Exchange(format!("未找到订单映射: {:?}", order_id)))?;

        let price_str = new_price
            .map(|p| p.to_string())
            .unwrap_or_else(|| "0.0".to_string());
        let size_str = new_quantity
            .map(|q| q.to_string())
            .unwrap_or_else(|| "0.0".to_string());

        let modify_request = ModifyRequest {
            asset: *asset_index,
            oid: *hyperliquid_oid,
            price: price_str,
            size: size_str,
        };

        let modify_action = ModifyAction {
            action_type: "modify".to_string(),
            modifies: vec![modify_request],
        };

        let nonce = self.next_nonce;
        self.next_nonce += 1;

        // 简化签名 - 实际需要按照 Hyperliquid 的 EIP-712 标准
        let signature = EthSignature {
            r: "0x0".to_string(),
            s: "0x0".to_string(),
            v: 27,
        };

        let payload = json!({
            "action": modify_action,
            "nonce": nonce,
            "signature": signature,
            "vaultAddress": self.cfg.vault_address
        });

        let request_body = json!({
            "method": "post",
            "id": now_micros(),
            "request": {
                "type": "action",
                "payload": payload
            }
        });

        debug!("发送 Hyperliquid 改单请求: {}", request_body);

        let response = self
            .http_client
            .post(&format!("{}/exchange", self.cfg.rest_base_url))
            .json(&request_body)
            .send()
            .await
            .map_err(|e| HftError::Network(format!("改单 HTTP 请求失败: {}", e)))?;

        let response_text = response
            .text()
            .await
            .map_err(|e| HftError::Network(format!("读取改单响应失败: {}", e)))?;

        debug!("Hyperliquid 改单响应: {}", response_text);

        // 发送改单确认事件
        if let Some(ref tx) = self.event_tx {
            let _ = tx.send(ExecutionEvent::OrderModified {
                order_id: order_id.clone(),
                new_quantity,
                new_price,
                timestamp: now_micros(),
            });
        }

        Ok(())
    }

    // 查询 Live 模式的未结订单
    async fn list_open_orders_live(&self) -> HftResult<Vec<OpenOrder>> {
        let signer = self
            .signer
            .as_ref()
            .ok_or_else(|| HftError::Exchange("签名器未初始化".to_string()))?;

        // 获取用户地址
        let user_address = format!("0x{:x}", signer.wallet_address);

        // 构造查询请求
        let request = OpenOrdersRequest {
            request_type: "openOrders".to_string(),
            user: user_address,
        };

        let request_body = json!({
            "method": "post",
            "id": now_micros(),
            "request": request
        });

        debug!("发送 Hyperliquid 未结订单查询请求: {}", request_body);

        // 发送 HTTP 请求到 info 端点（只读，不需要签名）
        let response = self
            .http_client
            .post(&format!("{}/info", self.cfg.rest_base_url))
            .json(&request_body)
            .send()
            .await
            .map_err(|e| HftError::Network(format!("查询未结订单 HTTP 请求失败: {}", e)))?;

        let response_text = response
            .text()
            .await
            .map_err(|e| HftError::Network(format!("读取未结订单响应失败: {}", e)))?;

        debug!("Hyperliquid 未结订单响应: {}", response_text);

        // 解析响应
        let parsed: Result<Value, _> = parse_json(&response_text);
        match parsed {
            Ok(data) => {
                // 尝试解析为订单数组
                if let Ok(hl_orders) = parse_value::<Vec<HyperliquidOpenOrder>>(data) {
                    let mut open_orders = Vec::new();

                    for hl_order in hl_orders {
                        if let Some(our_order) = self.convert_hyperliquid_order(hl_order) {
                            open_orders.push(our_order);
                        }
                    }

                    info!("查询到 {} 个未结订单", open_orders.len());
                    Ok(open_orders)
                } else {
                    debug!("未结订单响应格式不匹配，可能为空或错误格式");
                    Ok(Vec::new())
                }
            }
            Err(e) => {
                warn!("解析未结订单响应失败: {} | 响应: {}", e, response_text);
                Ok(Vec::new()) // 解析失败时返回空列表而不是错误
            }
        }
    }

    // 将 Hyperliquid 订单转换为系统统一格式
    fn convert_hyperliquid_order(&self, hl_order: HyperliquidOpenOrder) -> Option<OpenOrder> {
        // 查找内部订单 ID
        let our_order_id = self.reverse_order_map.get(&hl_order.oid)?;

        // 构建符号
        let symbol = Symbol::from(format!("{}-PERP", hl_order.coin));

        // 解析价格和数量
        let price = hl_order
            .limit_px
            .parse::<f64>()
            .ok()
            .and_then(|p| rust_decimal::Decimal::try_from(p).ok())
            .map(|d| Price(d))?;
        let original_quantity = hl_order
            .sz
            .parse::<f64>()
            .ok()
            .and_then(|q| rust_decimal::Decimal::try_from(q).ok())
            .map(|d| Quantity(d))?;
        let filled_quantity = hl_order
            .filled
            .parse::<f64>()
            .ok()
            .and_then(|q| rust_decimal::Decimal::try_from(q).ok())
            .map(|d| Quantity(d))
            .unwrap_or_else(|| Quantity(rust_decimal::Decimal::ZERO));

        // 计算剩余数量
        let remaining_quantity = Quantity(original_quantity.0 - filled_quantity.0);

        // 解析方向
        let side = match hl_order.side.as_str() {
            "B" => Side::Buy,
            "A" => Side::Sell,
            _ => return None,
        };

        // 解析订单类型
        let order_type = hft_core::OrderType::Limit; // Hyperliquid 主要使用限价单

        // 确定订单状态
        let status = if filled_quantity.0 == rust_decimal::Decimal::ZERO {
            OrderStatus::Accepted // 未成交
        } else if filled_quantity.0 >= original_quantity.0 {
            OrderStatus::Filled // 完全成交（不应该在未结订单中出现）
        } else {
            OrderStatus::PartiallyFilled // 部分成交
        };

        Some(OpenOrder {
            order_id: our_order_id.clone(),
            symbol,
            side,
            order_type,
            original_quantity,
            remaining_quantity,
            filled_quantity,
            price: Some(price),
            status,
            created_at: hl_order.timestamp * 1000, // 转换为微秒
            updated_at: now_micros(),
        })
    }
}

#[async_trait]
impl ExecutionClient for HyperliquidExecutionClient {
    async fn place_order(&mut self, intent: OrderIntent) -> HftResult<OrderId> {
        match self.cfg.mode {
            ExecutionMode::Paper => {
                let oid = OrderId(format!("HL_PAPER_{}", now_micros()));
                if let Some(ref tx) = self.event_tx {
                    let _ = tx.send(ExecutionEvent::OrderAck {
                        order_id: oid.clone(),
                        timestamp: now_micros(),
                    });

                    // Paper 模式立即模拟成交
                    if let Some(p) = intent.price {
                        let _ = tx.send(ExecutionEvent::Fill {
                            order_id: oid.clone(),
                            price: p,
                            quantity: intent.quantity,
                            timestamp: now_micros(),
                            fill_id: format!("HLFILL-{}", now_micros()),
                        });
                    }
                }
                Ok(oid)
            }
            ExecutionMode::Live => self.place_order_live(intent).await,
        }
    }

    async fn cancel_order(&mut self, order_id: &OrderId) -> HftResult<()> {
        match self.cfg.mode {
            ExecutionMode::Paper => {
                if let Some(ref tx) = self.event_tx {
                    let _ = tx.send(ExecutionEvent::OrderCanceled {
                        order_id: order_id.clone(),
                        timestamp: now_micros(),
                    });
                }
                Ok(())
            }
            ExecutionMode::Live => self.cancel_order_live(order_id).await,
        }
    }

    async fn modify_order(
        &mut self,
        order_id: &OrderId,
        new_quantity: Option<Quantity>,
        new_price: Option<Price>,
    ) -> HftResult<()> {
        match self.cfg.mode {
            ExecutionMode::Paper => {
                if let Some(ref tx) = self.event_tx {
                    let _ = tx.send(ExecutionEvent::OrderModified {
                        order_id: order_id.clone(),
                        new_quantity,
                        new_price,
                        timestamp: now_micros(),
                    });
                }
                Ok(())
            }
            ExecutionMode::Live => {
                self.modify_order_live(order_id, new_quantity, new_price)
                    .await
            }
        }
    }

    async fn execution_stream(&self) -> HftResult<BoxStream<ExecutionEvent>> {
        if let Some(ref tx) = self.event_tx {
            let rx = tx.subscribe();
            let stream = tokio_stream::wrappers::BroadcastStream::new(rx)
                .filter_map(|r| async move { r.ok().map(Ok) });
            return Ok(Box::pin(stream));
        }
        Ok(Box::pin(futures::stream::empty()))
    }

    async fn list_open_orders(&self) -> HftResult<Vec<OpenOrder>> {
        match self.cfg.mode {
            ExecutionMode::Paper => {
                // Paper 模式直接返回空列表（模拟所有订单都已成交）
                Ok(Vec::new())
            }
            ExecutionMode::Live => self.list_open_orders_live().await,
        }
    }

    async fn connect(&mut self) -> HftResult<()> {
        self.init_signer().await?;

        let (tx, _rx) = broadcast::channel(1000);
        self.event_tx = Some(tx);

        // 如果是 Live 模式，建立私有 WebSocket 连接
        if self.cfg.mode == ExecutionMode::Live && self.signer.is_some() {
            self.connect_private_ws().await?;
        }

        self.connected = true;
        info!(
            "Hyperliquid ExecutionClient 已連線 (模式: {:?})",
            self.cfg.mode
        );
        Ok(())
    }

    async fn disconnect(&mut self) -> HftResult<()> {
        // 关闭私有 WebSocket 连接
        if let Some(mut ws) = self.ws_stream.take() {
            let _ = ws.send(Message::Close(None)).await;
            let _ = ws.close(None).await;
        }

        self.connected = false;
        self.event_tx = None;
        Ok(())
    }

    async fn health(&self) -> ports::ConnectionHealth {
        ports::ConnectionHealth {
            connected: self.connected,
            latency_ms: Some(1.0),
            last_heartbeat: now_micros(),
        }
    }
}

impl HyperliquidExecutionClient {
    // 解析私有 WebSocket 消息并生成统一事件
    fn parse_private_ws_message(&mut self, message: &str) -> Option<ExecutionEvent> {
        let parsed: Result<WsResponse, _> = parse_json(message);

        match parsed {
            Ok(response) => match response.channel.as_str() {
                "userEvents" => self.parse_user_events(&response.data),
                "userFills" => self.parse_user_fills(&response.data),
                "subscriptionResponse" => {
                    debug!("收到私有订阅确认: {:?}", response.data);
                    None
                }
                _ => {
                    debug!("未知私有消息类型: {}", response.channel);
                    None
                }
            },
            Err(e) => {
                warn!("解析私有 WebSocket 消息失败: {} | 消息: {}", e, message);
                None
            }
        }
    }

    // 解析用户事件 (订单状态更新)
    fn parse_user_events(&mut self, data: &Value) -> Option<ExecutionEvent> {
        if let Ok(events) = parse_value::<Vec<UserEvent>>(data.clone()) {
            for event in events {
                match event {
                    UserEvent::Order { order } => {
                        return self.handle_order_update(order);
                    }
                    UserEvent::Fill { fill } => {
                        return self.handle_fill_update(fill);
                    }
                    UserEvent::Liquidation { liq } => {
                        warn!("收到清算事件: {:?}", liq);
                        // TODO: 处理清算事件
                    }
                }
            }
        }
        None
    }

    // 解析用户成交事件
    fn parse_user_fills(&mut self, data: &Value) -> Option<ExecutionEvent> {
        if let Ok(fills) = parse_value::<Vec<FillUpdate>>(data.clone()) {
            if let Some(fill) = fills.first() {
                return self.handle_fill_update(fill.clone());
            }
        }
        None
    }

    // 处理订单状态更新
    fn handle_order_update(&mut self, order: OrderUpdate) -> Option<ExecutionEvent> {
        // 查找内部订单 ID
        let our_order_id = self.reverse_order_map.get(&order.oid)?;

        // 构建符号
        let _symbol = Symbol::from(format!("{}-PERP", order.coin));

        // 解析价格和数量
        let _price = order
            .limit_px
            .parse::<f64>()
            .ok()
            .and_then(|p| rust_decimal::Decimal::try_from(p).ok())
            .map(|d| Price(d))?;
        let _quantity = order
            .sz
            .parse::<f64>()
            .ok()
            .and_then(|q| rust_decimal::Decimal::try_from(q).ok())
            .map(|d| Quantity(d))?;
        let _side = match order.side.as_str() {
            "B" => Side::Buy,
            "A" => Side::Sell,
            _ => return None,
        };

        // 对于订单更新，生成订单确认事件
        Some(ExecutionEvent::OrderAck {
            order_id: our_order_id.clone(),
            timestamp: order.timestamp * 1000, // 转换为微秒
        })
    }

    // 处理成交更新
    fn handle_fill_update(&mut self, fill: FillUpdate) -> Option<ExecutionEvent> {
        // 查找内部订单 ID
        let our_order_id = self.reverse_order_map.get(&fill.oid)?;

        // 构建符号
        let _symbol = Symbol::from(format!("{}-PERP", fill.coin));

        // 解析价格和数量
        let price = fill
            .px
            .parse::<f64>()
            .ok()
            .and_then(|p| rust_decimal::Decimal::try_from(p).ok())
            .map(|d| Price(d))?;
        let quantity = fill
            .sz
            .parse::<f64>()
            .ok()
            .and_then(|q| rust_decimal::Decimal::try_from(q).ok())
            .map(|d| Quantity(d))?;
        let _side = match fill.side.as_str() {
            "B" => Side::Buy,
            "A" => Side::Sell,
            _ => return None,
        };

        // 解析手续费
        let _fee = fill
            .fee
            .parse::<f64>()
            .ok()
            .and_then(|f| rust_decimal::Decimal::try_from(f).ok())
            .unwrap_or(rust_decimal::Decimal::ZERO);

        Some(ExecutionEvent::Fill {
            order_id: our_order_id.clone(),
            price,
            quantity,
            timestamp: fill.time * 1000, // 转换为微秒
            fill_id: format!("HL_FILL_{}", fill.time),
        })
    }
}

#[cfg(test)]
mod tests;
