//! ClickHouse 歷史回放適配器
//!
//! 將 ClickHouse 中的原始 LOB 事件重放為 `ports::MarketEvent`，
//! 便於與引擎對接進行回測或離線測試。

use async_trait::async_trait;
use hft_core::{HftError, HftResult, Price, Quantity, Symbol};
use ports::{BookLevel, BoxStream, MarketEvent, MarketSnapshot, MarketStream};

/// 回放來源交易所
#[derive(Debug, Clone)]
pub enum ReplayVenue {
    Binance,
    Bitget,
}

impl ReplayVenue {
    #[allow(dead_code)]
    fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "binance" => ReplayVenue::Binance,
            _ => ReplayVenue::Bitget,
        }
    }
}

/// ClickHouse 回放配置
#[derive(Debug, Clone)]
pub struct ReplayConfig {
    pub ch_url: String,
    pub database: String,
    pub venue: ReplayVenue,
    pub start_us: u64,
    pub end_us: Option<u64>,
    pub speed: f64, // 1.0 = 原速；0 = 不限速
}

impl Default for ReplayConfig {
    fn default() -> Self {
        Self {
            ch_url: "http://localhost:8123".to_string(),
            database: "hft".to_string(),
            venue: ReplayVenue::Bitget,
            start_us: 0,
            end_us: None,
            speed: 0.0,
        }
    }
}

pub struct ClickhouseReplayStream {
    cfg: ReplayConfig,
}

impl ClickhouseReplayStream {
    pub fn new(cfg: ReplayConfig) -> Self {
        Self { cfg }
    }
}

