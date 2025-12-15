//! Bitget 零拷貝 WebSocket 適配器 - 使用 channel 傳遞事件
//!
//! 優化要點：
//! 1. 使用 tokio::mpsc channel 解耦 adapter 與 engine
//! 2. 使用 simd-json 借用模式減少分配
//! 3. 復用緩衝區，統一時間戳處理
//!
//! 注意：不再直接依賴 engine::dataflow::EventIngester，保持 adapter 層獨立性

#![allow(dead_code)]

use adapters_common::ws_helpers::constants;
use async_trait::async_trait;
use integration::latency::WsFrameMetrics;
use tokio::sync::mpsc;
use tracing::{debug, error, info, trace, warn};

#[cfg(feature = "json-simd")]
use simd_json::prelude::{ValueAsContainer, ValueAsScalar, ValueObjectAccess};

#[cfg(feature = "metrics")]
use infra_metrics::MetricsRegistry;

use hft_core::*;
use integration::ws::{MessageHandler, ReconnectingWsClient, WsClientConfig};
use ports::*;

/// 零拷貝 Bitget 市場流
///
/// 使用 tokio channel 傳遞事件，保持與 engine 層的解耦
pub struct ZeroCopyBitgetStream {
    ws_client: Option<ReconnectingWsClient>,
    event_sender: Option<mpsc::UnboundedSender<MarketEvent>>,
    subscribed_symbols: Vec<Symbol>,
    /// 復用的 JSON 解析緩衝區
    parse_buffer: Vec<u8>,
}

impl Default for ZeroCopyBitgetStream {
    fn default() -> Self {
        Self::new()
    }
}

impl ZeroCopyBitgetStream {
    /// 創建新的零拷貝 Bitget 流
    pub fn new() -> Self {
        Self {
            ws_client: None,
            event_sender: None,
            subscribed_symbols: Vec::new(),
            parse_buffer: Vec::with_capacity(4096), // 預分配 4KB 緩衝
        }
    }

    fn create_ws_config() -> WsClientConfig {
        // 放寬訊息/幀上限以容納多品種 LOB/Trade 聚合
        WsClientConfig {
            url: "wss://ws.bitget.com/v2/ws/public".to_string(),
            heartbeat_interval: constants::heartbeat_interval(),
            reconnect_interval: constants::reconnect_interval(),
            max_reconnect_attempts: 10,
            tcp_nodelay: true,            // HFT 必須啟用
            disable_compression: true,    // HFT 必須禁用壓縮
            max_message_size: 512 * 1024, // 512KB 訊息上限
            max_frame_size: 256 * 1024,   // 256KB 幀上限
        }
    }

