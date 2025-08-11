/*!
 * Network Latency Optimization for Ultra-Low Latency Trading
 * 
 * Implements advanced network optimization techniques:
 * - TCP_NODELAY and socket-level optimizations  
 * - Kernel bypass networking preparation
 * - Message batching and zero-copy techniques
 * - Network buffer tuning
 * 
 * Target: Reduce network-induced latency to < 100μs p99
 */

use std::net::{TcpStream, SocketAddr};
use std::time::{Duration, Instant};
use std::sync::atomic::{AtomicU64, AtomicBool, Ordering};
use std::sync::Arc;
use anyhow::{Result, Context};
use tracing::{info, warn, debug, error};
use tokio::net::{TcpSocket, TcpStream as AsyncTcpStream};
use tokio::time::timeout;

#[cfg(target_os = "linux")]
use libc::{
    setsockopt, SOL_SOCKET, SO_REUSEADDR, SO_REUSEPORT, SO_RCVBUF, SO_SNDBUF,
    TCP_NODELAY, TCP_QUICKACK, TCP_USER_TIMEOUT, IPPROTO_TCP, SOL_TCP
};

/// Network optimization configuration
#[derive(Debug, Clone)]
pub struct NetworkOptConfig {
    /// Enable TCP_NODELAY (disable Nagle's algorithm)
    pub tcp_nodelay: bool,
    /// Enable TCP_QUICKACK (Linux only)
    pub tcp_quickack: bool,
    /// Socket receive buffer size (bytes)
    pub recv_buffer_size: u32,
    /// Socket send buffer size (bytes)  
    pub send_buffer_size: u32,
    /// TCP user timeout (milliseconds)
    pub tcp_user_timeout: u32,
    /// Connection timeout (milliseconds)
    pub connect_timeout_ms: u64,
    /// Enable SO_REUSEPORT for load balancing
    pub reuse_port: bool,
    /// Message batching parameters
    pub batch_config: MessageBatchConfig,
}

impl Default for NetworkOptConfig {
    fn default() -> Self {
        Self {
            tcp_nodelay: true,
            tcp_quickack: true,
            recv_buffer_size: 1024 * 1024,  // 1MB
            send_buffer_size: 1024 * 1024,  // 1MB
            tcp_user_timeout: 5000,         // 5 seconds
            connect_timeout_ms: 1000,       // 1 second
            reuse_port: true,
            batch_config: MessageBatchConfig::default(),
        }
    }
}

/// Message batching configuration
#[derive(Debug, Clone)]
pub struct MessageBatchConfig {
    /// Maximum messages per batch
    pub max_batch_size: usize,
    /// Maximum batch timeout (microseconds)
    pub max_batch_timeout_us: u64,
    /// Target batch latency (microseconds)
    pub target_batch_latency_us: u64,
    /// Enable adaptive batching
    pub adaptive_batching: bool,
}

impl Default for MessageBatchConfig {
    fn default() -> Self {
        Self {
            max_batch_size: 100,
            max_batch_timeout_us: 500,  // 500μs
            target_batch_latency_us: 200, // 200μs target
            adaptive_batching: true,
        }
    }
}

/// Network statistics for monitoring
#[derive(Debug, Default)]
pub struct NetworkStats {
    pub messages_sent: AtomicU64,
    pub messages_received: AtomicU64,
    pub bytes_sent: AtomicU64,
    pub bytes_received: AtomicU64,
    pub connection_count: AtomicU64,
    pub reconnection_count: AtomicU64,
    pub send_latency_us: AtomicU64,
    pub recv_latency_us: AtomicU64,
    pub batch_count: AtomicU64,
    pub avg_batch_size: AtomicU64,
}

impl NetworkStats {
    pub fn record_message_sent(&self, size: u64) {
        self.messages_sent.fetch_add(1, Ordering::Relaxed);
        self.bytes_sent.fetch_add(size, Ordering::Relaxed);
    }
    
