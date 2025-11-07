//! DL 策略主實作（同步版）
//!
//! 透過同步特徵抽取與推理引擎，適配 Phase 1 重構後的
//! `Strategy` trait 介面，專注於單筆市場快照的即時處理。

use crate::config::DlStrategyConfig;
use crate::feature_pipeline::FeaturePipeline;
use crate::inference_engine::{InferenceEngine, InferenceEngineState, InferenceResult};
use crate::model_loader::ModelLoader;

use hft_core::{HftError, HftResult, OrderType, Quantity, Side, Symbol, TimeInForce, Timestamp};
use ports::{AccountView, ExecutionEvent, MarketEvent, MarketSnapshot, OrderIntent, Strategy};
use std::collections::HashMap;
use tracing::{error, info, warn};

const MIN_INFERENCE_INTERVAL_US: u64 = 100_000;

#[derive(Debug, Clone)]
pub struct DlStrategyStats {
    pub symbol: Symbol,
    pub is_degraded: bool,
    pub last_inference_ts: Timestamp,
    pub last_success_ts: Timestamp,
    pub consecutive_failures: u32,
}

#[derive(Debug, Clone)]
struct BookContext {
    best_bid: f64,
    best_ask: f64,
    mid_price: f64,
}

#[derive(Debug, Clone)]
struct SymbolState {
    last_inference_request: Timestamp,
    last_successful_inference: Timestamp,
    consecutive_failures: u32,
    context: Option<BookContext>,
}

impl SymbolState {
    fn new(now: Timestamp) -> Self {
        Self {
            last_inference_request: now.saturating_sub(MIN_INFERENCE_INTERVAL_US),
            last_successful_inference: now,
            consecutive_failures: 0,
            context: None,
        }
    }
}

pub struct DlStrategy {
    config: DlStrategyConfig,
    feature_pipeline: FeaturePipeline,
    inference_engine: InferenceEngine,
    symbol_states: HashMap<String, SymbolState>,
}

impl DlStrategy {
    /// 建立策略實例
    pub async fn new(config: DlStrategyConfig) -> HftResult<Self> {
        info!("初始化 DL 策略: {}", config.name);
        crate::config::validate_config(&config)?;

        let now = Self::current_timestamp();

        let mut feature_pipeline = FeaturePipeline::new(config.features.clone());
        let mut symbol_states = HashMap::new();
        for symbol in &config.symbols {
            feature_pipeline.add_symbol(symbol.as_str().to_string());
            symbol_states.insert(symbol.as_str().to_string(), SymbolState::new(now));
        }

        let model_handle = ModelLoader::load_model(config.model.clone())?;
        let inference_engine = InferenceEngine::new(
            model_handle,
            config.inference.clone(),
            config.risk.clone(),
            config.model.model_version.clone(),
        )?;

        Ok(Self {
            config,
            feature_pipeline,
            inference_engine,
            symbol_states,
        })
    }

    fn current_timestamp() -> Timestamp {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64
    }

    fn state_for_symbol_mut(&mut self, symbol: &Symbol) -> &mut SymbolState {
        let key = symbol.as_str().to_string();
        let now = Self::current_timestamp();
        self.symbol_states
            .entry(key)
            .or_insert_with(|| SymbolState::new(now))
    }

    fn handle_snapshot(
        &mut self,
        snapshot: &MarketSnapshot,
        account: &AccountView,
    ) -> HftResult<Vec<OrderIntent>> {
        let symbol_key = snapshot.symbol.as_str().to_string();
        let context = Self::build_context(snapshot);
        let event = MarketEvent::Snapshot(snapshot.clone());
        let maybe_features = self.feature_pipeline.process_event(&symbol_key, &event)?;

        match maybe_features {
            Some(features) => {
                self.maybe_run_inference(symbol_key, &snapshot.symbol, context, features, account)
            }
            None => {
                if let Some(ctx) = context {
                    let state = self.state_for_symbol_mut(&snapshot.symbol);
                    state.context = Some(ctx);
                }
                Ok(Vec::new())
            }
        }
    }

    fn maybe_run_inference(
        &mut self,
        symbol_key: String,
        symbol: &Symbol,
        context: Option<BookContext>,
        features: ndarray::Array1<f32>,
        account: &AccountView,
    ) -> HftResult<Vec<OrderIntent>> {
        let now = Self::current_timestamp();

        {
            let state = self.state_for_symbol_mut(symbol);
            state.context = context.clone();
            if now.saturating_sub(state.last_inference_request) < MIN_INFERENCE_INTERVAL_US {
                return Ok(Vec::new());
            }
            state.last_inference_request = now;
        }

        match self.inference_engine.infer(&features, &symbol_key) {
            Ok(result) => {
                let output_threshold = self.config.inference.output_threshold;
                let strategy_name = self.config.name.clone();
                let orders = {
                    let state = self.state_for_symbol_mut(symbol);
                    state.last_successful_inference = now;
                    state.consecutive_failures = 0;
                    if let Some(ref ctx) = context {
                        state.context = Some(ctx.clone());
                    }
                    Self::build_orders(
                        output_threshold,
                        &strategy_name,
                        symbol,
                        state,
                        &result,
                        account,
                    )?
                };
                Ok(orders)
            }
            Err(err) => {
                error!("推理失敗 ({}): {}", symbol_key, err);
                let mut degrade = false;
                {
                    let state = self.state_for_symbol_mut(symbol);
                    state.consecutive_failures = state.consecutive_failures.saturating_add(1);
                    if state.consecutive_failures >= 3 {
                        degrade = true;
                    }
                }
                if degrade {
                    warn!("{} 連續推理失敗，進入降級模式", symbol_key);
                    self.inference_engine
                        .set_state(InferenceEngineState::Degraded);
                }
                Ok(Vec::new())
            }
        }
    }

