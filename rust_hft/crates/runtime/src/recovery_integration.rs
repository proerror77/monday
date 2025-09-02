//! Recovery system integration with SystemRuntime

use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn, error};
use futures::stream;
use async_trait::async_trait;

#[cfg(feature = "recovery")]
use hft_recovery::{RecoveryManager, RecoveryConfig, RecoveryStats};

use crate::SystemRuntime;

/// Recovery integration for SystemRuntime
#[cfg(feature = "recovery")]
pub struct RuntimeRecovery {
    recovery_manager: RecoveryManager,
}

#[cfg(feature = "recovery")]
impl RuntimeRecovery {
    pub fn new(config: RecoveryConfig) -> Self {
        let recovery_manager = RecoveryManager::new(config);
        Self { recovery_manager }
    }
    
    /// Initialize recovery system
    pub async fn init(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.recovery_manager.init().await.map_err(Into::into)
    }
    
    /// Perform startup recovery for the given runtime
    pub async fn perform_startup_recovery(
        &self,
        runtime: &SystemRuntime,
    ) -> Result<RecoveryStats, Box<dyn std::error::Error>> {
        info!("Starting system recovery process");

        // 1) 從引擎導出當前 OMS/Portfolio 狀態
        let (oms_state, pf_state) = {
            let eng = runtime.engine.lock().await;
            (eng.export_oms_state(), eng.export_portfolio_state())
        };

        // 2) 構建臨時 OmsCore/Portfolio 實例以供恢復模組操作
        let oms = Arc::new(Mutex::new({
            let mut o = oms_core::OmsCore::new();
            o.import_state(oms_state);
            o
        }));
        let portfolio = Arc::new(Mutex::new({
            let mut p = portfolio_core::Portfolio::new();
            p.import_state(pf_state);
            p
        }));

        // 3) 先執行快照恢復（不做交易所對賬）
        let mut stats = self.recovery_manager
            .perform_startup_recovery::<MockExecutionClient>(oms.clone(), portfolio.clone(), &[])
            .await
            .map_err(Into::into)?;

        // 4) 構建臨時執行客戶端，針對每個 venue 進行對賬（若啟用）
        #[cfg(feature = "adapter-bitget-execution")]
        {
            use crate::system_builder::{VenueType, VenueConfig};
            for venue in &runtime.config.venues {
                if matches!(venue.venue_type, VenueType::Bitget) {
                    let execution_mode = match venue.execution_mode.as_deref().unwrap_or("Paper") {
                        "Live" => adapter_bitget_execution::ExecutionMode::Live,
                        _ => adapter_bitget_execution::ExecutionMode::Paper,
                    };
                    let exec_cfg = adapter_bitget_execution::BitgetExecutionConfig {
                        credentials: integration::signing::BitgetCredentials {
                            api_key: venue.api_key.clone().unwrap_or_default(),
                            secret_key: venue.secret.clone().unwrap_or_default(),
                            passphrase: venue.passphrase.clone().unwrap_or_default(),
                        },
                        mode: execution_mode,
                        rest_base_url: venue.rest.clone().unwrap_or_default(),
                        ws_private_url: venue.ws_private.clone().unwrap_or_default(),
                        timeout_ms: 5000,
                    };
                    if let Ok(client) = adapter_bitget_execution::BitgetExecutionClient::new(exec_cfg) {
                        // 無需連線即可使用 REST 列單（根據實作決定）；這裡直接嘗試對賬
                        if let Err(e) = self.recovery_manager.reconcile_with_client(oms.clone(), &client, &mut stats).await {
                            warn!("Bitget 對賬失敗: {}", e);
                        }
                    } else {
                        warn!("無法創建 Bitget 臨時執行客戶端用於對賬");
                    }
                }
            }
        }

        #[cfg(feature = "adapter-binance-execution")]
        {
            use crate::system_builder::{VenueType};
            for venue in &runtime.config.venues {
                if matches!(venue.venue_type, VenueType::Binance) {
                    let client = adapter_binance_execution::BinanceExecutionClient::new();
                    if let Err(e) = self.recovery_manager.reconcile_with_client(oms.clone(), &client, &mut stats).await {
                        warn!("Binance 對賬失敗: {}", e);
                    }
                }
            }
        }

        // 5) 將恢復+對賬後的狀態導回引擎
        {
            let mut eng = runtime.engine.lock().await;
            let new_oms_state = oms.lock().await.export_state();
            let new_pf_state = portfolio.lock().await.export_state();
            eng.import_oms_state(new_oms_state);
            eng.import_portfolio_state(new_pf_state);
        }

        info!("Recovery completed: {} orders restored, {} positions restored",
              stats.orders_restored, stats.positions_restored);

        Ok(stats)
    }
    
