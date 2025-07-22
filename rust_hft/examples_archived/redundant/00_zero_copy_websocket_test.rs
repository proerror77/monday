/*!
 * 零拷貝 WebSocket 性能測試
 * 
 * 比較傳統 WebSocket 與零拷貝 WebSocket 的性能差異
 * 目標：驗證消息處理延遲 < 100ns
 */

use std::time::{Duration, Instant};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use clap::Parser;
use tracing::{info, debug};

#[derive(Parser, Debug)]
#[command(about = "零拷貝 WebSocket 性能測試")]
struct Args {
    /// 測試消息數量
    #[arg(short, long, default_value_t = 100000)]
    messages: usize,
    
    /// 消息大小（字節）
    #[arg(short, long, default_value_t = 1024)]
    size: usize,
    
    /// 批量大小
    #[arg(short, long, default_value_t = 64)]
    batch_size: usize,
    
    /// 並發線程數
    #[arg(short, long, default_value_t = 1)]
    threads: usize,
    
    /// 詳細輸出
    #[arg(short, long)]
    verbose: bool,
}

/// 模擬的消息數據
struct MockMessage {
    data: Vec<u8>,
    timestamp: u64,
}

impl MockMessage {
    fn new(size: usize) -> Self {
        let mut data = vec![0u8; size];
        // 填充一些模擬的 JSON 數據
        let json_template = r#"{"channel":"books5","instId":"BTCUSDT","data":[{"asks":[],"bids":[],"ts":"1234567890"}]}"#;
        let template_bytes = json_template.as_bytes();
        let repeat_count = size / template_bytes.len();
        
        for i in 0..repeat_count {
            let start = i * template_bytes.len();
            let end = std::cmp::min(start + template_bytes.len(), size);
            data[start..end].copy_from_slice(&template_bytes[..end-start]);
        }
        
        Self {
            data,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_micros() as u64,
        }
    }
}

/// 傳統消息處理器（帶分配）
struct TraditionalProcessor {
    processed_count: AtomicU64,
    total_latency_ns: AtomicU64,
}

impl TraditionalProcessor {
    fn new() -> Self {
        Self {
            processed_count: AtomicU64::new(0),
            total_latency_ns: AtomicU64::new(0),
        }
    }

    fn process_message(&self, message: &MockMessage) {
        let start = Instant::now();
        
        // 模擬傳統處理：分配新的 Vec
        let _copied_data = message.data.clone();
        
        // 模擬 JSON 解析（會分配內存）
        let _json_str = String::from_utf8_lossy(&message.data);
        
        // 模擬一些處理邏輯
        let _checksum: u32 = message.data.iter().map(|&b| b as u32).sum();
        
        let elapsed = start.elapsed().as_nanos() as u64;
        self.processed_count.fetch_add(1, Ordering::Relaxed);
        self.total_latency_ns.fetch_add(elapsed, Ordering::Relaxed);
    }

    fn stats(&self) -> (u64, f64) {
        let count = self.processed_count.load(Ordering::Relaxed);
        let total_latency = self.total_latency_ns.load(Ordering::Relaxed);
        let avg_latency = if count > 0 { total_latency as f64 / count as f64 } else { 0.0 };
        (count, avg_latency)
    }
}

/// 零拷貝消息處理器
struct ZeroCopyProcessor {
    processed_count: AtomicU64,
    total_latency_ns: AtomicU64,
    buffer_pool: Vec<Vec<u8>>,
    pool_index: AtomicU64,
}

impl ZeroCopyProcessor {
    fn new(pool_size: usize, buffer_size: usize) -> Self {
        let mut buffer_pool = Vec::with_capacity(pool_size);
        for _ in 0..pool_size {
            buffer_pool.push(vec![0u8; buffer_size]);
        }

        Self {
            processed_count: AtomicU64::new(0),
            total_latency_ns: AtomicU64::new(0),
            buffer_pool,
            pool_index: AtomicU64::new(0),
        }
    }