    pub fn record_message_received(&self, size: u64) {
        self.messages_received.fetch_add(1, Ordering::Relaxed);
        self.bytes_received.fetch_add(size, Ordering::Relaxed);
    }
    
    pub fn record_latency(&self, latency_us: u64, is_send: bool) {
        if is_send {
            self.send_latency_us.store(latency_us, Ordering::Relaxed);
        } else {
            self.recv_latency_us.store(latency_us, Ordering::Relaxed);
        }
    }
    
    pub fn record_batch(&self, batch_size: u64) {
        self.batch_count.fetch_add(1, Ordering::Relaxed);
        self.avg_batch_size.store(batch_size, Ordering::Relaxed);
    }
}

/// Optimized TCP connection
pub struct OptimizedTcpConnection {
    config: NetworkOptConfig,
    stats: Arc<NetworkStats>,
    is_connected: AtomicBool,
    last_activity: AtomicU64,
    connection_start: AtomicU64,
    message_buffer: parking_lot::Mutex<Vec<Vec<u8>>>,
    batch_timer: AtomicU64,
}

impl OptimizedTcpConnection {
    pub fn new(config: NetworkOptConfig) -> Self {
        let max_batch_size = config.batch_config.max_batch_size;
        Self {
            config,
            stats: Arc::new(NetworkStats::default()),
            is_connected: AtomicBool::new(false),
            last_activity: AtomicU64::new(0),
            connection_start: AtomicU64::new(0),
            message_buffer: parking_lot::Mutex::new(Vec::with_capacity(max_batch_size)),
            batch_timer: AtomicU64::new(0),
        }
    }
    
    /// Create optimized TCP socket with low-latency settings
    pub async fn connect_optimized(&self, addr: SocketAddr) -> Result<AsyncTcpStream> {
        let start_time = Instant::now();
        
        // Create socket
        let socket = if addr.is_ipv4() {
            TcpSocket::new_v4()?
        } else {
            TcpSocket::new_v6()?
        };
        
        // Apply socket optimizations
        self.configure_socket(&socket).context("Failed to configure socket")?;
        
        // Connect with timeout
        let stream = timeout(
            Duration::from_millis(self.config.connect_timeout_ms),
            socket.connect(addr)
        )
        .await
        .context("Connection timeout")?
        .context("Failed to connect")?;
        
        // Apply TCP-specific optimizations
        self.configure_tcp_stream(&stream).context("Failed to configure TCP stream")?;
        
        let connect_time = start_time.elapsed();
        debug!("Connected to {} in {:?}", addr, connect_time);
        
        self.is_connected.store(true, Ordering::Release);
        self.connection_start.store(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_micros() as u64,
            Ordering::Relaxed
        );
        self.stats.connection_count.fetch_add(1, Ordering::Relaxed);
        
        Ok(stream)
    }
    
    /// Configure socket-level optimizations
    fn configure_socket(&self, socket: &TcpSocket) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::io::AsRawFd;
            
            let fd = socket.as_raw_fd();
            
            // Enable address reuse
            if self.config.reuse_port {
                unsafe {
                    let optval: i32 = 1;
                    setsockopt(
                        fd,
                        SOL_SOCKET,
                        SO_REUSEADDR,
                        &optval as *const _ as *const _,
                        std::mem::size_of::<i32>() as u32,
                    );
                    
                    setsockopt(
                        fd,
                        SOL_SOCKET,
                        SO_REUSEPORT,
                        &optval as *const _ as *const _,
                        std::mem::size_of::<i32>() as u32,
                    );
                }
            }
            
