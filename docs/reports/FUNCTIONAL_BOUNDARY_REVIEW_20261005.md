# 功能边界与完成度复核 — 2026-10-05

本轮按功能及其下一层职责复核。目录、Cargo workspace、crate、镜像独立只证明部分构建边界；完成度还需要实际调用者、数据合同、权限边界、失败行为及对应证据。

## 复核源与证据边界

- 已合并基线：`05d164575e8eea24bc598aad5d192639dde91b2b`。包含 collector/depth 实际拆分和自动签名发布代码。
- Runtime 投影复核源：PR #1315 `c7512748e0af75f9a3a980158f69c9ae3cc1034f`。这是待合并 Code，不是已部署 Runtime。
- Prediction operator 复核源：PR #1309 `2f960cbd0875bfbdb6d6422744f2449045c54123`。
- CEX worker/portable 参数复核源：PR #1321 `f2536cadd033ee744b21ff8a59a78a225cf5f634`。
- 原生准入复核源：PR #1320 `c3aac9b9218c15cf72e4692d12e011bdef18be02`。
- 发展区 collection/平台接入为独立开发中的 Code。原生生产者、worker 完成回执、终态结账尚未构成真实运行验收。

本报告记录上述源的代码事实。它不宣称必需 CI、合并、产物发布、部署、真实科学终态或交易启用已经全部完成。真实云端验收继续归 #1304；本轮不创建云资源。

## 功能矩阵

| 功能 | 下一层职责与实际所有者 | 实际消费者/边界 | 完成度与缺口 |
| --- | --- | --- | --- |
| 行情接入 | venue wire、快照/增量、序列恢复属于 data adapters；Binance `BookSync` 属于 `hft-binance-depth` | collector、binance-md 使用 depth leaf；不再编译 order-loop engine | 实际功能分拆已合并；这不替代真实 capture 完整性验收 |
| 观察到的报价 | bid/ask、深度、token/instrument、事件及到达时钟 | 共享 `hft-ports::MarketEvent`；Prediction 的事件研究合同保留官方结算和 UP/DOWN token 身份 | CEX 参考价、Chainlink 开盘参考和 CLOB 可执行价格必须分别验证；不可互作缺失数据的替代 |
| 目标报价/策略 | 信号、定价、库存目标、订单意图属于策略；连接和签名属于 execution adapter | 共享 Formula 已输出 Polymarket `OrderIntent` 并向下舍入份额；Prediction 专有算法仍用 `StrategyLogic`/`TradingIntent` | 通用 Formula 支持 Prediction instrument；专有概率/事件策略的完整 governed handoff 仍未证明。不能由 venue adapter 存在推断策略迁移完成 |
| 风控 | hard caps、portfolio budgets、账户状态、每单 quantity/notional/slippage | `SystemBuilder` 给同一 engine 注入 risk manager；Envelope 经队列和执行端复核 | 共享主路径已存在；仍需逐实际策略验证真实意图经过该路径。Prediction 模拟策略自带风控不是生产风险准入 |
| OMS/Portfolio | 订单状态、部分成交、unknown outcome、余额/仓位、对账 | `oms-core`、`portfolio-core`、canonical engine | Prediction paper projection 使用共享 portfolio leaf，但其模拟 lifecycle 与 canonical engine 的消费路径不能混同 |
| 订单执行 | submit、cancel、replace、private WS、REST catch-up、fee、precision | `ExecutionClient` 与唯一 Polymarket adapter；`SystemBuilder` 按 venue/account 注册 | 真实 adapter 代码存在；Prediction 兼容平台的 production client 及 submit 会拒绝执行。没有真实交易启用验收 |
| 执行价格保护 | CEX 当前 canonical book；Prediction adapter 当前 venue quote | engine 的 `ExecutionPriceProtection` 明确分支；Polymarket 从当前 CLOB book 更新价格/fee/min-size 后检查生命周期及单量上限 | 不应把 Prediction 强行按 CEX arrival price 处理。该差异是合同内的专业化，不是两套执行权限 |
| Prediction 模拟 | settlement/markout、full-depth sweep、fill、latency、fee、capacity | `SimulatedExecutor` 与事件研究 runtime | 可保留研究模拟；旧任意 `CallbackExecutor` 没有实际调用者却宣称可直接接交易所，本次删除。泛型 Executor/live retry 的遗留模拟语义还需后续收束 |
| CEX 控制 | freeze/finalize、预算/批准、dispatch/stop/settle | 默认 alpha-harness operator；新 scientific worker 只接受 canonical campaign-execute | worker 分离 Code 已有；worker 仍复用 app 的 scientific library，控制模块的编译依赖需要进一步缩小 |
| CEX 搜索 | GP、search policy、trial/iteration cap、因果数据投影 | alpha-engine 与实际 scientific kernel | 实际内核复用；collection 仅交付授权发展区，不开放 selection/holdout |
| CEX train/fit | Burn、逐折 fit、固定 seed/参数、训练收据 | ML/fitting feature 与 scientific worker | 已有真实算法及输入适配 Code；新平台未给出真实训练终态。不能把整个发展区集合标为 Train 来通过通用 trainer |
| CEX replay/eval | 时间对齐、费用、执行 trace、独立指标、selection/holdout | Rust backtest/evaluator；实际 typed Replay | 有代码与局部验证；缺 selection 数据继续 insufficient_evidence。需要 native receipt/ZIP 与平台 Run/Attempt 的完整独立验收 |
| Prediction 研究 | settlement_probability、up_execution、down_execution | event-disjoint evaluator、token 特定 snapshot/labels | 三种任务的标签及指标分开；settlement calibration 不证明 token 执行模型有效。Code 不能替代真实研究/容量证据 |
| 治理到 runtime | 完整科学证据的验证、promotion、固定推理投影、签名与 intake | 待合并 runtime bundle seam；live 不消费研究 evaluator/domain | 正在独立合并；专有 Prediction 策略仍需显式 typed projection 与实际 runtime 消费者 |
| 平台准入/预算 | Build 签名、原生 grant、累计扣账、exclusive transfer、撤销 | 原生生产者 → 签名 witness → PG import → controller | 自动发布代码已合并。源预算 producer、scheduled revoke、多原因历史与平台终态结账仍在接入；三段 SHA 或 worker 自报不是授权 |
| 数据/会话/运维 | 持久 PG/CH、受控 gateway、native session 恢复、通知去重、cancel/retry/stop | 独立服务和 per-attempt 身份 | 部分 Code/局部真实协议验证已完成。默认 paused/零副本；持久部署、真实业务数据、实际通知送达和云端回滚尚未验收 |

