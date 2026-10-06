//! Read-only adapter from externally verified market-tape handles to representation proposals.
use alpha_domain::{representation::*, CexResearchContentRefV1};
use anyhow::{bail, ensure, Context, Result};
use data::{
    binance_lob_replay::ReplaySequenceEvent,
    binance_market_tape_artifact::{ReplayedBinanceBookEvent, VerifiedBinanceMarketTapeSeries},
};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

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
    goal.validate().map_err(anyhow::Error::msg)?;
    ensure!(
        !series.is_empty() && series.len() <= MAX_CAPABILITY_SERIES,
        "bounded verified series required"
    );
    let mut sources = Vec::new();
    let mut seen_sources = BTreeSet::new();
    let mut summaries = Vec::new();
    let mut market = None;
    let mut latest_receive = 0;
    let mut latest_available = 0;
    let mut latest_event = None;
    let mut directional_trades = false;
    for series in series {
        let verified = series.verified();
        ensure!(!verified.segments().is_empty(), "empty verified source");
        for segment in verified.segments() {
            let current = segment.market.as_str();
            ensure!(
                market.get_or_insert(current) == &current,
                "mixed market sources"
            );
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
                ReplayedBinanceBookEvent::Checkpoint { .. } => {
                    if let Some(summary) = &mut current {
                        summary.end_available_ns = received;
                    }
                }
            }
            ensure!(
                summaries.len() < MAX_CAPABILITY_SERIES,
                "too many recovery series"
            );
        }
        if let Some(current) = current {
            summaries.push(current);
        }
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
            directional_trades = true;
        }
    }
    if latest_available == 0 || summaries.is_empty() {
        bail!("no observed book before the planned decision window");
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
        use data::binance_market_tape::{LobContinuitySummaryBuilder, MARKET_TAPE_SCHEMA};
        use data::binance_market_tape_artifact::{
            BinanceMarketTapeTriplet, BinanceMarketTapeTrustAnchor,
        };
        let root = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(root.path()).unwrap();
        let rows: Vec<serde_json::Value> = RAW
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
        let name = "part-1700000000000000000.jsonl.zst";
        let sha = format!("{:x}", Sha256::digest(COMPRESSED));
        let manifest = serde_json::json!({
            "schema":MARKET_TAPE_SCHEMA,"venue":"binance","market":"usdm","dataset":"usdm_all","shard_id":"all","mode":"diff",
            "symbols":["BTCUSDT"],"security_token_symbols":[],"excluded_symbols":[],"snapshot_limit":1000,
            "replay_scope":"captured_aggregate_trades_plus_snapshot_seed_plus_sequence_checked_diffs","venue_depth_complete":false,
            "events":rows.len(),"event_types":counts,"has_replay_safe_checkpoint":true,
            "snapshot_ready_count":1,"bridged_count":1,"stream_coverage_verified_count":1,
            "snapshot_only_symbols":[],"all_symbols_bridged":true,"all_stream_coverage_verified":true,
            "start_received_at_ns":1700000000000000000u64,"end_received_at_ns":1700000120100000000u64,
            "date":"2023-11-14","hour":"22","file":name,"bytes":COMPRESSED.len(),"sha256":sha,
            "trade_representation":"aggregate_trade_only","price_surface_derivation":"latest aggregate trade price",
            "lob_continuity":summary.finish().unwrap(),
        });
        let mut manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        manifest_bytes.push(b'\n');
        let triplet = BinanceMarketTapeTriplet {
            data: dir.join(name),
            manifest: dir.join(format!("{name}.manifest.json")),
            success: dir.join(format!("{name}._SUCCESS")),
        };
        std::fs::write(&triplet.data, COMPRESSED).unwrap();
        std::fs::write(&triplet.manifest, &manifest_bytes).unwrap();
        std::fs::write(&triplet.success, format!("{sha}\n")).unwrap();
        let anchor = BinanceMarketTapeTrustAnchor::from_lower_hex(
            &sha,
            &format!("{:x}", Sha256::digest(&manifest_bytes)),
        )
        .unwrap();
        (root, triplet, anchor)
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
            target_name: "mid_return".into(),
            labels: alpha_domain::EvaluationLabelSpecV1 {
                horizon_buckets: 30,
                observation_frequency_millis: 1000,
            },
            window_start_ns: 1700000060100000000,
            window_end_ns: 1700000120100000000,
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
        let output = plan_from_verified_tape(&series, view.clone(), &goal).unwrap();
        assert_eq!(
            output.capability().series[0].continuity,
            BookContinuityV1::SequenceChecked
        );
        assert_eq!(output.capability().series[0].captured_seed_depth, 5);
        assert_eq!(output.capability().sources.len(), 2);
        assert_eq!(output.proposal().arms.len(), 2);
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
