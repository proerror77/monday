//! HFT 真盤交易應用
//! 
//! 特性開關示例：
//! - cargo run --features="bitget,trend-strategy,metrics"
//! - cargo run --features="full"  # 開啟所有特性

use clap::Parser;
use tracing::{info, warn};
use serde::{Deserialize};
use std::fs;
use runtime::{SystemBuilder, SystemConfig, ShardConfig, ShardStrategy};
use runtime::{SystemEngineConfig, VenueConfig as RtVenueConfig, VenueType, VenueCapabilities, StrategyConfig as RtStrategyConfig, StrategyType as RtStrategyType, StrategyParams as RtStrategyParams, StrategyRiskLimits as RtStrategyRiskLimits, RiskConfig as RtRiskConfig};
use data::capabilities; // only for listing, not for config
// 用於 Prometheus TextEncoder::encode
#[cfg(feature = "metrics")]
use prometheus::Encoder;

#[cfg(feature = "metrics")]
use std::sync::Arc;
use std::collections::HashMap;
use hft_infer_onnx::OnnxPredictor;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// 配置檔案路徑
    #[arg(short, long, default_value = "config/dev/system.yaml")]
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
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .init();
    
    let args = Args::parse();
    
    info!("啟動 HFT 真盤交易系統");
    info!("配置檔案: {}", args.config);

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
            let builder_with_sharding = if let (Some(shard_index), Some(shard_count)) = (args.shard_index, args.shard_count) {
                info!("配置靜態分片: {}/{} (策略: {})", shard_index + 1, shard_count, args.shard_strategy);
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
            let engine = SystemEngineConfig { queue_capacity: 32768, stale_us: 500_000, top_n: 10, flip_policy: engine::dataflow::FlipPolicy::OnTimer(100_000) };
            let venues = vec![
                RtVenueConfig {
                    name: "bitget".to_string(),
                    venue_type: VenueType::Bitget,
                    ws_public: Some("wss://ws.bitget.com/v2/ws/public".to_string()),
                    ws_private: Some("wss://ws.bitget.com/v2/ws/private".to_string()),
                    rest: Some("https://api.bitget.com".to_string()),
                    api_key: std::env::var("BITGET_API_KEY").ok(),
                    secret: std::env::var("BITGET_SECRET").ok(),
                    passphrase: std::env::var("BITGET_PASSPHRASE").ok(),
                    execution_mode: Some("Paper".to_string()),
                    capabilities: VenueCapabilities::default(),
                },
                RtVenueConfig {
                    name: "binance".to_string(),
                    venue_type: VenueType::Binance,
                    ws_public: Some("wss://stream.binance.com:9443/ws".to_string()),
                    ws_private: Some("wss://stream.binance.com:9443/ws".to_string()),
                    rest: Some("https://api.binance.com".to_string()),
                    api_key: std::env::var("BINANCE_API_KEY").ok(),
                    secret: std::env::var("BINANCE_SECRET").ok(),
                    passphrase: None,
                    execution_mode: Some("Paper".to_string()),
                    capabilities: VenueCapabilities::default(),
                },
            ];

            let mk_imb = |sym: &str| RtStrategyConfig {
                name: format!("imbalance_{}", sym),
                strategy_type: RtStrategyType::Imbalance,
                symbols: vec![hft_core::Symbol(sym.to_string())],
                params: RtStrategyParams::Imbalance { obi_threshold: 0.2, lot: rust_decimal::Decimal::try_from(0.01).unwrap(), top_levels: 5 },
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

            let cfg = SystemConfig { engine, venues, strategies, risk, router: None, infra: Some(runtime::system_builder::InfraConfig { redis: Some(runtime::system_builder::RedisConfig { url: "redis://127.0.0.1:6379".to_string() }), clickhouse: Some(runtime::system_builder::ClickHouseConfig { url: "http://localhost:8123".to_string(), database: Some("hft".to_string()) }) }) };

            let mut builder = SystemBuilder::new(cfg).auto_register_adapters();
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
        info!("啟用 ONNX 推理: model={}, K={}, L={}, step_ms={}", args.ml_model, args.ml_k, args.ml_l, args.ml_step_ms);
        let engine_arc = system.engine.clone();
        let model_path = args.ml_model.clone();
        let k = args.ml_k; let l = args.ml_l; let step_ms = args.ml_step_ms;
        tokio::spawn(async move {
            if let Err(e) = run_infer_worker(engine_arc, &model_path, k, l, step_ms).await { warn!("推理 worker 失敗: {}", e); }
        });
    }

    // 啟動 metrics HTTP 服務器（如果啟用 metrics feature）
    #[cfg(feature = "metrics")]
    let metrics_handle = {
        let port = args.metrics_port;
        let engine_arc = system.engine.clone();
        info!("啟動 Axum Metrics HTTP 服務器於端口 {}", port);
        tokio::spawn(run_axum_metrics_server(engine_arc, port))
    };
    
    // 可選：dry-run 下單驗證
    if args.dry_run_order {
        info!("執行 dry-run 下單驗證，symbol={}", args.dry_run_symbol);
        match system.place_test_order(&args.dry_run_symbol).await {
            Ok(order_id) => {
                info!("dry-run 下單成功，order_id={}", order_id.0);
                // 等待回報處理
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                let av = system.get_account_view().await;
                info!(cash = av.cash_balance, positions = av.positions.len(), unrealized = av.unrealized_pnl, realized = av.realized_pnl, "AccountView 更新");
            }
            Err(e) => warn!("dry-run 下單失敗: {}", e),
        }
    }

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

async fn run_infer_worker(
    engine_arc: std::sync::Arc<tokio::sync::Mutex<engine::Engine>>,
    model_path: &str,
    k: usize,
    l: usize,
    step_ms: u64,
) -> anyhow::Result<()> {
    use engine::aggregation::TopNSnapshot;
    use rust_decimal::prelude::ToPrimitive;
    let predictor = OnnxPredictor::load(model_path, (1, 4, l, k))?;
    let mut windows: HashMap<String, Vec<(Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>)>> = HashMap::new();
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(step_ms));
    loop {
        interval.tick().await;
        // 若引擎停止則退出
        {
            let eng = engine_arc.lock().await;
            if !eng.get_statistics().is_running { break; }
        }
        let market_view = { let eng = engine_arc.lock().await; eng.get_market_view() };
        for (sym, ob) in &market_view.orderbooks {
            // 取 Top-K 並轉為 (p-mid, log qty)
            if ob.bid_prices.is_empty() || ob.ask_prices.is_empty() { continue; }
            let mid = (ob.bid_prices[0].to_f64() + ob.ask_prices[0].to_f64())/2.0;
            let mut bid_px_rel = Vec::with_capacity(k);
            let mut bid_qty_log = Vec::with_capacity(k);
            let mut ask_px_rel = Vec::with_capacity(k);
            let mut ask_qty_log = Vec::with_capacity(k);
            for i in 0..k {
                let bp = ob.bid_prices.get(i).map(|p| p.to_f64()).unwrap_or(0.0) as f32;
                let bq = ob.bid_quantities.get(i).map(|q| q.to_f64()).unwrap_or(0.0) as f32;
                let ap = ob.ask_prices.get(i).map(|p| p.to_f64()).unwrap_or(0.0) as f32;
                let aq = ob.ask_quantities.get(i).map(|q| q.to_f64()).unwrap_or(0.0) as f32;
                bid_px_rel.push((bp - mid as f32) as f32);
                ask_px_rel.push((ap - mid as f32) as f32);
                bid_qty_log.push((1.0 + bq).ln());
                ask_qty_log.push((1.0 + aq).ln());
            }
            let entry = windows.entry(sym.0.clone()).or_default();
            entry.push((bid_px_rel, bid_qty_log, ask_px_rel, ask_qty_log));
            if entry.len() > l { entry.remove(0); }
            if entry.len() == l {
                // 組平面向量 (1*4*L*K)
                let mut flat = Vec::with_capacity(4*l*k);
                for t in 0..l {
                    let (ref bpr, ref bql, ref apr, ref aql) = entry[t];
                    flat.extend_from_slice(&bpr[..]);
                    flat.extend_from_slice(&bql[..]);
                    flat.extend_from_slice(&apr[..]);
                    flat.extend_from_slice(&aql[..]);
                }
                match predictor.infer(&flat) {
                    Ok(probs) => { info!(symbol = %sym.0, ts = market_view.timestamp, ?probs, "ONNX 推理"); }
                    Err(e) => { warn!(symbol = %sym.0, "推理失敗: {}", e); }
                }
            }
        }
    }
    Ok(())
}