    /// 零拷貝解析訂單簿數據
    fn parse_orderbook_zero_copy(
        &mut self,
        json_bytes: &[u8],
        symbol: &Symbol,
    ) -> Result<MarketSnapshot, HftError> {
        #[cfg(feature = "json-simd")]
        let value = {
            // 使用 simd-json 借用模式解析
            self.parse_buffer.clear();
            self.parse_buffer.extend_from_slice(json_bytes);
            simd_json::to_borrowed_value(&mut self.parse_buffer).map_err(|e| HftError::Generic {
                message: format!("JSON 解析失败: {}", e),
            })?
        };

        #[cfg(not(feature = "json-simd"))]
        let value: serde_json::Value =
            serde_json::from_slice(json_bytes).map_err(|e| HftError::Generic {
                message: format!("JSON 解析失败: {}", e),
            })?;

        // 借用模式訪問，減少字符串分配
        let data = value
            .get("data")
            .and_then(|d| d.as_array()?.first())
            .ok_or_else(|| HftError::Generic {
                message: "無效的訂單簿數據".to_string(),
            })?;

        // 統一時間戳處理 - 區分交易所和本地時間戳
        let exchange_ts = data
            .get("ts")
            .and_then(|ts| ts.as_str())
            .and_then(|ts_str| ts_str.parse::<u64>().ok())
            .unwrap_or(0);

        let unified_ts = if exchange_ts > 0 {
            // 將毫秒轉為微秒並創建統一時間戳
            UnifiedTimestamp::auto(exchange_ts * 1000)
        } else {
            // 使用當前時間作為回退
            UnifiedTimestamp::default()
        };

        let mut bids = Vec::new();
        let mut asks = Vec::new();

        // 零拷貝處理 bids
        if let Some(bids_array) = data.get("bids").and_then(|b| b.as_array()) {
            for bid_item in bids_array {
                if let Some(bid_array) = bid_item.as_array() {
                    if bid_array.len() >= 3 {
                        let price_str = bid_array[0].as_str().unwrap_or("0");
                        let qty_str = bid_array[1].as_str().unwrap_or("0");

                        // 直接解析到 FixedPrice/FixedQuantity，避免 Decimal 中間轉換
                        if let (Ok(price), Ok(qty)) =
                            (price_str.parse::<f64>(), qty_str.parse::<f64>())
                        {
                            bids.push(BookLevel {
                                price: Price::from_f64(price).unwrap_or(Price::zero()),
                                quantity: Quantity::from_f64(qty).unwrap_or(Quantity::zero()),
                            });
                        }
                    }
                }
            }
        }

        // 零拷貝處理 asks
        if let Some(asks_array) = data.get("asks").and_then(|a| a.as_array()) {
            for ask_item in asks_array {
                if let Some(ask_array) = ask_item.as_array() {
                    if ask_array.len() >= 3 {
                        let price_str = ask_array[0].as_str().unwrap_or("0");
                        let qty_str = ask_array[1].as_str().unwrap_or("0");

                        if let (Ok(price), Ok(qty)) =
                            (price_str.parse::<f64>(), qty_str.parse::<f64>())
                        {
                            asks.push(BookLevel {
                                price: Price::from_f64(price).unwrap_or(Price::zero()),
                                quantity: Quantity::from_f64(qty).unwrap_or(Quantity::zero()),
                            });
                        }
                    }
                }
            }
        }

        // 按價格排序
        bids.sort_by(|a, b| b.price.cmp(&a.price)); // 降序
        asks.sort_by(|a, b| a.price.cmp(&b.price)); // 升序

        Ok(MarketSnapshot {
            symbol: symbol.clone(),
            timestamp: unified_ts.into(), // 向後兼容轉換
            bids,
            asks,
            sequence: 0, // Bitget 暫時沒有序列號
            source_venue: Some(VenueId::BITGET),
        })
    }

    /// 零拷貝解析成交數據
    fn parse_trade_zero_copy(&mut self, json_bytes: &[u8]) -> Result<Trade, HftError> {
        #[cfg(feature = "json-simd")]
        let value = {
            self.parse_buffer.clear();
            self.parse_buffer.extend_from_slice(json_bytes);
            simd_json::to_borrowed_value(&mut self.parse_buffer).map_err(|e| HftError::Generic {
                message: format!("JSON 解析失败: {}", e),
            })?
        };

        #[cfg(not(feature = "json-simd"))]
        let value: serde_json::Value =
            serde_json::from_slice(json_bytes).map_err(|e| HftError::Generic {
                message: format!("JSON 解析失败: {}", e),
            })?;

        let data = value
            .get("data")
            .and_then(|d| d.as_array()?.first())
            .ok_or_else(|| HftError::Generic {
                message: "無效的成交數據".to_string(),
            })?;

        let symbol_str = data.get("instId").and_then(|s| s.as_str()).unwrap_or("");

        let exchange_ts = data
            .get("ts")
            .and_then(|ts| ts.as_str())
            .and_then(|ts_str| ts_str.parse::<u64>().ok())
            .unwrap_or(0);

        let unified_ts = if exchange_ts > 0 {
            // 將毫秒轉為微秒並創建統一時間戳
            UnifiedTimestamp::auto(exchange_ts * 1000)
        } else {
            // 使用當前時間作為回退
            UnifiedTimestamp::default()
        };

        let price = data
            .get("px")
            .and_then(|p| p.as_str())
            .and_then(|p_str| p_str.parse::<f64>().ok())
            .unwrap_or(0.0);

        let quantity = data
            .get("sz")
            .and_then(|q| q.as_str())
            .and_then(|q_str| q_str.parse::<f64>().ok())
            .unwrap_or(0.0);

        let side = data
            .get("side")
            .and_then(|s| s.as_str())
            .map(|side_str| match side_str {
                "buy" => Side::Buy,
                "sell" => Side::Sell,
                _ => Side::Buy, // 默認
            })
            .unwrap_or(Side::Buy);

        let trade_id = data
            .get("tradeId")
            .and_then(|id| id.as_str())
            .unwrap_or("")
            .to_string();

        Ok(Trade {
            symbol: Symbol::new(symbol_str),
            timestamp: unified_ts.into(), // 向後兼容轉換
            price: Price::from_f64(price).unwrap_or(Price::zero()),
            quantity: Quantity::from_f64(quantity).unwrap_or(Quantity::zero()),
            side,
            trade_id,
            source_venue: Some(VenueId::BITGET),
        })
    }
}

