//! HFT 真盤交易應用
//!
//! 特性開關示例：
//! - cargo run --features="bitget,trend-strategy,metrics"
//! - cargo run --features="full"  # 開啟所有特性

mod helpers;

use clap::Parser;
use runtime::{
    ExecutionQueueSettings, RiskConfig as RtRiskConfig, StrategyConfig as RtStrategyConfig,
    StrategyParams as RtStrategyParams, StrategyRiskLimits as RtStrategyRiskLimits,
    StrategyType as RtStrategyType, SystemEngineConfig, VenueCapabilities,
    VenueConfig as RtVenueConfig, VenueType,
};
use runtime::{ShardConfig, ShardStrategy, SystemBuilder, SystemConfig};
use tracing::{info, warn};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// 配置檔案路徑
    /// 預設切換為多交易所只行情配置（Binance/Bitget/Aster），便於快速 e2e 驗證
    #[arg(
        short,
        long,
        default_value = "config/prod/system_accounts_multi_quotes_only.yaml"
    )]
    config: String,

    /// 是否為測試模式
    #[arg(long)]
    test_mode: bool,

    /// 啟動後進行一次 dry-run 下單驗證（Paper 模式）
    #[arg(long)]
    dry_run_order: bool,

    /// dry-run 下單的交易對
    #[arg(long, default_value = "BTCUSDT")]
    dry_run_symbol: String,

    /// 啟動後自動退出的毫秒數（用於CI/sandbox）
    #[arg(long)]
    exit_after_ms: Option<u64>,

    /// 僅行情（不啟動執行端、不連私有WS、不下單）
    #[arg(long)]
    quotes_only: bool,

    /// Metrics HTTP 服務器端口（啟用 metrics feature 時生效）
    #[cfg(feature = "metrics")]
    #[arg(long, default_value = "9090")]
    metrics_port: u16,

    /// 靜態分片：當前分片索引 (0-based)
    #[arg(long)]
    shard_index: Option<u32>,

    /// 靜態分片：總分片數量
    #[arg(long)]
    shard_count: Option<u32>,

    /// 靜態分片：使用自定義分片策略 (symbol-hash, venue-round-robin, hybrid)
    #[arg(long, default_value = "symbol-hash")]
    shard_strategy: String,

    /// 啟用 ONNX 推理（model-only）
    #[arg(long, default_value_t = false)]
    ml_enable: bool,
    /// ONNX 模型路徑
    #[arg(long, default_value = "models/model.onnx")]
    ml_model: String,
    /// Top-K 檔數
    #[arg(long, default_value_t = 20)]
    ml_k: usize,
    /// 序列長度 L
    #[arg(long, default_value_t = 100)]
    ml_l: usize,
    /// 對齊步長（毫秒）
    #[arg(long, default_value_t = 100)]
    ml_step_ms: u64,

    /// 啟用 Sentinel 自動化風控
    #[arg(long, default_value_t = true)]
    sentinel_enable: bool,
    /// Sentinel 檢查間隔（毫秒）
    #[arg(long, default_value_t = 100)]
    sentinel_interval_ms: u64,
    /// Sentinel 延遲警告閾值（微秒）
    #[arg(long, default_value_t = 15000)]
    sentinel_latency_warn_us: u64,
    /// Sentinel 回撤停止閾值（百分比）
    #[arg(long, default_value_t = 5.0)]
    sentinel_drawdown_stop_pct: f64,

    /// gRPC 控制服務端口（啟用 grpc feature 時生效）
    #[cfg(feature = "grpc")]
    #[arg(long, default_value = "9092")]
    grpc_port: u16,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 初始化日誌
    tracing_subscriber::fmt().with_env_filter("info").init();

    let args = Args::parse();

    info!("啟動 HFT 真盤交易系統");
    info!("配置檔案: {}", args.config);

    // Quotes-only 模式：透過環境變量關閉執行端
    if args.quotes_only {
        std::env::set_var("HFT_QUOTES_ONLY", "1");
        info!("已啟用 quotes-only 模式（不下單）");
    }

    // 展示可用的適配器
    show_available_adapters();

    // 使用 SystemBuilder 從 YAML 配置建構系統
    let mut system = match SystemBuilder::from_yaml(&args.config) {
        Ok(builder) => {
            info!("成功載入配置檔案: {}", args.config);
            // 顯示關鍵引擎配置（包含 stale_us）便於核對
            let cfg = builder.config().clone();
            info!(
                queue_capacity = cfg.engine.queue_capacity,
                stale_us = cfg.engine.stale_us,
                top_n = cfg.engine.top_n,
                "Engine 配置已載入"
            );
            // 檢查分片配置並應用
            let builder_with_sharding = if let (Some(shard_index), Some(shard_count)) =
                (args.shard_index, args.shard_count)
            {
                info!(
                    "配置靜態分片: {}/{} (策略: {})",
                    shard_index + 1,
                    shard_count,
                    args.shard_strategy
                );
                let strategy = ShardStrategy::from(args.shard_strategy.as_str());
                let shard_config = ShardConfig::new(shard_index, shard_count, strategy);
                builder.with_sharding(shard_config)
            } else if args.shard_index.is_some() || args.shard_count.is_some() {
                warn!("分片配置不完整，需要同時指定 --shard-index 和 --shard-count，忽略分片配置");
                builder
            } else {
                info!("未配置分片，將處理所有符號");
                builder
            };

            builder_with_sharding.auto_register_adapters().build()
        }
        Err(e) => {
            warn!("無法載入配置檔案: {}, 使用預設配置", e);
            // 構造一個合理的預設配置，確保行情與策略可運行
            // 注意：提高 stale_us 以避免本機/容器延遲造成事件被丟棄
            let engine = SystemEngineConfig {
                queue_capacity: 32768,
                stale_us: 500_000,
                top_n: 10,
                flip_policy: engine::dataflow::FlipPolicy::OnTimer(100_000),
                cpu_affinity: runtime::system_builder::CpuAffinityConfig::default(),
                ack_timeout_ms: 3000,
                reconcile_interval_ms: 5000,
                auto_cancel_exchange_only: false,
                execution_queue: ExecutionQueueSettings::default(),
            };
            let venues = vec![
                RtVenueConfig {
                    name: "bitget".to_string(),
                    account_id: Some("bitget_default".to_string()),
                    venue_type: VenueType::Bitget,
                    ws_public: Some("wss://ws.bitget.com/v2/ws/public".to_string()),
                    ws_private: Some("wss://ws.bitget.com/v2/ws/private".to_string()),
                    rest: Some("https://api.bitget.com".to_string()),
                    api_key: std::env::var("BITGET_API_KEY").ok(),
                    secret: std::env::var("BITGET_SECRET").ok(),
                    passphrase: std::env::var("BITGET_PASSPHRASE").ok(),
                    execution_mode: Some("Paper".to_string()),
                    capabilities: VenueCapabilities::default(),
                    inst_type: None,
                    simulate_execution: false,
                    symbol_catalog: Vec::new(),
                    data_config: None,
                    execution_config: None,
                    secret_ref_api_key: None,
                    secret_ref_secret: None,
                    secret_ref_passphrase: None,
                },
                RtVenueConfig {
                    name: "binance".to_string(),
                    account_id: Some("binance_default".to_string()),
                    venue_type: VenueType::Binance,
                    ws_public: Some("wss://stream.binance.com:9443/ws".to_string()),
                    ws_private: Some("wss://stream.binance.com:9443/ws".to_string()),
                    rest: Some("https://api.binance.com".to_string()),
                    api_key: std::env::var("BINANCE_API_KEY").ok(),
                    secret: std::env::var("BINANCE_SECRET").ok(),
                    passphrase: None,
                    execution_mode: Some("Paper".to_string()),
                    capabilities: VenueCapabilities::default(),
                    inst_type: None,
                    simulate_execution: false,
                    symbol_catalog: Vec::new(),
                    data_config: None,
                    execution_config: None,
                    secret_ref_api_key: None,
                    secret_ref_secret: None,
                    secret_ref_passphrase: None,
                },
            ];

            let mk_imb = |sym: &str| RtStrategyConfig {
                name: format!("imbalance_{}", sym),
                strategy_type: RtStrategyType::Imbalance,
                symbols: vec![hft_core::Symbol::new(sym)],
                params: RtStrategyParams::Imbalance {
                    obi_threshold: 0.2,
                    lot: rust_decimal::Decimal::try_from(0.01).unwrap(),
                    top_levels: 5,
                },
                risk_limits: RtStrategyRiskLimits {
                    max_notional: rust_decimal::Decimal::from(15000),
                    max_position: rust_decimal::Decimal::from(3),
                    daily_loss_limit: rust_decimal::Decimal::from(800),
                    cooldown_ms: 1000,
                },
            };

            let strategies = vec![mk_imb("ETHUSDT"), mk_imb("SOLUSDT"), mk_imb("SUIUSDT")];
            let risk = RtRiskConfig {
                risk_type: "Default".to_string(),
                global_position_limit: rust_decimal::Decimal::from(1_000_000),
                global_notional_limit: rust_decimal::Decimal::from(10_000_000),
                max_daily_trades: 10000,
                max_orders_per_second: 100,
                staleness_threshold_us: 5000,
                enhanced: None,
                strategy_overrides: Default::default(),
            };

            let cfg = SystemConfig {
                engine,
                venues,
                strategies,
                risk,
                quotes_only: false,
                router: None,
                infra: Some(runtime::system_builder::InfraConfig {
                    redis: Some(runtime::system_builder::RedisConfig {
                        url: "redis://127.0.0.1:6379".to_string(),
                    }),
                    clickhouse: Some(runtime::system_builder::ClickHouseConfig {
                        url: "http://localhost:8123".to_string(),
                        database: Some("hft".to_string()),
                    }),
                }),
                strategy_accounts: std::collections::HashMap::new(),
                accounts: Vec::new(),
                portfolios: Vec::new(),
            };

            let builder = SystemBuilder::new(cfg).auto_register_adapters();
            {
                // 顯示最終使用的引擎配置
                let cfg = builder.config().clone();
                info!(
                    queue_capacity = cfg.engine.queue_capacity,
                    stale_us = cfg.engine.stale_us,
                    top_n = cfg.engine.top_n,
                    "Engine 配置(預設)"
                );
            }
            builder.build()
        }
    };

    // 啟動系統
    system.start().await?;

    info!("系統正在運行...");

    // 啟動 Sentinel 風控 worker（默認啟用）
    let _sentinel_handle = if args.sentinel_enable {
        let sentinel_config = helpers::SentinelWorkerConfig {
            check_interval_ms: args.sentinel_interval_ms,
            latency_warn_us: args.sentinel_latency_warn_us,
            latency_degrade_us: args.sentinel_latency_warn_us * 2, // 降頻閾值 = 警告閾值 * 2
            drawdown_warn_pct: args.sentinel_drawdown_stop_pct * 0.4, // 警告 = 停止 * 0.4
            drawdown_stop_pct: args.sentinel_drawdown_stop_pct,
        };
        Some(helpers::spawn_sentinel_worker(
            system.engine.clone(),
            sentinel_config,
        ))
    } else {
        None
    };

    // 啟動推理 worker（可選）
    let _inference_handle = if args.ml_enable {
        Some(helpers::spawn_inference_worker(
            system.engine.clone(),
            args.ml_model.clone(),
            args.ml_k,
            args.ml_l,
            args.ml_step_ms,
        ))
    } else {
        None
    };

    // 啟動 metrics HTTP 服務器（如果啟用 metrics feature）
    #[cfg(feature = "metrics")]
    let _metrics_handle = helpers::spawn_metrics_server(system.engine.clone(), args.metrics_port);

    // 啟動 gRPC 控制服務（如果啟用 grpc feature）
    #[cfg(feature = "grpc")]
    let _grpc_handle = helpers::spawn_grpc_server(system.engine.clone(), args.grpc_port);

    // 可選：dry-run 下單驗證
    helpers::run_dry_run_if_enabled(&system, args.dry_run_order, &args.dry_run_symbol).await;

    // 保持運行直到收到停止信號或到達自動退出時間
    if let Some(ms) = args.exit_after_ms {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = tokio::time::sleep(std::time::Duration::from_millis(ms)) => {},
        }
    } else {
        tokio::signal::ctrl_c().await?;
    }

    info!("收到停止信號，正在關閉系統...");

    // 優雅停止系統
    system.stop().await?;

    Ok(())
}

fn show_available_adapters() {
    info!("可用適配器:");

    #[cfg(feature = "bitget")]
    info!("  ✓ Bitget 交易所");

    #[cfg(feature = "binance")]
    info!("  ✓ Binance 交易所");

    #[cfg(feature = "okx")]
    info!("  ✓ OKX 交易所");

    #[cfg(feature = "mock")]
    info!("  ✓ 模擬交易所");

    #[cfg(feature = "trend-strategy")]
    info!("  ✓ 趨勢策略");

    #[cfg(feature = "arbitrage-strategy")]
    info!("  ✓ 套利策略");

    #[cfg(feature = "dl-strategy")]
    info!("  ✓ 深度學習策略");

    #[cfg(feature = "metrics")]
    info!("  ✓ 指標監控");

    #[cfg(feature = "clickhouse")]
    info!("  ✓ ClickHouse 存儲");

    #[cfg(feature = "redis")]
    info!("  ✓ Redis 快取");
}

// Legacy config structs removed - now using SystemBuilder from YAML directly
