use anyhow::{bail, Context, Result};
use num_bigint::BigUint;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
pub const CTF_CONTRACT: &str = "0x4d97dcd97ec945f40cf65f87097ace5ea0476045";
pub const POLYGON_CHAIN_ID: u64 = 137;
pub const TRANSFER_SINGLE_TOPIC: &str =
    "0xc3d58168c5ae7397731d063d5bbf3d657854427343f4c083240f7aacaa2d0f62";
pub const TRANSFER_BATCH_TOPIC: &str =
    "0x4a39dc06d4c0dbc64b70af90fd698a233a518aa5d07e595d983b8c0526c8f7fb";
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainLog {
    pub address: String,
    pub block_number: u64,
    pub block_hash: String,
    pub transaction_hash: String,
    pub transaction_index: u64,
    pub log_index: u64,
    pub topics: Vec<String>,
    pub data: String,
    pub removed: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScannedBlockRange {
    pub from_block: u64,
    pub to_block: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CtfTransfer {
    pub block_number: u64,
    pub block_hash: String,
    pub transaction_hash: String,
    pub log_index: u64,
    pub batch_index: u32,
    pub from: String,
    pub to: String,
    pub token_id: String,
    pub amount_raw: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HolderFlow {
    pub token_id: String,
    pub address: String,
    pub inflow_raw: String,
    pub outflow_raw: String,
    pub net_raw: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HolderFlowSnapshot {
    pub schema: String,
    pub chain_id: u64,
    pub contract: String,
    pub from_block: u64,
    pub to_block: u64,
    pub observed_head: Option<u64>,
    pub confirmations: Option<u64>,
    pub complete: bool,
    pub scanned_ranges: Vec<ScannedBlockRange>,
    pub token_ids: Vec<String>,
    pub transfer_count: usize,
    pub transfers: Vec<CtfTransfer>,
    pub holders: Vec<HolderFlow>,
}
#[derive(Default)]
struct Totals {
    inflow: BigUint,
    outflow: BigUint,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RpcLog {
    address: String,
    block_number: String,
    block_hash: String,
    transaction_hash: String,
    transaction_index: String,
    log_index: String,
    topics: Vec<String>,
    data: String,
    removed: bool,
}
#[derive(Debug, Clone)]
pub struct HolderFlowCollectionConfig {
    pub rpc_url: String,
    pub from_block: u64,
    pub to_block: Option<u64>,
    pub confirmations: u64,
    pub batch_size: usize,
    pub token_ids: Vec<String>,
    pub output: PathBuf,
    pub http_timeout: Duration,
}
pub fn parse_rpc_log(value: Value) -> Result<ChainLog> {
    let log: RpcLog = serde_json::from_value(value).context("invalid eth_getLogs row")?;
    Ok(ChainLog {
        address: log.address,
        block_number: parse_rpc_quantity(&log.block_number, "blockNumber")?,
        block_hash: log.block_hash,
        transaction_hash: log.transaction_hash,
        transaction_index: parse_rpc_quantity(&log.transaction_index, "transactionIndex")?,
        log_index: parse_rpc_quantity(&log.log_index, "logIndex")?,
        topics: log.topics,
        data: log.data,
        removed: log.removed,
    })
}
pub async fn collect_holder_flow(
    config: &HolderFlowCollectionConfig,
) -> Result<HolderFlowSnapshot> {
    if config.rpc_url.trim().is_empty() {
        bail!("Polygon RPC URL is required");
    }
    if config.http_timeout.is_zero() {
        bail!("holder-flow HTTP timeout must be positive");
    }
    normalize_token_ids(&config.token_ids)?;

    let client = reqwest::Client::builder()
        .timeout(config.http_timeout)
        .build()
        .context("build Polygon RPC client")?;
    let chain_id = rpc_quantity_result(
        rpc_call(&client, &config.rpc_url, "eth_chainId", json!([]), 1).await?,
        "eth_chainId",
    )?;
    validate_source(chain_id, config.confirmations)?;
    let head = rpc_quantity_result(
        rpc_call(&client, &config.rpc_url, "eth_blockNumber", json!([]), 2).await?,
        "eth_blockNumber",
    )?;
    let confirmed_head = head
        .checked_sub(config.confirmations)
        .context("chain head is below the required confirmation depth")?;
    let to_block = config.to_block.unwrap_or(confirmed_head);
    if to_block > confirmed_head {
        bail!(
            "requested to_block={to_block} exceeds confirmed head={confirmed_head} at head={head}"
        );
    }

    let ranges = plan_scan_ranges(config.from_block, to_block, config.batch_size)?;
    let mut logs = Vec::new();
    for (index, range) in ranges.iter().enumerate() {
        let result = rpc_call(
            &client,
            &config.rpc_url,
            "eth_getLogs",
            json!([{
                "address": CTF_CONTRACT,
                "fromBlock": format!("0x{:x}", range.from_block),
                "toBlock": format!("0x{:x}", range.to_block),
                "topics": [[TRANSFER_SINGLE_TOPIC, TRANSFER_BATCH_TOPIC]],
            }]),
            u64::try_from(index).unwrap_or(u64::MAX).saturating_add(3),
        )
        .await?;
        let rows = result
            .as_array()
            .context("eth_getLogs result is not an array")?;
        for row in rows {
            logs.push(parse_rpc_log(row.clone())?);
        }
    }

    let mut snapshot = project_holder_flow(
        config.from_block,
        to_block,
        &ranges,
        logs,
        &config.token_ids,
    )?;
    snapshot.observed_head = Some(head);
    snapshot.confirmations = Some(config.confirmations);
    atomic_write_json(&config.output, &snapshot)?;
    Ok(snapshot)
}
pub fn plan_scan_ranges(
    from_block: u64,
    to_block: u64,
    batch_size: usize,
) -> Result<Vec<ScannedBlockRange>> {
    if from_block > to_block {
        bail!("CTF holder-flow block range is reversed");
    }
    let batch_size = u64::try_from(batch_size).context("scan batch size does not fit in u64")?;
    if batch_size == 0 {
        bail!("scan batch size must be positive");
    }
    let mut ranges = Vec::new();
    let mut start = from_block;
    loop {
        let end = start.saturating_add(batch_size - 1).min(to_block);
        ranges.push(ScannedBlockRange {
            from_block: start,
            to_block: end,
        });
        if end == to_block {
            return Ok(ranges);
        }
        start = end + 1;
    }
}
pub fn project_holder_flow(
    from_block: u64,
    to_block: u64,
    scanned_ranges: &[ScannedBlockRange],
    logs: Vec<ChainLog>,
    token_ids: &[String],
) -> Result<HolderFlowSnapshot> {
    validate_coverage(from_block, to_block, scanned_ranges)?;
    let token_ids = normalize_token_ids(token_ids)?;
    let mut unique_logs = BTreeMap::new();

    for log in logs {
        validate_log(&log, from_block, to_block)?;
        let identity = (
            log.block_hash.to_ascii_lowercase(),
            log.transaction_hash.to_ascii_lowercase(),
            log.log_index,
        );
        if let Some(existing) = unique_logs.insert(identity, log.clone()) {
            if existing != log {
                bail!("conflicting CTF logs share one chain identity");
            }
        }
    }

    let mut transfers = Vec::new();
    for log in unique_logs.into_values() {
        for transfer in decode_log(&log)? {
            if token_ids.contains(&transfer.token_id) {
                transfers.push(transfer);
            }
        }
    }
    transfers.sort_by_key(|row| (row.block_number, row.log_index, row.batch_index));

    let mut totals: BTreeMap<(String, String), Totals> = BTreeMap::new();
    for transfer in &transfers {
        let amount = transfer
            .amount_raw
            .parse::<BigUint>()
            .context("decoded CTF amount is not an unsigned integer")?;
        if !is_zero_address(&transfer.from) {
            totals
                .entry((transfer.token_id.clone(), transfer.from.clone()))
                .or_default()
                .outflow += &amount;
        }
        if !is_zero_address(&transfer.to) {
            totals
                .entry((transfer.token_id.clone(), transfer.to.clone()))
                .or_default()
                .inflow += &amount;
        }
    }

    let holders = totals
        .into_iter()
        .map(|((token_id, address), totals)| HolderFlow {
            token_id,
            address,
            inflow_raw: totals.inflow.to_string(),
            outflow_raw: totals.outflow.to_string(),
            net_raw: signed_difference(&totals.inflow, &totals.outflow),
        })
        .collect();
    let token_ids = token_ids.into_iter().collect();

    Ok(HolderFlowSnapshot {
        schema: "monday.polymarket_ctf_holder_flow.v1".to_owned(),
        chain_id: POLYGON_CHAIN_ID,
        contract: CTF_CONTRACT.to_owned(),
        from_block,
        to_block,
        observed_head: None,
        confirmations: None,
        complete: true,
        scanned_ranges: scanned_ranges.to_vec(),
        token_ids,
        transfer_count: transfers.len(),
        transfers,
        holders,
    })
}
fn validate_coverage(
    from_block: u64,
    to_block: u64,
    scanned_ranges: &[ScannedBlockRange],
) -> Result<()> {
    if from_block > to_block {
        bail!("CTF holder-flow block range is reversed");
    }
    let mut expected = from_block;
    for range in scanned_ranges {
        if range.from_block != expected || range.from_block > range.to_block {
            bail!("CTF holder-flow scan coverage has a gap or overlap at block {expected}");
        }
        expected = range
            .to_block
            .checked_add(1)
            .context("CTF holder-flow block range overflows")?;
    }
    if expected != to_block.saturating_add(1) {
        bail!("CTF holder-flow scan coverage is incomplete");
    }
    Ok(())
}
fn normalize_token_ids(token_ids: &[String]) -> Result<BTreeSet<String>> {
    if token_ids.is_empty() {
        bail!("at least one CTF token id is required");
    }
    token_ids
        .iter()
        .map(|value| {
            value
                .parse::<BigUint>()
                .map(|value| value.to_string())
                .with_context(|| format!("invalid decimal CTF token id: {value}"))
        })
        .collect()
}
fn validate_log(log: &ChainLog, from_block: u64, to_block: u64) -> Result<()> {
    if !log.address.eq_ignore_ascii_case(CTF_CONTRACT) {
        bail!("CTF holder-flow log has the wrong contract address");
    }
    if log.removed {
        bail!("removed CTF holder-flow log requires a finalized rescan");
    }
    if !(from_block..=to_block).contains(&log.block_number) {
        bail!("CTF holder-flow log falls outside the requested block range");
    }
    validate_fixed_hex(&log.block_hash, 32, "block hash")?;
    validate_fixed_hex(&log.transaction_hash, 32, "transaction hash")?;
    Ok(())
}
fn decode_log(log: &ChainLog) -> Result<Vec<CtfTransfer>> {
    if log.topics.len() != 4 {
        bail!("CTF transfer log must have four topics");
    }
    let topic = log.topics[0].to_ascii_lowercase();
    let data = decode_hex(&log.data, "CTF transfer data")?;
    decode_address_topic(&log.topics[1])?;
    let from = decode_address_topic(&log.topics[2])?;
    let to = decode_address_topic(&log.topics[3])?;
    let make_transfer = |batch_index, token_id: BigUint, amount: BigUint| CtfTransfer {
        block_number: log.block_number,
        block_hash: log.block_hash.to_ascii_lowercase(),
        transaction_hash: log.transaction_hash.to_ascii_lowercase(),
        log_index: log.log_index,
        batch_index,
        from: from.clone(),
        to: to.clone(),
        token_id: token_id.to_string(),
        amount_raw: amount.to_string(),
    };

    match topic.as_str() {
        TRANSFER_SINGLE_TOPIC => {
            if data.len() != 64 {
                bail!("TransferSingle data must contain exactly two ABI words");
            }
            Ok(vec![make_transfer(
                0,
                BigUint::from_bytes_be(&data[..32]),
                BigUint::from_bytes_be(&data[32..]),
            )])
        }
        TRANSFER_BATCH_TOPIC => {
            let (token_ids, amounts) = decode_transfer_batch(&data)?;
            token_ids
                .into_iter()
                .zip(amounts)
                .enumerate()
                .map(|(index, (token_id, amount))| {
                    let batch_index = u32::try_from(index)
                        .context("TransferBatch contains too many token entries")?;
                    Ok(make_transfer(batch_index, token_id, amount))
                })
                .collect()
        }
        _ => bail!("unsupported CTF transfer topic: {}", log.topics[0]),
    }
}
fn decode_transfer_batch(data: &[u8]) -> Result<(Vec<BigUint>, Vec<BigUint>)> {
    if data.len() < 64 || !data.len().is_multiple_of(32) {
        bail!("TransferBatch data must contain complete ABI words");
    }
    let token_offset = abi_usize(&data[..32])?;
    let amount_offset = abi_usize(&data[32..64])?;
    let (token_ids, token_end) = decode_abi_uint_array(data, token_offset)?;
    if token_offset != 64 || amount_offset != token_end {
        bail!("TransferBatch arrays must use canonical contiguous ABI offsets");
    }
    let (amounts, amount_end) = decode_abi_uint_array(data, amount_offset)?;
    if token_ids.len() != amounts.len() || amount_end != data.len() {
        bail!("TransferBatch arrays differ in length or leave trailing ABI data");
    }
    Ok((token_ids, amounts))
}
fn decode_abi_uint_array(data: &[u8], offset: usize) -> Result<(Vec<BigUint>, usize)> {
    if !offset.is_multiple_of(32) || offset.checked_add(32).is_none_or(|end| end > data.len()) {
        bail!("TransferBatch array offset is outside ABI data");
    }
    let len = abi_usize(&data[offset..offset + 32])?;
    let body_start = offset + 32;
    let body_len = len
        .checked_mul(32)
        .context("TransferBatch array length overflows")?;
    let body_end = body_start
        .checked_add(body_len)
        .context("TransferBatch array end overflows")?;
    if body_end > data.len() {
        bail!("TransferBatch array is truncated");
    }
    Ok((
        data[body_start..body_end]
            .chunks_exact(32)
            .map(BigUint::from_bytes_be)
            .collect(),
        body_end,
    ))
}
fn abi_usize(word: &[u8]) -> Result<usize> {
    if word.len() != 32 || word[..24].iter().any(|byte| *byte != 0) {
        bail!("ABI offset or length does not fit in u64");
    }
    let value = u64::from_be_bytes(word[24..].try_into().expect("eight-byte ABI tail"));
    usize::try_from(value).context("ABI offset or length does not fit in usize")
}
fn decode_address_topic(value: &str) -> Result<String> {
    let bytes = decode_hex(value, "address topic")?;
    if bytes.len() != 32 || bytes[..12].iter().any(|byte| *byte != 0) {
        bail!("address topic must be a left-padded 20-byte address");
    }
    Ok(format!("0x{}", hex::encode(&bytes[12..])))
}
fn validate_fixed_hex(value: &str, bytes: usize, name: &str) -> Result<()> {
    if decode_hex(value, name)?.len() != bytes {
        bail!("{name} must contain exactly {bytes} bytes");
    }
    Ok(())
}
fn decode_hex(value: &str, name: &str) -> Result<Vec<u8>> {
    let raw = value
        .strip_prefix("0x")
        .ok_or_else(|| anyhow::anyhow!("{name} must be 0x-prefixed"))?;
    hex::decode(raw).with_context(|| format!("{name} is not valid hex"))
}
fn parse_rpc_quantity(value: &str, name: &str) -> Result<u64> {
    let raw = value
        .strip_prefix("0x")
        .ok_or_else(|| anyhow::anyhow!("{name} must be a 0x-prefixed RPC quantity"))?;
    if raw.is_empty() {
        bail!("{name} RPC quantity is empty");
    }
    u64::from_str_radix(raw, 16).with_context(|| format!("{name} RPC quantity exceeds u64"))
}
fn rpc_quantity_result(value: Value, name: &str) -> Result<u64> {
    value
        .as_str()
        .with_context(|| format!("{name} result is not a string"))
        .and_then(|value| parse_rpc_quantity(value, name))
}
fn validate_source(chain_id: u64, confirmations: u64) -> Result<()> {
    if chain_id != POLYGON_CHAIN_ID {
        bail!("Polygon chain id must be {POLYGON_CHAIN_ID}, got {chain_id}");
    }
    if confirmations == 0 {
        bail!("holder-flow confirmation depth must be positive");
    }
    Ok(())
}
fn signed_difference(inflow: &BigUint, outflow: &BigUint) -> String {
    if inflow >= outflow {
        (inflow - outflow).to_string()
    } else {
        format!("-{}", outflow - inflow)
    }
}
fn is_zero_address(address: &str) -> bool {
    address == "0x0000000000000000000000000000000000000000"
}

async fn rpc_call(
    client: &reqwest::Client,
    rpc_url: &str,
    method: &str,
    params: Value,
    id: u64,
) -> Result<Value> {
    let response = client
        .post(rpc_url)
        .json(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
        .send()
        .await
        .with_context(|| format!("{method} request failed"))?
        .error_for_status()
        .with_context(|| format!("{method} returned an HTTP error"))?;
    let body: Value = response
        .json()
        .await
        .with_context(|| format!("{method} returned invalid JSON"))?;
    if let Some(error) = body.get("error") {
        bail!("{method} RPC error: {error}");
    }
    body.get("result")
        .cloned()
        .with_context(|| format!("{method} response is missing result"))
}
fn atomic_write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .with_context(|| format!("create holder-flow output directory {}", parent.display()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("holder-flow output path requires a UTF-8 file name")?;
    let temporary = parent.join(format!(".{file_name}.{}.tmp", std::process::id()));
    let bytes = serde_json::to_vec_pretty(value).context("serialize holder-flow snapshot")?;
    fs::write(&temporary, bytes)
        .with_context(|| format!("write holder-flow temporary file {}", temporary.display()))?;
    fs::File::open(&temporary)
        .and_then(|file| file.sync_all())
        .with_context(|| format!("sync holder-flow temporary file {}", temporary.display()))?;
    fs::hard_link(&temporary, path)
        .with_context(|| format!("publish create-new holder-flow snapshot {}", path.display()))?;
    fs::remove_file(&temporary)
        .with_context(|| format!("remove holder-flow temporary file {}", temporary.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn trust_boundaries_fail_closed() {
        assert!(validate_source(1, 64).is_err());
        assert!(validate_source(POLYGON_CHAIN_ID, 0).is_err());
        let missing_removed = json!({
            "address": CTF_CONTRACT,
            "blockNumber": "0x1",
            "blockHash": format!("0x{}", "00".repeat(32)),
            "transactionHash": format!("0x{}", "11".repeat(32)),
            "transactionIndex": "0x0",
            "logIndex": "0x0",
            "topics": [],
            "data": "0x"
        });
        assert!(parse_rpc_log(missing_removed).is_err());
        let noncanonical_batch = vec![0; 96];
        assert!(decode_transfer_batch(&noncanonical_batch).is_err());
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().join("snapshot.json");
        atomic_write_json(&output, &json!({"version": 1})).unwrap();
        assert!(atomic_write_json(&output, &json!({"version": 2})).is_err());
    }
}
