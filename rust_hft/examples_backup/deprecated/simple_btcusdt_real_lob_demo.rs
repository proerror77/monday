/*!
 * Simple BTCUSDT Real LOB Data Processing Demo
 * 
 * 專注於單商品的真實LOB數據處理和特徵工程：
 * 1. Bitget V2 BTCUSDT WebSocket連接
 * 2. 實時LOB數據解析和標準化
 * 3. 基本特徵工程提取
 * 4. DL模型推理測試
 */

use rust_hft::{
    integrations::bitget_connector::*,
    ml::{
        lob_time_series_extractor::{LobTimeSeriesExtractor, LobTimeSeriesConfig},
        dl_trend_predictor::{DlTrendPredictor, DlTrendPredictorConfig, TrendClass},
    },
    core::{types::*, orderbook::OrderBook},
};
use anyhow::Result;
use tracing::{info, warn, error};
use std::sync::{Arc, Mutex};
use std::collections::VecDeque;
use tokio::time::Duration;
use serde_json::Value;

/// 真實LOB數據處理器
#[derive(Debug)]
pub struct RealLobProcessor {
    /// 訂單簿歷史記錄
    orderbook_history: Arc<Mutex<VecDeque<OrderBook>>>,
    
    /// 特徵提取器
    feature_extractor: LobTimeSeriesExtractor,
    
    /// DL預測器
    dl_predictor: DlTrendPredictor,
    
    /// 統計信息
    stats: Arc<Mutex<ProcessorStats>>,
}

#[derive(Debug, Default)]
pub struct ProcessorStats {
    pub total_updates: u64,
    pub feature_extractions: u64,
    pub dl_predictions: u64,
    pub valid_predictions: u64,
    pub last_price: f64,
    pub spread_bps: f64,
    pub trading_signals: u64,
    pub buy_signals: u64,
    pub sell_signals: u64,
    pub strong_signals: u64,
}

/// 簡化的交易信號
#[derive(Debug, Clone)]
pub struct TradingSignal {
    pub timestamp: u64,
    pub symbol: String,
    pub direction: TradeDirection,
    pub confidence: f64,
    pub predicted_change_bps: f64,
    pub current_price: f64,
    pub reasoning: String,
}

#[derive(Debug, Clone)]
pub enum TradeDirection {
    Buy,
    Sell,
    Hold,
}

impl RealLobProcessor {
    pub fn new() -> Result<Self> {
        // 使用默認配置
        let lob_config = LobTimeSeriesConfig::default();
        let feature_extractor = LobTimeSeriesExtractor::new("BTCUSDT".to_string(), lob_config.clone());

        // 使用默認DL預測器配置
        let dl_predictor_config = DlTrendPredictorConfig::default();
        let dl_predictor = DlTrendPredictor::new("BTCUSDT".to_string(), dl_predictor_config, lob_config)?;

        Ok(Self {
            orderbook_history: Arc::new(Mutex::new(VecDeque::with_capacity(100))),
            feature_extractor,
            dl_predictor,
            stats: Arc::new(Mutex::new(ProcessorStats::default())),
        })
    }

    /// 處理Bitget LOB數據
    pub fn process_bitget_orderbook(&mut self, symbol: &str, data: &Value, timestamp: u64) -> Result<()> {
        if symbol != "BTCUSDT" {
            return Ok(());
        }

        // 解析Bitget LOB數據格式
        let orderbook = self.parse_bitget_lob_data(data, timestamp)?;
        
        // 計算最佳買賣價
        let best_bid = orderbook.bids.keys().next_back().map(|p| p.0).unwrap_or(0.0);
        let best_ask = orderbook.asks.keys().next().map(|p| p.0).unwrap_or(0.0);
        
        // 更新統計
        {
            let mut stats = self.stats.lock().unwrap();
            stats.total_updates += 1;
            if best_bid > 0.0 && best_ask > 0.0 {
                stats.last_price = (best_bid + best_ask) / 2.0;
                stats.spread_bps = ((best_ask - best_bid) / stats.last_price * 10000.0);
            }
        }

        // 添加到歷史記錄
        {
            let mut history = self.orderbook_history.lock().unwrap();
            history.push_back(orderbook.clone());
            
            // 保持最新的100個記錄
            if history.len() > 100 {
                history.pop_front();
            }
        }

        // 嘗試進行特徵提取和預測
        if self.orderbook_history.lock().unwrap().len() >= 10 {
            self.try_feature_extraction_and_prediction()?;
        }

        Ok(())
    }

