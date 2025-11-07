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