/// 零拷貝消息處理器 - 通過 channel 發送事件
///
/// 不再直接依賴 EventIngester，通過 channel 解耦
pub struct ZeroCopyMessageHandler {
    event_sender: mpsc::UnboundedSender<MarketEvent>,
    subscribed_symbols: Vec<Symbol>,
    /// 復用緩衝區
    buffer: Vec<u8>,
}

impl ZeroCopyMessageHandler {
    pub fn new(sender: mpsc::UnboundedSender<MarketEvent>, symbols: Vec<Symbol>) -> Self {
        Self {
            event_sender: sender,
            subscribed_symbols: symbols,
            buffer: Vec::with_capacity(4096),
        }
    }

    /// 發送市場事件到 channel
    fn send_event(&self, event: MarketEvent) {
        if let Err(e) = self.event_sender.send(event) {
            warn!("事件發送失敗: {}", e);
        }
    }
}

impl MessageHandler for ZeroCopyMessageHandler {
    fn handle_connected(
        &mut self,
        _client: &mut integration::ws::WsClient,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        info!("零拷貝 Bitget WebSocket 連接成功");

        // 發送訂閱請求 - 簡化處理，暫時只記錄
        for symbol in &self.subscribed_symbols {
            debug!("訂閱 {} 的訂單簿數據", symbol.as_str());
        }

        Ok(())
    }

