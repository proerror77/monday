use super::book::{CorrectnessBook, TopBook};
use super::book_sync::{BookSync, BufferedApplyResult, SequenceDecision, UpdateMeta};
use super::features::{compute_features, FeatureView};
use super::latency::LatencyTrace;
use super::parser::{parse_depth_update, ParseDepthError, ParsedDepthUpdate};
use super::raw::RawFrameRef;
use super::replay::{ReplayBatch, ReplayBridgeUpdate, ReplayKind, ReplayPayload, ReplayRecord};
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
        let mut applied_updates = Vec::new();
        if result.gap.is_none() {
            for decision in &result.decisions {
                if decision.decision != SequenceDecision::Apply {
                    continue;
                }
                if let Some(update) = self.buffered_depth.get(decision.index) {
                    applied_updates.push(ReplayBridgeUpdate {
                        symbol_id: update.symbol_id,
                        exchange_ts_ns: update.exchange_ts_ns,
                        receive_ts_ns: update.receive_ts_ns,
                        first_update_id: update.first_update_id,
                        final_update_id: update.final_update_id,
                        bids: update.bids.clone(),
                        asks: update.asks.clone(),
                    });
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
                applied_updates,
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

    pub fn load_snapshot_for_replay(
        &mut self,
        snapshot_last_update_id: u64,
        bids: &[(i64, i64)],
        asks: &[(i64, i64)],
        ts_ns: i64,
    ) {
        self.book
            .apply_snapshot(bids, asks, snapshot_last_update_id, ts_ns);
        self.sync.load_snapshot_for_replay(snapshot_last_update_id);
    }

    pub fn apply_replay_snapshot(
        &mut self,
        snapshot_last_update_id: u64,
        bids: &[(i64, i64)],
        asks: &[(i64, i64)],
        ts_ns: i64,
    ) -> ReplayBatch {
        self.load_snapshot_for_replay(snapshot_last_update_id, bids, asks, ts_ns);
        ReplayBatch {
            snapshot: Some(self.snapshot_replay_record(
                ReplayKind::Snapshot,
                snapshot_last_update_id,
                ts_ns,
                ReplayPayload::Snapshot {
                    bids: bids.to_vec(),
                    asks: asks.to_vec(),
                },
            )),
            ..ReplayBatch::default()
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

    pub fn process_raw_depth_frame_with_clock<C>(
        &mut self,
        frame: RawFrameRef<'_>,
        mut clock: C,
    ) -> Result<ProcessOutcome, ParseDepthError>
    where
        C: FnMut() -> i64,
    {
        let update = parse_depth_update(frame.bytes, self.symbol_id, frame.recv_ts_ns)?;
        let mut latency = LatencyTrace {
            recv_ns: frame.recv_ts_ns,
            ..LatencyTrace::default()
        };
        latency.parse_done_ns = clock();

        let mut outcome = self.process_depth_update_with_clock(&update, latency, &mut clock)?;
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
        self.process_depth_update_inner(update, latency, None)
    }

    pub fn process_depth_update_with_clock<C>(
        &mut self,
        update: &ParsedDepthUpdate,
        latency: LatencyTrace,
        clock: &mut C,
    ) -> Result<ProcessOutcome, ParseDepthError>
    where
        C: FnMut() -> i64,
    {
        self.process_depth_update_inner(update, latency, Some(clock))
    }

    pub fn apply_bridge_update_for_replay(
        &mut self,
        update: &ParsedDepthUpdate,
    ) -> SequenceDecision {
        let meta = UpdateMeta::new(update.first_update_id, update.final_update_id);
        let decision = self.sync.classify_live_update(meta);
        if decision == SequenceDecision::Apply {
            self.book.apply_diff(
                &update.bids,
                &update.asks,
                update.final_update_id,
                update.exchange_ts_ns,
            );
        }
        decision
    }

    fn process_depth_update_inner(
        &mut self,
        update: &ParsedDepthUpdate,
        mut latency: LatencyTrace,
        mut clock: Option<&mut dyn FnMut() -> i64>,
    ) -> Result<ProcessOutcome, ParseDepthError> {
        let meta = UpdateMeta::new(update.first_update_id, update.final_update_id);
        let decision = self.sync.classify_live_update(meta);

        if decision != SequenceDecision::Apply {
            if let Some(done_ns) = stamp(&mut clock) {
                latency.book_done_ns = done_ns;
                latency.feature_done_ns = done_ns;
                latency.signal_done_ns = done_ns;
            }

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

        if let Some(book_done_ns) = stamp(&mut clock) {
            latency.book_done_ns = book_done_ns;
        }

        let feature = compute_features(
            update.symbol_id,
            self.book.top(),
            self.flow_1s,
            update.receive_ts_ns,
        );
        if let Some(feature_done_ns) = stamp(&mut clock) {
            latency.feature_done_ns = feature_done_ns;
        }

        let signal = feature
            .as_ref()
            .and_then(|view| self.signal_rules.evaluate(&view.snapshot));
        if let Some(signal_done_ns) = stamp(&mut clock) {
            latency.signal_done_ns = signal_done_ns;
        }

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

fn stamp(clock: &mut Option<&mut dyn FnMut() -> i64>) -> Option<i64> {
    clock.as_mut().map(|clock| clock())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binance_md::{BookSyncState, SignalSide};

    const BTCUSDT_ID: u32 = 1;

    fn depth_update(
        first_update_id: u64,
        final_update_id: u64,
        bids: Vec<(i64, i64)>,
        asks: Vec<(i64, i64)>,
    ) -> ParsedDepthUpdate {
        ParsedDepthUpdate {
            symbol_id: BTCUSDT_ID,
            exchange_ts_ns: final_update_id as i64,
            receive_ts_ns: final_update_id as i64,
            first_update_id,
            final_update_id,
            bids,
            asks,
        }
    }

    fn bridge_to_update_id_99<const N: usize>(lane: &mut MarketDataLane<N>) {
        lane.start_buffering();
        lane.buffer_depth_update(depth_update(99, 99, vec![], vec![]));
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
            BTCUSDT_ID,
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
        let feature = outcome.feature.unwrap();
        assert_eq!(feature.book_update_id, 100);
        assert_eq!(feature.snapshot.symbol_id, BTCUSDT_ID);
        assert_eq!(feature.snapshot.spread, 2_000_000);
        assert_eq!(feature.snapshot.obi1_fp, 600_000);
        assert_eq!(feature.snapshot.obi3_fp, 600_000);
        assert_eq!(feature.snapshot.microgap, 600_000);
        let signal = outcome.signal.unwrap();
        assert_eq!(signal.side, SignalSide::Long);
        assert_eq!(signal.ts_ns, 5);
        assert_eq!(signal.expire_ts_ns, 1_005);

        let raw_record = outcome.replay.raw.unwrap();
        assert_eq!(raw_record.kind, ReplayKind::RawFrame);
        assert_eq!(raw_record.symbol_id, BTCUSDT_ID);
        assert_eq!(raw_record.first_update_id, 100);
        assert_eq!(raw_record.final_update_id, 100);
        assert_eq!(raw_record.sync_state, BookSyncState::Live);
        assert_eq!(raw_record.last_book_update_id, 100);
        assert_eq!(raw_record.latency.recv_ns, 5);
        assert!(matches!(
            raw_record.payload,
            ReplayPayload::RawFrame { bytes } if bytes == raw
        ));

        let depth_record = outcome.replay.depth.unwrap();
        assert_eq!(depth_record.kind, ReplayKind::DepthUpdate);
        assert_eq!(depth_record.final_update_id, 100);
        assert!(matches!(
            depth_record.payload,
            ReplayPayload::DepthUpdate {
                decision: SequenceDecision::Apply,
                bids,
                asks,
            } if bids == vec![(100_000_000, 200)] && asks == vec![(102_000_000, 50)]
        ));

        assert!(matches!(
            outcome.replay.feature.unwrap().payload,
            ReplayPayload::Feature(snapshot) if snapshot == feature.snapshot
        ));
        assert!(matches!(
            outcome.replay.signal.unwrap().payload,
            ReplayPayload::Signal(replayed_signal) if replayed_signal == signal
        ));
    }

    #[test]
    fn raw_frame_with_clock_stamps_pipeline_stages() {
        let mut lane = MarketDataLane::<5>::new(
            BTCUSDT_ID,
            SignalRules {
                max_spread: 3_000_000,
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
        let mut ticks = [10, 15, 20, 25].into_iter();

        let outcome = lane
            .process_raw_depth_frame_with_clock(
                RawFrameRef {
                    recv_ts_ns: 5,
                    bytes: raw,
                },
                || ticks.next().unwrap(),
            )
            .unwrap();

        assert_eq!(
            outcome.latency,
            LatencyTrace {
                recv_ns: 5,
                parse_done_ns: 10,
                book_done_ns: 15,
                feature_done_ns: 20,
                signal_done_ns: 25,
            }
        );
        assert_eq!(outcome.latency.parse_latency_ns(), 5);
        assert_eq!(outcome.latency.book_latency_ns(), 5);
        assert_eq!(outcome.latency.feature_latency_ns(), 5);
        assert_eq!(outcome.latency.signal_latency_ns(), 5);
        assert_eq!(outcome.latency.total_latency_ns(), 20);
        assert_eq!(outcome.replay.raw.unwrap().latency, outcome.latency);
        assert_eq!(outcome.replay.depth.unwrap().latency, outcome.latency);
        assert_eq!(outcome.replay.feature.unwrap().latency, outcome.latency);
        assert_eq!(outcome.replay.signal.unwrap().latency, outcome.latency);
    }

    #[test]
    fn sequence_gap_does_not_mutate_book() {
        let mut lane = MarketDataLane::<5>::new(BTCUSDT_ID, SignalRules::default());
        bridge_to_update_id_99(&mut lane);

        let update = depth_update(101, 101, vec![(105_000_000, 10)], vec![]);

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
        assert_eq!(lane.book().best_bid().unwrap().price, 100_000_000);
        assert_eq!(lane.sync().state(), BookSyncState::RebuildRequired);
        assert!(outcome.feature.is_none());
        assert!(outcome.signal.is_none());
        assert!(outcome.replay.raw.is_none());
        assert!(outcome.replay.feature.is_none());
        assert!(outcome.replay.signal.is_none());

        let depth_record = outcome.replay.depth.unwrap();
        assert_eq!(depth_record.kind, ReplayKind::DepthUpdate);
        assert_eq!(depth_record.sync_state, BookSyncState::RebuildRequired);
        assert_eq!(depth_record.last_book_update_id, 99);
        assert!(matches!(
            depth_record.payload,
            ReplayPayload::DepthUpdate {
                decision: SequenceDecision::Gap {
                    expected: 100,
                    first_seen: 101,
                },
                bids,
                asks,
            } if bids == vec![(105_000_000, 10)] && asks.is_empty()
        ));
    }

    #[test]
    fn snapshot_bridge_applies_buffered_depth_updates() {
        let mut lane = MarketDataLane::<3>::new(BTCUSDT_ID, SignalRules::default());
        lane.start_buffering();
        lane.buffer_depth_update(depth_update(90, 99, vec![(98, 1)], vec![]));
        lane.buffer_depth_update(depth_update(95, 100, vec![(100, 10)], vec![]));
        lane.buffer_depth_update(depth_update(101, 101, vec![(101, 11)], vec![]));

        let outcome = lane.apply_snapshot_bridge(99, &[(99, 9)], &[(102, 1)], 99);
        let result = outcome.result;

        assert_eq!(result.applied, 2);
        assert_eq!(result.ignored_stale, 1);
        assert_eq!(result.gap, None);
        let snapshot_record = outcome.replay.snapshot.unwrap();
        assert_eq!(snapshot_record.kind, ReplayKind::Snapshot);
        assert_eq!(snapshot_record.symbol_id, BTCUSDT_ID);
        assert_eq!(snapshot_record.first_update_id, 99);
        assert_eq!(snapshot_record.final_update_id, 99);
        assert_eq!(snapshot_record.sync_state, BookSyncState::Live);
        assert_eq!(snapshot_record.last_book_update_id, 101);
        assert!(matches!(
            snapshot_record.payload,
            ReplayPayload::Snapshot { bids, asks } if bids == vec![(99, 9)] && asks == vec![(102, 1)]
        ));

        let bridge_record = outcome.replay.bridge.unwrap();
        assert_eq!(bridge_record.kind, ReplayKind::Bridge);
        assert_eq!(bridge_record.sync_state, BookSyncState::Live);
        assert_eq!(bridge_record.last_book_update_id, 101);
        assert!(matches!(
            bridge_record.payload,
            ReplayPayload::Bridge {
                result,
                applied_updates,
            } if result.applied == 2
                && result.ignored_stale == 1
                && result.gap.is_none()
                && applied_updates.len() == 2
        ));
        assert_eq!(lane.sync().last_update_id(), 101);
        assert_eq!(lane.book().best_bid().unwrap().price, 101);
    }

    #[test]
    fn snapshot_bridge_applies_exact_decision_indices() {
        let mut lane = MarketDataLane::<3>::new(BTCUSDT_ID, SignalRules::default());
        lane.start_buffering();
        lane.buffer_depth_update(depth_update(95, 100, vec![(100, 10)], vec![]));
        lane.buffer_depth_update(depth_update(96, 100, vec![(105, 10)], vec![]));
        lane.buffer_depth_update(depth_update(101, 101, vec![(101, 11)], vec![]));

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
