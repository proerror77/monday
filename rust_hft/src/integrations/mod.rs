/*!
 * Integrations Module - 外部集成
 * 
 * 包含交易所連接器和 barter-rs 適配器
 */

pub mod bitget_connector;
pub mod multi_connection_bitget;
pub mod dynamic_bitget_connector;
pub mod volume_monitor;
pub mod barter_bitget_adapter;
pub mod barter_orderbook_manager;
pub mod high_perf_orderbook_manager;
pub mod barter_standard_subscription_manager;

pub use bitget_connector::{BitgetConnector, BitgetConfig, BitgetChannel, BitgetMessage};
pub use multi_connection_bitget::{
    MultiConnectionBitgetConnector, 
    ConnectionStrategy, 
    ConnectionStats,
    create_optimized_multi_connector,
    create_high_volume_connector,
    create_volume_based_connector,
    create_dynamic_balancing_connector
};
pub use dynamic_bitget_connector::{
    DynamicBitgetConnector,
    DynamicGroupingStrategy,
    ConnectionGroup,
    DynamicConnectorStats,
    create_traffic_based_dynamic_connector,
    create_adaptive_dynamic_connector
};
pub use volume_monitor::{
    SymbolVolumeMonitor,
    SymbolTrafficStats,
    DynamicLoadBalancer
};
pub use barter_bitget_adapter::{BitgetAdapter, BitgetMarketStream, create_bitget_stream};
pub use barter_orderbook_manager::{
    BarterOrderBookManager, 
    MultiLevelOrderBook, 
    ExtendedLevel, 
    OrderBookLevel,
    OrderBookManagerStats
};
pub use high_perf_orderbook_manager::{
    HighPerfOrderBookManager,
    HighPerfOrderBook,
    HighPerfLevel,
    CachedOrderBookStats,
    HighPerfManagerStats
};
pub use barter_standard_subscription_manager::{
    BarterStandardSubscriptionManager,
    BarterSubscriptionConfig,
    BarterMarketStream,
    SubscriptionManagerStats
};