    fn handle_message(
        &mut self,
        message: String,
        mut metrics: WsFrameMetrics,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // 零拷貝解析 JSON - 將 buffer 聲明在函數級別以延長生命週期
        #[cfg(feature = "json-simd")]
        let mut buffer = message.into_bytes();

        #[cfg(feature = "json-simd")]
        let value = match simd_json::to_borrowed_value(&mut buffer) {
            Ok(v) => {
                metrics.mark_parsed();
                v
            }
            Err(e) => {
                metrics.mark_parsed();
                let parse_latency = metrics.parsed_at_us.saturating_sub(metrics.received_at_us);
                trace!("零拷貝解析失敗，耗時 {}μs", parse_latency);
                return Err(Box::new(e) as Box<dyn std::error::Error + Send + Sync>);
            }
        };

        #[cfg(not(feature = "json-simd"))]
        let value: serde_json::Value = match serde_json::from_str(&message) {
            Ok(v) => {
                metrics.mark_parsed();
                v
            }
            Err(e) => {
                metrics.mark_parsed();
                let parse_latency = metrics.parsed_at_us.saturating_sub(metrics.received_at_us);
                trace!("JSON 解析失敗，耗時 {}μs", parse_latency);
                return Err(Box::new(e) as Box<dyn std::error::Error + Send + Sync>);
            }
        };

        let parse_latency = metrics.parsed_at_us.saturating_sub(metrics.received_at_us);
        trace!("零拷貝解析完成，耗時 {}μs", parse_latency);
        let mut tracker = LatencyTracker::from_monotonic(metrics.received_at_us);
        tracker.record_stage_with_offset(LatencyStage::WsReceive, 0);
        tracker.record_stage_with_offset(LatencyStage::Parsing, parse_latency);

        #[cfg(feature = "metrics")]
        MetricsRegistry::global().record_parsing_latency(parse_latency as f64);

        // 檢查消息類型
        if let Some(arg) = value.get("arg") {
            let channel = arg.get("channel").and_then(|c| c.as_str()).unwrap_or("");
            let inst_id = arg.get("instId").and_then(|id| id.as_str()).unwrap_or("");

            match channel {
                "books15" => {
                    // 處理訂單簿快照
                    let symbol = Symbol::new(inst_id);

                    // 提取交易所時間戳並創建統一時間戳
                    let exchange_ts = value
                        .get("data")
                        .and_then(|data_array| data_array.as_array()?.first())
                        .and_then(|data_item| data_item.get("ts"))
                        .and_then(|ts| ts.as_str())
                        .and_then(|ts_str| ts_str.parse::<u64>().ok())
                        .unwrap_or(0);

                    let unified_ts = if exchange_ts > 0 {
                        // Bitget 時間戳通常是毫秒，轉為微秒
                        UnifiedTimestamp::auto(exchange_ts * 1000)
                    } else {
                        UnifiedTimestamp::default()
                    };

                    // 這裡需要實現完整的訂單簿解析
                    // 暫時創建一個最小快照用於演示
                    let snapshot = MarketSnapshot {
                        symbol: symbol.clone(),
                        timestamp: unified_ts.into(), // 向後兼容轉換
                        bids: Vec::new(),
                        asks: Vec::new(),
                        sequence: 0,
                        source_venue: Some(VenueId::BITGET),
                    };

                    let event = MarketEvent::Snapshot(snapshot);
                    self.send_event(event);
                }
                "trade" | "trades" => {
                    // 處理成交數據
                    if let Some(data_arr) = value.get("data").and_then(|d| d.as_array()) {
                        for item in data_arr.iter() {
                            let symbol_str = item
                                .get("instId")
                                .and_then(|v| v.as_str())
                                .unwrap_or(inst_id);
                            // 解析時間戳（毫秒 -> 微秒）
                            let exchange_ts = item
                                .get("ts")
                                .and_then(|ts| ts.as_str())
                                .and_then(|s| s.parse::<u64>().ok())
                                .or_else(|| item.get("ts").and_then(|v| v.as_u64()))
                                .unwrap_or(0);
                            let unified_ts = if exchange_ts > 0 {
                                UnifiedTimestamp::auto(exchange_ts.saturating_mul(1000))
                            } else {
                                UnifiedTimestamp::default()
                            };

                            // 解析價格/數量（優先字串，再嘗試數字）
                            let price = item
                                .get("px")
                                .and_then(|v| v.as_str())
                                .and_then(|s| s.parse::<f64>().ok())
                                .or_else(|| {
                                    item.get("price")
                                        .and_then(|v| v.as_str())
                                        .and_then(|s| s.parse::<f64>().ok())
                                })
                                .or_else(|| item.get("px").and_then(|v| v.as_f64()))
                                .or_else(|| item.get("price").and_then(|v| v.as_f64()))
                                .unwrap_or(0.0);
                            let qty = item
                                .get("sz")
                                .and_then(|v| v.as_str())
                                .and_then(|s| s.parse::<f64>().ok())
                                .or_else(|| {
                                    item.get("size")
                                        .and_then(|v| v.as_str())
                                        .and_then(|s| s.parse::<f64>().ok())
                                })
                                .or_else(|| item.get("sz").and_then(|v| v.as_f64()))
                                .or_else(|| item.get("size").and_then(|v| v.as_f64()))
                                .unwrap_or(0.0);

                            let side = item.get("side").and_then(|v| v.as_str()).unwrap_or("buy");
                            let side = match side {
                                "buy" | "BUY" => Side::Buy,
                                "sell" | "SELL" => Side::Sell,
                                _ => Side::Buy,
                            };
                            let trade_id = item
                                .get("tradeId")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();

                            let event = MarketEvent::Trade(Trade {
                                symbol: Symbol::new(symbol_str),
                                timestamp: unified_ts.into(),
                                price: Price::from_f64(price).unwrap_or(Price::zero()),
                                quantity: Quantity::from_f64(qty).unwrap_or(Quantity::zero()),
                                side,
                                trade_id,
                                source_venue: Some(VenueId::BITGET),
                            });
                            self.send_event(event);
                        }
                    }
                }
                _ => {
                    debug!("未知頻道: {}", channel);
                }
            }
        }

        Ok(())
    }

