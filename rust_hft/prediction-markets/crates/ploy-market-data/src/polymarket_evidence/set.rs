use super::{
    PolymarketEvidenceBook, PolymarketEvidenceContract, PolymarketEvidenceIdentity,
    PolymarketEvidenceReference, PolymarketEvidenceSettlement, PolymarketEvidenceTrade,
    VerifiedPolymarketEvidence,
};
use anyhow::{bail, ensure, Result};
use chrono::{DateTime, Duration, Utc};
use std::collections::{BTreeMap, BTreeSet};

const WINDOW_SECS: i64 = 300;
const REQUIRED_SYMBOLS: [&str; 2] = ["BTCUSDT", "SOLUSDT"];

/// An ordered aggregate that retains each independently verified evidence handle.
/// Members may have disjoint ranges; every member remains complete within its own range.
#[derive(Debug)]
pub struct VerifiedPolymarketEvidenceSet {
    members: Vec<VerifiedPolymarketEvidence>,
    event_start_gte: DateTime<Utc>,
    event_start_lt: DateTime<Utc>,
}

impl VerifiedPolymarketEvidenceSet {
    pub fn members(&self) -> &[VerifiedPolymarketEvidence] {
        &self.members
    }

    pub fn event_start_gte(&self) -> DateTime<Utc> {
        self.event_start_gte
    }

    pub fn event_start_lt(&self) -> DateTime<Utc> {
        self.event_start_lt
    }

    pub fn identities(&self) -> impl Iterator<Item = &PolymarketEvidenceIdentity> {
        self.members
            .iter()
            .map(VerifiedPolymarketEvidence::identity)
    }

    pub fn contracts(&self) -> impl Iterator<Item = &PolymarketEvidenceContract> {
        self.members
            .iter()
            .flat_map(VerifiedPolymarketEvidence::contracts)
    }

    pub fn books(&self) -> impl Iterator<Item = &PolymarketEvidenceBook> {
        self.members
            .iter()
            .flat_map(VerifiedPolymarketEvidence::books)
    }

    pub fn references(&self) -> impl Iterator<Item = &PolymarketEvidenceReference> {
        self.members
            .iter()
            .flat_map(VerifiedPolymarketEvidence::references)
    }

    pub fn trades(&self) -> impl Iterator<Item = &PolymarketEvidenceTrade> {
        self.members
            .iter()
            .flat_map(VerifiedPolymarketEvidence::trades)
    }

    pub fn settlements(&self) -> impl Iterator<Item = &PolymarketEvidenceSettlement> {
        self.members
            .iter()
            .flat_map(VerifiedPolymarketEvidence::settlements)
    }
}

pub fn aggregate_verified_polymarket_evidence(
    mut members: Vec<VerifiedPolymarketEvidence>,
) -> Result<VerifiedPolymarketEvidenceSet> {
    ensure!(
        !members.is_empty(),
        "verified evidence set must not be empty"
    );
    members.sort_by(|left, right| {
        left.identity()
            .event_start_gte
            .cmp(&right.identity().event_start_gte)
            .then_with(|| {
                left.identity()
                    .event_start_lt
                    .cmp(&right.identity().event_start_lt)
            })
            .then_with(|| {
                left.identity()
                    .content_sha256
                    .cmp(&right.identity().content_sha256)
            })
    });
    let (event_start_gte, event_start_lt) = validate_members(&members)?;
    Ok(VerifiedPolymarketEvidenceSet {
        members,
        event_start_gte,
        event_start_lt,
    })
}

fn validate_members(
    members: &[VerifiedPolymarketEvidence],
) -> Result<(DateTime<Utc>, DateTime<Utc>)> {
    let mut content_digests = BTreeSet::new();
    let mut manifest_digests = BTreeSet::new();
    for member in members {
        let identity = member.identity();
        if !content_digests.insert(identity.content_sha256.as_str())
            || !manifest_digests.insert(identity.manifest_sha256.as_str())
        {
            bail!("verified evidence set contains a duplicate artifact digest");
        }
        validate_aligned_range(identity)?;
    }
    for adjacent in members.windows(2) {
        let previous_end = adjacent[0].identity().event_start_lt;
        let next_start = adjacent[1].identity().event_start_gte;
        if next_start < previous_end {
            bail!("verified evidence artifact ranges overlap");
        }
    }

    let start = members[0].identity().event_start_gte;
    let end = members[members.len() - 1].identity().event_start_lt;
    validate_global_identities(members)?;
    for member in members {
        validate_member_slot_pairs(member)?;
    }
    Ok((start, end))
}

