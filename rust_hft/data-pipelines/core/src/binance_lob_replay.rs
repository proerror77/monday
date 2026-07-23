//! Venue-neutral ownership point for Binance L2 replay evidence.
//!
//! Both acquisition tooling and executable backtests consume this contract.  Keeping
//! the sequence validator here prevents a backtest from depending on a tool crate or
//! silently implementing a second interpretation of a collector tape.

use std::{collections::HashMap, str::FromStr};

use anyhow::Result;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Market {
    Spot,
    Usdm,
}

impl FromStr for Market {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "spot" => Ok(Self::Spot),
            "usdm" => Ok(Self::Usdm),
            other => Err(format!("unsupported MARKET={other}")),
        }
    }
}

impl Market {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Spot => "spot",
            Self::Usdm => "usdm",
        }
    }
}

/// Hash the ordered raw-segment digests that form a replay artifact.
pub fn source_revision<'a>(segment_hashes: impl IntoIterator<Item = &'a str>) -> String {
    let mut digest = Sha256::new();
    for hash in segment_hashes {
        digest.update(hash.as_bytes());
        digest.update(b"\n");
    }
    hex::encode(digest.finalize())
}

#[derive(Debug)]
pub struct ReplaySequenceValidator {
    market: Market,
    symbol: String,
    state: Option<ReplaySequenceState>,
    pending: Vec<ReplaySequenceDiff>,
}

#[derive(Debug)]
struct ReplaySequenceState {
    session_id: String,
    last_update_id: u64,
    bridged: bool,
    continuity_complete: bool,
    bids: HashMap<String, String>,
    asks: HashMap<String, String>,
}

#[derive(Debug)]
struct ReplaySequenceDiff {
    session_id: String,
    diff: ReplayDepthDiff,
    received_at_ns: u64,
}

