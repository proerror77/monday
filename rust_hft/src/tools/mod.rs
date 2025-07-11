/*!
 * 🛠️ Rust HFT Core Tools - 底層工具統一接口
 * 
 * 提供完整的HFT工具集：
 * - 數據下載和收集
 * - 數據清洗和預處理  
 * - 特徵工程
 * - 回測引擎
 * - 實時交易執行
 * 
 * 設計目標：
 * - 統一CLI接口
 * - Python PyO3調用
 * - 高性能執行
 * - 模塊化設計
 */

use crate::core::types::*;
use crate::core::config::Config;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use clap::{Parser, Subcommand};

pub mod data_downloader;
pub mod data_cleaner;
pub mod feature_engineer;
pub mod backtester;
pub mod live_trader;
pub mod performance_analyzer;

use data_downloader::DataDownloader;
use data_cleaner::DataCleaner;
use feature_engineer::FeatureEngineer;
use backtester::BacktestEngine;
use live_trader::LiveTrader;
use performance_analyzer::PerformanceAnalyzer;

/// HFT工具統一CLI接口
#[derive(Parser)]
#[command(name = "rust-hft")]
#[command(about = "Rust HFT Trading Tools - 高頻交易工具集")]
pub struct HftCli {
    #[command(subcommand)]
    pub command: HftCommand,
    
    /// 配置文件路徑
    #[arg(short, long, default_value = "config.yaml")]
    pub config: String,
    
    /// 日誌級別
    #[arg(short, long, default_value = "info")]
    pub log_level: String,
    
    /// 輸出格式
    #[arg(short, long, default_value = "json")]
    pub output_format: String,
}

/// HFT命令類型
#[derive(Subcommand)]
pub enum HftCommand {
    /// 數據下載
    Download {
        /// 交易對
        #[arg(short, long)]
        symbol: String,
        
        /// 開始時間
        #[arg(long)]
        start_time: Option<String>,
        
        /// 結束時間  
        #[arg(long)]
        end_time: Option<String>,
        
        /// 數據類型
        #[arg(short, long, default_value = "kline")]
        data_type: String,
        
        /// 輸出目錄
        #[arg(short, long, default_value = "data/raw")]
        output_dir: String,
    },
    
    /// 數據清洗
    Clean {
        /// 輸入文件路徑
        #[arg(short, long)]
        input_path: String,
        
        /// 輸出路徑
        #[arg(short, long)]
        output_path: String,
        
        /// 清洗配置
        #[arg(long)]
        config_path: Option<String>,
    },
    
    /// 特徵工程
    Features {
        /// 輸入數據路徑
        #[arg(short, long)]
        input_path: String,
        
        /// 輸出路徑
        #[arg(short, long)]
        output_path: String,
        
        /// 特徵類型
        #[arg(long, default_value = "all")]
        feature_types: String,
        
        /// 時間窗口
        #[arg(long)]
        windows: Option<String>,
    },
    
    /// 回測
    Backtest {
        /// 策略配置文件
        #[arg(short, long)]
        strategy_config: String,
        
        /// 數據路徑
        #[arg(short, long)]
        data_path: String,
        
        /// 開始日期
        #[arg(long)]
        start_date: String,
        
        /// 結束日期
        #[arg(long)]
        end_date: String,
        
        /// 初始資金
        #[arg(long, default_value = "100000")]
        initial_capital: f64,
        
        /// 輸出報告路徑
        #[arg(short, long)]
        output_path: String,
    },
    
    /// 實時交易
    Trade {
        /// 策略配置
        #[arg(short, long)]
        strategy_config: String,
        
        /// 是否為模擬模式
        #[arg(long)]
        dry_run: bool,
        
        /// 交易對
        #[arg(short, long)]
        symbol: String,
        
        /// 初始資金
        #[arg(long, default_value = "10000")]
        capital: f64,
    },
    
