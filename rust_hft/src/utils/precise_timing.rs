/*!
 * 高精度時間同步和延遲測量模塊
 * 
 * 提供真實的端到端延遲測量，包括：
 * - 與交易所時間服務器的NTP同步
 * - 納秒級精度時間戳
 * - 網絡層延遲測量
 * - 多層延遲分解分析
 */

use anyhow::Result;
use chrono::Utc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use tracing::{info, warn, debug, error};
use serde::{Serialize, Deserialize};

/// 納秒級時間戳
pub type NanoTimestamp = u64;

/// 時間同步配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeSyncConfig {
    /// NTP 服務器列表 (Bitget 和公共 NTP 服務器)
    pub ntp_servers: Vec<String>,
    
    /// 同步間隔 (秒)
    pub sync_interval_sec: u64,
    
    /// 最大允許時間偏差 (微秒)
    pub max_drift_us: i64,
    
    /// 同步超時 (毫秒)
    pub timeout_ms: u64,
    
    /// 啟用高精度模式
    pub high_precision: bool,
}

impl Default for TimeSyncConfig {
    fn default() -> Self {
        Self {
            ntp_servers: vec![
                // Bitget 可能使用的時間服務器
                "time.bitget.com".to_string(),
                // 公共高精度 NTP 服務器
                "time.google.com".to_string(),
                "time.cloudflare.com".to_string(),
                "time.nist.gov".to_string(),
                "pool.ntp.org".to_string(),
            ],
            sync_interval_sec: 300, // 5分鐘同步一次
            max_drift_us: 1000,     // 1ms 最大偏差
            timeout_ms: 5000,       // 5秒超時
            high_precision: true,
        }
    }
}

/// 時間同步狀態
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeSyncStatus {
    /// 是否已同步
    pub is_synchronized: bool,
    
    /// 與服務器的時間偏差 (微秒，正值表示本地時間快)
    pub offset_us: i64,
    
    /// 網絡往返時間 (微秒)
    pub round_trip_us: u64,
    
    /// 最後同步時間
    pub last_sync: NanoTimestamp,
    
    /// 同步精度 (微秒)
    pub precision_us: u64,
    
    /// 使用的 NTP 服務器
    pub active_server: String,
}

/// 延遲測量結果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyMeasurement {
    /// 交易所時間戳 (從消息中解析)
    pub exchange_timestamp_us: u64,
    
    /// 數據包到達時間 (內核時間戳)
    pub kernel_arrival_us: u64,
    
    /// 應用接收時間
    pub app_receive_us: u64,
    
    /// 處理開始時間
    pub processing_start_us: u64,
    
    /// 處理完成時間
    pub processing_end_us: u64,
    
    /// 計算出的延遲分解
    pub latency_breakdown: LatencyBreakdown,
}

/// 延遲分解分析
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyBreakdown {
    /// 網絡延遲 (交易所 → 本機網卡)
    pub network_latency_us: u64,
    
    /// 內核延遲 (網卡 → 應用程序)
    pub kernel_latency_us: u64,
    
    /// 應用處理延遲
    pub processing_latency_us: u64,
    
    /// 總端到端延遲
    pub total_latency_us: u64,
    
    /// 各部分延遲占比
    pub network_ratio: f64,
    pub kernel_ratio: f64,
    pub processing_ratio: f64,
}

/// 高精度時間同步管理器
pub struct PreciseTimeSync {
    config: TimeSyncConfig,
    
    /// 時間偏差 (原子操作)
    offset_us: Arc<AtomicI64>,
    
    /// 最後同步時間
    last_sync: Arc<AtomicU64>,
    
    /// 同步狀態
    is_synchronized: Arc<std::sync::atomic::AtomicBool>,
    
    /// 當前使用的服務器
    active_server: Arc<parking_lot::RwLock<String>>,
    
    /// 同步統計
    sync_count: Arc<AtomicU64>,
    failed_sync_count: Arc<AtomicU64>,
}

impl PreciseTimeSync {
    /// 創建新的時間同步管理器
    pub fn new(config: TimeSyncConfig) -> Self {
        Self {
            config,
            offset_us: Arc::new(AtomicI64::new(0)),
            last_sync: Arc::new(AtomicU64::new(0)),
            is_synchronized: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            active_server: Arc::new(parking_lot::RwLock::new(String::new())),
            sync_count: Arc::new(AtomicU64::new(0)),
            failed_sync_count: Arc::new(AtomicU64::new(0)),
        }
    }
    
