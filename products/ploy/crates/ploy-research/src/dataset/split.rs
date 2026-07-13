use std::collections::HashSet;

use super::{DatasetSplit, DatasetSplitAssignment, DatasetSplitPolicy, EventChronologyKey};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SplitBuildError {
    DuplicateEventId {
        event_id: String,
    },
    TooFewUniqueEvents {
        found: usize,
        min_required: usize,
    },
    EvalSplitTooSmall {
        split: DatasetSplit,
        found: usize,
        min_required: usize,
    },
}

pub fn assign_chronological_event_splits(
    ordered_events: &[EventChronologyKey],
    policy: &DatasetSplitPolicy,
) -> Result<Vec<DatasetSplitAssignment>, SplitBuildError> {
    let mut seen = HashSet::with_capacity(ordered_events.len());
    for event in ordered_events {
        if !seen.insert(event.event_id.as_str()) {
            return Err(SplitBuildError::DuplicateEventId {
                event_id: event.event_id.clone(),
            });
        }
    }

    let unique_events = ordered_events.len();
    if unique_events < policy.min_unique_events {
        return Err(SplitBuildError::TooFewUniqueEvents {
            found: unique_events,
            min_required: policy.min_unique_events,
        });
    }

    let train_count = unique_events * usize::from(policy.train_percent) / 100;
    let val_count = unique_events * usize::from(policy.val_percent) / 100;
    let test_count = unique_events.saturating_sub(train_count + val_count);

    if val_count < policy.min_eval_events {
        return Err(SplitBuildError::EvalSplitTooSmall {
            split: DatasetSplit::Val,
            found: val_count,
            min_required: policy.min_eval_events,
        });
    }

    if test_count < policy.min_eval_events {
        return Err(SplitBuildError::EvalSplitTooSmall {
            split: DatasetSplit::Test,
            found: test_count,
            min_required: policy.min_eval_events,
        });
    }

    let mut assignments = Vec::with_capacity(unique_events);
    let mut train_rank = 0;
    let mut val_rank = 0;
    let mut test_rank = 0;

    for (ordered_event_index, event) in ordered_events.iter().enumerate() {
        let split = if ordered_event_index < train_count {
            let split = DatasetSplit::Train;
            let rank = train_rank;
            train_rank += 1;
            (split, rank)
        } else if ordered_event_index < train_count + val_count {
            let split = DatasetSplit::Val;
            let rank = val_rank;
            val_rank += 1;
            (split, rank)
        } else {
            let split = DatasetSplit::Test;
            let rank = test_rank;
            test_rank += 1;
            (split, rank)
        };

        assignments.push(DatasetSplitAssignment {
            event_id: event.event_id.clone(),
            symbol: event.symbol.clone(),
            end_time: event.end_time,
            ordered_event_index,
            split: split.0,
            split_rank: split.1,
        });
    }

    Ok(assignments)
}

#[cfg(test)]
mod tests {
    use super::{assign_chronological_event_splits, SplitBuildError};
    use crate::dataset::{DatasetSplit, DatasetSplitPolicy, EventChronologyKey};
    use chrono::{Duration, TimeZone, Utc};
    use std::collections::{HashMap, HashSet};

    fn synthetic_events(count: usize) -> Vec<EventChronologyKey> {
        let start = Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap();
        (0..count)
            .map(|idx| EventChronologyKey {
                event_id: format!("evt-{idx:03}"),
                symbol: if idx % 2 == 0 { "BTC" } else { "ETH" }.to_string(),
                start_time: start + Duration::minutes(idx as i64),
                end_time: start + Duration::minutes(idx as i64 + 5),
            })
            .collect()
    }

    #[test]
    fn split_assignment_is_chronological_and_leakage_safe() {
        let assignments = assign_chronological_event_splits(
            &synthetic_events(140),
            &DatasetSplitPolicy::default(),
        )
        .expect("split assignment should succeed");

        assert_eq!(assignments.len(), 140);

        let split_counts = assignments
            .iter()
            .fold(HashMap::new(), |mut counts, assignment| {
                *counts.entry(assignment.split).or_insert(0usize) += 1;
                counts
            });
        assert_eq!(split_counts.get(&DatasetSplit::Train), Some(&98));
        assert_eq!(split_counts.get(&DatasetSplit::Val), Some(&21));
        assert_eq!(split_counts.get(&DatasetSplit::Test), Some(&21));

        for window in assignments.windows(2) {
            assert!(
                window[0].ordered_event_index < window[1].ordered_event_index,
                "assignment ordering must remain chronological"
            );
        }

        let unique_event_ids: HashSet<_> = assignments
            .iter()
            .map(|assignment| assignment.event_id.as_str())
            .collect();
        assert_eq!(unique_event_ids.len(), assignments.len());
    }

    #[test]
    fn split_assignment_fails_when_unique_events_are_too_small() {
        let error =
            assign_chronological_event_splits(&synthetic_events(2), &DatasetSplitPolicy::default())
                .expect_err("split assignment should fail");

        assert_eq!(
            error,
            SplitBuildError::TooFewUniqueEvents {
                found: 2,
                min_required: 3,
            }
        );
    }

    #[test]
    fn split_assignment_fails_when_eval_split_is_below_threshold() {
        let error = assign_chronological_event_splits(
            &synthetic_events(100),
            &DatasetSplitPolicy::default(),
        )
        .expect_err("split assignment should fail");

        assert_eq!(
            error,
            SplitBuildError::EvalSplitTooSmall {
                split: DatasetSplit::Val,
                found: 15,
                min_required: 20,
            }
        );
    }
}