#[derive(Debug, Deserialize)]
struct ReplayDepthDiff {
    #[serde(rename = "s")]
    symbol: String,
    #[serde(rename = "U")]
    first_update_id: u64,
    #[serde(rename = "u")]
    final_update_id: u64,
    #[serde(rename = "pu", default)]
    previous_update_id: Option<u64>,
    #[serde(rename = "b", default)]
    bids: Vec<[String; 2]>,
    #[serde(rename = "a", default)]
    asks: Vec<[String; 2]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplaySequenceEvent {
    Snapshot {
        received_at_ns: u64,
        bids: Vec<[String; 2]>,
        asks: Vec<[String; 2]>,
    },
    Diff {
        received_at_ns: u64,
        bids: Vec<[String; 2]>,
        asks: Vec<[String; 2]>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayBookSnapshot {
    pub last_update_id: u64,
    pub bids: Vec<[String; 2]>,
    pub asks: Vec<[String; 2]>,
}

impl ReplaySequenceValidator {
    pub fn new(market: Market, symbol: impl Into<String>) -> Result<Self> {
        let symbol = symbol.into();
        if symbol.is_empty() {
            anyhow::bail!("replay symbol is empty");
        }
        Ok(Self {
            market,
            symbol,
            state: None,
            pending: Vec::new(),
        })
    }

    pub fn observe(
        &mut self,
        event_type: &str,
        raw: &serde_json::Map<String, Value>,
        received_at_ns: u64,
    ) -> Result<Vec<ReplaySequenceEvent>> {
        self.observe_inner(event_type, raw, received_at_ns, false)
    }

    pub fn observe_verified_stream_coverage_checkpoint(
        &mut self,
        raw: &serde_json::Map<String, Value>,
        received_at_ns: u64,
    ) -> Result<Vec<ReplaySequenceEvent>> {
        if raw.get("stream_coverage_verified").and_then(Value::as_bool) != Some(true) {
            anyhow::bail!("checkpoint has no verified stream coverage");
        }
        self.observe_inner("checkpoint", raw, received_at_ns, true)
    }

    fn observe_inner(
        &mut self,
        event_type: &str,
        raw: &serde_json::Map<String, Value>,
        received_at_ns: u64,
        allow_verified_static_checkpoint: bool,
    ) -> Result<Vec<ReplaySequenceEvent>> {
        let mut events = Vec::new();
        match event_type {
            "snapshot" if required_string(raw, "symbol")? == self.symbol => {
                if self
                    .state
                    .as_ref()
                    .is_some_and(|state| !state.continuity_complete)
                {
                    anyhow::bail!("snapshot replaced an unbridged replay series");
                }
                let snapshot = raw
                    .get("snapshot")
                    .and_then(Value::as_object)
                    .ok_or_else(|| anyhow::anyhow!("snapshot event has no nested payload"))?;
                self.state = Some(ReplaySequenceState {
                    session_id: required_string(raw, "session_id")?.to_string(),
                    last_update_id: snapshot
                        .get("lastUpdateId")
                        .and_then(Value::as_u64)
                        .ok_or_else(|| anyhow::anyhow!("snapshot payload has no lastUpdateId"))?,
                    bridged: false,
                    continuity_complete: false,
                    bids: parse_snapshot_side(snapshot.get("bids"))?,
                    asks: parse_snapshot_side(snapshot.get("asks"))?,
                });
                events.push(ReplaySequenceEvent::Snapshot {
                    received_at_ns,
                    bids: parse_replay_levels(snapshot.get("bids"), "snapshot bids")?,
                    asks: parse_replay_levels(snapshot.get("asks"), "snapshot asks")?,
                });
                for pending in std::mem::take(&mut self.pending) {
                    let effective_time = received_at_ns.max(pending.received_at_ns);
                    if self.apply_diff(&pending)? {
                        events.push(ReplaySequenceEvent::Diff {
                            received_at_ns: effective_time,
                            bids: pending.diff.bids,
                            asks: pending.diff.asks,
                        });
                    }
                }
            }
            "diff" => {
                let frame = raw
                    .get("frame")
                    .ok_or_else(|| anyhow::anyhow!("diff event has no nested frame"))?;
                let diff = ReplayDepthDiff::from_frame(frame)?;
                if diff.symbol != self.symbol {
                    return Ok(events);
                }
                let diff = ReplaySequenceDiff {
                    session_id: required_string(raw, "session_id")?.to_string(),
                    diff,
                    received_at_ns,
                };
                if self.state.is_none() {
                    self.pending.push(diff);
                } else if self.state.as_ref().expect("checked state").session_id != diff.session_id
                {
                    if !self
                        .state
                        .as_ref()
                        .expect("checked state")
                        .continuity_complete
                    {
                        anyhow::bail!("diff replaced an unbridged replay series");
                    }
                    self.state = None;
                    self.pending = vec![diff];
                } else if self.apply_diff(&diff)? {
                    events.push(ReplaySequenceEvent::Diff {
                        received_at_ns,
                        bids: diff.diff.bids,
                        asks: diff.diff.asks,
                    });
                }
            }
            "checkpoint" if required_string(raw, "symbol")? == self.symbol => {
                let checkpoint = ReplaySequenceState {
                    session_id: required_string(raw, "session_id")?.to_string(),
                    last_update_id: raw
                        .get("last_update_id")
                        .and_then(Value::as_u64)
                        .ok_or_else(|| anyhow::anyhow!("checkpoint has no last update id"))?,
                    bridged: raw
                        .get("bridged")
                        .and_then(Value::as_bool)
                        .ok_or_else(|| anyhow::anyhow!("checkpoint has no bridged state"))?,
                    continuity_complete: raw
                        .get("continuity_complete")
                        .and_then(Value::as_bool)
                        .unwrap_or_else(|| {
                            raw.get("bridged").and_then(Value::as_bool) == Some(true)
                        }),
                    bids: parse_snapshot_side(raw.get("bids"))?,
                    asks: parse_snapshot_side(raw.get("asks"))?,
                };
                match self.state.as_ref() {
                    None if !self.pending.is_empty() => {
                        anyhow::bail!("diffs arrived before replay seed")
                    }
                    None => {
                        self.state = Some(checkpoint);
                        events.push(replay_checkpoint_seed(raw, received_at_ns)?);
                    }
                    Some(state) if state.session_id != checkpoint.session_id => {
                        if !state.continuity_complete {
                            anyhow::bail!("checkpoint replaced an unbridged replay series");
                        }
                        self.state = Some(checkpoint);
                        events.push(replay_checkpoint_seed(raw, received_at_ns)?);
                    }
                    Some(state)
                        if allow_verified_static_checkpoint
                            && !state.bridged
                            && !checkpoint.bridged
                            && checkpoint.continuity_complete
                            && raw.get("stream_coverage_verified").and_then(Value::as_bool)
                                == Some(true)
                            && state.last_update_id == checkpoint.last_update_id
                            && state.bids == checkpoint.bids
                            && state.asks == checkpoint.asks =>
                    {
                        self.state = Some(checkpoint);
                    }
                    Some(state)
                        if state.last_update_id != checkpoint.last_update_id
                            || state.bridged != checkpoint.bridged
                            || state.continuity_complete != checkpoint.continuity_complete
                            || state.bids != checkpoint.bids
                            || state.asks != checkpoint.asks =>
                    {
                        anyhow::bail!("checkpoint does not match replayed update state")
                    }
                    Some(_) => {}
                }
            }
            _ => {}
        }
        Ok(events)
    }

    fn apply_diff(&mut self, update: &ReplaySequenceDiff) -> Result<bool> {
        let state = self
            .state
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("diff has no replay seed"))?;
        if update.session_id != state.session_id {
            anyhow::bail!("diff session does not match replay state");
        }
        if update.diff.final_update_id <= state.last_update_id {
            return Ok(false);
        }
        let expected = state
            .last_update_id
            .checked_add(u64::from(self.market == Market::Spot))
            .ok_or_else(|| anyhow::anyhow!("update id overflow"))?;
        let accepted = if self.market == Market::Usdm {
            update.diff.previous_update_id == Some(state.last_update_id)
                || !state.bridged
                    && update.diff.first_update_id <= expected
                    && expected <= update.diff.final_update_id
        } else {
            update.diff.first_update_id <= expected && expected <= update.diff.final_update_id
        };
        if !accepted {
            anyhow::bail!(
                "Binance sequence gap: expected {expected}, received {}-{}",
                update.diff.first_update_id,
                update.diff.final_update_id
            );
        }
        state.last_update_id = update.diff.final_update_id;
        state.bridged = true;
        state.continuity_complete = true;
        update_side(&mut state.bids, &update.diff.bids);
        update_side(&mut state.asks, &update.diff.asks);
        Ok(true)
    }

    pub fn finish(&self) -> Result<()> {
        if !self.pending.is_empty()
            || self.state.is_none()
            || self
                .state
                .as_ref()
                .is_some_and(|state| !state.continuity_complete)
        {
            anyhow::bail!("collector replay did not finish in a bridged state");
        }
        Ok(())
    }

    pub fn book_snapshot(&self) -> Result<ReplayBookSnapshot> {
        let state = self
            .state
            .as_ref()
            .filter(|state| state.continuity_complete)
            .ok_or_else(|| anyhow::anyhow!("collector replay has no bridged book snapshot"))?;
        Ok(ReplayBookSnapshot {
            last_update_id: state.last_update_id,
            bids: sorted_levels(&state.bids, true)?,
            asks: sorted_levels(&state.asks, false)?,
        })
    }
}

impl ReplayDepthDiff {
    fn from_frame(frame: &Value) -> Result<Self> {
        let data = frame.get("data").unwrap_or(frame);
        let mut diff: Self = serde_json::from_value(data.clone())?;
        if diff.symbol.is_empty() {
            anyhow::bail!("depth frame missing symbol");
        }
        if diff.first_update_id > diff.final_update_id {
            anyhow::bail!(
                "invalid depth sequence {}-{}",
                diff.first_update_id,
                diff.final_update_id
            );
        }
        diff.symbol.make_ascii_uppercase();
        validate_levels(&diff.bids)?;
        validate_levels(&diff.asks)?;
        Ok(diff)
    }
}

fn replay_checkpoint_seed(
    raw: &serde_json::Map<String, Value>,
    received_at_ns: u64,
) -> Result<ReplaySequenceEvent> {
    Ok(ReplaySequenceEvent::Snapshot {
        received_at_ns,
        bids: parse_replay_levels(raw.get("bids"), "checkpoint bids")?,
        asks: parse_replay_levels(raw.get("asks"), "checkpoint asks")?,
    })
}

fn parse_replay_levels(value: Option<&Value>, field: &str) -> Result<Vec<[String; 2]>> {
    serde_json::from_value(
        value
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{field} are missing"))?,
    )
    .map_err(Into::into)
}

fn required_string<'a>(object: &'a serde_json::Map<String, Value>, field: &str) -> Result<&'a str> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("collector payload is missing {field}"))
}

