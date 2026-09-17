use serde::{Deserialize, Serialize};

use hft_research_manifest::sec_orderflow::SecOrderflowError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlumbingSplitV1 {
    pub label: String,
    pub holdout_open: bool,
    pub train_decisions: usize,
    pub validation_decisions: usize,
    pub test_decisions: usize,
    pub purge_gap_s: i64,
    pub calendar_days: u32,
}

pub fn plumbing_split(
    decision_times_ms: &[i64],
    gap_floor_s: i64,
) -> Result<PlumbingSplitV1, SecOrderflowError> {
    if gap_floor_s < 2_105 {
        return Err(SecOrderflowError::Invalid(
            "full dependency gap floor must be at least 2105s",
        ));
    }
    let mut times = decision_times_ms.to_vec();
    times.sort_unstable();
    times.dedup();
    if times.len() < 3 {
        return Ok(PlumbingSplitV1 {
            label: "plumbing_chronological_not_oos".to_string(),
            holdout_open: false,
            train_decisions: times.len(),
            validation_decisions: 0,
            test_decisions: 0,
            purge_gap_s: gap_floor_s,
            calendar_days: calendar_days(&times),
        });
    }
    let first = times[0];
    let last = *times.last().expect("non-empty");
    let span = last.saturating_sub(first);
    let train_cut = first.saturating_add(span * 6 / 10);
    let test_cut = first.saturating_add(span * 8 / 10);
    let gap_ms = gap_floor_s.saturating_mul(1_000);
    let train: Vec<i64> = times
        .iter()
        .copied()
        .filter(|time| *time <= train_cut)
        .collect();
    let validation: Vec<i64> = times
        .iter()
        .copied()
        .filter(|time| *time > train_cut + gap_ms && *time <= test_cut)
        .collect();
    let test: Vec<i64> = times
        .iter()
        .copied()
        .filter(|time| *time > test_cut + gap_ms)
        .collect();
    Ok(PlumbingSplitV1 {
        label: "plumbing_chronological_not_oos".to_string(),
        holdout_open: false,
        train_decisions: train.len(),
        validation_decisions: validation.len(),
        test_decisions: test.len(),
        purge_gap_s: gap_floor_s,
        calendar_days: calendar_days(&times),
    })
}

fn calendar_days(times_ms: &[i64]) -> u32 {
    match (times_ms.first(), times_ms.last()) {
        (Some(first), Some(last)) if *last >= *first => {
            let span_ms = last.saturating_sub(*first);
            u32::try_from(span_ms / 86_400_000)
                .unwrap_or(0)
                .saturating_add(1)
        }
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_to_open_holdout_or_shrink_purge_floor() {
        assert!(plumbing_split(&[0, 1], 2_104).is_err());
        let split = plumbing_split(&[0, 2_200_000, 4_400_000], 2_105).unwrap();
        assert!(!split.holdout_open);
    }
}
