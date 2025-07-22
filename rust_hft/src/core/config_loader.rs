use anyhow::{Context, Result};
use chrono::{Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use tracing::{debug, info};

/// 歷史資料配置結構
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoricalDataConfig {
    /// 交易對符號
    pub symbol: Option<String>,
    /// 多個交易對符號
    pub symbols: Option<Vec<String>>,
    /// 資料類型 (depth, trade, all)
    pub data_type: Option<String>,
    /// 日期範圍配置
    pub date_range: Option<DateRangeConfig>,
    /// 輸出配置
    pub output: Option<OutputConfig>,
    /// 性能配置
    pub performance: Option<PerformanceConfig>,
    /// ClickHouse 配置
    pub clickhouse: Option<ClickHouseConfig>,
    /// 訓練配置
    pub training: Option<TrainingConfig>,
    /// 繼承的配置名稱
    pub extends: Option<String>,
}

/// 日期範圍配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DateRangeConfig {
    /// 開始日期 (YYYY-MM-DD)
    pub start_date: Option<String>,
    /// 結束日期 (YYYY-MM-DD)
    pub end_date: Option<String>,
    /// 從今天往前推的天數
    pub days_back: Option<i64>,
}

/// 輸出配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputConfig {
    /// 輸出目錄
    pub directory: Option<String>,
    /// 是否清理 ZIP 文件
    pub cleanup_zip: Option<bool>,
    /// 是否跳過已存在的文件
    pub skip_existing: Option<bool>,
}

/// 性能配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceConfig {
    /// 最大並行下載數
    pub max_concurrent: Option<usize>,
    /// 批量大小
    pub batch_size: Option<usize>,
    /// 最大重試次數
    pub max_retries: Option<usize>,
}

/// ClickHouse 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickHouseConfig {
    /// 是否啟用 ClickHouse
    pub enabled: Option<bool>,
    /// 連接 URL
    pub url: Option<String>,
    /// 資料庫名稱
    pub database: Option<String>,
    /// 使用者名稱
    pub username: Option<String>,
    /// 密碼
    pub password: Option<String>,
}

/// 訓練配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrainingConfig {
    /// 是否啟用訓練
    pub enabled: Option<bool>,
    /// 歷史資料天數
    pub historical_days: Option<i64>,
    /// 預訓練 epochs
    pub pretrain_epochs: Option<usize>,
    /// 預訓練批次大小
    pub pretrain_batch_size: Option<usize>,
    /// 預訓練學習率
    pub pretrain_lr: Option<f64>,
    /// 特徵維度
    pub feature_dim: Option<usize>,
    /// 隱藏層維度
    pub hidden_dim: Option<usize>,
    /// 模型保存路徑
    pub model_save_path: Option<String>,
}

/// 解析後的最終配置
#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub symbols: Vec<String>,
    pub data_type: String,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub output_dir: String,
    pub cleanup_zip: bool,
    pub skip_existing: bool,
    pub max_concurrent: usize,
    pub batch_size: usize,
    pub max_retries: usize,
    pub clickhouse_enabled: bool,
    pub clickhouse_url: Option<String>,
    pub clickhouse_db: String,
    pub clickhouse_user: String,
    pub clickhouse_password: String,
    pub training_enabled: bool,
    pub training_config: Option<TrainingConfig>,
}

/// 配置加載器
pub struct ConfigLoader;

impl ConfigLoader {
    /// 從 YAML 文件加載配置
    pub fn load_from_file(file_path: &str, profile: Option<&str>) -> Result<ResolvedConfig> {
        let content = fs::read_to_string(file_path)
            .context(format!("Failed to read config file: {}", file_path))?;

        let mut configs: HashMap<String, HistoricalDataConfig> = serde_yaml::from_str(&content)
            .context("Failed to parse YAML config")?;

        let profile_name = profile.unwrap_or("default");
        
        info!("Loading config profile: {}", profile_name);

        // 獲取指定的配置
        let config = configs.remove(profile_name)
            .ok_or_else(|| anyhow::anyhow!("Profile '{}' not found in config", profile_name))?;

        // 處理繼承
        let final_config = if let Some(extends) = &config.extends {
            let base_config = configs.get(extends)
                .ok_or_else(|| anyhow::anyhow!("Base profile '{}' not found", extends))?;
            
            let merged = Self::merge_configs(base_config, &config)?;
            debug!("Merged config with base profile: {}", extends);
            merged
        } else {
            config
        };

        Self::resolve_config(final_config)
    }

