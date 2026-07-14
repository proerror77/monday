use chrono::{DateTime, Utc};

use super::{DatasetSkipCounts, EventChronologyKey};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventMetadataChronologyInput {
    pub event_id: String,
    pub symbol: String,
    pub start_time: Option<DateTime<Utc>>,
    pub end_time: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventChronologyBuild {
    pub ordered_events: Vec<EventChronologyKey>,
    pub skip_counts: DatasetSkipCounts,
}

pub fn build_canonical_event_chronology(
    events: impl IntoIterator<Item = EventMetadataChronologyInput>,
) -> EventChronologyBuild {
    let mut skip_counts = DatasetSkipCounts::default();
    let mut ordered_events = Vec::new();

    for event in events {
        match (event.start_time, event.end_time) {
            (Some(start_time), Some(end_time)) => ordered_events.push(EventChronologyKey {
                event_id: event.event_id,
                symbol: event.symbol,
                start_time,
                end_time,
            }),
            (None, Some(_)) => {
                skip_counts.missing_start_time += 1;
                skip_counts.missing_timing_fields += 1;
            }
            (Some(_), None) => {
                skip_counts.missing_end_time += 1;
                skip_counts.missing_timing_fields += 1;
            }
            (None, None) => {
                skip_counts.missing_start_time += 1;
                skip_counts.missing_end_time += 1;
                skip_counts.missing_timing_fields += 1;
            }
        }
    }

    ordered_events.sort_by(|lhs, rhs| {
        lhs.end_time
            .cmp(&rhs.end_time)
            .then_with(|| lhs.symbol.cmp(&rhs.symbol))
            .then_with(|| lhs.event_id.cmp(&rhs.event_id))
    });

    EventChronologyBuild {
        ordered_events,
        skip_counts,
    }
}

#[cfg(test)]
mod tests {
    use super::{build_canonical_event_chronology, EventMetadataChronologyInput};
    use chrono::{TimeZone, Utc};

    #[test]
    fn chronology_is_reproducible_and_skips_missing_timing_fields() {
        let result = build_canonical_event_chronology(vec![
            EventMetadataChronologyInput {
                event_id: "evt-c".to_string(),
                symbol: "ETH".to_string(),
                start_time: Some(Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap()),
                end_time: Some(Utc.with_ymd_and_hms(2026, 4, 1, 0, 5, 0).unwrap()),
            },
            EventMetadataChronologyInput {
                event_id: "evt-a".to_string(),
                symbol: "BTC".to_string(),
                start_time: Some(Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap()),
                end_time: Some(Utc.with_ymd_and_hms(2026, 4, 1, 0, 4, 0).unwrap()),
            },
            EventMetadataChronologyInput {
                event_id: "evt-b".to_string(),
                symbol: "BTC".to_string(),
                start_time: Some(Utc.with_ymd_and_hms(2026, 4, 1, 0, 1, 0).unwrap()),
                end_time: Some(Utc.with_ymd_and_hms(2026, 4, 1, 0, 5, 0).unwrap()),
            },
            EventMetadataChronologyInput {
                event_id: "evt-d".to_string(),
                symbol: "SOL".to_string(),
                start_time: None,
                end_time: Some(Utc.with_ymd_and_hms(2026, 4, 1, 0, 5, 0).unwrap()),
            },
            EventMetadataChronologyInput {
                event_id: "evt-e".to_string(),
                symbol: "SOL".to_string(),
                start_time: Some(Utc.with_ymd_and_hms(2026, 4, 1, 0, 2, 0).unwrap()),
                end_time: None,
            },
            EventMetadataChronologyInput {
                event_id: "evt-f".to_string(),
                symbol: "ARB".to_string(),
                start_time: None,
                end_time: None,
            },
        ]);

        let ordered_event_ids: Vec<_> = result
            .ordered_events
            .iter()
            .map(|event| event.event_id.as_str())
            .collect();

        assert_eq!(ordered_event_ids, vec!["evt-a", "evt-b", "evt-c"]);
        assert_eq!(result.skip_counts.missing_start_time, 2);
        assert_eq!(result.skip_counts.missing_end_time, 2);
        assert_eq!(result.skip_counts.missing_timing_fields, 3);
    }
}
