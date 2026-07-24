use std::collections::BTreeMap;

use chrono::Duration;
use serde::Serialize;

use crate::prediction_loop::validate_sha256_id;
use crate::prediction_loop_fs::{canonical_json_bytes, sha256_hex};
use crate::research_snapshot::ResearchSnapshot;

pub const EVENT_COHORT_PARTITION_VERSION: &str = "event_cohort_partition.v1";

#[derive(Debug, Clone, PartialEq, Eq)]
struct EventCohortMetadata {
    market_id: String,
    reference_path_start_ms: i64,
    reference_path_end_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventCohortExclusionReason {
    ReferencePathCrossesBoundary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EventCohortExclusion {
    pub market_id: String,
    pub reason: EventCohortExclusionReason,
}

/// Immutable, content-addressed assignment shared by every research task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EventCohortPartition {
    schema_version: &'static str,
    authenticated_snapshot_digest: String,
    common_time_boundary_ms: i64,
    train_market_ids: Vec<String>,
    crossing_excluded: Vec<EventCohortExclusion>,
    held_out_market_ids: Vec<String>,
    digest: String,
}

#[derive(Serialize)]
struct EventCohortPartitionPayload<'a> {
    schema_version: &'static str,
    authenticated_snapshot_digest: &'a str,
    common_time_boundary_ms: i64,
    train_market_ids: &'a [String],
    crossing_excluded: &'a [EventCohortExclusion],
    held_out_market_ids: &'a [String],
}

