/*!
 * 統一命令行參數系統 - 消除重複的參數定義
 * 
 * 提供標準化的命令行參數組合：
 * - 基礎參數 (symbol, duration, dry-run)
 * - 交易參數 (capital, position size)
 * - 訓練參數 (epochs, batch size, learning rate)
 * - 性能參數 (CPU core, SIMD, iterations)
 */

use clap::{Parser, ValueEnum};
use serde::{Serialize, Deserialize};

/// 基礎通用參數 - 所有應用都需要的參數
#[derive(Parser, Debug, Clone)]
pub struct CommonArgs {
    /// 交易標的符號
    #[arg(short, long, default_value = "BTCUSDT")]
    pub symbol: String,
    
    /// 運行時長（秒）
    #[arg(long, default_value_t = 3600)]
    pub duration_seconds: u64,
    
    /// 乾跑模式（不實際執行交易）
    #[arg(long, default_value_t = true)]
    pub dry_run: bool,
    
    /// 詳細日誌模式
    #[arg(short, long, default_value_t = false)]
    pub verbose: bool,
    
    /// 輸出檔案路徑
    #[arg(short, long)]
    pub output: Option<String>,
}

/// 交易相關參數
#[derive(Parser, Debug, Clone)]
pub struct TradingArgs {
    /// 初始資金（USDT）
    #[arg(long, default_value_t = 1000.0)]
    pub capital: f64,
    
    /// 最大倉位比例（0-1）
    #[arg(long, default_value_t = 0.1)]
    pub max_position_pct: f64,
    
    /// 交易手續費率
    #[arg(long, default_value_t = 0.001)]
    pub trading_fee: f64,
    
    /// 止損百分比
    #[arg(long, default_value_t = 0.02)]
    pub stop_loss_pct: f64,
    
    /// 交易模式
    #[arg(long, value_enum, default_value_t = TradingMode::DryRun)]
    pub mode: TradingMode,
}

/// 機器學習訓練參數
#[derive(Parser, Debug, Clone)]
pub struct TrainingArgs {
    /// 訓練輪數
    #[arg(short, long, default_value_t = 50)]
    pub epochs: usize,
    
    /// 批次大小
    #[arg(short, long, default_value_t = 32)]
    pub batch_size: usize,
    
    /// 學習率
    #[arg(long, default_value_t = 1e-4)]
    pub learning_rate: f64,
    
    /// 數據收集時長（小時）
    #[arg(long, default_value_t = 2)]
    pub collect_hours: u32,
    
    /// 跳過數據收集，使用現有數據
    #[arg(long, default_value_t = false)]
    pub skip_collection: bool,
    
    /// 模型保存路徑
    #[arg(long, default_value = "models/")]
    pub model_path: String,
    
    /// 驗證集比例
    #[arg(long, default_value_t = 0.2)]
    pub validation_split: f64,
}

/// 模型評估參數
#[derive(Parser, Debug, Clone)]
pub struct EvaluationArgs {
    /// 模型檔案路徑
    #[arg(short, long, default_value = "models/lob_transformer.safetensors")]
    pub model_path: String,
    
    /// 測試天數
    #[arg(long, default_value_t = 7)]
    pub test_days: u32,
    
    /// 置信度閾值
    #[arg(long, default_value_t = 0.6)]
    pub confidence_threshold: f64,
    
    /// 只進行回測，不做實時評估
    #[arg(long, default_value_t = false)]
    pub backtest_only: bool,
    
    /// 實時評估模式
    #[arg(long, default_value_t = false)]
    pub live_evaluation: bool,
}

/// 性能測試參數
#[derive(Parser, Debug, Clone)]
pub struct PerformanceArgs {
    /// 測試迭代次數
    #[arg(long, default_value_t = 10000)]
    pub iterations: usize,
    
    /// 啟用SIMD加速
    #[arg(long, default_value_t = true)]
    pub enable_simd: bool,
    
    /// 啟用模型量化
    #[arg(long, default_value_t = true)]
    pub enable_quantization: bool,
    
    /// 啟用模型剪枝
    #[arg(long, default_value_t = false)]
    pub enable_pruning: bool,
    
    /// 綁定CPU核心
    #[arg(long, default_value_t = 0)]
    pub cpu_core: u32,
    
    /// 執行延遲測試
    #[arg(long, default_value_t = false)]
    pub test_latency: bool,
    
    /// 目標延遲（微秒）
    #[arg(long, default_value_t = 50)]
    pub target_latency_us: u64,
}

/// 數據收集參數
#[derive(Parser, Debug, Clone)]
pub struct DataArgs {
    /// 數據輸出檔案
    #[arg(short = 'f', long, default_value = "market_data.jsonl")]
    pub output_file: String,
    
