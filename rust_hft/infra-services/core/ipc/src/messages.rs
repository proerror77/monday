//! IPC Message definitions
//!
//! All control plane messages serialized using MessagePack

use hft_core::Symbol;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use shared_config::StrategyParams as SharedStrategyParams;
use std::collections::HashMap;

#[cfg(feature = "ipc")]
use uuid::Uuid;

/// Request ID type - uses UUID when IPC feature enabled, String otherwise
#[cfg(feature = "ipc")]
pub type RequestId = Uuid;

#[cfg(not(feature = "ipc"))]
pub type RequestId = String;

/// Top-level IPC message envelope
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IPCMessage {
    /// Unique request ID for correlation
    pub id: RequestId,
    /// Message timestamp (Unix epoch nanos)
    pub timestamp: u64,
    /// Message payload
    pub payload: IPCPayload,
}

#[cfg(feature = "ipc")]
impl IPCMessage {
    pub fn new(payload: IPCPayload) -> Self {
        Self {
            id: Uuid::new_v4(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64,
            payload,
        }
    }
}

#[cfg(not(feature = "ipc"))]
impl IPCMessage {
    pub fn new(payload: IPCPayload) -> Self {
        Self {
            id: String::new(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64,
            payload,
        }
    }
}

/// IPC payload variants
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum IPCPayload {
    /// Command from client to server
    Command(Command),
    /// Response from server to client
    Response(Response),
    /// Status update from server
    Status(StatusUpdate),
}

/// Control commands
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Command {
    /// Start trading system
    Start,
    /// Stop trading system gracefully
    Stop,
    /// Emergency stop with immediate position closure
    EmergencyStop,
    /// Load new model
    LoadModel {
        model_path: String,
        model_version: String,
        sha256_hash: Option<String>,
    },
    /// Update risk parameters
    UpdateRisk {
        global_position_limit: Option<Decimal>,
        global_notional_limit: Option<Decimal>,
        max_daily_trades: Option<u32>,
        max_orders_per_second: Option<u32>,
        staleness_threshold_us: Option<u64>,
        /// Strategy-specific overrides
        strategy_overrides: HashMap<String, StrategyRiskConfig>,
    },
    /// Get system status
    GetStatus,
    /// Get account information
    GetAccount,
    /// Get positions
    GetPositions,
    /// Get open orders
    GetOpenOrders,
    /// Cancel all orders
    CancelAllOrders,
    /// Cancel orders for specific symbol
    CancelOrdersForSymbol { symbol: Symbol },
    /// Cancel orders for specific strategy
    CancelOrdersForStrategy { strategy_id: String },
    /// Cancel single order by id and symbol
    CancelOrder { order_id: String, symbol: Symbol },
    /// Set trading mode
    SetTradingMode { mode: TradingMode },
    /// Enable/disable strategy
    SetStrategyEnabled { strategy_id: String, enabled: bool },
    /// Adjust position limits for symbol
    SetSymbolLimits {
        symbol: Symbol,
        max_position: Option<Decimal>,
        max_notional: Option<Decimal>,
    },
    /// Update strategy parameters at runtime
    UpdateStrategyParams {
        strategy_id: String,
        params: SharedStrategyParams,
    },
}

/// Command responses
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    /// Command acknowledged and executed successfully
    Ok,
    /// Command executed with additional data
    Data(ResponseData),
    /// Command failed with error
    Error { message: String, code: Option<u16> },
}

/// Response data variants
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ResponseData {
    /// System status information
    Status(SystemStatus),
    /// Account information
    Account(AccountInfo),
    /// Position information
    Positions(Vec<Position>),
    /// Open orders
    OpenOrders(Vec<Order>),
    /// Result of cancel commands
    CancelResult(CancelStats),
}

/// Cancel command result statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelStats {
    pub requested: u32,
    pub succeeded: u32,
    pub failed: u32,
    #[serde(default)]
    pub details: Vec<CancelDetail>,
}