    /// 解析Bitget LOB數據
    fn parse_bitget_lob_data(&self, data: &Value, timestamp: u64) -> Result<OrderBook> {
        let data_array = data.as_array()
            .ok_or_else(|| anyhow::anyhow!("Invalid data format"))?;
        
        let first_item = data_array.first()
            .ok_or_else(|| anyhow::anyhow!("Empty data array"))?;

        // 解析bids
        let mut bids = Vec::new();
        if let Some(bid_data) = first_item.get("bids").and_then(|v| v.as_array()) {
            for bid in bid_data.iter().take(5) { // 只取前5層
                if let Some(bid_array) = bid.as_array() {
                    if bid_array.len() >= 2 {
                        let price: f64 = bid_array[0].as_str().unwrap_or("0").parse().unwrap_or(0.0);
                        let qty: f64 = bid_array[1].as_str().unwrap_or("0").parse().unwrap_or(0.0);
                        bids.push((price, qty));
                    }
                }
            }
        }

        // 解析asks
        let mut asks = Vec::new();
        if let Some(ask_data) = first_item.get("asks").and_then(|v| v.as_array()) {
            for ask in ask_data.iter().take(5) { // 只取前5層
                if let Some(ask_array) = ask.as_array() {
                    if ask_array.len() >= 2 {
                        let price: f64 = ask_array[0].as_str().unwrap_or("0").parse().unwrap_or(0.0);
                        let qty: f64 = ask_array[1].as_str().unwrap_or("0").parse().unwrap_or(0.0);
                        asks.push((price, qty));
                    }
                }
            }
        }

        // 創建簡化的OrderBook結構（兼容現有系統）
        let mut orderbook = OrderBook::new("BTCUSDT".to_string());
        
        // 添加bids和asks數據（使用BTreeMap格式）
        for (price, qty) in bids {
            if price > 0.0 && qty > 0.0 {
                use ordered_float::OrderedFloat;
                use rust_decimal::Decimal;
                orderbook.bids.insert(OrderedFloat(price), Decimal::from_f64_retain(qty).unwrap_or_default());
            }
        }
        
        for (price, qty) in asks {
            if price > 0.0 && qty > 0.0 {
                use ordered_float::OrderedFloat;
                use rust_decimal::Decimal;
                orderbook.asks.insert(OrderedFloat(price), Decimal::from_f64_retain(qty).unwrap_or_default());
            }
        }
        
        orderbook.last_update = timestamp;
        
        // 標記為有效並計算數據質量分數
        if !orderbook.bids.is_empty() && !orderbook.asks.is_empty() {
            orderbook.is_valid = true;
            orderbook.data_quality_score = 1.0;
            orderbook.update_count += 1;
        }
        
        Ok(orderbook)
    }

    /// 嘗試特徵提取和DL預測
    fn try_feature_extraction_and_prediction(&mut self) -> Result<()> {
        let history = self.orderbook_history.lock().unwrap();
        if history.len() < 10 {
            return Ok(());
        }

        // 使用最新的OrderBook進行特徵提取
        if let Some(latest_orderbook) = history.back() {
            // 特徵提取
            match self.feature_extractor.extract_snapshot(latest_orderbook, 100) {
                Ok(_snapshot) => {
                    {
                        let mut stats = self.stats.lock().unwrap();
                        stats.feature_extractions += 1;
                    }

                    // DL推理
                    match self.dl_predictor.predict_trend(&self.feature_extractor) {
                        Ok(Some(prediction)) => {
                            {
                                let mut stats = self.stats.lock().unwrap();
                                stats.dl_predictions += 1;
                                stats.valid_predictions += 1;
                                
                                info!("🎯 DL Prediction: {:?} (confidence: {:.3})", 
                                     prediction.trend_class, prediction.confidence);
                            }
                            
                            // 生成交易信號
                            if let Some(signal) = self.generate_trading_signal(&prediction, latest_orderbook) {
                                self.process_trading_signal(signal);
                            }
                        }
                        Ok(None) => {
                            // 沒有足夠數據進行預測
                        }
                        Err(e) => {
                            warn!("DL prediction failed: {}", e);
                        }
                    }
                }
                Err(e) => {
                    warn!("Feature extraction failed: {}", e);
                }
            }
        }

        Ok(())
    }

