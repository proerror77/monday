# Aster 交易所（Perpetual Pro + Spot）Agentic 集成 PRD  
**版本**：2025-09-27  
**文档目的**：为自主交易 Agent（Agentic）提供对 Aster 交易所的端到端能力映射与技术约束，支持快速落地下单、风控、行情与账户监听。本文档聚合公开 API 说明并固化成机器与人类均可直接消费的单一资料。

---

## 0. 项目目标与边界

### 0.1 目标（Outcomes）
- 在 **Aster Perpetual（Pro）** 上实现机构级的 **行情订阅、下单、撤单、订单查询、持仓与资金管理、风控与报错恢复** 全链路能力。  
- 在 **Aster Spot** 上提供 **行情与交易** 的可扩展支撑（按公开资料现状，优先交付 Perpetual Pro，Spot 路线图与接口位留白以便逐步补齐）。
- 充分利用 **WebSocket 市场与用户数据流** 降低请求权重，确保在 WAF/429/418 限流策略下仍具备高可用性。

### 0.2 范围（In-scope）
- **REST 基础能力**：时间同步、交易所规则拉取、行情拉取、订单与账户操作。
- **WS 能力**：深度、撮合相关 ticker、强平、用户数据流（listenKey）。
- **签名与鉴权**：`X-MBX-APIKEY` 头、`HMAC SHA256`、`timestamp/recvWindow`、签名串拼接。
- **限流与回退**：按权重与订单速率限制实现退避与自愈。
- **风控与幂等**：价格/数量过滤器校验、`newClientOrderId` 幂等、未知执行状态处理。

### 0.3 非目标（Out-of-scope）
- UI/前端实现、网关/代理运维、策略逻辑本身（只提供交易底座）。
- Aster 1001x（Simple）合约的链上交互接口（不在本次 REST/WS 集成范围）。
- Earn/USDF/ALP 等理财与生态接口。

---

## 1. 基础信息与兼容性

- **Perpetual Pro REST 基址**：`https://fapi.asterdex.com`  
- **返回格式**：JSON；时间戳单位毫秒；按时间 **升序** 返回。  
- **HTTP 语义**：`4XX` 为请求方错误；`429` 触发速率限制；继续打满会返回 `418`（自动封禁）；`5XX` 为服务端错误；`503` 表示 **超时未知** —— 不应直接视为失败。  
- **参数传输**：GET 用 query；POST/PUT/DELETE 支持 query 或 `application/x-www-form-urlencoded` body；二者并存时 **query 优先**。  
- **签名端点（SIGNED）**：新增 `signature` 参数（`HMAC SHA256(secretKey, totalParams)`）；鉴权头 `X-MBX-APIKEY`。  
- **WebSocket（Perp）**：`wss://fstream.asterdex.com`；用户流 `POST /fapi/v1/listenKey` 创建，**需要定期保活**（常见 60 分钟失效；以线上为准），`PUT` 续期；**建议按 23–24h 滚动重连**。  
- **限流与权重**：响应头 `X-MBX-USED-WEIGHT-<interval>`（IP 维度）；订单速率头 `X-MBX-ORDER-COUNT-<interval>`。API 权重以端点为单位统计。  

> 以上基于官方公开文档提炼；实现时必须以 `/fapi/v1/exchangeInfo` 的 `rateLimits` 与 `filters` 动态配置为准。

---

## 2. 能力矩阵（Capability Matrix）

### 2.1 市场数据（REST 示例路径）
- 健康检查：`GET /fapi/v1/ping`，`GET /fapi/v1/time`（时间同步）。
- 交易所信息与过滤器：`GET /fapi/v1/exchangeInfo`（合约规则、`filters`、`rateLimits`）。
- 深度：`GET /fapi/v1/depth`（L5/L10/L20/...）。
- 成交：`GET /fapi/v1/trades`，历史：`GET /fapi/v1/historicalTrades`，聚合：`GET /fapi/v1/aggTrades`。
- K 线：`GET /fapi/v1/klines`；指数价/标记价 K 线：`GET /fapi/v1/indexPriceKlines`，`GET /fapi/v1/markPriceKlines`。
- 标记价：`GET /fapi/v1/premiumIndex`（命名以实际为准）。
- 资金费率：`GET /fapi/v1/fundingRate`。
- 24h 统计、现价、盘口中间价：`GET /fapi/v1/ticker/24hr|price|bookTicker`。

