//! HFT 真盤交易應用
//!
//! 特性開關示例：
//! - cargo run --features="bitget,trend-strategy,metrics"
//! - cargo run --features="full"  # 開啟所有特性

mod helpers;

use clap::Parser;
use runtime::{
    RiskConfig as RtRiskConfig, StrategyConfig as RtStrategyConfig,
    StrategyParams as RtStrategyParams, StrategyRiskLimits as RtStrategyRiskLimits,
    StrategyType as RtStrategyType, SystemEngineConfig, VenueCapabilities,
    VenueConfig as RtVenueConfig, VenueType,
};
use runtime::{ShardConfig, ShardStrategy, SystemBuilder, SystemConfig};
use serde::Deserialize;
use std::fs;
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
                    max_notional: rust_decimal::Decimal::try_from(15000).unwrap(),
                    max_position: rust_decimal::Decimal::try_from(3).unwrap(),
                    daily_loss_limit: rust_decimal::Decimal::try_from(800).unwrap(),
                    cooldown_ms: 1000,
                },
            };

            let strategies = vec![mk_imb("ETHUSDT"), mk_imb("SOLUSDT"), mk_imb("SUIUSDT")];
            let risk = RtRiskConfig {
                risk_type: "Default".to_string(),
                global_position_limit: rust_decimal::Decimal::try_from(1_000_000).unwrap(),
                global_notional_limit: rust_decimal::Decimal::try_from(10_000_000).unwrap(),
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

    // 啟動推理 worker（可選）
    if args.ml_enable {
        let _ = helpers::spawn_inference_worker(
            system.engine.clone(),
            args.ml_model.clone(),
            args.ml_k,
            args.ml_l,
            args.ml_step_ms,
        );
    }

    // 啟動 metrics HTTP 服務器（如果啟用 metrics feature）
    #[cfg(feature = "metrics")]
    let _metrics_handle = helpers::spawn_metrics_server(system.engine.clone(), args.metrics_port);

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

// Removed dead code: register_adapters() was never called
// This functionality is now handled by SystemBuilder

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct Config {
    engine: EngineConfig,
    venues: Vec<VenueConfig>,
    infra: Option<InfraConfig>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct EngineConfig {
    queue_capacity: usize,
    stale_us: u64,
    top_n: usize,
    #[allow(dead_code)]
    flip_policy: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct VenueConfig {
    name: String,
    ws_public: Option<String>,
    ws_private: Option<String>,
    rest: Option<String>,
    api_key: Option<String>,
    secret: Option<String>,
    #[allow(dead_code)]
    capabilities: Option<CapabilitiesConfig>,
    #[allow(dead_code)]
    #[serde(default)]
    simulate_execution: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct CapabilitiesConfig {
    #[allow(dead_code)]
    ws_order: Option<bool>,
    #[allow(dead_code)]
    snapshot_crc: Option<bool>,
    #[allow(dead_code)]
    all_in_one_topics: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct InfraConfig {
    #[allow(dead_code)]
    clickhouse: Option<ClickhouseCfg>,
    #[allow(dead_code)]
    redis: Option<RedisCfg>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ClickhouseCfg {
    #[allow(dead_code)]
    url: String,
    #[allow(dead_code)]
    auth: Option<String>,
    #[allow(dead_code)]
    table_map: Option<String>,
    #[allow(dead_code)]
    batch_bytes: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct RedisCfg {
    #[allow(dead_code)]
    url: String,
}

#[allow(dead_code)]
fn load_config(path: &str) -> Result<Config, Box<dyn std::error::Error>> {
    let s = fs::read_to_string(path)?;
    let cfg: Config = serde_yaml::from_str(&s)?;
    Ok(cfg)
}

/// 轉換舊配置格式到新的 SystemConfig
#[allow(dead_code)]
fn convert_legacy_config(legacy: Config) -> SystemConfig {
    use engine::dataflow::FlipPolicy;
    use runtime::{RiskConfig, SystemEngineConfig, VenueCapabilities, VenueConfig, VenueType};

    SystemConfig {
        engine: SystemEngineConfig {
            queue_capacity: legacy.engine.queue_capacity,
            stale_us: legacy.engine.stale_us,
            top_n: legacy.engine.top_n,
            flip_policy: FlipPolicy::OnUpdate,
            cpu_affinity: runtime::system_builder::CpuAffinityConfig::default(),
            ack_timeout_ms: 3000,
            reconcile_interval_ms: 5000,
            auto_cancel_exchange_only: false,
        },
        venues: legacy
            .venues
            .into_iter()
            .map(|v| {
                let venue_type = match v.name.to_lowercase().as_str() {
                    "bitget" => VenueType::Bitget,
                    "binance" => VenueType::Binance,
                    "bybit" => VenueType::Bybit,
                    "grvt" => VenueType::Grvt,
                    "hyperliquid" => VenueType::Hyperliquid,
                    "backpack" => VenueType::Backpack,
                    "asterdex" | "aster" => VenueType::Asterdex,
                    _ => {
                        warn!("未知交易所類型: {}, 默認為 Bitget", v.name);
                        VenueType::Bitget
                    }
                };

                VenueConfig {
                    name: v.name,
                    account_id: None,
                    venue_type,
                    ws_public: v.ws_public,
                    ws_private: v.ws_private,
                    rest: v.rest,
                    api_key: v.api_key,
                    secret: v.secret,
                    passphrase: None, // 新增字段：預設為 None
                    execution_mode: Some("Paper".to_string()), // 新增字段：預設為 Paper
                    capabilities: VenueCapabilities::default(),
                    inst_type: None,
                    simulate_execution: v.simulate_execution.unwrap_or(false),
                    symbol_catalog: Vec::new(),
                    data_config: None,
                    execution_config: None,
                }
            })
            .collect(),
        strategies: Vec::new(), // 稍後從 YAML 讀取
        risk: RiskConfig {
            risk_type: "Default".to_string(),
            global_position_limit: rust_decimal::Decimal::from(1000000),
            global_notional_limit: rust_decimal::Decimal::from(10000000),
            max_daily_trades: 10000,
            max_orders_per_second: 100,
            staleness_threshold_us: 5000,
            enhanced: None,
            strategy_overrides: Default::default(),
        },
        router: None,
        infra: None,
        quotes_only: false,
        strategy_accounts: std::collections::HashMap::new(),
        accounts: Vec::new(),
        portfolios: Vec::new(),
    }
}
