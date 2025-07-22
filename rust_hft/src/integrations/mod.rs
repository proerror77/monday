/*!
 * Integrations Module - 外部集成
 * 
 * 包含交易所連接器和 barter-rs 適配器
 */

// 統一的實現（推薦使用）
pub mod unified_bitget_connector;
pub mod unified_orderbook_manager;
pub mod zero_copy_websocket;

// 簡化實現（臨時修復）
pub mod simple_bitget_connector;


// 舊版連接器（將被廢棄）
#[deprecated(since = "0.2.0", note = "Please use unified_bitget_connector instead")]
pub mod bitget_connector;

pub mod volume_monitor;
pub mod barter_bitget_adapter;

// 舊版 OrderBook 管理器已移除

// barter_standard_subscription_manager 已完全廢棄，不再導出
#[deprecated(since = "0.2.0", note = "Please use unified_bitget_connector with barter_bitget_adapter instead")]
pub mod barter_standard_subscription_manager;

// 導出統一實現（推薦使用）
pub use unified_bitget_connector::{
    UnifiedBitgetConnector, 
    UnifiedBitgetConfig, 
    ConnectionMode, 
    LoadBalancingStrategy,
    BitgetChannel as UnifiedBitgetChannel,
    BitgetMessage as UnifiedBitgetMessage,
    ConnectionState,
    ConnectionStats,
    create_single_connector,
    create_multi_connector,
    create_dynamic_connector,
};

// 簡化連接器導出
pub use simple_bitget_connector::{
    SimpleBitgetConnector,
    SimpleConfig,
    SimpleChannel,
    SimpleMessage,
    create_simple_connector,
    create_custom_connector,
};


pub use unified_orderbook_manager::{
    UnifiedOrderBookManager,
    UnifiedOrderBookConfig,
    OrderBookSnapshot,
    PriceLevel,
    OrderBookUpdate,
    OrderBookManagerStats,
    create_default_manager,
    create_high_performance_manager,
    create_low_latency_manager,
};

// 舊版連接器導出已移除，請使用 unified_bitget_connector
pub use volume_monitor::{
    SymbolVolumeMonitor,
    SymbolTrafficStats,
    DynamicLoadBalancer
};
pub use barter_bitget_adapter::{BitgetAdapter, BitgetMarketStream, create_bitget_stream};
// 舊版 OrderBook 管理器已移除，請使用 unified_orderbook_manager
// barter_standard_subscription_manager 導出已完全移除，模塊已廢棄