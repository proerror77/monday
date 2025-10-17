use std::fs;
use std::path::Path;

use anyhow::Context;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct BacktestConfig {
    pub data: DataConfig,
    pub strategy: StrategyConfig,
    #[serde(default)]
    pub execution: ExecutionConfig,
    #[serde(default)]
    pub risk: RiskConfig,
    #[serde(default)]
    pub output: OutputConfig,
}

impl BacktestConfig {
    pub fn from_file<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let yaml = fs::read_to_string(&path)
            .with_context(|| format!("無法讀取配置檔: {}", path.as_ref().display()))?;
        let mut cfg: BacktestConfig = serde_yaml::from_str(&yaml)
            .with_context(|| format!("解析配置檔失敗: {}", path.as_ref().display()))?;
        cfg.normalize();
        Ok(cfg)
    }

    fn normalize(&mut self) {
        // clamp 合理值
        if self.strategy.price_delta_ticks < 1.0 {
            self.strategy.price_delta_ticks = 1.0;
        }
        if self.strategy.liquidity_window_secs < 1.0 {
            self.strategy.liquidity_window_secs = 60.0;
        }
        if self.strategy.breakout_window_secs < 0.2 {
            self.strategy.breakout_window_secs = 0.2;
        }
        if self.execution.base_qty <= 0.0 {
            self.execution.base_qty = self.data.lot_size.max(0.001);
        }
        if self.execution.max_position <= 0.0 {
            self.execution.max_position = self.execution.base_qty * 5.0;
        }
        if self.execution.stop_loss_ticks <= 0.0 {
            self.execution.stop_loss_ticks = 8.0;
        }
        if self.execution.take_profit_ticks <= 0.0 {
            self.execution.take_profit_ticks = 8.0;
        }
        if self.risk.inventory_limit <= 0.0 {
            self.risk.inventory_limit = self.execution.max_position * 2.0;
        }
        if self.risk.slippage_limit_ticks <= 0.0 {
            self.risk.slippage_limit_ticks = self.execution.max_slippage_ticks;
        }
        if self.strategy.support_count == 0 {
            self.strategy.support_count = 3;
        }
        if self.strategy.resistance_count == 0 {
            self.strategy.resistance_count = 3;
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct DataConfig {
    pub path: String,
    #[serde(default = "default_format")]
    pub format: String,
    #[serde(default = "default_tick_size")]
    pub tick_size: f64,
    #[serde(default = "default_lot_size")]
    pub lot_size: f64,
    #[serde(default = "default_depth_levels")]
    pub max_depth_levels: usize,
    #[serde(default)]
    pub start_ts: Option<i64>,
    #[serde(default)]
    pub end_ts: Option<i64>,
}

fn default_format() -> String {
    "ndjson".to_string()
}

fn default_tick_size() -> f64 {
    0.5
}

fn default_lot_size() -> f64 {
    0.001
}

fn default_depth_levels() -> usize {
    20
}

#[derive(Debug, Clone, Deserialize)]
pub struct StrategyConfig {
    #[serde(default = "default_liquidity_window")]
    pub liquidity_window_secs: f64,
    #[serde(default = "default_breakout_window")]
    pub breakout_window_secs: f64,
    #[serde(default = "default_price_delta_ticks")]
    pub price_delta_ticks: f64,
    #[serde(default = "default_volume_factor")]
    pub volume_factor: f64,
    #[serde(default = "default_cvd_threshold")]
    pub cvd_threshold: f64,
    #[serde(default = "default_ofi_threshold")]
    pub ofi_threshold: f64,
    #[serde(default = "default_support_count")]
    pub support_count: usize,
    #[serde(default = "default_resistance_count")]
    pub resistance_count: usize,
    #[serde(default = "default_smoothing_alpha")]
    pub smoothing_alpha: f64,
}

fn default_liquidity_window() -> f64 {
    900.0 // 15 分
}

fn default_breakout_window() -> f64 {
    5.0
}

fn default_price_delta_ticks() -> f64 {
    2.0
}

fn default_volume_factor() -> f64 {
    1.0
}

fn default_cvd_threshold() -> f64 {
    0.0
}

fn default_ofi_threshold() -> f64 {
    0.0
}

fn default_support_count() -> usize {
    3
}

fn default_resistance_count() -> usize {
    3
}

fn default_smoothing_alpha() -> f64 {
    0.2
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExecutionConfig {
    #[serde(default = "default_base_qty")]
    pub base_qty: f64,
    #[serde(default = "default_max_position")]
    pub max_position: f64,
    #[serde(default = "default_slippage_ticks")]
    pub max_slippage_ticks: f64,
    #[serde(default = "default_slippage_ticks")]
    pub stop_loss_ticks: f64,
    #[serde(default = "default_slippage_ticks")]
    pub take_profit_ticks: f64,
    #[serde(default)]
    pub hold_secs: Option<f64>,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            base_qty: default_base_qty(),
            max_position: default_max_position(),
            max_slippage_ticks: default_slippage_ticks(),
            stop_loss_ticks: default_slippage_ticks(),
            take_profit_ticks: default_slippage_ticks(),
            hold_secs: Some(900.0),
        }
    }
}

fn default_base_qty() -> f64 {
    0.01
}

fn default_max_position() -> f64 {
    0.05
}

fn default_slippage_ticks() -> f64 {
    2.0
}

#[derive(Debug, Clone, Deserialize)]
pub struct RiskConfig {
    #[serde(default = "default_inventory_limit")]
    pub inventory_limit: f64,
    #[serde(default = "default_slippage_ticks")]
    pub slippage_limit_ticks: f64,
    #[serde(default = "default_max_consecutive_losses")]
    pub max_consecutive_losses: usize,
    #[serde(default)]
    pub daily_loss_limit: Option<f64>,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            inventory_limit: default_inventory_limit(),
            slippage_limit_ticks: default_slippage_ticks(),
            max_consecutive_losses: default_max_consecutive_losses(),
            daily_loss_limit: None,
        }
    }
}

fn default_inventory_limit() -> f64 {
    0.1
}

fn default_max_consecutive_losses() -> usize {
    3
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct OutputConfig {
    #[serde(default = "default_trade_csv")]
    pub trades_csv: String,
    #[serde(default = "default_summary_csv")]
    pub summary_csv: String,
    #[serde(default)]
    pub metrics_json: Option<String>,
}

fn default_trade_csv() -> String {
    "backtest_trades.csv".to_string()
}

fn default_summary_csv() -> String {
    "backtest_summary.csv".to_string()
}

