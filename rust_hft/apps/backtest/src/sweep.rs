use std::path::PathBuf;

use anyhow::{bail, Context};
use itertools::Itertools;
use serde_yaml::{Mapping, Value};
use tracing::info;

use crate::config::BacktestConfig;
use crate::engine::BacktestEngine;
use crate::{print_summary, write_outputs};

pub struct SweepOptions<'a> {
    pub config_path: &'a str,
    pub grid_path: &'a str,
    pub output_root: &'a str,
    pub shard_index: usize,
    pub shard_count: usize,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq)]
struct PlannedRun {
    index: usize,
    overrides: Vec<(String, Value)>,
    slug: String,
}

pub fn run(options: SweepOptions<'_>) -> anyhow::Result<()> {
    let base_yaml = std::fs::read_to_string(options.config_path)
        .with_context(|| format!("無法讀取回測配置: {}", options.config_path))?;
    let base: Value = serde_yaml::from_str(&base_yaml)
        .with_context(|| format!("解析回測配置失敗: {}", options.config_path))?;
    let grid_yaml = std::fs::read_to_string(options.grid_path)
        .with_context(|| format!("無法讀取參數格點: {}", options.grid_path))?;
    let plan = build_plan(&grid_yaml, options.shard_index, options.shard_count)?;

    let total = count_combinations(&grid_yaml)?;
    info!(
        "參數總數={} 分片={}/{} 本分片待回測={}",
        total,
        options.shard_index,
        options.shard_count,
        plan.len()
    );

    let output_root = PathBuf::from(options.output_root);
    std::fs::create_dir_all(&output_root)
        .with_context(|| format!("無法建立輸出目錄: {}", output_root.display()))?;

    for planned in plan {
        let config_value = apply_strategy_overrides(&base, &planned.overrides)?;
        let config_yaml = serde_yaml::to_string(&config_value).context("序列化參數組合失敗")?;
        let cfg = BacktestConfig::from_yaml_str(
            &config_yaml,
            &format!("{}#{}", options.grid_path, planned.index),
        )?;
        let combo_output = output_root.join(format!("{:06}_{}", planned.index, planned.slug));

        info!(
            "組合 index={} slug={} output={}",
            planned.index,
            planned.slug,
            combo_output.display()
        );
        if options.dry_run {
            crate::validate_dry_run(&cfg)?;
            let strategy = config_value
                .as_mapping()
                .and_then(|mapping| mapping.get(Value::String("strategy".to_string())))
                .context("回測配置缺少 strategy 區段")?;
            info!(
                "{}",
                serde_json::to_string_pretty(strategy).context("序列化 strategy 失敗")?
            );
            continue;
        }

        info!("載入事件：{}", cfg.data.path);
        let mut engine = BacktestEngine::new(cfg.clone());
        let result = engine.run()?;
        write_outputs(
            &cfg.output,
            Some(combo_output.to_string_lossy().as_ref()),
            &result,
        )?;
        print_summary(&result.summary);
    }

    Ok(())
}

fn build_plan(
    grid_yaml: &str,
    shard_index: usize,
    shard_count: usize,
) -> anyhow::Result<Vec<PlannedRun>> {
    validate_shard(shard_index, shard_count)?;
    let axes = parse_axes(grid_yaml)?;
    let combinations = axes
        .iter()
        .map(|(_, values)| values.iter().cloned())
        .multi_cartesian_product();

    Ok(combinations
        .enumerate()
        .filter(|(index, _)| index % shard_count == shard_index)
        .map(|(index, values)| {
            let overrides = axes
                .iter()
                .map(|(key, _)| key.clone())
                .zip(values)
                .collect::<Vec<_>>();
            let slug = slugify(&overrides);
            PlannedRun {
                index,
                overrides,
                slug,
            }
        })
        .collect())
}

fn count_combinations(grid_yaml: &str) -> anyhow::Result<usize> {
    parse_axes(grid_yaml)?
        .into_iter()
        .try_fold(1usize, |count, (_, values)| {
            count.checked_mul(values.len()).context("參數組合數量溢出")
        })
}

fn validate_shard(shard_index: usize, shard_count: usize) -> anyhow::Result<()> {
    if shard_count == 0 {
        bail!("--shard-count 必須大於 0");
    }
    if shard_index >= shard_count {
        bail!("--shard-index 必須小於 --shard-count");
    }
    Ok(())
}

