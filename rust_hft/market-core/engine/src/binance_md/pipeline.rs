use super::book::{CorrectnessBook, TopBook};
use super::book_sync::{BookSync, BufferedApplyResult, SequenceDecision, UpdateMeta};
use super::features::{compute_features, FeatureView};
use super::latency::LatencyTrace;
use super::parser::{parse_depth_update, ParseDepthError, ParsedDepthUpdate};
use super::raw::RawFrameRef;
use super::replay::{ReplayBatch, ReplayKind, ReplayPayload, ReplayRecord};
use super::signal::{Signal, SignalRules};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessOutcome {
    pub decision: SequenceDecision,
    pub feature: Option<FeatureView>,
    pub signal: Option<Signal>,
    pub latency: LatencyTrace,
    pub replay: ReplayBatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeOutcome {
    pub result: BufferedApplyResult,
    pub replay: ReplayBatch,
}

/// Narrow Binance local hot-path runner.
///
/// Tokio/websocket, REST snapshot fetch, persistence, metrics export, and order
/// execution stay outside this type. This runner only owns sequence state,
/// TopN book state, feature computation, and signal rules.
#[derive(Debug, Clone)]
pub struct MarketDataLane<const N: usize> {
    symbol_id: u32,
    book: CorrectnessBook<N>,
    sync: BookSync,
    buffered_depth: Vec<ParsedDepthUpdate>,
    signal_rules: SignalRules,
    flow_1s: i64,
}

impl<const N: usize> MarketDataLane<N> {
    pub fn new(symbol_id: u32, signal_rules: SignalRules) -> Self {
        Self {
            symbol_id,
            book: CorrectnessBook::new(),
            sync: BookSync::new(),
            buffered_depth: Vec::new(),
            signal_rules,
            flow_1s: 0,
        }
    }

    #[inline]
    pub fn book(&self) -> &TopBook<N> {
        self.book.top()
    }

    #[inline]
    pub fn sync(&self) -> &BookSync {
        &self.sync
    }

    pub fn start_buffering(&mut self) {
        self.sync.start_buffering();
        self.buffered_depth.clear();
    }

    pub fn start_snapshot_fetch(&mut self) {
        self.sync.start_snapshot_fetch();
    }

    pub fn buffer_depth_update(&mut self, update: ParsedDepthUpdate) {
        self.sync.buffer_diff(UpdateMeta::new(
            update.first_update_id,
            update.final_update_id,
        ));
        self.buffered_depth.push(update);
    }

    pub fn apply_snapshot_bridge(
        &mut self,
        snapshot_last_update_id: u64,
        bids: &[(i64, i64)],
        asks: &[(i64, i64)],
        ts_ns: i64,
    ) -> BridgeOutcome {
        self.book
            .apply_snapshot(bids, asks, snapshot_last_update_id, ts_ns);

        let result = self.sync.apply_snapshot(snapshot_last_update_id);
        if result.gap.is_none() {
            for decision in &result.decisions {
                if decision.decision != SequenceDecision::Apply {
                    continue;
                }
                if let Some(update) = self.buffered_depth.get(decision.index) {
                    self.book.apply_diff(
                        &update.bids,
                        &update.asks,
                        update.final_update_id,
                        update.exchange_ts_ns,
                    );
                }
            }
        }

        self.buffered_depth.clear();
        let snapshot_record = self.snapshot_replay_record(
            ReplayKind::Snapshot,
            snapshot_last_update_id,
            ts_ns,
            ReplayPayload::Snapshot {
                bids: bids.to_vec(),
                asks: asks.to_vec(),
            },
        );
        let bridge_record = self.snapshot_replay_record(
            ReplayKind::Bridge,
            snapshot_last_update_id,
            ts_ns,
            ReplayPayload::Bridge {
                result: result.clone(),
            },
        );

        BridgeOutcome {
            result,
            replay: ReplayBatch {
                snapshot: Some(snapshot_record),
                bridge: Some(bridge_record),
                ..ReplayBatch::default()
            },
        }
    }

    pub fn process_raw_depth_frame(
        &mut self,
        frame: RawFrameRef<'_>,
        mut latency: LatencyTrace,
    ) -> Result<ProcessOutcome, ParseDepthError> {
        let update = parse_depth_update(frame.bytes, self.symbol_id, frame.recv_ts_ns)?;
        latency.recv_ns = frame.recv_ts_ns;
        let mut outcome = self.process_depth_update(&update, latency)?;
        outcome.replay.raw = Some(self.replay_record(
            ReplayKind::RawFrame,
            &update,
            outcome.latency,
            ReplayPayload::RawFrame {
                bytes: frame.bytes.to_vec(),
            },
        ));
        Ok(outcome)
    }

    pub fn process_depth_update(
        &mut self,
        update: &ParsedDepthUpdate,
        latency: LatencyTrace,
    ) -> Result<ProcessOutcome, ParseDepthError> {
        let meta = UpdateMeta::new(update.first_update_id, update.final_update_id);
        let decision = self.sync.classify_live_update(meta);

        if decision != SequenceDecision::Apply {
            return Ok(ProcessOutcome {
                decision,
                feature: None,
                signal: None,
                latency,
                replay: ReplayBatch {
                    depth: Some(self.replay_record(
                        ReplayKind::DepthUpdate,
                        update,
                        latency,
                        ReplayPayload::DepthUpdate {
                            decision,
                            bids: update.bids.clone(),
                            asks: update.asks.clone(),
                        },
                    )),
                    ..ReplayBatch::default()
                },
            });
        }

        self.book.apply_diff(
            &update.bids,
            &update.asks,
            update.final_update_id,
            update.exchange_ts_ns,
        );

        let feature = compute_features(
            update.symbol_id,
            self.book.top(),
            self.flow_1s,
            update.receive_ts_ns,
        );
        let signal = feature
            .as_ref()
            .and_then(|view| self.signal_rules.evaluate(&view.snapshot));

        let mut replay = ReplayBatch {
            depth: Some(self.replay_record(
                ReplayKind::DepthUpdate,
                update,
                latency,
                ReplayPayload::DepthUpdate {
                    decision,
                    bids: update.bids.clone(),
                    asks: update.asks.clone(),
                },
            )),
            ..ReplayBatch::default()
        };

        if let Some(view) = feature {
            replay.feature = Some(self.replay_record(
                ReplayKind::Feature,
                update,
                latency,
                ReplayPayload::Feature(view.snapshot),
            ));
        }
        if let Some(signal) = signal {
            replay.signal = Some(self.replay_record(
                ReplayKind::Signal,
                update,
                latency,
                ReplayPayload::Signal(signal),
            ));
        }

        Ok(ProcessOutcome {
            decision,
            feature,
            signal,
            latency,
            replay,
        })
    }

    fn replay_record(
        &self,
        kind: ReplayKind,
        update: &ParsedDepthUpdate,
        latency: LatencyTrace,
        payload: ReplayPayload,
    ) -> ReplayRecord {
        ReplayRecord {
            kind,
            symbol_id: update.symbol_id,
            exchange_ts_ns: update.exchange_ts_ns,
            receive_ts_ns: update.receive_ts_ns,
            first_update_id: update.first_update_id,
            final_update_id: update.final_update_id,
            sync_state: self.sync.state(),
            last_book_update_id: self.sync.last_update_id(),
            latency,
            payload,
        }
    }

    fn snapshot_replay_record(
        &self,
        kind: ReplayKind,
        snapshot_last_update_id: u64,
        ts_ns: i64,
        payload: ReplayPayload,
    ) -> ReplayRecord {
        ReplayRecord {
            kind,
            symbol_id: self.symbol_id,
            exchange_ts_ns: ts_ns,
            receive_ts_ns: ts_ns,
            first_update_id: snapshot_last_update_id,
            final_update_id: snapshot_last_update_id,
            sync_state: self.sync.state(),
            last_book_update_id: self.sync.last_update_id(),
            latency: LatencyTrace::default(),
            payload,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binance_md::SignalSide;

    fn bridge_to_update_id_99<const N: usize>(lane: &mut MarketDataLane<N>) {
        lane.start_buffering();
        lane.buffer_depth_update(ParsedDepthUpdate {
            symbol_id: 1,
            exchange_ts_ns: 99,
            receive_ts_ns: 99,
            first_update_id: 99,
            final_update_id: 99,
            bids: vec![],
            asks: vec![],
        });
        let bridge = lane.apply_snapshot_bridge(
            98,
            &[(100_000_000, 100_000_000)],
            &[(102_000_000, 100_000_000)],
            1,
        );
        assert_eq!(bridge.result.applied, 1);
        assert_eq!(bridge.result.gap, None);
        assert!(bridge.replay.snapshot.is_some());
        assert!(bridge.replay.bridge.is_some());
    }

    #[test]
    fn raw_frame_to_expiring_signal_after_snapshot_bridge() {
        let mut lane = MarketDataLane::<5>::new(
            1,
            SignalRules {
                max_spread: 3_000_000,
                ttl_ns: 1_000,
                ..SignalRules::default()
            },
        );
        bridge_to_update_id_99(&mut lane);

        let raw = br#"{
            "e":"depthUpdate",
            "E":1700000000123,
            "s":"BTCUSDT",
            "U":100,
            "u":100,
            "b":[["100.000000","0.000200"]],
            "a":[["102.000000","0.000050"]]
        }"#;

        let outcome = lane
            .process_raw_depth_frame(
                RawFrameRef {
                    recv_ts_ns: 5,
                    bytes: raw,
                },
                LatencyTrace {
                    recv_ns: 0,
                    parse_done_ns: 10,
                    book_done_ns: 15,
                    feature_done_ns: 20,
                    signal_done_ns: 25,
                },
            )
            .unwrap();

        assert_eq!(outcome.decision, SequenceDecision::Apply);
        assert_eq!(lane.book().last_update_id, 100);
        assert!(outcome.feature.is_some());
        let signal = outcome.signal.unwrap();
        assert_eq!(signal.side, SignalSide::Long);
        assert_eq!(signal.ts_ns, 5);
        assert!(outcome.replay.raw.is_some());
        assert!(outcome.replay.depth.is_some());
        assert!(outcome.replay.feature.is_some());
        assert!(outcome.replay.signal.is_some());
    }

    #[test]
    fn sequence_gap_does_not_mutate_book() {
        let mut lane = MarketDataLane::<5>::new(1, SignalRules::default());
        lane.start_buffering();
        lane.buffer_depth_update(ParsedDepthUpdate {
            symbol_id: 1,
            exchange_ts_ns: 99,
            receive_ts_ns: 99,
            first_update_id: 99,
            final_update_id: 99,
            bids: vec![],
            asks: vec![],
        });
        let bridge = lane.apply_snapshot_bridge(98, &[(100, 1)], &[(101, 1)], 1);
        assert_eq!(bridge.result.gap, None);

        let update = ParsedDepthUpdate {
            symbol_id: 1,
            exchange_ts_ns: 2,
            receive_ts_ns: 3,
            first_update_id: 101,
            final_update_id: 101,
            bids: vec![(105, 10)],
            asks: vec![],
        };

        let outcome = lane
            .process_depth_update(&update, LatencyTrace::default())
            .unwrap();

        assert_eq!(
            outcome.decision,
            SequenceDecision::Gap {
                expected: 100,
                first_seen: 101
            }
        );
        assert_eq!(lane.book().best_bid().unwrap().price, 100);
    }

    #[test]
    fn snapshot_bridge_applies_buffered_depth_updates() {
        let mut lane = MarketDataLane::<3>::new(1, SignalRules::default());
        lane.start_buffering();
        lane.buffer_depth_update(ParsedDepthUpdate {
            symbol_id: 1,
            exchange_ts_ns: 90,
            receive_ts_ns: 90,
            first_update_id: 90,
            final_update_id: 99,
            bids: vec![(98, 1)],
            asks: vec![],
        });
        lane.buffer_depth_update(ParsedDepthUpdate {
            symbol_id: 1,
            exchange_ts_ns: 100,
            receive_ts_ns: 100,
            first_update_id: 95,
            final_update_id: 100,
            bids: vec![(100, 10)],
            asks: vec![],
        });
        lane.buffer_depth_update(ParsedDepthUpdate {
            symbol_id: 1,
            exchange_ts_ns: 101,
            receive_ts_ns: 101,
            first_update_id: 101,
            final_update_id: 101,
            bids: vec![(101, 11)],
            asks: vec![],
        });

        let outcome = lane.apply_snapshot_bridge(99, &[(99, 9)], &[(102, 1)], 99);
        let result = outcome.result;

        assert_eq!(result.applied, 2);
        assert_eq!(result.ignored_stale, 1);
        assert_eq!(result.gap, None);
        assert!(outcome.replay.snapshot.is_some());
        assert!(outcome.replay.bridge.is_some());
        assert_eq!(lane.sync().last_update_id(), 101);
        assert_eq!(lane.book().best_bid().unwrap().price, 101);
    }

    #[test]
    fn snapshot_bridge_applies_exact_decision_indices() {
        let mut lane = MarketDataLane::<3>::new(1, SignalRules::default());
        lane.start_buffering();
        lane.buffer_depth_update(ParsedDepthUpdate {
            symbol_id: 1,
            exchange_ts_ns: 100,
            receive_ts_ns: 100,
            first_update_id: 95,
            final_update_id: 100,
            bids: vec![(100, 10)],
            asks: vec![],
        });
        lane.buffer_depth_update(ParsedDepthUpdate {
            symbol_id: 1,
            exchange_ts_ns: 100,
            receive_ts_ns: 100,
            first_update_id: 96,
            final_update_id: 100,
            bids: vec![(105, 10)],
            asks: vec![],
        });
        lane.buffer_depth_update(ParsedDepthUpdate {
            symbol_id: 1,
            exchange_ts_ns: 101,
            receive_ts_ns: 101,
            first_update_id: 101,
            final_update_id: 101,
            bids: vec![(101, 11)],
            asks: vec![],
        });

        let outcome = lane.apply_snapshot_bridge(99, &[(99, 9)], &[(102, 1)], 99);

        assert_eq!(outcome.result.applied, 2);
        assert_eq!(outcome.result.ignored_stale, 1);
        assert_eq!(
            outcome
                .result
                .decisions
                .iter()
                .map(|decision| (decision.index, decision.decision))
                .collect::<Vec<_>>(),
            vec![
                (0, SequenceDecision::Apply),
                (1, SequenceDecision::IgnoreStale),
                (2, SequenceDecision::Apply),
            ]
        );
        assert_eq!(lane.sync().last_update_id(), 101);
        assert_eq!(lane.book().best_bid().unwrap().price, 101);
    }
}
