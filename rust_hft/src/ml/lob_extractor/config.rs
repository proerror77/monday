/*!
 * LOB Time Series Configuration
 * 
 * 配置时间序列特征提取的参数
 */

/// Time series configuration for LOB analysis
#[derive(Debug, Clone)]
pub struct LobTimeSeriesConfig {
    /// Lookback window for DL trend prediction (seconds)
    pub dl_lookback_seconds: u64,          // 30 seconds
    
    /// Prediction horizon for DL model (seconds)  
    pub dl_prediction_seconds: u64,        // 10 seconds
    
    /// RL state history window (milliseconds)
    pub rl_state_window_ms: u64,           // 5000 ms (5 seconds)
    
    /// Feature extraction frequency (microseconds)
    pub extraction_interval_us: u64,      // 500_000 μs (500ms)
    
    /// Maximum sequence length for Transformer
    pub max_sequence_length: usize,       // 60 steps (30s / 500ms)
    
    /// LOB depth for tensor representation
    pub lob_depth: usize,                 // 20 levels
    
    /// Enable advanced LOB features
    pub enable_advanced_features: bool,    // true
    
    /// Memory optimization frequency
    pub memory_cleanup_interval: u64,     // 60 seconds
}

impl Default for LobTimeSeriesConfig {
    fn default() -> Self {
        Self {
            dl_lookback_seconds: 30,
            dl_prediction_seconds: 10,
            rl_state_window_ms: 5000,
            extraction_interval_us: 500_000,
            max_sequence_length: 60,
            lob_depth: 20,
            enable_advanced_features: true,
            memory_cleanup_interval: 60,
        }
    }
}

impl LobTimeSeriesConfig {
    /// Create configuration for high-frequency trading
    pub fn for_hft() -> Self {
        Self {
            dl_lookback_seconds: 5,
            dl_prediction_seconds: 2,
            rl_state_window_ms: 1000,
            extraction_interval_us: 100_000, // 100ms
            max_sequence_length: 50,
            lob_depth: 10,
            enable_advanced_features: true,
            memory_cleanup_interval: 30,
        }
    }

    /// Create configuration for medium-frequency trading
    pub fn for_mft() -> Self {
        Self {
            dl_lookback_seconds: 60,
            dl_prediction_seconds: 20,
            rl_state_window_ms: 10000,
            extraction_interval_us: 1_000_000, // 1s
            max_sequence_length: 60,
            lob_depth: 20,
            enable_advanced_features: true,
            memory_cleanup_interval: 120,
        }
    }

    /// Create configuration for backtesting
    pub fn for_backtest() -> Self {
        Self {
            dl_lookback_seconds: 300, // 5 minutes
            dl_prediction_seconds: 60, // 1 minute
            rl_state_window_ms: 30000, // 30 seconds
            extraction_interval_us: 5_000_000, // 5s
            max_sequence_length: 60,
            lob_depth: 50,
            enable_advanced_features: true,
            memory_cleanup_interval: 300,
        }
    }

    /// Validate configuration
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.dl_lookback_seconds == 0 {
            return Err(anyhow::anyhow!("DL lookback seconds must be > 0"));
        }
        
        if self.dl_prediction_seconds == 0 {
            return Err(anyhow::anyhow!("DL prediction seconds must be > 0"));
        }
        
        if self.extraction_interval_us == 0 {
            return Err(anyhow::anyhow!("Extraction interval must be > 0"));
        }
        
        if self.max_sequence_length == 0 {
            return Err(anyhow::anyhow!("Max sequence length must be > 0"));
        }
        
        if self.lob_depth == 0 {
            return Err(anyhow::anyhow!("LOB depth must be > 0"));
        }
        
        Ok(())
    }
}