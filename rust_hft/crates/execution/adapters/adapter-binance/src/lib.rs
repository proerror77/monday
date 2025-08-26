//! Binance 執行 adapter（實作 `ports::ExecutionClient`）
//! - REST 下單 + 私有 WS 回報 → 統一 ExecutionReport（骨架）

use async_trait::async_trait;
use futures::stream;
use ports::{BoxStream, ExecutionClient, ExecutionEvent};
use hft_core::{HftResult, OrderId, Price, Quantity};

pub struct BinanceExecutionClient;
impl BinanceExecutionClient { pub fn new() -> Self { Self } }

#[async_trait]
impl ExecutionClient for BinanceExecutionClient {
    async fn place_order(&mut self, _intent: ports::OrderIntent) -> HftResult<OrderId> { Ok(OrderId("stub-order".into())) }
    async fn cancel_order(&mut self, _order_id: &OrderId) -> HftResult<()> { Ok(()) }
    async fn modify_order(&mut self, _order_id: &OrderId, _new_quantity: Option<Quantity>, _new_price: Option<Price>) -> HftResult<()> { Ok(()) }
    async fn execution_stream(&self) -> HftResult<BoxStream<ExecutionEvent>> { Ok(Box::pin(stream::empty())) }
    async fn connect(&mut self) -> HftResult<()> { Ok(()) }
    async fn disconnect(&mut self) -> HftResult<()> { Ok(()) }
    async fn health(&self) -> ports::ConnectionHealth { ports::ConnectionHealth { connected: true, latency_ms: None, last_heartbeat: 0 } }
}