fn show_available_adapters() {
    info!("可用適配器:");
    
    #[cfg(feature = "bitget")]
    info!("  ✓ Bitget 交易所");
    
    #[cfg(feature = "binance")]
    info!("  ✓ Binance 交易所");
    
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
struct Config { engine: EngineConfig, venues: Vec<VenueConfig>, infra: Option<InfraConfig> }

#[derive(Debug, Deserialize)]
struct EngineConfig { queue_capacity: usize, stale_us: u64, top_n: usize, #[allow(dead_code)] flip_policy: Option<String> }

#[derive(Debug, Deserialize)]
struct VenueConfig { name: String, ws_public: Option<String>, ws_private: Option<String>, rest: Option<String>, api_key: Option<String>, secret: Option<String>, #[allow(dead_code)] capabilities: Option<CapabilitiesConfig> }

#[derive(Debug, Deserialize)]
struct CapabilitiesConfig { #[allow(dead_code)] ws_order: Option<bool>, #[allow(dead_code)] snapshot_crc: Option<bool>, #[allow(dead_code)] all_in_one_topics: Option<bool> }

#[derive(Debug, Deserialize)]
struct InfraConfig { #[allow(dead_code)] clickhouse: Option<ClickhouseCfg>, #[allow(dead_code)] redis: Option<RedisCfg> }

#[derive(Debug, Deserialize)]
struct ClickhouseCfg { #[allow(dead_code)] url: String, #[allow(dead_code)] auth: Option<String>, #[allow(dead_code)] table_map: Option<String>, #[allow(dead_code)] batch_bytes: Option<usize> }

#[derive(Debug, Deserialize)]
struct RedisCfg { #[allow(dead_code)] url: String }

fn load_config(path: &str) -> Result<Config, Box<dyn std::error::Error>> {
    let s = fs::read_to_string(path)?;
    let cfg: Config = serde_yaml::from_str(&s)?;
    Ok(cfg)
}

/// Axum 驅動的 HTTP 服務器提供 /metrics 端點
#[cfg(feature = "metrics")]
async fn run_axum_metrics_server(
    engine_arc: Arc<tokio::sync::Mutex<engine::Engine>>, 
    port: u16
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use infra_metrics::http_server::{MetricsServer, MetricsServerConfig};
    
    // 創建 metrics 服務器配置
    let config = MetricsServerConfig {
        bind_address: "0.0.0.0".to_string(),
        port,
        verbose_logging: false,
        readiness_max_idle_secs: 5,
        readiness_max_utilization: 0.9,
    };
    
    let server = MetricsServer::new(config);
    
    info!("Metrics 服務器監聽於 http://0.0.0.0:{}/metrics", port);
    
    // 在後台定期從 engine 同步延遲統計到 Prometheus
    let sync_engine_arc = engine_arc.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            interval.tick().await;
            
            // 從 Engine 獲取最新的延遲統計
            if let Ok(engine) = sync_engine_arc.try_lock() {
                let latency_stats = engine.get_latency_stats();
                if !latency_stats.is_empty() {
                    infra_metrics::MetricsRegistry::global()
                        .update_from_latency_monitor(&latency_stats);
                }
            }
        }
    });
    
    // 啟動 axum 服務器（阻塞於該任務直到關閉）
    server.start().await
}

/// 生成基本的 Prometheus 格式指標
#[cfg(feature = "metrics")]
async fn generate_basic_metrics(engine_arc: Arc<tokio::sync::Mutex<engine::Engine>>) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    // 擷取當前帳戶與引擎統計
    let (av, stats) = {
        let eng = engine_arc.lock().await;
        // 將 LatencyMonitor 統計同步到 Prometheus （若啟用 metrics）
        #[cfg(feature = "metrics")]
        {
            eng.sync_latency_metrics_to_prometheus();
        }
        (eng.get_account_view(), eng.get_statistics())
    };

    // 位置指標（每個 symbol 一個 gauge）
    let mut position_lines = String::new();
    for (sym, pos) in &av.positions {
        use std::fmt::Write as _;
        let _ = writeln!(
            position_lines,
            "hft_position_quantity{{symbol=\"{}\"}} {}",
            sym.0, pos.quantity.0
        );
        let _ = writeln!(
            position_lines,
            "hft_position_avg_price{{symbol=\"{}\"}} {}",
            sym.0, pos.avg_price.0
        );
    }

    // 衍生總 PnL 與 Equity（僅供顯示；equity= cash + realized + unrealized）
    let total_pnl = av.realized_pnl + av.unrealized_pnl;
    let equity = av.cash_balance + total_pnl;

    let custom = format!(
        r#"# HELP hft_system_uptime_seconds HFT system uptime in seconds
# TYPE hft_system_uptime_seconds counter
hft_system_uptime_seconds {}

# HELP hft_build_info Build information
# TYPE hft_build_info gauge
hft_build_info{{version="{}"}} 1

# HELP hft_account_cash_balance Account cash balance
# TYPE hft_account_cash_balance gauge
hft_account_cash_balance {}

# HELP hft_account_unrealized_pnl Account unrealized PnL
# TYPE hft_account_unrealized_pnl gauge
hft_account_unrealized_pnl {}

# HELP hft_account_realized_pnl Account realized PnL
# TYPE hft_account_realized_pnl gauge
hft_account_realized_pnl {}

# HELP hft_account_total_pnl Account total PnL (realized + unrealized)
# TYPE hft_account_total_pnl gauge
hft_account_total_pnl {}

# HELP hft_account_equity Account equity (cash + total_pnl)
# TYPE hft_account_equity gauge
hft_account_equity {}

# HELP hft_account_positions Number of open positions
# TYPE hft_account_positions gauge
hft_account_positions {}

# HELP hft_engine_cycles_total Engine tick cycles total
# TYPE hft_engine_cycles_total counter
hft_engine_cycles_total {}

# HELP hft_engine_exec_events_total Execution events processed total
# TYPE hft_engine_exec_events_total counter
hft_engine_exec_events_total {}

# HELP hft_orders_submitted_total Orders submitted (OrderNew)
# TYPE hft_orders_submitted_total counter
hft_orders_submitted_total {}

# HELP hft_orders_ack_total Orders acknowledged
# TYPE hft_orders_ack_total counter
hft_orders_ack_total {}

# HELP hft_orders_filled_total Orders filled
# TYPE hft_orders_filled_total counter
hft_orders_filled_total {}

# HELP hft_orders_rejected_total Orders rejected
# TYPE hft_orders_rejected_total counter
hft_orders_rejected_total {}

# HELP hft_orders_canceled_total Orders canceled
# TYPE hft_orders_canceled_total counter
hft_orders_canceled_total {}

# HELP hft_position_quantity Per-symbol position quantity
# TYPE hft_position_quantity gauge
# HELP hft_position_avg_price Per-symbol average price
# TYPE hft_position_avg_price gauge
{}
"#,
        now / 1000, // uptime approximation  
        env!("CARGO_PKG_VERSION"),
        av.cash_balance,
        av.unrealized_pnl,
        av.realized_pnl,
        total_pnl,
        equity,
        av.positions.len(),
        stats.cycle_count,
        stats.execution_events_processed,
        stats.orders_submitted,
        stats.orders_ack,
        stats.orders_filled,
        stats.orders_rejected,
        stats.orders_canceled,
        position_lines,
    );

    // 追加 Prometheus Registry 直方圖/計數器
    let mut combined = custom;
    #[cfg(feature = "metrics")]
    {
        let mf = infra_metrics::MetricsRegistry::global().registry().gather();
        let mut buf = Vec::new();
        let encoder = prometheus::TextEncoder::new();
        if encoder.encode(&mf, &mut buf).is_ok() {
            let text = String::from_utf8_lossy(&buf);
            combined.push_str("\n");
            combined.push_str(&text);
        }
    }

    combined
}

/// 轉換舊配置格式到新的 SystemConfig
fn convert_legacy_config(legacy: Config) -> SystemConfig {
    use runtime::{SystemEngineConfig, VenueConfig, VenueType, VenueCapabilities, RiskConfig};
    use engine::dataflow::FlipPolicy;
    
    SystemConfig {
        engine: SystemEngineConfig {
            queue_capacity: legacy.engine.queue_capacity,
            stale_us: legacy.engine.stale_us,
            top_n: legacy.engine.top_n,
            flip_policy: FlipPolicy::OnUpdate,
        },
        venues: legacy.venues.into_iter().map(|v| {
            let venue_type = match v.name.to_lowercase().as_str() {
                "bitget" => VenueType::Bitget,
                "binance" => VenueType::Binance,
                "bybit" => VenueType::Bybit,
                _ => {
                    warn!("未知交易所類型: {}, 默認為 Bitget", v.name);
                    VenueType::Bitget
                }
            };
            
            VenueConfig {
                name: v.name,
                venue_type,
                ws_public: v.ws_public,
                ws_private: v.ws_private,
                rest: v.rest,
                api_key: v.api_key,
                secret: v.secret,
                passphrase: None,  // 新增字段：預設為 None
                execution_mode: Some("Paper".to_string()),  // 新增字段：預設為 Paper
                capabilities: VenueCapabilities::default(),
            }
        }).collect(),
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
    }
}