    /// 數據格式
    #[arg(long, value_enum, default_value_t = DataFormat::Jsonl)]
    pub format: DataFormat,
    
    /// 壓縮輸出
    #[arg(long, default_value_t = false)]
    pub compress: bool,
    
    /// 最大檔案大小（MB）
    #[arg(long, default_value_t = 1000)]
    pub max_file_size_mb: usize,
}

/// 交易模式枚舉
#[derive(Clone, ValueEnum, Debug, Serialize, Deserialize)]
pub enum TradingMode {
    /// 乾跑模式 - 連接真實數據但不下單
    DryRun,
    /// 紙上交易 - 模擬下單
    Paper,
    /// 實盤交易 - 真實下單
    Live,
}

/// 回測參數
#[derive(Parser, Debug, Clone)]
pub struct BacktestingArgs {
    /// 歷史數據輸入檔案
    #[arg(short, long, default_value = "market_data.jsonl")]
    pub input_file: String,
    
    /// 初始資金
    #[arg(long, default_value_t = 10000.0)]
    pub initial_capital: f64,
    
    /// 交易手續費率
    #[arg(long, default_value_t = 0.001)]
    pub trading_fee: f64,
    
    /// 最大樣本數量
    #[arg(long)]
    pub max_samples: Option<usize>,
    
    /// 策略名稱
    #[arg(long, default_value = "simple_obi")]
    pub strategy: String,
}

/// 數據格式枚舉
#[derive(Clone, ValueEnum, Debug)]
pub enum DataFormat {
    /// JSON Lines格式
    Jsonl,
    /// CSV格式
    Csv,
    /// Parquet格式
    Parquet,
    /// 二進制格式
    Binary,
}

// === 組合參數結構 ===

/// 基礎連接測試參數
#[derive(Parser, Debug)]
#[command(name = "basic_connection")]
#[command(about = "Basic Bitget connection test")]
pub struct BasicConnectionArgs {
    #[command(flatten)]
    pub common: CommonArgs,
}

/// 數據收集應用參數
#[derive(Parser, Debug)]
#[command(name = "data_collection")]
#[command(about = "Collect market data from Bitget")]
pub struct DataCollectionArgs {
    #[command(flatten)]
    pub common: CommonArgs,
    
    #[command(flatten)]
    pub data: DataArgs,
}

/// 模型訓練應用參數
#[derive(Parser, Debug)]
#[command(name = "model_training")]
#[command(about = "Train LOB Transformer model")]
pub struct ModelTrainingArgs {
    #[command(flatten)]
    pub common: CommonArgs,
    
    #[command(flatten)]
    pub training: TrainingArgs,
    
    #[command(flatten)]
    pub performance: PerformanceArgs,
}

/// 模型評估應用參數
#[derive(Parser, Debug)]
#[command(name = "model_evaluation")]
#[command(about = "Evaluate trained model")]
pub struct ModelEvaluationArgs {
    #[command(flatten)]
    pub common: CommonArgs,
    
    #[command(flatten)]
    pub trading: TradingArgs,
    
    #[command(flatten)]
    pub evaluation: EvaluationArgs,
}

/// 回測應用參數
#[derive(Parser, Debug)]
#[command(name = "backtesting")]
#[command(about = "Backtest trading strategies")]
pub struct BacktestArgs {
    #[command(flatten)]
    pub common: CommonArgs,
    
    #[command(flatten)]
    pub backtest: BacktestingArgs,
}

/// 完整交易系統參數
#[derive(Parser, Debug)]
#[command(name = "trading_system")]
#[command(about = "Complete HFT trading system")]
pub struct TradingSystemArgs {
    #[command(flatten)]
    pub common: CommonArgs,
    
    #[command(flatten)]
    pub trading: TradingArgs,
    
    #[command(flatten)]
    pub performance: PerformanceArgs,
}

/// 性能優化測試參數
#[derive(Parser, Debug)]
#[command(name = "performance_test")]
#[command(about = "Performance optimization testing")]
pub struct PerformanceTestArgs {
    #[command(flatten)]
    pub common: CommonArgs,
    
    #[command(flatten)]
    pub performance: PerformanceArgs,
}

// === 便利函數 ===

impl CommonArgs {
    /// 檢查是否為詳細模式
    pub fn is_verbose(&self) -> bool {
        self.verbose
    }
    
    /// 獲取日誌級別
    pub fn log_level(&self) -> tracing::Level {
        if self.verbose {
            tracing::Level::DEBUG
        } else {
            tracing::Level::INFO
        }
    }
    
