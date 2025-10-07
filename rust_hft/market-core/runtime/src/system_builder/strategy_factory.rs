#[allow(unused_imports)]
use super::{StrategyConfig, StrategyParams, StrategyType, SystemBuilder};
use hft_core::{HftError, HftResult, Symbol};
use ports::Strategy as StrategyTrait;
#[cfg(feature = "strategy-imbalance")]
use rust_decimal::prelude::ToPrimitive;
use tracing::{info, warn};

#[derive(Debug, thiserror::Error)]
pub enum StrategyFactoryError {
    #[error("策略功能未啟用: {0}")]
    FeatureDisabled(&'static str),
    #[error("策略參數缺失: {0}")]
    InvalidParams(String),
    #[error("策略建立失敗: {0}")]
    BuildFailure(String),
}

impl From<StrategyFactoryError> for HftError {
    fn from(err: StrategyFactoryError) -> Self {
        HftError::Config(err.to_string())
    }
}

pub fn create_strategy_instances_from_config(
    config: &StrategyConfig,
) -> HftResult<Vec<Box<dyn StrategyTrait>>> {
    match config.strategy_type {
        StrategyType::Trend => create_trend_strategies(config),
        StrategyType::Imbalance => create_imbalance_strategies(config),
        StrategyType::LobFlowGrid => create_lob_flow_grid_strategies(config),
        StrategyType::Arbitrage => {
            Err(StrategyFactoryError::FeatureDisabled("strategy-arbitrage").into())
        }
        StrategyType::MarketMaking => {
            Err(StrategyFactoryError::FeatureDisabled("strategy-market-making").into())
        }
        StrategyType::Dl => create_dl_strategy(config),
    }
}

pub fn create_strategy_instance_for_symbol(
    config: &StrategyConfig,
    symbol: &Symbol,
) -> HftResult<Box<dyn StrategyTrait>> {
    let instances = create_strategy_instances_from_config(config)?;
    let target_id = format!("{}:{}", config.name, symbol.0);
    instances
        .into_iter()
        .find(|strategy| strategy.id() == target_id)
        .ok_or_else(|| {
            HftError::Config(format!(
                "策略 {} 未為符號 {} 建立實例",
                config.name, symbol.0
            ))
        })
}

impl SystemBuilder {
    pub(super) fn register_strategy_from_config(
        mut self,
        strategy_config: &StrategyConfig,
    ) -> Self {
        match create_strategy_instances_from_config(strategy_config) {
            Ok(instances) => {
                for strategy in instances {
                    info!("註冊策略實例: {}", strategy.id());
                    self.strategies.push(strategy);
                }
            }
            Err(err) => {
                warn!("無法建立策略 {}: {}", strategy_config.name, err);
            }
        }
        self
    }
}

fn create_trend_strategies(_config: &StrategyConfig) -> HftResult<Vec<Box<dyn StrategyTrait>>> {
    #[cfg(feature = "strategy-trend")]
    {
        let (ema_fast, ema_slow, rsi_period) = match &_config.params {
            StrategyParams::Trend {
                ema_fast,
                ema_slow,
                rsi_period,
            } => (*ema_fast, *ema_slow, *rsi_period),
            _ => {
                return Err(StrategyFactoryError::InvalidParams(format!(
                    "Trend 策略缺少 Trend 參數: {}",
                    _config.name
                ))
                .into())
            }
        };

        let mut instances: Vec<Box<dyn StrategyTrait>> = Vec::new();
        for sym in &_config.symbols {
            let mut cfg = strategy_trend::TrendStrategyConfig::default();
            cfg.ema_fast_period = ema_fast;
            cfg.ema_slow_period = ema_slow;
            cfg.rsi_period = rsi_period;
            let instance_id = format!("{}:{}", _config.name, sym.0);
            let strat = strategy_trend::TrendStrategy::with_name(sym.clone(), cfg, instance_id);
            instances.push(Box::new(strat));
        }
        Ok(instances)
    }
    #[cfg(not(feature = "strategy-trend"))]
    {
        Err(StrategyFactoryError::FeatureDisabled("strategy-trend").into())
    }
}

fn create_imbalance_strategies(_config: &StrategyConfig) -> HftResult<Vec<Box<dyn StrategyTrait>>> {
    #[cfg(feature = "strategy-imbalance")]
    {
        let mut instances: Vec<Box<dyn StrategyTrait>> = Vec::new();
        for sym in &_config.symbols {
            let params = match &_config.params {
                StrategyParams::Imbalance {
                    obi_threshold,
                    lot,
                    top_levels,
                } => Some(strategy_imbalance::ImbalanceParams {
                    obi_threshold: *obi_threshold,
                    lot: lot.to_f64().unwrap_or(0.01),
                    top_levels: *top_levels,
                }),
                _ => None,
            };
            let instance_id = format!("{}:{}", _config.name, sym.0);
            let strat =
                strategy_imbalance::ImbalanceStrategy::with_name(sym.clone(), params, instance_id);
            instances.push(Box::new(strat));
        }
        Ok(instances)
    }
    #[cfg(not(feature = "strategy-imbalance"))]
    {
        Err(StrategyFactoryError::FeatureDisabled("strategy-imbalance").into())
    }
}

fn create_lob_flow_grid_strategies(
    _config: &StrategyConfig,
) -> HftResult<Vec<Box<dyn StrategyTrait>>> {
    #[cfg(feature = "strategy-lob-flow-grid")]
    {
        use strategy_lob_flow_grid::LobFlowGridStrategy;
        let params = match &_config.params {
            StrategyParams::LobFlowGrid { config } => config.clone(),
            _ => super::LobFlowGridParams::default(),
        };

        let mut instances: Vec<Box<dyn StrategyTrait>> = Vec::new();
        for sym in &_config.symbols {
            let mut cfg = strategy_lob_flow_grid::LobFlowGridConfig::default();
            super::apply_lob_flow_overrides(&mut cfg, &params);
            let instance_id = format!("{}:{}", _config.name, sym.0);
            let strat = LobFlowGridStrategy::with_name(sym.clone(), cfg, instance_id);
            instances.push(Box::new(strat));
        }
        Ok(instances)
    }
    #[cfg(not(feature = "strategy-lob-flow-grid"))]
    {
        Err(StrategyFactoryError::FeatureDisabled("strategy-lob-flow-grid").into())
    }
}

fn create_dl_strategy(_config: &StrategyConfig) -> HftResult<Vec<Box<dyn StrategyTrait>>> {
    #[cfg(feature = "strategy-dl")]
    {
        use hft_strategy_dl as sdl;

        let mut cfg = sdl::DlStrategyConfig::default();
        cfg.name = _config.name.clone();
        cfg.symbols = _config.symbols.clone();

        if let StrategyParams::Dl {
            model_path,
            device,
            top_n,
            window_size,
            trigger_threshold,
            output_threshold,
            queue_capacity,
            timeout_ms,
            max_error_rate,
            degradation_mode,
        } = &_config.params
        {
            cfg.model.model_path = std::path::PathBuf::from(model_path);
            cfg.model.device = device.clone();
            cfg.features.top_n = *top_n;
            cfg.features.window_size = *window_size;
            cfg.inference.queue_capacity = *queue_capacity;
            cfg.inference.timeout_ms = *timeout_ms;
            cfg.inference.trigger_threshold = *trigger_threshold;
            cfg.inference.output_threshold = *output_threshold;
            cfg.risk.max_error_rate = *max_error_rate;
            cfg.risk.degradation_mode = degradation_mode.clone();
        } else {
            warn!("DL 策略缺少特定參數，使用預設");
        }

        let strat = match tokio::runtime::Handle::try_current() {
            Ok(handle) => handle.block_on(sdl::DlStrategy::new(cfg.clone())),
            Err(_) => {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("建立臨時 runtime 失敗");
                rt.block_on(sdl::DlStrategy::new(cfg.clone()))
            }
        };

        match strat {
            Ok(strategy) => Ok(vec![Box::new(strategy) as Box<dyn StrategyTrait>]),
            Err(err) => Err(StrategyFactoryError::BuildFailure(err.to_string()).into()),
        }
    }
    #[cfg(not(feature = "strategy-dl"))]
    {
        Err(StrategyFactoryError::FeatureDisabled("strategy-dl").into())
    }
}
