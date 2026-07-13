//! Canonical market-update recording and replay feeds.
//!
//! `RecordingFeed` wraps any other feed and appends each `MarketUpdate` to an
//! NDJSON log. `RecordedFeed` replays the exact same update sequence back into
//! the strategy runtime.

use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use crate::traits::{Feed, MarketUpdate};

const FLUSH_EVERY_RECORDS: usize = 256;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingLimits {
    pub max_records: Option<u64>,
    pub max_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordedMarketUpdate {
    pub sequence: u64,
    pub recorded_at: DateTime<Utc>,
    pub update: MarketUpdate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppendOutcome {
    Written,
    LimitReached,
}

#[derive(Debug, thiserror::Error)]
pub enum RecordedFeedError {
    #[error("failed to open market-update log {path}: {source}")]
    Open {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to read market-update log {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid market-update log line {line} in {path}: {source}")]
    Parse {
        path: PathBuf,
        line: usize,
        #[source]
        source: serde_json::Error,
    },
}

struct MarketUpdateLogWriter {
    path: PathBuf,
    writer: BufWriter<File>,
    next_sequence: u64,
    pending_records: usize,
    bytes_written: u64,
    limits: RecordingLimits,
}

impl MarketUpdateLogWriter {
    fn create_with_limits(path: impl AsRef<Path>, limits: RecordingLimits) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }

        // If the path already exists, rotate it with a timestamp suffix so the
        // previous session's recording is never silently destroyed.
        let path = if path.exists() {
            let ts = Utc::now().format("%Y%m%dT%H%M%S");
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("recording");
            let ext = path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("ndjson");
            let rotated = path.with_file_name(format!("{stem}.{ts}.{ext}"));
            warn!(
                original = %path.display(),
                rotated = %rotated.display(),
                "Recording path already exists — rotating previous file to avoid data loss",
            );
            fs::rename(&path, &rotated)?;
            path
        } else {
            path
        };

        let file = File::create(&path)?;
        info!(path = %path.display(), "Recording market updates to NDJSON log");

        Ok(Self {
            path,
            writer: BufWriter::new(file),
            next_sequence: 0,
            pending_records: 0,
            bytes_written: 0,
            limits,
        })
    }

    fn append(&mut self, update: &MarketUpdate) -> io::Result<AppendOutcome> {
        if self
            .limits
            .max_records
            .is_some_and(|max_records| self.next_sequence >= max_records)
        {
            self.flush()?;
            return Ok(AppendOutcome::LimitReached);
        }

        let record = RecordedMarketUpdate {
            sequence: self.next_sequence,
            recorded_at: Utc::now(),
            update: update.clone(),
        };
        let mut line = serde_json::to_vec(&record).map_err(io::Error::other)?;
        line.push(b'\n');

        if self.bytes_written > 0
            && self.limits.max_bytes.is_some_and(|max_bytes| {
                self.bytes_written + u64::try_from(line.len()).unwrap_or(u64::MAX) > max_bytes
            })
        {
            self.flush()?;
            return Ok(AppendOutcome::LimitReached);
        }

        self.next_sequence += 1;
        self.writer.write_all(&line)?;
        self.bytes_written += u64::try_from(line.len()).unwrap_or(u64::MAX);
        self.pending_records += 1;

        let is_lifecycle = matches!(
            update,
            MarketUpdate::EventDiscovered { .. } | MarketUpdate::EventExpired { .. }
        );

        if self.pending_records >= FLUSH_EVERY_RECORDS || is_lifecycle {
            self.flush()?;
        }

        Ok(AppendOutcome::Written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()?;
        self.pending_records = 0;
        Ok(())
    }
}

impl Drop for MarketUpdateLogWriter {
    fn drop(&mut self) {
        if let Err(error) = self.flush() {
            warn!(
                path = %self.path.display(),
                error = %error,
                "Failed to flush market-update log on drop",
            );
        }
    }
}

/// Wraps a feed and records each emitted update to an NDJSON log.
pub struct RecordingFeed<F> {
    inner: F,
    writer: Option<MarketUpdateLogWriter>,
}

impl<F> RecordingFeed<F> {
    pub fn new(inner: F, path: impl AsRef<Path>) -> io::Result<Self> {
        Self::with_limits(inner, path, RecordingLimits::default())
    }

    pub fn with_limits(
        inner: F,
        path: impl AsRef<Path>,
        limits: RecordingLimits,
    ) -> io::Result<Self> {
        Ok(Self {
            inner,
            writer: Some(MarketUpdateLogWriter::create_with_limits(path, limits)?),
        })
    }
}

#[async_trait]
impl<F> Feed for RecordingFeed<F>
where
    F: Feed,
{
    async fn next(&mut self) -> Option<MarketUpdate> {
        let update = self.inner.next().await?;

        if let Some(writer) = self.writer.as_mut() {
            match writer.append(&update) {
                Ok(AppendOutcome::Written) => {}
                Ok(AppendOutcome::LimitReached) => {
                    info!(
                        path = %writer.path.display(),
                        records = writer.next_sequence,
                        bytes = writer.bytes_written,
                        "Market-update recording limit reached; preserving bounded replay log",
                    );
                    self.writer = None;
                }
                Err(error) => {
                    error!(
                        path = %writer.path.display(),
                        error = %error,
                        "Market-update recording failed; disabling recorder for the rest of the run",
                    );
                    self.writer = None;
                }
            }
        }

        Some(update)
    }
}

/// Feed that replays a previously recorded market-update log in file order.
pub struct RecordedFeed {
    updates: VecDeque<MarketUpdate>,
}

impl RecordedFeed {
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, RecordedFeedError> {
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path).map_err(|source| RecordedFeedError::Open {
            path: path.clone(),
            source,
        })?;
        let reader = BufReader::new(file);
        let mut updates = VecDeque::new();

        for (idx, line) in reader.lines().enumerate() {
            let line = line.map_err(|source| RecordedFeedError::Read {
                path: path.clone(),
                source,
            })?;

            if line.trim().is_empty() {
                continue;
            }

            let record = serde_json::from_str::<RecordedMarketUpdate>(&line).map_err(|source| {
                RecordedFeedError::Parse {
                    path: path.clone(),
                    line: idx + 1,
                    source,
                }
            })?;
            updates.push_back(record.update);
        }

        info!(
            path = %path.display(),
            updates = updates.len(),
            "Loaded recorded market-update log",
        );

        Ok(Self { updates })
    }

    pub fn remaining(&self) -> usize {
        self.updates.len()
    }
}

#[async_trait]
impl Feed for RecordedFeed {
    async fn next(&mut self) -> Option<MarketUpdate> {
        self.updates.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use rust_decimal_macros::dec;

    fn temp_log_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("ploy-{name}-{}.ndjson", uuid::Uuid::new_v4()));
        path
    }

    #[tokio::test]
    async fn recording_feed_round_trips_updates() {
        let now = Utc::now();
        let updates = vec![
            MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(100000),
                ts: now,
            },
            MarketUpdate::EventDiscovered {
                event_id: "evt-1".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up-1".into(),
                down_token: "down-1".into(),
                end_time: now + Duration::minutes(5),
                window_secs: 300,
                price_to_beat: Some(dec!(100000)),
                resolved_up_won: Some(true),
            },
            MarketUpdate::Quote {
                token_id: "up-1".into(),
                bid: Some(dec!(0.39)),
                ask: Some(dec!(0.40)),
                ts: now + Duration::seconds(1),
                bid_size: None,
                ask_size: None,
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
            },
            MarketUpdate::SportsState {
                game_id: "19439".into(),
                league: "nfl".into(),
                slug: "nfl-lac-buf-2025-01-26".into(),
                home_team: "LAC".into(),
                away_team: "BUF".into(),
                status: "InProgress".into(),
                period: Some("Q4".into()),
                score: Some("3-16".into()),
                elapsed: Some("5:18".into()),
                live: true,
                ended: false,
                finished_at: None,
                ts: now + Duration::seconds(2),
            },
            MarketUpdate::ReferencePrice {
                symbol: "aapl".into(),
                source: "pyth".into(),
                asset_class: "equity".into(),
                price: dec!(212.45),
                full_accuracy_value: Some("212.450000".into()),
                is_carried_forward: false,
                ts: now + Duration::seconds(3),
            },
        ];

        let path = temp_log_path("recording-feed-round-trip");
        let mut feed =
            RecordingFeed::new(crate::HistoricalFeed::new(updates.clone()), &path).unwrap();

        let mut recorded = Vec::new();
        while let Some(update) = feed.next().await {
            recorded.push(update);
        }
        drop(feed);

        let mut replay = RecordedFeed::from_path(&path).unwrap();
        let mut replayed = Vec::new();
        while let Some(update) = replay.next().await {
            replayed.push(update);
        }

        assert_eq!(recorded, updates);
        assert_eq!(replayed, updates);

        let _ = fs::remove_file(path);
    }

    #[tokio::test]
    async fn recording_feed_stops_after_record_limit_without_blocking_feed() {
        let now = Utc::now();
        let updates = vec![
            MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(100000),
                ts: now,
            },
            MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(100010),
                ts: now + Duration::seconds(1),
            },
            MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(100020),
                ts: now + Duration::seconds(2),
            },
        ];
        let path = temp_log_path("recording-feed-limit");
        let mut feed = RecordingFeed::with_limits(
            crate::HistoricalFeed::new(updates.clone()),
            &path,
            RecordingLimits {
                max_records: Some(2),
                max_bytes: None,
            },
        )
        .unwrap();

        let mut forwarded = Vec::new();
        while let Some(update) = feed.next().await {
            forwarded.push(update);
        }
        drop(feed);

        let mut replay = RecordedFeed::from_path(&path).unwrap();
        let mut replayed = Vec::new();
        while let Some(update) = replay.next().await {
            replayed.push(update);
        }

        assert_eq!(forwarded, updates);
        assert_eq!(replayed, updates[..2]);

        let _ = fs::remove_file(path);
    }
}
