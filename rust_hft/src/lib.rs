//! Workspace feature orchestrator crate.
//!
//! 此 crate 不包含執行邏輯，僅用於集中宣告工作區共用的 feature gate，
//! 讓 `cargo build --features` 能在 workspace root 正確地套用。

#[cfg(test)]
mod tests {
    #[test]
    fn workspace_crate_builds() {
        assert_eq!(2 + 2, 4);
    }
}
