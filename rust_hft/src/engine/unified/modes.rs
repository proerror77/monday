/*!
 * Engine Operating Modes
 * 
 * 引擎运行模式定义
 */

/// 引擎運行模式
#[derive(Debug, Clone)]
pub enum EngineMode {
    /// 實時交易模式
    Live {
        dry_run: bool,
        enable_paper_trading: bool,
    },
    
    /// 回測模式
    Backtest {
        start_time: u64,
        end_time: u64,
        initial_capital: f64,
    },
    
    /// 模擬模式 (用於測試)
    Simulation {
        duration_seconds: u64,
        enable_noise: bool,
    },
}

/// 實時交易模式
#[derive(Debug, Clone)]
pub struct LiveMode {
    pub dry_run: bool,
    pub enable_paper_trading: bool,
    pub enable_real_money: bool,
}

/// 回測模式
#[derive(Debug, Clone)]
pub struct BacktestMode {
    pub start_time: u64,
    pub end_time: u64,
    pub initial_capital: f64,
    pub commission_rate: f64,
    pub slippage_model: SlippageModel,
}

/// 滑點模型
#[derive(Debug, Clone)]
pub enum SlippageModel {
    /// 固定滑點 (基點)
    Fixed(f64),
    
    /// 線性滑點 (基於交易量)
    Linear {
        base_bps: f64,
        volume_impact: f64,
    },
    
    /// 市場影響模型
    MarketImpact {
        permanent_impact: f64,
        temporary_impact: f64,
    },
}

impl Default for LiveMode {
    fn default() -> Self {
        Self {
            dry_run: true,
            enable_paper_trading: true,
            enable_real_money: false,
        }
    }
}

impl Default for BacktestMode {
    fn default() -> Self {
        Self {
            start_time: 0,
            end_time: 0,
            initial_capital: 100000.0,
            commission_rate: 0.001, // 0.1%
            slippage_model: SlippageModel::Fixed(5.0), // 5 bps
        }
    }
}

impl EngineMode {
    /// 判断是否为实时模式
    pub fn is_live(&self) -> bool {
        matches!(self, EngineMode::Live { .. })
    }

    /// 判断是否为回测模式
    pub fn is_backtest(&self) -> bool {
        matches!(self, EngineMode::Backtest { .. })
    }

    /// 判断是否为模拟模式
    pub fn is_simulation(&self) -> bool {
        matches!(self, EngineMode::Simulation { .. })
    }

    /// 获取初始资金
    pub fn initial_capital(&self) -> f64 {
        match self {
            EngineMode::Live { .. } => 100000.0, // Default for live
            EngineMode::Backtest { initial_capital, .. } => *initial_capital,
            EngineMode::Simulation { .. } => 50000.0, // Default for sim
        }
    }
}