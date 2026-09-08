//! Row partitions shared by the signed view binding and actual dataset readers.
use crate::{DomainError, EvaluationProtocolV1, EVALUATION_PROTOCOL_VERSION_V2};
use serde::{Deserialize, Serialize};
use std::ops::Range;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationSelectionV1 {
    pub rows: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationRowPartitionsV1 {
    /// Exclusive upper bound for both search and proposal readers.
    pub search: Range<usize>,
    pub selection: Option<Range<usize>>,
    pub sealed_holdout: Range<usize>,
}

impl EvaluationProtocolV1 {
    pub fn with_independent_selection(mut self, rows: usize) -> Result<Self, DomainError> {
        self.version = EVALUATION_PROTOCOL_VERSION_V2.to_string();
        self.selection = Some(EvaluationSelectionV1 { rows });
        self.validate()?;
        Ok(self)
    }

    pub fn row_partitions(&self, rows: usize) -> Result<EvaluationRowPartitionsV1, DomainError> {
        self.validate()?;
        let invalid = DomainError::InvalidEvaluationProtocol;
        let holdout_start = rows
            .checked_sub(self.walk_forward.sealed_holdout_rows)
            .ok_or(invalid.clone())?;
        let (search_end, selection) = if let Some(policy) = &self.selection {
            let end = holdout_start
                .checked_sub(self.walk_forward.purge_rows)
                .ok_or(invalid.clone())?;
            let start = end.checked_sub(policy.rows).ok_or(invalid.clone())?;
            let search_end = start
                .checked_sub(self.walk_forward.purge_rows)
                .ok_or(invalid.clone())?;
            (search_end, Some(start..end))
        } else {
            (holdout_start, None)
        };
        let search_schedule_end = self
            .walk_forward
            .validation_rows
            .checked_add(self.walk_forward.embargo_rows)
            .and_then(|step| step.checked_mul(self.walk_forward.fold_count))
            .and_then(|end| end.checked_add(self.walk_forward.initial_train_rows))
            .and_then(|end| end.checked_add(self.walk_forward.purge_rows))
            .ok_or(invalid.clone())?;
        if search_schedule_end > search_end {
            return Err(invalid);
        }
        Ok(EvaluationRowPartitionsV1 {
            search: 0..search_end,
            selection,
            sealed_holdout: holdout_start..rows,
        })
    }
}