### 2.2 市场数据（WebSocket 常用主题）
- `aggTrade`，`markPrice`（单合约/全市场），`kline_<interval>`，`miniTicker`（单/全），`ticker`（单/全），`bookTicker`（单/全），`depth31@100ms`，`diffDepth@100ms`，`forceOrder`（强平单）。
- **本地订单簿维护**：按官方指引先拉快照再应用 diff 流；丢包/乱序需重建。

### 2.3 交易与账户（REST）
- 下单：`POST /fapi/v1/order`（SIGNED）。批量：`POST /fapi/v1/batchOrders`。  
- 查/撤：`GET /fapi/v1/order`，`DELETE /fapi/v1/order`；`DELETE /fapi/v1/allOpenOrders`；`DELETE /fapi/v1/batchOrders`；倒计时自动撤：`POST /fapi/v1/countdownCancelAll`。  
- 查询：单开单 `GET /fapi/v1/openOrder`；全部开单 `GET /fapi/v1/openOrders`；历史 `GET /fapi/v1/allOrders`；成交 `GET /fapi/v1/userTrades`。  
- 账户资产与持仓：`GET /fapi/v2/balance`，`GET /fapi/v4/account`，`GET /fapi/v2/positionRisk`。  
- 杠杆/保证金：`POST /fapi/v1/leverage`，`POST /fapi/v1/marginType`，`POST /fapi/v1/positionMargin` / `GET /fapi/v1/positionMargin/history`。  
- 风险相关：名义档与杠杆档 `GET /fapi/v1/leverageBracket`；ADL 分位 `GET /fapi/v1/adlQuantile`；强平记录 `GET /fapi/v1/forceOrders`；佣金 `GET /fapi/v1/commissionRate`。

### 2.4 账户用户流（WebSocket）
- 启动/保活/关闭：`POST|PUT|DELETE /fapi/v1/listenKey`。  
- 事件：`listenKeyExpired`、`MARGIN_CALL`、余额/持仓更新、订单更新、账户配置（含杠杆）更新。**高负载下事件不保证顺序**，Agent 端需按 `E/e/u`（事件时间/更新序号）自排序。

---

## 3. 关键业务规则（实现须遵循）

- **签名**：`signature = HMAC_SHA256(secretKey, totalParams)`；`timestamp`（ms）必传，`recvWindow` 建议 5s-10s；Header 带 `X-MBX-APIKEY`。  
- **未知执行状态**：对 `HTTP 503` 或网络异常，状态未知，必须 **主动轮询** 订单状态进行判定，避免重复下单。  
- **过滤器校验**：下单前使用 `exchangeInfo.symbols[n].filters` 校验 `price`（`PRICE_FILTER`/`PERCENT_PRICE`）、`quantity`（`LOT_SIZE`）、`notional`、`stepSize/tickSize`。  
- **幂等性**：传 `newClientOrderId`；失败重试需延迟+指数退避并做状态对账。  
- **限流自适应**：读取 `X-MBX-USED-WEIGHT-*` 与 `X-MBX-ORDER-COUNT-*`，动态调整请求频率；`429` 触发 **退避**；多次触发会被 `418` 短封。  
- **WS 可靠性**：心跳/断线重连/位点续传（快照 + lastUpdateId）；**24h** 强制断开前做滚动重连。

---

## 4. 费用与分层（供风控与收益核算）

- **基础费率（Perp）**：Maker **0.01%**，Taker **0.035%**。若用 `$ASTER` 支付手续费可 **再减 5%**。  
- **VIP 分层（14 日滚动成交额）**：VIP1 `<$5M`：Taker 3.5 bps / Maker 1 bps；VIP2 `≥$5M`：3.4/0.8；VIP3 `≥$25M`：3.2/0.5；VIP4 `≥$100M`：3.0/0；VIP5 `≥$500M`：2.8/0；VIP6 `≥$1B`：2.5/0。  
- **做市返佣（MM）**：门槛 `≥$100M` 或 Maker 占比 `≥0.5%` 起步，返佣最高 **-0.5 bps**；系统每 30 分钟结算返佣。

> Spot 板块当前公开资料以产品指南为主，首发交易对为 **CDL/USD1** 与 **CDL/FORM**；更复杂的 Spot API 说明存在于 GitHub 仓库文件中，建议后续按链接补充验证。

