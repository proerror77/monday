//! HFT 模擬盤應用
//!
//! 零風險模式，適用於策略驗證和調試

use clap::Parser;
use runtime::{SystemBuilder, SystemConfig};
use tracing::{info, warn};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// 配置檔案路徑
    #[arg(short, long, default_value = "config/dev/system.yaml")]
    config: String,

    /// 啟動後自動退出的毫秒數（用於測試）
    #[arg(long)]
    exit_after_ms: Option<u64>,

    /// Metrics HTTP 服務器端口（啟用 metrics feature 時生效）
    #[cfg(feature = "metrics")]
    #[arg(long, default_value = "9091")]
    metrics_port: u16,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 初始化日誌
    tracing_subscriber::fmt().with_env_filter("info").init();

    let args = Args::parse();

    info!("啟動 HFT 模擬盤系統");
    info!("配置檔案: {}", args.config);

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

    info!("模擬盤系統正在運行...");

    // 啟動 metrics HTTP 服務器（如果啟用 metrics feature）
    #[cfg(feature = "metrics")]
    {
        use infra_metrics::http_server::{MetricsServer, MetricsServerConfig};
        let cfg = MetricsServerConfig {
            bind_address: "0.0.0.0".to_string(),
            port: args.metrics_port,
            verbose_logging: false,
            readiness_max_idle_secs: 5,
            readiness_max_utilization: 0.9,
        };
        let _handle = MetricsServer::start_background(cfg);
        info!(
            "Metrics 服務器已啟動: http://0.0.0.0:{}/metrics",
            args.metrics_port
        );
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