    fn process_message_zero_copy(&self, data: &[u8], _timestamp: u64) {
        let start = Instant::now();
        
        // 零拷貝處理：直接使用引用
        let data_ref = data;
        
        // 零拷貝 JSON 解析（不分配字符串）
        self.parse_json_zero_copy(data_ref);
        
        // 模擬處理邏輯（零拷貝）
        let _checksum: u32 = data_ref.iter().map(|&b| b as u32).sum();
        
        let elapsed = start.elapsed().as_nanos() as u64;
        self.processed_count.fetch_add(1, Ordering::Relaxed);
        self.total_latency_ns.fetch_add(elapsed, Ordering::Relaxed);
    }

    fn parse_json_zero_copy(&self, data: &[u8]) {
        // 模擬零拷貝 JSON 解析（使用 simd-json 或類似庫）
        // 這裡只是簡單模擬，實際可以使用 simd-json 進行零拷貝解析
        
        // 查找關鍵字段（零拷貝）
        if let Some(_pos) = find_subsequence(data, b"books5") {
            // 找到了頻道信息
        }
        if let Some(_pos) = find_subsequence(data, b"BTCUSDT") {
            // 找到了交易對信息
        }
    }

    fn stats(&self) -> (u64, f64) {
        let count = self.processed_count.load(Ordering::Relaxed);
        let total_latency = self.total_latency_ns.load(Ordering::Relaxed);
        let avg_latency = if count > 0 { total_latency as f64 / count as f64 } else { 0.0 };
        (count, avg_latency)
    }
}

/// 批量零拷貝處理器
struct BatchZeroCopyProcessor {
    processed_count: AtomicU64,
    total_latency_ns: AtomicU64,
    batch_latency_ns: AtomicU64,
    batch_count: AtomicU64,
}

impl BatchZeroCopyProcessor {
    fn new() -> Self {
        Self {
            processed_count: AtomicU64::new(0),
            total_latency_ns: AtomicU64::new(0),
            batch_latency_ns: AtomicU64::new(0),
            batch_count: AtomicU64::new(0),
        }
    }

    fn process_batch(&self, messages: &[&MockMessage]) {
        let start = Instant::now();
        
        for message in messages {
            let msg_start = Instant::now();
            
            // 零拷貝批量處理
            let data_ref = &message.data;
            let _checksum: u32 = data_ref.iter().map(|&b| b as u32).sum();
            
            // 批量 JSON 解析
            if let Some(_pos) = find_subsequence(data_ref, b"books5") {
                // 處理 orderbook 數據
            }
            
            let msg_elapsed = msg_start.elapsed().as_nanos() as u64;
            self.total_latency_ns.fetch_add(msg_elapsed, Ordering::Relaxed);
            self.processed_count.fetch_add(1, Ordering::Relaxed);
        }
        
        let batch_elapsed = start.elapsed().as_nanos() as u64;
        self.batch_latency_ns.fetch_add(batch_elapsed, Ordering::Relaxed);
        self.batch_count.fetch_add(1, Ordering::Relaxed);
    }

    fn stats(&self) -> (u64, f64, f64) {
        let count = self.processed_count.load(Ordering::Relaxed);
        let total_latency = self.total_latency_ns.load(Ordering::Relaxed);
        let batch_count = self.batch_count.load(Ordering::Relaxed);
        let batch_latency = self.batch_latency_ns.load(Ordering::Relaxed);
        
        let avg_msg_latency = if count > 0 { total_latency as f64 / count as f64 } else { 0.0 };
        let avg_batch_latency = if batch_count > 0 { batch_latency as f64 / batch_count as f64 } else { 0.0 };
        
        (count, avg_msg_latency, avg_batch_latency)
    }
}

