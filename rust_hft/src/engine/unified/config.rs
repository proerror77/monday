/*!
 * Unified Engine Configuration
 * 
 * 统一引擎的配置结构和选项
 */

use super::modes::EngineMode;

/// 統一引擎配置
#[derive(Debug, Clone)]
pub struct UnifiedEngineConfig {
    /// 引擎模式
    pub mode: EngineMode,
    
    /// 支持的交易符號
    pub symbols: Vec<String>,
    
    /// 風險管理配置
    pub risk_config: RiskConfig,
    
    /// 執行配置
    pub execution_config: ExecutionConfig,
    
    /// 性能配置
    pub performance_config: PerformanceConfig,
}

/// 風險管理配置
#[derive(Debug, Clone)]
pub struct RiskConfig {
    /// 最大持倉比例 (0.0-1.0)
    pub max_position_ratio: f64,
    
    /// 每筆交易最大損失 (0.0-1.0)
    pub max_loss_per_trade: f64,
    
    /// 每日最大損失 (0.0-1.0)
    pub max_daily_loss: f64,
    
    /// 最大杠桿
    pub max_leverage: f64,
    
    /// 啟用動態風險調整
    pub enable_dynamic_adjustment: bool,
    
    /// Kelly 準則係數 (可選)
    pub kelly_fraction: Option<f64>,
}

/// 執行配置
#[derive(Debug, Clone)]
pub struct ExecutionConfig {
    /// 首選訂單類型
    pub preferred_order_type: OrderType,
    
    /// 滑點容忍度 (基點)
    pub slippage_tolerance_bps: f64,
    
    /// 訂單超時時間 (毫秒)
    pub order_timeout_ms: u64,
    
    /// 啟用智能路由
    pub enable_smart_routing: bool,
    
    /// 啟用冰山訂單
    pub enable_iceberg: bool,
}

/// 性能配置
#[derive(Debug, Clone)]
pub struct PerformanceConfig {
    /// 啟用指標收集
    pub enable_metrics: bool,
    
    /// 指標更新間隔 (秒)
    pub metrics_update_interval_secs: u64,
    
    /// 啟用延遲追踪
    pub enable_latency_tracking: bool,
    
    /// 啟用記憶體優化
    pub enable_memory_optimization: bool,
}

/// 訂單類型
#[derive(Debug, Clone)]
pub enum OrderType {
    Market,
    Limit,
    StopLimit,
    Iceberg,
}

impl Default for UnifiedEngineConfig {
    fn default() -> Self {
        Self {
            mode: EngineMode::Live {
                dry_run: true,
                enable_paper_trading: true,
            },
            symbols: vec!["BTCUSDT".to_string()],
            risk_config: RiskConfig::default(),
            execution_config: ExecutionConfig::default(),
            performance_config: PerformanceConfig::default(),
        }
    }
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            max_position_ratio: 0.1,
            max_loss_per_trade: 0.02,
            max_daily_loss: 0.05,
            max_leverage: 1.0,
            enable_dynamic_adjustment: true,
            kelly_fraction: Some(0.25),
        }
    }
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            preferred_order_type: OrderType::Limit,
            slippage_tolerance_bps: 10.0,
            order_timeout_ms: 5000,
            enable_smart_routing: true,
            enable_iceberg: false,
        }
    }
}

impl Default for PerformanceConfig {
    fn default() -> Self {
        Self {
            enable_metrics: true,
            metrics_update_interval_secs: 10,
            enable_latency_tracking: true,
            enable_memory_optimization: true,
        }
    }
}