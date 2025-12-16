use async_trait::async_trait;
use futures::StreamExt;
use hft_core::{HftError, HftResult, Price, Quantity, Symbol, Timestamp, VenueId};
use ports::{AggregatedBar, BoxStream, ConnectionHealth, MarketEvent, MarketStream};
use tokio::time::{interval, Duration, Instant};
use tokio_stream::wrappers::IntervalStream;
use tracing::info;

/// 簡單的 mock 行情：定期產生 Bar 與 Trade 事件
pub struct MockMarketStream {
    pub interval_ms: u64,
}

impl Default for MockMarketStream {
    fn default() -> Self {
        Self { interval_ms: 200 }
    }
}

impl MockMarketStream {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_interval_ms(interval_ms: u64) -> Self {
        Self { interval_ms }
    }
}

#[async_trait]
impl MarketStream for MockMarketStream {
    async fn subscribe(&self, symbols: Vec<Symbol>) -> HftResult<BoxStream<MarketEvent>> {
        info!(
            "MockMarketStream 訂閱 symbols={:?}, 間隔: {}ms",
            symbols, self.interval_ms
        );
        let t0 = Instant::now();
        let interval = interval(Duration::from_millis(self.interval_ms));
        let mut counter = 0u64;

        let stream = IntervalStream::new(interval).map(move |_| {
            counter += 1;
            let ts = current_ts();
            let sym = symbols.first().cloned().unwrap_or(Symbol::new("BTCUSDT"));

            // 生成更加波動的價格數據以觸發策略信號
            let base_price = 67000.0;
            let _time_secs = t0.elapsed().as_secs() as f64;

            // 使用多個不同頻率的 sin 波組合產生更複雜的價格變化
            let fast_wave = (counter as f64 * 0.5).sin() * 50.0; // 快速波動 ±50
            let medium_wave = (counter as f64 * 0.2).sin() * 100.0; // 中速波動 ±100
            let slow_wave = (counter as f64 * 0.05).sin() * 200.0; // 慢速波動 ±200

            // 組合波動並添加趨勢
            let close = base_price + fast_wave + medium_wave + slow_wave + (counter as f64 * 2.0); // 更強的趨勢
            let open = close - 10.0; // 更大的開盤價差異
            let high = close.max(open) + 15.0; // 更大的高低價差
            let low = close.min(open) - 15.0;

            let bar = AggregatedBar {
                symbol: sym.clone(),
                interval_ms: 60_000,        // 1分鐘K線，更符合策略預期
                open_time: ts - 60_000_000, // 1分鐘前
                close_time: ts,
                open: Price::from_f64(open).unwrap(),
                high: Price::from_f64(high).unwrap(),
                low: Price::from_f64(low).unwrap(),
                close: Price::from_f64(close).unwrap(),
                volume: Quantity::from_f64((counter % 10 + 1) as f64).unwrap(), // 變化的成交量
                trade_count: (counter % 5 + 1) as u32,
                source_venue: Some(VenueId::MOCK), // Mock adapter always uses MOCK venue
            };

            tracing::info!(
                "MockStream 生成 Bar: symbol={}, close={:.1}, counter={}",
                sym.as_str(),
                close,
                counter
            );

            Ok::<MarketEvent, HftError>(MarketEvent::Bar(bar))
        });
        Ok(Box::pin(stream))
    }

    async fn health(&self) -> ConnectionHealth {
        ConnectionHealth {
            connected: true,
            latency_ms: Some(0.1),
            last_heartbeat: current_ts(),
        }
    }

    async fn connect(&mut self) -> HftResult<()> {
        Ok(())
    }
    async fn disconnect(&mut self) -> HftResult<()> {
        Ok(())
    }
}

