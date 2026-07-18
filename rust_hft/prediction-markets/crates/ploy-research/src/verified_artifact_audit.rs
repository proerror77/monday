use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Duration, Utc};
use data::binance_lob_replay::Market;
use data::binance_market_tape_artifact::VerifiedBinanceMarketTape;
use ploy_market_data::diagnostics::{
    assemble_prediction_market_data_audit, evaluate_event_completeness,
    evaluate_time_series_coverage, EventCompletenessObservation, PredictionMarketDataAuditReport,
    PredictionMarketDataAuditRequest, TimeSeriesCoverageObservation, BINANCE_AGG_TRADE_SURFACE,
    BINANCE_LOB_SURFACE, BINANCE_PRICE_SURFACE, CHAINLINK_REFERENCE_SURFACE, PM_ORDERBOOK_SURFACE,
    PM_SETTLEMENT_SURFACE,
};
use ploy_market_data::polymarket_evidence::{
    PolymarketEvidenceBook, PolymarketEvidenceContract, PolymarketEvidenceReference,
    PolymarketEvidenceSettlement, VerifiedPolymarketEvidenceSet,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedArtifactAuditRequest {
    pub symbol: String,
    pub snapshot_start: DateTime<Utc>,
    pub snapshot_end: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy)]
struct SourceClock {
    source: DateTime<Utc>,
    received: DateTime<Utc>,
}

