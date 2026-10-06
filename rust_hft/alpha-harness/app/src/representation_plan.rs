//! Read-only adapter from externally verified market-tape handles to representation proposals.
use alpha_domain::{representation::*, CexResearchContentRefV1};
use anyhow::{bail, ensure, Context, Result};
use data::{
    binance_lob_replay::ReplaySequenceEvent,
    binance_market_tape_artifact::{ReplayedBinanceBookEvent, VerifiedBinanceMarketTapeSeries},
};
use hft_collector::{
    binance_spot_reference_artifact::VerifiedSpotReferenceArtifact,
    binance_usdm_reference_artifact::VerifiedReferenceArtifact,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

/// Borrow only source-bound handles returned by the original rule artifact verifiers.
pub enum VerifiedInstrumentRuleReferences<'a> {
    Spot(&'a [VerifiedSpotReferenceArtifact]),
    Usdm(&'a [VerifiedReferenceArtifact]),
}

/// This handle cannot be restored from JSON. Reverify raw objects after a process restart.
/// It confirms raw integrity and replay continuity, not permission, scientific success or execution admission.
#[derive(Debug)]
pub struct VerifiedTapeRepresentationProposal {
    capability: DataCapabilityV1,
    proposal: RepresentationPlanV1,
}
impl VerifiedTapeRepresentationProposal {
    pub fn capability(&self) -> &DataCapabilityV1 {
        &self.capability
    }
    pub fn proposal(&self) -> &RepresentationPlanV1 {
        &self.proposal
    }
    pub fn to_readonly_json(&self) -> Result<serde_json::Value> {
        Ok(serde_json::json!({
            "schema": "monday.verified_tape_representation_proposal.v1",
            "raw_verification": "externally_anchored_market_tape_and_sequence_checked_replay",
            "permission_and_execution": "requires_current_native_admission",
            "capability": self.capability,
            "proposal": self.proposal,
            "proposal_sha256": self.proposal.digest().map_err(anyhow::Error::msg)?,
        }))
    }
}

/// Call only after the original externally anchored series verifier succeeds.
/// This function does not acquire data, fit models, call an LLM, debit budget or submit a Campaign.
pub fn plan_from_verified_tape(
    series: &[VerifiedBinanceMarketTapeSeries],
    view: PlanningViewV1,
    goal: &RepresentationGoalV1,
) -> Result<VerifiedTapeRepresentationProposal> {
    plan_from_verified_tape_inner(series, None, view, goal)
}

/// Propose materials only after source-bound rules cover the same instrument and window.
/// This still does not resolve a scientific trial budget or native execution authority.
pub fn plan_from_verified_tape_with_rules(
    series: &[VerifiedBinanceMarketTapeSeries],
    references: VerifiedInstrumentRuleReferences<'_>,
    view: PlanningViewV1,
    goal: &RepresentationGoalV1,
) -> Result<VerifiedTapeRepresentationProposal> {
    plan_from_verified_tape_inner(series, Some(references), view, goal)
}

fn plan_from_verified_tape_inner(
    series: &[VerifiedBinanceMarketTapeSeries],
    references: Option<VerifiedInstrumentRuleReferences<'_>>,
    view: PlanningViewV1,
    goal: &RepresentationGoalV1,
) -> Result<VerifiedTapeRepresentationProposal> {
    goal.validate().map_err(anyhow::Error::msg)?;
    ensure!(
        !series.is_empty() && series.len() <= MAX_CAPABILITY_SERIES,
        "bounded verified series required"
    );
    let mut sources = Vec::new();
    let mut seen_sources = BTreeSet::new();
    let mut summaries = Vec::new();
    let mut market = None;
    let mut collection_scope = None;
    let mut seen_sessions = BTreeSet::new();
    let mut previous_segment_end = None;
    let mut latest_receive = 0;
    let mut latest_available = 0;
    let mut latest_event = None;
    let mut directional_trades = true;
    for series in series {
        let verified = series.verified();
        ensure!(!verified.segments().is_empty(), "empty verified source");
        let scope = (verified.dataset(), verified.shard_id());
        ensure!(
            collection_scope.get_or_insert(scope) == &scope,
            "verified series do not share one dataset/shard scope"
        );
        ensure!(
            seen_sessions.insert(series.session_id()),
            "verified capture session reappeared in the planned collection"
        );
        for segment in verified.segments() {
            let current = segment.market.as_str();
            ensure!(
                market.get_or_insert(current) == &current,
                "mixed market sources"
            );
            ensure!(
                previous_segment_end.is_none_or(|end| segment.start_received_at_ns >= end),
                "verified collection receive time moved backwards across segments"
            );
            previous_segment_end = Some(segment.end_received_at_ns);
            for (kind, digest) in [
                ("market-tape", &segment.content_sha256),
                ("market-manifest", &segment.manifest_sha256),
            ] {
                if seen_sources.insert(digest.clone()) {
                    ensure!(
                        sources.len() < MAX_CAPABILITY_SOURCES,
                        "too many immutable source identities"
                    );
                    sources.push(CexResearchContentRefV1 {
                        id: format!("{kind}-{digest}"),
                        content_sha256: digest.clone(),
                    });
                }
            }
        }
        let book = verified
            .replayed_books()
            .iter()
            .find(|book| book.symbol == goal.symbol)
            .context("requested instrument has no verified book")?;
        let mut current: Option<BookSeriesCapabilityV1> = None;
        let mut seed_number = 0;
        for event in book.events() {
            let received = event.received_at_ns();
            if received > goal.window_end_ns {
                break;
            }
            match event {
                ReplayedBinanceBookEvent::Replay(ReplaySequenceEvent::Snapshot {
                    clock,
                    bids,
                    asks,
                    ..
                }) => {
                    if let Some(previous) = current.take() {
                        summaries.push(previous);
                    }
                    seed_number += 1;
                    let depth = u16::try_from(bids.len().min(asks.len()))
                        .context("captured seed depth overflow")?;
                    let original = clock
                        .as_ref()
                        .context("verified seed lost its original clock")?;
                    ensure!(
                        original.source.is_some(),
                        "verified seed lacks a sealed source row"
                    );
                    current = Some(BookSeriesCapabilityV1 {
                        session_id: format!("{}:seed-{seed_number}", series.session_id()),
                        start_available_ns: received,
                        end_available_ns: received,
                        snapshots: 1,
                        diffs: 0,
                        captured_seed_depth: depth,
                        continuity: BookContinuityV1::SnapshotOnly,
                    });
                    observe_clock(
                        received,
                        original.raw_received_at_ns,
                        original.exchange_event_time_ms,
                        &mut latest_available,
                        &mut latest_receive,
                        &mut latest_event,
                    )?;
                }
                ReplayedBinanceBookEvent::Replay(ReplaySequenceEvent::Diff { clock, .. }) => {
                    let original = clock
                        .as_ref()
                        .context("verified diff lost its original clock")?;
                    ensure!(
                        original.source.is_some(),
                        "verified diff lacks a sealed source row"
                    );
                    let summary = current
                        .as_mut()
                        .context("verified diff has no captured seed")?;
                    summary.diffs = summary
                        .diffs
                        .checked_add(1)
                        .context("diff count overflow")?;
                    summary.end_available_ns = received;
                    summary.continuity = BookContinuityV1::SequenceChecked;
                    observe_clock(
                        received,
                        original.raw_received_at_ns,
                        original.exchange_event_time_ms,
                        &mut latest_available,
                        &mut latest_receive,
                        &mut latest_event,
                    )?;
                }
                // Legacy H1/H2 and sequence renderers ignore checkpoints.
                // The shared coverage summary must not promise their quiet tail.
                ReplayedBinanceBookEvent::Checkpoint { .. } => {}
            }
            ensure!(
                summaries.len() < MAX_CAPABILITY_SERIES,
                "too many recovery series"
            );
        }
        if let Some(current) = current {
            summaries.push(current);
        }
        let has_trade_modality = verified
            .segments()
            .iter()
            .any(|segment| segment.trade_summaries.contains_key(&goal.symbol));
        let mut has_causal_trade = false;
        for trade in verified.aggregate_trades().iter().filter(|trade| {
            trade.symbol == goal.symbol && trade.received_at_ns <= goal.window_end_ns
        }) {
            let event_ns = trade
                .event_time_ms
                .checked_mul(1_000_000)
                .context("trade event clock overflow")?;
            ensure!(
                event_ns <= trade.received_at_ns,
                "future exchange trade clock is not a causal input"
            );
            has_causal_trade = true;
        }
        // The registered renderer consumes every supplied series and rejects
        // mixed trade modalities. One session cannot establish another's input.
        directional_trades &= has_trade_modality && has_causal_trade;
    }
    if latest_available == 0 || summaries.is_empty() {
        bail!("no observed book before the planned decision window");
    }
    let instrument_rules = references
        .map(|references| rule_coverage(references, goal))
        .transpose()?;
    if let Some(rules) = &instrument_rules {
        for reference in &rules.sources {
            if seen_sources.insert(reference.content_sha256.clone()) {
                ensure!(
                    sources.len() < MAX_CAPABILITY_SOURCES,
                    "too many immutable source identities"
                );
                sources.push(reference.clone());
            }
        }
    }
    let capability = DataCapabilityV1 {
        schema: CAPABILITY_SCHEMA.into(),
        venue: "binance".into(),
        market: market.context("no verified market")?.into(),
        symbol: goal.symbol.clone(),
        sources,
        normalizer: CexResearchContentRefV1 {
            id: "binance-market-tape-replay-summary-v1".into(),
            content_sha256: format!(
                "{:x}",
                Sha256::digest(
                    concat!(
                        include_str!("representation_plan.rs"),
                        include_str!("../../../data-pipelines/core/src/binance_lob_replay.rs"),
                        include_str!(
                            "../../../data-pipelines/core/src/binance_market_tape_artifact.rs"
                        ),
                        include_str!(
                            "../../../tools/collector/src/binance_spot_reference_artifact.rs"
                        ),
                        include_str!(
                            "../../../tools/collector/src/binance_usdm_reference_artifact.rs"
                        ),
                    )
                    .as_bytes()
                )
            ),
        },
        series: summaries,
        fields: vec![FieldClockV1 {
            field: "captured_book".into(),
            unit: "venue_price_and_base_quantity".into(),
            event_ns: latest_event,
            received_ns: latest_receive,
            available_ns: latest_available,
            decision_ns: goal.window_end_ns,
        }],
        aggregate_trade_direction: directional_trades,
        instrument_rules,
        view,
    };
    let proposal =
        alpha_engine::representation_plan::propose_representation_comparison(&capability, goal)
            .map_err(anyhow::Error::msg)?;
    Ok(VerifiedTapeRepresentationProposal {
        capability,
        proposal,
    })
}

fn rule_coverage(
    references: VerifiedInstrumentRuleReferences<'_>,
    goal: &RepresentationGoalV1,
) -> Result<InstrumentRuleCoverageV1> {
    let mut observations = Vec::new();
    let mut sources = Vec::new();
    let mut seen_sources = BTreeSet::new();
    let mut rule_identity = None;
    let mut observe = |received, identity: String, data: &str, manifest: &str| -> Result<()> {
        ensure!(
            rule_identity.get_or_insert_with(|| identity.clone()) == &identity,
            "instrument rules changed inside the supplied PIT window"
        );
        observations.push(received);
        for (kind, digest) in [
            ("instrument-rule-data", data),
            ("instrument-rule-manifest", manifest),
        ] {
            if seen_sources.insert(digest.to_owned()) {
                ensure!(
                    sources.len() < MAX_CAPABILITY_SOURCES,
                    "too many instrument-rule sources"
                );
                sources.push(CexResearchContentRefV1 {
                    id: format!("{kind}-{digest}"),
                    content_sha256: digest.into(),
                });
            }
        }
        Ok(())
    };
    match references {
        VerifiedInstrumentRuleReferences::Usdm(references) => {
            ensure!(
                goal.market == "usdm"
                    && !references.is_empty()
                    && references.len() <= MAX_CAPABILITY_SOURCES / 2,
                "nonempty USD-M rule artifacts must match the goal market"
            );
            for reference in references {
                let rule = reference
                    .contracts()
                    .iter()
                    .find(|rule| rule.symbol == goal.symbol)
                    .context("verified USD-M rule artifact lacks the requested instrument")?;
                let identity = alpha_domain::canonical_json_hash(&(
                    rule.tick_size,
                    rule.step_size,
                    rule.min_notional,
                ))?;
                observe(
                    rule.received_at_ns,
                    identity,
                    reference.data_sha256(),
                    reference.manifest_sha256(),
                )?;
            }
        }
        VerifiedInstrumentRuleReferences::Spot(references) => {
            ensure!(
                goal.market == "spot"
                    && !references.is_empty()
                    && references.len() <= MAX_CAPABILITY_SOURCES / 2,
                "nonempty Spot rule artifacts must match the goal market"
            );
            for reference in references {
                let rule = reference
                    .rules()
                    .iter()
                    .find(|rule| rule.symbol == goal.symbol)
                    .context("verified Spot rule artifact lacks the requested instrument")?;
                ensure!((rule.price_filter.tick_size.is_sign_positive() && !rule.price_filter.tick_size.is_zero())
                    && (rule.lot_size_filter.step_size.is_sign_positive() && !rule.lot_size_filter.step_size.is_zero())
                    && (rule.lot_size_filter.min_quantity.is_sign_positive() && !rule.lot_size_filter.min_quantity.is_zero())
                    && (rule.lot_size_filter.max_quantity.is_sign_positive() && !rule.lot_size_filter.max_quantity.is_zero())
                    && rule.market_lot_size_filter.as_ref().is_none_or(|filter| (filter.max_quantity.is_sign_negative() || filter.max_quantity.is_zero()) || filter.min_quantity <= filter.max_quantity)
                    && rule.notional_filter.max_notional.is_none_or(|max| (max.is_sign_positive() && !max.is_zero()) && max >= rule.notional_filter.min_notional),
                    "verified Spot rule artifact has disabled or invalid materialization fill bounds");
                let mut identity = serde_json::to_value(rule)?;
                let identity = identity
                    .as_object_mut()
                    .context("Spot rule identity is not an object")?;
                for clock in [
                    "source_time_ms",
                    "source_clock_received_at_ns",
                    "received_at_ns",
                ] {
                    identity.remove(clock);
                }
                let identity = alpha_domain::canonical_json_hash(identity)?;
                observe(
                    rule.received_at_ns,
                    identity,
                    reference.data_sha256(),
                    reference.manifest_sha256(),
                )?;
            }
        }
    }
    observations.sort_unstable();
    observations.dedup();
    let lookback_start = goal
        .window_start_ns
        .checked_sub(60_000_000_000)
        .context("representation lookback underflow")?;
    let horizon_ns = goal
        .labels
        .observation_frequency_millis
        .checked_mul(goal.labels.horizon_buckets as u64)
        .and_then(|millis| millis.checked_mul(1_000_000))
        .context("label availability window overflow")?;
    let label_end = goal
        .window_end_ns
        .checked_add(horizon_ns)
        .context("label availability window overflow")?;
    let before = observations
        .iter()
        .rposition(|&time| time <= lookback_start)
        .context("instrument-rule coverage starts after the required lookback")?;
    let after = observations
        .iter()
        .position(|&time| time >= label_end)
        .context("instrument-rule coverage ends before label availability")?;
    let selected = &observations[before..=after];
    let max_gap_ns = selected
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .max()
        .unwrap_or(0);
    ensure!(
        max_gap_ns <= hft_research_manifest::CEX_DERIVATIVES_MAX_GAP_NS,
        "instrument-rule coverage has a gap above 90s"
    );
    Ok(InstrumentRuleCoverageV1 {
        market: goal.market.clone(),
        symbol: goal.symbol.clone(),
        sources,
        rules_identity_sha256: rule_identity.context("instrument rules are missing")?,
        first_available_ns: selected[0],
        last_available_ns: *selected.last().unwrap(),
        max_gap_ns,
    })
}

fn observe_clock(
    available: u64,
    received: u64,
    exchange_ms: Option<u64>,
    latest_available: &mut u64,
    latest_receive: &mut u64,
    latest_event: &mut Option<u64>,
) -> Result<()> {
    let event = exchange_ms
        .map(|ms| ms.checked_mul(1_000_000).context("exchange clock overflow"))
        .transpose()?;
    ensure!(
        received <= available && event.is_none_or(|event| event <= available),
        "future field is unavailable at its decision clock"
    );
    if available >= *latest_available {
        *latest_available = available;
        *latest_receive = received;
        *latest_event = event;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    // Synthetic protocol case for software verification; never research evidence.
    const RAW: &str = r###"{"schema":"binance.market_tape.v1","received_at_ns":1700000000000000000,"type":"session_start","session_id":"session-1","market":"usdm","symbols":1,"websocket_shards":2,"websocket_streams":2}
{"schema":"binance.market_tape.v1","received_at_ns":1700000000000000001,"type":"stream_coverage","session_id":"session-1","shards":[["btcusdt@depth@100ms"],["btcusdt@aggTrade"]]}
{"schema":"binance.market_tape.v1","received_at_ns":1700000000100000000,"type":"snapshot","session_id":"session-1","symbol":"BTCUSDT","request_started_at_ns":1700000000050000000,"snapshot":{"lastUpdateId":100,"bids":[["100","1"],["99","1"],["98","1"],["97","1"],["96","1"]],"asks":[["101","1"],["102","1"],["103","1"],["104","1"],["105","1"]]}}
{"schema":"binance.market_tape.v1","received_at_ns":1700000000200000000,"type":"diff","session_id":"session-1","frame":{"data":{"e":"depthUpdate","E":1700000000200,"T":1700000000200,"s":"BTCUSDT","U":101,"u":101,"pu":100,"b":[["100","2"]],"a":[]}}}
{"schema":"binance.market_tape.v1","received_at_ns":1700000000300000000,"type":"agg_trade","session_id":"session-1","frame":{"stream":"btcusdt@aggTrade","data":{"e":"aggTrade","E":1700000000300,"s":"BTCUSDT","a":10,"p":"100.5","q":"2","f":10,"l":10,"T":1700000000300,"m":false}}}
{"schema":"binance.market_tape.v1","received_at_ns":1700000120000000000,"type":"diff","session_id":"session-1","frame":{"data":{"e":"depthUpdate","E":1700000120000,"T":1700000120000,"s":"BTCUSDT","U":102,"u":102,"pu":101,"b":[["100","2"]],"a":[]}}}
{"schema":"binance.market_tape.v1","received_at_ns":1700000120100000000,"type":"checkpoint","session_id":"session-1","symbol":"BTCUSDT","last_update_id":102,"synced":true,"bridged":true,"continuity_complete":true,"stream_coverage_verified":true,"bids":[["100","2"],["99","1"],["98","1"],["97","1"],["96","1"]],"asks":[["101","1"],["102","1"],["103","1"],["104","1"],["105","1"]],"replay_safe":true,"reason":"test"}
"###;
    const COMPRESSED: &[u8] = &[
        40, 181, 47, 253, 4, 72, 53, 17, 0, 6, 35, 98, 36, 32, 141, 182, 13, 32, 100, 52, 89, 179,
        217, 127, 76, 3, 170, 244, 237, 110, 45, 202, 4, 16, 230, 165, 43, 79, 143, 131, 34, 191,
        90, 90, 128, 16, 33, 84, 4, 84, 0, 91, 0, 86, 0, 231, 71, 247, 53, 203, 179, 123, 10, 225,
        214, 27, 97, 34, 110, 108, 196, 113, 229, 217, 137, 26, 207, 132, 143, 127, 28, 79, 207,
        247, 70, 158, 62, 199, 175, 0, 215, 224, 251, 105, 115, 131, 162, 134, 121, 249, 249, 149,
        38, 125, 84, 6, 43, 207, 80, 107, 109, 110, 37, 205, 50, 251, 142, 95, 18, 129, 149, 180,
        91, 111, 228, 233, 35, 176, 12, 164, 80, 211, 120, 94, 159, 133, 48, 124, 74, 8, 103, 224,
        242, 117, 188, 82, 235, 12, 70, 158, 93, 211, 56, 42, 181, 242, 206, 97, 91, 157, 180, 104,
        86, 231, 148, 36, 232, 167, 205, 173, 121, 39, 136, 125, 199, 43, 24, 30, 22, 11, 10, 133,
        231, 157, 168, 241, 76, 58, 130, 176, 164, 144, 90, 203, 242, 117, 250, 20, 143, 135, 226,
        114, 126, 148, 90, 9, 144, 150, 214, 37, 180, 227, 137, 233, 198, 25, 140, 125, 232, 91,
        73, 196, 180, 71, 247, 245, 2, 168, 176, 12, 199, 179, 231, 33, 142, 39, 16, 80, 106, 61,
        47, 29, 213, 202, 107, 2, 243, 143, 238, 107, 118, 250, 186, 61, 30, 154, 168, 44, 140,
        188, 30, 151, 229, 236, 120, 53, 73, 211, 113, 169, 149, 95, 176, 226, 168, 20, 107, 56,
        126, 91, 157, 180, 128, 180, 180, 238, 249, 61, 37, 175, 173, 238, 148, 252, 62, 250, 28,
        14, 71, 227, 88, 101, 33, 24, 72, 48, 181, 86, 132, 245, 184, 20, 24, 28, 32, 4, 0, 149,
        55, 111, 63, 15, 69, 172, 251, 28, 202, 109, 162, 142, 180, 239, 200, 26, 71, 243, 86, 210,
        182, 215, 145, 158, 136, 105, 147, 110, 38, 168, 105, 40, 98, 30, 174, 121, 141, 227, 78,
        175, 243, 110, 25, 104, 194, 71, 166, 86, 222, 53, 109, 91, 157, 52, 181, 102, 117, 160,
        38, 146, 34, 204, 66, 152, 165, 216, 90, 140, 41, 197, 214, 90, 118, 212, 145, 127, 142,
        74, 37, 86, 222, 57, 170, 149, 119, 4, 64, 32, 176, 162, 41, 17, 186, 14, 60, 138, 140,
        145, 158, 34, 169, 7, 15, 155, 125, 166, 14, 58, 73, 128, 208, 166, 67, 195, 72, 158, 179,
        152, 76, 34, 179, 91, 138, 130, 0, 146, 73, 41, 140, 192, 17, 36, 76, 187, 184, 129, 244,
        29, 37, 228, 17, 240, 78, 67, 123, 32, 164, 128, 14, 59, 110, 60, 60, 7, 88, 17, 4, 186,
        25, 103, 198, 124, 36, 144, 60, 91, 52, 36, 120, 102, 167, 233, 227, 22, 205, 133, 49, 225,
        30, 150, 12, 57, 223, 114, 41, 209, 193, 26, 25, 115, 159, 153, 87, 46, 145, 20, 227, 222,
        44, 182, 168, 205, 46, 115, 49, 22, 108, 112, 124, 249, 229, 221, 33, 139, 43, 106, 171,
        244, 45, 237, 249, 144, 176, 138, 50, 253, 118, 103, 104, 212, 19, 230, 3, 1, 54, 102, 40,
        198, 33, 106, 15, 43, 71, 134, 163,
    ];
    fn fixture() -> (
        tempfile::TempDir,
        data::binance_market_tape_artifact::BinanceMarketTapeTriplet,
        data::binance_market_tape_artifact::BinanceMarketTapeTrustAnchor,
    ) {
        fixture_with(RAW, COMPRESSED, "usdm_all", "all", None)
    }
    fn fixture_with(
        raw: &str,
        compressed: &[u8],
        dataset: &str,
        shard_id: &str,
        stream_types: Option<&[&str]>,
    ) -> (
        tempfile::TempDir,
        data::binance_market_tape_artifact::BinanceMarketTapeTriplet,
        data::binance_market_tape_artifact::BinanceMarketTapeTrustAnchor,
    ) {
        use data::binance_market_tape::{LobContinuitySummaryBuilder, MARKET_TAPE_SCHEMA};
        use data::binance_market_tape_artifact::{
            BinanceMarketTapeTriplet, BinanceMarketTapeTrustAnchor,
        };
        let root = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(root.path()).unwrap();
        let rows: Vec<serde_json::Value> = raw
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let mut summary = LobContinuitySummaryBuilder::new(["BTCUSDT".into()]).unwrap();
        let mut counts = std::collections::BTreeMap::<String, u64>::new();
        for row in &rows {
            summary.observe(row.as_object().unwrap()).unwrap();
            *counts
                .entry(row["type"].as_str().unwrap().into())
                .or_default() += 1;
        }
        let start = rows.first().unwrap()["received_at_ns"].as_u64().unwrap();
        let end = rows.last().unwrap()["received_at_ns"].as_u64().unwrap();
        let name = format!("part-{start}.jsonl.zst");
        let sha = format!("{:x}", Sha256::digest(compressed));
        let mut manifest = serde_json::json!({
            "schema":rows.first().unwrap()["schema"],"venue":"binance","market":rows.first().unwrap()["market"],"dataset":dataset,"shard_id":shard_id,"mode":"diff",
            "symbols":["BTCUSDT"],"security_token_symbols":[],"excluded_symbols":[],"snapshot_limit":1000,
            "replay_scope":"captured_aggregate_trades_plus_snapshot_seed_plus_sequence_checked_diffs","venue_depth_complete":false,
            "events":rows.len(),"event_types":counts,"has_replay_safe_checkpoint":true,
            "snapshot_ready_count":1,"bridged_count":1,"stream_coverage_verified_count":1,
            "snapshot_only_symbols":[],"all_symbols_bridged":true,"all_stream_coverage_verified":true,
            "start_received_at_ns":start,"end_received_at_ns":end,
            "date":"2023-11-14","hour":"22","file":name,"bytes":compressed.len(),"sha256":sha,
            "trade_representation":"aggregate_trade_only","price_surface_derivation":"latest aggregate trade price",
            "lob_continuity":summary.finish().unwrap(),
        });
        if let Some(stream_types) = stream_types {
            manifest["stream_types"] = serde_json::json!(stream_types);
        } else {
            assert_eq!(manifest["schema"], MARKET_TAPE_SCHEMA);
        }
        let mut manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        manifest_bytes.push(b'\n');
        let triplet = BinanceMarketTapeTriplet {
            data: dir.join(&name),
            manifest: dir.join(format!("{name}.manifest.json")),
            success: dir.join(format!("{name}._SUCCESS")),
        };
        std::fs::write(&triplet.data, compressed).unwrap();
        std::fs::write(&triplet.manifest, &manifest_bytes).unwrap();
        std::fs::write(&triplet.success, format!("{sha}\n")).unwrap();
        let anchor = BinanceMarketTapeTrustAnchor::from_lower_hex(
            &sha,
            &format!("{:x}", Sha256::digest(&manifest_bytes)),
        )
        .unwrap();
        (root, triplet, anchor)
    }
    fn planning_bindings() -> (PlanningViewV1, RepresentationGoalV1) {
        let content = |id: &str| CexResearchContentRefV1 {
            id: id.into(),
            content_sha256: format!("{:x}", Sha256::digest(id.as_bytes())),
        };
        let view = PlanningViewV1 {
            view: content("development"),
            family_id: "family".into(),
            visibility: PlanningVisibilityV1::Development,
            permission: content("permission"),
        };
        let goal = RepresentationGoalV1 {
            goal: content("goal"),
            family_id: "family".into(),
            venue: "binance".into(),
            market: "usdm".into(),
            symbol: "BTCUSDT".into(),
            target_name: "forward_mid_return".into(),
            labels: alpha_domain::EvaluationLabelSpecV1 {
                horizon_buckets: 30,
                observation_frequency_millis: 1000,
            },
            window_start_ns: 1700000060100000000,
            window_end_ns: 1700000120000000000,
            model: content("ridge"),
            scaling: content("train-scaler"),
            costs: content("costs"),
            partition: content("partition"),
            resource_limit: PlanningResourcesV1 {
                cpu_millis: 2000,
                memory_mib: 4096,
                wall_seconds: 900,
                trials: 2,
            },
        };
        (view, goal)
    }
    #[test]
    fn original_raw_verifier_produces_the_only_accepted_adapter_input() {
        use data::binance_market_tape_artifact::{
            seal_binance_market_tape_triplet,
            verify_binance_market_tape_series_with_required_lob_continuity,
        };
        let (_root, triplet, anchor) = fixture();
        let sealed = seal_binance_market_tape_triplet(&triplet, &anchor).unwrap();
        let series =
            verify_binance_market_tape_series_with_required_lob_continuity(vec![sealed]).unwrap();
        let (view, goal) = planning_bindings();
        let output = plan_from_verified_tape(&series, view.clone(), &goal).unwrap();
        assert_eq!(
            output.capability().series[0].continuity,
            BookContinuityV1::SequenceChecked
        );
        assert_eq!(output.capability().series[0].captured_seed_depth, 5);
        assert_eq!(output.capability().sources.len(), 2);
        assert!(output.proposal().arms.is_empty());
        assert!(output.proposal().materializations.is_empty());
        assert_eq!(
            output.to_readonly_json().unwrap()["permission_and_execution"],
            "requires_current_native_admission"
        );
        let mut wrong = goal.clone();
        wrong.symbol = "SOLUSDT".into();
        assert!(plan_from_verified_tape(&series, view, &wrong).is_err());
        let mut tampered = COMPRESSED.to_vec();
        tampered[0] ^= 1;
        std::fs::write(&triplet.data, tampered).unwrap();
        assert!(seal_binance_market_tape_triplet(&triplet, &anchor).is_err());
    }

    fn shifted_fixture(
        dataset: &str,
        shard: &str,
        lob_only: bool,
    ) -> (
        tempfile::TempDir,
        data::binance_market_tape_artifact::BinanceMarketTapeTriplet,
        data::binance_market_tape_artifact::BinanceMarketTapeTrustAnchor,
    ) {
        use data::binance_market_tape::MARKET_TAPE_SCHEMA_V2;
        let mut rows: Vec<serde_json::Value> = RAW
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .filter(|row: &serde_json::Value| !lob_only || row["type"] != "agg_trade")
            .collect();
        for row in &mut rows {
            row["session_id"] = serde_json::json!("session-2");
            row["received_at_ns"] =
                serde_json::json!(row["received_at_ns"].as_u64().unwrap() + 130_000_000_000);
            if let Some(start) = row.get("request_started_at_ns").and_then(|v| v.as_u64()) {
                row["request_started_at_ns"] = serde_json::json!(start + 130_000_000_000);
            }
            if let Some(frame) = row.get_mut("frame") {
                for clock in ["E", "T"] {
                    frame["data"][clock] =
                        serde_json::json!(frame["data"][clock].as_u64().unwrap() + 130_000);
                }
            }
            if lob_only {
                row["schema"] = serde_json::json!(MARKET_TAPE_SCHEMA_V2);
                if row["type"] == "session_start" {
                    row["websocket_shards"] = serde_json::json!(1);
                    row["websocket_streams"] = serde_json::json!(1);
                    row["stream_types"] = serde_json::json!(["depth@100ms"]);
                }
                if row["type"] == "stream_coverage" {
                    row["shards"] = serde_json::json!([["btcusdt@depth@100ms"]]);
                }
            }
        }
        let raw = rows
            .iter()
            .map(|row| serde_json::to_string(row).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        // A small standard Zstd frame with one uncompressed block. The original
        // sealer and verifier decode and check it; no test verification handle is minted.
        let compressed = raw_block_fixture(&raw);
        fixture_with(
            &raw,
            &compressed,
            dataset,
            shard,
            lob_only.then_some(&["depth@100ms"][..]),
        )
    }

    fn raw_block_fixture(raw: &str) -> Vec<u8> {
        let size = u32::try_from(raw.len()).unwrap();
        assert!(size < 128 * 1024);
        let mut compressed = vec![0x28, 0xb5, 0x2f, 0xfd, 0xa0];
        compressed.extend_from_slice(&size.to_le_bytes());
        compressed.extend_from_slice(&(size * 8 + 1).to_le_bytes()[..3]);
        compressed.extend_from_slice(raw.as_bytes());
        compressed
    }

    #[test]
    fn review_trade_direction_must_cover_every_verified_series() {
        use data::binance_market_tape_artifact::{
            seal_binance_market_tape_triplet,
            verify_binance_market_tape_series_with_required_lob_continuity,
        };
        let (_first_root, first, first_anchor) = fixture();
        let (_second_root, second, second_anchor) = shifted_fixture("usdm_all", "all", true);
        let series = verify_binance_market_tape_series_with_required_lob_continuity(vec![
            seal_binance_market_tape_triplet(&first, &first_anchor).unwrap(),
            seal_binance_market_tape_triplet(&second, &second_anchor).unwrap(),
        ])
        .unwrap();
        assert_eq!(series.len(), 2);
        assert!(!series[0].verified().aggregate_trades().is_empty());
        assert!(series[1].verified().aggregate_trades().is_empty());
        let (view, mut goal) = planning_bindings();
        goal.window_start_ns = 1700000191000000000;
        goal.window_end_ns = 1700000250000000000;
        if let Ok(output) = plan_from_verified_tape(&series, view, &goal) {
            assert!(!output.capability().aggregate_trade_direction);
            assert!(output.proposal().arms.is_empty());
        }
    }

    #[test]
    fn review_checkpoint_does_not_extend_legacy_renderer_coverage() {
        use data::binance_market_tape_artifact::{
            seal_binance_market_tape_triplet,
            verify_binance_market_tape_series_with_required_lob_continuity,
        };
        let (_root, triplet, anchor) = fixture();
        let series = verify_binance_market_tape_series_with_required_lob_continuity(vec![
            seal_binance_market_tape_triplet(&triplet, &anchor).unwrap(),
        ])
        .unwrap();
        let (view, mut goal) = planning_bindings();
        goal.window_end_ns = 1700000120100000000;
        let output = plan_from_verified_tape(&series, view, &goal).unwrap();
        assert_eq!(
            output.capability().series[0].end_available_ns,
            1700000120000000000
        );
        assert!(output.proposal().arms.is_empty());
    }

    #[test]
    fn review_separate_verifier_calls_cannot_mix_dataset_or_shard_scope() {
        use data::binance_market_tape_artifact::{
            seal_binance_market_tape_triplet,
            verify_binance_market_tape_series_with_required_lob_continuity,
        };
        for (dataset, shard) in [("other_dataset", "all"), ("usdm_all", "other_shard")] {
            let (_first_root, first, first_anchor) = fixture();
            let (_second_root, second, second_anchor) = shifted_fixture(dataset, shard, false);
            // The original verifier rejects exactly this collection scope.
            assert!(
                verify_binance_market_tape_series_with_required_lob_continuity(vec![
                    seal_binance_market_tape_triplet(&first, &first_anchor).unwrap(),
                    seal_binance_market_tape_triplet(&second, &second_anchor).unwrap(),
                ])
                .is_err()
            );
            let mut series = verify_binance_market_tape_series_with_required_lob_continuity(vec![
                seal_binance_market_tape_triplet(&first, &first_anchor).unwrap(),
            ])
            .unwrap();
            series.extend(
                verify_binance_market_tape_series_with_required_lob_continuity(vec![
                    seal_binance_market_tape_triplet(&second, &second_anchor).unwrap(),
                ])
                .unwrap(),
            );
            let (view, goal) = planning_bindings();
            assert!(plan_from_verified_tape(&series, view, &goal).is_err());
        }
    }

    #[test]
    fn review_tape_only_inventory_reports_missing_instrument_rules() {
        use data::binance_market_tape_artifact::{
            seal_binance_market_tape_triplet,
            verify_binance_market_tape_series_with_required_lob_continuity,
        };
        let (_root, triplet, anchor) = fixture();
        let series = verify_binance_market_tape_series_with_required_lob_continuity(vec![
            seal_binance_market_tape_triplet(&triplet, &anchor).unwrap(),
        ])
        .unwrap();
        let (view, mut goal) = planning_bindings();
        goal.resource_limit.trials = 1_000;
        let output = plan_from_verified_tape(&series, view, &goal).unwrap();
        assert!(output.proposal().arms.is_empty());
        assert!(output
            .proposal()
            .limitations
            .iter()
            .any(|reason| reason.contains("instrument-rule")));
    }

    fn rule_fixture(
        received: u64,
        symbol: &str,
        tick_size: &str,
    ) -> (tempfile::TempDir, VerifiedReferenceArtifact) {
        use data::binance_usdm_reference::{
            ActivePerpetualContract, CompleteReferenceBatch, MarkIndexFundingObservation,
            OpenInterestObservation, EXCHANGE_INFO_ENDPOINT, OPEN_INTEREST_ENDPOINT,
            PREMIUM_INDEX_ENDPOINT, REFERENCE_SCHEMA, SERVER_TIME_ENDPOINT,
        };
        use hft_collector::binance_usdm_reference_artifact::{
            publish_reference_batch, verify_bound_reference_artifact_read_only_current_batch,
            ReferenceArtifactConfig,
        };
        use hft_collector::binance_usdm_reference_collector::OFFICIAL_USDM_SOURCE_ORIGIN;
        let source_ms = (received - 10_000_000) / 1_000_000;
        let batch = CompleteReferenceBatch::new(
            vec![ActivePerpetualContract {
                schema: REFERENCE_SCHEMA.into(),
                symbol: symbol.into(),
                pair: symbol.into(),
                base_asset: symbol.strip_suffix("USDT").unwrap().into(),
                quote_asset: "USDT".into(),
                margin_asset: "USDT".into(),
                tick_size: tick_size.parse().unwrap(),
                step_size: "0.001".parse().unwrap(),
                min_notional: "5".parse().unwrap(),
                contract_type: "PERPETUAL".into(),
                status: "TRADING".into(),
                onboard_date_ms: 1,
                delivery_date_ms: 4_133_404_800_000,
                source_time_ms: source_ms,
                source_clock_received_at_ns: received - 1,
                received_at_ns: received,
                source_endpoint: EXCHANGE_INFO_ENDPOINT.into(),
                source_clock_endpoint: SERVER_TIME_ENDPOINT.into(),
            }],
            vec![MarkIndexFundingObservation {
                schema: REFERENCE_SCHEMA.into(),
                symbol: symbol.into(),
                mark_price: "101".parse().unwrap(),
                index_price: "100".parse().unwrap(),
                basis: "1".parse().unwrap(),
                basis_rate: "0.01".parse().unwrap(),
                last_funding_rate: "0.0001".parse().unwrap(),
                interest_rate: "0.0001".parse().unwrap(),
                next_funding_time_ms: source_ms + 28_800_000,
                source_time_ms: source_ms,
                received_at_ns: received + 1,
                source_endpoint: PREMIUM_INDEX_ENDPOINT.into(),
            }],
            vec![OpenInterestObservation {
                schema: REFERENCE_SCHEMA.into(),
                symbol: symbol.into(),
                open_interest: "12".parse().unwrap(),
                source_time_ms: source_ms,
                received_at_ns: received + 2,
                source_endpoint: OPEN_INTEREST_ENDPOINT.into(),
            }],
        )
        .unwrap();
        let root = tempfile::tempdir().unwrap();
        let published = publish_reference_batch(
            &ReferenceArtifactConfig {
                output_root: std::fs::canonicalize(root.path()).unwrap(),
                observed_at_ns: received + 3,
                max_staleness_ms: 1_000,
            },
            OFFICIAL_USDM_SOURCE_ORIGIN,
            &batch,
        )
        .unwrap();
        let verified = verify_bound_reference_artifact_read_only_current_batch(
            &published,
            &published.data_sha256,
            &published.manifest_sha256,
        )
        .unwrap();
        (root, verified)
    }

    #[test]
    fn source_bound_rules_propose_materials_without_claiming_scientific_trial_resources() {
        use data::binance_market_tape_artifact::{
            seal_binance_market_tape_triplet,
            verify_binance_market_tape_series_with_required_lob_continuity,
        };
        let (_root, triplet, anchor) = fixture();
        let series = verify_binance_market_tape_series_with_required_lob_continuity(vec![
            seal_binance_market_tape_triplet(&triplet, &anchor).unwrap(),
        ])
        .unwrap();
        let fixtures = [
            1700000000100000000,
            1700000090100000000,
            1700000150000000000,
        ]
        .map(|time| rule_fixture(time, "BTCUSDT", "0.1"));
        let (_roots, references): (Vec<_>, Vec<_>) = fixtures.into_iter().unzip();
        let (view, goal) = planning_bindings();
        let output = plan_from_verified_tape_with_rules(
            &series,
            VerifiedInstrumentRuleReferences::Usdm(&references),
            view,
            &goal,
        )
        .unwrap();
        assert_eq!(output.proposal().materializations.len(), 2);
        assert!(output.proposal().arms.is_empty());
        assert!(output.proposal().requested_resources.is_none());
        assert_eq!(
            output.proposal().status,
            RepresentationPlanStatusV1::NoExecutableComparison
        );
        let coverage = output.capability().instrument_rules.as_ref().unwrap();
        assert_eq!(
            coverage.max_gap_ns,
            hft_research_manifest::CEX_DERIVATIVES_MAX_GAP_NS
        );
        assert_eq!(coverage.sources.len(), 6);
        assert_eq!(output.capability().sources.len(), 8);
        for reference in &references {
            assert!(coverage
                .sources
                .iter()
                .any(|source| source.content_sha256 == reference.data_sha256()));
            assert!(coverage
                .sources
                .iter()
                .any(|source| source.content_sha256 == reference.manifest_sha256()));
        }
    }

    #[test]
    fn rule_references_reject_wrong_instrument_change_gap_and_short_window() {
        use data::binance_market_tape_artifact::{
            seal_binance_market_tape_triplet,
            verify_binance_market_tape_series_with_required_lob_continuity,
        };
        let (_root, triplet, anchor) = fixture();
        let series = verify_binance_market_tape_series_with_required_lob_continuity(vec![
            seal_binance_market_tape_triplet(&triplet, &anchor).unwrap(),
        ])
        .unwrap();
        let (view, goal) = planning_bindings();
        let seed = 1700000000100000000;
        let middle = 1700000090100000000;
        let end = 1700000150000000000;
        for (symbol, times, changed) in [
            ("ETHUSDT", vec![seed, middle, end], false),
            ("BTCUSDT", vec![seed, middle, end], true),
            ("BTCUSDT", vec![seed, end], false),
            ("BTCUSDT", vec![seed + 1, middle, end], false),
            ("BTCUSDT", vec![seed, middle, end - 1], false),
        ] {
            let fixtures = times
                .into_iter()
                .enumerate()
                .map(|(index, time)| {
                    rule_fixture(
                        time,
                        symbol,
                        if changed && index == 1 { "0.2" } else { "0.1" },
                    )
                })
                .collect::<Vec<_>>();
            let (_roots, references): (Vec<_>, Vec<_>) = fixtures.into_iter().unzip();
            assert!(plan_from_verified_tape_with_rules(
                &series,
                VerifiedInstrumentRuleReferences::Usdm(&references),
                view.clone(),
                &goal
            )
            .is_err());
        }
        assert!(plan_from_verified_tape_with_rules(
            &series,
            VerifiedInstrumentRuleReferences::Usdm(&[]),
            view.clone(),
            &goal
        )
        .is_err());
        assert!(plan_from_verified_tape_with_rules(
            &series,
            VerifiedInstrumentRuleReferences::Spot(&[]),
            view,
            &goal
        )
        .is_err());
    }

    fn spot_rule_fixture(
        received: u64,
        tick_size: &str,
    ) -> (tempfile::TempDir, VerifiedSpotReferenceArtifact) {
        use data::binance_spot_reference::{
            SpotInstrumentRules, SpotNotionalFilter, SpotPriceFilter, SpotQuantityFilter,
            SpotReferenceBatch, EXCHANGE_INFO_ENDPOINT, OFFICIAL_SOURCE_ORIGIN, REFERENCE_SCHEMA,
            SERVER_TIME_ENDPOINT,
        };
        use hft_collector::binance_spot_reference_artifact::{
            publish_spot_reference, verify_bound_spot_reference_artifact,
            SpotReferenceArtifactConfig,
        };
        let batch = SpotReferenceBatch::new(vec![SpotInstrumentRules {
            schema: REFERENCE_SCHEMA.into(),
            venue: "binance".into(),
            market: "spot".into(),
            symbol: "BTCUSDT".into(),
            base_asset: "BTC".into(),
            quote_asset: "USDT".into(),
            status: "TRADING".into(),
            is_spot_trading_allowed: true,
            base_asset_precision: 8,
            quote_asset_precision: 8,
            price_filter: SpotPriceFilter {
                min_price: "0".parse().unwrap(),
                max_price: "0".parse().unwrap(),
                tick_size: tick_size.parse().unwrap(),
            },
            lot_size_filter: SpotQuantityFilter {
                min_quantity: "0.001".parse().unwrap(),
                max_quantity: "9000".parse().unwrap(),
                step_size: "0.001".parse().unwrap(),
            },
            market_lot_size_filter: None,
            notional_filter: SpotNotionalFilter {
                filter_type: "MIN_NOTIONAL".into(),
                min_notional: "5".parse().unwrap(),
                max_notional: None,
                apply_min_to_market: true,
                apply_max_to_market: None,
                avg_price_mins: 5,
            },
            source_time_ms: (received - 10_000_000) / 1_000_000,
            source_clock_received_at_ns: received - 1,
            received_at_ns: received,
            source_endpoint: EXCHANGE_INFO_ENDPOINT.into(),
            source_clock_endpoint: SERVER_TIME_ENDPOINT.into(),
        }])
        .unwrap();
        let root = tempfile::tempdir().unwrap();
        let published = publish_spot_reference(
            &SpotReferenceArtifactConfig {
                output_root: std::fs::canonicalize(root.path()).unwrap(),
                observed_at_ns: received + 1,
                max_staleness_ms: 1_000,
            },
            OFFICIAL_SOURCE_ORIGIN,
            received - 1,
            received,
            &batch,
        )
        .unwrap();
        let verified = verify_bound_spot_reference_artifact(
            &published,
            &published.data_sha256,
            &published.manifest_sha256,
        )
        .unwrap();
        (root, verified)
    }

    #[test]
    fn spot_rule_materialization_preserves_fill_bounds_and_source_window() {
        use data::binance_market_tape_artifact::{
            seal_binance_market_tape_triplet,
            verify_binance_market_tape_series_with_required_lob_continuity,
        };
        let raw = RAW.replace("\"market\":\"usdm\"", "\"market\":\"spot\"");
        let (_root, triplet, anchor) =
            fixture_with(&raw, &raw_block_fixture(&raw), "spot_all", "all", None);
        let series = verify_binance_market_tape_series_with_required_lob_continuity(vec![
            seal_binance_market_tape_triplet(&triplet, &anchor).unwrap(),
        ])
        .unwrap();
        let (view, mut goal) = planning_bindings();
        goal.market = "spot".into();
        for tick in ["0.1", "0"] {
            let fixtures = [
                1700000000100000000,
                1700000090100000000,
                1700000150000000000,
            ]
            .map(|time| spot_rule_fixture(time, tick));
            let (_roots, references): (Vec<_>, Vec<_>) = fixtures.into_iter().unzip();
            let result = plan_from_verified_tape_with_rules(
                &series,
                VerifiedInstrumentRuleReferences::Spot(&references),
                view.clone(),
                &goal,
            );
            if tick == "0.1" {
                let output = result.unwrap();
                assert_eq!(output.proposal().materializations.len(), 2);
                assert_eq!(
                    output
                        .capability()
                        .instrument_rules
                        .as_ref()
                        .unwrap()
                        .market,
                    "spot"
                );
                assert!(output.proposal().arms.is_empty());
            } else {
                assert!(result.is_err());
            }
        }
    }
    #[test]
    fn exchange_future_and_receive_after_availability_are_rejected() {
        let (mut available, mut receive, mut event) = (0, 0, None);
        assert!(observe_clock(
            2_000_000,
            2_000_001,
            None,
            &mut available,
            &mut receive,
            &mut event
        )
        .is_err());
        assert!(observe_clock(
            2_000_000,
            2_000_000,
            Some(3),
            &mut available,
            &mut receive,
            &mut event
        )
        .is_err());
        observe_clock(
            2_000_000,
            1_000_000,
            Some(1),
            &mut available,
            &mut receive,
            &mut event,
        )
        .unwrap();
        assert_eq!(
            (available, receive, event),
            (2_000_000, 1_000_000, Some(1_000_000))
        );
    }
}
