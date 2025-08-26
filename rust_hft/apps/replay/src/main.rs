//! HFT 歷史回放應用
//! 
//! 用於歷史數據回放、回測和策略驗證

use clap::Parser;
use tracing::{info, warn};
use runtime::{SystemBuilder, SystemConfig};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// 配置檔案路徑
    #[arg(short, long, default_value = "config/dev/system.yaml")]
    config: String,
    
    /// 回放數據檔案路徑
    #[arg(long)]
    data_file: Option<String>,
    
    /// 回放開始時間 (YYYY-MM-DD HH:MM:SS)
    #[arg(long)]
    start_time: Option<String>,
    
    /// 回放結束時間 (YYYY-MM-DD HH:MM:SS)
    #[arg(long)]
    end_time: Option<String>,
    
    /// 回放速度倍數 (1.0 = 原速)
    #[arg(long, default_value = "1.0")]
    speed: f64,
    
    /// 啟動後自動退出的毫秒數（用於測試）
    #[arg(long)]
    exit_after_ms: Option<u64>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 初始化日誌
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .init();
    
    let args = Args::parse();
    
    info!("啟動 HFT 歷史回放系統");
    info!("配置檔案: {}", args.config);
    
    if let Some(ref data_file) = args.data_file {
        info!("數據檔案: {}", data_file);
    }
    if let Some(ref start_time) = args.start_time {
        info!("開始時間: {}", start_time);
    }
    if let Some(ref end_time) = args.end_time {
        info!("結束時間: {}", end_time);
    }
    info!("回放速度: {}x", args.speed);

    // 使用 SystemBuilder 從 YAML 配置建構系統
    let mut system = match SystemBuilder::from_yaml(&args.config) {
        Ok(builder) => {
            info!("成功載入配置檔案: {}", args.config);
            // TODO: 添加回放模式適配器配置
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
    
    info!("歷史回放系統正在運行...");

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