    /// 性能分析
    Analyze {
        /// 回測結果路徑
        #[arg(short, long)]
        results_path: String,
        
        /// 基準比較
        #[arg(long)]
        benchmark: Option<String>,
        
        /// 輸出報告路徑
        #[arg(short, long)]
        output_path: String,
    },
}

/// 工具執行結果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub success: bool,
    pub message: String,
    pub data: Option<serde_json::Value>,
    pub execution_time_ms: u64,
    pub output_files: Vec<String>,
    pub metrics: HashMap<String, f64>,
}

impl Default for ToolResult {
    fn default() -> Self {
        Self {
            success: false,
            message: String::new(),
            data: None,
            execution_time_ms: 0,
            output_files: Vec::new(),
            metrics: HashMap::new(),
        }
    }
}

/// HFT工具執行器
pub struct HftToolExecutor {
    config: Config,
    data_downloader: DataDownloader,
    data_cleaner: DataCleaner,
    feature_engineer: FeatureEngineer,
    backtest_engine: BacktestEngine,
    live_trader: LiveTrader,
    performance_analyzer: PerformanceAnalyzer,
}

impl HftToolExecutor {
    /// 創建新的工具執行器
    pub fn new(config: Config) -> Result<Self> {
        Ok(Self {
            data_downloader: DataDownloader::new(&config)?,
            data_cleaner: DataCleaner::new(&config)?,
            feature_engineer: FeatureEngineer::new(&config)?,
            backtest_engine: BacktestEngine::new(&config)?,
            live_trader: LiveTrader::new(&config)?,
            performance_analyzer: PerformanceAnalyzer::new(&config)?,
            config,
        })
    }
    
    /// 執行CLI命令
    pub async fn execute_command(&mut self, command: HftCommand) -> Result<ToolResult> {
        let start_time = std::time::Instant::now();
        
        let result = match command {
            HftCommand::Download { symbol, start_time, end_time, data_type, output_dir } => {
                self.execute_download(symbol, start_time, end_time, data_type, output_dir).await?
            },
            
            HftCommand::Clean { input_path, output_path, config_path } => {
                self.execute_clean(input_path, output_path, config_path).await?
            },
            
            HftCommand::Features { input_path, output_path, feature_types, windows } => {
                self.execute_features(input_path, output_path, feature_types, windows).await?
            },
            
            HftCommand::Backtest { strategy_config, data_path, start_date, end_date, initial_capital, output_path } => {
                self.execute_backtest(strategy_config, data_path, start_date, end_date, initial_capital, output_path).await?
            },
            
            HftCommand::Trade { strategy_config, dry_run, symbol, capital } => {
                self.execute_trade(strategy_config, dry_run, symbol, capital).await?
            },
            
            HftCommand::Analyze { results_path, benchmark, output_path } => {
                self.execute_analyze(results_path, benchmark, output_path).await?
            },
        };
        
        let execution_time = start_time.elapsed().as_millis() as u64;
        
        Ok(ToolResult {
            execution_time_ms: execution_time,
            ..result
        })
    }
    
    /// 執行數據下載
    async fn execute_download(
        &mut self,
        symbol: String,
        start_time: Option<String>,
        end_time: Option<String>,
        data_type: String,
        output_dir: String,
    ) -> Result<ToolResult> {
        
        let download_config = DownloadConfig {
            symbol: symbol.clone(),
            data_type: parse_data_type(&data_type)?,
            start_time: parse_time_option(start_time)?,
            end_time: parse_time_option(end_time)?,
            output_directory: output_dir.clone(),
            batch_size: 1000,
            rate_limit_ms: 100,
        };
        
        let download_result = self.data_downloader.download_data(download_config).await?;
        
        let mut metrics = HashMap::new();
        metrics.insert("records_downloaded".to_string(), download_result.total_records as f64);
        metrics.insert("files_created".to_string(), download_result.output_files.len() as f64);
        metrics.insert("data_size_mb".to_string(), download_result.total_size_bytes as f64 / 1024.0 / 1024.0);
        
        Ok(ToolResult {
            success: download_result.success,
            message: format!("下載完成: {} 記錄數: {}", symbol, download_result.total_records),
            data: Some(serde_json::to_value(&download_result)?),
            execution_time_ms: 0, // 將在上層設置
            output_files: download_result.output_files,
            metrics,
        })
    }
    