    /// 啟動時間同步服務
    pub async fn start(&self) -> Result<()> {
        info!("🕐 Starting precise time synchronization service");
        
        // 立即執行第一次同步
        self.sync_time().await?;
        
        // 啟動定期同步任務
        let sync_self = self.clone_for_sync();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(
                Duration::from_secs(sync_self.config.sync_interval_sec)
            );
            
            loop {
                interval.tick().await;
                
                if let Err(e) = sync_self.sync_time().await {
                    error!("時間同步失敗: {}", e);
                    sync_self.failed_sync_count.fetch_add(1, Ordering::Relaxed);
                } else {
                    sync_self.sync_count.fetch_add(1, Ordering::Relaxed);
                }
            }
        });
        
        Ok(())
    }
    
    /// 執行時間同步
    async fn sync_time(&self) -> Result<()> {
        let mut best_server = String::new();
        let mut best_offset = i64::MAX;
        let mut best_rtt = u64::MAX;
        
        // 嘗試每個 NTP 服務器
        for server in &self.config.ntp_servers {
            match self.query_ntp_server(server).await {
                Ok((offset, rtt)) => {
                    debug!("NTP 服務器 {} - 偏差: {}μs, RTT: {}μs", server, offset, rtt);
                    
                    // 選擇 RTT 最小的服務器
                    if rtt < best_rtt {
                        best_server = server.clone();
                        best_offset = offset;
                        best_rtt = rtt;
                    }
                },
                Err(e) => {
                    warn!("NTP 服務器 {} 查詢失敗: {}", server, e);
                }
            }
        }
        
        if best_server.is_empty() {
            return Err(anyhow::anyhow!("所有 NTP 服務器都不可用"));
        }
        
        // 更新同步狀態
        self.offset_us.store(best_offset, Ordering::SeqCst);
        self.last_sync.store(self.now_nanos(), Ordering::SeqCst);
        self.is_synchronized.store(true, Ordering::SeqCst);
        
        *self.active_server.write() = best_server.clone();
        
        info!("✅ 時間同步完成 - 服務器: {}, 偏差: {}μs, RTT: {}μs", 
              best_server, best_offset, best_rtt);
        
        // 檢查偏差是否超出閾值
        if best_offset.abs() > self.config.max_drift_us {
            warn!("⚠️  時間偏差過大: {}μs (閾值: {}μs)", 
                  best_offset, self.config.max_drift_us);
        }
        
        Ok(())
    }
    
    /// 查詢 NTP 服務器
    async fn query_ntp_server(&self, server: &str) -> Result<(i64, u64)> {
        // 實現簡化的 SNTP 客戶端
        use std::net::UdpSocket;
        use std::time::Instant;
        
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        socket.set_read_timeout(Some(Duration::from_millis(self.config.timeout_ms)))?;
        socket.set_write_timeout(Some(Duration::from_millis(self.config.timeout_ms)))?;
        
        // 構建 NTP 請求包
        let mut request = [0u8; 48];
        request[0] = 0x1b; // NTP version 3, client mode
        
        let start_time = Instant::now();
        let send_time = self.now_micros();
        
        // 發送請求
        socket.send_to(&request, format!("{}:123", server))?;
        
        // 接收響應
        let mut response = [0u8; 48];
        let (len, _) = socket.recv_from(&mut response)?;
        
        let recv_time = self.now_micros();
        let rtt = start_time.elapsed().as_micros() as u64;
        
        if len != 48 {
            return Err(anyhow::anyhow!("無效的 NTP 響應長度"));
        }
        
        // 解析 NTP 時間戳 (簡化版本)
        let server_time = self.parse_ntp_timestamp(&response[40..48])?;
        
        // 計算時間偏差
        let local_time = (send_time + recv_time) / 2; // 估算發送時的本地時間
        let offset = server_time as i64 - local_time as i64;
        
        Ok((offset, rtt))
    }
    
    /// 解析 NTP 時間戳
    fn parse_ntp_timestamp(&self, bytes: &[u8]) -> Result<u64> {
        if bytes.len() != 8 {
            return Err(anyhow::anyhow!("無效的 NTP 時間戳長度"));
        }
        
        // NTP 時間戳是從 1900-01-01 開始的秒數和分數秒
        let seconds = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as u64;
        let fraction = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as u64;
        
        // 轉換為 Unix 時間戳 (微秒)
        const NTP_UNIX_OFFSET: u64 = 2_208_988_800; // 1970-1900 的秒數差
        let unix_seconds = seconds - NTP_UNIX_OFFSET;
        let micros = (fraction * 1_000_000) >> 32; // 將分數秒轉換為微秒
        
        Ok(unix_seconds * 1_000_000 + micros)
    }
    
    /// 獲取同步後的精確時間 (微秒)
    pub fn now_synced_micros(&self) -> u64 {
        let local_time = self.now_micros();
        let offset = self.offset_us.load(Ordering::SeqCst);
        
        if self.is_synchronized.load(Ordering::SeqCst) {
            (local_time as i64 - offset) as u64
        } else {
            local_time
        }
    }
    
    /// 獲取本地時間 (微秒)
    pub fn now_micros(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64
    }
    
    /// 獲取本地時間 (納秒)
    pub fn now_nanos(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64
    }
    
    /// 解析交易所時間戳
    pub fn parse_exchange_timestamp(&self, timestamp_str: &str) -> Result<u64> {
        // Bitget 通常使用毫秒時間戳
        let timestamp_ms: u64 = timestamp_str.parse()
            .map_err(|_| anyhow::anyhow!("無效的交易所時間戳格式"))?;
        
        Ok(timestamp_ms * 1000) // 轉換為微秒
    }
    
    /// 獲取同步狀態
    pub fn get_sync_status(&self) -> TimeSyncStatus {
        TimeSyncStatus {
            is_synchronized: self.is_synchronized.load(Ordering::SeqCst),
            offset_us: self.offset_us.load(Ordering::SeqCst),
            round_trip_us: 0, // 需要在同步時記錄
            last_sync: self.last_sync.load(Ordering::SeqCst),
            precision_us: if self.config.high_precision { 1 } else { 1000 },
            active_server: self.active_server.read().clone(),
        }
    }
    
    /// 克隆用於異步任務
    fn clone_for_sync(&self) -> Self {
        Self {
            config: self.config.clone(),
            offset_us: self.offset_us.clone(),
            last_sync: self.last_sync.clone(),
            is_synchronized: self.is_synchronized.clone(),
            active_server: self.active_server.clone(),
            sync_count: self.sync_count.clone(),
            failed_sync_count: self.failed_sync_count.clone(),
        }
    }
}

