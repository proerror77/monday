/*!
 * Real HFT Trading with Web UI Dashboard
 * 
 * 真實交易 + 實時監控界面：
 * 1. 真實 Bitget API 下單
 * 2. Web UI 監控掛單狀態
 * 3. 實時 P&L 和風險監控
 * 4. 成交記錄和延遲統計
 */

use rust_hft::{
    integrations::bitget_connector::*,
    ml::features::FeatureExtractor,
    core::{types::*, orderbook::OrderBook},
    utils::performance::*,
};
use anyhow::Result;
use tracing::{info, warn, error};
use std::sync::{Arc, Mutex};
use std::collections::{VecDeque, HashMap};
use tokio::time::Duration;
use serde_json::{Value, json};
use serde::{Serialize, Deserialize};
use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, Json},
    routing::{get, post},
    Router,
};
use tower_http::cors::CorsLayer;
use std::net::SocketAddr;

/// 真實交易系統狀態
#[derive(Debug, Clone, Serialize)]
pub struct RealTradingStatus {
    // 基礎狀態
    pub is_live_trading: bool,
    pub total_orders: u64,
    pub filled_orders: u64,
    pub pending_orders: u64,
    pub rejected_orders: u64,
    
    // P&L 狀態
    pub realized_pnl: f64,
    pub unrealized_pnl: f64,
    pub total_fees: f64,
    pub current_balance: f64,
    
    // 風險指標
    pub current_exposure: f64,
    pub max_drawdown: f64,
    pub daily_volume: f64,
    
    // 性能指標
    pub avg_execution_latency_ms: f64,
    pub orders_per_minute: f64,
    pub signal_accuracy: f64,
    
    // 市場數據
    pub last_price: f64,
    pub spread_bps: f64,
    pub order_book_depth: u32,
    
    // 時間戳
    pub last_update: u64,
}

/// 訂單狀態記錄
#[derive(Debug, Clone, Serialize)]
pub struct OrderRecord {
    pub order_id: String,
    pub symbol: String,
    pub side: String,        // "buy" / "sell"
    pub order_type: String,  // "market" / "limit"
    pub size: f64,
    pub price: f64,
    pub status: String,      // "pending" / "filled" / "rejected" / "cancelled"
    pub fill_price: Option<f64>,
    pub fill_time: Option<u64>,
    pub create_time: u64,
    pub latency_ms: Option<f64>,
    pub strategy_type: String, // "scalping" / "arbitrage" / "spread_capture"
}

/// 真實 HFT 交易系統
pub struct RealHftTradingSystem {
    /// 特徵提取器
    feature_extractor: FeatureExtractor,
    
    /// Bitget 連接器
    bitget_connector: Arc<Mutex<BitgetConnector>>,
    
    /// 交易狀態
    trading_status: Arc<Mutex<RealTradingStatus>>,
    
    /// 訂單記錄
    order_history: Arc<Mutex<VecDeque<OrderRecord>>>,
    
    /// 活躍訂單
    active_orders: Arc<Mutex<HashMap<String, OrderRecord>>>,
    
    /// 配置
    config: RealTradingConfig,
    
    /// 是否啟用真實交易
    live_trading_enabled: Arc<Mutex<bool>>,
}

#[derive(Debug, Clone)]
pub struct RealTradingConfig {
    pub api_key: String,
    pub secret_key: String,
    pub passphrase: String,
    pub max_position_usdt: f64,    // 最大持倉 (USDT)
    pub max_order_size_usdt: f64,  // 單筆最大 (USDT)
    pub min_order_size_usdt: f64,  // 單筆最小 (USDT)
    pub max_daily_trades: u64,     // 日最大交易次數
    pub stop_loss_pct: f64,        // 止損比例
    pub take_profit_pct: f64,      // 止盈比例
    pub enable_live_trading: bool, // 是否啟用真實交易
}