fn current_ts() -> Timestamp {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    #[test]
    fn test_mock_market_stream_default() {
        let stream = MockMarketStream::default();
        assert_eq!(stream.interval_ms, 200);
    }

    #[test]
    fn test_mock_market_stream_new() {
        let stream = MockMarketStream::new();
        assert_eq!(stream.interval_ms, 200);
    }

    #[test]
    fn test_mock_market_stream_with_interval() {
        let stream = MockMarketStream::with_interval_ms(100);
        assert_eq!(stream.interval_ms, 100);
    }

    #[tokio::test]
    async fn test_health_check() {
        let stream = MockMarketStream::new();
        let health = stream.health().await;

        assert!(health.connected);
        assert_eq!(health.latency_ms, Some(0.1));
        assert!(health.last_heartbeat > 0);
    }

    #[tokio::test]
    async fn test_connect_disconnect() {
        let mut stream = MockMarketStream::new();

        // Connect should succeed
        let connect_result = stream.connect().await;
        assert!(connect_result.is_ok());

        // Disconnect should succeed
        let disconnect_result = stream.disconnect().await;
        assert!(disconnect_result.is_ok());
    }

    #[tokio::test]
    async fn test_subscribe_returns_stream() {
        let stream = MockMarketStream::with_interval_ms(50);
        let symbols = vec![Symbol::new("BTCUSDT")];

        let result = stream.subscribe(symbols).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_subscribe_produces_bar_events() {
        let stream = MockMarketStream::with_interval_ms(10);
        let symbols = vec![Symbol::new("ETHUSDT")];

        let mut event_stream = stream.subscribe(symbols).await.unwrap();

        // Get first event
        let event = tokio::time::timeout(
            Duration::from_millis(100),
            event_stream.next(),
        )
        .await
        .expect("Timeout waiting for event")
        .expect("Stream ended unexpectedly");

        match event {
            Ok(MarketEvent::Bar(bar)) => {
                assert_eq!(bar.symbol, Symbol::new("ETHUSDT"));
                assert_eq!(bar.interval_ms, 60_000);
                assert_eq!(bar.source_venue, Some(VenueId::MOCK));
                assert!(bar.high >= bar.low);
                assert!(bar.high >= bar.open);
                assert!(bar.high >= bar.close);
                assert!(bar.low <= bar.open);
                assert!(bar.low <= bar.close);
            }
            Ok(other) => panic!("Expected Bar event, got {:?}", other),
            Err(e) => panic!("Event error: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_subscribe_multiple_events() {
        let stream = MockMarketStream::with_interval_ms(10);
        let symbols = vec![Symbol::new("BTCUSDT")];

        let mut event_stream = stream.subscribe(symbols).await.unwrap();

        // Collect 3 events
        let mut events = Vec::new();
        for _ in 0..3 {
            let event = tokio::time::timeout(
                Duration::from_millis(100),
                event_stream.next(),
            )
            .await
            .expect("Timeout waiting for event")
            .expect("Stream ended unexpectedly");
            events.push(event);
        }

        assert_eq!(events.len(), 3);

        // All events should be Ok Bar events
        for event in events {
            assert!(matches!(event, Ok(MarketEvent::Bar(_))));
        }
    }

    #[tokio::test]
    async fn test_subscribe_empty_symbols_uses_default() {
        let stream = MockMarketStream::with_interval_ms(10);
        let symbols: Vec<Symbol> = vec![];

        let mut event_stream = stream.subscribe(symbols).await.unwrap();

        let event = tokio::time::timeout(
            Duration::from_millis(100),
            event_stream.next(),
        )
        .await
        .expect("Timeout waiting for event")
        .expect("Stream ended unexpectedly");

        match event {
            Ok(MarketEvent::Bar(bar)) => {
                // When no symbols provided, defaults to BTCUSDT
                assert_eq!(bar.symbol, Symbol::new("BTCUSDT"));
            }
            _ => panic!("Expected Bar event with default symbol"),
        }
    }

    #[tokio::test]
    async fn test_bar_price_volatility() {
        let stream = MockMarketStream::with_interval_ms(10);
        let symbols = vec![Symbol::new("BTCUSDT")];

        let mut event_stream = stream.subscribe(symbols).await.unwrap();

        // Collect multiple bars and verify prices change
        let mut prices = Vec::new();
        for _ in 0..5 {
            let event = tokio::time::timeout(
                Duration::from_millis(100),
                event_stream.next(),
            )
            .await
            .expect("Timeout waiting for event")
            .expect("Stream ended unexpectedly");

            if let Ok(MarketEvent::Bar(bar)) = event {
                prices.push(bar.close.to_f64().unwrap());
            }
        }

        // Prices should vary (not all the same)
        let first = prices[0];
        let has_variation = prices.iter().any(|p| (*p - first).abs() > 0.01);
        assert!(has_variation, "Mock stream should produce varying prices");
    }

    #[tokio::test]
    async fn test_bar_volume_and_trade_count() {
        let stream = MockMarketStream::with_interval_ms(10);
        let symbols = vec![Symbol::new("BTCUSDT")];

        let mut event_stream = stream.subscribe(symbols).await.unwrap();

        let event = tokio::time::timeout(
            Duration::from_millis(100),
            event_stream.next(),
        )
        .await
        .expect("Timeout waiting for event")
        .expect("Stream ended unexpectedly");

        if let Ok(MarketEvent::Bar(bar)) = event {
            // Volume should be positive
            assert!(bar.volume.to_f64().unwrap() > 0.0);
            // Trade count should be positive
            assert!(bar.trade_count > 0);
        } else {
            panic!("Expected Bar event");
        }
    }

    #[tokio::test]
    async fn test_bar_timestamp_ordering() {
        let stream = MockMarketStream::with_interval_ms(10);
        let symbols = vec![Symbol::new("BTCUSDT")];

        let mut event_stream = stream.subscribe(symbols).await.unwrap();

        let event = tokio::time::timeout(
            Duration::from_millis(100),
            event_stream.next(),
        )
        .await
        .expect("Timeout waiting for event")
        .expect("Stream ended unexpectedly");

        if let Ok(MarketEvent::Bar(bar)) = event {
            // close_time should be after open_time
            assert!(bar.close_time > bar.open_time);
            // The difference should be interval_ms in microseconds
            assert_eq!(bar.close_time - bar.open_time, 60_000_000);
        } else {
            panic!("Expected Bar event");
        }
    }
}
