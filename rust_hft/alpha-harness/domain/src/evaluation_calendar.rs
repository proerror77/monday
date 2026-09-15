//! UTC calendar admission. Resolve boundaries from decision clocks, never labels.
use crate::{
    DomainError, EvaluationLabelSpecV1, EvaluationProtocolV1, EvaluationSelectionV1,
    EVALUATION_PROTOCOL_VERSION_V3,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationCalendarV1 {
    pub start: DateTime<Utc>,
    pub develop_end: DateTime<Utc>,
    pub validation_end: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationCalendarBindingV1 {
    pub calendar: EvaluationCalendarV1,
    pub total_rows: usize,
    pub develop_end_row: usize,
    pub validation_end_row: usize,
}

impl EvaluationCalendarV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        if !(self.start < self.develop_end
            && self.develop_end < self.validation_end
            && self.validation_end < self.end)
        {
            return Err(DomainError::InvalidEvaluationProtocol);
        }
        Ok(())
    }

    pub fn resolve(
        &self,
        clocks: &[DateTime<Utc>],
        labels: &EvaluationLabelSpecV1,
    ) -> Result<EvaluationCalendarBindingV1, DomainError> {
        self.validate()?;
        // The materializer samples on bucket boundaries and begins at sample 1
        // to compute changes from the previous book: at most two initial buckets.
        // Its forward label removes h trailing buckets, plus the end boundary.
        // These are fixed algorithmic exclusions, not tolerances learned from data.
        let millis = i64::try_from(labels.observation_frequency_millis)
            .ok()
            .filter(|millis| *millis > 0)
            .ok_or(DomainError::InvalidEvaluationProtocol)?;
        let warmup = millis
            .checked_mul(2)
            .and_then(chrono::TimeDelta::try_milliseconds)
            .ok_or(DomainError::InvalidEvaluationProtocol)?;
        let tail = i64::try_from(labels.horizon_buckets)
            .ok()
            .and_then(|h| h.checked_add(1))
            .and_then(|buckets| buckets.checked_mul(millis))
            .and_then(chrono::TimeDelta::try_milliseconds)
            .ok_or(DomainError::InvalidEvaluationProtocol)?;
        let latest_start = self
            .start
            .checked_add_signed(warmup)
            .ok_or(DomainError::InvalidEvaluationProtocol)?;
        let earliest_end = self
            .end
            .checked_sub_signed(tail)
            .ok_or(DomainError::InvalidEvaluationProtocol)?;
        if clocks.is_empty()
            || clocks[0] < self.start
            || clocks[0] > latest_start
            || clocks[clocks.len() - 1] >= self.end
            || clocks[clocks.len() - 1] < earliest_end
            || clocks.windows(2).any(|pair| {
                pair[1].signed_duration_since(pair[0]) != chrono::TimeDelta::milliseconds(millis)
            })
        {
            return Err(DomainError::InvalidEvaluationProtocol);
        }
        let binding = EvaluationCalendarBindingV1 {
            calendar: self.clone(),
            total_rows: clocks.len(),
            develop_end_row: clocks.partition_point(|clock| *clock < self.develop_end),
            validation_end_row: clocks.partition_point(|clock| *clock < self.validation_end),
        };
        if binding.develop_end_row == 0
            || binding.develop_end_row >= binding.validation_end_row
            || binding.validation_end_row >= binding.total_rows
        {
            return Err(DomainError::InvalidEvaluationProtocol);
        }
        Ok(binding)
    }
}

impl EvaluationCalendarBindingV1 {
    /// Keep three expanding development folds. Reserve the entire later
    /// validation view for already-fitted weights; its tail purge protects sealed.
    pub fn bind(
        &self,
        mut protocol: EvaluationProtocolV1,
    ) -> Result<EvaluationProtocolV1, DomainError> {
        self.calendar.validate()?;
        let invalid = DomainError::InvalidEvaluationProtocol;
        let purge = protocol.walk_forward.purge_rows;
        let embargo = protocol.walk_forward.embargo_rows;
        let folds = protocol.walk_forward.fold_count;
        let search_rows = self
            .develop_end_row
            .checked_sub(purge)
            .ok_or(invalid.clone())?;
        let usable = search_rows
            .checked_sub(purge)
            .and_then(|rows| rows.checked_sub(folds.checked_mul(embargo)?))
            .ok_or(invalid.clone())?;
        let validation_rows = usable
            .checked_div(folds.checked_mul(2).ok_or(invalid.clone())?)
            .filter(|rows| *rows > 0)
            .ok_or(invalid.clone())?;
        let initial_train_rows = usable
            .checked_sub(folds.checked_mul(validation_rows).ok_or(invalid.clone())?)
            .ok_or(invalid.clone())?;
        let selection_rows = self
            .validation_end_row
            .checked_sub(purge)
            .and_then(|end| end.checked_sub(self.develop_end_row))
            .filter(|rows| *rows > 0)
            .ok_or(invalid.clone())?;
        protocol.walk_forward.initial_train_rows = initial_train_rows;
        protocol.walk_forward.validation_rows = validation_rows;
        protocol.walk_forward.sealed_holdout_rows = self
            .total_rows
            .checked_sub(self.validation_end_row)
            .filter(|rows| *rows > 0)
            .ok_or(invalid)?;
        protocol.selection = Some(EvaluationSelectionV1 {
            rows: selection_rows,
        });
        protocol.calendar = Some(self.clone());
        protocol.version = EVALUATION_PROTOCOL_VERSION_V3.into();
        protocol.validate()?;
        Ok(protocol)
    }