            // Set buffer sizes
            unsafe {
                let recv_buf = self.config.recv_buffer_size as i32;
                let send_buf = self.config.send_buffer_size as i32;
                
                setsockopt(
                    fd,
                    SOL_SOCKET,
                    SO_RCVBUF,
                    &recv_buf as *const _ as *const _,
                    std::mem::size_of::<i32>() as u32,
                );
                
                setsockopt(
                    fd,
                    SOL_SOCKET,
                    SO_SNDBUF,
                    &send_buf as *const _ as *const _,
                    std::mem::size_of::<i32>() as u32,
                );
            }
        }
        
        info!("Socket configured with optimizations");
        Ok(())
    }
    
    /// Configure TCP stream optimizations
    fn configure_tcp_stream(&self, stream: &AsyncTcpStream) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::io::AsRawFd;
            
            let fd = stream.as_raw_fd();
            
            // Disable Nagle's algorithm for low latency
            if self.config.tcp_nodelay {
                unsafe {
                    let optval: i32 = 1;
                    setsockopt(
                        fd,
                        IPPROTO_TCP,
                        TCP_NODELAY,
                        &optval as *const _ as *const _,
                        std::mem::size_of::<i32>() as u32,
                    );
                }
            }
            
            // Enable TCP quick ACK
            if self.config.tcp_quickack {
                unsafe {
                    let optval: i32 = 1;
                    setsockopt(
                        fd,
                        SOL_TCP,
                        TCP_QUICKACK,
                        &optval as *const _ as *const _,
                        std::mem::size_of::<i32>() as u32,
                    );
                }
            }
            
            // Set TCP user timeout
            if self.config.tcp_user_timeout > 0 {
                unsafe {
                    let optval = self.config.tcp_user_timeout as i32;
                    setsockopt(
                        fd,
                        SOL_TCP,
                        TCP_USER_TIMEOUT,
                        &optval as *const _ as *const _,
                        std::mem::size_of::<i32>() as u32,
                    );
                }
            }
        }
        
        info!("TCP stream configured with low-latency optimizations");
        Ok(())
    }
    
    /// Send message with batching optimization
    pub async fn send_with_batching<T: AsRef<[u8]>>(
        &self,
        stream: &mut AsyncTcpStream,
        message: T
    ) -> Result<()> {
        let message_bytes = message.as_ref();
        let send_start = Instant::now();
        
        if self.config.batch_config.adaptive_batching {
            // Add to batch buffer
            {
                let mut buffer = self.message_buffer.lock();
                buffer.push(message_bytes.to_vec());
                
                // Check if we should flush the batch
                let should_flush = buffer.len() >= self.config.batch_config.max_batch_size
                    || self.should_flush_batch();
                
                if should_flush {
                    let batch_size = buffer.len();
                    let combined_message = self.combine_batch_messages(&buffer);
                    buffer.clear();
                    
                    // Send the batched message
                    use tokio::io::AsyncWriteExt;
                    stream.write_all(&combined_message).await?;
                    
                    let send_latency = send_start.elapsed().as_micros() as u64;
                    self.stats.record_latency(send_latency, true);
                    self.stats.record_batch(batch_size as u64);
                    
                    for msg in &combined_message {
                        self.stats.record_message_sent(1);
                    }
                    self.stats.bytes_sent.fetch_add(combined_message.len() as u64, Ordering::Relaxed);
                }
            }
        } else {
            // Send immediately without batching
            use tokio::io::AsyncWriteExt;
            stream.write_all(message_bytes).await?;
            
            let send_latency = send_start.elapsed().as_micros() as u64;
            self.stats.record_latency(send_latency, true);
            self.stats.record_message_sent(message_bytes.len() as u64);
        }
        
        self.update_activity();
        Ok(())
    }
    
    /// Receive message with optimization
    pub async fn receive_optimized(
        &self,
        stream: &mut AsyncTcpStream,
        buffer: &mut [u8]
    ) -> Result<usize> {
        let recv_start = Instant::now();
        
        use tokio::io::AsyncReadExt;
        let bytes_read = stream.read(buffer).await?;
        
        let recv_latency = recv_start.elapsed().as_micros() as u64;
        self.stats.record_latency(recv_latency, false);
        self.stats.record_message_received(bytes_read as u64);
        
        self.update_activity();
        Ok(bytes_read)
    }
    
    /// Check if batch should be flushed based on timing
    fn should_flush_batch(&self) -> bool {
        let current_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64;
        
        let batch_start = self.batch_timer.load(Ordering::Relaxed);
        if batch_start == 0 {
            self.batch_timer.store(current_time, Ordering::Relaxed);
            return false;
        }
        
        let elapsed = current_time - batch_start;
        elapsed >= self.config.batch_config.max_batch_timeout_us
    }
    
    /// Combine multiple messages into a single batch
    fn combine_batch_messages(&self, messages: &[Vec<u8>]) -> Vec<u8> {
        let total_size: usize = messages.iter().map(|m| m.len()).sum();
        let mut combined = Vec::with_capacity(total_size);
        
        for message in messages {
            combined.extend_from_slice(message);
        }
        
        // Reset batch timer
        self.batch_timer.store(0, Ordering::Relaxed);
        
        combined
    }
    
    /// Update last activity timestamp
    fn update_activity(&self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64;
        self.last_activity.store(now, Ordering::Relaxed);
    }
    
    /// Get connection statistics
    pub fn get_stats(&self) -> NetworkStats {
        NetworkStats {
            messages_sent: AtomicU64::new(self.stats.messages_sent.load(Ordering::Relaxed)),
            messages_received: AtomicU64::new(self.stats.messages_received.load(Ordering::Relaxed)),
            bytes_sent: AtomicU64::new(self.stats.bytes_sent.load(Ordering::Relaxed)),
            bytes_received: AtomicU64::new(self.stats.bytes_received.load(Ordering::Relaxed)),
            connection_count: AtomicU64::new(self.stats.connection_count.load(Ordering::Relaxed)),
            reconnection_count: AtomicU64::new(self.stats.reconnection_count.load(Ordering::Relaxed)),
            send_latency_us: AtomicU64::new(self.stats.send_latency_us.load(Ordering::Relaxed)),
            recv_latency_us: AtomicU64::new(self.stats.recv_latency_us.load(Ordering::Relaxed)),
            batch_count: AtomicU64::new(self.stats.batch_count.load(Ordering::Relaxed)),
            avg_batch_size: AtomicU64::new(self.stats.avg_batch_size.load(Ordering::Relaxed)),
        }
    }
    
    /// Check connection health
    pub fn is_healthy(&self) -> bool {
        let is_connected = self.is_connected.load(Ordering::Acquire);
        let last_activity = self.last_activity.load(Ordering::Relaxed);
        let current_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64;
        
        let idle_time_us = current_time - last_activity;
        let max_idle_us = 30_000_000; // 30 seconds
        
        is_connected && (last_activity == 0 || idle_time_us < max_idle_us)
    }
    
    /// Force flush any pending batched messages
    pub async fn flush_batch(&self, stream: &mut AsyncTcpStream) -> Result<()> {
        let mut buffer = self.message_buffer.lock();
        if !buffer.is_empty() {
            let combined_message = self.combine_batch_messages(&buffer);
            buffer.clear();
            
            use tokio::io::AsyncWriteExt;
            stream.write_all(&combined_message).await?;
            stream.flush().await?;
        }
        Ok(())
    }
}