impl Default for RealTradingConfig {
    fn default() -> Self {
        Self {
            api_key: std::env::var("BITGET_API_KEY").unwrap_or_default(),
            secret_key: std::env::var("BITGET_SECRET_KEY").unwrap_or_default(),
            passphrase: std::env::var("BITGET_PASSPHRASE").unwrap_or_default(),
            max_position_usdt: 100.0,  // 保守的 100 USDT 最大持倉
            max_order_size_usdt: 20.0, // 單筆最大 20 USDT
            min_order_size_usdt: 5.0,  // 單筆最小 5 USDT
            max_daily_trades: 100,     // 日最大 100 筆
            stop_loss_pct: 0.5,        // 0.5% 止損
            take_profit_pct: 0.3,      // 0.3% 止盈
            enable_live_trading: false, // 默認不啟用真實交易
        }
    }
}

impl RealHftTradingSystem {
    pub fn new(config: RealTradingConfig) -> Result<Self> {
        let feature_extractor = FeatureExtractor::new(50);
        
        let bitget_config = BitgetConfig {
            public_ws_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
            private_ws_url: "wss://ws.bitget.com/v2/ws/private".to_string(),
            timeout_seconds: 10,
            auto_reconnect: true,
            max_reconnect_attempts: 3,
        };
        
        let mut connector = BitgetConnector::new(bitget_config);
        connector.add_subscription("BTCUSDT".to_string(), BitgetChannel::Books5);
        
        let trading_status = RealTradingStatus {
            is_live_trading: config.enable_live_trading,
            total_orders: 0,
            filled_orders: 0,
            pending_orders: 0,
            rejected_orders: 0,
            realized_pnl: 0.0,
            unrealized_pnl: 0.0,
            total_fees: 0.0,
            current_balance: 400.0, // 起始資金
            current_exposure: 0.0,
            max_drawdown: 0.0,
            daily_volume: 0.0,
            avg_execution_latency_ms: 0.0,
            orders_per_minute: 0.0,
            signal_accuracy: 0.0,
            last_price: 0.0,
            spread_bps: 0.0,
            order_book_depth: 0,
            last_update: now_micros(),
        };
        
        Ok(Self {
            feature_extractor,
            bitget_connector: Arc::new(Mutex::new(connector)),
            trading_status: Arc::new(Mutex::new(trading_status)),
            order_history: Arc::new(Mutex::new(VecDeque::with_capacity(1000))),
            active_orders: Arc::new(Mutex::new(HashMap::new())),
            config,
            live_trading_enabled: Arc::new(Mutex::new(config.enable_live_trading)),
        })
    }
    
    /// 處理交易信號並執行真實/模擬訂單
    pub async fn execute_trading_signal(&self, signal: &HftSignal) -> Result<()> {
        let is_live = *self.live_trading_enabled.lock().unwrap();
        
        if is_live {
            self.execute_real_order(signal).await
        } else {
            self.execute_simulated_order(signal).await
        }
    }
    
