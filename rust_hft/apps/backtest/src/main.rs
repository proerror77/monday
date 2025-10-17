mod config;
mod engine;
mod event;

use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::Parser;
use tracing::info;

use config::{BacktestConfig, OutputConfig, StrategyConfig};
use engine::{BacktestEngine, BacktestResult, SummaryMetrics, TradeRecord};

#[derive(Parser, Debug)]
#[command(author, version, about = "L3 事件重放回測模板", long_about = None)]
struct Args {
    /// 回測設定檔
    #[arg(long, default_value = "config/backtest/default.yaml")]
    config: String,
    /// 覆蓋策略參數檔（可只保留 strategy 區段）
    #[arg(long)]
    params: Option<String>,
    /// 輸出目錄（預設寫入目前路徑）
    #[arg(long)]
    output_dir: Option<String>,
    /// 僅檢查配置，不運行回測
    #[arg(long)]
    dry_run: bool,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_target(false).init();

    let args = Args::parse();

    let mut cfg = BacktestConfig::from_file(&args.config)?;
    if let Some(params_path) = args.params {
        apply_strategy_overrides(&mut cfg, &params_path)?;
    }

    if args.dry_run {
        info!("dry-run 模式：僅檢查配置成功");
        return Ok(());
    }

    info!("載入事件：{}", cfg.data.path);
    let mut engine = BacktestEngine::new(cfg.clone());
    let result = engine.run()?;
    info!(
        "回測完成：交易筆數={}，總損益={:.4}",
        result.summary.trades, result.summary.total_pnl
    );

    write_outputs(&cfg.output, args.output_dir.as_deref(), &result)?;
    print_summary(&result.summary);

    Ok(())
}

fn apply_strategy_overrides(cfg: &mut BacktestConfig, path: &str) -> anyhow::Result<()> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("無法讀取參數檔: {}", path))?;

    // 允許兩種格式：直接是 strategy 欄位，或含包裝
    #[derive(serde::Deserialize)]
    struct Wrapper {
        strategy: StrategyConfig,
    }

    let strategy = match serde_yaml::from_str::<StrategyConfig>(&content) {
        Ok(strategy) => strategy,
        Err(_) => {
            let wrapper: Wrapper = serde_yaml::from_str(&content)
                .with_context(|| format!("解析策略參數檔失敗: {}", path))?;
            wrapper.strategy
        }
    };

    cfg.strategy = strategy;
    Ok(())
}

fn write_outputs(
    output: &OutputConfig,
    override_dir: Option<&str>,
    result: &BacktestResult,
) -> anyhow::Result<()> {
    let out_dir = override_dir
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    if !out_dir.exists() {
        std::fs::create_dir_all(&out_dir)
            .with_context(|| format!("無法建立輸出目錄: {}", out_dir.display()))?;
    }

    write_trades_csv(&out_dir.join(&output.trades_csv), &result.trades)?;
    write_summary_csv(&out_dir.join(&output.summary_csv), &result.summary)?;

    if let Some(metrics_path) = &output.metrics_json {
        write_metrics_json(&out_dir.join(metrics_path), &result.summary)?;
    }
    Ok(())
}

fn write_trades_csv(path: &Path, trades: &[TradeRecord]) -> anyhow::Result<()> {
    let mut writer = csv::Writer::from_path(path)
        .with_context(|| format!("無法寫入交易檔案: {}", path.display()))?;
    writer.write_record([
        "entry_ts",
        "exit_ts",
        "side",
        "qty",
        "entry_price",
        "exit_price",
        "pnl",
        "reason",
        "reference_level",
        "reference_depth",
    ])?;
    for trade in trades {
        writer.write_record([
            format!("{:.6}", trade.entry_ts),
            format!("{:.6}", trade.exit_ts),
            format!("{:?}", trade.side),
            format!("{:.6}", trade.qty),
            format!("{:.6}", trade.entry_price),
            format!("{:.6}", trade.exit_price),
            format!("{:.6}", trade.pnl),
            format!("{:?}", trade.reason),
            format!("{:.6}", trade.reference_level),
            format!("{:.6}", trade.reference_depth),
        ])?;
    }
    writer.flush()?;
    Ok(())
}

fn write_summary_csv(path: &Path, summary: &SummaryMetrics) -> anyhow::Result<()> {
    let mut writer = csv::Writer::from_path(path)
        .with_context(|| format!("無法寫入摘要檔案: {}", path.display()))?;
    writer.write_record(["total_pnl", "trades", "win_rate", "max_drawdown", "max_position"])?;
    writer.write_record([
        format!("{:.6}", summary.total_pnl),
        summary.trades.to_string(),
        format!("{:.4}", summary.win_rate),
        format!("{:.6}", summary.max_drawdown),
        format!("{:.6}", summary.max_position),
    ])?;
    writer.flush()?;
    Ok(())
}

fn write_metrics_json(path: &Path, summary: &SummaryMetrics) -> anyhow::Result<()> {
    let buffer = serde_json::to_string_pretty(summary)
        .context("序列化回測指標失敗")?;
    std::fs::write(path, buffer)
        .with_context(|| format!("寫入指標檔案失敗: {}", path.display()))?;
    Ok(())
}

fn print_summary(summary: &SummaryMetrics) {
    info!("===== 回測摘要 =====");
    info!("總損益 (PnL): {:.6}", summary.total_pnl);
    info!("交易筆數: {}", summary.trades);
    info!("勝率: {:.2}%", summary.win_rate * 100.0);
    info!("最大回撤: {:.6}", summary.max_drawdown);
    info!("最高持倉: {:.6}", summary.max_position);
}