    fn build_orders(
        output_threshold: f64,
        strategy_name: &str,
        symbol: &Symbol,
        state: &SymbolState,
        result: &InferenceResult,
        account: &AccountView,
    ) -> HftResult<Vec<OrderIntent>> {
        let signal = result.output.first().copied().unwrap_or(0.0);
        if signal.abs() < output_threshold as f32 {
            return Ok(Vec::new());
        }

        let context = match &state.context {
            Some(ctx) => ctx,
            None => return Ok(Vec::new()),
        };

        if context.best_ask <= context.best_bid || context.mid_price <= 0.0 {
            return Ok(Vec::new());
        }

        let inventory = account
            .positions
            .get(symbol)
            .and_then(|pos| pos.quantity.to_f64())
            .unwrap_or(0.0);

        let target_position = (signal as f64).tanh();
        let position_change = target_position - inventory;
        if position_change.abs() < 1e-6 {
            return Ok(Vec::new());
        }

        let side = if position_change > 0.0 {
            Side::Buy
        } else {
            Side::Sell
        };

        let quantity = Quantity::from_f64(position_change.abs())
            .map_err(|_| HftError::InvalidOrder("無法將目標倉位轉換為 Quantity".to_string()))?;

        let intent = OrderIntent {
            symbol: symbol.clone(),
            side,
            quantity,
            order_type: OrderType::Market,
            price: None,
            time_in_force: TimeInForce::IOC,
            strategy_id: strategy_name.to_string(),
            target_venue: None,
        };

        Ok(vec![intent])
    }

    fn build_context(snapshot: &MarketSnapshot) -> Option<BookContext> {
        let best_bid = snapshot
            .bids
            .first()
            .and_then(|level| level.price.to_f64())?;
        let best_ask = snapshot
            .asks
            .first()
            .and_then(|level| level.price.to_f64())?;
        let mid_price = (best_bid + best_ask) * 0.5;

        Some(BookContext {
            best_bid,
            best_ask,
            mid_price,
        })
    }
}

impl Strategy for DlStrategy {
    fn on_market_event(&mut self, event: &MarketEvent, account: &AccountView) -> Vec<OrderIntent> {
        match event {
            MarketEvent::Snapshot(snapshot) => match self.handle_snapshot(snapshot, account) {
                Ok(orders) => orders,
                Err(err) => {
                    error!("處理快照失敗: {}", err);
                    Vec::new()
                }
            },
            _ => Vec::new(),
        }
    }

    fn on_execution_event(
        &mut self,
        _event: &ExecutionEvent,
        _account: &AccountView,
    ) -> Vec<OrderIntent> {
        Vec::new()
    }

    fn name(&self) -> &str {
        &self.config.name
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

impl DlStrategy {
    /// 热重载模型
    pub async fn load_model(
        &mut self,
        model_path: std::path::PathBuf,
        model_version: Option<String>,
    ) -> HftResult<()> {
        info!("热重载 DL 模型: {:?} (版本: {:?})", model_path, model_version);

        // 更新配置
        self.config.model.model_path = model_path;
        self.config.model.model_version = model_version;

        // 重新加载模型
        let model_handle = ModelLoader::load_model(self.config.model.clone())?;

        // 创建新的推理引擎
        let new_inference_engine = InferenceEngine::new(
            model_handle,
            self.config.inference.clone(),
            self.config.risk.clone(),
            self.config.model.model_version.clone(),
        )?;

        // 原子性替换推理引擎
        self.inference_engine = new_inference_engine;

        info!("模型热重载完成");
        Ok(())
    }

    /// 获取当前模型版本
    pub fn get_model_version(&self) -> Option<String> {
        self.config.model.model_version.clone()
    }

    /// 获取策略统计信息
    pub fn get_stats(&self) -> Vec<DlStrategyStats> {
        let now = Self::current_timestamp();
        self.symbol_states
            .iter()
            .map(|(symbol_str, state)| {
                let is_degraded = matches!(
                    self.inference_engine.get_state(),
                    InferenceEngineState::Degraded
                );
                DlStrategyStats {
                    symbol: Symbol::new(symbol_str),
                    is_degraded,
                    last_inference_ts: state.last_inference_request,
                    last_success_ts: state.last_successful_inference,
                    consecutive_failures: state.consecutive_failures,
                }
            })
            .collect()
    }
}