impl EventCohortPartition {
    /// Build once from a snapshot whose loader has rehashed every referenced
    /// artifact. Consumers receive this partition, never the source rows.
    pub fn from_verified_snapshot(
        snapshot: &ResearchSnapshot,
        symbols: &[String],
        event_window_secs: i64,
        common_time_boundary_ms: i64,
    ) -> Result<Self, String> {
        let authenticated_snapshot_digest = snapshot
            .manifest
            .snapshot_contract_hash
            .as_deref()
            .or(snapshot.manifest.snapshot_hash.as_deref())
            .ok_or_else(|| "verified snapshot is missing its authenticated digest".to_string())?;
        validate_sha256_id(
            authenticated_snapshot_digest,
            "authenticated snapshot digest",
        )?;
        let window = Duration::try_seconds(event_window_secs)
            .filter(|window| *window > Duration::zero())
            .ok_or_else(|| "event cohort window must be positive".to_string())?;
        let mut event_ends = BTreeMap::new();
        for row in snapshot
            .observations
            .iter()
            .filter(|row| symbols.contains(&row.symbol))
            .filter(|row| row.event_window_secs == event_window_secs)
        {
            match event_ends.entry(row.event_id.as_str()) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(row.event_end_ts);
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    if *entry.get() != row.event_end_ts {
                        entry.insert(None);
                    }
                }
            }
        }
        let events = event_ends
            .into_iter()
            .map(|(market_id, event_end)| {
                let event_end = event_end.ok_or_else(|| {
                    format!("market_id {market_id} has no consistent canonical event end")
                })?;
                let event_start = event_end.checked_sub_signed(window).ok_or_else(|| {
                    format!("market_id {market_id} reference path start overflows")
                })?;
                Ok(EventCohortMetadata {
                    market_id: market_id.to_string(),
                    reference_path_start_ms: event_start.timestamp_millis(),
                    reference_path_end_ms: event_end.timestamp_millis(),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Self::build(
            authenticated_snapshot_digest,
            events,
            common_time_boundary_ms,
        )
    }

    #[cfg(test)]
    pub(crate) fn from_test_observations(
        rows: &[crate::factors_v2::FactorObservationV2],
        boundary_ms: i64,
        event_window_secs: i64,
    ) -> Result<Self, String> {
        let window = Duration::seconds(event_window_secs);
        let events = rows
            .iter()
            .map(|row| {
                let event_end = row
                    .event_end_ts
                    .ok_or_else(|| "test observation lacks event end".to_string())?;
                Ok(EventCohortMetadata {
                    market_id: row.event_id.clone(),
                    reference_path_start_ms: (event_end - window).timestamp_millis(),
                    reference_path_end_ms: event_end.timestamp_millis(),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let mut unique = BTreeMap::new();
        for event in events {
            unique.entry(event.market_id.clone()).or_insert(event);
        }
        Self::build(
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            unique.into_values(),
            boundary_ms,
        )
    }

    fn build(
        authenticated_snapshot_digest: &str,
        authenticated_events: impl IntoIterator<Item = EventCohortMetadata>,
        common_time_boundary_ms: i64,
    ) -> Result<Self, String> {
        if common_time_boundary_ms <= 0 {
            return Err(
                "common time boundary must be a positive Unix millisecond timestamp".into(),
            );
        }
        let mut events = authenticated_events.into_iter().collect::<Vec<_>>();
        events.sort_by(|left, right| left.market_id.cmp(&right.market_id));

        let mut train_market_ids = Vec::new();
        let mut crossing_excluded = Vec::new();
        let mut held_out_market_ids = Vec::new();
        let mut previous_market_id: Option<&str> = None;
        for event in &events {
            if event.market_id.trim().is_empty() || event.market_id.trim() != event.market_id {
                return Err("event cohort market_id must be a trimmed non-empty string".into());
            }
            if previous_market_id == Some(event.market_id.as_str()) {
                return Err(format!(
                    "event cohort contains duplicate market_id {}",
                    event.market_id
                ));
            }
            if event.reference_path_start_ms >= event.reference_path_end_ms {
                return Err(format!(
                    "event cohort market_id {} has an invalid reference path",
                    event.market_id
                ));
            }
            previous_market_id = Some(event.market_id.as_str());

            if event.reference_path_end_ms < common_time_boundary_ms {
                train_market_ids.push(event.market_id.clone());
            } else if event.reference_path_start_ms >= common_time_boundary_ms {
                held_out_market_ids.push(event.market_id.clone());
            } else {
                crossing_excluded.push(EventCohortExclusion {
                    market_id: event.market_id.clone(),
                    reason: EventCohortExclusionReason::ReferencePathCrossesBoundary,
                });
            }
        }
        let payload = EventCohortPartitionPayload {
            schema_version: EVENT_COHORT_PARTITION_VERSION,
            authenticated_snapshot_digest,
            common_time_boundary_ms,
            train_market_ids: &train_market_ids,
            crossing_excluded: &crossing_excluded,
            held_out_market_ids: &held_out_market_ids,
        };
        let digest = format!("sha256:{}", sha256_hex(&canonical_json_bytes(&payload)?));
        Ok(Self {
            schema_version: EVENT_COHORT_PARTITION_VERSION,
            authenticated_snapshot_digest: authenticated_snapshot_digest.to_string(),
            common_time_boundary_ms,
            train_market_ids,
            crossing_excluded,
            held_out_market_ids,
            digest,
        })
    }

    pub fn common_time_boundary_ms(&self) -> i64 {
        self.common_time_boundary_ms
    }

    pub fn train_market_ids(&self) -> &[String] {
        &self.train_market_ids
    }

    pub fn crossing_excluded(&self) -> &[EventCohortExclusion] {
        &self.crossing_excluded
    }

    pub fn held_out_market_ids(&self) -> &[String] {
        &self.held_out_market_ids
    }

    pub fn digest(&self) -> &str {
        &self.digest
    }

    pub fn contains_train_market(&self, market_id: &str) -> bool {
        self.train_market_ids
            .binary_search_by(|candidate| candidate.as_str().cmp(market_id))
            .is_ok()
    }

    pub fn contains_held_out_market(&self, market_id: &str) -> bool {
        self.held_out_market_ids
            .binary_search_by(|candidate| candidate.as_str().cmp(market_id))
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SNAPSHOT_DIGEST: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[test]
    fn authenticated_events_build_one_stable_common_time_partition() {
        let events = vec![
            EventCohortMetadata {
                market_id: "held-out".to_string(),
                reference_path_start_ms: 2_000,
                reference_path_end_ms: 2_500,
            },
            EventCohortMetadata {
                market_id: "crossing".to_string(),
                reference_path_start_ms: 900,
                reference_path_end_ms: 1_100,
            },
            EventCohortMetadata {
                market_id: "train".to_string(),
                reference_path_start_ms: 100,
                reference_path_end_ms: 999,
            },
        ];

        let partition =
            EventCohortPartition::build(SNAPSHOT_DIGEST, events.clone(), 1_000).unwrap();
        let reordered =
            EventCohortPartition::build(SNAPSHOT_DIGEST, events.into_iter().rev(), 1_000).unwrap();

        assert_eq!(partition.common_time_boundary_ms(), 1_000);
        assert_eq!(partition.train_market_ids(), ["train"]);
        assert_eq!(
            partition.crossing_excluded(),
            [EventCohortExclusion {
                market_id: "crossing".to_string(),
                reason: EventCohortExclusionReason::ReferencePathCrossesBoundary,
            }]
        );
        assert_eq!(partition.held_out_market_ids(), ["held-out"]);
        assert_eq!(partition.digest(), reordered.digest());
    }

    #[test]
    fn one_market_cannot_enter_more_than_one_partition() {
        let duplicate = EventCohortMetadata {
            market_id: "same-market".to_string(),
            reference_path_start_ms: 100,
            reference_path_end_ms: 999,
        };

        let error =
            EventCohortPartition::build(SNAPSHOT_DIGEST, [duplicate.clone(), duplicate], 1_000)
                .expect_err("duplicate market identity must fail closed");

        assert!(error.contains("duplicate market_id same-market"));
    }

    #[test]
    fn reference_path_touching_or_crossing_boundary_is_excluded() {
        let partition = EventCohortPartition::build(
            SNAPSHOT_DIGEST,
            [
                EventCohortMetadata {
                    market_id: "ends-at-boundary".to_string(),
                    reference_path_start_ms: 500,
                    reference_path_end_ms: 1_000,
                },
                EventCohortMetadata {
                    market_id: "starts-at-boundary".to_string(),
                    reference_path_start_ms: 1_000,
                    reference_path_end_ms: 1_500,
                },
            ],
            1_000,
        )
        .unwrap();

        assert!(partition.train_market_ids().is_empty());
        assert_eq!(
            partition
                .crossing_excluded()
                .iter()
                .map(|event| event.market_id.as_str())
                .collect::<Vec<_>>(),
            ["ends-at-boundary"]
        );
        assert_eq!(partition.held_out_market_ids(), ["starts-at-boundary"]);
    }
}