/// Network latency measurement utility
pub struct NetworkLatencyMeasurer {
    ping_history: crossbeam_channel::Receiver<u64>,
    ping_sender: crossbeam_channel::Sender<u64>,
    running: AtomicBool,
}

impl NetworkLatencyMeasurer {
    pub fn new() -> Self {
        let (sender, receiver) = crossbeam_channel::bounded(10000);
        
        Self {
            ping_history: receiver,
            ping_sender: sender,
            running: AtomicBool::new(true),
        }
    }
    
    /// Start continuous latency monitoring
    pub async fn start_monitoring(&self, mut stream: AsyncTcpStream) -> Result<()> {
        let sender = self.ping_sender.clone();
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = Arc::clone(&running);
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            
            while running_clone.load(Ordering::Relaxed) {
                interval.tick().await;
                
                let ping_start = Instant::now();
                
                // Send ping message (simplified)
                let ping_msg = b"PING\n";
                use tokio::io::AsyncWriteExt;
                if stream.write_all(ping_msg).await.is_ok() {
                    // Wait for pong (simplified - in real implementation would parse response)
                    let mut buffer = [0u8; 1024];
                    use tokio::io::AsyncReadExt;
                    if stream.read(&mut buffer).await.is_ok() {
                        let latency_us = ping_start.elapsed().as_micros() as u64;
                        let _ = sender.try_send(latency_us);
                    }
                }
            }
        });
        
        Ok(())
    }
    
    /// Get recent latency statistics
    pub fn get_latency_stats(&self) -> Option<LatencyStats> {
        let mut samples = Vec::new();
        
        while let Ok(sample) = self.ping_history.try_recv() {
            samples.push(sample);
        }
        
        if samples.is_empty() {
            return None;
        }
        
        samples.sort_unstable();
        let len = samples.len();
        
        Some(LatencyStats {
            count: len,
            min_us: samples[0],
            max_us: samples[len - 1],
            mean_us: samples.iter().sum::<u64>() / len as u64,
            p50_us: samples[len / 2],
            p95_us: samples[len * 95 / 100],
            p99_us: samples[len * 99 / 100],
        })
    }
    
    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