fn parse_axes(grid_yaml: &str) -> anyhow::Result<Vec<(String, Vec<Value>)>> {
    let grid: Value = serde_yaml::from_str(grid_yaml).context("解析參數格點失敗")?;
    let root = grid.as_mapping().context("參數格點根節點必須映射")?;
    let strategy = root
        .get(Value::String("strategy".to_string()))
        .and_then(Value::as_mapping)
        .context("格點檔案需至少包含 strategy 映射")?;
    if strategy.is_empty() {
        bail!("格點檔案的 strategy 不能為空");
    }

    strategy
        .iter()
        .map(|(key, values)| {
            let key = key
                .as_str()
                .context("strategy 參數名稱必須是字串")?
                .to_string();
            let values = values
                .as_sequence()
                .with_context(|| format!("strategy.{key} 必須是非空陣列"))?;
            if values.is_empty() {
                bail!("strategy.{key} 必須是非空陣列");
            }
            Ok((key, values.clone()))
        })
        .collect()
}

fn apply_strategy_overrides(base: &Value, overrides: &[(String, Value)]) -> anyhow::Result<Value> {
    let mut config = base.clone();
    let root = config.as_mapping_mut().context("回測配置根節點必須映射")?;
    let strategy = root
        .entry(Value::String("strategy".to_string()))
        .or_insert_with(|| Value::Mapping(Mapping::new()))
        .as_mapping_mut()
        .context("回測配置 strategy 必須映射")?;
    for (key, value) in overrides {
        strategy.insert(Value::String(key.clone()), value.clone());
    }
    Ok(config)
}

fn slugify(overrides: &[(String, Value)]) -> String {
    overrides
        .iter()
        .map(|(key, value)| format!("{}-{}", slug_component(key), slug_value(value)))
        .join("_")
}

fn slug_value(value: &Value) -> String {
    let value = match value {
        Value::Number(number) if number.as_i64().is_some() || number.as_u64().is_some() => {
            number.to_string()
        }
        Value::Number(number) => number
            .as_f64()
            .map(|value| format!("{value:.3}").replace('.', "p"))
            .unwrap_or_else(|| number.to_string()),
        Value::String(value) => value.clone(),
        Value::Bool(value) => value.to_string(),
        Value::Null => "null".to_string(),
        other => serde_yaml::to_string(other)
            .unwrap_or_else(|_| "value".to_string())
            .trim()
            .to_string(),
    };
    slug_component(&value)
}

fn slug_component(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "value".to_string()
    } else {
        sanitized
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRID: &str = r#"
strategy:
  price_delta_ticks: [1.0, 2.5]
  support_count: [2, 4]
"#;

    #[test]
    fn preserves_cartesian_order_and_shards_by_global_index() {
        let plan = build_plan(GRID, 1, 2).expect("plan");
        assert_eq!(
            plan.iter().map(|run| run.index).collect::<Vec<_>>(),
            vec![1, 3]
        );
        assert_eq!(plan[0].slug, "price_delta_ticks-1p000_support_count-4");
        assert_eq!(plan[1].slug, "price_delta_ticks-2p500_support_count-4");
    }

    #[test]
    fn overrides_only_strategy_fields() {
        let base: Value = serde_yaml::from_str(
            r#"
data:
  path: fixture.ndjson
strategy:
  price_delta_ticks: 9.0
execution:
  base_qty: 0.01
"#,
        )
        .expect("base yaml");
        let updated = apply_strategy_overrides(
            &base,
            &[("price_delta_ticks".to_string(), Value::from(2.5))],
        )
        .expect("override");

        assert_eq!(updated["strategy"]["price_delta_ticks"].as_f64(), Some(2.5));
        assert_eq!(updated["execution"]["base_qty"].as_f64(), Some(0.01));
    }

    #[test]
    fn rejects_invalid_shard() {
        let error = build_plan(GRID, 2, 2).expect_err("invalid shard must fail");
        assert!(error.to_string().contains("--shard-index"));
        assert!(build_plan(GRID, 0, 0).is_err());
    }

    #[test]
    fn dry_run_rejects_invalid_output_names() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("config.yaml");
        let grid_path = directory.path().join("grid.yaml");
        let base = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../config/backtest/default.yaml"),
        )
        .unwrap()
        .replace(
            "summary_csv: backtest_summary.csv",
            "summary_csv: backtest_trades.csv",
        );
        std::fs::write(&config_path, base).unwrap();
        std::fs::write(&grid_path, GRID).unwrap();

        let error = run(SweepOptions {
            config_path: config_path.to_str().unwrap(),
            grid_path: grid_path.to_str().unwrap(),
            output_root: directory.path().join("output").to_str().unwrap(),
            shard_index: 0,
            shard_count: 1,
            dry_run: true,
        })
        .unwrap_err();

        assert!(error.to_string().contains("output artifact names"));
    }
}
