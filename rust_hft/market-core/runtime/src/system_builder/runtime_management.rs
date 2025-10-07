//! 運行時管理相關擴充方法（從 system_builder.rs 拆分）

use super::SystemRuntime;

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
}