    /// 執行數據清洗
    async fn execute_clean(
        &mut self,
        input_path: String,
        output_path: String,
        config_path: Option<String>,
    ) -> Result<ToolResult> {
        
        let clean_config = if let Some(config_file) = config_path {
            CleaningConfig::from_file(&config_file)?
        } else {
            CleaningConfig::default()
        };
        
        let clean_result = self.data_cleaner.clean_data(&input_path, &output_path, clean_config).await?;
        
        let mut metrics = HashMap::new();
        metrics.insert("records_processed".to_string(), clean_result.total_records as f64);
        metrics.insert("records_removed".to_string(), clean_result.removed_records as f64);
        metrics.insert("data_quality_score".to_string(), clean_result.quality_score);
        metrics.insert("cleaning_efficiency".to_string(), clean_result.cleaning_efficiency);
        
        Ok(ToolResult {
            success: clean_result.success,
            message: format!("數據清洗完成: 處理 {} 記錄，移除 {} 異常記錄", 
                           clean_result.total_records, clean_result.removed_records),
            data: Some(serde_json::to_value(&clean_result)?),
            execution_time_ms: 0,
            output_files: vec![output_path],
            metrics,
        })
    }
    
    /// 執行特徵工程
    async fn execute_features(
        &mut self,
        input_path: String,
        output_path: String,
        feature_types: String,
        windows: Option<String>,
    ) -> Result<ToolResult> {
        
        let feature_config = FeatureConfig {
            input_path: input_path.clone(),
            output_path: output_path.clone(),
            feature_types: parse_feature_types(&feature_types)?,
            time_windows: parse_windows(windows)?,
            include_technical_indicators: true,
            include_microstructure: true,
            include_sentiment: false,
        };
        
        let feature_result = self.feature_engineer.generate_features(feature_config).await?;
        
        let mut metrics = HashMap::new();
        metrics.insert("features_generated".to_string(), feature_result.feature_count as f64);
        metrics.insert("records_processed".to_string(), feature_result.record_count as f64);
        metrics.insert("feature_quality_score".to_string(), feature_result.quality_score);
        
        Ok(ToolResult {
            success: feature_result.success,
            message: format!("特徵工程完成: 生成 {} 個特徵，處理 {} 條記錄", 
                           feature_result.feature_count, feature_result.record_count),
            data: Some(serde_json::to_value(&feature_result)?),
            execution_time_ms: 0,
            output_files: vec![output_path],
            metrics,
        })
    }
    
    /// 執行回測
    async fn execute_backtest(
        &mut self,
        strategy_config: String,
        data_path: String,
        start_date: String,
        end_date: String,
        initial_capital: f64,
        output_path: String,
    ) -> Result<ToolResult> {
        
        let backtest_config = BacktestConfig {
            strategy_config_path: strategy_config,
            data_path,
            start_date: parse_date(&start_date)?,
            end_date: parse_date(&end_date)?,
            initial_capital,
            commission_rate: 0.001,
            slippage_bps: 2.0,
            output_path: output_path.clone(),
        };
        
        let backtest_result = self.backtest_engine.run_backtest(backtest_config).await?;
        
        let mut metrics = HashMap::new();
        metrics.insert("total_return".to_string(), backtest_result.total_return);
        metrics.insert("sharpe_ratio".to_string(), backtest_result.sharpe_ratio);
        metrics.insert("max_drawdown".to_string(), backtest_result.max_drawdown);
        metrics.insert("win_rate".to_string(), backtest_result.win_rate);
        metrics.insert("profit_factor".to_string(), backtest_result.profit_factor);
        metrics.insert("total_trades".to_string(), backtest_result.total_trades as f64);
        
        Ok(ToolResult {
            success: backtest_result.success,
            message: format!("回測完成: 總收益 {:.2}%, 夏普比率 {:.2}, 最大回撤 {:.2}%", 
                           backtest_result.total_return * 100.0,
                           backtest_result.sharpe_ratio,
                           backtest_result.max_drawdown * 100.0),
            data: Some(serde_json::to_value(&backtest_result)?),
            execution_time_ms: 0,
            output_files: vec![output_path],
            metrics,
        })
    }
    
