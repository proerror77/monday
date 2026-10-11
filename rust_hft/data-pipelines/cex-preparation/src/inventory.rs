use anyhow::bail;
use hft_research_artifacts::temporary_output_file;
use hft_research_manifest::binance::Market;
use std::io::Write;
/// Prepare immutable input selection for the existing materialization entrypoint.
pub(crate) fn freeze_research_inventory_request(
    args: &crate::cli::FreezeInventoryArgs,
) -> anyhow::Result<hft_cex_research_input::inventory::FrozenInventory> {
    let request = hft_cex_research_input::inventory::InventoryRequest {
        raw_root: args.raw_root.clone(),
        reference_root: args.reference_root.clone(),
        start_received_at_ns: args.start_received_at_ns,
        end_received_at_ns: args.end_received_at_ns,
        market: args.market.parse::<Market>().map_err(anyhow::Error::msg)?,
        symbol: args.symbol.clone(),
        source_revision: crate::cli::BUILD_SOURCE_REVISION.to_string(),
        image_ref: args.image_ref.clone(),
        mission_id: args.mission_id.clone(),
        output_prefix: args.output_prefix.clone(),
        bucket_ms: args.bucket_ms,
        label_horizon_buckets: args.label_horizon_buckets,
        top_depth: args.top_depth,
        max_scan_entries: args.max_scan_entries,
        max_inputs: args.max_inputs,
        max_input_bytes: args.max_input_bytes,
    };
    hft_collector::research_inventory::freeze_inventory(&request)
}

pub fn verify_materialization_manifest(
    args: crate::cli::VerifyMaterializationManifestArgs,
) -> anyhow::Result<()> {
    use hft_collector::research_inventory::{
        verify_materialization_manifest_metadata, MaterializationManifestKind,
    };
    let kind = match args.kind {
        crate::cli::MaterializationManifestKindArg::Raw => MaterializationManifestKind::Raw,
        crate::cli::MaterializationManifestKindArg::Reference => {
            MaterializationManifestKind::Reference
        }
    };
    let window = match (args.start_received_at_ns, args.end_received_at_ns) {
        (Some(start), Some(end)) => Some((start, end)),
        (None, None) => None,
        _ => bail!("materialization manifest requires both window bounds together"),
    };
    verify_materialization_manifest_metadata(
        &args.manifest,
        &args.manifest_sha256,
        kind,
        args.market.parse::<Market>().map_err(anyhow::Error::msg)?,
        window,
    )?;
    crate::cli::print_json(&serde_json::json!({
        "status": "verified", "kind": kind, "manifest_sha256": args.manifest_sha256,
        "requires_pit_admission": true,
    }))
}

/// Prepare immutable input selection for the existing materialization entrypoint.
pub fn freeze_research_inventory(args: crate::cli::FreezeInventoryArgs) -> anyhow::Result<()> {
    let inventory = freeze_research_inventory_request(&args)?;
    let mut output = temporary_output_file(&args.output, ".frozen-inventory-")?;
    output
        .as_file_mut()
        .write_all(inventory.inventory_env.as_bytes())?;
    output.as_file().sync_all()?;
    output
        .persist_noclobber(&args.output)
        .map_err(|error| error.error)?;
    if hft_research_artifacts::sha256_file(&args.output)? != inventory.inventory_sha256 {
        bail!("frozen inventory output readback differs");
    }
    crate::cli::print_json(&serde_json::json!({"output":args.output,"inventory":inventory}))
}
