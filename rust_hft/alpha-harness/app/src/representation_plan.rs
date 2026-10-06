//! Signed normalized inventory. Raw family-search requires a producer-qualified projection.

/// Inspect only the original signed search projection. The result is not a raw
/// book qualification, an OFI regeneration plan, or a scientific execution grant.
pub fn inspect_signed_normalized_inventory(
    control_path: &std::path::Path,
    request_path: &std::path::Path,
) -> anyhow::Result<serde_json::Value> {
    crate::mission_dispatch::admission::planning_view::inspect_inventory(control_path, request_path)
}