    /// 合併配置（子配置覆蓋父配置）
    fn merge_configs(base: &HistoricalDataConfig, override_config: &HistoricalDataConfig) -> Result<HistoricalDataConfig> {
        let mut merged = base.clone();

        // 合併各個字段
        if override_config.symbol.is_some() {
            merged.symbol = override_config.symbol.clone();
        }
        if override_config.symbols.is_some() {
            merged.symbols = override_config.symbols.clone();
        }
        if override_config.data_type.is_some() {
            merged.data_type = override_config.data_type.clone();
        }

        // 合併日期範圍
        if let Some(override_date_range) = &override_config.date_range {
            let mut merged_date_range = merged.date_range.unwrap_or_default();
            if override_date_range.start_date.is_some() {
                merged_date_range.start_date = override_date_range.start_date.clone();
            }
            if override_date_range.end_date.is_some() {
                merged_date_range.end_date = override_date_range.end_date.clone();
            }
            if override_date_range.days_back.is_some() {
                merged_date_range.days_back = override_date_range.days_back;
            }
            merged.date_range = Some(merged_date_range);
        }

        // 合併輸出配置
        if let Some(override_output) = &override_config.output {
            let mut merged_output = merged.output.unwrap_or_default();
            if override_output.directory.is_some() {
                merged_output.directory = override_output.directory.clone();
            }
            if override_output.cleanup_zip.is_some() {
                merged_output.cleanup_zip = override_output.cleanup_zip;
            }
            if override_output.skip_existing.is_some() {
                merged_output.skip_existing = override_output.skip_existing;
            }
            merged.output = Some(merged_output);
        }

        // 合併性能配置
        if let Some(override_perf) = &override_config.performance {
            let mut merged_perf = merged.performance.unwrap_or_default();
            if override_perf.max_concurrent.is_some() {
                merged_perf.max_concurrent = override_perf.max_concurrent;
            }
            if override_perf.batch_size.is_some() {
                merged_perf.batch_size = override_perf.batch_size;
            }
            if override_perf.max_retries.is_some() {
                merged_perf.max_retries = override_perf.max_retries;
            }
            merged.performance = Some(merged_perf);
        }

        // 合併 ClickHouse 配置
        if let Some(override_ch) = &override_config.clickhouse {
            let mut merged_ch = merged.clickhouse.unwrap_or_default();
            if override_ch.enabled.is_some() {
                merged_ch.enabled = override_ch.enabled;
            }
            if override_ch.url.is_some() {
                merged_ch.url = override_ch.url.clone();
            }
            if override_ch.database.is_some() {
                merged_ch.database = override_ch.database.clone();
            }
            if override_ch.username.is_some() {
                merged_ch.username = override_ch.username.clone();
            }
            if override_ch.password.is_some() {
                merged_ch.password = override_ch.password.clone();
            }
            merged.clickhouse = Some(merged_ch);
        }

        // 合併訓練配置
        if let Some(override_training) = &override_config.training {
            merged.training = Some(override_training.clone());
        }

        Ok(merged)
    }

    /// 解析配置為最終格式
    fn resolve_config(config: HistoricalDataConfig) -> Result<ResolvedConfig> {
        // 解析交易對
        let symbols = if let Some(symbols) = config.symbols {
            symbols
        } else if let Some(symbol) = config.symbol {
            vec![symbol]
        } else {
            return Err(anyhow::anyhow!("Either 'symbol' or 'symbols' must be specified"));
        };

        // 解析資料類型
        let data_type = config.data_type.unwrap_or_else(|| "depth".to_string());

        // 解析日期範圍
        let (start_date, end_date) = if let Some(date_range) = config.date_range {
            if let Some(days_back) = date_range.days_back {
                let end_date = Utc::now().naive_utc().date();
                let start_date = end_date - Duration::days(days_back);
                (start_date, end_date)
            } else {
                let start_str = date_range.start_date
                    .ok_or_else(|| anyhow::anyhow!("Either 'days_back' or 'start_date' must be specified"))?;
                let end_str = date_range.end_date
                    .ok_or_else(|| anyhow::anyhow!("end_date must be specified when using start_date"))?;
                
                let start_date = NaiveDate::parse_from_str(&start_str, "%Y-%m-%d")
                    .context("Invalid start_date format, use YYYY-MM-DD")?;
                let end_date = NaiveDate::parse_from_str(&end_str, "%Y-%m-%d")
                    .context("Invalid end_date format, use YYYY-MM-DD")?;
                
                (start_date, end_date)
            }
        } else {
            return Err(anyhow::anyhow!("date_range must be specified"));
        };

        // 解析輸出配置
        let output = config.output.unwrap_or_default();
        let output_dir = output.directory.unwrap_or_else(|| "./historical_data".to_string());
        let cleanup_zip = output.cleanup_zip.unwrap_or(true);
        let skip_existing = output.skip_existing.unwrap_or(true);

        // 解析性能配置
        let performance = config.performance.unwrap_or_default();
        let max_concurrent = performance.max_concurrent.unwrap_or(5);
        let batch_size = performance.batch_size.unwrap_or(5000);
        let max_retries = performance.max_retries.unwrap_or(3);

        // 解析 ClickHouse 配置
        let clickhouse = config.clickhouse.unwrap_or_default();
        let clickhouse_enabled = clickhouse.enabled.unwrap_or(false);
        let clickhouse_url = if clickhouse_enabled {
            Some(clickhouse.url.unwrap_or_else(|| "http://localhost:8123".to_string()))
        } else {
            None
        };
        let clickhouse_db = clickhouse.database.unwrap_or_else(|| "hft_db".to_string());
        let clickhouse_user = clickhouse.username.unwrap_or_else(|| "hft_user".to_string());
        let clickhouse_password = clickhouse.password.unwrap_or_else(|| "hft_password".to_string());

        // 解析訓練配置
        let training_enabled = config.training.as_ref()
            .map(|t| t.enabled.unwrap_or(false))
            .unwrap_or(false);

        Ok(ResolvedConfig {
            symbols,
            data_type,
            start_date,
            end_date,
            output_dir,
            cleanup_zip,
            skip_existing,
            max_concurrent,
            batch_size,
            max_retries,
            clickhouse_enabled,
            clickhouse_url,
            clickhouse_db,
            clickhouse_user,
            clickhouse_password,
            training_enabled,
            training_config: config.training,
        })
    }