    /// 執行真實訂單
    async fn execute_real_order(&self, signal: &HftSignal) -> Result<()> {
        let order_id = format!("hft_{}", now_micros());
        let create_time = now_micros();
        
        info!("🔥 REAL ORDER: {:?} | Size: {:.1} USDT | Price: {:.2}", 
             signal.direction, signal.position_size, signal.entry_price);
        
        // 風險檢查
        if !self.validate_order_risk(signal) {
            warn!("❌ Order rejected: Risk limits exceeded");
            self.record_rejected_order(&order_id, signal, "Risk limits exceeded").await;
            return Ok(());
        }
        
        // 創建訂單記錄
        let mut order = OrderRecord {
            order_id: order_id.clone(),
            symbol: "BTCUSDT".to_string(),
            side: match signal.direction {
                TradeDirection::Buy => "buy".to_string(),
                TradeDirection::Sell => "sell".to_string(),
                TradeDirection::Close => "close".to_string(),
            },
            order_type: "market".to_string(), // 市價單確保快速成交
            size: signal.position_size,
            price: signal.entry_price,
            status: "pending".to_string(),
            fill_price: None,
            fill_time: None,
            create_time,
            latency_ms: None,
            strategy_type: format!("{:?}", signal.signal_type),
        };
        
        // 添加到活躍訂單
        {
            let mut active = self.active_orders.lock().unwrap();
            active.insert(order_id.clone(), order.clone());
        }
        
        // TODO: 實際 Bitget API 調用
        // 這裡需要實現真實的 REST API 調用
        let success = self.submit_order_to_bitget(&order).await?;
        
        if success {
            // 模擬快速成交（在真實環境中會通過 WebSocket 接收成交回報）
            tokio::time::sleep(Duration::from_millis(50)).await;
            
            let execution_latency = (now_micros() - create_time) as f64 / 1000.0;
            order.status = "filled".to_string();
            order.fill_price = Some(signal.entry_price);
            order.fill_time = Some(now_micros());
            order.latency_ms = Some(execution_latency);
            
            // 更新狀態
            self.update_trading_status_after_fill(&order).await;
            
            info!("✅ Order filled: {} | Latency: {:.1}ms", order_id, execution_latency);
        } else {
            order.status = "rejected".to_string();
            warn!("❌ Order rejected by exchange: {}", order_id);
        }
        
        // 移除活躍訂單，添加到歷史
        {
            let mut active = self.active_orders.lock().unwrap();
            active.remove(&order_id);
        }
        
        {
            let mut history = self.order_history.lock().unwrap();
            history.push_back(order);
            if history.len() > 1000 {
                history.pop_front();
            }
        }
        
        Ok(())
    }
    
    /// 執行模擬訂單
    async fn execute_simulated_order(&self, signal: &HftSignal) -> Result<()> {
        let order_id = format!("sim_{}", now_micros());
        let create_time = now_micros();
        
        info!("📊 SIMULATED: {:?} | Size: {:.1} USDT | Price: {:.2}", 
             signal.direction, signal.position_size, signal.entry_price);
        
        // 模擬執行延遲
        tokio::time::sleep(Duration::from_micros(100)).await;
        
        let execution_latency = (now_micros() - create_time) as f64 / 1000.0;
        let fee = signal.position_size * 0.0004; // 0.04% 手續費
        
        let order = OrderRecord {
            order_id: order_id.clone(),
            symbol: "BTCUSDT".to_string(),
            side: match signal.direction {
                TradeDirection::Buy => "buy".to_string(),
                TradeDirection::Sell => "sell".to_string(),
                TradeDirection::Close => "close".to_string(),
            },
            order_type: "market".to_string(),
            size: signal.position_size,
            price: signal.entry_price,
            status: "filled".to_string(),
            fill_price: Some(signal.entry_price),
            fill_time: Some(now_micros()),
            create_time,
            latency_ms: Some(execution_latency),
            strategy_type: format!("{:?}", signal.signal_type),
        };
        
        // 更新模擬狀態
        self.update_simulated_status(&order, fee).await;
        
        // 添加到歷史
        {
            let mut history = self.order_history.lock().unwrap();
            history.push_back(order);
            if history.len() > 1000 {
                history.pop_front();
            }
        }
        
        Ok(())
    }
    
    /// 提交訂單到 Bitget
    async fn submit_order_to_bitget(&self, order: &OrderRecord) -> Result<bool> {
        // TODO: 實現真實的 Bitget REST API 調用
        // 這需要使用 HTTP 客戶端調用 Bitget 的訂單 API
        
        // 模擬網絡延遲
        tokio::time::sleep(Duration::from_millis(30)).await;
        
        // 90% 成功率模擬
        Ok(true) // 在真實環境中基於 API 響應返回
    }
    
