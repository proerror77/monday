//! HFT Runtime - System Builder and Runtime Management
//!
//! This crate provides the runtime system builder and management for HFT applications.
//! It handles declarative assembly and registration of system components including:
//! - Market data adapters
//! - Execution clients
//! - Trading strategies
//! - Risk managers
//! - Event consumers

pub mod ipc_handler;
pub mod portfolio_manager;
pub mod recovery_integration;
pub mod risk_manager_factory;
pub mod sharding;
pub mod system_builder;

// Re-export main types for convenient access
pub use ipc_handler::*;
pub use portfolio_manager::*;
pub use recovery_integration::*;
pub use risk_manager_factory::*;
pub use sharding::*;
pub use system_builder::*;

#[cfg(test)]
mod tests {
    use super::*;
    use engine::dataflow::FlipPolicy;
    use hft_core::Symbol;

    #[test]
    fn test_system_builder_smoke() {
        // 創建最小配置
        let config = SystemConfig {
            engine: SystemEngineConfig {
                queue_capacity: 1024,
                stale_us: 5000,
                top_n: 10,
                flip_policy: FlipPolicy::OnUpdate,
                cpu_affinity: CpuAffinityConfig::default(),
                ack_timeout_ms: 3000,
                reconcile_interval_ms: 5000,
                auto_cancel_exchange_only: false,
            },
            venues: vec![],
            strategies: vec![],
            risk: RiskConfig {
                risk_type: "Default".to_string(),
                global_position_limit: rust_decimal::Decimal::from(100000),
                global_notional_limit: rust_decimal::Decimal::from(1000000),
                max_daily_trades: 1000,
                max_orders_per_second: 10,
                staleness_threshold_us: 3000,
                ..Default::default()
            },
            quotes_only: false,
            router: None,
            infra: None,
            strategy_accounts: std::collections::HashMap::new(),
            accounts: Vec::new(),
            portfolios: Vec::new(),
        };

        // 測試 SystemBuilder 創建
        let builder = SystemBuilder::new(config.clone());
        // 不能直接訪問私有字段，檢查可以成功創建即可

        // 測試鏈式調用
        let _builder = builder.register_market_stream_plan(
            VenueType::Bitget,
            "bitget".to_string(),
            vec![Symbol("BTCUSDT".to_string())],
        );
    }

    #[test]
    fn test_system_runtime_build_only() {
        // 簡化測試：僅測試從 YAML 載入和構建，不涉及 async 啟動

        // 創建測試用的最小配置
        let test_config = r#"
engine:
  queue_capacity: 512
  stale_us: 3000
  top_n: 5
  flip_policy: OnUpdate

venues: []
strategies: []

risk:
  global_position_limit: 50000
  global_notional_limit: 500000
  max_daily_trades: 100
  max_orders_per_second: 5
  staleness_threshold_us: 2000
"#;

        // 寫入臨時文件
        let temp_path = "/tmp/hft_test_config.yaml";
        std::fs::write(temp_path, test_config).expect("無法寫入測試配置檔案");

        // 測試 from_yaml
        let builder = SystemBuilder::from_yaml(temp_path)
            .expect("SystemBuilder::from_yaml 失敗")
            .auto_register_adapters();

        // 測試 build
        let _system = builder.build();

        // 如果能成功構建系統，測試就通過了

        // 清理
        let _ = std::fs::remove_file(temp_path);
    }

    #[cfg(feature = "full_runtime_test")]
    #[tokio::test]
    async fn test_system_runtime_full_lifecycle() {
        // 完整的 from_yaml -> build -> start -> stop 流程測試
        // 運行方式：cargo test -p hft-runtime --features full_runtime_test

        // 創建測試用的最小配置
        let test_config = r#"
engine:
  queue_capacity: 256
  stale_us: 2000
  top_n: 3
  flip_policy: OnUpdate

venues: []
strategies: []

risk:
  global_position_limit: 10000
  global_notional_limit: 100000
  max_daily_trades: 50
  max_orders_per_second: 2
  staleness_threshold_us: 1000
"#;

        // 寫入臨時文件
        let temp_path = "/tmp/hft_full_test_config.yaml";
        std::fs::write(temp_path, test_config).expect("無法寫入測試配置檔案");

        // 測試 from_yaml
        let builder = SystemBuilder::from_yaml(temp_path)
            .expect("SystemBuilder::from_yaml 失敗")
            .auto_register_adapters();

        // 測試 build
        let mut system = builder.build();

        // 測試 start（短時間運行）
        let start_result = system.start().await;
        assert!(
            start_result.is_ok(),
            "系統啟動失敗: {:?}",
            start_result.err()
        );

        // 短暫延遲讓系統運行
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // 測試系統狀態
        let account_view = system.get_account_view().await;
        assert!(account_view.cash_balance >= 0.0, "Account view 應該有效");

        // 測試 stop
        let stop_result = system.stop().await;
        assert!(stop_result.is_ok(), "系統停止失敗: {:?}", stop_result.err());

        // 清理
        let _ = std::fs::remove_file(temp_path);
    }

    #[test]
    fn test_env_var_expansion() {
        // 設置測試環境變量
        std::env::set_var("TEST_API_KEY", "test_key_123");
        std::env::set_var("TEST_SECRET", "test_secret_456");

        // 創建包含環境變量的測試配置
        let test_config = r#"
engine:
  queue_capacity: 1024
  stale_us: 3000
  top_n: 5
  flip_policy: OnUpdate

venues:
  - name: "bitget"
    venue_type: "Bitget"
    api_key: "${TEST_API_KEY}"
    secret: "${TEST_SECRET}"
    passphrase: "${MISSING_VAR}"
    capabilities:
      ws_order_placement: true
      snapshot_crc: false
      all_in_one_topics: true
      private_ws_heartbeat: true

strategies: []

risk:
  global_position_limit: 50000
  global_notional_limit: 500000
  max_daily_trades: 100
  max_orders_per_second: 5
  staleness_threshold_us: 2000
"#;

        // 寫入臨時文件
        let temp_path = "/tmp/hft_env_test_config.yaml";
        std::fs::write(temp_path, test_config).expect("無法寫入測試配置檔案");

        // 測試 from_yaml 能正確展開環境變量
        let builder_result = SystemBuilder::from_yaml(temp_path);
        assert!(
            builder_result.is_ok(),
            "SystemBuilder::from_yaml 失敗: {:?}",
            builder_result.err()
        );

        let builder = builder_result.unwrap();
        assert_eq!(builder.config().venues.len(), 1);

        let venue = &builder.config().venues[0];
        assert_eq!(venue.api_key, Some("test_key_123".to_string()));
        assert_eq!(venue.secret, Some("test_secret_456".to_string()));
        // 未設置的環境變量應保持原始格式
        assert_eq!(venue.passphrase, Some("${MISSING_VAR}".to_string()));

        // 清理
        let _ = std::fs::remove_file(temp_path);
        std::env::remove_var("TEST_API_KEY");
        std::env::remove_var("TEST_SECRET");
    }
}