/// Per-order cancel result detail
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelDetail {
    pub order_id: String,
    pub success: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

/// Trading modes
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum TradingMode {
    /// Live trading mode
    Live,
    /// Paper trading mode
    Paper,
    /// Replay mode for testing
    Replay,
    /// Paused (no new orders)
    Paused,
}

/// Strategy-specific risk configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyRiskConfig {
    pub max_position: Option<Decimal>,
    pub max_notional: Option<Decimal>,
    pub max_orders_per_second: Option<u32>,
    pub enabled: bool,
}

/// System status information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemStatus {
    /// System uptime in seconds
    pub uptime_seconds: u64,
    /// Trading mode
    pub trading_mode: TradingMode,
    /// Number of active strategies
    pub active_strategies: u32,
    /// Number of connected venues
    pub connected_venues: u32,
    /// Total orders placed today
    pub orders_today: u64,
    /// Total trades today
    pub trades_today: u64,
    /// Current PnL
    pub current_pnl: Decimal,
    /// Maximum drawdown today
    pub max_drawdown: Decimal,
    /// Last model version
    pub model_version: Option<String>,
    /// System health indicators
    pub health: SystemHealth,
}

/// System health indicators
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemHealth {
    /// Data ingestion lag (microseconds)
    pub ingestion_lag_us: u64,
    /// Order execution lag (microseconds)
    pub execution_lag_us: u64,
    /// Market data staleness (microseconds)
    pub data_staleness_us: u64,
    /// Ring buffer utilization (0.0 to 1.0)
    pub ring_utilization: f64,
    /// Memory usage in bytes
    pub memory_usage_bytes: u64,
    /// CPU usage percentage
    pub cpu_usage_pct: f64,
}

/// Account information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountInfo {
    /// Available cash balance
    pub cash_balance: Decimal,
    /// Total portfolio value
    pub total_value: Decimal,
    /// Today's realized PnL
    pub realized_pnl: Decimal,
    /// Today's unrealized PnL
    pub unrealized_pnl: Decimal,
    /// Maximum drawdown from start of day
    pub max_drawdown: Decimal,
    /// Number of open positions
    pub open_positions: u32,
    /// Number of open orders
    pub open_orders: u32,
}

/// Position information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub symbol: Symbol,
    pub quantity: Decimal,
    pub average_price: Decimal,
    pub market_value: Decimal,
    pub unrealized_pnl: Decimal,
    pub realized_pnl: Decimal,
    pub last_update: u64,
}

/// Order information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub order_id: String,
    pub symbol: Symbol,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub quantity: Decimal,
    pub price: Option<Decimal>,
    pub filled_quantity: Decimal,
    pub status: OrderStatus,
    pub timestamp: u64,
    pub strategy_id: Option<String>,
    #[serde(default)]
    pub account_id: Option<String>,
}

/// Order side
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum OrderSide {
    Buy,
    Sell,
}

/// Order type
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum OrderType {
    Market,
    Limit,
    StopLimit,
}

/// Order status
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum OrderStatus {
    Pending,
    PartiallyFilled,
    Filled,
    Cancelled,
    Rejected,
}

/// Status updates (push notifications from server)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StatusUpdate {
    /// System state changed
    SystemStateChanged {
        new_state: TradingMode,
        reason: String,
    },
    /// Model updated
    ModelUpdated { version: String, loaded_at: u64 },
    /// Risk breach detected
    RiskBreach {
        breach_type: RiskBreachType,
        value: Decimal,
        threshold: Decimal,
        action: String,
    },
    /// Strategy state changed
    StrategyStateChanged {
        strategy_id: String,
        enabled: bool,
        reason: String,
    },
    /// Venue connection state changed
    VenueStateChanged {
        venue: String,
        connected: bool,
        reason: String,
    },
}

/// Risk breach types
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum RiskBreachType {
    PositionLimit,
    NotionalLimit,
    DrawdownLimit,
    OrderRateLimit,
    StalenessLimit,
}
