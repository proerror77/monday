use hft_collector::polymarket_holder_flow::{
    plan_scan_ranges, project_holder_flow, ChainLog, HolderFlowSnapshot, ScannedBlockRange,
    CTF_CONTRACT, TRANSFER_BATCH_TOPIC, TRANSFER_SINGLE_TOPIC,
};

fn word(value: u64) -> String {
    format!("{value:064x}")
}

fn address_topic(address: &str) -> String {
    format!("0x{:0>64}", address.trim_start_matches("0x"))
}

fn chain_log(block: u64, topic: &str, from: &str, to: &str, data: String) -> ChainLog {
    ChainLog {
        address: CTF_CONTRACT.to_owned(),
        block_number: block,
        block_hash: format!("0x{block:064x}"),
        transaction_hash: format!("0x{:064x}", block + 1),
        transaction_index: 3,
        log_index: 7,
        topics: vec![
            topic.to_owned(),
            address_topic("0x3333333333333333333333333333333333333333"),
            address_topic(from),
            address_topic(to),
        ],
        data,
        removed: false,
    }
}

fn project(block: u64, log: ChainLog, token_ids: &[&str]) -> HolderFlowSnapshot {
    project_holder_flow(
        block,
        block,
        &[ScannedBlockRange {
            from_block: block,
            to_block: block,
        }],
        vec![log],
        &token_ids
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>(),
    )
    .expect("complete holder-flow projection")
}

#[test]
fn single_transfer_projects_holder_inflow_and_outflow() {
    let from = "0x1111111111111111111111111111111111111111";
    let to = "0x2222222222222222222222222222222222222222";
    let log = chain_log(
        100,
        TRANSFER_SINGLE_TOPIC,
        from,
        to,
        format!("0x{}{}", word(42), word(2_500_000)),
    );
    let snapshot = project(100, log, &["42"]);

    assert_eq!(snapshot.holders[0].address, from);
    assert_eq!(snapshot.holders[0].net_raw, "-2500000");
    assert_eq!(snapshot.holders[1].address, to);
    assert_eq!(snapshot.holders[1].net_raw, "2500000");
}

#[test]
fn batch_transfer_projects_every_token_amount_pair() {
    let recipient = "0x2222222222222222222222222222222222222222";
    let log = chain_log(
        101,
        TRANSFER_BATCH_TOPIC,
        "0x0000000000000000000000000000000000000000",
        recipient,
        format!(
            "0x{}{}{}{}{}{}{}{}",
            word(64),
            word(160),
            word(2),
            word(42),
            word(43),
            word(2),
            word(1_000_000),
            word(3_000_000),
        ),
    );
    let snapshot = project(101, log, &["42", "43"]);

    assert_eq!(snapshot.transfers[0].token_id, "42");
    assert_eq!(snapshot.transfers[0].amount_raw, "1000000");
    assert_eq!(snapshot.transfers[1].token_id, "43");
    assert_eq!(snapshot.transfers[1].amount_raw, "3000000");
}

#[test]
fn scan_plan_covers_every_requested_block_once() {
    let ranges = plan_scan_ranges(100, 105, 2).expect("bounded contiguous scan plan");
    let bounds = ranges
        .iter()
        .map(|range| (range.from_block, range.to_block))
        .collect::<Vec<_>>();
    assert_eq!(bounds, [(100, 101), (102, 103), (104, 105)]);
    assert!(plan_scan_ranges(100, 105, 0).is_err());
}

#[test]
fn incomplete_scan_range_cannot_claim_a_complete_snapshot() {
    let error = project_holder_flow(
        100,
        102,
        &[ScannedBlockRange {
            from_block: 100,
            to_block: 100,
        }],
        Vec::new(),
        &["42".to_owned()],
    )
    .expect_err("block 101 is unscanned");

    assert!(error.to_string().contains("coverage is incomplete"));
}