---

## 5. Agent 端技术方案（落地要点）

### 5.1 连接与时间
- 启动阶段先 `GET /time` 对齐服务器时间；本地维护漂移与抖动窗口；签名时严格使用 ms 精度。

### 5.2 市场行情
- 优先 WS（合约级组合流）；按本地簿指引维护 `bids/asks`，跌落时用 REST `depth` 快照快速复位。

### 5.3 下单路径
1) 规则缓存：`exchangeInfo` → `filters` 提前固化到内存。  
2) 预校验：价格/数量/名义/最小步长。  
3) 下单：构造参数 + 签名；生成 `newClientOrderId`。  
4) 回执处理：对 `UNKNOWN` 场景进入状态轮询（`GET /order`）。  
5) 用户流驱动状态机：以 WS 订单更新为主、REST 为补偿；实现 **最终一致性**。

### 5.4 撤单与风控
- 条件撤单：`countdownCancelAll` 支持全撤保险丝。  
- 风险事件：订阅 `MARGIN_CALL` 与 `forceOrder`，结合策略限损与降杠杆动作。

### 5.5 限流治理
- 统一网关封装：每路由携带权重元数据，结合令牌桶 + 幂等队列，自动回退至 WS。

---

## 6. 可交付接口清单（机器可读大纲）

> **说明**：以下为与官方文档一致的 **端点目录**（按 Aster Pro 语义），字段/可选参数请以 `exchangeInfo` 与线上文档为准。

- **系统**：`GET /fapi/v1/ping`，`GET /fapi/v1/time`，`GET /fapi/v1/exchangeInfo`  
- **市场数据**：`GET /fapi/v1/depth|trades|historicalTrades|aggTrades|klines|indexPriceKlines|markPriceKlines|premiumIndex|fundingRate|ticker/24hr|ticker/price|ticker/bookTicker`  
- **账户/交易（SIGNED）**：  
  - 订单：`POST|GET|DELETE /fapi/v1/order`；批量：`POST|DELETE /fapi/v1/batchOrders`；全撤：`DELETE /fapi/v1/allOpenOrders`；倒计时撤单：`POST /fapi/v1/countdownCancelAll`；查询：`GET /fapi/v1/openOrder|openOrders|allOrders`  
  - 资产持仓：`GET /fapi/v2/balance`；账户信息：`GET /fapi/v4/account`；持仓风险：`GET /fapi/v2/positionRisk`  
  - 杠杆与保证金：`POST /fapi/v1/leverage|marginType|positionMargin`；历史：`GET /fapi/v1/positionMargin/history`  
  - 其他：`GET /fapi/v1/leverageBracket|adlQuantile|forceOrders|commissionRate`  
- **用户数据流（USER_STREAM）**：`POST|PUT|DELETE /fapi/v1/listenKey`；事件：`listenKeyExpired|MARGIN_CALL|ACCOUNT_UPDATE|ORDER_TRADE_UPDATE|CONFIG_UPDATE`  
- **WebSocket 市场流**：`aggTrade|markPrice|kline|miniTicker|ticker|bookTicker|depth|diffDepth|forceOrder`（单/全市场）。

---

## 7. 测试计划（必做）
- 环境连通：`ping/time`；签名样例下单（最小仓位、GTC、隐藏单可选）。
- 限流压测：验证 `429/418` 恢复逻辑；WS 24h 滚动重连。
- 容错用例：`503` 未知状态、网络抖动、WS 丢包/乱序。
- 对账：以 `userTrades` 与订单生命周期交叉验证。

---

## 8. 开放问题与依赖
- **Spot API**：GitHub 中提供 `aster-finance-spot-api*.md`，但需后续进一步抓取解析以补齐 REST/WS 细节（基础费用、首发交易对已确认）。
- **错误码明细**：文档列出示例结构（`{code, msg}`），完整 mapping 链接暂不可用，建议上线前通过实测/支持渠道补齐。

---

## 9. 参考来源（实现必须对齐线上行为）
- Aster Perpetual Pro **API 文档（REST/WS/签名/限流）**  
- Aster Pro **费率与说明**、**VIP & 做市**  
- Aster Spot **产品说明/首发交易对** 与 **如何创建 API**

> 注：本 PRD 将随官方文档更新而滚动修订。