    /// 執行實時交易
    async fn execute_trade(
        &mut self,
        strategy_config: String,
        dry_run: bool,
        symbol: String,
        capital: f64,
    ) -> Result<ToolResult> {
        
        let trade_config = LiveTradingConfig {
            strategy_config_path: strategy_config,
            symbol: symbol.clone(),
            initial_capital: capital,
            dry_run,
            max_position_size: 0.1,
            stop_loss_pct: 0.02,
            take_profit_pct: 0.04,
        };
        
        // 實時交易是長期運行的，這裡返回啟動狀態
        let trade_result = self.live_trader.start_trading(trade_config).await?;
        
        let mut metrics = HashMap::new();
        metrics.insert("initial_capital".to_string(), capital);
        metrics.insert("is_dry_run".to_string(), if dry_run { 1.0 } else { 0.0 });
        
        Ok(ToolResult {
            success: trade_result.success,
            message: format!("{}交易已啟動: {} 初始資金: ${:.2}", 
                           if dry_run { "模擬" } else { "實盤" }, symbol, capital),
            data: Some(serde_json::to_value(&trade_result)?),
            execution_time_ms: 0,
            output_files: Vec::new(),
            metrics,
        })
    }
    
    /// 執行性能分析
    async fn execute_analyze(
        &mut self,
        results_path: String,
        benchmark: Option<String>,
        output_path: String,
    ) -> Result<ToolResult> {
        
        let analysis_config = AnalysisConfig {
            results_path: results_path.clone(),
            benchmark_symbol: benchmark,
            output_path: output_path.clone(),
            include_risk_metrics: true,
            include_comparison: true,
            generate_plots: true,
        };
        
        let analysis_result = self.performance_analyzer.analyze_performance(analysis_config).await?;
        
        let mut metrics = HashMap::new();
        metrics.insert("analysis_score".to_string(), analysis_result.overall_score);
        metrics.insert("risk_score".to_string(), analysis_result.risk_score);
        metrics.insert("return_score".to_string(), analysis_result.return_score);
        
        Ok(ToolResult {
            success: analysis_result.success,
            message: format!("性能分析完成: 總體評分 {:.2}/10", analysis_result.overall_score),
            data: Some(serde_json::to_value(&analysis_result)?),
            execution_time_ms: 0,
            output_files: vec![output_path],
            metrics,
        })
    }
    