pub fn build_prediction_market_data_audit_from_verified_artifacts(
    binance: &VerifiedBinanceMarketTape,
    polymarket: &VerifiedPolymarketEvidenceSet,
    input: VerifiedArtifactAuditRequest,
) -> Result<PredictionMarketDataAuditReport> {
    let request = governed_request(&input)?;
    let coverage_start = request
        .coverage_start()
        .context("verified artifact audit coverage window overflows")?;
    let segments = binance.segments();
    let first = segments.first().context("verified Binance tape is empty")?;
    let last = segments.last().context("verified Binance tape is empty")?;
    ensure!(
        segments
            .iter()
            .all(|segment| segment.market == Market::Spot),
        "prediction audit requires a verified Binance spot tape"
    );
    let binance_start = datetime_from_ns(first.start_received_at_ns)?;
    let binance_end = datetime_from_ns(last.end_received_at_ns)?;
    validate_artifact_windows(
        &request,
        binance_start,
        binance_end,
        polymarket.event_start_gte(),
        polymarket.event_start_lt(),
    )?;

    let trade_clocks = binance
        .aggregate_trades()
        .iter()
        .filter(|trade| trade.symbol == input.symbol.as_str())
        .map(|trade| {
            Ok(SourceClock {
                source: datetime_from_ms(trade.trade_time_ms)?,
                received: datetime_from_ns(trade.received_at_ns)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let lob_clocks = binance
        .lob_observations()
        .iter()
        .filter(|observation| observation.symbol == input.symbol.as_str())
        .map(|observation| {
            Ok(SourceClock {
                source: datetime_from_ms(observation.source_time_ms)?,
                received: datetime_from_ns(observation.received_at_ns)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut results = Vec::with_capacity(6);
    for (surface, clocks) in [
        (BINANCE_PRICE_SURFACE, trade_clocks.as_slice()),
        (BINANCE_AGG_TRADE_SURFACE, trade_clocks.as_slice()),
        (BINANCE_LOB_SURFACE, lob_clocks.as_slice()),
    ] {
        results.push(evaluate_time_series_coverage(
            &request,
            time_series_observation(
                &request,
                coverage_start,
                surface,
                &input.symbol,
                clocks.iter().copied(),
            )?,
        ));
    }
    results.push(evaluate_time_series_coverage(
        &request,
        chainlink_reference_observation(
            &request,
            &input.symbol,
            polymarket.contracts(),
            polymarket.references(),
        )?,
    ));
    let mut book_request = request.clone();
    book_request.minimum_coverage_bps = 10_000;
    results.push(evaluate_time_series_coverage(
        &book_request,
        polymarket_book_observation(
            &request,
            &input.symbol,
            polymarket.contracts(),
            polymarket.books(),
        )?,
    ));
    results.push(evaluate_event_completeness(settlement_observation(
        &request,
        &input.symbol,
        polymarket.contracts(),
        polymarket.settlements(),
    )?));

    let generated_at = polymarket
        .contracts()
        .map(|row| row.available_at)
        .chain(polymarket.books().map(|row| row.available_at))
        .chain(polymarket.references().map(|row| row.available_at))
        .chain(polymarket.trades().map(|row| row.available_at))
        .chain(polymarket.settlements().map(|row| row.observed_at))
        .fold(binance_end, |current, observed| current.max(observed));
    ensure!(
        generated_at >= request.snapshot_end,
        "verified evidence was not available through the snapshot end"
    );
    Ok(assemble_prediction_market_data_audit(
        request,
        generated_at,
        results,
        Vec::new(),
    ))
}

fn governed_request(
    input: &VerifiedArtifactAuditRequest,
) -> Result<PredictionMarketDataAuditRequest> {
    ensure!(
        matches!(input.symbol.as_str(), "BTCUSDT" | "SOLUSDT"),
        "verified artifact audit supports one BTCUSDT or SOLUSDT mission"
    );
    ensure!(
        input.snapshot_start.timestamp().rem_euclid(300) == 0
            && input.snapshot_start.timestamp_subsec_nanos() == 0
            && input.snapshot_end.timestamp().rem_euclid(300) == 0
            && input.snapshot_end.timestamp_subsec_nanos() == 0,
        "verified artifact audit window must align to full five-minute events"
    );
    let mut request = PredictionMarketDataAuditRequest::btc_sol_five_minute(
        input.snapshot_start,
        input.snapshot_end,
    );
    request.symbols = vec![input.symbol.clone()];
    ensure!(
        request.validation_findings().is_empty(),
        "verified artifact audit request is invalid"
    );
    Ok(request)
}

fn validate_artifact_windows(
    request: &PredictionMarketDataAuditRequest,
    binance_start: DateTime<Utc>,
    binance_end: DateTime<Utc>,
    polymarket_start: DateTime<Utc>,
    polymarket_end: DateTime<Utc>,
) -> Result<()> {
    let coverage_start = request
        .coverage_start()
        .context("verified artifact audit coverage window overflows")?;
    ensure!(
        binance_start <= coverage_start && binance_end >= request.snapshot_end,
        "verified Binance tape does not cover the audit warmup and snapshot window"
    );
    ensure!(
        polymarket_start <= request.snapshot_start && polymarket_end >= request.snapshot_end,
        "verified Polymarket evidence does not cover the snapshot window"
    );
    Ok(())
}

fn datetime_from_ns(value: u64) -> Result<DateTime<Utc>> {
    let seconds = i64::try_from(value / 1_000_000_000).context("nanosecond time overflow")?;
    DateTime::from_timestamp(seconds, (value % 1_000_000_000) as u32)
        .context("nanosecond time is outside DateTime range")
}

fn datetime_from_ms(value: u64) -> Result<DateTime<Utc>> {
    let milliseconds = i64::try_from(value).context("millisecond time overflow")?;
    DateTime::from_timestamp_millis(milliseconds)
        .context("millisecond time is outside DateTime range")
}

fn expected_bucket_count(
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    bucket_secs: i64,
) -> Result<u64> {
    let duration_secs = (end - start).num_seconds();
    ensure!(
        bucket_secs > 0 && duration_secs > 0 && duration_secs.rem_euclid(bucket_secs) == 0,
        "invalid or misaligned audit bucket window"
    );
    u64::try_from(duration_secs / bucket_secs).context("audit bucket count overflows")
}

fn maximum_missing_gap(
    expected_buckets: u64,
    present: &BTreeSet<u64>,
    bucket_secs: i64,
) -> Result<u64> {
    let mut current = 0_u64;
    let mut maximum = 0_u64;
    for bucket in 0..expected_buckets {
        if present.contains(&bucket) {
            current = 0;
        } else {
            current = current.checked_add(1).context("audit gap overflows")?;
            maximum = maximum.max(current);
        }
    }
    maximum
        .checked_mul(u64::try_from(bucket_secs)?)
        .context("audit gap duration overflows")
}

fn time_series_observation(
    request: &PredictionMarketDataAuditRequest,
    start: DateTime<Utc>,
    surface: &str,
    symbol: &str,
    clocks: impl IntoIterator<Item = SourceClock>,
) -> Result<TimeSeriesCoverageObservation> {
    let expected_buckets = expected_bucket_count(start, request.snapshot_end, request.bucket_secs)?;
    let maximum_delay = Duration::seconds(request.maximum_source_delay_secs);
    let mut row_count = 0_u64;
    let mut usable_row_count = 0_u64;
    let mut source_delay_rejected_rows = 0_u64;
    let mut causality_violations = 0_u64;
    let mut present = BTreeSet::new();
    let mut first_at: Option<DateTime<Utc>> = None;
    let mut last_at: Option<DateTime<Utc>> = None;
    for clock in clocks {
        if clock.source < start || clock.source >= request.snapshot_end {
            continue;
        }
        row_count = row_count
            .checked_add(1)
            .context("audit row count overflows")?;
        if clock.received < clock.source {
            causality_violations += 1;
        } else if clock.received - clock.source > maximum_delay {
            source_delay_rejected_rows += 1;
        } else {
            usable_row_count += 1;
            first_at = Some(first_at.map_or(clock.source, |value| value.min(clock.source)));
            last_at = Some(last_at.map_or(clock.source, |value| value.max(clock.source)));
            present.insert(u64::try_from(
                (clock.source - start).num_seconds() / request.bucket_secs,
            )?);
        }
    }
    Ok(TimeSeriesCoverageObservation {
        surface: surface.to_owned(),
        symbol: symbol.to_owned(),
        row_count,
        usable_row_count,
        source_delay_rejected_rows,
        invalid_payload_rows: 0,
        causality_violations,
        first_at,
        last_at,
        expected_buckets,
        present_buckets: u64::try_from(present.len())?,
        max_gap_secs: maximum_missing_gap(expected_buckets, &present, request.bucket_secs)?,
        query_ms: 0,
    })
}

fn chainlink_reference_observation<'a, 'b>(
    request: &PredictionMarketDataAuditRequest,
    symbol: &str,
    contracts: impl IntoIterator<Item = &'a PolymarketEvidenceContract>,
    references: impl IntoIterator<Item = &'b PolymarketEvidenceReference>,
) -> Result<TimeSeriesCoverageObservation> {
    let mut events = contracts
        .into_iter()
        .filter(|contract| {
            contract.symbol == symbol
                && contract.event_start >= request.snapshot_start
                && contract.event_end <= request.snapshot_end
        })
        .collect::<Vec<_>>();
    events.sort_by(|left, right| {
        (left.event_start, left.market_id.as_str())
            .cmp(&(right.event_start, right.market_id.as_str()))
    });
    let event_index = events
        .iter()
        .enumerate()
        .map(|(i, row)| (row.market_id.as_str(), (i as u64, row.event_start)))
        .collect::<BTreeMap<_, _>>();
    let expected_buckets = u64::try_from(events.len())?;
    let maximum_delay = Duration::seconds(request.maximum_source_delay_secs);
    let mut row_count = 0_u64;
    let mut usable_row_count = 0_u64;
    let mut source_delay_rejected_rows = 0_u64;
    let mut causality_violations = 0_u64;
    let mut present = BTreeSet::new();
    let mut first_at: Option<DateTime<Utc>> = None;
    let mut last_at: Option<DateTime<Utc>> = None;
    for reference in references {
        let Some(&(event, event_start)) = event_index.get(reference.market_id.as_str()) else {
            continue;
        };
        if reference.source_time >= event_start || reference.is_carried_forward {
            continue;
        }
        row_count = row_count
            .checked_add(1)
            .context("audit row count overflows")?;
        if reference.available_at < reference.source_time {
            causality_violations += 1;
        } else if reference.available_at - reference.source_time > maximum_delay {
            source_delay_rejected_rows += 1;
        } else {
            usable_row_count += 1;
            first_at = Some(first_at.map_or(reference.source_time, |value| {
                value.min(reference.source_time)
            }));
            last_at = Some(last_at.map_or(reference.source_time, |value| {
                value.max(reference.source_time)
            }));
            present.insert(event);
        }
    }
    // Completeness is one causally usable reference per full five-minute event.
    Ok(TimeSeriesCoverageObservation {
        surface: CHAINLINK_REFERENCE_SURFACE.to_owned(),
        symbol: symbol.to_owned(),
        row_count,
        usable_row_count,
        source_delay_rejected_rows,
        invalid_payload_rows: 0,
        causality_violations,
        first_at,
        last_at,
        expected_buckets,
        present_buckets: u64::try_from(present.len())?,
        max_gap_secs: maximum_missing_gap(expected_buckets, &present, 300)?,
        query_ms: 0,
    })
}

fn polymarket_book_observation<'a, 'b>(
    request: &PredictionMarketDataAuditRequest,
    symbol: &str,
    contracts: impl IntoIterator<Item = &'a PolymarketEvidenceContract>,
    books: impl IntoIterator<Item = &'b PolymarketEvidenceBook>,
) -> Result<TimeSeriesCoverageObservation> {
    let mut windows = BTreeMap::new();
    for contract in contracts {
        if contract.symbol != symbol
            || contract.event_start < request.snapshot_start
            || contract.event_end > request.snapshot_end
        {
            continue;
        }
        let start = contract.event_start;
        let end = contract.event_end;
        for token in [&contract.up_token_id, &contract.down_token_id] {
            ensure!(
                windows
                    .insert((contract.market_id.as_str(), token.as_str()), (start, end))
                    .is_none(),
                "verified Polymarket audit contains a duplicate token window"
            );
        }
    }

    // Count every event-lifetime bucket per token, without legacy discovery padding.
    let mut expected_buckets = 0_u64;
    let mut present = windows
        .keys()
        .copied()
        .map(|key| (key, BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    for (start, end) in windows.values() {
        expected_buckets = expected_buckets
            .checked_add(expected_bucket_count(*start, *end, request.bucket_secs)?)
            .context("Polymarket expected bucket count overflows")?;
    }
    let maximum_delay = Duration::seconds(request.maximum_source_delay_secs);
    let mut row_count = 0_u64;
    let mut usable_row_count = 0_u64;
    let mut invalid_payload_rows = 0_u64;
    let mut first_at: Option<DateTime<Utc>> = None;
    let mut last_at: Option<DateTime<Utc>> = None;
    for book in books {
        let key = (book.market_id.as_str(), book.token_id.as_str());
        let Some(&(start, end)) = windows.get(&key) else {
            continue;
        };
        if book.source_time < start || book.source_time >= end {
            continue;
        }
        row_count = row_count
            .checked_add(1)
            .context("audit row count overflows")?;
        if book.available_at - book.source_time > maximum_delay {
            invalid_payload_rows += 1;
            continue;
        }
        let usable = book
            .bid_levels
            .as_ref()
            .is_some_and(|levels| !levels.is_empty())
            && book
                .ask_levels
                .as_ref()
                .is_some_and(|levels| !levels.is_empty());
        if !usable {
            invalid_payload_rows += 1;
            continue;
        }
        usable_row_count += 1;
        first_at = Some(first_at.map_or(book.source_time, |value| value.min(book.source_time)));
        last_at = Some(last_at.map_or(book.source_time, |value| value.max(book.source_time)));
        present
            .get_mut(&key)
            .expect("verified token window exists")
            .insert(u64::try_from(
                (book.source_time - start).num_seconds() / request.bucket_secs,
            )?);
    }
    let present_buckets = u64::try_from(present.values().map(BTreeSet::len).sum::<usize>())?;
    let mut max_gap_secs = 0_u64;
    for (key, (start, end)) in &windows {
        max_gap_secs = max_gap_secs.max(maximum_missing_gap(
            expected_bucket_count(*start, *end, request.bucket_secs)?,
            present.get(key).expect("verified token window exists"),
            request.bucket_secs,
        )?);
    }
    Ok(TimeSeriesCoverageObservation {
        surface: PM_ORDERBOOK_SURFACE.to_owned(),
        symbol: symbol.to_owned(),
        row_count,
        usable_row_count,
        source_delay_rejected_rows: 0,
        invalid_payload_rows,
        causality_violations: 0,
        first_at,
        last_at,
        expected_buckets,
        present_buckets,
        max_gap_secs,
        query_ms: 0,
    })
}

fn settlement_observation<'a, 'b>(
    request: &PredictionMarketDataAuditRequest,
    symbol: &str,
    contracts: impl IntoIterator<Item = &'a PolymarketEvidenceContract>,
    settlements: impl IntoIterator<Item = &'b PolymarketEvidenceSettlement>,
) -> Result<EventCompletenessObservation> {
    let settlements = settlements
        .into_iter()
        .map(|settlement| settlement.market_id.as_str())
        .collect::<BTreeSet<_>>();
    let mut eligible_events = 0_u64;
    let mut complete_events = 0_u64;
    let mut resolved_tokens = 0_u64;
    for contract in contracts {
        if contract.symbol != symbol
            || contract.event_start < request.snapshot_start
            || contract.event_end > request.snapshot_end
        {
            continue;
        }
        eligible_events = eligible_events
            .checked_add(1)
            .context("settlement event count overflows")?;
        // The private verified-set constructor already binds the winner to the exact 1/0 pair.
        if settlements.contains(contract.market_id.as_str()) {
            complete_events = complete_events
                .checked_add(1)
                .context("complete settlement count overflows")?;
            resolved_tokens = resolved_tokens
                .checked_add(2)
                .context("settlement token count overflows")?;
        }
    }
    Ok(EventCompletenessObservation {
        surface: PM_SETTLEMENT_SURFACE.to_owned(),
        symbol: symbol.to_owned(),
        eligible_events,
        complete_events,
        expected_tokens: eligible_events
            .checked_mul(2)
            .context("expected settlement token count overflows")?,
        resolved_tokens,
        query_ms: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ploy_market_data::diagnostics::{
        evaluate_time_series_coverage, AuditStatus, AuditSurfaceMetrics, AuditSurfaceResult,
    };
    use ploy_market_data::polymarket_evidence::{
        BinaryOutcomeSide, PolymarketBookLevel, PolymarketEvidenceBook, PolymarketEvidenceContract,
        PolymarketEvidenceReference,
    };
    use rust_decimal::Decimal;

    fn time(value: &str) -> DateTime<Utc> {
        value.parse().unwrap()
    }

    fn request() -> PredictionMarketDataAuditRequest {
        governed_request(&VerifiedArtifactAuditRequest {
            symbol: "BTCUSDT".to_owned(),
            snapshot_start: time("2026-07-17T05:30:00Z"),
            snapshot_end: time("2026-07-17T05:35:00Z"),
        })
        .unwrap()
    }

    fn contract() -> PolymarketEvidenceContract {
        PolymarketEvidenceContract {
            market_id: "market-1".to_owned(),
            condition_id: "condition-1".to_owned(),
            symbol: "BTCUSDT".to_owned(),
            event_start: time("2026-07-17T05:30:00Z"),
            event_end: time("2026-07-17T05:35:00Z"),
            up_token_id: "up-token".to_owned(),
            down_token_id: "down-token".to_owned(),
            price_to_beat: Decimal::from(63_000),
            resolution_source: "chainlink".to_owned(),
            available_at: time("2026-07-17T05:29:59Z"),
        }
    }

    #[test]
    fn governed_request_rejects_misaligned_event_windows() {
        assert!(governed_request(&VerifiedArtifactAuditRequest {
            symbol: "BTCUSDT".to_owned(),
            snapshot_start: time("2026-07-17T05:30:01Z"),
            snapshot_end: time("2026-07-17T05:35:00Z"),
        })
        .is_err());
    }

    fn book(token_id: &str, available_at: DateTime<Utc>) -> PolymarketEvidenceBook {
        let level = PolymarketBookLevel {
            price: Decimal::new(5, 1),
            size: Decimal::from(10),
        };
        PolymarketEvidenceBook {
            market_id: "market-1".to_owned(),
            token_id: token_id.to_owned(),
            side: if token_id == "up-token" {
                BinaryOutcomeSide::Up
            } else {
                BinaryOutcomeSide::Down
            },
            source_time: available_at,
            available_at,
            bid: Some(level.price),
            ask: Some(level.price),
            bid_size: Some(level.size),
            ask_size: Some(level.size),
            bid_levels: Some(vec![level.clone()]),
            ask_levels: Some(vec![level]),
        }
    }

    fn reference(contract: &PolymarketEvidenceContract) -> PolymarketEvidenceReference {
        let source_time = contract.event_start - Duration::seconds(30);
        PolymarketEvidenceReference {
            market_id: contract.market_id.clone(),
            source_time,
            price: contract.price_to_beat,
            is_carried_forward: false,
            available_at: source_time + Duration::seconds(1),
        }
    }

    fn assert_time_series(
        result: &AuditSurfaceResult,
        status: AuditStatus,
        buckets: (u64, u64),
        max_gap_secs: u64,
        rejected_rows: (u64, u64),
    ) {
        assert_eq!(result.status, status);
        let AuditSurfaceMetrics::TimeSeries {
            expected_buckets,
            present_buckets,
            source_delay_rejected_rows,
            invalid_payload_rows,
            max_gap_secs: actual_gap,
            ..
        } = &result.metrics
        else {
            panic!("expected time-series audit metrics");
        };
        assert_eq!((*expected_buckets, *present_buckets), buckets);
        assert_eq!(*actual_gap, max_gap_secs);
        assert_eq!((*source_delay_rejected_rows, *invalid_payload_rows), rejected_rows);
    }

    #[test]
    fn chainlink_gate_requires_one_usable_reference_per_event() {
        let mut request = request();
        request.snapshot_end += Duration::minutes(5);
        let first = contract();
        let mut second = contract();
        second.market_id = "market-2".to_owned();
        second.condition_id = "condition-2".to_owned();
        second.up_token_id = "up-token-2".to_owned();
        second.down_token_id = "down-token-2".to_owned();
        second.event_start = first.event_end;
        second.event_end = second.event_start + Duration::minutes(5);
        let contracts = [first, second];
        let mut references = contracts.iter().map(reference).collect::<Vec<_>>();
        let evaluate = |rows: &[PolymarketEvidenceReference]| {
            evaluate_time_series_coverage(
                &request,
                chainlink_reference_observation(&request, "BTCUSDT", contracts.iter(), rows.iter())
                    .unwrap(),
            )
        };
        let healthy = evaluate(&references);
        assert_time_series(&healthy, AuditStatus::Ok, (2, 2), 0, (0, 0));
        let mut post_open = reference(&contracts[0]);
        post_open.source_time = contracts[0].event_start;
        post_open.available_at = post_open.source_time;
        references.push(post_open);
        assert_time_series(&evaluate(&references), AuditStatus::Ok, (2, 2), 0, (0, 0));
        references.pop();
        let late = contracts[0].event_start;
        references[0].source_time = late;
        references[0].available_at = late;
        let missing = evaluate(&references);
        assert_time_series(&missing, AuditStatus::Critical, (2, 1), 300, (0, 0));
        references[0] = reference(&contracts[0]);
        references[0].is_carried_forward = true;
        let carried = evaluate(&references);
        assert_time_series(&carried, AuditStatus::Critical, (2, 1), 300, (0, 0));
    }

    #[test]
    fn polymarket_book_gate_uses_every_event_lifetime_bucket() {
        let request = request();
        let mut book_request = request.clone();
        book_request.minimum_coverage_bps = 10_000;
        let coverage_start = request.coverage_start().unwrap();
        assert!(validate_artifact_windows(
            &request,
            coverage_start,
            request.snapshot_end,
            request.snapshot_start + Duration::minutes(5),
            request.snapshot_end,
        )
        .is_err());
        let contract = contract();
        let mut books = Vec::new();
        for minute in 0..5 {
            for token in ["up-token", "down-token"] {
                books.push(book(
                    token,
                    contract.event_start + Duration::minutes(minute),
                ));
            }
        }
        let evaluate = |rows: &[PolymarketEvidenceBook]| {
            evaluate_time_series_coverage(
                &book_request,
                polymarket_book_observation(&request, "BTCUSDT", [&contract], rows.iter()).unwrap(),
            )
        };
        let healthy = evaluate(&books);
        assert_time_series(&healthy, AuditStatus::Ok, (10, 10), 0, (0, 0));
        let mut stale_extra = books[0].clone();
        stale_extra.available_at += Duration::seconds(31);
        books.push(stale_extra);
        assert_time_series(&evaluate(&books), AuditStatus::Ok, (10, 10), 0, (0, 1));
        books.pop();
        let ask_levels = books[0].ask_levels.take();
        let one_sided = evaluate(&books);
        assert_time_series(&one_sided, AuditStatus::Critical, (10, 9), 60, (0, 1));
        books[0].ask_levels = ask_levels;
        books[0].available_at += Duration::seconds(31);
        let late_book = evaluate(&books);
        assert_time_series(&late_book, AuditStatus::Critical, (10, 9), 60, (0, 1));
        books[0].available_at = books[0].source_time;
        let delayed = books.last_mut().unwrap();
        delayed.source_time = contract.event_end - Duration::seconds(1);
        delayed.available_at = contract.event_end;
        let source_complete = evaluate(&books);
        assert_eq!(source_complete.status, AuditStatus::Ok);
        books.pop();
        let mut missing =
            polymarket_book_observation(&request, "BTCUSDT", [&contract], books.iter()).unwrap();
        let short_window = evaluate_time_series_coverage(&book_request, missing.clone());
        assert_time_series(&short_window, AuditStatus::Critical, (10, 9), 60, (0, 0));
        (missing.expected_buckets, missing.present_buckets) = (1_000, 999);
        let long_window = evaluate_time_series_coverage(&book_request, missing);
        assert_time_series(&long_window, AuditStatus::Critical, (1_000, 999), 60, (0, 0));
    }
}
