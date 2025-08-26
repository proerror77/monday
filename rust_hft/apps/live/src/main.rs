//! HFT 真盤交易應用
//! 
//! 特性開關示例：
//! - cargo run --features="bitget,trend-strategy,metrics"
//! - cargo run --features="full"  # 開啟所有特性

use clap::Parser;
use tracing::{info, warn};
use serde::{Deserialize};
use std::fs;
use runtime::{SystemBuilder, SystemConfig};
use data::capabilities; // only for listing, not for config

#[cfg(feature = "metrics")]
use std::sync::Arc;
#[cfg(feature = "metrics")]
use tokio::net::TcpListener;

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
            builder.auto_register_adapters().build()
        }
        Err(e) => {
            warn!("無法載入配置檔案: {}, 使用預設配置", e);
            SystemBuilder::new(SystemConfig::default())
                .auto_register_adapters()
                .build()
        }
    };
    
    // 啟動系統
    system.start().await?;
    
    info!("系統正在運行...");

    // 啟動 metrics HTTP 服務器（如果啟用 metrics feature）
    #[cfg(feature = "metrics")]
    let metrics_handle = {
        let port = args.metrics_port;
        info!("啟動 Metrics HTTP 服務器於端口 {}", port);
        tokio::spawn(run_metrics_server(port))
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

/// 輕量級 HTTP 服務器提供 /metrics 端點
#[cfg(feature = "metrics")]
async fn run_metrics_server(port: u16) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    
    let listener = TcpListener::bind(format!("0.0.0.0:{}", port)).await?;
    info!("Metrics 服務器監聽於 http://0.0.0.0:{}/metrics", port);
    
    loop {
        let (stream, _) = listener.accept().await?;
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        
        // 讀取 HTTP 請求第一行
        if reader.read_line(&mut line).await? == 0 {
            continue; // 連接關閉
        }
        
        // 簡單解析：GET /metrics HTTP/1.1
        if line.starts_with("GET /metrics") {
            // 讀取剩餘的 headers（忽略）
            let mut buffer = String::new();
            while reader.read_line(&mut buffer).await? > 2 { // 直到空行
                buffer.clear();
            }
            
            // 生成 Prometheus 格式的 metrics
            let metrics_data = generate_basic_metrics().await;
            
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\n\r\n{}",
                metrics_data.len(),
                metrics_data
            );
            
            write_half.write_all(response.as_bytes()).await?;
        } else if line.starts_with("GET /") {
            // 其他路徑返回 404
            let response = "HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\n\r\nNot Found";
            write_half.write_all(response.as_bytes()).await?;
        }
        
        let _ = write_half.flush().await;
    }
}

/// 生成基本的 Prometheus 格式指標
#[cfg(feature = "metrics")]
async fn generate_basic_metrics() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    
    format!(
        r#"# HELP hft_system_uptime_seconds HFT system uptime in seconds
# TYPE hft_system_uptime_seconds counter
hft_system_uptime_seconds {}

# HELP hft_build_info Build information
# TYPE hft_build_info gauge
hft_build_info{{version="{}"}} 1

# HELP hft_metrics_requests_total Total number of metrics requests
# TYPE hft_metrics_requests_total counter
hft_metrics_requests_total {}
"#,
        now / 1000, // uptime approximation  
        env!("CARGO_PKG_VERSION"),
        1, // 簡化的請求計數
    )
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
            global_position_limit: rust_decimal::Decimal::from(1000000),
            global_notional_limit: rust_decimal::Decimal::from(10000000),
            max_daily_trades: 10000,
            max_orders_per_second: 100,
            staleness_threshold_us: 5000,
        },
    }
}