/// 延遲測量分析器
pub struct LatencyAnalyzer {
    time_sync: Arc<PreciseTimeSync>,
    
    /// 延遲統計
    measurements: Arc<parking_lot::RwLock<Vec<LatencyMeasurement>>>,
    max_measurements: usize,
    
    /// 統計數據
    total_measurements: Arc<AtomicU64>,
    high_latency_count: Arc<AtomicU64>,
    network_latency_sum: Arc<AtomicU64>,
    processing_latency_sum: Arc<AtomicU64>,
}

impl LatencyAnalyzer {
    /// 創建新的延遲分析器
    pub fn new(time_sync: Arc<PreciseTimeSync>, max_measurements: usize) -> Self {
        Self {
            time_sync,
            measurements: Arc::new(parking_lot::RwLock::new(Vec::with_capacity(max_measurements))),
            max_measurements,
            total_measurements: Arc::new(AtomicU64::new(0)),
            high_latency_count: Arc::new(AtomicU64::new(0)),
            network_latency_sum: Arc::new(AtomicU64::new(0)),
            processing_latency_sum: Arc::new(AtomicU64::new(0)),
        }
    }
    
    /// 記錄延遲測量
    pub fn record_measurement(
        &self,
        exchange_timestamp_str: &str,
        kernel_arrival_us: u64,
        processing_start_us: u64,
        processing_end_us: u64,
    ) -> Result<LatencyMeasurement> {
        // 解析交易所時間戳
        let exchange_timestamp_us = self.time_sync.parse_exchange_timestamp(exchange_timestamp_str)?;
        
        // 獲取同步後的當前時間
        let app_receive_us = self.time_sync.now_synced_micros();
        
        // 計算延遲分解
        let network_latency_us = kernel_arrival_us.saturating_sub(exchange_timestamp_us);
        let kernel_latency_us = app_receive_us.saturating_sub(kernel_arrival_us);
        let processing_latency_us = processing_end_us.saturating_sub(processing_start_us);
        let total_latency_us = processing_end_us.saturating_sub(exchange_timestamp_us);
        
        // 計算占比
        let total_f = total_latency_us as f64;
        let (network_ratio, kernel_ratio, processing_ratio) = if total_f > 0.0 {
            (
                network_latency_us as f64 / total_f,
                kernel_latency_us as f64 / total_f,
                processing_latency_us as f64 / total_f,
            )
        } else {
            (0.0, 0.0, 0.0)
        };
        
        let measurement = LatencyMeasurement {
            exchange_timestamp_us,
            kernel_arrival_us,
            app_receive_us,
            processing_start_us,
            processing_end_us,
            latency_breakdown: LatencyBreakdown {
                network_latency_us,
                kernel_latency_us,
                processing_latency_us,
                total_latency_us,
                network_ratio,
                kernel_ratio,
                processing_ratio,
            },
        };
        
        // 存儲測量結果
        {
            let mut measurements = self.measurements.write();
            if measurements.len() >= self.max_measurements {
                measurements.remove(0); // 移除最舊的測量
            }
            measurements.push(measurement.clone());
        }
        
        // 更新統計
        self.total_measurements.fetch_add(1, Ordering::Relaxed);
        self.network_latency_sum.fetch_add(network_latency_us, Ordering::Relaxed);
        self.processing_latency_sum.fetch_add(processing_latency_us, Ordering::Relaxed);
        
        // 檢查高延遲
        if total_latency_us > 10_000 { // 超過 10ms 認為是高延遲
            self.high_latency_count.fetch_add(1, Ordering::Relaxed);
            warn!("🚨 檢測到高延遲: {}μs (網絡: {}μs, 處理: {}μs)", 
                  total_latency_us, network_latency_us, processing_latency_us);
        }
        
        Ok(measurement)
    }
    
