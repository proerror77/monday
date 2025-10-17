//! 運行時管理相關擴充方法（從 system_builder.rs 拆分）

use super::SystemRuntime;
use crate::system_builder::strategy_factory::{
    create_strategy_instance_for_symbol, create_strategy_instances_from_config,
};
use crate::system_builder::{RiskConfig, SharedStrategyParams};
// use crate::portfolio_manager::PortfolioManager; // 未使用，移除
use engine; // for types in signatures
use hft_core::{HftError, Symbol};
use std::sync::Arc;

impl SystemRuntime {
    /// 為 IPC 創建共享實例，避免雙實例問題
    /// 共享引擎和配置，但使用獨立的任務列表
    #[allow(dead_code)]
    pub(crate) fn clone_for_ipc(&self) -> SystemRuntime {
        SystemRuntime {
            engine: self.engine.clone(),
            config: self.config.clone(),
            tasks: vec![],                  // IPC 專用空任務列表
            execution_worker_tasks: vec![], // IPC 專用空任務列表
            exec_control_txs: vec![],
            market_plans: self.market_plans.clone(),
            execution_client_venues: self.execution_client_venues.clone(),
            execution_client_accounts: self.execution_client_accounts.clone(),
            portfolio_manager: self.portfolio_manager.clone(),
        }
    }

    /// 熱更新指定策略實例的參數
    pub async fn update_strategy_params(
        &mut self,
        strategy_id: &str,
        new_params: SharedStrategyParams,
    ) -> Result<(), HftError> {
        tracing::info!("熱更新策略參數: {}", strategy_id);
        let (base_name, symbol_part) = match strategy_id.split_once(':') {
            Some((base, sym)) => (base.to_string(), Some(sym.to_string())),
            None => (strategy_id.to_string(), None),
        };

        let cfg_index = self
            .config
            .strategies
            .iter()
            .position(|cfg| cfg.name == base_name)
            .ok_or_else(|| HftError::Config(format!("找不到策略 '{}' 的配置", base_name)))?;

        let strategy_type = self.config.strategies[cfg_index].strategy_type.clone();
        let shared_type = super::to_shared_strategy_type(&strategy_type);
        let runtime_params =
            super::config_loader::convert_strategy_params(&shared_type, new_params)
                .map_err(HftError::Config)?;

        let mut updated_cfg = self.config.strategies[cfg_index].clone();
        updated_cfg.params = runtime_params.clone();

        let target_symbol = if let Some(symbol_str) = symbol_part {
            let symbol = Symbol::from(symbol_str.clone());
            if !updated_cfg.symbols.iter().any(|s| s == &symbol) {
                return Err(HftError::Config(format!(
                    "策略 '{}' 未配置商品 {}",
                    base_name, symbol_str
                )));
            }
            Some(symbol)
        } else if updated_cfg.symbols.len() == 1 {
            updated_cfg.symbols.first().cloned()
        } else {
            None
        };

        let new_strategy = if let Some(symbol) = target_symbol {
            create_strategy_instance_for_symbol(&updated_cfg, &symbol)?
        } else {
            let mut instances = create_strategy_instances_from_config(&updated_cfg)?;
            if instances.len() != 1 {
                return Err(HftError::Config(format!(
                    "策略 '{}' 產生 {} 個實例，請提供更精確的策略 ID",
                    base_name,
                    instances.len()
                )));
            }
            instances.remove(0)
        };

        {
            let mut engine = self.engine.lock().await;
            let index = engine
                .strategy_instance_ids()
                .iter()
                .position(|id| id == strategy_id)
                .ok_or_else(|| HftError::Config(format!("策略實例 '{}' 不存在", strategy_id)))?;
            engine.replace_strategy(index, new_strategy)?;
        }

        self.config.strategies[cfg_index] = updated_cfg;
        Ok(())
    }

    /// 獲取市場/賬戶視圖快照
    pub async fn get_market_view(&self) -> Arc<engine::aggregation::MarketView> {
        self.engine.lock().await.get_market_view()
    }

    pub async fn get_account_view(&self) -> Arc<ports::AccountView> {
        self.engine.lock().await.get_account_view()
    }

    /// 取消指定策略的所有未結訂單（非阻塞）
    pub async fn cancel_orders_for_strategy(&self, strategy_id: &str) -> usize {
        let pairs = {
            let eng = self.engine.lock().await;
            eng.open_order_pairs_by_strategy(strategy_id)
        };
        if pairs.is_empty() {
            return 0;
        }
        for tx in &self.exec_control_txs {
            let _ = tx.send(engine::execution_worker::ControlCommand::CancelOrders(
                pairs.clone(),
            ));
        }
        pairs.len()
    }

    /// 熱更新風控配置：替換風控管理器並應用新的策略覆蓋
    pub async fn update_risk_config(
        &mut self,
        new_risk: RiskConfig,
    ) -> Result<(), Box<dyn std::error::Error>> {
        tracing::info!(
            "更新風控配置: 風控類型={}, 覆蓋策略數={}",
            new_risk.risk_type,
            new_risk.strategy_overrides.len()
        );
        self.config.risk = new_risk;
        let new_manager =
            crate::RiskManagerFactory::create_strategy_aware_risk_manager(&self.config.risk);
        let mut eng = self.engine.lock().await;
        eng.register_risk_manager_boxed(new_manager);
        tracing::info!("風控配置已更新並生效");
        Ok(())
    }

    /// 測試：通过执行队列发送订单意图 (异步处理)
    pub async fn place_test_order(
        &self,
        symbol: &str,
    ) -> Result<hft_core::OrderId, Box<dyn std::error::Error>> {
        use hft_core::{OrderType, Quantity, Side, Symbol, TimeInForce};
        let intent = ports::OrderIntent {
            symbol: Symbol::new(symbol),
            side: Side::Buy,
            quantity: Quantity::from_f64(0.001)?,
            order_type: OrderType::Market,
            price: None,
            time_in_force: TimeInForce::GTC,
            strategy_id: "test_order".to_string(),
            target_venue: None,
        };
        {
            let mut engine_lock = self.engine.lock().await;
            engine_lock.submit_order_intent(intent)?;
        }
        let test_order_id = hft_core::OrderId(format!(
            "test_{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        tracing::info!(
            "測試訂單已成功提交到執行隊列: {} {}",
            symbol,
            test_order_id.0
        );
        Ok(test_order_id)
    }
}