    /// 生成交易信號
    fn generate_trading_signal(&self, prediction: &rust_hft::ml::dl_trend_predictor::TrendPrediction, orderbook: &OrderBook) -> Option<TradingSignal> {
        // 計算當前價格
        let best_bid = orderbook.bids.keys().next_back().map(|p| p.0).unwrap_or(0.0);
        let best_ask = orderbook.asks.keys().next().map(|p| p.0).unwrap_or(0.0);
        let current_price = (best_bid + best_ask) / 2.0;
        
        // 設置信號閾值
        let min_confidence = 0.62; // 最低信心閾值
        let strong_confidence = 0.70; // 強信號閾值
        
        if prediction.confidence < min_confidence {
            return None;
        }
        
        let (direction, reasoning) = match prediction.trend_class {
            TrendClass::StrongUp => {
                (TradeDirection::Buy, format!("Strong uptrend predicted, confidence: {:.3}", prediction.confidence))
            }
            TrendClass::WeakUp => {
                if prediction.confidence > strong_confidence {
                    (TradeDirection::Buy, format!("Weak uptrend with high confidence: {:.3}", prediction.confidence))
                } else {
                    (TradeDirection::Hold, format!("Weak uptrend, insufficient confidence: {:.3}", prediction.confidence))
                }
            }
            TrendClass::StrongDown => {
                (TradeDirection::Sell, format!("Strong downtrend predicted, confidence: {:.3}", prediction.confidence))
            }
            TrendClass::WeakDown => {
                if prediction.confidence > strong_confidence {
                    (TradeDirection::Sell, format!("Weak downtrend with high confidence: {:.3}", prediction.confidence))
                } else {
                    (TradeDirection::Hold, format!("Weak downtrend, insufficient confidence: {:.3}", prediction.confidence))
                }
            }
            TrendClass::Neutral => {
                (TradeDirection::Hold, format!("Neutral trend, confidence: {:.3}", prediction.confidence))
            }
        };
        
        // 只生成Buy和Sell信號，過濾Hold
        match direction {
            TradeDirection::Hold => None,
            _ => Some(TradingSignal {
                timestamp: now_micros(),
                symbol: "BTCUSDT".to_string(),
                direction,
                confidence: prediction.confidence,
                predicted_change_bps: prediction.expected_change_bps,
                current_price,
                reasoning,
            })
        }
    }
    
    /// 處理交易信號
    fn process_trading_signal(&self, signal: TradingSignal) {
        {
            let mut stats = self.stats.lock().unwrap();
            stats.trading_signals += 1;
            
            match signal.direction {
                TradeDirection::Buy => {
                    stats.buy_signals += 1;
                    if signal.confidence > 0.70 {
                        stats.strong_signals += 1;
                    }
                }
                TradeDirection::Sell => {
                    stats.sell_signals += 1;
                    if signal.confidence > 0.70 {
                        stats.strong_signals += 1;
                    }
                }
                TradeDirection::Hold => {}
            }
        }
        
        // 記錄交易信號
        info!("🚀 Trading Signal: {:?} | Confidence: {:.3} | Price: {:.2} | Change: {:.1}bps", 
             signal.direction, signal.confidence, signal.current_price, signal.predicted_change_bps);
        info!("   Reasoning: {}", signal.reasoning);
        
        // 這裡可以添加實際的交易執行邏輯
        self.simulate_trade_execution(&signal);
    }
    
    /// 模擬交易執行
    fn simulate_trade_execution(&self, signal: &TradingSignal) {
        // 簡單的執行模擬
        let position_size = self.calculate_position_size(signal);
        let estimated_fee = position_size * 0.0004; // 0.04% 手續費
        
        info!("📈 Simulated Trade Execution:");
        info!("   Direction: {:?}", signal.direction);
        info!("   Position Size: {:.2} USDT", position_size);
        info!("   Entry Price: {:.2}", signal.current_price);
        info!("   Estimated Fee: {:.4} USDT", estimated_fee);
        info!("   Expected P&L: {:.2} USDT", position_size * signal.predicted_change_bps / 10000.0);
    }
    
    /// 計算倉位大小
    fn calculate_position_size(&self, signal: &TradingSignal) -> f64 {
        // 基於Kelly公式的簡化版本
        let base_capital = 400.0; // 400 USDT總資金
        let max_risk_per_trade = 0.02; // 2%風險
        let kelly_fraction = (signal.confidence - 0.5) * 2.0; // 轉換為Kelly分數
        
        let position_size = base_capital * max_risk_per_trade * kelly_fraction;
        position_size.max(10.0).min(50.0) // 限制在10-50 USDT之間
    }

    /// 獲取處理器統計信息
    pub fn get_stats(&self) -> ProcessorStats {
        self.stats.lock().unwrap().clone()
    }
}

