use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use ploy_market_contracts::{BookLevel, MarketUpdate};
use ploy_market_data::polymarket_evidence::{
    BinaryOutcomeSide, PolymarketBookLevel, PolymarketEvidenceBook, PolymarketEvidenceIdentity,
    VerifiedPolymarketEvidence,
};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use std::collections::{btree_map::Entry, BTreeMap};
use std::sync::Arc;

use crate::verified_artifact_audit::{usable_polymarket_book, usable_polymarket_reference};
use crate::{ResearchPmBookLevel, ResearchPmBookSnapshot};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolymarketResearchSurfaceCounts {
    pub contracts: usize,
    pub books: usize,
    pub references: usize,
    pub trades: usize,
    pub settlements: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResearchPolymarketContract {
    pub event_id: String,
    pub symbol: String,
    pub up_token_id: String,
    pub down_token_id: String,
    pub event_start: DateTime<Utc>,
    pub event_end: DateTime<Utc>,
    pub price_to_beat: Decimal,
    pub available_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResearchPolymarketSettlement {
    pub event_id: String,
    pub winning_token_id: String,
    pub resolved_up_won: bool,
    pub available_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct PolymarketResearchProjection {
    pub evidence_identity: PolymarketEvidenceIdentity,
    pub surface_counts: PolymarketResearchSurfaceCounts,
    pub updates: Vec<MarketUpdate>,
    pub pm_book_snapshots: Vec<ResearchPmBookSnapshot>,
    pub contracts: Vec<ResearchPolymarketContract>,
    pub settlements: Vec<ResearchPolymarketSettlement>,
    pub evidence_available_through: DateTime<Utc>,
}

pub fn project_verified_polymarket_evidence(
    verified: &VerifiedPolymarketEvidence,
    pm_book_sample_secs: i64,
    maximum_source_delay_secs: i64,
) -> Result<PolymarketResearchProjection> {
    if pm_book_sample_secs <= 0 {
        bail!("pm_book_sample_secs must be positive");
    }
    if maximum_source_delay_secs <= 0 {
        bail!("maximum_source_delay_secs must be positive");
    }

    let contracts_by_market = verified
        .contracts()
        .iter()
        .map(|contract| (contract.market_id.as_str(), contract))
        .collect::<BTreeMap<_, _>>();
    let mut contracts = verified
        .contracts()
        .iter()
        .map(|contract| ResearchPolymarketContract {
            event_id: contract.market_id.clone(),
            symbol: contract.symbol.clone(),
            up_token_id: contract.up_token_id.clone(),
            down_token_id: contract.down_token_id.clone(),
            event_start: contract.event_start,
            event_end: contract.event_end,
            price_to_beat: contract.price_to_beat,
            available_at: contract.available_at,
        })
        .collect::<Vec<_>>();
    let mut settlements = verified
        .settlements()
        .iter()
        .map(|settlement| ResearchPolymarketSettlement {
            event_id: settlement.market_id.clone(),
            winning_token_id: settlement.winning_token_id.clone(),
            resolved_up_won: settlement.winning_side == BinaryOutcomeSide::Up,
            available_at: settlement.observed_at,
        })
        .collect::<Vec<_>>();

    // Event contract and settlement variants in the generic update tape do not carry
    // availability clocks. Keep them in the typed carriers above so replay cannot
    // expose discovery metadata or resolved labels before they were observed.
    let mut updates = Vec::new();
    for reference in verified.references() {
        let contract = contracts_by_market
            .get(reference.market_id.as_str())
            .expect("verified reference belongs to a verified contract");
        if !usable_polymarket_reference(contract, reference, maximum_source_delay_secs) {
            continue;
        }
        updates.push(MarketUpdate::ReferencePrice {
            symbol: Arc::from(contract.symbol.as_str()),
            source: Arc::from("chainlink"),
            asset_class: Arc::from("crypto"),
            price: reference.price,
            full_accuracy_value: None,
            is_carried_forward: reference.is_carried_forward,
            received_at: Some(reference.available_at),
            ts: reference.source_time,
        });
    }

    let mut sampled_books = BTreeMap::<(String, String, i64), &PolymarketEvidenceBook>::new();
    for book in verified.books() {
        let contract = contracts_by_market
            .get(book.market_id.as_str())
            .expect("verified book belongs to a verified contract");
        if !usable_polymarket_book(contract, book, maximum_source_delay_secs) {
            continue;
        }
        let key = (
            book.market_id.clone(),
            book.token_id.clone(),
            book.available_at
                .timestamp()
                .div_euclid(pm_book_sample_secs),
        );
        match sampled_books.entry(key) {
            Entry::Vacant(slot) => {
                slot.insert(book);
            }
            Entry::Occupied(mut slot) => {
                let current = *slot.get();
                let book_clock = (book.available_at, book.source_time);
                let current_clock = (current.available_at, current.source_time);
                if book_clock > current_clock {
                    slot.insert(book);
                } else if book_clock == current_clock && book != current {
                    bail!("ambiguous Polymarket books share the same sampling clock");
                }
            }
        }
    }

    let mut pm_book_snapshots = Vec::with_capacity(sampled_books.len());
    for book in sampled_books.into_values() {
        let mut bids = book.bid_levels.clone().unwrap_or_default();
        let mut asks = book.ask_levels.clone().unwrap_or_default();
        bids.sort_by(|left, right| right.price.cmp(&left.price));
        asks.sort_by(|left, right| left.price.cmp(&right.price));
        updates.push(MarketUpdate::Quote {
            token_id: Arc::from(book.token_id.as_str()),
            bid: book.bid,
            ask: book.ask,
            bid_size: book.bid_size,
            ask_size: book.ask_size,
            bid_levels: market_levels(&bids),
            ask_levels: market_levels(&asks),
            ts: book.available_at,
        });
        pm_book_snapshots.push(ResearchPmBookSnapshot {
            event_id: book.market_id.clone(),
            token_id: book.token_id.clone(),
            side: match book.side {
                BinaryOutcomeSide::Up => "UP",
                BinaryOutcomeSide::Down => "DOWN",
            }
            .to_owned(),
            ts: book.available_at,
            bids: research_levels(&bids)?,
            asks: research_levels(&asks)?,
        });
    }

    contracts.sort_by(|left, right| {
        (left.event_start, &left.event_id).cmp(&(right.event_start, &right.event_id))
    });
    settlements.sort_by(|left, right| {
        (left.available_at, &left.event_id).cmp(&(right.available_at, &right.event_id))
    });
    updates.sort_by_key(MarketUpdate::sort_ts);
    pm_book_snapshots.sort_by(|left, right| {
        (left.ts, &left.event_id, &left.token_id).cmp(&(right.ts, &right.event_id, &right.token_id))
    });

    let evidence_available_through = verified
        .contracts()
        .iter()
        .map(|row| row.available_at)
        .chain(verified.books().iter().map(|row| row.available_at))
        .chain(verified.references().iter().map(|row| row.available_at))
        .chain(verified.trades().iter().map(|row| row.available_at))
        .chain(verified.settlements().iter().map(|row| row.observed_at))
        .max()
        .expect("verified evidence contains every required surface");

    Ok(PolymarketResearchProjection {
        evidence_identity: verified.identity().clone(),
        surface_counts: PolymarketResearchSurfaceCounts {
            contracts: verified.contracts().len(),
            books: verified.books().len(),
            references: verified.references().len(),
            trades: verified.trades().len(),
            settlements: verified.settlements().len(),
        },
        updates,
        pm_book_snapshots,
        contracts,
        settlements,
        evidence_available_through,
    })
}

fn market_levels(levels: &[PolymarketBookLevel]) -> Vec<BookLevel> {
    levels
        .iter()
        .map(|level| BookLevel {
            price: level.price,
            size: level.size,
        })
        .collect()
}

fn research_levels(levels: &[PolymarketBookLevel]) -> Result<Vec<ResearchPmBookLevel>> {
    levels
        .iter()
        .map(|level| {
            Ok(ResearchPmBookLevel {
                price: level.price.to_f64().context("convert book price")?,
                size: level.size.to_f64().context("convert book size")?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ploy_market_data::polymarket_evidence::{
        seal_polymarket_evidence_triplet, verify_polymarket_evidence, PolymarketEvidenceTriplet,
        PolymarketEvidenceTrustAnchor,
    };
    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn time(value: &str) -> DateTime<Utc> {
        value.parse().unwrap()
    }

    #[rustfmt::skip]
    fn rows() -> Vec<Value> {
        let context = json!({"schema":"monday.polymarket.evidence_row.v1","market_id":"market-1","condition_id":"condition-1","symbol":"BTCUSDT","event_start":"2026-07-17T05:30:00Z","event_end":"2026-07-17T05:35:00Z","window_secs":300});
        let row = |surface: &str, fields: Value| { let mut value=context.clone(); value["surface"]=json!(surface); value.as_object_mut().unwrap().extend(fields.as_object().unwrap().clone()); value };
        vec![
            row("market_contract", json!({"source_token_ids":["down-token","up-token"],"source_outcomes":["Down","Up"],"price_to_beat":"63000","resolution_source":"https://data.chain.link/streams/btc-usd","metadata_retrieved_at":"2026-07-17T05:29:58Z","discovery_recorded_at":"2026-07-17T05:29:57Z","metadata_recorded_at":"2026-07-17T05:29:59Z","available_at":"2026-07-17T05:29:59Z","discovery_source_sequence":1,"metadata_source_sequence":2,"source_datasets":["crypto_expiry","crypto_expiry_reference"]})),
            row("orderbook_snapshot", json!({"token_id":"down-token","ts":"2026-07-17T05:30:01Z","recorded_at":"2026-07-17T05:30:02Z","available_at":"2026-07-17T05:30:02Z","source_sequence":3,"source_dataset":"crypto_expiry","bid":"0.4","ask":"0.5","bid_size":"10","ask_size":"11","bid_levels":[{"price":"0.4","size":"10"}],"ask_levels":[{"price":"0.5","size":"11"}]})),
            row("orderbook_snapshot", json!({"token_id":"up-token","ts":"2026-07-17T05:30:01Z","recorded_at":"2026-07-17T05:30:02Z","available_at":"2026-07-17T05:30:02Z","source_sequence":4,"source_dataset":"crypto_expiry","bid":"0.5","ask":"0.6","bid_size":"11","ask_size":"10","bid_levels":[{"price":"0.5","size":"11"}],"ask_levels":[{"price":"0.6","size":"10"}]})),
            row("chainlink_reference", json!({"source":"chainlink","asset_class":"crypto","source_symbol":"btc/usd","price":"63000","full_accuracy_value":null,"is_carried_forward":false,"ts":"2026-07-17T05:29:55Z","received_at":"2026-07-17T05:29:56Z","available_at":"2026-07-17T05:29:57Z","recorded_at":"2026-07-17T05:29:57Z","source_sequence":5,"source_dataset":"crypto_expiry"})),
            row("polymarket_trade", json!({"record_id":"trade-v2-1","record_id_version":"v2","token_id":"up-token","source_outcome":"Up","outcome_index":1,"side":"BUY","size":"2","price":"0.6","trade_ts":"2026-07-17T05:30:03Z","trade_ts_unix":1784266203_i64,"transaction_hash":"0xabc","proxy_wallet":"0xdef","source":"polymarket_data_api","received_at":"2026-07-17T05:30:04Z","available_at":"2026-07-17T05:30:05Z","recorded_at":"2026-07-17T05:30:05Z","source_sequence":6,"source_dataset":"crypto_expiry_reference"})),
            row("official_settlement_evidence", json!({"source_token_ids":["down-token","up-token"],"source_outcomes":["Down","Up"],"source_outcome_prices":["0","1"],"winning_token_id":"up-token","winning_outcome":"Up","resolution_source":"gamma_api_closed_market","retrieved_at":"2026-07-17T05:35:01Z","available_at":"2026-07-17T05:35:02Z","recorded_at":"2026-07-17T05:35:02Z","source_sequence":7,"source_dataset":"crypto_expiry_reference"})),
        ]
    }

    #[rustfmt::skip]
    fn validated_inputs() -> Value {
        let segment = |dataset: &str, sample: u64, versions: Value| json!({
            "schema":"monday.polymarket.raw.v1","venue":"polymarket","dataset":dataset,"date":"2026-07-17","hour":"05","file":format!("{dataset}.ndjson.zst"),"bytes":100,"sha256":"1".repeat(64),"events":2,"start_sequence":1,"end_sequence":2,"sequence_gaps":0,"start_recorded_at":"2026-07-17T05:00:00Z","end_recorded_at":"2026-07-17T05:01:00Z","source_file":format!("{dataset}.ndjson"),"replay_scope":if sample == 0 { "complete_reference_hour_segment" } else { "complete_full_depth_sampled_normalized_hour_segment" },"record_id_versions":versions,"recording_policy":{"quote_sample_ms":sample,"quote_depth_levels":0,"event_scoped_quotes":true}
        });
        json!({"schema":"monday.polymarket.research_segment_validation.v1","market":segment("crypto_expiry", 1000, json!([])),"reference":segment("crypto_expiry_reference", 0, json!(["v2"]))})
    }

    fn verified(rows: &[Value]) -> VerifiedPolymarketEvidence {
        let temp = tempfile::tempdir().unwrap();
        let mut data_bytes = Vec::new();
        let mut counts = BTreeMap::new();
        for row in rows {
            serde_json::to_writer(&mut data_bytes, row).unwrap();
            data_bytes.push(b'\n');
            *counts
                .entry(row["surface"].as_str().unwrap())
                .or_insert(0_u64) += 1;
        }
        let content_sha = format!("{:x}", Sha256::digest(&data_bytes));
        let root = fs::canonicalize(temp.path())
            .unwrap()
            .join(format!("sha256={content_sha}"));
        fs::create_dir(&root).unwrap();
        let name = format!("polymarket-btc-sol-5m.{content_sha}.ndjson");
        let triplet = PolymarketEvidenceTriplet {
            data: root.join(&name),
            manifest: root.join(format!("{name}.manifest.json")),
            success: root.join(format!("{name}._SUCCESS")),
        };
        fs::write(&triplet.data, &data_bytes).unwrap();
        let manifest = json!({
            "schema":"monday.polymarket.evidence_artifact.v1","file":name,"format":"ndjson","content_sha256":content_sha,"content_bytes":data_bytes.len(),"rows":rows.len(),"events":counts["market_contract"],"surface_counts":counts,"event_start_gte":"2026-07-17T05:30:00Z","event_start_lt":"2026-07-17T05:35:00Z","symbols":["BTCUSDT","SOLUSDT"],"window_secs":300,"event_selection":"event_start in [event_start_gte,event_start_lt)","evidence_scope":"immutable collector evidence only; not an execution authorization or evaluator label artifact","content_digest_semantics":"content_sha256 binds the published NDJSON bytes only; it is not a snapshot_contract_hash","recording_semantics":{"orderbook":{"level":"L2","depth":"full visible depth as received","quote_sample_ms":1000,"venue_depth_complete":true,"temporal_updates_complete":false,"l3_order_ids_available":false,"queue_position_modeled":false,"endogenous_impact_modeled":false,"capacity_modeled":false},"trades":"exact market_id association using canonical v2 records; trade_ts may fall outside the event lifetime","references":"typed Chainlink BTC/USD or SOL/USD with source timestamp in [event_start - 30 seconds, event_end)","settlement":"gamma_api_closed_market closed-market evidence joined by exact market_id","availability_clock":"point-in-time rows expose the latest validated recorded or retrieved clock as available_at"},"trust_boundary":"typed collector staging evidence only; not an evaluator label snapshot or snapshot_contract_hash; validated staged triplets and adjacent local supersession markers; omitted remote-prefix markers are not proven absent","validated_inputs":validated_inputs()
        });
        fs::write(
            &triplet.manifest,
            format!("{}\n", serde_json::to_string(&manifest).unwrap()),
        )
        .unwrap();
        fs::write(&triplet.success, format!("{content_sha}\n")).unwrap();
        for path in [&triplet.data, &triplet.manifest, &triplet.success] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o444)).unwrap();
        }
        let hash = |path| format!("{:x}", Sha256::digest(fs::read(path).unwrap()));
        let trust = PolymarketEvidenceTrustAnchor::from_lower_hex(
            &hash(&triplet.data),
            &hash(&triplet.manifest),
        )
        .unwrap();
        verify_polymarket_evidence(seal_polymarket_evidence_triplet(&triplet, &trust).unwrap())
            .unwrap()
    }

    #[test]
    fn rejects_non_positive_book_sample_interval() {
        let evidence = verified(&rows());
        let error = project_verified_polymarket_evidence(&evidence, 0, 30).unwrap_err();
        assert!(error.to_string().contains("must be positive"), "{error:#}");
        let error = project_verified_polymarket_evidence(&evidence, 1, 0).unwrap_err();
        assert!(error.to_string().contains("must be positive"), "{error:#}");
    }

    #[test]
    fn projects_source_and_availability_clocks_without_lookahead() {
        let evidence = verified(&rows());
        let projected = project_verified_polymarket_evidence(&evidence, 1, 30).unwrap();

        let (reference_source, reference_available) = projected
            .updates
            .iter()
            .find_map(|update| match update {
                MarketUpdate::ReferencePrice {
                    ts, received_at, ..
                } => Some((*ts, *received_at)),
                _ => None,
            })
            .unwrap();
        assert_eq!(reference_source, time("2026-07-17T05:29:55Z"));
        assert_eq!(reference_available, Some(time("2026-07-17T05:29:57Z")));

        let quote_ts = projected
            .updates
            .iter()
            .find_map(|update| match update {
                MarketUpdate::Quote { token_id, ts, .. } if &**token_id == "down-token" => {
                    Some(*ts)
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(quote_ts, time("2026-07-17T05:30:02Z"));
        assert_eq!(projected.pm_book_snapshots[0].ts, quote_ts);
        assert_eq!(
            projected.contracts[0].available_at,
            time("2026-07-17T05:29:59Z")
        );
        assert_eq!(
            projected.settlements[0].available_at,
            time("2026-07-17T05:35:02Z")
        );
    }

    #[test]
    fn rejects_delayed_rows_before_sampling() {
        let mut fixture = rows();
        let mut delayed_book = fixture[1].clone();
        delayed_book["ts"] = json!("2026-07-17T05:30:01.500Z");
        delayed_book["recorded_at"] = json!("2026-07-17T05:30:42Z");
        delayed_book["available_at"] = json!("2026-07-17T05:30:42Z");
        delayed_book["source_sequence"] = json!(8);
        delayed_book["bid"] = json!("0.41");
        delayed_book["bid_levels"][0]["price"] = json!("0.41");
        fixture.push(delayed_book);
        let mut delayed_reference = fixture[3].clone();
        delayed_reference["ts"] = json!("2026-07-17T05:29:54Z");
        delayed_reference["received_at"] = json!("2026-07-17T05:30:35Z");
        delayed_reference["recorded_at"] = json!("2026-07-17T05:30:35Z");
        delayed_reference["available_at"] = json!("2026-07-17T05:30:35Z");
        delayed_reference["source_sequence"] = json!(9);
        delayed_reference["price"] = json!("64000");
        fixture.push(delayed_reference);

        let evidence = verified(&fixture);
        let projected = project_verified_polymarket_evidence(&evidence, 300, 30).unwrap();
        assert_eq!(projected.surface_counts.books, 3);
        assert_eq!(projected.surface_counts.references, 2);
        assert_eq!(projected.pm_book_snapshots.len(), 2);
        let down = projected
            .pm_book_snapshots
            .iter()
            .find(|book| book.token_id == "down-token")
            .unwrap();
        assert_eq!(down.ts, time("2026-07-17T05:30:02Z"));
        assert_eq!(down.bids[0].price, 0.4);
        assert_eq!(
            projected
                .updates
                .iter()
                .filter(|update| matches!(update, MarketUpdate::ReferencePrice { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn equal_availability_books_use_source_time_independent_of_row_order() {
        let mut forward = rows();
        let mut later_source = forward[1].clone();
        later_source["ts"] = json!("2026-07-17T05:30:01.500Z");
        later_source["source_sequence"] = json!(8);
        later_source["bid"] = json!("0.41");
        later_source["bid_levels"][0]["price"] = json!("0.41");
        forward.push(later_source);
        let mut reversed = forward.clone();
        let last = reversed.len() - 1;
        reversed.swap(1, last);

        for fixture in [forward, reversed] {
            let evidence = verified(&fixture);
            let projected = project_verified_polymarket_evidence(&evidence, 10, 30).unwrap();
            let down = projected
                .pm_book_snapshots
                .iter()
                .find(|book| book.token_id == "down-token")
                .unwrap();
            assert_eq!(down.ts, time("2026-07-17T05:30:02Z"));
            assert_eq!(down.bids[0].price, 0.41);
        }
    }

    #[test]
    fn ambiguous_books_with_identical_clocks_fail_closed() {
        let mut fixture = rows();
        let mut contradictory = fixture[1].clone();
        contradictory["source_sequence"] = json!(8);
        contradictory["bid"] = json!("0.41");
        contradictory["bid_levels"][0]["price"] = json!("0.41");
        fixture.push(contradictory);

        let evidence = verified(&fixture);
        let error = project_verified_polymarket_evidence(&evidence, 10, 30).unwrap_err();
        assert!(error.to_string().contains("ambiguous"), "{error:#}");
    }

    #[test]
    fn derives_the_winner_from_verified_semantic_sides() {
        let evidence = verified(&rows());
        let projected = project_verified_polymarket_evidence(&evidence, 1, 30).unwrap();

        assert_eq!(&projected.evidence_identity, evidence.identity());
        assert_ne!(
            projected.evidence_identity.content_sha256,
            projected.evidence_identity.manifest_sha256
        );
        assert_eq!(
            projected.surface_counts,
            PolymarketResearchSurfaceCounts {
                contracts: 1,
                books: 2,
                references: 1,
                trades: 1,
                settlements: 1,
            }
        );
        assert_eq!(projected.contracts[0].up_token_id, "up-token");
        assert!(projected.settlements[0].resolved_up_won);
        assert!(!projected.updates.iter().any(|update| matches!(
            update,
            MarketUpdate::EventDiscovered { .. } | MarketUpdate::EventExpired { .. }
        )));
        assert!(!projected
            .updates
            .iter()
            .any(|update| matches!(update, MarketUpdate::AggTrade { .. })));
    }
}
