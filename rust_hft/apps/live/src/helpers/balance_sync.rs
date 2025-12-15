//! Balance Sync Worker - 交易所餘額同步
//!
//! 定期從交易所同步帳戶餘額到 Portfolio

use std::sync::Arc;
use std::time::Duration;

use engine::Engine;
use ports::{AccountBalance, ExecutionClient};
use rust_decimal::Decimal;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{info, warn};

/// 餘額同步配置
#[derive(Debug, Clone)]
pub struct BalanceSyncConfig {
    /// 同步間隔（秒）
    pub sync_interval_secs: u64,
    /// 主要報價幣種（用於計算 cash_balance）
    pub quote_asset: String,
}

impl Default for BalanceSyncConfig {
    fn default() -> Self {
        Self {
            sync_interval_secs: 30, // 每 30 秒同步一次
            quote_asset: "USDT".to_string(),
        }
    }
}

/// 啟動餘額同步 Worker
pub fn spawn_balance_sync_worker<E: ExecutionClient + 'static>(
    engine_arc: Arc<Mutex<Engine>>,
    execution_client: Arc<Mutex<E>>,
    config: BalanceSyncConfig,
) -> JoinHandle<()> {
    info!(
        "啟動餘額同步: interval={}s, quote_asset={}",
        config.sync_interval_secs, config.quote_asset
    );

    tokio::spawn(async move {
        run_balance_sync_loop(engine_arc, execution_client, config).await;
    })
}

/// 餘額同步主循環
async fn run_balance_sync_loop<E: ExecutionClient + 'static>(
    engine_arc: Arc<Mutex<Engine>>,
    execution_client: Arc<Mutex<E>>,
    config: BalanceSyncConfig,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(config.sync_interval_secs));

    loop {
        interval.tick().await;

        // 檢查引擎是否仍在運行
        {
            let engine = engine_arc.lock().await;
            if !engine.get_statistics().is_running {
                info!("引擎已停止，餘額同步退出");
                break;
            }
        }

        // 從交易所獲取餘額
        let balances = {
            let client = execution_client.lock().await;
            match client.get_balance().await {
                Ok(b) => b,
                Err(e) => {
                    warn!("餘額同步失敗: {}", e);
                    continue;
                }
            }
        };

        if balances.is_empty() {
            continue;
        }

        // 計算主要報價幣種的餘額
        let quote_balance = balances
            .iter()
            .find(|b| b.asset == config.quote_asset)
            .map(|b| b.available)
            .unwrap_or(Decimal::ZERO);

        // 計算總 USD 價值
        let total_usd_value: Decimal = balances
            .iter()
            .filter_map(|b| b.usd_value)
            .sum();

        info!(
            "餘額同步完成: {} {} 可用, 總價值 {} USD, {} 種資產",
            quote_balance,
            config.quote_asset,
            total_usd_value,
            balances.len()
        );

        // 更新 Portfolio 的 cash_balance
        // 注意：這裡我們更新的是 Portfolio 的初始餘額，
        // 實際 PnL 計算仍由 Fill 事件驅動
        {
            let mut engine = engine_arc.lock().await;

            // 獲取當前狀態
            let current_state = engine.export_portfolio_state();

            // 只在餘額變化顯著時更新（避免頻繁更新）
            let current_balance = current_state.account_view.cash_balance;
            let diff = (quote_balance - current_balance).abs();

            // 如果差異超過 1 USD 或 1%，則更新
            let should_update = diff > Decimal::ONE
                || (current_balance > Decimal::ZERO
                    && diff / current_balance > Decimal::new(1, 2));

            if should_update {
                info!(
                    "更新 Portfolio 餘額: {} -> {} {}",
                    current_balance, quote_balance, config.quote_asset
                );

                // 創建更新後的狀態
                let mut updated_state = current_state;
                updated_state.account_view.cash_balance = quote_balance;

                // 導入更新後的狀態
                engine.import_portfolio_state(updated_state);
            }
        }
    }
}

/// 餘額同步 Handle
pub struct BalanceSyncHandle {
    _handle: JoinHandle<()>,
}

impl BalanceSyncHandle {
    pub fn new(handle: JoinHandle<()>) -> Self {
        Self { _handle: handle }
    }
}

/// 單次餘額同步（用於啟動時初始化）
pub async fn sync_balance_once<E: ExecutionClient>(
    engine: &mut Engine,
    execution_client: &E,
    quote_asset: &str,
) -> Result<Vec<AccountBalance>, String> {
    let balances = execution_client
        .get_balance()
        .await
        .map_err(|e| format!("餘額查詢失敗: {}", e))?;

    if balances.is_empty() {
        return Ok(balances);
    }

    // 計算主要報價幣種的餘額
    let quote_balance = balances
        .iter()
        .find(|b| b.asset == quote_asset)
        .map(|b| b.available)
        .unwrap_or(Decimal::ZERO);

    info!(
        "初始餘額同步: {} {} 可用, {} 種資產",
        quote_balance,
        quote_asset,
        balances.len()
    );

    // 更新 Portfolio
    let mut state = engine.export_portfolio_state();
    state.account_view.cash_balance = quote_balance;
    engine.import_portfolio_state(state);

    Ok(balances)
}