## 已确认的问题与修正

### P2 — 无消费者的直接交易回调与既定权限边界相冲突

`ploy-strategy-bundles::CallbackExecutor` 导出任意异步提交函数。模块注释明确建议直接调用交易所；`cancel` 仍为未实现。全仓查找只有它的声明、导出与自有示例，没有调用者。

本次删除 callback 模块及导出，保留真实使用的研究模拟器。生产订单由 `SystemBuilder`、`hft-ports`、risk、OMS 和 canonical adapter 所有。该删除不创建新的订单路径，也不把模拟指标升级成真实执行证据。

### P1 — 专有 Prediction 策略到 canonical runtime 的完成度缺口

Prediction 的概率/事件策略消费 `MarketUpdate`、输出 `portfolio_core::prediction::TradingIntent`。通用 Formula 已能对 Polymarket 输出共享 `OrderIntent`；这两件事不同。复核没有找到专有 `StrategyLogic` 在 canonical runtime/strategy-framework/apps 中的消费者。

后续接口需要明确 episode/UP/DOWN token、模型/配置/evaluator 身份、可执行价格来源、意图生命周期和每单约束。只有固定策略投影及 shared intent consumer 通过独立测试后，才可认为该功能迁移完成。继续保持 LiveSmall 禁止；不能以桥接代码替代 separate runtime acceptance。

### P1 — 新平台科学闭环的缺口

发展区 collection、原生累计预算和平台 Attempt 分别有实现，但必须沿同一 finalized request、真实数据及 signed grant 收敛。尚缺实际 Run 的 train/replay/eval、原生完整 receipt/ZIP 独立验收、provider 终态及停止读回和原生结账。不得用准备成功或产物存在关闭该缺口。

### P2 — CEX worker 的下一层编译所有权

独立 binary/image 仍复用 alpha-harness scientific library。搜索、fit、replay、evaluation 是真实功能，控制器/签名/运维模块不是这些算法的必要职责。下一步从实际消费者裁定提取范围，避免仅改 Cargo 分组或同时复制两套算法。

## 对每项功能的复核循环

1. 指明输入、输出、所有者和有权产生该输入的角色。
2. 追踪实际调用者及下一层依赖，区分观察报价、目标报价和最终执行价格。
3. 检查公共合同的单位、身份、时间、边界及失败状态；仅共享同义合同。
4. 用反例验证职责边界：外部参考替代结算、DOWN 替代 UP、迟到行情、直接执行、未签授权、过期预算、取消后上传、未停止重试、缺回执伪造终态。
5. 删除无消费者的旧入口；有消费者的迁移先建立正确 seam，再移除旧实现。
6. 分别记录 Code、CI、Merge、Release、Runtime、独立 Readback。只有对应功能及其真实消费者通过所需验收才关闭缺口。

复核不按“拆出的 crate 数量”计完成度。它按职责、实际消费者及可推翻错误行为的证据计完成度。

## 本次最小修正的验证

- 全仓查找确认 CallbackExecutor/SubmitFn 仅有声明、导出和自身示例，没有调用者。
- ploy-strategy-bundles 默认 library owning check 通过（56.83 秒）。
- 实际消费者 ploy-strategy-runtime 默认 library check 通过（3.44 秒）。
- git diff --check 通过。两次编译均保留 7 条既有 dead-code 警告；没有宣称严格 Clippy 已通过。
- 这些检查只证明删除旧回调不会破坏现有默认消费者，不证明真实交易、所有策略迁移或完整平台研究终态。