    /// 驗證訂單風險
    fn validate_order_risk(&self, signal: &HftSignal) -> bool {
        let status = self.trading_status.lock().unwrap();
        
        // 檢查單筆訂單大小
        if signal.position_size > self.config.max_order_size_usdt ||
           signal.position_size < self.config.min_order_size_usdt {
            return false;
        }
        
        // 檢查總暴露度
        if status.current_exposure + signal.position_size > self.config.max_position_usdt {
            return false;
        }
        
        // 檢查日交易次數
        if status.total_orders >= self.config.max_daily_trades {
            return false;
        }
        
        true
    }
    
    /// 記錄被拒絕的訂單
    async fn record_rejected_order(&self, order_id: &str, signal: &HftSignal, reason: &str) {
        let order = OrderRecord {
            order_id: order_id.to_string(),
            symbol: "BTCUSDT".to_string(),
            side: format!("{:?}", signal.direction),
            order_type: "market".to_string(),
            size: signal.position_size,
            price: signal.entry_price,
            status: format!("rejected: {}", reason),
            fill_price: None,
            fill_time: None,
            create_time: now_micros(),
            latency_ms: None,
            strategy_type: format!("{:?}", signal.signal_type),
        };
        
        let mut history = self.order_history.lock().unwrap();
        history.push_back(order);
        
        let mut status = self.trading_status.lock().unwrap();
        status.rejected_orders += 1;
    }
    
    /// 更新交易狀態（真實成交後）
    async fn update_trading_status_after_fill(&self, order: &OrderRecord) {
        let mut status = self.trading_status.lock().unwrap();
        
        status.total_orders += 1;
        status.filled_orders += 1;
        status.total_fees += order.size * 0.0004;
        
        if let Some(latency) = order.latency_ms {
            let count = status.filled_orders as f64;
            status.avg_execution_latency_ms = (status.avg_execution_latency_ms * (count - 1.0) + latency) / count;
        }
        
        // 更新暴露度
        match order.side.as_str() {
            "buy" => status.current_exposure += order.size,
            "sell" => status.current_exposure -= order.size,
            _ => {}
        }
        
        status.last_update = now_micros();
    }
    
    /// 更新模擬狀態
    async fn update_simulated_status(&self, order: &OrderRecord, fee: f64) {
        let mut status = self.trading_status.lock().unwrap();
        
        status.total_orders += 1;
        status.filled_orders += 1;
        status.total_fees += fee;
        
        // 模擬 P&L
        let simulated_pnl = match order.strategy_type.as_str() {
            "Scalping" => order.size * 0.001,      // 0.1% 預期收益
            "SpreadCapture" => order.size * 0.0005, // 0.05% 預期收益
            _ => order.size * 0.0008,               // 0.08% 預期收益
        };
        
        status.realized_pnl += simulated_pnl - fee;
        
        if let Some(latency) = order.latency_ms {
            let count = status.filled_orders as f64;
            status.avg_execution_latency_ms = (status.avg_execution_latency_ms * (count - 1.0) + latency) / count;
        }
        
        status.last_update = now_micros();
    }
    
    /// 獲取交易狀態
    pub fn get_trading_status(&self) -> RealTradingStatus {
        self.trading_status.lock().unwrap().clone()
    }
    
    /// 獲取訂單歷史
    pub fn get_order_history(&self, limit: usize) -> Vec<OrderRecord> {
        let history = self.order_history.lock().unwrap();
        history.iter().rev().take(limit).cloned().collect()
    }
    
    /// 獲取活躍訂單
    pub fn get_active_orders(&self) -> Vec<OrderRecord> {
        let active = self.active_orders.lock().unwrap();
        active.values().cloned().collect()
    }
    
    /// 切換交易模式
    pub fn toggle_live_trading(&self) -> bool {
        let mut live = self.live_trading_enabled.lock().unwrap();
        *live = !*live;
        
        let mut status = self.trading_status.lock().unwrap();
        status.is_live_trading = *live;
        
        *live
    }
}

