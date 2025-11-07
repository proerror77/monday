//! OMS 例子：示範如何使用 `oms-core` 管理訂單狀態機
//!
//! 執行：`cargo run -p hft-examples --bin 03_execution`

use hft_core::{now_micros, OrderId, Price, Quantity, Side, Symbol, VenueId};
use oms_core::{OmsCore, OrderStatus};
use ports::ExecutionEvent;
use tracing::info;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_target(false)
        .init();

    let order_id = OrderId("DEMO-ORDER-1".to_string());
    let symbol = Symbol::new("BTCUSDT");
    let quantity = Quantity::from_f64(0.05).expect("可轉換數量");
    let venue = Some(VenueId::BINANCE);

    let mut oms = OmsCore::new();
    oms.register_order(
        order_id.clone(),
        Some("client-123".to_string()),
        symbol.clone(),
        Side::Buy,
        quantity,
        venue,
        Some("demo-strategy".to_string()),
    );

    info!("註冊訂單完成", order_id = %order_id.0);

    // 依序餵入幾個執行事件，觀察狀態演化
    for event in demo_events(order_id.clone()) {
        if let Some(update) = oms.on_execution_event(&event) {
            info!(
                "狀態更新: {:?} -> {:?}, cum_qty = {}, avg_price = {:?}",
                update.previous_status, update.status, update.cum_qty, update.avg_price
            );
        }
    }

    // 顯示最終狀態
    let record = oms.get(&order_id).expect("訂單應存在");
    println!(
        "最終狀態: {:?}, 成交總量: {}, 均價: {:?}",
        record.status, record.cum_qty, record.avg_price
    );

    assert_eq!(record.status, OrderStatus::Filled);
}

fn demo_events(order_id: OrderId) -> Vec<ExecutionEvent> {
    let ts = now_micros();
    let price = Price::from_f64(25000.0).expect("價格可轉換");
    let fill_qty = Quantity::from_f64(0.025).expect("數量可轉換");

    vec![
        ExecutionEvent::OrderAck {
            order_id: order_id.clone(),
            timestamp: ts,
        },
        ExecutionEvent::Fill {
            order_id: order_id.clone(),
            price,
            quantity: fill_qty,
            timestamp: ts + 1_000,
            fill_id: "fill-1".to_string(),
        },
        ExecutionEvent::Fill {
            order_id,
            price,
            quantity: fill_qty,
            timestamp: ts + 2_000,
            fill_id: "fill-2".to_string(),
        },
    ]
}
