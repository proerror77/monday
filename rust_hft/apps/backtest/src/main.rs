mod config;
mod engine;
mod event;
mod sweep;

use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::Parser;
use sha2::Digest;
use tracing::info;

use config::{BacktestConfig, BacktestInputEvidence, OutputConfig, StrategyConfig};
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
    /// 參數格點檔（含 strategy 區段）
    #[arg(long, conflicts_with = "params")]
    grid: Option<String>,
    /// 輸出目錄（預設寫入目前路徑）
    #[arg(long, conflicts_with = "grid")]
    output_dir: Option<String>,
    /// 參數掃描輸出根目錄
    #[arg(long, requires = "grid")]
    output: Option<String>,
    /// 參數掃描分片索引
    #[arg(long, requires = "grid")]
    shard_index: Option<usize>,
    /// 參數掃描總分片數
    #[arg(long, requires = "grid")]
    shard_count: Option<usize>,
    /// 僅檢查配置，不運行回測
    #[arg(long)]
    dry_run: bool,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_target(false).init();

    let args = Args::parse();

    if let Some(grid) = args.grid.as_deref() {
        let shard_index = resolve_usize(args.shard_index, "JOB_COMPLETION_INDEX", 0)?;
        let shard_count = resolve_usize(args.shard_count, "BACKTEST_SHARD_COUNT", 1)?;
        return sweep::run(sweep::SweepOptions {
            config_path: &args.config,
            grid_path: grid,
            output_root: args.output.as_deref().unwrap_or("runs/backtest-sweep"),
            shard_index,
            shard_count,
            dry_run: args.dry_run,
        });
    }

    let mut cfg = BacktestConfig::from_file(&args.config)?;
    if let Some(params_path) = args.params {
        apply_strategy_overrides(&mut cfg, &params_path)?;
    }

    if args.dry_run {
        validate_dry_run(&cfg)?;
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

fn validate_dry_run(cfg: &BacktestConfig) -> anyhow::Result<()> {
    cfg.validate_data_artifact().map(drop)
}

fn resolve_usize(value: Option<usize>, env_name: &str, fallback: usize) -> anyhow::Result<usize> {
    if let Some(value) = value {
        return Ok(value);
    }
    match std::env::var(env_name) {
        Ok(value) => value
            .parse::<usize>()
            .with_context(|| format!("環境變數 {env_name} 必須是非負整數")),
        Err(std::env::VarError::NotPresent) => Ok(fallback),
        Err(error) => Err(error).with_context(|| format!("無法讀取環境變數 {env_name}")),
    }
}

fn apply_strategy_overrides(cfg: &mut BacktestConfig, path: &str) -> anyhow::Result<()> {
    let content =
        std::fs::read_to_string(path).with_context(|| format!("無法讀取參數檔: {}", path))?;

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
    validate_output_names(output)?;
    let out_dir = override_dir
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    if !out_dir.exists() {
        std::fs::create_dir_all(&out_dir)
            .with_context(|| format!("無法建立輸出目錄: {}", out_dir.display()))?;
    }
    for name in [
        Some(&output.trades_csv),
        Some(&output.summary_csv),
        output.metrics_json.as_ref(),
        Some(&output.evidence_json),
    ]
    .into_iter()
    .flatten()
    {
        if out_dir.join(name).try_exists()? {
            anyhow::bail!(
                "backtest output already exists: {}",
                out_dir.join(name).display()
            );
        }
    }
    let evidence = result
        .input_evidence
        .as_ref()
        .context("backtest result is missing input evidence")?;

    write_trades_csv(&out_dir.join(&output.trades_csv), &result.trades, evidence)?;
    write_summary_csv(
        &out_dir.join(&output.summary_csv),
        &result.summary,
        evidence,
    )?;

    if let Some(metrics_path) = &output.metrics_json {
        write_metrics_json(&out_dir.join(metrics_path), result)?;
    }
    write_output_evidence(&out_dir, output, result)?;
    Ok(())
}

fn validate_output_names(output: &OutputConfig) -> anyhow::Result<()> {
    let mut names = std::collections::HashSet::new();
    for name in [
        Some(&output.trades_csv),
        Some(&output.summary_csv),
        output.metrics_json.as_ref(),
        Some(&output.evidence_json),
    ]
    .into_iter()
    .flatten()
    {
        if name.trim().is_empty() || !names.insert(name) {
            anyhow::bail!("backtest output artifact names must be non-empty and unique");
        }
    }
    Ok(())
}

fn write_output_evidence(
    out_dir: &Path,
    output: &OutputConfig,
    result: &BacktestResult,
) -> anyhow::Result<()> {
    let mut artifacts = std::collections::BTreeMap::new();
    for name in [
        Some(&output.trades_csv),
        Some(&output.summary_csv),
        output.metrics_json.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        let bytes = std::fs::read(out_dir.join(name))?;
        artifacts.insert(name, hex::encode(sha2::Sha256::digest(bytes)));
    }
    let evidence = serde_json::json!({
        "input": result.input_evidence,
        "artifacts": artifacts,
    });
    serde_json::to_writer_pretty(
        create_output(&out_dir.join(&output.evidence_json))?,
        &evidence,
    )?;
    Ok(())
}

fn create_output(path: &Path) -> anyhow::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("refusing to overwrite backtest output: {}", path.display()))
}