/// 輔助函數：查找子序列（零拷貝）
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    
    let args = Args::parse();
    
    info!("🚀 零拷貝 WebSocket 性能測試");
    info!("測試參數: {} 條消息, {} 字節/消息, 批量大小 {}, {} 線程", 
          args.messages, args.size, args.batch_size, args.threads);
    
    // 生成測試消息
    info!("📝 生成測試數據...");
    let mut messages = Vec::with_capacity(args.messages);
    for _ in 0..args.messages {
        messages.push(MockMessage::new(args.size));
    }
    info!("✅ 生成了 {} 條測試消息", messages.len());
    
    // 測試 1: 傳統消息處理
    info!("📊 測試傳統消息處理...");
    let traditional_processor = Arc::new(TraditionalProcessor::new());
    let start = Instant::now();
    
    for message in &messages {
        traditional_processor.process_message(message);
    }
    
    let traditional_elapsed = start.elapsed();
    let (traditional_count, traditional_avg_latency) = traditional_processor.stats();
    
    // 測試 2: 零拷貝單線程處理
    info!("📊 測試零拷貝單線程處理...");
    let zero_copy_processor = Arc::new(ZeroCopyProcessor::new(1024, args.size));
    let start = Instant::now();
    
    for message in &messages {
        zero_copy_processor.process_message_zero_copy(&message.data, message.timestamp);
    }
    
    let zero_copy_elapsed = start.elapsed();
    let (zero_copy_count, zero_copy_avg_latency) = zero_copy_processor.stats();
    
    // 測試 3: 批量零拷貝處理
    info!("📊 測試批量零拷貝處理...");
    let batch_processor = Arc::new(BatchZeroCopyProcessor::new());
    let start = Instant::now();
    
    for chunk in messages.chunks(args.batch_size) {
        let message_refs: Vec<&MockMessage> = chunk.iter().collect();
        batch_processor.process_batch(&message_refs);
    }
    
    let batch_elapsed = start.elapsed();
    let (batch_count, batch_avg_msg_latency, batch_avg_batch_latency) = batch_processor.stats();
    
    // 測試 4: 並發零拷貝處理
    info!("📊 測試並發零拷貝處理 ({} 線程)...", args.threads);
    let concurrent_processor = Arc::new(ZeroCopyProcessor::new(1024, args.size));
    let messages_arc = Arc::new(messages);
    let start = Instant::now();
    
    let handles: Vec<_> = (0..args.threads)
        .map(|thread_id| {
            let processor = concurrent_processor.clone();
            let messages = messages_arc.clone();
            let chunk_size = args.messages / args.threads;
            
            tokio::spawn(async move {
                let start_idx = thread_id * chunk_size;
                let end_idx = if thread_id == args.threads - 1 { 
                    args.messages 
                } else { 
                    (thread_id + 1) * chunk_size 
                };
                
                for message in &messages[start_idx..end_idx] {
                    processor.process_message_zero_copy(&message.data, message.timestamp);
                }
            })
        })
        .collect();
    
    for handle in handles {
        handle.await?;
    }
    
    let concurrent_elapsed = start.elapsed();
    let (concurrent_count, concurrent_avg_latency) = concurrent_processor.stats();
    
    // 結果分析
    info!("\n🎯 ===== 性能測試結果 =====");
    
    info!("🔸 傳統消息處理:");
    info!("   總時間: {:.2}ms", traditional_elapsed.as_millis());
    info!("   處理消息: {}", traditional_count);
    info!("   平均延遲: {:.2}ns ({:.3}μs)", traditional_avg_latency, traditional_avg_latency / 1000.0);
    info!("   吞吐量: {:.0} msgs/s", traditional_count as f64 / traditional_elapsed.as_secs_f64());
    
    info!("🔸 零拷貝單線程:");
    info!("   總時間: {:.2}ms", zero_copy_elapsed.as_millis());
    info!("   處理消息: {}", zero_copy_count);
    info!("   平均延遲: {:.2}ns ({:.3}μs)", zero_copy_avg_latency, zero_copy_avg_latency / 1000.0);
    info!("   吞吐量: {:.0} msgs/s", zero_copy_count as f64 / zero_copy_elapsed.as_secs_f64());
    
    info!("🔸 批量零拷貝:");
    info!("   總時間: {:.2}ms", batch_elapsed.as_millis());
    info!("   處理消息: {}", batch_count);
    info!("   平均消息延遲: {:.2}ns ({:.3}μs)", batch_avg_msg_latency, batch_avg_msg_latency / 1000.0);
    info!("   平均批量延遲: {:.2}ns ({:.3}μs)", batch_avg_batch_latency, batch_avg_batch_latency / 1000.0);
    info!("   吞吐量: {:.0} msgs/s", batch_count as f64 / batch_elapsed.as_secs_f64());
    
    info!("🔸 並發零拷貝 ({} 線程):", args.threads);
    info!("   總時間: {:.2}ms", concurrent_elapsed.as_millis());
    info!("   處理消息: {}", concurrent_count);
    info!("   平均延遲: {:.2}ns ({:.3}μs)", concurrent_avg_latency, concurrent_avg_latency / 1000.0);
    info!("   吞吐量: {:.0} msgs/s", concurrent_count as f64 / concurrent_elapsed.as_secs_f64());
    
    // 性能提升分析
    let zero_copy_speedup = traditional_avg_latency / zero_copy_avg_latency;
    let batch_speedup = traditional_avg_latency / batch_avg_msg_latency;
    let concurrent_speedup = traditional_avg_latency / concurrent_avg_latency;
    
    info!("\n🏆 ===== 性能提升分析 =====");
    info!("🚀 零拷貝 vs 傳統: {:.2}x 提升", zero_copy_speedup);
    info!("🚀 批量零拷貝 vs 傳統: {:.2}x 提升", batch_speedup);
    info!("🚀 並發零拷貝 vs 傳統: {:.2}x 提升", concurrent_speedup);
    
    // 目標達成評估
    let zero_copy_target_met = zero_copy_avg_latency < 100.0;
    let batch_target_met = batch_avg_msg_latency < 100.0;
    let concurrent_target_met = concurrent_avg_latency < 100.0;
    
    info!("\n🎯 ===== 目標達成評估 (< 100ns) =====");
    info!("零拷貝單線程: {:.2}ns - {}", zero_copy_avg_latency, 
          if zero_copy_target_met { "✅ 達成" } else { "❌ 未達成" });
    info!("批量零拷貝: {:.2}ns - {}", batch_avg_msg_latency,
          if batch_target_met { "✅ 達成" } else { "❌ 未達成" });
    info!("並發零拷貝: {:.2}ns - {}", concurrent_avg_latency,
          if concurrent_target_met { "✅ 達成" } else { "❌ 未達成" });
    
    if zero_copy_target_met || batch_target_met || concurrent_target_met {
        info!("🎉 恭喜！零拷貝實現已達到 < 100ns 的超低延遲目標！");
    } else {
        info!("⚠️  建議進一步優化：");
        info!("   • 使用 SIMD 指令集進行並行處理");
        info!("   • 實施內存預取策略");
        info!("   • 優化 CPU 緩存對齊");
        info!("   • 使用專用硬件加速");
    }
    
    if args.verbose {
        info!("\n📈 ===== 詳細統計 =====");
        info!("測試配置:");
        info!("   消息數量: {}", args.messages);
        info!("   消息大小: {} 字節", args.size);
        info!("   批量大小: {}", args.batch_size);
        info!("   線程數: {}", args.threads);
        
        let memory_usage = args.messages * args.size;
        info!("   總內存使用: {:.2} MB", memory_usage as f64 / 1024.0 / 1024.0);
        
        info!("吞吐量對比:");
        let traditional_throughput = traditional_count as f64 / traditional_elapsed.as_secs_f64();
        let zero_copy_throughput = zero_copy_count as f64 / zero_copy_elapsed.as_secs_f64();
        let batch_throughput = batch_count as f64 / batch_elapsed.as_secs_f64();
        let concurrent_throughput = concurrent_count as f64 / concurrent_elapsed.as_secs_f64();
        
        info!("   傳統: {:.0} msgs/s", traditional_throughput);
        info!("   零拷貝: {:.0} msgs/s", zero_copy_throughput);
        info!("   批量: {:.0} msgs/s", batch_throughput);
        info!("   並發: {:.0} msgs/s", concurrent_throughput);
    }
    
    info!("✅ 測試完成");
    
    Ok(())
}