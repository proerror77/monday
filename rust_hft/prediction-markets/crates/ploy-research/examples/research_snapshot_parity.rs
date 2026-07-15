//! Compare a research snapshot with dry-run and live trading-state snapshots.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::File;
use std::io::BufReader;

use ploy_operator_contracts::{TradingIntentSnapshot, TradingStateSnapshot};
use ploy_research::load_research_snapshot;
use serde::Serialize;

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|window| window[0] == flag)
        .map(|window| window[1].clone())
}

fn flag_present(args: &[String], flag: &str) -> bool {
    args.iter().any(|arg| arg == flag)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct ParityKey {
    event_id: String,
    token_id: String,
    side: String,
    purpose: String,
}

#[derive(Debug, Clone, Serialize)]
struct OrderParityRow {
    key: ParityKey,
    order_id: String,
    state: String,
    requested_qty: String,
    filled_qty: String,
    rejection_reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct SnapshotRuntimeParityReport {
    snapshot_schema: String,
    snapshot_hash: String,
    snapshot_generated_at: String,
    snapshot_observations: usize,
    snapshot_events: usize,
    dryrun_orders: usize,
    live_orders: usize,
    dryrun_only_orders: Vec<OrderParityRow>,
    live_only_orders: Vec<OrderParityRow>,
    live_fill_shortfalls: Vec<LiveFillShortfall>,
    order_record_mismatches: Vec<OrderRecordMismatch>,
    live_order_events_not_in_snapshot: Vec<String>,
    dryrun_order_events_not_in_snapshot: Vec<String>,
}

impl SnapshotRuntimeParityReport {
    fn has_blocking_mismatch(&self) -> bool {
        !self.dryrun_only_orders.is_empty()
            || !self.live_only_orders.is_empty()
            || !self.live_fill_shortfalls.is_empty()
            || !self.order_record_mismatches.is_empty()
            || !self.live_order_events_not_in_snapshot.is_empty()
            || !self.dryrun_order_events_not_in_snapshot.is_empty()
    }
}

#[derive(Debug, Serialize)]
struct LiveFillShortfall {
    key: ParityKey,
    dryrun_filled_qty: String,
    live_filled_qty: String,
}

#[derive(Debug, Serialize)]
struct OrderRecordMismatch {
    key: ParityKey,
    field: String,
    dryrun_value: String,
    live_value: String,
}

#[derive(Debug, Default)]
struct OrderKeySummary {
    order_count: usize,
    requested_qty: f64,
    filled_qty: f64,
    state_counts: BTreeMap<String, usize>,
    rejection_reason_counts: BTreeMap<String, usize>,
}

fn read_state(path: &str) -> anyhow::Result<TradingStateSnapshot> {
    let file = File::open(path)?;
    Ok(serde_json::from_reader(BufReader::new(file))?)
}

fn order_rows(snapshot: &TradingStateSnapshot) -> Vec<OrderParityRow> {
    let intents: HashMap<&str, &TradingIntentSnapshot> = snapshot
        .intents
        .iter()
        .map(|intent| (intent.intent_id.as_str(), intent))
        .collect();

    snapshot
        .orders
        .iter()
        .filter_map(|order| {
            let intent = intents.get(order.intent_id.as_str())?;
            Some(OrderParityRow {
                key: ParityKey {
                    event_id: intent.market_id.clone(),
                    token_id: order.token_id.clone(),
                    side: intent.side.to_ascii_lowercase(),
                    purpose: format!("{:?}", intent.purpose).to_ascii_lowercase(),
                },
                order_id: order.order_id.clone(),
                state: order.state.clone(),
                requested_qty: order.requested_qty.to_string(),
                filled_qty: order.filled_qty.to_string(),
                rejection_reason: order
                    .rejection_reason
                    .clone()
                    .or_else(|| order.last_error.clone()),
            })
        })
        .collect()
}

fn filled_by_key(rows: &[OrderParityRow]) -> BTreeMap<ParityKey, f64> {
    let mut out = BTreeMap::new();
    for row in rows {
        let qty = quantity(&row.filled_qty);
        *out.entry(row.key.clone()).or_insert(0.0) += qty;
    }
    out
}

fn quantity(raw: &str) -> f64 {
    raw.parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
        .unwrap_or(0.0)
}

fn bump_count(counts: &mut BTreeMap<String, usize>, value: impl Into<String>) {
    *counts.entry(value.into()).or_insert(0) += 1;
}

fn order_summaries_by_key(rows: &[OrderParityRow]) -> BTreeMap<ParityKey, OrderKeySummary> {
    let mut summaries: BTreeMap<ParityKey, OrderKeySummary> = BTreeMap::new();
    for row in rows {
        let summary = summaries.entry(row.key.clone()).or_default();
        summary.order_count += 1;
        summary.requested_qty += quantity(&row.requested_qty);
        summary.filled_qty += quantity(&row.filled_qty);
        bump_count(&mut summary.state_counts, row.state.clone());
        bump_count(
            &mut summary.rejection_reason_counts,
            row.rejection_reason
                .clone()
                .unwrap_or_else(|| "<none>".to_string()),
        );
    }
    summaries
}

fn format_counts(counts: &BTreeMap<String, usize>) -> String {
    counts
        .iter()
        .map(|(value, count)| format!("{value}:{count}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn push_mismatch(
    mismatches: &mut Vec<OrderRecordMismatch>,
    key: &ParityKey,
    field: &str,
    dryrun_value: impl Into<String>,
    live_value: impl Into<String>,
) {
    mismatches.push(OrderRecordMismatch {
        key: key.clone(),
        field: field.to_string(),
        dryrun_value: dryrun_value.into(),
        live_value: live_value.into(),
    });
}

fn order_record_mismatches(
    dryrun_rows: &[OrderParityRow],
    live_rows: &[OrderParityRow],
) -> Vec<OrderRecordMismatch> {
    const EPSILON: f64 = 0.0001;

    let dryrun = order_summaries_by_key(dryrun_rows);
    let live = order_summaries_by_key(live_rows);
    let keys = dryrun
        .keys()
        .chain(live.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut mismatches = Vec::new();

    for key in keys {
        let Some(dryrun_summary) = dryrun.get(&key) else {
            continue;
        };
        let Some(live_summary) = live.get(&key) else {
            continue;
        };

        if dryrun_summary.order_count != live_summary.order_count {
            push_mismatch(
                &mut mismatches,
                &key,
                "order_count",
                dryrun_summary.order_count.to_string(),
                live_summary.order_count.to_string(),
            );
        }
        if dryrun_summary.state_counts != live_summary.state_counts {
            push_mismatch(
                &mut mismatches,
                &key,
                "state_counts",
                format_counts(&dryrun_summary.state_counts),
                format_counts(&live_summary.state_counts),
            );
        }
        if (dryrun_summary.requested_qty - live_summary.requested_qty).abs() > EPSILON {
            push_mismatch(
                &mut mismatches,
                &key,
                "requested_qty",
                format!("{:.8}", dryrun_summary.requested_qty),
                format!("{:.8}", live_summary.requested_qty),
            );
        }
        if (dryrun_summary.filled_qty - live_summary.filled_qty).abs() > EPSILON {
            push_mismatch(
                &mut mismatches,
                &key,
                "filled_qty",
                format!("{:.8}", dryrun_summary.filled_qty),
                format!("{:.8}", live_summary.filled_qty),
            );
        }
        if dryrun_summary.rejection_reason_counts != live_summary.rejection_reason_counts {
            push_mismatch(
                &mut mismatches,
                &key,
                "rejection_reason_counts",
                format_counts(&dryrun_summary.rejection_reason_counts),
                format_counts(&live_summary.rejection_reason_counts),
            );
        }
    }

    mismatches
}

fn build_report_from_rows(
    snapshot_schema: String,
    snapshot_hash: String,
    snapshot_generated_at: String,
    snapshot_observations: usize,
    snapshot_events: BTreeSet<String>,
    dryrun_rows: Vec<OrderParityRow>,
    live_rows: Vec<OrderParityRow>,
) -> SnapshotRuntimeParityReport {
    let dry_keys: BTreeSet<_> = dryrun_rows.iter().map(|row| row.key.clone()).collect();
    let live_keys: BTreeSet<_> = live_rows.iter().map(|row| row.key.clone()).collect();
    let live_filled = filled_by_key(&live_rows);
    let dry_filled = filled_by_key(&dryrun_rows);
    let order_record_mismatches = order_record_mismatches(&dryrun_rows, &live_rows);

    let dryrun_only_orders = dryrun_rows
        .iter()
        .filter(|row| !live_keys.contains(&row.key))
        .cloned()
        .collect::<Vec<_>>();
    let live_only_orders = live_rows
        .iter()
        .filter(|row| !dry_keys.contains(&row.key))
        .cloned()
        .collect::<Vec<_>>();
    let live_fill_shortfalls = dry_filled
        .iter()
        .filter_map(|(key, dry_qty)| {
            let live_qty = live_filled.get(key).copied().unwrap_or(0.0);
            if *dry_qty > 0.0 && live_qty + 0.0001 < *dry_qty {
                Some(LiveFillShortfall {
                    key: key.clone(),
                    dryrun_filled_qty: format!("{dry_qty:.8}"),
                    live_filled_qty: format!("{live_qty:.8}"),
                })
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    let dryrun_order_events_not_in_snapshot = dryrun_rows
        .iter()
        .map(|row| row.key.event_id.clone())
        .filter(|event_id| !snapshot_events.contains(event_id))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let live_order_events_not_in_snapshot = live_rows
        .iter()
        .map(|row| row.key.event_id.clone())
        .filter(|event_id| !snapshot_events.contains(event_id))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    SnapshotRuntimeParityReport {
        snapshot_schema,
        snapshot_hash,
        snapshot_generated_at,
        snapshot_observations,
        snapshot_events: snapshot_events.len(),
        dryrun_orders: dryrun_rows.len(),
        live_orders: live_rows.len(),
        dryrun_only_orders,
        live_only_orders,
        live_fill_shortfalls,
        order_record_mismatches,
        live_order_events_not_in_snapshot,
        dryrun_order_events_not_in_snapshot,
    }
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let snapshot_dir = flag_value(&args, "--snapshot-dir").expect("--snapshot-dir required");
    let dryrun_state = flag_value(&args, "--dryrun-state").expect("--dryrun-state required");
    let live_state = flag_value(&args, "--live-state").expect("--live-state required");
    let fail_on_mismatch = flag_present(&args, "--fail-on-mismatch");

    let snapshot = load_research_snapshot(snapshot_dir)?;
    let dryrun = read_state(&dryrun_state)?;
    let live = read_state(&live_state)?;

    let snapshot_events: BTreeSet<String> = snapshot
        .observations
        .iter()
        .map(|row| row.event_id.clone())
        .collect();
    let report = build_report_from_rows(
        snapshot.manifest.schema_version,
        snapshot
            .manifest
            .snapshot_hash
            .unwrap_or_else(|| "<missing>".to_string()),
        snapshot.manifest.generated_at.to_rfc3339(),
        snapshot.observations.len(),
        snapshot_events,
        order_rows(&dryrun),
        order_rows(&live),
    );

    println!("{}", serde_json::to_string_pretty(&report)?);
    if fail_on_mismatch && report.has_blocking_mismatch() {
        std::process::exit(2);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(event_id: &str, token_id: &str, side: &str, filled_qty: &str) -> OrderParityRow {
        OrderParityRow {
            key: ParityKey {
                event_id: event_id.to_string(),
                token_id: token_id.to_string(),
                side: side.to_string(),
                purpose: "entry".to_string(),
            },
            order_id: format!("{event_id}-{token_id}-{side}"),
            state: "filled".to_string(),
            requested_qty: "10".to_string(),
            filled_qty: filled_qty.to_string(),
            rejection_reason: None,
        }
    }

    #[test]
    fn parity_report_flags_live_fill_shortfall() {
        let report = build_report_from_rows(
            "research_snapshot_v1".to_string(),
            "hash-1".to_string(),
            "2026-04-28T00:00:00Z".to_string(),
            1,
            BTreeSet::from(["event-1".to_string()]),
            vec![row("event-1", "token-up", "buy", "10")],
            vec![row("event-1", "token-up", "buy", "4")],
        );
        assert_eq!(report.live_fill_shortfalls.len(), 1);
        assert!(report
            .order_record_mismatches
            .iter()
            .any(|row| row.field == "filled_qty"));
        assert!(report.has_blocking_mismatch());
    }

    #[test]
    fn parity_report_flags_orders_outside_snapshot_events() {
        let report = build_report_from_rows(
            "research_snapshot_v1".to_string(),
            "hash-1".to_string(),
            "2026-04-28T00:00:00Z".to_string(),
            1,
            BTreeSet::from(["event-1".to_string()]),
            vec![row("event-2", "token-up", "buy", "0")],
            vec![row("event-2", "token-up", "buy", "0")],
        );
        assert_eq!(report.dryrun_order_events_not_in_snapshot, vec!["event-2"]);
        assert_eq!(report.live_order_events_not_in_snapshot, vec!["event-2"]);
        assert!(report.has_blocking_mismatch());
    }

    #[test]
    fn parity_report_flags_same_key_record_differences() {
        let mut dryrun = row("event-1", "token-up", "buy", "4");
        dryrun.requested_qty = "10".to_string();
        dryrun.state = "filled".to_string();
        dryrun.rejection_reason = None;

        let mut live = row("event-1", "token-up", "buy", "8");
        live.requested_qty = "12".to_string();
        live.state = "rejected".to_string();
        live.rejection_reason = Some("not enough balance".to_string());

        let report = build_report_from_rows(
            "research_snapshot_v1".to_string(),
            "hash-1".to_string(),
            "2026-04-28T00:00:00Z".to_string(),
            1,
            BTreeSet::from(["event-1".to_string()]),
            vec![dryrun],
            vec![live],
        );
        let fields = report
            .order_record_mismatches
            .iter()
            .map(|row| row.field.as_str())
            .collect::<BTreeSet<_>>();
        assert!(fields.contains("state_counts"));
        assert!(fields.contains("requested_qty"));
        assert!(fields.contains("filled_qty"));
        assert!(fields.contains("rejection_reason_counts"));
        assert!(report.live_fill_shortfalls.is_empty());
        assert!(report.has_blocking_mismatch());
    }
}
