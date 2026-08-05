//! 運行時管理相關擴充方法（從 system_builder.rs 拆分）

use super::SystemRuntime;
// use crate::portfolio_manager::PortfolioManager; // 未使用，移除
use engine; // for types in signatures
use hft_core::{HftError, OrderId, Price, Quantity, Symbol};
use std::sync::Arc;

impl SystemRuntime {
    pub fn execution_control_handle(&self) -> engine::ExecutionControlHandle {
        let execution_enabled = self.exec_control_tx.is_some();
        engine::ExecutionControlHandle::new(
            self.engine.clone(),
            self.exec_control_tx.clone(),
            execution_enabled,
        )
        .with_balance_tolerance_usd(self.config.engine.balance_reconcile_tolerance_usd)
        .with_operation_gate(self.execution_control_gate.clone())
    }

    /// 為 IPC 創建共享實例，避免雙實例問題
    /// 共享引擎和配置，但使用獨立的任務列表
    #[allow(dead_code)] // Used when infra-ipc feature is enabled
    pub(crate) fn clone_for_ipc(&self) -> SystemRuntime {
        SystemRuntime {
            engine: self.engine.clone(),
            execution_control_gate: self.execution_control_gate.clone(),
            config: self.config.clone(),
            tasks: vec![],                  // IPC 專用空任務列表
            execution_worker_tasks: vec![], // IPC 專用空任務列表
            ipc_task: None,
            exec_control_tx: self.exec_control_tx.clone(),
            market_plans: self.market_plans.clone(),
            execution_client_venues: self.execution_client_venues.clone(),
            execution_client_accounts: self.execution_client_accounts.clone(),
            execution_account_admissions: self.execution_account_admissions.clone(),
            portfolio_manager: self.portfolio_manager.clone(),
            adapter_bridge: None, // IPC 專用實例不持有 bridge
        }
    }

    /// 獲取市場/賬戶視圖快照
    pub async fn get_market_view(&self) -> Arc<engine::aggregation::MarketView> {
        self.engine.lock().await.get_market_view()
    }

    pub async fn get_account_view(&self) -> Arc<ports::AccountView> {
        self.engine.lock().await.get_account_view()
    }

    /// Cancel every OMS-open or worker-tracked order owned by a strategy.
    pub async fn cancel_orders_for_strategy(
        &self,
        strategy_id: &str,
    ) -> Result<engine::execution_worker::CancelDispatchReport, HftError> {
        self.execution_control_handle()
            .cancel_orders_for_strategy(strategy_id)
            .await
    }

    /// Fetch venue-authoritative balances, positions, open orders, and recent fills.
    pub async fn inspect_execution_accounts(
        &self,
    ) -> Result<engine::execution_worker::WorkerReconcileSnapshot, HftError> {
        self.execution_control_handle().inspect_account().await
    }

    /// Replace an open order through the execution worker and venue adapter.
    pub async fn replace_order(
        &self,
        order_id: OrderId,
        symbol: Symbol,
        new_quantity: Option<Quantity>,
        new_price: Option<Price>,
    ) -> Result<(), HftError> {
        self.execution_control_handle()
            .replace_order(order_id, symbol, new_quantity, new_price)
            .await
    }
}
