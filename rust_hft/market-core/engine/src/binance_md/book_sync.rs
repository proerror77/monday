#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateMeta {
    pub first_update_id: u64,
    pub final_update_id: u64,
}

impl UpdateMeta {
    #[inline]
    pub fn new(first_update_id: u64, final_update_id: u64) -> Self {
        Self {
            first_update_id,
            final_update_id,
        }
    }

    #[inline]
    pub fn is_valid(self) -> bool {
        self.first_update_id <= self.final_update_id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SequenceDecision {
    Apply,
    IgnoreStale,
    Gap { expected: u64, first_seen: u64 },
    InvalidRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookSyncState {
    Disconnected,
    BufferingDiffs,
    FetchingSnapshot,
    ApplyingBuffered,
    Live,
    RebuildRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferedDecision {
    pub index: usize,
    pub update: UpdateMeta,
    pub decision: SequenceDecision,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BufferedApplyResult {
    pub applied: usize,
    pub ignored_stale: usize,
    pub gap: Option<(u64, u64)>,
    pub decisions: Vec<BufferedDecision>,
}

/// Binance snapshot/diff bridge state.
///
/// It keeps only sequence metadata. Price levels are applied by the caller
/// after `SequenceDecision::Apply`.
#[derive(Debug, Clone)]
pub struct BookSync {
    state: BookSyncState,
    last_update_id: u64,
    buffered: Vec<UpdateMeta>,
}

impl Default for BookSync {
    fn default() -> Self {
        Self {
            state: BookSyncState::Disconnected,
            last_update_id: 0,
            buffered: Vec::new(),
        }
    }
}

impl BookSync {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn state(&self) -> BookSyncState {
        self.state
    }

    #[inline]
    pub fn last_update_id(&self) -> u64 {
        self.last_update_id
    }

    pub fn start_buffering(&mut self) {
        self.state = BookSyncState::BufferingDiffs;
        self.last_update_id = 0;
        self.buffered.clear();
    }

    pub fn start_snapshot_fetch(&mut self) {
        if self.state != BookSyncState::BufferingDiffs {
            self.start_buffering();
        }
        self.state = BookSyncState::FetchingSnapshot;
    }

    pub fn buffer_diff(&mut self, update: UpdateMeta) {
        if matches!(
            self.state,
            BookSyncState::BufferingDiffs | BookSyncState::FetchingSnapshot
        ) {
            self.buffered.push(update);
        }
    }

    pub fn apply_snapshot(&mut self, snapshot_last_update_id: u64) -> BufferedApplyResult {
        self.state = BookSyncState::ApplyingBuffered;
        self.last_update_id = snapshot_last_update_id;

        let mut result = BufferedApplyResult::default();
        let mut bridged = false;

        for (index, update) in self.buffered.iter().copied().enumerate() {
            if !update.is_valid() {
                result.decisions.push(BufferedDecision {
                    index,
                    update,
                    decision: SequenceDecision::InvalidRange,
                });
                self.state = BookSyncState::RebuildRequired;
                result.gap = Some((self.last_update_id + 1, update.first_update_id));
                return result;
            }

            if !bridged {
                if update.final_update_id <= snapshot_last_update_id {
                    result.ignored_stale += 1;
                    result.decisions.push(BufferedDecision {
                        index,
                        update,
                        decision: SequenceDecision::IgnoreStale,
                    });
                    continue;
                }

                if !bridges_snapshot(snapshot_last_update_id, update) {
                    let decision = SequenceDecision::Gap {
                        expected: snapshot_last_update_id + 1,
                        first_seen: update.first_update_id,
                    };
                    result.decisions.push(BufferedDecision {
                        index,
                        update,
                        decision,
                    });
                    self.state = BookSyncState::RebuildRequired;
                    result.gap = Some((snapshot_last_update_id + 1, update.first_update_id));
                    return result;
                }

                bridged = true;
            }

            match classify_update(self.last_update_id, update) {
                SequenceDecision::Apply => {
                    self.last_update_id = update.final_update_id;
                    result.applied += 1;
                    result.decisions.push(BufferedDecision {
                        index,
                        update,
                        decision: SequenceDecision::Apply,
                    });
                }
                SequenceDecision::IgnoreStale => {
                    result.ignored_stale += 1;
                    result.decisions.push(BufferedDecision {
                        index,
                        update,
                        decision: SequenceDecision::IgnoreStale,
                    });
                }
                SequenceDecision::Gap {
                    expected,
                    first_seen,
                } => {
                    result.decisions.push(BufferedDecision {
                        index,
                        update,
                        decision: SequenceDecision::Gap {
                            expected,
                            first_seen,
                        },
                    });
                    self.state = BookSyncState::RebuildRequired;
                    result.gap = Some((expected, first_seen));
                    return result;
                }
                SequenceDecision::InvalidRange => {
                    result.decisions.push(BufferedDecision {
                        index,
                        update,
                        decision: SequenceDecision::InvalidRange,
                    });
                    self.state = BookSyncState::RebuildRequired;
                    result.gap = Some((self.last_update_id + 1, update.first_update_id));
                    return result;
                }
            }
        }

        self.buffered.clear();
        self.state = if bridged {
            BookSyncState::Live
        } else {
            BookSyncState::RebuildRequired
        };

        result
    }

    pub fn classify_live_update(&mut self, update: UpdateMeta) -> SequenceDecision {
        let decision = classify_update(self.last_update_id, update);
        match decision {
            SequenceDecision::Apply => {
                self.last_update_id = update.final_update_id;
                self.state = BookSyncState::Live;
            }
            SequenceDecision::Gap { .. } | SequenceDecision::InvalidRange => {
                self.state = BookSyncState::RebuildRequired;
            }
            SequenceDecision::IgnoreStale => {}
        }
        decision
    }
}

#[inline]
pub fn bridges_snapshot(snapshot_last_update_id: u64, update: UpdateMeta) -> bool {
    let expected = snapshot_last_update_id + 1;
    update.first_update_id <= expected && update.final_update_id >= expected
}

#[inline]
pub fn classify_update(last_update_id: u64, update: UpdateMeta) -> SequenceDecision {
    if !update.is_valid() {
        return SequenceDecision::InvalidRange;
    }

    let expected = last_update_id + 1;
    if update.final_update_id < expected {
        return SequenceDecision::IgnoreStale;
    }
    if update.first_update_id > expected {
        return SequenceDecision::Gap {
            expected,
            first_seen: update.first_update_id,
        };
    }

    SequenceDecision::Apply
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridges_first_update_after_snapshot() {
        assert!(bridges_snapshot(100, UpdateMeta::new(90, 101)));
        assert!(bridges_snapshot(100, UpdateMeta::new(101, 110)));
        assert!(!bridges_snapshot(100, UpdateMeta::new(102, 110)));
        assert!(!bridges_snapshot(100, UpdateMeta::new(90, 100)));
    }

    #[test]
    fn detects_live_sequence_gap() {
        assert_eq!(
            classify_update(100, UpdateMeta::new(102, 105)),
            SequenceDecision::Gap {
                expected: 101,
                first_seen: 102
            }
        );
        assert_eq!(
            classify_update(100, UpdateMeta::new(99, 100)),
            SequenceDecision::IgnoreStale
        );
        assert_eq!(
            classify_update(100, UpdateMeta::new(99, 101)),
            SequenceDecision::Apply
        );
    }

    #[test]
    fn applies_buffered_updates_and_enters_live() {
        let mut sync = BookSync::new();
        sync.start_buffering();
        sync.buffer_diff(UpdateMeta::new(90, 100));
        sync.buffer_diff(UpdateMeta::new(95, 101));
        sync.buffer_diff(UpdateMeta::new(102, 103));

        let result = sync.apply_snapshot(100);

        assert_eq!(result.applied, 2);
        assert_eq!(result.ignored_stale, 1);
        assert_eq!(result.gap, None);
        assert_eq!(
            result
                .decisions
                .iter()
                .map(|decision| decision.decision)
                .collect::<Vec<_>>(),
            vec![
                SequenceDecision::IgnoreStale,
                SequenceDecision::Apply,
                SequenceDecision::Apply,
            ]
        );
        assert_eq!(sync.state(), BookSyncState::Live);
        assert_eq!(sync.last_update_id(), 103);
    }

    #[test]
    fn records_apply_stale_apply_buffer_decisions() {
        let mut sync = BookSync::new();
        sync.start_buffering();
        sync.buffer_diff(UpdateMeta::new(95, 100));
        sync.buffer_diff(UpdateMeta::new(96, 100));
        sync.buffer_diff(UpdateMeta::new(101, 101));

        let result = sync.apply_snapshot(99);

        assert_eq!(result.applied, 2);
        assert_eq!(result.ignored_stale, 1);
        assert_eq!(result.gap, None);
        assert_eq!(
            result
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
        assert_eq!(sync.last_update_id(), 101);
    }

    #[test]
    fn rebuilds_when_buffer_cannot_bridge_snapshot() {
        let mut sync = BookSync::new();
        sync.start_buffering();
        sync.buffer_diff(UpdateMeta::new(102, 103));

        let result = sync.apply_snapshot(100);

        assert_eq!(sync.state(), BookSyncState::RebuildRequired);
        assert_eq!(result.gap, Some((101, 102)));
    }
}