fn parse_snapshot_side(value: Option<&Value>) -> Result<HashMap<String, String>> {
    let levels = parse_replay_levels(value, "snapshot levels")?;
    validate_levels(&levels)?;
    Ok(levels
        .into_iter()
        .map(|[price, quantity]| (price, quantity))
        .collect())
}

fn validate_levels(levels: &[[String; 2]]) -> Result<()> {
    for [price, quantity] in levels {
        Decimal::from_str(price)?;
        Decimal::from_str(quantity)?;
    }
    Ok(())
}

fn update_side(side: &mut HashMap<String, String>, levels: &[[String; 2]]) {
    for [price, quantity] in levels {
        if quantity
            .parse::<Decimal>()
            .is_ok_and(|value| value.is_zero())
        {
            side.remove(price);
        } else {
            side.insert(price.clone(), quantity.clone());
        }
    }
}

fn sorted_levels(side: &HashMap<String, String>, descending: bool) -> Result<Vec<[String; 2]>> {
    let mut levels = side
        .iter()
        .map(|(price, quantity)| Ok((Decimal::from_str(price)?, [price.clone(), quantity.clone()])))
        .collect::<Result<Vec<_>>>()?;
    levels.sort_by(|left, right| {
        let price_order = if descending {
            right.0.cmp(&left.0)
        } else {
            left.0.cmp(&right.0)
        };
        price_order.then_with(|| left.1[0].cmp(&right.1[0]))
    });
    Ok(levels.into_iter().map(|(_, level)| level).collect())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Market, ReplayBookSnapshot, ReplaySequenceEvent, ReplaySequenceValidator};

    #[test]
    fn replay_requires_a_bridged_sequence_before_completion() {
        let mut replay = ReplaySequenceValidator::new(Market::Spot, "BTCUSDT").unwrap();
        let snapshot = json!({
            "symbol": "BTCUSDT",
            "session_id": "session-1",
            "snapshot": {
                "lastUpdateId": 100,
                "bids": [["100", "1"]],
                "asks": [["101", "1"]]
            }
        });
        let emitted = replay
            .observe("snapshot", snapshot.as_object().unwrap(), 100)
            .unwrap();
        assert!(matches!(
            emitted.as_slice(),
            [ReplaySequenceEvent::Snapshot { .. }]
        ));
        assert!(replay.finish().is_err());

        let diff = json!({
            "session_id": "session-1",
            "frame": {
                "data": {
                    "s": "BTCUSDT",
                    "U": 101,
                    "u": 101,
                    "b": [["100", "0"]],
                    "a": [["101", "2"]]
                }
            }
        });
        let emitted = replay
            .observe("diff", diff.as_object().unwrap(), 101)
            .unwrap();
        assert!(matches!(
            emitted.as_slice(),
            [ReplaySequenceEvent::Diff { .. }]
        ));
        replay.finish().unwrap();
    }

    #[test]
    fn replay_book_snapshot_requires_a_bridged_sequence() {
        let mut replay = ReplaySequenceValidator::new(Market::Spot, "BTCUSDT").unwrap();
        let snapshot = json!({
            "symbol": "BTCUSDT",
            "session_id": "session-1",
            "snapshot": {
                "lastUpdateId": 100,
                "bids": [["100", "1"]],
                "asks": [["101", "1"]]
            }
        });
        replay
            .observe("snapshot", snapshot.as_object().unwrap(), 100)
            .unwrap();

        assert!(replay.book_snapshot().is_err());
    }

    #[test]
    fn verified_stream_coverage_bridges_an_unchanged_snapshot() {
        let mut replay = ReplaySequenceValidator::new(Market::Spot, "BTCUSDT").unwrap();
        let snapshot = json!({
            "symbol": "BTCUSDT",
            "session_id": "session-1",
            "snapshot": {
                "lastUpdateId": 100,
                "bids": [["100", "1"]],
                "asks": [["101", "1"]]
            }
        });
        replay
            .observe("snapshot", snapshot.as_object().unwrap(), 100)
            .unwrap();
        let checkpoint = json!({
            "symbol": "BTCUSDT",
            "session_id": "session-1",
            "last_update_id": 100,
            "bridged": false,
            "continuity_complete": true,
            "stream_coverage_verified": true,
            "bids": [["100", "1"]],
            "asks": [["101", "1"]]
        });

        replay
            .observe_verified_stream_coverage_checkpoint(checkpoint.as_object().unwrap(), 101)
            .unwrap();
        replay.finish().unwrap();
    }

    #[test]
    fn generic_replay_does_not_adopt_collector_stream_coverage_semantics() {
        let mut replay = ReplaySequenceValidator::new(Market::Spot, "BTCUSDT").unwrap();
        let snapshot = json!({
            "symbol": "BTCUSDT",
            "session_id": "session-1",
            "snapshot": {
                "lastUpdateId": 100,
                "bids": [["100", "1"]],
                "asks": [["101", "1"]]
            }
        });
        replay
            .observe("snapshot", snapshot.as_object().unwrap(), 100)
            .unwrap();
        let checkpoint = json!({
            "symbol": "BTCUSDT",
            "session_id": "session-1",
            "last_update_id": 100,
            "bridged": false,
            "continuity_complete": true,
            "stream_coverage_verified": true,
            "bids": [["100", "1"]],
            "asks": [["101", "1"]]
        });

        let error = replay
            .observe("checkpoint", checkpoint.as_object().unwrap(), 101)
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("checkpoint does not match replayed update state"));
    }

    #[test]
    fn replay_book_snapshot_is_full_and_price_sorted() {
        let mut replay = ReplaySequenceValidator::new(Market::Spot, "BTCUSDT").unwrap();
        let snapshot = json!({
            "symbol": "BTCUSDT",
            "session_id": "session-1",
            "snapshot": {
                "lastUpdateId": 100,
                "bids": [["99", "1"], ["100", "2"], ["98", "3"]],
                "asks": [["103", "3"], ["101", "1"], ["102", "2"]]
            }
        });
        replay
            .observe("snapshot", snapshot.as_object().unwrap(), 100)
            .unwrap();
        let diff = json!({
            "session_id": "session-1",
            "frame": {
                "data": {
                    "s": "BTCUSDT",
                    "U": 101,
                    "u": 101,
                    "b": [["99", "0"], ["101", "4"]],
                    "a": [["102", "5"], ["104", "6"]]
                }
            }
        });
        replay
            .observe("diff", diff.as_object().unwrap(), 101)
            .unwrap();

        assert_eq!(
            replay.book_snapshot().unwrap(),
            ReplayBookSnapshot {
                last_update_id: 101,
                bids: vec![
                    ["101".into(), "4".into()],
                    ["100".into(), "2".into()],
                    ["98".into(), "3".into()],
                ],
                asks: vec![
                    ["101".into(), "1".into()],
                    ["102".into(), "5".into()],
                    ["103".into(), "3".into()],
                    ["104".into(), "6".into()],
                ],
            }
        );
    }
}
