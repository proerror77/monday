/*!
 * 零拷貝消息處理單元測試
 * 
 * 提取自 simple_zero_copy_test，專注於零拷貝處理功能測試
 */

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// 模擬消息結構
#[derive(Debug, Clone)]
pub struct TestMessage {
    pub data: Vec<u8>,
    pub timestamp: u64,
}

impl TestMessage {
    pub fn new(size: usize) -> Self {
        let json_template = r#"{"channel":"books5","instId":"BTCUSDT","data":[{"asks":[["50000.1","0.5"]],"bids":[["49999.9","1.5"]],"ts":"1234567890","checksum":123456}]}"#;
        let template_bytes = json_template.as_bytes();
        
        let mut data = vec![0u8; size];
        let repeat_count = (size + template_bytes.len() - 1) / template_bytes.len();
        
        for i in 0..repeat_count {
            let start = i * template_bytes.len();
            let end = std::cmp::min(start + template_bytes.len(), size);
            if start < size {
                let copy_len = end - start;
                data[start..end].copy_from_slice(&template_bytes[..copy_len]);
            }
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

/// 傳統處理器（帶內存分配）
#[derive(Debug)]
pub struct TraditionalProcessor {
    pub processed: AtomicU64,
    pub total_latency_ns: AtomicU64,
}

impl TraditionalProcessor {
    pub fn new() -> Self {
        Self {
            processed: AtomicU64::new(0),
            total_latency_ns: AtomicU64::new(0),
        }
    }
    
    pub fn process(&self, message: &TestMessage) -> u64 {
        let start = Instant::now();
        
        // 模擬傳統處理：克隆數據（分配內存）
        let _owned_data = message.data.clone();
        
        // 模擬 JSON 解析（會分配字符串）
        let json_str = String::from_utf8_lossy(&message.data);
        let _parsed = json_str.contains("books5");
        
        // 模擬處理邏輯
        let _sum: u32 = message.data.iter().map(|&b| b as u32).sum();
        
        let elapsed = start.elapsed().as_nanos() as u64;
        self.processed.fetch_add(1, Ordering::Relaxed);
        self.total_latency_ns.fetch_add(elapsed, Ordering::Relaxed);
        
        elapsed
    }
    
    pub fn stats(&self) -> (u64, f64) {
        let count = self.processed.load(Ordering::Relaxed);
        let total = self.total_latency_ns.load(Ordering::Relaxed);
        (count, if count > 0 { total as f64 / count as f64 } else { 0.0 })
    }
    
    pub fn reset(&self) {
        self.processed.store(0, Ordering::Relaxed);
        self.total_latency_ns.store(0, Ordering::Relaxed);
    }
}

/// 零拷貝處理器
#[derive(Debug)]
pub struct ZeroCopyProcessor {
    pub processed: AtomicU64,
    pub total_latency_ns: AtomicU64,
}

impl ZeroCopyProcessor {
    pub fn new() -> Self {
        Self {
            processed: AtomicU64::new(0),
            total_latency_ns: AtomicU64::new(0),
        }
    }
    
    pub fn process_zero_copy(&self, data: &[u8], _timestamp: u64) -> u64 {
        let start = Instant::now();
        
        // 零拷貝處理：直接使用切片引用
        let data_slice = data;
        
        // 零拷貝 JSON 檢查（不分配字符串）
        let _has_books5 = self.contains_pattern(data_slice, b"books5");
        let _has_btcusdt = self.contains_pattern(data_slice, b"BTCUSDT");
        
        // 模擬處理（零拷貝）
        let _sum: u32 = data_slice.iter().map(|&b| b as u32).sum();
        
        let elapsed = start.elapsed().as_nanos() as u64;
        self.processed.fetch_add(1, Ordering::Relaxed);
        self.total_latency_ns.fetch_add(elapsed, Ordering::Relaxed);
        
        elapsed
    }
    
    /// 零拷貝模式匹配
    pub fn contains_pattern(&self, haystack: &[u8], needle: &[u8]) -> bool {
        if needle.is_empty() {
            return true;
        }
        if haystack.len() < needle.len() {
            return false;
        }
        
        haystack.windows(needle.len()).any(|window| window == needle)
    }
    
    pub fn stats(&self) -> (u64, f64) {
        let count = self.processed.load(Ordering::Relaxed);
        let total = self.total_latency_ns.load(Ordering::Relaxed);
        (count, if count > 0 { total as f64 / count as f64 } else { 0.0 })
    }
    
    pub fn reset(&self) {
        self.processed.store(0, Ordering::Relaxed);
        self.total_latency_ns.store(0, Ordering::Relaxed);
    }
}

/// 批量零拷貝處理器
#[derive(Debug)]
pub struct BatchZeroCopyProcessor {
    pub processed: AtomicU64,
    pub total_latency_ns: AtomicU64,
    pub batch_count: AtomicU64,
    pub batch_latency_ns: AtomicU64,
}

impl BatchZeroCopyProcessor {
    pub fn new() -> Self {
        Self {
            processed: AtomicU64::new(0),
            total_latency_ns: AtomicU64::new(0),
            batch_count: AtomicU64::new(0),
            batch_latency_ns: AtomicU64::new(0),
        }
    }
    
    pub fn process_batch(&self, messages: &[&TestMessage]) -> u64 {
        let batch_start = Instant::now();
        
        for message in messages {
            let msg_start = Instant::now();
            
            // 零拷貝批量處理
            let data_ref = &message.data;
            let _checksum: u32 = data_ref.iter().map(|&b| b as u32).sum();
            
            // 批量 JSON 解析（零拷貝）
            let _has_books5 = data_ref.windows(6).any(|w| w == b"books5");
            
            let msg_elapsed = msg_start.elapsed().as_nanos() as u64;
            self.total_latency_ns.fetch_add(msg_elapsed, Ordering::Relaxed);
            self.processed.fetch_add(1, Ordering::Relaxed);
        }
        
        let batch_elapsed = batch_start.elapsed().as_nanos() as u64;
        self.batch_latency_ns.fetch_add(batch_elapsed, Ordering::Relaxed);
        self.batch_count.fetch_add(1, Ordering::Relaxed);
        
        batch_elapsed
    }
    
    pub fn stats(&self) -> (u64, f64, f64) {
        let count = self.processed.load(Ordering::Relaxed);
        let total_latency = self.total_latency_ns.load(Ordering::Relaxed);
        let batch_count = self.batch_count.load(Ordering::Relaxed);
        let batch_latency = self.batch_latency_ns.load(Ordering::Relaxed);
        
        let avg_msg_latency = if count > 0 { total_latency as f64 / count as f64 } else { 0.0 };
        let avg_batch_latency = if batch_count > 0 { batch_latency as f64 / batch_count as f64 } else { 0.0 };
        
        (count, avg_msg_latency, avg_batch_latency)
    }
}

/// SIMD 優化處理器（模擬）
#[derive(Debug)]
pub struct SimdProcessor {
    pub processed: AtomicU64,
    pub total_latency_ns: AtomicU64,
}

impl SimdProcessor {
    pub fn new() -> Self {
        Self {
            processed: AtomicU64::new(0),
            total_latency_ns: AtomicU64::new(0),
        }
    }
    
    pub fn process_simd(&self, data: &[u8], _timestamp: u64) -> u64 {
        let start = Instant::now();
        
        // 模擬 SIMD 向量化處理
        // 在實際實現中，這裡會使用 SIMD 指令
        let chunk_size = 8; // 模擬 8-byte SIMD 處理
        let mut sum = 0u64;
        
        for chunk in data.chunks_exact(chunk_size) {
            // 模擬 SIMD 加法
            let chunk_sum: u64 = chunk.iter().map(|&b| b as u64).sum();
            sum = sum.wrapping_add(chunk_sum);
        }
        
        // 處理剩餘字節
        for &byte in data.chunks_exact(chunk_size).remainder() {
            sum = sum.wrapping_add(byte as u64);
        }
        
        // 模擬 SIMD 模式搜索
        let _has_pattern = self.simd_search(data, b"books5");
        
        let elapsed = start.elapsed().as_nanos() as u64;
        self.processed.fetch_add(1, Ordering::Relaxed);
        self.total_latency_ns.fetch_add(elapsed, Ordering::Relaxed);
        
        elapsed
    }
    
    fn simd_search(&self, haystack: &[u8], needle: &[u8]) -> bool {
        // 模擬 SIMD 字符串搜索
        // 實際實現可以使用 memchr 或 SIMD 指令
        if needle.is_empty() {
            return true;
        }
        if haystack.len() < needle.len() {
            return false;
        }
        
        haystack.windows(needle.len()).any(|window| window == needle)
    }
    
    pub fn stats(&self) -> (u64, f64) {
        let count = self.processed.load(Ordering::Relaxed);
        let total = self.total_latency_ns.load(Ordering::Relaxed);
        (count, if count > 0 { total as f64 / count as f64 } else { 0.0 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::PerformanceHelper;
    
    #[test]
    fn test_message_creation() {
        let message = TestMessage::new(1024);
        assert_eq!(message.data.len(), 1024);
        assert!(message.timestamp > 0);
        
        // 驗證消息包含預期內容
        let data_str = String::from_utf8_lossy(&message.data);
        assert!(data_str.contains("books5"));
        assert!(data_str.contains("BTCUSDT"));
    }
    
    #[test]
    fn test_zero_copy_pattern_matching() {
        let processor = ZeroCopyProcessor::new();
        
        let test_data = b"Hello books5 world BTCUSDT test";
        assert!(processor.contains_pattern(test_data, b"books5"));
        assert!(processor.contains_pattern(test_data, b"BTCUSDT"));
        assert!(!processor.contains_pattern(test_data, b"nonexistent"));
        
        // 邊界情況測試
        assert!(processor.contains_pattern(test_data, b""));
        assert!(processor.contains_pattern(b"", b""));
        assert!(!processor.contains_pattern(b"", b"test"));
    }
    
    #[test]
    fn test_traditional_vs_zero_copy_processing() {
        let message = TestMessage::new(512);
        
        let traditional = TraditionalProcessor::new();
        let zero_copy = ZeroCopyProcessor::new();
        
        // 單次處理測試
        let traditional_latency = traditional.process(&message);
        let zero_copy_latency = zero_copy.process_zero_copy(&message.data, message.timestamp);
        
        println!("Traditional: {}ns, Zero-copy: {}ns", traditional_latency, zero_copy_latency);
        
        let (traditional_count, traditional_avg) = traditional.stats();
        let (zero_copy_count, zero_copy_avg) = zero_copy.stats();
        
        assert_eq!(traditional_count, 1);
        assert_eq!(zero_copy_count, 1);
        assert_eq!(traditional_avg, traditional_latency as f64);
        assert_eq!(zero_copy_avg, zero_copy_latency as f64);
    }
    
    #[test]
    fn test_batch_processing() {
        let messages: Vec<_> = (0..10).map(|_| TestMessage::new(256)).collect();
        let message_refs: Vec<_> = messages.iter().collect();
        
        let batch_processor = BatchZeroCopyProcessor::new();
        let batch_latency = batch_processor.process_batch(&message_refs);
        
        let (count, avg_msg_latency, avg_batch_latency) = batch_processor.stats();
        
        assert_eq!(count, 10);
        assert!(avg_msg_latency > 0.0);
        assert!(avg_batch_latency > 0.0);
        assert_eq!(avg_batch_latency, batch_latency as f64);
        
        println!("Batch processing: {} messages, {:.2}ns/msg, {:.2}ns/batch", 
                 count, avg_msg_latency, avg_batch_latency);
    }
    
    #[test]
    fn test_simd_processing() {
        let message = TestMessage::new(1024);
        let simd_processor = SimdProcessor::new();
        
        let simd_latency = simd_processor.process_simd(&message.data, message.timestamp);
        let (count, avg_latency) = simd_processor.stats();
        
        assert_eq!(count, 1);
        assert_eq!(avg_latency, simd_latency as f64);
        assert!(simd_latency > 0);
        
        println!("SIMD processing: {}ns", simd_latency);
    }
    
    #[test]
    fn test_performance_comparison_multiple_messages() {
        let messages: Vec<_> = (0..1000).map(|_| TestMessage::new(512)).collect();
        
        let traditional = TraditionalProcessor::new();
        let zero_copy = ZeroCopyProcessor::new();
        let simd = SimdProcessor::new();
        
        // 傳統處理
        for message in &messages {
            traditional.process(message);
        }
        
        // 零拷貝處理
        for message in &messages {
            zero_copy.process_zero_copy(&message.data, message.timestamp);
        }
        
        // SIMD 處理
        for message in &messages {
            simd.process_simd(&message.data, message.timestamp);
        }
        
        let (traditional_count, traditional_avg) = traditional.stats();
        let (zero_copy_count, zero_copy_avg) = zero_copy.stats();
        let (simd_count, simd_avg) = simd.stats();
        
        println!("Performance comparison (1000 messages):");
        println!("Traditional: {:.2}ns avg", traditional_avg);
        println!("Zero-copy: {:.2}ns avg", zero_copy_avg);
        println!("SIMD: {:.2}ns avg", simd_avg);
        
        assert_eq!(traditional_count, 1000);
        assert_eq!(zero_copy_count, 1000);
        assert_eq!(simd_count, 1000);
        
        // 一般情況下，零拷貝應該比傳統方式更快
        // 注意：在 debug 模式下可能看不到明顯差異
        let speedup = traditional_avg / zero_copy_avg;
        println!("Zero-copy speedup: {:.2}x", speedup);
    }
    
    #[tokio::test]
    async fn test_concurrent_zero_copy_processing() {
        use std::sync::Arc;
        
        let messages: Vec<_> = (0..1000).map(|_| TestMessage::new(256)).collect();
        let messages = Arc::new(messages);
        let processor = Arc::new(ZeroCopyProcessor::new());
        
        let handles: Vec<_> = (0..4)
            .map(|thread_id| {
                let msgs = messages.clone();
                let proc = processor.clone();
                let chunk_size = 250;
                
                tokio::spawn(async move {
                    let start_idx = thread_id * chunk_size;
                    let end_idx = (thread_id + 1) * chunk_size;
                    
                    for message in &msgs[start_idx..end_idx] {
                        proc.process_zero_copy(&message.data, message.timestamp);
                    }
                })
            })
            .collect();
        
        for handle in handles {
            handle.await.unwrap();
        }
        
        let (count, avg_latency) = processor.stats();
        assert_eq!(count, 1000);
        
        println!("Concurrent zero-copy processing: {} messages, {:.2}ns avg", 
                 count, avg_latency);
    }
}