fn write_trades_csv(
    path: &Path,
    trades: &[TradeRecord],
    evidence: &BacktestInputEvidence,
) -> anyhow::Result<()> {
    let mut writer = csv::Writer::from_writer(create_output(path)?);
    writer.write_record([
        "entry_ts",
        "exit_ts",
        "side",
        "qty",
        "entry_price",
        "exit_price",
        "gross_pnl",
        "fees",
        "pnl",
        "reason",
        "reference_level",
        "reference_depth",
        "input_manifest_sha256",
        "effective_config_sha256",
        "source_revision",
    ])?;
    for trade in trades {
        writer.write_record([
            format!("{:.6}", trade.entry_ts),
            format!("{:.6}", trade.exit_ts),
            format!("{:?}", trade.side),
            format!("{:.6}", trade.qty),
            format!("{:.6}", trade.entry_price),
            format!("{:.6}", trade.exit_price),
            format!("{:.6}", trade.gross_pnl),
            format!("{:.6}", trade.fees),
            format!("{:.6}", trade.pnl),
            format!("{:?}", trade.reason),
            format!("{:.6}", trade.reference_level),
            format!("{:.6}", trade.reference_depth),
            evidence.manifest_sha256.clone(),
            evidence.config_sha256.clone(),
            evidence.source_revision.clone(),
        ])?;
    }
    writer.flush()?;
    Ok(())
}

fn write_summary_csv(
    path: &Path,
    summary: &SummaryMetrics,
    evidence: &BacktestInputEvidence,
) -> anyhow::Result<()> {
    let mut writer = csv::Writer::from_writer(create_output(path)?);
    writer.write_record([
        "total_pnl",
        "gross_pnl",
        "total_fees",
        "turnover",
        "trades",
        "win_rate",
        "max_drawdown",
        "max_position",
        "open_position_qty",
        "input_manifest_sha256",
        "effective_config_sha256",
        "source_revision",
    ])?;
    writer.write_record([
        format!("{:.6}", summary.total_pnl),
        format!("{:.6}", summary.gross_pnl),
        format!("{:.6}", summary.total_fees),
        format!("{:.6}", summary.turnover),
        summary.trades.to_string(),
        format!("{:.4}", summary.win_rate),
        format!("{:.6}", summary.max_drawdown),
        format!("{:.6}", summary.max_position),
        format!("{:.6}", summary.open_position_qty),
        evidence.manifest_sha256.clone(),
        evidence.config_sha256.clone(),
        evidence.source_revision.clone(),
    ])?;
    writer.flush()?;
    Ok(())
}