    /// 獲取輸出檔案路徑，如果未指定則使用默認
    pub fn output_path(&self, default_name: &str) -> String {
        self.output.clone().unwrap_or_else(|| {
            format!("{}_{}.out", default_name, 
                   chrono::Utc::now().format("%Y%m%d_%H%M%S"))
        })
    }
}

impl TradingArgs {
    /// 計算最大倉位價值
    pub fn max_position_value(&self) -> f64 {
        self.capital * self.max_position_pct
    }
    
    /// 檢查是否為實盤模式
    pub fn is_live_mode(&self) -> bool {
        matches!(self.mode, TradingMode::Live)
    }
    
    /// 檢查是否允許下單
    pub fn can_place_orders(&self) -> bool {
        !matches!(self.mode, TradingMode::DryRun)
    }
}

impl TrainingArgs {
    /// 獲取完整模型路徑
    pub fn full_model_path(&self, model_name: &str) -> String {
        format!("{}/{}", self.model_path.trim_end_matches('/'), model_name)
    }
    
    /// 計算總預期樣本數
    pub fn estimated_samples(&self) -> usize {
        (self.collect_hours as usize) * 3600 * 10 // 假設每秒10個樣本
    }
    
    /// 計算驗證集大小
    pub fn validation_samples(&self, total_samples: usize) -> usize {
        (total_samples as f64 * self.validation_split) as usize
    }
}

impl PerformanceArgs {
    /// 檢查是否達到延遲目標
    pub fn meets_latency_target(&self, actual_latency_us: u64) -> bool {
        actual_latency_us <= self.target_latency_us
    }
    
    /// 獲取優化配置
    pub fn to_optimization_config(&self) -> crate::utils::performance::PerformanceConfig {
        crate::utils::performance::PerformanceConfig {
            cpu_isolation: true,
            memory_prefaulting: true,
            huge_pages: false,
            numa_awareness: true,
            simd_acceleration: self.enable_simd,
            cache_optimization: true,
            gc_optimization: true,
        }
    }
}

/// 創建標準日誌初始化函數
pub fn init_logging(args: &CommonArgs) {
    tracing_subscriber::fmt()
        .with_max_level(args.log_level())
        .with_target(args.is_verbose())
        .init();
}

/// 打印參數摘要
pub fn print_args_summary<T: std::fmt::Debug>(args: &T, app_name: &str) {
    tracing::info!("🚀 {} started", app_name);
    tracing::debug!("Configuration: {:#?}", args);
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    
    #[test]
    fn test_basic_connection_args() {
        let cmd = BasicConnectionArgs::command();
        cmd.debug_assert();
    }
    
    #[test]
    fn test_trading_args_calculation() {
        let args = TradingArgs {
            capital: 1000.0,
            max_position_pct: 0.1,
            mode: TradingMode::DryRun,
            ..Default::default()
        };
        
        assert_eq!(args.max_position_value(), 100.0);
        assert!(!args.is_live_mode());
        assert!(!args.can_place_orders());
    }
    
    #[test]
    fn test_training_args_calculations() {
        let args = TrainingArgs {
            model_path: "models".to_string(),
            collect_hours: 2,
            validation_split: 0.2,
            ..Default::default()
        };
        
        assert_eq!(args.full_model_path("test.model"), "models/test.model");
        assert_eq!(args.estimated_samples(), 72000); // 2 * 3600 * 10
        assert_eq!(args.validation_samples(1000), 200);
    }
    
    #[test] 
    fn test_performance_args() {
        let args = PerformanceArgs {
            target_latency_us: 50,
            enable_simd: true,
            ..Default::default()
        };
        
        assert!(args.meets_latency_target(40));
        assert!(!args.meets_latency_target(60));
    }
}

// 為沒有Default的結構實現Default
impl Default for TradingArgs {
    fn default() -> Self {
        Self {
            capital: 1000.0,
            max_position_pct: 0.1,
            trading_fee: 0.001,
            stop_loss_pct: 0.02,
            mode: TradingMode::DryRun,
        }
    }
}

impl Default for TrainingArgs {
    fn default() -> Self {
        Self {
            epochs: 50,
            batch_size: 32,
            learning_rate: 1e-4,
            collect_hours: 2,
            skip_collection: false,
            model_path: "models/".to_string(),
            validation_split: 0.2,
        }
    }
}

impl Default for PerformanceArgs {
    fn default() -> Self {
        Self {
            iterations: 10000,
            enable_simd: true,
            enable_quantization: true,
            enable_pruning: false,
            cpu_core: 0,
            test_latency: false,
            target_latency_us: 50,
        }
    }
}