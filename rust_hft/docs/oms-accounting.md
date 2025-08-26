# OMS 與會計（Portfolio）契約草案

- OMS（hft-oms-core）
  - 統一訂單狀態機：Ack / Partial / Fill / Cancel / Rejected / Expired / Replaced
  - 冪等：client_order_id / idempotency key 的語義
  - 路由：多交易所/多產品的路由策略（無網路，純邏輯）

- Portfolio（hft-portfolio-core）
  - 真相源：fills / fees / funding
  - 對外介面：只讀 `AccountView` 快照（ArcSwap / left-right）
  - 回放一致性：mock/live 共用 ExecutionReport 事件模型，錄帶重放結果一致

> 細節設計依照 PRD 與 ports 契約對齊，實作細節另行補充。
