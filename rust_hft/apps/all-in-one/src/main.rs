use clap::Parser;
use runtime::{ShardConfig, ShardStrategy, SystemBuilder};
use std::error::Error;
use std::time::Duration;
use tracing::{info, warn};

#[derive(Parser, Debug)]
#[command(author, version, about = "HFT all-in-one runtime", long_about = None)]
struct Args {
    /// 系統設定檔案路徑
    #[arg(short, long, default_value = "config/dev/system.yaml")]
    config: String,

    /// 僅建構系統（不啟動），可用於驗證配置
    #[arg(long)]
    build_only: bool,

    /// 跳過自動註冊適配器，改由外部程式碼手動註冊
    #[arg(long)]
    no_auto_register: bool,

    /// 啟動後自動結束前的等待毫秒數，未設定時改為等待 Ctrl+C
    #[arg(long)]
    exit_after_ms: Option<u64>,

    /// 分片索引 (0-based)
    #[arg(long)]
    shard_index: Option<u32>,

    /// 分片總數
    #[arg(long)]
    shard_count: Option<u32>,

    /// 分片策略 (symbol-hash, venue-round-robin, hybrid)
    #[arg(long, default_value = "symbol-hash")]
    shard_strategy: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_target(false)
        .init();

    let args = Args::parse();
    info!(config = %args.config, "啟動 hft-all-in-one");

    let mut builder = SystemBuilder::from_yaml(&args.config).map_err(|e| {
        warn!("載入配置失敗: {}", e);
        e
    })?;

    if let (Some(index), Some(count)) = (args.shard_index, args.shard_count) {
        let strategy = ShardStrategy::from(args.shard_strategy.as_str());
        let shard_config = ShardConfig::new(index, count, strategy);
        info!(
            "啟用分片: {}/{} ({})",
            index + 1,
            count,
            args.shard_strategy
        );
        builder = builder.with_sharding(shard_config);
    } else if args.shard_index.is_some() || args.shard_count.is_some() {
        warn!("分片參數不完整，忽略分片設定");
    }

    if !args.no_auto_register {
        builder = builder.auto_register_adapters();
    }

    let mut system = builder.build();
    info!("系統建構完成");

    if args.build_only {
        info!("建構檢查完成 (build-only mode)，未啟動運行時");
        return Ok(());
    }

    system.start().await?;
    info!("系統啟動完成");

    if let Some(ms) = args.exit_after_ms {
        info!("將在 {} ms 後自動停止", ms);
        tokio::time::sleep(Duration::from_millis(ms)).await;
    } else {
        info!("按 Ctrl+C 結束");
        tokio::signal::ctrl_c().await?;
    }

    system.stop().await?;
    info!("系統已停止");
    Ok(())
}