    /// 創建默認配置文件
    pub fn create_default_config(file_path: &str) -> Result<()> {
        let default_config = include_str!("../../config/historical_data_config.yaml");
        
        if let Some(parent) = Path::new(file_path).parent() {
            fs::create_dir_all(parent)?;
        }
        
        fs::write(file_path, default_config)?;
        info!("Created default config file: {}", file_path);
        
        Ok(())
    }

    /// 列出配置文件中的所有 profiles
    pub fn list_profiles(file_path: &str) -> Result<Vec<String>> {
        let content = fs::read_to_string(file_path)
            .context(format!("Failed to read config file: {}", file_path))?;

        let configs: HashMap<String, HistoricalDataConfig> = serde_yaml::from_str(&content)
            .context("Failed to parse YAML config")?;

        let mut profiles: Vec<String> = configs.keys().cloned().collect();
        profiles.sort();
        
        Ok(profiles)
    }

    /// 驗證配置
    pub fn validate_config(config: &ResolvedConfig) -> Result<()> {
        // 驗證日期範圍
        if config.start_date > config.end_date {
            return Err(anyhow::anyhow!("start_date must be before end_date"));
        }

        // 驗證資料類型
        if !["depth", "trade", "all"].contains(&config.data_type.as_str()) {
            return Err(anyhow::anyhow!("data_type must be 'depth', 'trade', or 'all'"));
        }

        // 驗證性能參數
        if config.max_concurrent == 0 {
            return Err(anyhow::anyhow!("max_concurrent must be greater than 0"));
        }

        if config.batch_size == 0 {
            return Err(anyhow::anyhow!("batch_size must be greater than 0"));
        }

        // 驗證交易對格式
        for symbol in &config.symbols {
            if symbol.is_empty() {
                return Err(anyhow::anyhow!("Symbol cannot be empty"));
            }
        }

        info!("Configuration validation passed");
        Ok(())
    }
}

// 為結構體實現默認值
impl Default for DateRangeConfig {
    fn default() -> Self {
        Self {
            start_date: None,
            end_date: None,
            days_back: None,
        }
    }
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            directory: None,
            cleanup_zip: None,
            skip_existing: None,
        }
    }
}

impl Default for PerformanceConfig {
    fn default() -> Self {
        Self {
            max_concurrent: None,
            batch_size: None,
            max_retries: None,
        }
    }
}

impl Default for ClickHouseConfig {
    fn default() -> Self {
        Self {
            enabled: None,
            url: None,
            database: None,
            username: None,
            password: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;
    use std::io::Write;

    #[test]
    fn test_config_parsing() {
        let yaml_content = r#"
default:
  symbol: "BTCUSDT"
  data_type: "depth"
  date_range:
    start_date: "2024-01-01"
    end_date: "2024-01-02"
  performance:
    max_concurrent: 3
    batch_size: 1000
  clickhouse:
    enabled: true
    url: "http://localhost:8123"
"#;

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(yaml_content.as_bytes()).unwrap();
        
        let config = ConfigLoader::load_from_file(
            temp_file.path().to_str().unwrap(), 
            Some("default")
        ).unwrap();

        assert_eq!(config.symbols, vec!["BTCUSDT"]);
        assert_eq!(config.data_type, "depth");
        assert_eq!(config.max_concurrent, 3);
        assert_eq!(config.batch_size, 1000);
        assert!(config.clickhouse_enabled);
    }

    #[test]
    fn test_config_inheritance() {
        let yaml_content = r#"
base:
  symbol: "BTCUSDT"
  performance:
    max_concurrent: 5

child:
  extends: base
  symbol: "ETHUSDT"
  data_type: "trade"
  date_range:
    days_back: 7
"#;

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(yaml_content.as_bytes()).unwrap();
        
        let config = ConfigLoader::load_from_file(
            temp_file.path().to_str().unwrap(), 
            Some("child")
        ).unwrap();

        assert_eq!(config.symbols, vec!["ETHUSDT"]);
        assert_eq!(config.data_type, "trade");
        assert_eq!(config.max_concurrent, 5); // 繼承自 base
    }
}