//! Canonical market-update recording and replay feeds.
//!
//! `RecordingFeed` wraps any other feed and appends each `MarketUpdate` to an
//! NDJSON log. `RecordedFeed` replays the exact same update sequence back into
//! the strategy runtime.

use std::collections::{HashMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use crate::traits::{Feed, MarketUpdate};

const FLUSH_EVERY_RECORDS: usize = 256;

fn rotation_path(path: &Path) -> PathBuf {
    let ts = Utc::now().format("%Y%m%dT%H%M%S%6f");
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("recording");
    let ext = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("ndjson");
    let candidate = path.with_file_name(format!("{stem}.{ts}.{ext}"));
    if candidate.exists() {
        path.with_file_name(format!("{stem}.{ts}.{}.{}", uuid::Uuid::new_v4(), ext))
    } else {
        candidate
    }
}

fn rotation_bucket(rotate_seconds: Option<u64>) -> Option<i64> {
    let seconds = i64::try_from(rotate_seconds.filter(|seconds| *seconds > 0)?).ok()?;
    Some(Utc::now().timestamp().div_euclid(seconds))
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingLimits {
    pub max_records: Option<u64>,
    pub max_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingKind {
    SpotPrice,
    AggTrade,
    Quote,
    L2,
    L2Depth,
    EventDiscovered,
    EventExpired,
    SportsState,
    SportsPregame,
    SportsLive,
    ReferencePrice,
    Kline,
}

impl RecordingKind {
    fn matches(self, update: &MarketUpdate) -> bool {
        matches!(
            (self, update),
            (Self::SpotPrice, MarketUpdate::SpotPrice { .. })
                | (Self::AggTrade, MarketUpdate::AggTrade { .. })
                | (Self::Quote, MarketUpdate::Quote { .. })
                | (Self::L2, MarketUpdate::L2 { .. })
                | (Self::L2Depth, MarketUpdate::L2Depth { .. })
                | (Self::EventDiscovered, MarketUpdate::EventDiscovered { .. })
                | (Self::EventExpired, MarketUpdate::EventExpired { .. })
                | (Self::SportsState, MarketUpdate::SportsState { .. })
                | (Self::SportsPregame, MarketUpdate::SportsPregame { .. })
                | (Self::SportsLive, MarketUpdate::SportsLive { .. })
                | (Self::ReferencePrice, MarketUpdate::ReferencePrice { .. })
                | (Self::Kline, MarketUpdate::Kline { .. })
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecordingPolicy {
    pub limits: RecordingLimits,
    /// Rotate the active tape without restarting the feed process.
    pub rotate_seconds: Option<u64>,
    /// Empty preserves the historical behavior of recording every update kind.
    pub include_kinds: Vec<RecordingKind>,
    /// Minimum event-time spacing between recorded quotes for the same token.
    pub quote_sample_ms: Option<u64>,
    /// Maximum number of bid/ask levels persisted per quote.
    pub quote_depth_levels: Option<usize>,
    /// Persist quotes only while their discovered event is active.
    pub event_scoped_quotes: bool,
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
    rotate_seconds: Option<u64>,
    rotation_bucket: Option<i64>,
}

impl MarketUpdateLogWriter {
    fn create_with_limits(
        path: impl AsRef<Path>,
        limits: RecordingLimits,
        rotate_seconds: Option<u64>,
    ) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }

        // If the path already exists, rotate it with a timestamp suffix so the
        // previous session's recording is never silently destroyed.
        let path = if path.exists() {
            let rotated = rotation_path(&path);
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
            rotate_seconds,
            rotation_bucket: rotation_bucket(rotate_seconds),
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

    fn rotation_due(&self) -> bool {
        self.rotation_bucket != rotation_bucket(self.rotate_seconds)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()?;
        self.pending_records = 0;
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<Option<PathBuf>> {
        self.flush()?;
        self.writer.get_ref().sync_all()?;
        let rotated = rotation_path(&self.path);
        let temporary = self.path.with_file_name(format!(
            ".{}.{}.rotate.tmp",
            self.path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("recording"),
            uuid::Uuid::new_v4()
        ));
        let file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => file,
            Err(error) => {
                warn!(
                    path = %self.path.display(),
                    error = %error,
                    "Could not stage the next market-update tape; continuing the active tape",
                );
                return Ok(None);
            }
        };
        if let Err(error) = fs::rename(&self.path, &rotated) {
            let _ = fs::remove_file(&temporary);
            warn!(
                path = %self.path.display(),
                error = %error,
                "Could not rotate market-update tape; continuing the active tape",
            );
            return Ok(None);
        }
        if let Err(publish_error) = fs::rename(&temporary, &self.path) {
            if let Err(rollback_error) = fs::rename(&rotated, &self.path) {
                return Err(io::Error::other(format!(
                    "failed to publish rotated tape: {publish_error}; rollback failed: {rollback_error}"
                )));
            }
            let _ = fs::remove_file(&temporary);
            warn!(
                path = %self.path.display(),
                error = %publish_error,
                "Could not publish the next market-update tape; restored and continuing the active tape",
            );
            return Ok(None);
        }
        self.writer = BufWriter::new(file);
        self.next_sequence = 0;
        self.pending_records = 0;
        self.bytes_written = 0;
        self.rotation_bucket = rotation_bucket(self.rotate_seconds);
        info!(
            active = %self.path.display(),
            rotated = %rotated.display(),
            "Rotated market-update tape without restarting the feed",
        );
        Ok(Some(rotated))
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
    policy: RecordingPolicy,
    last_quote_recorded_at: HashMap<String, DateTime<Utc>>,
    event_tokens: HashMap<String, [String; 2]>,
    quote_token_end_times: HashMap<String, DateTime<Utc>>,
    active_event_updates: HashMap<String, MarketUpdate>,
}

impl<F> RecordingFeed<F> {
    pub fn new(inner: F, path: impl AsRef<Path>) -> io::Result<Self> {
        Self::with_policy(inner, path, RecordingPolicy::default())
    }

    pub fn with_limits(
        inner: F,
        path: impl AsRef<Path>,
        limits: RecordingLimits,
    ) -> io::Result<Self> {
        Self::with_policy(
            inner,
            path,
            RecordingPolicy {
                limits,
                ..RecordingPolicy::default()
            },
        )
    }

    pub fn with_policy(
        inner: F,
        path: impl AsRef<Path>,
        policy: RecordingPolicy,
    ) -> io::Result<Self> {
        Ok(Self {
            inner,
            writer: Some(MarketUpdateLogWriter::create_with_limits(
                path,
                policy.limits,
                policy.rotate_seconds,
            )?),
            policy,
            last_quote_recorded_at: HashMap::new(),
            event_tokens: HashMap::new(),
            quote_token_end_times: HashMap::new(),
            active_event_updates: HashMap::new(),
        })
    }

    fn prepare_recorded_update(&mut self, update: &MarketUpdate) -> Option<MarketUpdate> {
        match update {
            MarketUpdate::EventDiscovered { event_id, .. } => {
                self.active_event_updates
                    .insert(event_id.to_string(), update.clone());
            }
            MarketUpdate::EventExpired { event_id, .. } => {
                self.active_event_updates.remove(event_id.as_ref());
            }
            _ => {}
        }

        if self.policy.event_scoped_quotes {
            match update {
                MarketUpdate::EventDiscovered {
                    event_id,
                    up_token,
                    down_token,
                    end_time,
                    ..
                } => {
                    let tokens = [up_token.to_string(), down_token.to_string()];
                    for token in &tokens {
                        self.quote_token_end_times.insert(token.clone(), *end_time);
                    }
                    self.event_tokens.insert(event_id.to_string(), tokens);
                }
                MarketUpdate::EventExpired { event_id, .. } => {
                    if let Some(tokens) = self.event_tokens.remove(event_id.as_ref()) {
                        for token in tokens {
                            self.quote_token_end_times.remove(&token);
                        }
                    }
                }
                MarketUpdate::Quote { token_id, ts, .. } => {
                    if !self
                        .quote_token_end_times
                        .get(token_id.as_ref())
                        .is_some_and(|end_time| *ts <= *end_time)
                    {
                        return None;
                    }
                }
                _ => {}
            }
        }

        if !self.policy.include_kinds.is_empty()
            && !self
                .policy
                .include_kinds
                .iter()
                .any(|kind| kind.matches(update))
        {
            return None;
        }

        let mut recorded = update.clone();
        if let MarketUpdate::Quote {
            token_id,
            bid_levels,
            ask_levels,
            ts,
            ..
        } = &mut recorded
        {
            if let Some(sample_ms) = self.policy.quote_sample_ms.filter(|value| *value > 0) {
                let sample_ms = i64::try_from(sample_ms).unwrap_or(i64::MAX);
                if self
                    .last_quote_recorded_at
                    .get(token_id.as_ref())
                    .is_some_and(|last| *ts < *last + chrono::Duration::milliseconds(sample_ms))
                {
                    return None;
                }
                self.last_quote_recorded_at
                    .insert(token_id.to_string(), *ts);
            }

            if let Some(depth) = self.policy.quote_depth_levels {
                bid_levels.truncate(depth);
                ask_levels.truncate(depth);
            }
        }

        Some(recorded)
    }
}

#[async_trait]
impl<F> Feed for RecordingFeed<F>
where
    F: Feed,
{
    async fn next(&mut self) -> Option<MarketUpdate> {
        let update = self.inner.next().await?;
        let recorded_update = if self.writer.is_some() {
            self.prepare_recorded_update(&update)
        } else {
            None
        };

        let rotation_due = self
            .writer
            .as_ref()
            .is_some_and(MarketUpdateLogWriter::rotation_due);
        let include_event_checkpoints = self.policy.include_kinds.is_empty()
            || self
                .policy
                .include_kinds
                .contains(&RecordingKind::EventDiscovered);
        let mut event_checkpoints = if rotation_due && include_event_checkpoints {
            self.active_event_updates
                .iter()
                .map(|(event_id, update)| (event_id.clone(), update.clone()))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        event_checkpoints.sort_by(|left, right| left.0.cmp(&right.0));

        let mut recording_error = None;
        let mut limit_reached = false;
        if let Some(writer) = self.writer.as_mut() {
            if rotation_due {
                match writer.rotate() {
                    Ok(Some(_)) => {
                        for (_, checkpoint) in &event_checkpoints {
                            match writer.append(checkpoint) {
                                Ok(AppendOutcome::Written) => {}
                                Ok(AppendOutcome::LimitReached) => {
                                    limit_reached = true;
                                    break;
                                }
                                Err(error) => {
                                    recording_error = Some(error);
                                    break;
                                }
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(error) => recording_error = Some(error),
                }
            }
            if !limit_reached && recording_error.is_none() {
                if let Some(recorded_update) = recorded_update.as_ref() {
                    match writer.append(recorded_update) {
                        Ok(AppendOutcome::Written) => {}
                        Ok(AppendOutcome::LimitReached) => limit_reached = true,
                        Err(error) => recording_error = Some(error),
                    }
                }
            }
        }

        if limit_reached {
            if let Some(writer) = self.writer.as_ref() {
                info!(
                    path = %writer.path.display(),
                    records = writer.next_sequence,
                    bytes = writer.bytes_written,
                    "Market-update recording limit reached; preserving bounded replay log",
                );
            }
            self.writer = None;
        } else if let Some(error) = recording_error {
            if let Some(writer) = self.writer.as_ref() {
                error!(
                    path = %writer.path.display(),
                    error = %error,
                    "Market-update recording failed; disabling recorder for the rest of the run",
                );
            }
            self.writer = None;
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
    use rust_decimal::Decimal;
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

    #[test]
    fn market_update_writer_rotates_without_restarting_and_resets_sequence() {
        let path = temp_log_path("writer-in-process-rotation");
        let update = MarketUpdate::SpotPrice {
            symbol: "BTCUSDT".into(),
            price: dec!(100000),
            ts: Utc::now(),
        };
        let mut writer =
            MarketUpdateLogWriter::create_with_limits(&path, RecordingLimits::default(), None)
                .unwrap();
        assert_eq!(writer.append(&update).unwrap(), AppendOutcome::Written);

        let rotated = writer.rotate().unwrap().unwrap();
        assert_eq!(writer.append(&update).unwrap(), AppendOutcome::Written);
        writer.flush().unwrap();
        drop(writer);

        for tape in [&rotated, &path] {
            let line = fs::read_to_string(tape).unwrap();
            let record: RecordedMarketUpdate = serde_json::from_str(line.trim()).unwrap();
            assert_eq!(record.sequence, 0);
        }

        let _ = fs::remove_file(rotated);
        let _ = fs::remove_file(path);
    }

    #[tokio::test]
    async fn in_process_rotation_seeds_active_event_context_before_quotes() {
        let now = Utc::now();
        let discovered = MarketUpdate::EventDiscovered {
            event_id: "evt-rotation".into(),
            symbol: "BTCUSDT".into(),
            up_token: "up-rotation".into(),
            down_token: "down-rotation".into(),
            end_time: now + Duration::minutes(5),
            window_secs: 300,
            price_to_beat: Some(dec!(100000)),
            resolved_up_won: None,
        };
        let quote = |millis| MarketUpdate::Quote {
            token_id: "up-rotation".into(),
            bid: Some(dec!(0.49)),
            ask: Some(dec!(0.51)),
            bid_size: Some(dec!(10)),
            ask_size: Some(dec!(11)),
            bid_levels: Vec::new(),
            ask_levels: Vec::new(),
            ts: now + Duration::milliseconds(millis),
        };
        let path = temp_log_path("rotation-event-context");
        let mut feed = RecordingFeed::with_policy(
            crate::HistoricalFeed::new(vec![discovered.clone(), quote(1), quote(2)]),
            &path,
            RecordingPolicy {
                rotate_seconds: Some(3600),
                include_kinds: vec![RecordingKind::EventDiscovered, RecordingKind::Quote],
                event_scoped_quotes: true,
                ..RecordingPolicy::default()
            },
        )
        .unwrap();

        assert_eq!(feed.next().await, Some(discovered.clone()));
        assert_eq!(feed.next().await, Some(quote(1)));
        let writer = feed.writer.as_mut().unwrap();
        writer.rotation_bucket = writer.rotation_bucket.map(|bucket| bucket - 1);
        assert_eq!(feed.next().await, Some(quote(2)));
        drop(feed);

        let records = fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<RecordedMarketUpdate>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].sequence, 0);
        assert!(matches!(
            records[0].update,
            MarketUpdate::EventDiscovered { .. }
        ));
        assert_eq!(records[1].sequence, 1);
        assert!(matches!(records[1].update, MarketUpdate::Quote { .. }));

        let _ = fs::remove_file(path);
    }

    #[tokio::test]
    async fn recording_feed_bounds_prediction_market_tape_without_changing_forwarded_updates() {
        let now = Utc::now();
        let quote = |token: &str, millis: i64, bid: Decimal| MarketUpdate::Quote {
            token_id: token.into(),
            bid: Some(bid),
            ask: Some(bid + dec!(0.01)),
            bid_size: Some(dec!(10)),
            ask_size: Some(dec!(11)),
            bid_levels: vec![
                ploy_market_contracts::BookLevel {
                    price: bid,
                    size: dec!(10),
                },
                ploy_market_contracts::BookLevel {
                    price: bid - dec!(0.01),
                    size: dec!(12),
                },
            ],
            ask_levels: vec![
                ploy_market_contracts::BookLevel {
                    price: bid + dec!(0.01),
                    size: dec!(11),
                },
                ploy_market_contracts::BookLevel {
                    price: bid + dec!(0.02),
                    size: dec!(13),
                },
            ],
            ts: now + Duration::milliseconds(millis),
        };
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
                resolved_up_won: None,
            },
            quote("up-1", 0, dec!(0.49)),
            quote("up-1", 100, dec!(0.50)),
            quote("down-1", 100, dec!(0.48)),
            quote("up-1", 600, dec!(0.51)),
            quote("orphan", 700, dec!(0.52)),
            quote("up-1", 301_000, dec!(0.53)),
            MarketUpdate::ReferencePrice {
                symbol: "btc/usd".into(),
                source: "chainlink".into(),
                asset_class: "crypto".into(),
                price: dec!(100001),
                full_accuracy_value: None,
                is_carried_forward: false,
                ts: now + Duration::seconds(1),
            },
        ];

        let path = temp_log_path("recording-feed-bounded-prediction-market");
        let mut feed = RecordingFeed::with_policy(
            crate::HistoricalFeed::new(updates.clone()),
            &path,
            RecordingPolicy {
                limits: RecordingLimits::default(),
                rotate_seconds: None,
                include_kinds: vec![
                    RecordingKind::Quote,
                    RecordingKind::EventDiscovered,
                    RecordingKind::EventExpired,
                    RecordingKind::ReferencePrice,
                ],
                quote_sample_ms: Some(500),
                quote_depth_levels: Some(1),
                event_scoped_quotes: true,
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
        assert_eq!(replayed.len(), 5);
        assert!(matches!(replayed[0], MarketUpdate::EventDiscovered { .. }));
        let recorded_quotes = replayed
            .iter()
            .filter_map(|update| match update {
                MarketUpdate::Quote {
                    token_id,
                    bid,
                    bid_levels,
                    ask_levels,
                    ..
                } => Some((token_id.as_ref(), *bid, bid_levels.len(), ask_levels.len())),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            recorded_quotes,
            vec![
                ("up-1", Some(dec!(0.49)), 1, 1),
                ("down-1", Some(dec!(0.48)), 1, 1),
                ("up-1", Some(dec!(0.51)), 1, 1),
            ]
        );
        assert!(matches!(replayed[4], MarketUpdate::ReferencePrice { .. }));

        let _ = fs::remove_file(path);
    }
}