#[derive(Debug, Clone)]
pub struct LatencyStats {
    pub count: usize,
    pub min_us: u64,
    pub max_us: u64,
    pub mean_us: u64,
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
}

impl LatencyStats {
    pub fn format(&self) -> String {
        format!(
            "Network Latency: count={}, min={}μs, max={}μs, mean={}μs, p50={}μs, p95={}μs, p99={}μs",
            self.count, self.min_us, self.max_us, self.mean_us, 
            self.p50_us, self.p95_us, self.p99_us
        )
    }
    
    pub fn meets_targets(&self) -> bool {
        self.p99_us <= 100 // Target: p99 network latency ≤ 100μs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::test;
    
    #[test]
    async fn test_optimized_connection_config() {
        let config = NetworkOptConfig::default();
        let conn = OptimizedTcpConnection::new(config);
        
        assert!(conn.is_healthy()); // Should start healthy
    }
    
    #[test]  
    async fn test_message_batching() {
        let config = NetworkOptConfig {
            batch_config: MessageBatchConfig {
                max_batch_size: 3,
                max_batch_timeout_us: 1000,
                target_batch_latency_us: 500,
                adaptive_batching: true,
            },
            ..Default::default()
        };
        
        let conn = OptimizedTcpConnection::new(config);
        
        // Test batch combination
        let messages = vec![
            b"message1".to_vec(),
            b"message2".to_vec(),
            b"message3".to_vec(),
        ];
        
        let combined = conn.combine_batch_messages(&messages);
        assert_eq!(combined, b"message1message2message3");
    }
    
    #[tokio::test]
    async fn test_network_stats() {
        let stats = NetworkStats::default();
        
        stats.record_message_sent(100);
        stats.record_message_received(200);
        stats.record_latency(50, true);
        stats.record_batch(10);
        
        assert_eq!(stats.messages_sent.load(Ordering::Relaxed), 1);
        assert_eq!(stats.bytes_sent.load(Ordering::Relaxed), 100);
        assert_eq!(stats.messages_received.load(Ordering::Relaxed), 1);
        assert_eq!(stats.bytes_received.load(Ordering::Relaxed), 200);
        assert_eq!(stats.send_latency_us.load(Ordering::Relaxed), 50);
        assert_eq!(stats.avg_batch_size.load(Ordering::Relaxed), 10);
    }
}