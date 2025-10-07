use std::time::Duration;

use runtime::SystemRuntime;
use tracing::{info, warn};

/// 在 dry-run 模式下下單並輸出帳戶資訊
pub async fn run_dry_run_if_enabled(system: &SystemRuntime, enabled: bool, symbol: &str) {
    if !enabled {
        return;
    }

    info!("執行 dry-run 下單驗證，symbol={}", symbol);
    match system.place_test_order(symbol).await {
        Ok(order_id) => {
            info!("dry-run 下單成功，order_id={}", order_id.0);
            tokio::time::sleep(Duration::from_millis(500)).await;
            let account_view = system.get_account_view().await;
            info!(
                cash = account_view.cash_balance,
                positions = account_view.positions.len(),
                unrealized = account_view.unrealized_pnl,
                realized = account_view.realized_pnl,
                "AccountView 更新"
            );
        }
        Err(e) => warn!("dry-run 下單失敗: {}", e),
    }
}