    /// Save current runtime state
    pub async fn save_snapshot(
        &self,
        runtime: &SystemRuntime,
        system_version: String,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // 導出引擎當前狀態到臨時實例，再交由恢復系統保存
        let (oms_state, pf_state) = {
            let eng = runtime.engine.lock().await;
            (eng.export_oms_state(), eng.export_portfolio_state())
        };

        let oms = Arc::new(Mutex::new({
            let mut o = oms_core::OmsCore::new();
            o.import_state(oms_state);
            o
        }));
        let portfolio = Arc::new(Mutex::new({
            let mut p = portfolio_core::Portfolio::new();
            p.import_state(pf_state);
            p
        }));

        self.recovery_manager
            .save_snapshot(oms, portfolio, system_version)
            .await
            .map_err(Into::into)
    }
}

// Mock execution client for compilation - should be replaced with real clients
struct MockExecutionClient;

#[async_trait]
impl ports::ExecutionClient for MockExecutionClient {
    async fn place_order(&mut self, _intent: ports::OrderIntent) -> Result<hft_core::OrderId, hft_core::HftError> {
        Ok(hft_core::OrderId("mock-order-1".to_string()))
    }
    
    async fn cancel_order(&mut self, _order_id: &hft_core::OrderId) -> Result<(), hft_core::HftError> {
        Ok(())
    }
    
    async fn modify_order(
        &mut self,
        _order_id: &hft_core::OrderId,
        _new_quantity: Option<hft_core::Quantity>,
        _new_price: Option<hft_core::Price>
    ) -> Result<(), hft_core::HftError> {
        Ok(())
    }
    
    async fn execution_stream(&self) -> Result<ports::BoxStream<ports::ExecutionEvent>, hft_core::HftError> {
        Ok(Box::pin(stream::empty()))
    }
    
    async fn connect(&mut self) -> Result<(), hft_core::HftError> {
        Ok(())
    }
    
    async fn disconnect(&mut self) -> Result<(), hft_core::HftError> {
        Ok(())
    }
    
    async fn list_open_orders(&self) -> Result<Vec<ports::OpenOrder>, hft_core::HftError> {
        Ok(Vec::new())
    }
    
    async fn health(&self) -> ports::ConnectionHealth {
        ports::ConnectionHealth {
            connected: true,
            latency_ms: Some(1.0),
            last_heartbeat: 12345,
        }
    }
}

/// Helper function to create recovery manager with default config
#[cfg(feature = "recovery")]
pub fn create_default_recovery() -> RuntimeRecovery {
    let config = RecoveryConfig::default();
    RuntimeRecovery::new(config)
}

/// Helper function to create recovery manager with custom config
#[cfg(feature = "recovery")]
pub fn create_recovery(state_dir: std::path::PathBuf) -> RuntimeRecovery {
    let config = RecoveryConfig {
        state_dir,
        ..RecoveryConfig::default()
    };
    RuntimeRecovery::new(config)
}

// Stub implementations when recovery feature is disabled
#[cfg(not(feature = "recovery"))]
pub struct RuntimeRecovery;

#[cfg(not(feature = "recovery"))]
impl RuntimeRecovery {
    pub fn new(_config: ()) -> Self {
        RuntimeRecovery
    }
    
    pub async fn init(&self) -> Result<(), Box<dyn std::error::Error>> {
        warn!("Recovery system disabled - compile with 'recovery' feature to enable");
        Ok(())
    }
    
    pub async fn perform_startup_recovery(
        &self,
        _runtime: &SystemRuntime,
    ) -> Result<(), Box<dyn std::error::Error>> {
        warn!("Recovery system disabled - no startup recovery performed");
        Ok(())
    }
    
    pub async fn save_snapshot(
        &self,
        _runtime: &SystemRuntime,
        _system_version: String,
    ) -> Result<(), Box<dyn std::error::Error>> {
        warn!("Recovery system disabled - no snapshot saved");
        Ok(())
    }
}

#[cfg(not(feature = "recovery"))]  
pub fn create_default_recovery() -> RuntimeRecovery {
    RuntimeRecovery
}

#[cfg(not(feature = "recovery"))]
pub fn create_recovery(_state_dir: std::path::PathBuf) -> RuntimeRecovery {
    RuntimeRecovery
}