    fn handle_disconnect(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        warn!("零拷貝 Bitget WebSocket 連接中斷");

        let disconnect_event = MarketEvent::Disconnect {
            reason: "Connection lost".to_string(),
        };

        self.send_event(disconnect_event);
        Ok(())
    }
}

/// 零拷貝市場流實現
#[async_trait]
impl MarketStream for ZeroCopyBitgetStream {
    async fn health(&self) -> ConnectionHealth {
        // 簡單的健康檢查實現
        if self.event_sender.is_some() {
            ConnectionHealth {
                connected: true,
                latency_ms: None,
                last_heartbeat: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_micros() as u64,
            }
        } else {
            ConnectionHealth {
                connected: false,
                latency_ms: None,
                last_heartbeat: 0,
            }
        }
    }

    async fn connect(&mut self) -> Result<(), HftError> {
        // 連接邏輯，這裡簡化處理
        info!("零拷貝 Bitget 流連接中");
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), HftError> {
        // 斷開連接邏輯
        info!("零拷貝 Bitget 流斷開連接");
        self.ws_client = None;
        Ok(())
    }

    async fn subscribe(&self, symbols: Vec<Symbol>) -> HftResult<BoxStream<MarketEvent>> {
        info!("零拷貝 Bitget 流開始訂閱 {} 個符號", symbols.len());

        // 創建事件通道
        let (tx, mut rx) = mpsc::unbounded_channel();

        // 創建 WebSocket 客戶端
        let ws_config = Self::create_ws_config();
        let mut ws_client = ReconnectingWsClient::new(ws_config);

        // 創建零拷貝消息處理器
        let handler = ZeroCopyMessageHandler::new(tx, symbols.clone());

        // 在後台任務中運行 WebSocket 客戶端
        tokio::spawn(async move {
            if let Err(e) = ws_client.run_with_handler(handler).await {
                error!("零拷貝 Bitget WebSocket 客戶端錯誤: {}", e);
            }
        });

        // 創建事件流
        let stream = async_stream::stream! {
            while let Some(event) = rx.recv().await {
                yield Ok(event);
            }
        };

        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zero_copy_stream_creation() {
        let stream = ZeroCopyBitgetStream::new();
        assert!(stream.event_sender.is_none());
        assert_eq!(stream.subscribed_symbols.len(), 0);
        assert_eq!(stream.parse_buffer.capacity(), 4096);
    }

    #[tokio::test]
    async fn test_zero_copy_json_parsing() {
        let mut stream = ZeroCopyBitgetStream::new();
        let symbol = Symbol::new("BTCUSDT");

        // 模擬 Bitget 訂單簿 JSON 數據
        let json_data = r#"
        {
            "data": [{
                "asks": [["67000.5", "0.1", 1], ["67001.0", "0.2", 1]],
                "bids": [["66999.5", "0.15", 1], ["66999.0", "0.25", 1]],
                "ts": "1638360000000"
            }]
        }
        "#;

        let result = stream.parse_orderbook_zero_copy(json_data.as_bytes(), &symbol);
        assert!(result.is_ok());

        let snapshot = result.unwrap();
        assert_eq!(snapshot.symbol.as_str(), "BTCUSDT");
        // Timestamp is converted from milliseconds to microseconds (multiply by 1000)
        assert_eq!(snapshot.timestamp, 1638360000000 * 1000);
        assert_eq!(snapshot.bids.len(), 2);
        assert_eq!(snapshot.asks.len(), 2);
    }
}
