use async_trait::async_trait;
use futures::StreamExt;
use ports::{BoxStream, MarketEvent, MarketStream, ConnectionHealth, AggregatedBar};
use hft_core::{HftResult, HftError, Symbol, Price, Quantity, Timestamp};
use tokio::time::{interval, Duration, Instant};
use tokio_stream::wrappers::IntervalStream;
use tracing::info;

/// 簡單的 mock 行情：定期產生 Bar 與 Trade 事件
pub struct MockMarketStream {
    pub interval_ms: u64,
}

impl Default for MockMarketStream { fn default() -> Self { Self { interval_ms: 200 } } }

impl MockMarketStream { 
    pub fn new() -> Self { Self::default() } 
    
    pub fn with_interval_ms(interval_ms: u64) -> Self {
        Self { interval_ms }
    }
}

#[async_trait]
impl MarketStream for MockMarketStream {
    async fn subscribe(&self, symbols: Vec<Symbol>) -> HftResult<BoxStream<MarketEvent>> {
        info!("MockMarketStream 訂閱 symbols={:?}, 間隔: {}ms", symbols, self.interval_ms);
        let t0 = Instant::now();
        let interval = interval(Duration::from_millis(self.interval_ms));
        let mut counter = 0u64;
        
        let stream = IntervalStream::new(interval).map(move |_| {
            counter += 1;
            let ts = current_ts();
            let sym = symbols.get(0).cloned().unwrap_or(Symbol("BTCUSDT".into()));
            
            // 生成更加波動的價格數據以觸發策略信號
            let base_price = 67000.0;
            let _time_secs = t0.elapsed().as_secs() as f64;
            
            // 使用多個不同頻率的 sin 波組合產生更複雜的價格變化
            let fast_wave = (counter as f64 * 0.5).sin() * 50.0;  // 快速波動 ±50
            let medium_wave = (counter as f64 * 0.2).sin() * 100.0; // 中速波動 ±100
            let slow_wave = (counter as f64 * 0.05).sin() * 200.0;  // 慢速波動 ±200
            
            // 組合波動並添加趨勢
            let close = base_price + fast_wave + medium_wave + slow_wave + (counter as f64 * 2.0); // 更強的趨勢
            let open = close - 10.0; // 更大的開盤價差異
            let high = close.max(open) + 15.0; // 更大的高低價差
            let low = close.min(open) - 15.0;
            
            let bar = AggregatedBar {
                symbol: sym.clone(), 
                interval_ms: 60_000, // 1分鐘K線，更符合策略預期
                open_time: ts - 60_000_000, // 1分鐘前
                close_time: ts,
                open: Price::from_f64(open).unwrap(), 
                high: Price::from_f64(high).unwrap(), 
                low: Price::from_f64(low).unwrap(), 
                close: Price::from_f64(close).unwrap(),
                volume: Quantity::from_f64((counter % 10 + 1) as f64).unwrap(), // 變化的成交量
                trade_count: (counter % 5 + 1) as u32,
            };
            
            tracing::info!("MockStream 生成 Bar: symbol={}, close={:.1}, counter={}", 
                          sym.0, close, counter);
            
            Ok::<MarketEvent, HftError>(MarketEvent::Bar(bar))
        });
        Ok(Box::pin(stream))
    }

    async fn health(&self) -> ConnectionHealth {
        ConnectionHealth { connected: true, latency_ms: Some(0.1), last_heartbeat: current_ts() }
    }

    async fn connect(&mut self) -> HftResult<()> { Ok(()) }
    async fn disconnect(&mut self) -> HftResult<()> { Ok(()) }
}

fn current_ts() -> Timestamp {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_micros() as u64
}