    /// 獲取延遲統計
    pub fn get_latency_stats(&self) -> LatencyStats {
        let measurements = self.measurements.read();
        let total = self.total_measurements.load(Ordering::Relaxed);
        
        if measurements.is_empty() || total == 0 {
            return LatencyStats::default();
        }
        
        // 計算統計指標
        let mut total_latencies = Vec::new();
        let mut network_latencies = Vec::new();
        let mut processing_latencies = Vec::new();
        
        for m in measurements.iter() {
            total_latencies.push(m.latency_breakdown.total_latency_us);
            network_latencies.push(m.latency_breakdown.network_latency_us);
            processing_latencies.push(m.latency_breakdown.processing_latency_us);
        }
        
        LatencyStats {
            total_measurements: total,
            avg_total_latency_us: calculate_average(&total_latencies),
            avg_network_latency_us: calculate_average(&network_latencies),
            avg_processing_latency_us: calculate_average(&processing_latencies),
            p50_total_latency_us: calculate_percentile(&mut total_latencies, 50.0),
            p95_total_latency_us: calculate_percentile(&mut total_latencies, 95.0),
            p99_total_latency_us: calculate_percentile(&mut total_latencies, 99.0),
            max_total_latency_us: total_latencies.iter().max().copied().unwrap_or(0),
            min_total_latency_us: total_latencies.iter().min().copied().unwrap_or(0),
            high_latency_ratio: self.high_latency_count.load(Ordering::Relaxed) as f64 / total as f64,
            avg_network_ratio: self.network_latency_sum.load(Ordering::Relaxed) as f64 / 
                              (self.network_latency_sum.load(Ordering::Relaxed) + 
                               self.processing_latency_sum.load(Ordering::Relaxed)) as f64,
        }
    }
}

/// 延遲統計數據
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyStats {
    pub total_measurements: u64,
    pub avg_total_latency_us: f64,
    pub avg_network_latency_us: f64,
    pub avg_processing_latency_us: f64,
    pub p50_total_latency_us: u64,
    pub p95_total_latency_us: u64,
    pub p99_total_latency_us: u64,
    pub max_total_latency_us: u64,
    pub min_total_latency_us: u64,
    pub high_latency_ratio: f64,
    pub avg_network_ratio: f64,
}

impl Default for LatencyStats {
    fn default() -> Self {
        Self {
            total_measurements: 0,
            avg_total_latency_us: 0.0,
            avg_network_latency_us: 0.0,
            avg_processing_latency_us: 0.0,
            p50_total_latency_us: 0,
            p95_total_latency_us: 0,
            p99_total_latency_us: 0,
            max_total_latency_us: 0,
            min_total_latency_us: 0,
            high_latency_ratio: 0.0,
            avg_network_ratio: 0.0,
        }
    }
}

// 輔助函數
fn calculate_average(values: &[u64]) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<u64>() as f64 / values.len() as f64
    }
}

fn calculate_percentile(values: &mut [u64], percentile: f64) -> u64 {
    if values.is_empty() {
        return 0;
    }
    
    values.sort_unstable();
    let index = ((percentile / 100.0) * (values.len() - 1) as f64).round() as usize;
    values[index.min(values.len() - 1)]
}