impl Clone for ProcessorStats {
    fn clone(&self) -> Self {
        Self {
            total_updates: self.total_updates,
            feature_extractions: self.feature_extractions,
            dl_predictions: self.dl_predictions,
            valid_predictions: self.valid_predictions,
            last_price: self.last_price,
            spread_bps: self.spread_bps,
            trading_signals: self.trading_signals,
            buy_signals: self.buy_signals,
            sell_signals: self.sell_signals,
            strong_signals: self.strong_signals,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日誌
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("🚀 Starting Simple BTCUSDT Real LOB Processing Demo");

    // 創建LOB處理器
    let mut lob_processor = RealLobProcessor::new()?;
    info!("✅ LOB processor initialized");

    // 創建Bitget配置
    let config = BitgetConfig {
        public_ws_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
        private_ws_url: "wss://ws.bitget.com/v2/ws/private".to_string(),
        timeout_seconds: 10,
        auto_reconnect: true,
        max_reconnect_attempts: 3,
    };

    // 創建連接器
    let mut connector = BitgetConnector::new(config);
    connector.add_subscription("BTCUSDT".to_string(), BitgetChannel::Books5);

    info!("📊 Subscribed to BTCUSDT books5 channel");

    // 統計計數器
    let stats_counter = Arc::new(Mutex::new(0u64));
    let start_time = std::time::Instant::now();

    // 克隆處理器用於消息處理
    let processor_arc = Arc::new(Mutex::new(lob_processor));
    let processor_clone = processor_arc.clone();
    let stats_clone = stats_counter.clone();

    // 創建消息處理器
    let message_handler = move |message: BitgetMessage| {
        match message {
            BitgetMessage::OrderBook { symbol, data, timestamp, .. } => {
                // 處理LOB數據
                if let Ok(mut processor) = processor_clone.lock() {
                    if let Err(e) = processor.process_bitget_orderbook(&symbol, &data, timestamp) {
                        error!("LOB processing error: {}", e);
                    }
                }

                // 更新統計
                {
                    let mut count = stats_clone.lock().unwrap();
                    *count += 1;

                    // 每50次更新顯示統計信息
                    if *count % 50 == 0 {
                        let elapsed = start_time.elapsed();
                        let rate = *count as f64 / elapsed.as_secs_f64();
                        
                        if let Ok(processor) = processor_clone.lock() {
                            let stats = processor.get_stats();
                            info!("📊 Updates: {}, Rate: {:.1}/s, Price: {:.2}, Spread: {:.1}bps", 
                                 *count, rate, stats.last_price, stats.spread_bps);
                            info!("   Features: {}, DL Preds: {}, Signals: {} (Buy:{}, Sell:{}, Strong:{})", 
                                 stats.feature_extractions, stats.dl_predictions, stats.trading_signals,
                                 stats.buy_signals, stats.sell_signals, stats.strong_signals);
                        }
                    }
                }
            }
            _ => {}
        }
    };

    info!("🔌 Connecting to Bitget BTCUSDT LOB stream...");

    // 設置運行時間限制
    let timeout = Duration::from_secs(60); // 1分鐘測試

    // 啟動連接
    match tokio::time::timeout(timeout, connector.connect_public(message_handler)).await {
        Ok(Ok(_)) => {
            info!("✅ LOB processing completed successfully");
        }
        Ok(Err(e)) => {
            error!("❌ Connection failed: {}", e);
            return Err(e);
        }
        Err(_) => {
            info!("⏰ Demo completed after 1 minute");
        }
    }

    // 最終統計
    if let Ok(processor) = processor_arc.lock() {
        let stats = processor.get_stats();
        let final_count = *stats_counter.lock().unwrap();
        let elapsed = start_time.elapsed();
        let rate = final_count as f64 / elapsed.as_secs_f64();

        info!("🏁 Final Statistics:");
        info!("   Total LOB updates: {}", final_count);
        info!("   Average rate: {:.1} updates/sec", rate);
        info!("   Feature extractions: {}", stats.feature_extractions);
        info!("   DL predictions: {}", stats.dl_predictions);
        info!("   Trading signals: {} (Buy:{}, Sell:{}, Strong:{})", 
             stats.trading_signals, stats.buy_signals, stats.sell_signals, stats.strong_signals);
        info!("   Signal rate: {:.1}%", 
             stats.trading_signals as f64 / stats.dl_predictions.max(1) as f64 * 100.0);
        info!("   Last price: {:.2} USDT", stats.last_price);
        info!("   Last spread: {:.1} bps", stats.spread_bps);
    }

    Ok(())
}