#[async_trait]
impl MarketStream for ClickhouseReplayStream {
    async fn subscribe(&self, symbols: Vec<Symbol>) -> HftResult<BoxStream<MarketEvent>> {
        if symbols.is_empty() {
            return Err(HftError::Generic {
                message: "Replay 需要至少一個 symbol".to_string(),
            });
        }
        let symbol = symbols[0].as_str().to_string();
        let cfg = self.cfg.clone();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            // 建立 ClickHouse 連線
            let ch = clickhouse::Client::default()
                .with_url(&cfg.ch_url)
                .with_database(&cfg.database);

            // 載入初始快照
            #[derive(serde::Deserialize, clickhouse::Row)]
            struct SnapRow {
                ts: u64,
                bids_px: Vec<f64>,
                bids_qty: Vec<f64>,
                asks_px: Vec<f64>,
                asks_qty: Vec<f64>,
            }
            let snap_q = "SELECT ts, bids_px, bids_qty, asks_px, asks_qty FROM snapshot_books WHERE symbol = ? AND venue = ? AND ts <= ? ORDER BY ts DESC LIMIT 1";
            let venue_str = match cfg.venue {
                ReplayVenue::Binance => "binance",
                ReplayVenue::Bitget => "bitget",
            };
            let mut snap_cur = match ch
                .query(snap_q)
                .bind(&symbol)
                .bind(venue_str)
                .bind(cfg.start_us)
                .fetch::<SnapRow>()
            {
                Ok(cur) => cur,
                Err(e) => {
                    let _ = tx.send(MarketEvent::Disconnect {
                        reason: format!("CH snapshot query failed: {}", e),
                    });
                    return;
                }
            };
            if let Ok(Some(s)) = snap_cur.next().await {
                let bids: Vec<BookLevel> = s
                    .bids_px
                    .iter()
                    .enumerate()
                    .filter_map(|(i, p)| {
                        s.bids_qty.get(i).and_then(|q| {
                            Some(BookLevel {
                                price: Price::from_f64(*p).ok()?,
                                quantity: Quantity::from_f64(*q).ok()?,
                            })
                        })
                    })
                    .collect();
                let asks: Vec<BookLevel> = s
                    .asks_px
                    .iter()
                    .enumerate()
                    .filter_map(|(i, p)| {
                        s.asks_qty.get(i).and_then(|q| {
                            Some(BookLevel {
                                price: Price::from_f64(*p).ok()?,
                                quantity: Quantity::from_f64(*q).ok()?,
                            })
                        })
                    })
                    .collect();
                let snapshot = MarketSnapshot {
                    symbol: Symbol::from(symbol.clone()),
                    timestamp: s.ts,
                    bids,
                    asks,
                    sequence: 0,
                    source_venue: Some(hft_core::VenueId::MOCK),
                };
                let _ = tx.send(MarketEvent::Snapshot(snapshot));
            } else {
                let _ = tx.send(MarketEvent::Disconnect {
                    reason: "No starting snapshot".to_string(),
                });
            }

            // 讀取增量事件（按 venue 選擇表與排序）
            let (table, order) = match cfg.venue {
                ReplayVenue::Binance => ("raw_depth_binance", "ORDER BY event_ts ASC, u ASC"),
                ReplayVenue::Bitget => ("raw_books_bitget", "ORDER BY event_ts ASC, seq ASC"),
            };
            let end_us = cfg.end_us.unwrap_or(u64::MAX);
            let q = format!(
                "SELECT event_ts, bids_px, bids_qty, asks_px, asks_qty FROM {} WHERE symbol = ? AND event_ts >= ? AND event_ts <= ? {}",
                table, order
            );
            #[derive(serde::Deserialize, clickhouse::Row)]
            struct EvRow {
                event_ts: u64,
                bids_px: Vec<f64>,
                bids_qty: Vec<f64>,
                asks_px: Vec<f64>,
                asks_qty: Vec<f64>,
            }
            let mut cur = match ch
                .query(&q)
                .bind(&symbol)
                .bind(cfg.start_us)
                .bind(end_us)
                .fetch::<EvRow>()
            {
                Ok(cur) => cur,
                Err(e) => {
                    let _ = tx.send(MarketEvent::Disconnect {
                        reason: format!("CH query failed: {}", e),
                    });
                    return;
                }
            };

            let mut last_ts = cfg.start_us;
            let speed = if cfg.speed <= 0.0 { 0.0 } else { cfg.speed };
            while let Ok(Some(r)) = cur.next().await {
                if speed > 0.0 && r.event_ts > last_ts {
                    let dt = r.event_ts - last_ts; // us
                    let sleep_ns = (dt as f64 / speed) as u64 * 1000; // us->ns
                    if sleep_ns > 0 {
                        tokio::time::sleep(std::time::Duration::from_nanos(sleep_ns)).await;
                    }
                }
                last_ts = r.event_ts;

                let bids: Vec<BookLevel> = r
                    .bids_px
                    .iter()
                    .enumerate()
                    .filter_map(|(i, p)| {
                        r.bids_qty.get(i).and_then(|q| {
                            Some(BookLevel {
                                price: Price::from_f64(*p).ok()?,
                                quantity: Quantity::from_f64(*q).ok()?,
                            })
                        })
                    })
                    .collect();
                let asks: Vec<BookLevel> = r
                    .asks_px
                    .iter()
                    .enumerate()
                    .filter_map(|(i, p)| {
                        r.asks_qty.get(i).and_then(|q| {
                            Some(BookLevel {
                                price: Price::from_f64(*p).ok()?,
                                quantity: Quantity::from_f64(*q).ok()?,
                            })
                        })
                    })
                    .collect();
                let snapshot = MarketSnapshot {
                    symbol: Symbol::from(symbol.clone()),
                    timestamp: r.event_ts,
                    bids,
                    asks,
                    sequence: 0,
                    source_venue: Some(hft_core::VenueId::MOCK),
                };
                if tx.send(MarketEvent::Snapshot(snapshot)).is_err() {
                    break;
                }
            }
            let _ = tx.send(MarketEvent::Disconnect {
                reason: "replay_finish".to_string(),
            });
        });

        let stream = async_stream::stream! {
            while let Some(ev) = rx.recv().await { yield Ok(ev); }
        };
        Ok(Box::pin(stream))
    }

    async fn health(&self) -> ports::ConnectionHealth {
        ports::ConnectionHealth {
            connected: true,
            latency_ms: None,
            last_heartbeat: hft_core::now_micros(),
        }
    }

    async fn connect(&mut self) -> HftResult<()> {
        Ok(())
    }
    async fn disconnect(&mut self) -> HftResult<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ports::MarketStream;

    #[test]
    fn test_replay_venue_from_str() {
        let binance = ReplayVenue::from_str("binance");
        assert!(matches!(binance, ReplayVenue::Binance));

        let binance_upper = ReplayVenue::from_str("BINANCE");
        assert!(matches!(binance_upper, ReplayVenue::Binance));

        let bitget = ReplayVenue::from_str("bitget");
        assert!(matches!(bitget, ReplayVenue::Bitget));

        // Unknown venue defaults to Bitget
        let unknown = ReplayVenue::from_str("unknown");
        assert!(matches!(unknown, ReplayVenue::Bitget));
    }

    #[test]
    fn test_replay_config_default() {
        let config = ReplayConfig::default();

        assert_eq!(config.ch_url, "http://localhost:8123");
        assert_eq!(config.database, "hft");
        assert!(matches!(config.venue, ReplayVenue::Bitget));
        assert_eq!(config.start_us, 0);
        assert_eq!(config.end_us, None);
        assert_eq!(config.speed, 0.0);
    }

    #[test]
    fn test_replay_config_custom() {
        let config = ReplayConfig {
            ch_url: "http://clickhouse:8123".to_string(),
            database: "trading".to_string(),
            venue: ReplayVenue::Binance,
            start_us: 1000000,
            end_us: Some(2000000),
            speed: 2.0,
        };

        assert_eq!(config.ch_url, "http://clickhouse:8123");
        assert_eq!(config.database, "trading");
        assert!(matches!(config.venue, ReplayVenue::Binance));
        assert_eq!(config.start_us, 1000000);
        assert_eq!(config.end_us, Some(2000000));
        assert_eq!(config.speed, 2.0);
    }

    #[test]
    fn test_clickhouse_replay_stream_creation() {
        let config = ReplayConfig::default();
        let stream = ClickhouseReplayStream::new(config.clone());

        assert_eq!(stream.cfg.ch_url, config.ch_url);
        assert_eq!(stream.cfg.database, config.database);
    }

    #[tokio::test]
    async fn test_health_check() {
        let config = ReplayConfig::default();
        let stream = ClickhouseReplayStream::new(config);

        let health = stream.health().await;
        assert!(health.connected);
        assert!(health.latency_ms.is_none());
        assert!(health.last_heartbeat > 0);
    }

    #[tokio::test]
    async fn test_connect_disconnect() {
        let config = ReplayConfig::default();
        let mut stream = ClickhouseReplayStream::new(config);

        // Connect should succeed
        let connect_result = stream.connect().await;
        assert!(connect_result.is_ok());

        // Disconnect should succeed
        let disconnect_result = stream.disconnect().await;
        assert!(disconnect_result.is_ok());
    }

    #[tokio::test]
    async fn test_subscribe_empty_symbols_fails() {
        let config = ReplayConfig::default();
        let stream = ClickhouseReplayStream::new(config);

        let result = stream.subscribe(vec![]).await;
        assert!(result.is_err());

        if let Err(HftError::Generic { message }) = result {
            assert!(message.contains("symbol"));
        } else {
            panic!("Expected Generic error with symbol message");
        }
    }

    #[test]
    fn test_replay_venue_debug() {
        let binance = ReplayVenue::Binance;
        let bitget = ReplayVenue::Bitget;

        // Debug trait should be implemented
        assert_eq!(format!("{:?}", binance), "Binance");
        assert_eq!(format!("{:?}", bitget), "Bitget");
    }

    #[test]
    fn test_replay_venue_clone() {
        let original = ReplayVenue::Binance;
        let cloned = original.clone();

        assert!(matches!(cloned, ReplayVenue::Binance));
    }

    #[test]
    fn test_replay_config_clone() {
        let config = ReplayConfig {
            ch_url: "http://test:8123".to_string(),
            database: "testdb".to_string(),
            venue: ReplayVenue::Binance,
            start_us: 100,
            end_us: Some(200),
            speed: 1.5,
        };

        let cloned = config.clone();
        assert_eq!(cloned.ch_url, config.ch_url);
        assert_eq!(cloned.database, config.database);
        assert_eq!(cloned.start_us, config.start_us);
        assert_eq!(cloned.end_us, config.end_us);
        assert_eq!(cloned.speed, config.speed);
    }
}