    /// Python調用接口 - 統一工具執行函數
    pub async fn execute_tool(&mut self, tool_name: &str, params: serde_json::Value) -> Result<ToolResult> {
        let command = match tool_name {
            "download" => {
                let symbol = params["symbol"].as_str().unwrap_or("BTCUSDT").to_string();
                let data_type = params["data_type"].as_str().unwrap_or("kline").to_string();
                let output_dir = params["output_dir"].as_str().unwrap_or("data/raw").to_string();
                
                HftCommand::Download {
                    symbol,
                    start_time: params["start_time"].as_str().map(|s| s.to_string()),
                    end_time: params["end_time"].as_str().map(|s| s.to_string()),
                    data_type,
                    output_dir,
                }
            },
            
            "clean" => {
                let input_path = params["input_path"].as_str().unwrap().to_string();
                let output_path = params["output_path"].as_str().unwrap().to_string();
                
                HftCommand::Clean {
                    input_path,
                    output_path,
                    config_path: params["config_path"].as_str().map(|s| s.to_string()),
                }
            },
            
            "features" => {
                let input_path = params["input_path"].as_str().unwrap().to_string();
                let output_path = params["output_path"].as_str().unwrap().to_string();
                let feature_types = params["feature_types"].as_str().unwrap_or("all").to_string();
                
                HftCommand::Features {
                    input_path,
                    output_path,
                    feature_types,
                    windows: params["windows"].as_str().map(|s| s.to_string()),
                }
            },
            
            "backtest" => {
                let strategy_config = params["strategy_config"].as_str().unwrap().to_string();
                let data_path = params["data_path"].as_str().unwrap().to_string();
                let start_date = params["start_date"].as_str().unwrap().to_string();
                let end_date = params["end_date"].as_str().unwrap().to_string();
                let output_path = params["output_path"].as_str().unwrap().to_string();
                let initial_capital = params["initial_capital"].as_f64().unwrap_or(100000.0);
                
                HftCommand::Backtest {
                    strategy_config,
                    data_path,
                    start_date,
                    end_date,
                    initial_capital,
                    output_path,
                }
            },
            
            "trade" => {
                let strategy_config = params["strategy_config"].as_str().unwrap().to_string();
                let symbol = params["symbol"].as_str().unwrap().to_string();
                let dry_run = params["dry_run"].as_bool().unwrap_or(true);
                let capital = params["capital"].as_f64().unwrap_or(10000.0);
                
                HftCommand::Trade {
                    strategy_config,
                    dry_run,
                    symbol,
                    capital,
                }
            },
            
            "analyze" => {
                let results_path = params["results_path"].as_str().unwrap().to_string();
                let output_path = params["output_path"].as_str().unwrap().to_string();
                
                HftCommand::Analyze {
                    results_path,
                    benchmark: params["benchmark"].as_str().map(|s| s.to_string()),
                    output_path,
                }
            },
            
            _ => return Err(anyhow::anyhow!("未知的工具類型: {}", tool_name)),
        };
        
        self.execute_command(command).await
    }
}

// 輔助函數
fn parse_data_type(data_type: &str) -> Result<DataType> {
    match data_type.to_lowercase().as_str() {
        "kline" | "candlestick" => Ok(DataType::Kline),
        "ticker" => Ok(DataType::Ticker),
        "orderbook" | "depth" => Ok(DataType::OrderBook),
        "trade" | "trades" => Ok(DataType::Trades),
        _ => Err(anyhow::anyhow!("不支持的數據類型: {}", data_type)),
    }
}

fn parse_time_option(time_str: Option<String>) -> Result<Option<Timestamp>> {
    match time_str {
        Some(s) => Ok(Some(parse_timestamp(&s)?)),
        None => Ok(None),
    }
}

fn parse_timestamp(time_str: &str) -> Result<Timestamp> {
    // 簡化的時間解析，實際實現會更複雜
    Ok(chrono::Utc::now().timestamp_micros() as u64)
}

fn parse_date(date_str: &str) -> Result<chrono::NaiveDate> {
    chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d")
        .map_err(|e| anyhow::anyhow!("日期解析錯誤: {}", e))
}

fn parse_feature_types(types_str: &str) -> Result<Vec<FeatureType>> {
    let types: Vec<&str> = types_str.split(',').collect();
    let mut feature_types = Vec::new();
    
    for type_str in types {
        match type_str.trim().to_lowercase().as_str() {
            "all" => return Ok(vec![
                FeatureType::Technical,
                FeatureType::Microstructure,
                FeatureType::Statistical,
                FeatureType::Temporal,
            ]),
            "technical" => feature_types.push(FeatureType::Technical),
            "microstructure" => feature_types.push(FeatureType::Microstructure),
            "statistical" => feature_types.push(FeatureType::Statistical),
            "temporal" => feature_types.push(FeatureType::Temporal),
            _ => return Err(anyhow::anyhow!("未知的特徵類型: {}", type_str)),
        }
    }
    
    Ok(feature_types)
}