fn validate_aligned_range(identity: &PolymarketEvidenceIdentity) -> Result<()> {
    let seconds = (identity.event_start_lt - identity.event_start_gte).num_seconds();
    if identity.event_start_gte.timestamp().rem_euclid(WINDOW_SECS) != 0
        || identity.event_start_lt.timestamp().rem_euclid(WINDOW_SECS) != 0
        || seconds <= 0
        || seconds % WINDOW_SECS != 0
    {
        bail!("verified evidence artifact range is not aligned to complete 5-minute slots");
    }
    Ok(())
}

fn validate_global_identities(members: &[VerifiedPolymarketEvidence]) -> Result<()> {
    let mut markets = BTreeSet::new();
    let mut conditions = BTreeSet::new();
    let mut tokens = BTreeSet::new();
    let mut trades = BTreeSet::new();
    for member in members {
        for contract in member.contracts() {
            if !markets.insert(contract.market_id.as_str()) {
                bail!("verified evidence set contains a duplicate market_id");
            }
            if !conditions.insert(contract.condition_id.as_str()) {
                bail!("verified evidence set contains a duplicate condition_id");
            }
            if !tokens.insert(contract.up_token_id.as_str())
                || !tokens.insert(contract.down_token_id.as_str())
            {
                bail!("verified evidence set contains a duplicate token_id");
            }
        }
        for trade in member.trades() {
            if !trades.insert(trade.record_id.as_str()) {
                bail!("verified evidence set contains a duplicate trade record_id");
            }
        }
    }
    Ok(())
}