    pub(crate) fn validate_protocol(&self, protocol: &EvaluationProtocolV1) -> bool {
        self.calendar.validate().is_ok()
            && self.develop_end_row > 0
            && self.develop_end_row < self.validation_end_row
            && self.validation_end_row < self.total_rows
            && self.total_rows - self.validation_end_row
                == protocol.walk_forward.sealed_holdout_rows
            && protocol.selection.as_ref().is_some_and(|selection| {
                self.validation_end_row
                    .checked_sub(protocol.walk_forward.purge_rows)
                    .and_then(|end| end.checked_sub(selection.rows))
                    == Some(self.develop_end_row)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EvaluationCostsV1, EvaluationLabelSpecV1, EvaluationWalkForwardV1};
    use chrono::TimeDelta;

    #[test]
    fn calendar_keeps_validate_and_sealed_outside_develop_for_all_h1_horizons() {
        let start: DateTime<Utc> = "2026-09-11T02:00:00Z".parse().unwrap();
        let calendar = EvaluationCalendarV1 {
            start,
            develop_end: start + TimeDelta::hours(4),
            validation_end: start + TimeDelta::hours(6),
            end: start + TimeDelta::hours(8),
        };
        // Real materialization can lose warm-up and label-tail rows. Boundaries
        // must still follow UTC, not an assumed offset from the first row.
        for horizon in [5, 10, 30] {
            let clocks = (2..(28_800 - horizon as i64))
                .map(|i| start + TimeDelta::seconds(i))
                .collect::<Vec<_>>();
            let protocol = EvaluationProtocolV1::new(
                EvaluationWalkForwardV1 {
                    initial_train_rows: 7_200,
                    validation_rows: 3_600,
                    fold_count: 3,
                    purge_rows: 2 * horizon,
                    embargo_rows: horizon,
                    sealed_holdout_rows: 3_600,
                },
                EvaluationCostsV1 {
                    fee_bps: 2.0,
                    rebate_bps: 0.0,
                    funding_bps: 0.0,
                    latency_bps: 0.5,
                    slippage_bps: 0.0,
                    cross_spread: false,
                    position_notional_usd: 0.0,
                    capacity_depth_levels: 0,
                    max_book_depth_fraction: 0.0,
                },
                EvaluationLabelSpecV1 {
                    horizon_buckets: horizon,
                    observation_frequency_millis: 1_000,
                },
            )
            .unwrap();
            let binding = calendar.resolve(&clocks, &protocol.labels).unwrap();
            assert_eq!(binding.develop_end_row, 14_398);
            assert_eq!(binding.validation_end_row, 21_598);
            assert!(calendar
                .resolve(&clocks[3_600..clocks.len() - 3_600], &protocol.labels)
                .is_err());
            assert!(calendar.resolve(&clocks[1..], &protocol.labels).is_err());
            assert!(calendar
                .resolve(&clocks[..clocks.len() - 1], &protocol.labels)
                .is_err());
            let mut drifted = clocks.clone();
            drifted[14_398] -= TimeDelta::seconds(1);
            assert!(calendar.resolve(&drifted, &protocol.labels).is_err());
            let mut missing_bucket = clocks.clone();
            missing_bucket.remove(7_200);
            assert!(calendar.resolve(&missing_bucket, &protocol.labels).is_err());
            let bound = binding.bind(protocol).unwrap();
            let parts = bound.row_partitions(clocks.len()).unwrap();
            assert!(clocks[parts.search.end - 1] < calendar.develop_end);
            assert_eq!(
                clocks[parts.selection.clone().unwrap().start],
                calendar.develop_end
            );
            assert_eq!(clocks[parts.sealed_holdout.start], calendar.validation_end);
            assert!(bound.row_partitions(clocks.len() + 1).is_err());
        }
    }
}