fn parse_windows(windows_str: Option<String>) -> Result<Vec<u32>> {
    match windows_str {
        Some(s) => {
            let windows: Result<Vec<u32>, _> = s.split(',')
                .map(|w| w.trim().parse::<u32>())
                .collect();
            windows.map_err(|e| anyhow::anyhow!("時間窗口解析錯誤: {}", e))
        },
        None => Ok(vec![30, 60, 120, 300]), // 默認窗口
    }
}

// 數據類型定義
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DataType {
    Kline,
    Ticker,
    OrderBook,
    Trades,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FeatureType {
    Technical,
    Microstructure,
    Statistical,
    Temporal,
}

// 配置結構體定義（簡化）
#[derive(Debug, Clone)]
pub struct DownloadConfig {
    pub symbol: String,
    pub data_type: DataType,
    pub start_time: Option<Timestamp>,
    pub end_time: Option<Timestamp>,
    pub output_directory: String,
    pub batch_size: u32,
    pub rate_limit_ms: u64,
}

#[derive(Debug, Clone)]
pub struct CleaningConfig {
    pub remove_outliers: bool,
    pub outlier_threshold: f64,
    pub fill_missing: bool,
    pub validate_timestamps: bool,
    pub min_data_quality: f64,
}

impl Default for CleaningConfig {
    fn default() -> Self {
        Self {
            remove_outliers: true,
            outlier_threshold: 3.0,
            fill_missing: true,
            validate_timestamps: true,
            min_data_quality: 0.95,
        }
    }
}

impl CleaningConfig {
    pub fn from_file(path: &str) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: CleaningConfig = serde_yaml::from_str(&content)?;
        Ok(config)
    }
}

#[derive(Debug, Clone)]
pub struct FeatureConfig {
    pub input_path: String,
    pub output_path: String,
    pub feature_types: Vec<FeatureType>,
    pub time_windows: Vec<u32>,
    pub include_technical_indicators: bool,
    pub include_microstructure: bool,
    pub include_sentiment: bool,
}

#[derive(Debug, Clone)]
pub struct BacktestConfig {
    pub strategy_config_path: String,
    pub data_path: String,
    pub start_date: chrono::NaiveDate,
    pub end_date: chrono::NaiveDate,
    pub initial_capital: f64,
    pub commission_rate: f64,
    pub slippage_bps: f64,
    pub output_path: String,
}

#[derive(Debug, Clone)]
pub struct LiveTradingConfig {
    pub strategy_config_path: String,
    pub symbol: String,
    pub initial_capital: f64,
    pub dry_run: bool,
    pub max_position_size: f64,
    pub stop_loss_pct: f64,
    pub take_profit_pct: f64,
}

#[derive(Debug, Clone)]
pub struct AnalysisConfig {
    pub results_path: String,
    pub benchmark_symbol: Option<String>,
    pub output_path: String,
    pub include_risk_metrics: bool,
    pub include_comparison: bool,
    pub generate_plots: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_parse_data_type() {
        assert!(matches!(parse_data_type("kline").unwrap(), DataType::Kline));
        assert!(matches!(parse_data_type("orderbook").unwrap(), DataType::OrderBook));
        assert!(parse_data_type("invalid").is_err());
    }
    
    #[test]
    fn test_parse_feature_types() {
        let types = parse_feature_types("technical,microstructure").unwrap();
        assert_eq!(types.len(), 2);
        
        let all_types = parse_feature_types("all").unwrap();
        assert_eq!(all_types.len(), 4);
    }
    
    #[test]
    fn test_parse_windows() {
        let windows = parse_windows(Some("30,60,120".to_string())).unwrap();
        assert_eq!(windows, vec![30, 60, 120]);
        
        let default_windows = parse_windows(None).unwrap();
        assert_eq!(default_windows, vec![30, 60, 120, 300]);
    }
}