fn validate_member_slot_pairs(member: &VerifiedPolymarketEvidence) -> Result<()> {
    let start = member.identity().event_start_gte;
    let end = member.identity().event_start_lt;
    let mut slots: BTreeMap<DateTime<Utc>, BTreeSet<&str>> = BTreeMap::new();
    for contract in member.contracts() {
        if contract.event_start < start
            || contract.event_start >= end
            || contract.event_start.timestamp().rem_euclid(WINDOW_SECS) != 0
        {
            bail!("verified evidence contract is outside an aligned artifact slot");
        }
        let symbols = slots.entry(contract.event_start).or_default();
        if !symbols.insert(contract.symbol.as_str()) {
            bail!("verified evidence slot contains a duplicate symbol");
        }
    }

    let slot_count = (end - start).num_seconds() / WINDOW_SECS;
    if usize::try_from(slot_count)? != slots.len() {
        bail!("verified evidence set is missing a 5-minute slot");
    }
    let required = BTreeSet::from(REQUIRED_SYMBOLS);
    let mut expected_start = start;
    for (slot_start, symbols) in slots {
        if slot_start != expected_start || symbols != required {
            bail!("every verified 5-minute slot must contain BTCUSDT and SOLUSDT");
        }
        expected_start = expected_start
            .checked_add_signed(Duration::seconds(WINDOW_SECS))
            .ok_or_else(|| anyhow::anyhow!("verified evidence slot range overflows"))?;
    }
    ensure!(
        expected_start == end,
        "verified evidence set is missing a 5-minute slot"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::polymarket_evidence::{
        artifact::tests as artifact_tests, seal_polymarket_evidence_triplet,
        verify_polymarket_evidence,
    };
    use chrono::SecondsFormat;
    use serde_json::{json, Value};
    use std::{fs, os::unix::fs::PermissionsExt};

    fn timestamp(value: DateTime<Utc>) -> String {
        value.to_rfc3339_opts(SecondsFormat::Secs, true)
    }

    #[rustfmt::skip]
    fn event_rows(symbol: &str, start: DateTime<Utc>, suffix: &str) -> Vec<Value> {
        let end = start + Duration::seconds(WINDOW_SECS);
        let market = format!("market-{symbol}-{suffix}");
        let condition = format!("condition-{symbol}-{suffix}");
        let down = format!("down-{symbol}-{suffix}");
        let up = format!("up-{symbol}-{suffix}");
        let context = json!({
            "schema":"monday.polymarket.evidence_row.v1", "market_id":market,
            "condition_id":condition, "symbol":symbol, "event_start":timestamp(start),
            "event_end":timestamp(end), "window_secs":300
        });
        let row = |surface: &str, fields: Value| {
            let mut value = context.clone();
            value["surface"] = json!(surface);
            value
                .as_object_mut()
                .unwrap()
                .extend(fields.as_object().unwrap().clone());
            value
        };
        let (reference_symbol, price_to_beat, resolution_source) = match symbol {
            "BTCUSDT" => ("btc/usd", "63000", "https://data.chain.link/streams/btc-usd"),
            "SOLUSDT" => ("sol/usd", "150", "https://data.chain.link/streams/sol-usd"),
            _ => unreachable!(),
        };
        vec![
            row("market_contract", json!({"source_token_ids":[down,up],"source_outcomes":["Down","Up"],"price_to_beat":price_to_beat,"resolution_source":resolution_source,"metadata_retrieved_at":timestamp(start-Duration::seconds(2)),"discovery_recorded_at":timestamp(start-Duration::seconds(3)),"metadata_recorded_at":timestamp(start-Duration::seconds(1)),"available_at":timestamp(start-Duration::seconds(1)),"discovery_source_sequence":1,"metadata_source_sequence":2,"source_datasets":["crypto_expiry","crypto_expiry_reference"]})),
            row("orderbook_snapshot", json!({"token_id":down,"ts":timestamp(start+Duration::seconds(1)),"recorded_at":timestamp(start+Duration::seconds(2)),"available_at":timestamp(start+Duration::seconds(2)),"source_sequence":3,"source_dataset":"crypto_expiry","bid":"0.4","ask":"0.5","bid_size":"10","ask_size":"11","bid_levels":[{"price":"0.4","size":"10"}],"ask_levels":[{"price":"0.5","size":"11"}]})),
            row("orderbook_snapshot", json!({"token_id":up,"ts":timestamp(start+Duration::seconds(1)),"recorded_at":timestamp(start+Duration::seconds(2)),"available_at":timestamp(start+Duration::seconds(2)),"source_sequence":4,"source_dataset":"crypto_expiry","bid":"0.5","ask":"0.6","bid_size":"11","ask_size":"10","bid_levels":[{"price":"0.5","size":"11"}],"ask_levels":[{"price":"0.6","size":"10"}]})),
            row("chainlink_reference", json!({"source":"chainlink","asset_class":"crypto","source_symbol":reference_symbol,"price":price_to_beat,"full_accuracy_value":null,"is_carried_forward":false,"ts":timestamp(start-Duration::seconds(5)),"received_at":timestamp(start-Duration::seconds(4)),"available_at":timestamp(start-Duration::seconds(3)),"recorded_at":timestamp(start-Duration::seconds(3)),"source_sequence":5,"source_dataset":"crypto_expiry"})),
            row("polymarket_trade", json!({"record_id":format!("trade-{symbol}-{suffix}"),"record_id_version":"v2","token_id":up,"source_outcome":"Up","outcome_index":1,"side":"BUY","size":"2","price":"0.6","trade_ts":timestamp(start+Duration::seconds(3)),"trade_ts_unix":(start+Duration::seconds(3)).timestamp(),"transaction_hash":format!("tx-{symbol}-{suffix}"),"proxy_wallet":"wallet","source":"polymarket_data_api","received_at":timestamp(start+Duration::seconds(4)),"available_at":timestamp(start+Duration::seconds(5)),"recorded_at":timestamp(start+Duration::seconds(5)),"source_sequence":6,"source_dataset":"crypto_expiry_reference"})),
            row("official_settlement_evidence", json!({"source_token_ids":[down,up],"source_outcomes":["Down","Up"],"source_outcome_prices":["0","1"],"winning_token_id":up,"winning_outcome":"Up","resolution_source":"gamma_api_closed_market","retrieved_at":timestamp(end+Duration::seconds(1)),"available_at":timestamp(end+Duration::seconds(2)),"recorded_at":timestamp(end+Duration::seconds(2)),"source_sequence":7,"source_dataset":"crypto_expiry_reference"})),
        ]
    }

    fn rows(start: DateTime<Utc>, suffix: &str) -> Vec<Value> {
        let mut rows = event_rows("BTCUSDT", start, suffix);
        rows.extend(event_rows("SOLUSDT", start, suffix));
        rows
    }

    fn hour_rows(start: DateTime<Utc>, suffix: &str) -> Vec<Value> {
        let mut hour = Vec::new();
        for slot in 0..12 {
            let slot_start = start + Duration::seconds(WINDOW_SECS * slot);
            hour.extend(rows(slot_start, &format!("{suffix}-{slot}")));
        }
        hour
    }

    fn verified_range(
        rows: &[Value],
        lower: DateTime<Utc>,
        upper: DateTime<Utc>,
    ) -> VerifiedPolymarketEvidence {
        let temp = tempfile::tempdir().unwrap();
        let triplet = artifact_tests::write_triplet_rows(&temp, rows);
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(&triplet.manifest).unwrap()).unwrap();
        manifest["event_start_gte"] = json!(timestamp(lower));
        manifest["event_start_lt"] = json!(timestamp(upper));
        for dataset in ["market", "reference"] {
            let input = &mut manifest["validated_inputs"][dataset];
            input["date"] = json!(lower.format("%Y-%m-%d").to_string());
            input["hour"] = json!(lower.format("%H").to_string());
            input["start_recorded_at"] = json!(timestamp(lower));
            input["end_recorded_at"] = json!(timestamp(lower + Duration::minutes(1)));
        }
        for completion in manifest["validated_inputs"]["reference"]["trade_completions"]
            .as_object_mut()
            .unwrap()
            .values_mut()
        {
            completion["retrieved_at"] = json!(timestamp(lower + Duration::minutes(1)));
        }
        fs::set_permissions(&triplet.manifest, fs::Permissions::from_mode(0o644)).unwrap();
        fs::write(
            &triplet.manifest,
            format!("{}\n", serde_json::to_string(&manifest).unwrap()),
        )
        .unwrap();
        fs::set_permissions(&triplet.manifest, fs::Permissions::from_mode(0o444)).unwrap();
        let sealed =
            seal_polymarket_evidence_triplet(&triplet, &artifact_tests::trust(&triplet)).unwrap();
        verify_polymarket_evidence(sealed).unwrap()
    }

    fn verified(rows: &[Value], start: DateTime<Utc>) -> VerifiedPolymarketEvidence {
        verified_range(rows, start, start + Duration::seconds(WINDOW_SECS))
    }

    fn base() -> DateTime<Utc> {
        "2026-07-17T05:30:00Z".parse().unwrap()
    }

    fn replace(value: &mut Value, from: &str, to: &str) {
        match value {
            Value::String(current) if current.as_str() == from => *current = to.to_owned(),
            Value::Array(values) => {
                for value in values {
                    replace(value, from, to);
                }
            }
            Value::Object(fields) => {
                for value in fields.values_mut() {
                    replace(value, from, to);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn aggregates_out_of_order_independently_verified_artifacts() {
        let start = base();
        let first = verified(&rows(start, "a"), start);
        let second_start = start + Duration::seconds(WINDOW_SECS);
        let second = verified(&rows(second_start, "b"), second_start);
        let set = aggregate_verified_polymarket_evidence(vec![second, first]).unwrap();

        assert_eq!(set.members().len(), 2);
        assert_eq!(set.event_start_gte(), start);
        assert_eq!(
            set.event_start_lt(),
            second_start + Duration::seconds(WINDOW_SECS)
        );
        assert_eq!(set.contracts().count(), 4);
        assert_eq!(set.books().count(), 8);
        assert_eq!(set.references().count(), 4);
        assert_eq!(set.trades().count(), 4);
        assert_eq!(set.settlements().count(), 4);
        assert_eq!(set.identities().next().unwrap().event_start_gte, start);
    }

    #[test]
    fn aggregates_two_contiguous_verified_hourly_artifacts() {
        let start: DateTime<Utc> = "2026-07-17T05:00:00Z".parse().unwrap();
        let second_start = start + Duration::hours(1);
        let end = second_start + Duration::hours(1);
        let first = verified_range(&hour_rows(start, "first"), start, second_start);
        let second = verified_range(&hour_rows(second_start, "second"), second_start, end);

        let set = aggregate_verified_polymarket_evidence(vec![first, second]).unwrap();
        assert_eq!(set.members().len(), 2);
        assert_eq!(set.event_start_gte(), start);
        assert_eq!(set.event_start_lt(), end);
        assert_eq!(set.contracts().count(), 48);
    }

    #[test]
    fn rejects_verified_manifest_range_not_aligned_to_five_minutes() {
        let start = base();
        let member = verified_range(
            &rows(start, "unaligned"),
            start - Duration::seconds(1),
            start + Duration::seconds(WINDOW_SECS),
        );

        let error = aggregate_verified_polymarket_evidence(vec![member]).unwrap_err();
        assert!(error.to_string().contains("not aligned"), "{error:#}");
    }

    #[test]
    fn accepts_gap_but_rejects_overlap_or_duplicate_digest() {
        let start = base();
        let first = || verified(&rows(start, "a"), start);
        let gap_start = start + Duration::seconds(WINDOW_SECS * 2);
        let set = aggregate_verified_polymarket_evidence(vec![
            first(),
            verified(&rows(gap_start, "c"), gap_start),
        ])
        .unwrap();
        assert_eq!(set.members().len(), 2);
        assert_eq!(set.event_start_gte(), start);
        assert_eq!(
            set.event_start_lt(),
            gap_start + Duration::seconds(WINDOW_SECS)
        );

        let overlap = aggregate_verified_polymarket_evidence(vec![
            first(),
            verified(&rows(start, "b"), start),
        ])
        .unwrap_err();
        assert!(overlap.to_string().contains("overlap"), "{overlap:#}");

        let duplicate = aggregate_verified_polymarket_evidence(vec![first(), first()]).unwrap_err();
        assert!(
            duplicate.to_string().contains("duplicate artifact digest"),
            "{duplicate:#}"
        );
    }

    #[test]
    fn rejects_missing_slot_inside_an_artifact_range() {
        let start = base();
        let member = verified_range(
            &rows(start, "missing-slot"),
            start,
            start + Duration::seconds(WINDOW_SECS * 2),
        );

        let error = aggregate_verified_polymarket_evidence(vec![member]).unwrap_err();
        assert!(
            error.to_string().contains("missing a 5-minute slot"),
            "{error:#}"
        );
    }

    #[test]
    fn rejects_missing_or_duplicate_symbol_in_any_slot() {
        let start = base();
        let second_start = start + Duration::seconds(WINDOW_SECS);
        let first = || verified(&rows(start, "a"), start);
        let mut missing = rows(second_start, "b");
        missing.retain(|row| row["symbol"] != "SOLUSDT");
        let error =
            aggregate_verified_polymarket_evidence(vec![first(), verified(&missing, second_start)])
                .unwrap_err();
        assert!(
            error.to_string().contains("BTCUSDT and SOLUSDT"),
            "{error:#}"
        );

        let mut duplicate = rows(second_start, "c");
        for row in duplicate
            .iter_mut()
            .filter(|row| row["symbol"] == "SOLUSDT")
        {
            row["symbol"] = json!("BTCUSDT");
            if row["surface"] == "chainlink_reference" {
                row["source_symbol"] = json!("btc/usd");
            }
        }
        let error = aggregate_verified_polymarket_evidence(vec![
            first(),
            verified(&duplicate, second_start),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("duplicate symbol"), "{error:#}");
    }

    #[test]
    fn rejects_cross_artifact_market_condition_token_or_trade_reuse() {
        let start = base();
        let second_start = start + Duration::seconds(WINDOW_SECS);
        for (from, to, message) in [
            ("market-BTCUSDT-b", "market-BTCUSDT-a", "market_id"),
            ("condition-BTCUSDT-b", "condition-BTCUSDT-a", "condition_id"),
            ("up-BTCUSDT-b", "up-BTCUSDT-a", "token_id"),
            ("trade-BTCUSDT-b", "trade-BTCUSDT-a", "trade record_id"),
        ] {
            let mut second_rows = rows(second_start, "b");
            for row in &mut second_rows {
                replace(row, from, to);
            }
            let error = aggregate_verified_polymarket_evidence(vec![
                verified(&rows(start, "a"), start),
                verified(&second_rows, second_start),
            ])
            .unwrap_err();
            assert!(error.to_string().contains(message), "{error:#}");
        }
    }
}