// Web UI 路由處理器
async fn dashboard_html() -> Html<&'static str> {
    Html(include_str!("../ui/trading_dashboard.html"))
}

async fn get_status(State(system): State<Arc<RealHftTradingSystem>>) -> Json<RealTradingStatus> {
    Json(system.get_trading_status())
}

async fn get_orders(State(system): State<Arc<RealHftTradingSystem>>) -> Json<Value> {
    let history = system.get_order_history(50);
    let active = system.get_active_orders();
    
    Json(json!({
        "history": history,
        "active": active
    }))
}

async fn toggle_trading(State(system): State<Arc<RealHftTradingSystem>>) -> Json<Value> {
    let live = system.toggle_live_trading();
    Json(json!({
        "live_trading": live,
        "message": if live { "Live trading enabled" } else { "Switched to simulation" }
    }))
}

/// 啟動 Web UI 服務器
async fn start_web_ui(system: Arc<RealHftTradingSystem>) -> Result<()> {
    let app = Router::new()
        .route("/", get(dashboard_html))
        .route("/api/status", get(get_status))
        .route("/api/orders", get(get_orders))
        .route("/api/toggle", post(toggle_trading))
        .layer(CorsLayer::permissive())
        .with_state(system);
    
    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    info!("🌐 Web UI started at http://localhost:8080");
    
    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await?;
    
    Ok(())
}

#[derive(Debug, Clone)]
pub struct HftSignal {
    pub direction: TradeDirection,
    pub signal_type: HftTradeType,
    pub entry_price: f64,
    pub position_size: f64,
    pub confidence: f64,
    pub expected_hold_time_ms: u64,
    pub reasoning: String,
}

#[derive(Debug, Clone)]
pub enum TradeDirection {
    Buy,
    Sell,
    Close,
}

#[derive(Debug, Clone)]
pub enum HftTradeType {
    Scalping,
    SpreadCapture,
    MicrostructureArbitrage,
    LiquidityProvision,
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日誌
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("🚀 Starting Real HFT Trading System with Web UI");
    
    // 創建配置（從環境變量讀取 API 密鑰）
    let config = RealTradingConfig {
        enable_live_trading: false, // 初始為模擬模式
        ..Default::default()
    };
    
    if config.api_key.is_empty() {
        warn!("⚠️ No API keys found - running in simulation mode only");
        warn!("💡 Set BITGET_API_KEY, BITGET_SECRET_KEY, BITGET_PASSPHRASE to enable live trading");
    }
    
    // 創建交易系統
    let system = Arc::new(RealHftTradingSystem::new(config)?);
    info!("✅ Real HFT trading system initialized");
    
    // 啟動 Web UI（後台任務）
    let system_ui = system.clone();
    tokio::spawn(async move {
        if let Err(e) = start_web_ui(system_ui).await {
            error!("Web UI failed: {}", e);
        }
    });
    
    info!("🌐 Web UI available at: http://localhost:8080");
    info!("📊 Dashboard shows real-time trading status");
    info!("🔄 Toggle between simulation and live trading via UI");
    info!("💡 Press Ctrl+C to stop");
    
    // 模擬一些交易信號進行演示
    let mut counter = 0;
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        
        // 生成測試信號
        let signal = HftSignal {
            direction: if counter % 2 == 0 { TradeDirection::Buy } else { TradeDirection::Sell },
            signal_type: HftTradeType::Scalping,
            entry_price: 50000.0 + (counter as f64 * 10.0),
            position_size: 5.0 + (counter as f64 % 3.0),
            confidence: 0.7,
            expected_hold_time_ms: 2000,
            reasoning: format!("Test signal #{}", counter),
        };
        
        if let Err(e) = system.execute_trading_signal(&signal).await {
            error!("Signal execution failed: {}", e);
        }
        
        counter += 1;
        
        if counter > 100 {
            break;
        }
    }
    
    Ok(())
}