fn write_metrics_json(path: &Path, result: &BacktestResult) -> anyhow::Result<()> {
    let buffer = serde_json::to_string_pretty(&serde_json::json!({
        "input": result.input_evidence,
        "summary": result.summary,
    }))
    .context("序列化回測指標失敗")?;
    std::io::Write::write_all(&mut create_output(path)?, buffer.as_bytes())?;
    Ok(())
}

fn print_summary(summary: &SummaryMetrics) {
    info!("===== 回測摘要 =====");
    info!("總損益 (PnL): {:.6}", summary.total_pnl);
    info!("總毛利: {:.6}", summary.gross_pnl);
    info!("總費用: {:.6}", summary.total_fees);
    info!("成交額: {:.6}", summary.turnover);
    info!("交易筆數: {}", summary.trades);
    info!("勝率: {:.2}%", summary.win_rate * 100.0);
    info!("最大回撤: {:.6}", summary.max_drawdown);
    info!("最高持倉: {:.6}", summary.max_position);
    info!("未平倉殘量: {:.6}", summary.open_position_qty);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_run_validates_the_input_artifact() {
        let config_path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/backtest/default.yaml");
        let mut config = BacktestConfig::from_file(config_path).unwrap();
        config.data.manifest_sha256 = Some("0".repeat(64));

        assert!(validate_dry_run(&config).is_err());
    }

    #[test]
    fn output_evidence_binds_inputs_and_every_result_artifact() {
        let id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let out_dir = std::env::temp_dir().join(format!(
            "monday-backtest-output-evidence-{}-{id}",
            std::process::id()
        ));
        let output = OutputConfig {
            trades_csv: "trades.csv".to_string(),
            summary_csv: "summary.csv".to_string(),
            metrics_json: Some("metrics.json".to_string()),
            evidence_json: "evidence.json".to_string(),
        };
        let result = BacktestResult {
            trades: Vec::new(),
            summary: SummaryMetrics {
                total_pnl: 0.0,
                gross_pnl: 0.0,
                total_fees: 0.0,
                turnover: 0.0,
                trades: 0,
                win_rate: 0.0,
                max_drawdown: 0.0,
                max_position: 0.0,
                open_position_qty: 0.0,
            },
            input_evidence: Some(BacktestInputEvidence {
                manifest_sha256: "a".repeat(64),
                config_sha256: "b".repeat(64),
                source_revision: "c".repeat(64),
            }),
        };

        write_outputs(&output, Some(out_dir.to_str().unwrap()), &result).unwrap();

        let evidence: serde_json::Value =
            serde_json::from_slice(&std::fs::read(out_dir.join(&output.evidence_json)).unwrap())
                .unwrap();
        assert_eq!(evidence["input"]["manifest_sha256"], "a".repeat(64));
        assert_eq!(evidence["input"]["config_sha256"], "b".repeat(64));
        assert_eq!(evidence["input"]["source_revision"], "c".repeat(64));
        assert_eq!(evidence["artifacts"].as_object().unwrap().len(), 3);
        for name in ["trades.csv", "summary.csv", "metrics.json"] {
            assert_eq!(evidence["artifacts"][name].as_str().unwrap().len(), 64);
        }

        let original_evidence = std::fs::read(out_dir.join(&output.evidence_json)).unwrap();
        assert!(write_outputs(&output, Some(out_dir.to_str().unwrap()), &result).is_err());
        assert_eq!(
            std::fs::read(out_dir.join(&output.evidence_json)).unwrap(),
            original_evidence
        );

        let mut colliding_output = output.clone();
        colliding_output.evidence_json = colliding_output.summary_csv.clone();
        assert!(
            write_outputs(&colliding_output, Some(out_dir.to_str().unwrap()), &result).is_err()
        );

        std::fs::remove_dir_all(out_dir).unwrap();
    }
}
