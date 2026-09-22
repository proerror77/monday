```text
From: Codex
To: Cursor
Routed by: Monk
Goal: 建立 research-only 秒级订单流数据审计、标签/特征契约和实验骨架；先证明数据与评估有效，再判断 5s/10s/30s/60s/300s 预测是否提供扣成本后的增量价值。
Evidence paths: https://github.com/proerror77/monday ; local source HEAD 577c9ddede29935f2505ec6115e29f9206b481d9 ; docs/agents/cursor-codex-handoff.md ; docs/plans/2026-09-17-sec-orderflow-ml-research-packet.md ; /Users/proerror/tools/marscoin-footprint/{collect-flow.js,collect-l2.js,atoms.py,signals.py,orderflow-strategy-v2.md,strategy-v2-triple-gate.md,directionality-findings.md,data/flow,data/l2} ; 数据截止与摘要见 §1.1、§7.5；issue/PR/Campaign/Job: none。
Constraints: 本次仅研究包，不提交订单、不改 live risk、不合并实现、不启动训练或生产作业；Cursor 后续实现须由 Monk 路由。Rust-only，复用 Monday 研究边界；旧规则 S1–S5 不获 Live 资格，triple-gate 仍待 OOS；不得创建 Hermes/Codex/Cursor chat bus；Mac 本地数据不等于 Cloud 可访问数据。
Done criteria: 本次两个指定 Markdown 文件字节一致，顶端交接字段齐全，含数据覆盖、标签、成本/OOS/样本门槛及 Cursor 落点。Cursor 后续完成 P0 时回传 PR URL、head SHA、required checks、审计/配置/输入清单 SHA-256、逐目标有效/无效行数和拒绝原因；通过骨架测试不等于策略通过。
CONFIRM / trading gates: human CONFIRM still required for any live trading；LiveSmall activation、signed deployment envelope、runtime approval/risk/reconciliation、sealed holdout access、Bybit dataset admission、cost/capacity gates 均保持 fail-closed；本包不打开任何门，不授权自动 Paper/Shadow/Live 推进。
Branch: cursor/sec-orderflow-ml-scaffold
Writer: Cursor Cloud Agent
```

# 秒级订单流 ML / DL / RL 研究包

日期：2026-09-17。状态：**analysis / plan only**。研究模型要求：`gpt-6-astra`, reasoning effort `xhigh`（本次用户要求优先于 handoff 文档的旧默认；这是请求配置，不是运行时模型身份凭证）。本包作者 Codex；上方 Writer 指后续实现的唯一写者。本轮续写前次草稿并生成指定的完全相同副本，没有运行拟合、回测搜索、Campaign 或交易操作。覆盖统计、完整数据扫描与摘要均复用前次审计；本轮未重新全量扫描。

## 1. Feasible vs not / 当前数据密度下能做与不能做的研究

**当前可做数据审计、标签原型和有限的预测可行性探针；不能认定秒级可交易 alpha，不能训练出可信的真实排队位置模型，也不能把 32.8 小时数据量包装成 DL / RL 的充分证据。**

主问题：过去 5m / 15m / 30m 的背景，加上最近 1s / 5s 的成交与当时可用盘口，能否优于价格历史、持续性和随机对照，预测未来 5s / 10s / 30s / 1m / 5m 的收益分布、方向、路径极值和触价风险？经济问题另行回答：这些预测能否改善**成本后期望、盈亏比和回撤**，而非仅提高胜率？

| 能力 | 当前判断 | 可接受的研究输出 |
| --- | --- | --- |
| `flow/trades` 的 1s / 5s 流量与 5m / 15m / 30m 背景 | 可重建有成交秒的聚合；时钟与缺失语义不完整 | 原始字段审计、缺失掩码、带限制的 last-return / OHLC 路径标签 |
| `flow/book` 的 mid、spread、L1/L5/L20 深度 | 约 10s 一次，不能提供连续 1s / 5s 队列变化 | 30s–300s 稀疏 mid 探针；5s mid 主标签应拒绝或严格筛选，不插值伪造 |
| 额外发现的 `data/l2` 秒级摘要 | 有约 21.6h，但来源时钟、断线新鲜度和序列完整性未经证明 | 独立的 `legacy_l2_exploratory` 数据版本；静态 mid/microprice/imbalance 消融 |
| 真正 queue position、撤单归因、maker fill、冲击/容量 | 当前不可识别 | 输出 `unsupported`；不能将挂单量变化视为自己订单的排队变化 |
| mark / index / funding / OI | 被检查的 flow 与 l2 schema 均无相应历史字段 | 先定义契约，标签为 missing；补采后才启用 |
| GBDT / 小型序列模型 | 适合后续阶梯；目前只够管线与小样本诊断 | 不把拟合收敛或训练分数当 OOS |
| 深层 LOB 模型、Transformer、RL | 当前样本日历与数据质量不足 | 作为后续受门槛控制的实验，不作为首轮实现目标 |

以 **MARSCOIN、60s、mid-return** 为预注册主问题；当前严格 mid 数据门未过时明确报告主问题不可检验。`last-return` 或 `mid_sampled` 可作独立探索，不冒充主问题通过。其余两币、5s/10s/30s/300s、long/short 均为预登记的次级检验。所有标的沿用相同数据规范；参数是否共享以 train-only 协议决定，逐币逐方向结果分别报告。

旧材料属于假设来源：`orderflow-strategy-v2.md` 的 S1–S5、`strategy-v2-triple-gate.md` 的“位置 × 事件 × 裁判”、`directionality-findings.md` 的多空差异都不能当已验证先验。后者自己的表格列出宽池 OOS 为 −2.4 至 −3.2R；其标题性结论不构成秒级策略证据。旧 16.5 天 K 线研究不能增加本次 flow 数据的覆盖天数。用户已拒绝 S1–S5 上 Live，本文完整保留这一边界。

### 1.1 Evidence / 复用前次本地覆盖与数据质量审计

#### 1.1.1 固定审计窗口

前次只读扫描于 **2026-09-17 00:51:17 UTC（08:51:17 CST）**开始，统一取 `ts < 1789603200000`，即 **2026-09-17 00:00:00 UTC / 08:00:00 CST**。未计正在增长的当前小时。flow 的 trades/book/big 使用文件名早于 `2026-09-17_08` 的文件并再次按行内时间过滤；liq 按行内时间过滤。L2 文件名混用 UTC 日期与本地小时，导入必须以行内时间为准，本次所选文件覆盖截止前的实测区间。

来源根目录：`/Users/proerror/tools/marscoin-footprint/data/flow/{MARSCOINUSDT,NIULAIUSDT,HAJIMIUSDT}/`。每币 trades、book 各 33 个小时文件；首日为部分小时。下表覆盖率 = 不同 `sec` 数 / 首尾间日历秒数，不是已证明的采集在线率。

| Symbol | trades 首尾 UTC | 跨度 h | 原始行 | 不同成交秒 | 同秒额外行 | 有成交秒占比 | 相邻有成交秒最大间隔 |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| MARSCOINUSDT | 09-15 15:11:35 → 09-16 23:59:56 | 32.80583 | 52,658 | 44,559 | 8,099 | 37.729% | 45s |
| NIULAIUSDT | 09-15 15:11:38 → 09-16 23:59:41 | 32.80083 | 29,169 | 26,275 | 2,894 | 22.251% | 77s |
| HAJIMIUSDT | 09-15 15:11:39 → 09-16 23:59:41 | 32.80056 | 6,956 | 6,462 | 494 | 5.472% | 553s |

| Symbol | book 行 | 间隔 p50 / p95 / max（秒） | >15s 间隔数 | spread p50 / p95（bp） | 1s 网格上 book age ≤1s / ≤5s |
| --- | ---: | --- | ---: | --- | --- |
| MARSCOINUSDT | 11,808 | 9.999 / 10.619 / 20.576 | 2 | 5.03 / 10.21 | 10.003% / 49.992% |
| NIULAIUSDT | 11,809 | 9.999 / 10.622 / 20.187 | 2 | 6.26 / 9.89 | 10.009% / 49.999% |
| HAJIMIUSDT | 11,805 | 9.999 / 10.625 / 20.161 | 7 | 20.98 / 33.18 | 10.000% / 49.980% |

盘口从约 09-15 15:11:34 UTC 到 09-16 23:59:52 UTC，全部各侧 20 档，本次未发现 `bid1 >= ask1`；已选 trades 未发现无效 OHLC，已选流未发现 JSON 解析错误。**这些检查不证明网络无缺失、正确到达顺序、真实 availability 或可成交性。**约九成决策秒没有 1s 内的新 flow 盘口；5s 目标尤其不能靠复制旧盘口取得假精度。

另已实测 `/Users/proerror/tools/marscoin-footprint/data/l2/<SYMBOL>/l2-*.jsonl`，首条约 09-16 02:26:09 UTC，截止前约 **21.5639h**：

| Symbol | L2 摘要行 | 不同接收秒 | 秒覆盖率 | 间隔 p50 / p95 / max（秒） |
| --- | ---: | ---: | ---: | --- |
| MARSCOINUSDT | 77,548 | 77,545 | 99.889% | 1.001 / 1.362 / 46.213 |
| NIULAIUSDT | 77,541 | 77,534 | 99.875% | 1.001 / 1.446 / 46.033 |
| HAJIMIUSDT | 77,522 | 77,506 | 99.839% | 1.001 / 1.516 / 46.216 |

L2 高行密度来自定时输出，**不等于每秒收到新盘口**。`collect-l2.js` 断线处理没有清空 `ready`，定时器仍可写旧状态；没有保存 event time、`u/seq` 或最后有效盘口接收时刻。因此这些行不可据 `ts` 新鲜就通过严格报价门。

32.8h 只有约 1.37 个日历日。按 `30m context + 5m label + 5s buffer` 完全不重叠切块，最多约 **56 块/币**，这是分块数量估算，不是测得的统计有效样本量。三个同市场、同时间段的币不能简单乘三作为独立 regime。1Hz 扩行、重采样、增加 trial 或使用三种 context 都不创造独立信息。

#### 1.1.2 真实 schema 样本

以下是 `MARSCOINUSDT/trades-2026-09-15_23.jsonl` 首行摘录，字段保持真实命名：

```json
{"ts":1789485095000,"sec":1789485095,"o":0.09227,"h":0.09227,"l":0.09227,"c":0.09227,"buyVol":0,"sellVol":8440,"buyTo":0,"sellTo":778.7588,"buyN":0,"sellN":2,"tradesN":2,"tradesTo":778.7588,"vwap":0.09227,"netTo":-778.7588,"delta":-778.7588,"cvd":-778.7588,"cvdClean":-778.7588,"washTo":0,"washN":0}
```

同目录 `book-2026-09-15_23.jsonl` 首行摘录（bids/asks 各仅展示前两档，源行各 20 档）：

```json
{"ts":1789485094695,"symbol":"MARSCOINUSDT","mid":0.0923,"bid1":0.09227,"ask1":0.09233,"spreadBp":6.5,"bidNotional5":359.83,"askNotional5":368.53,"imb5":0.9764,"imb20":0.929,"bids":[[0.09227,3160],[0.09226,120]],"asks":[[0.09233,330],[0.09234,130]]}
```

这里的 `imb5` 是 **bid/ask 名义额比值**，不是 `[-1,1]` 标准化失衡。`data/l2` 的同名 `imb5` 却是 **base quantity 的 (bid−ask)/(bid+ask)**，两个数据源不能按同名直接拼接。历史文档的 `activeBuyVol/activeSellVol` 不是实际 flow 字段，应使用 `buyVol/sellVol`。

#### 1.1.3 导入前必须处理的具体问题

| 证据路径 / 位置 | 观察或代码推断 | Cursor 处理规则 |
| --- | --- | --- |
| `collect-flow.js:216–279,355–356`；`atoms.py:grid` | 定时 flush 与跨秒 flush 产生同秒分片；`ts` 只是秒起点，没有收齐时间 | 数量/名义额/笔数求和，high=max、low=min；open/close 按原文件追加顺序合并。保存 path+line 身份；跨文件同秒顺序无法证明则隔离。不能把分片当重复删除，也不能只保留最后一行 |
| `collect-flow.js:250,272–273` | 跨秒新成交先计入 `st.cvd`，随后写上一秒，已有 CVD 可能包含后一秒成交；重启又归零 | **丢弃历史 cvd/cvdClean/d30/d60/d300 作为训练输入**，用已完成的 buyTo−sellTo 重算 trailing delta/CVD；session 边界显式重置 |
| `collect-flow.js:148–154,237` | d30/d60/d300 按本机 now 截断且当前行尚未入 ring | 按统一 event/available clock 重算，不沿用预计算值 |
| `collect-flow.js:168` | `sq[i]` 是对象却判断 `sq[i] > 0`；实测三币所有 washTo=0 且 cvdClean=cvd | wash/clean 字段禁用；不能推断“市场无对敲”，也不声称能凭匿名成交流确认 wash trading |
| `collect-flow.js:157–214,272,288,355` | big 的时间是后续 flush 触发时间，timer 未先 flush 毫秒桶；阈值可能已含下一笔交易 | big/sweep/block/maxTo 先隔离；重新有原始逐笔和严格 watermark 后再启用，不倒填成原桶发生时刻 |
| `collect-flow.js:315` | REST book 的 ts 为响应后的本地时间，未保存交易所快照时间/序列或请求 RTT | 可以当本地“最早可见”近似，交易所新鲜度仍 unknown；增加 request/receive/event 时间后才通过严格门 |
| `collect-flow.js:377`；`liq-*.jsonl` | 103/89/20 条强平事件全部缺 side；代码读 `t.side` | signed liquidation feature 禁用。官方字段是 `S`，而且表示被强平仓位方向，`p` 是破产价，不是普通成交价；不能拿它做 last-price 标签 |
| `atoms.py:163–170` | 查不到过去盘口时强取第 0 张，可能取到未来 | 缺失即 None，禁止 forward join / backward fill；增加首个盘口晚于决策时刻的回归测试 |
| `atoms.py:grid`；`signals.py:81–87` | 缺失秒无条件置零并填价；ATR 预热无效时取第一个后续有效值 | unknown gap 与真无成交分开；ATR/history 不足则 abstain，不能拿未来完成值补预热 |
| `signals.py:105–145` | 用秒 OHLC 模拟路径，净值主要减费用，没有完整逐单 spread/depth/latency 模型；short MFE/MAE 的高低价映射也需修正 | 只作待验证参考；新 cashflow ledger 独立测试 long/short 和同秒双触，不复制旧“无前视/可部署”结论 |
| `collect-l2.js:94–95,184–210` | 1.5s 内该价有交易就把档位减少归为 fill；不按成交数量分配、无完整序列证据 | add/cancel/fill 都标 proxy；不作为撤单真值、填单真值或 RL reward；重连需新 snapshot 后才 ready |
| `collect-l2.js:156` | UTC 日期 + local hour 的文件命名 | 跨午夜不可按文件名当时间顺序；按行时间排序，保留来源与冲突，禁止用命名修造时间 |

强平方向与价格语义已对照 [Bybit All Liquidation 官方文档](https://bybit-exchange.github.io/docs/v5/websocket/public/all-liquidation)。上述旧代码只读，不在本次修改；静态代码风险与本次实测缺陷已分别注明。

## 2. Leak-safe labels / 5s、10s、30s、1m、5m 的精确标签

### 2.1 时钟和基础价格

全部存 UTC int64 时间；`h ∈ {5,10,30,60,300}` 秒。决策 `t` 是模型实际可消费特征的 **available_time**，不是成交秒起点。新增采集应保存 `event_time, receive_time, available_time, ingest_time, session_id, sequence/trade_id`。特征必须在 t 前到达，且聚合桶已封闭。

1. 成交桶使用半开区间 `[s,s+1s)`，在 `s+1s` 后按 watermark 封闭；迟到记录不允许悄悄改写以前决策看到的行。旧数据无 receive/watermark：只能设 `availability_quality=unknown`，做 event-time 假设探针；加 2s/5s 延迟敏感性也不能把 unknown 变 verified。
2. 新采集的特征窗口为 `[t−L,t)` 中 `available_time < t` 的已知记录；最长 `L=1800s`。不使用居中 rolling、全样本标准化、未来补值或未完成 K 线。
3. `P_mid(u)=(best_bid+best_ask)/2`：取 u 时已经可见的最近一次有效、未锁/未交叉报价。`P_last(u)`：u 前最近实际成交价；旧 flow 用最后已结束秒的合并 close 近似，年龄按该秒起点计算保守上界。`P_mark(u)`：独立 ticker 的 markPrice；不允许用 mid 或 last 填补。
4. 严格主标签的端点 freshness：mid/mark 的**源事件年龄和接收年龄均 ≤1s**；last 年龄上界 ≤1s。事件时间未知或连接状态未知则不能进入严格集。另设 `legacy_mid_sampled_15s`：REST 本地 as-of 年龄 ≤15s，只有 h≥30s 可探索，明确标示端点陈旧；与 strict 标签分开统计。旧 L2 摘要另立 source，不能与 REST 不留来源地补接。
5. 每个 target 独立保存 `valid, invalid_reason, source, start_age_ms, end_age_ms, label_available_time`。缺值为 null，不编码成 flat/0。未来无新成交造成 last 不动时，同时记录 `future_trade_count`；不能让 stale-last 猜零模型取得虚假优势。
6. 本包固定时域端点为 t+h，不取“此后第一笔”替代：后者是随机长度的 next-event 标签，若另研究必须另命名。离线标签只允许查看其自身未来区间，标签信息从不进入 X、筛选或归一化。

### 2.2 Return、direction、path 和 touch

对于价格类型 `p ∈ {mid,last,mark}`，相同时间端点、相同有效性掩码下定义：

```text
y_ret[p,h](t) = 10,000 × (P_p(t+h) / P_p(t) − 1)        # simple return, bp
epsilon[p](t) = max(1 bp, 10,000 × tick_size(t) / P_p(t))
y_dir[p,h](t) = +1 if y_ret > epsilon; −1 if y_ret < −epsilon; else 0
MFE[p,h,+](t) = max(0, max_{u ∈ (t,t+h]} 10,000 × (P_p(u)/P_p(t)−1))
MAE[p,h,+](t) = max(0, max_{u ∈ (t,t+h]} −10,000 × (P_p(u)/P_p(t)−1))
MFE[p,h,−] = MAE[p,h,+];  MAE[p,h,−] = MFE[p,h,+]
```

`tick_size` 缺失时 direction/touch 为 invalid；`C_rt_est` 缺失时成本障碍 touch 为 invalid，不能填零。`future_trade_count`、未来路径质量与标签是否有效仅供目标构建和事后诊断，不得进入 t 时特征或可交易入场筛选；经济回放中未来数据中断必须报告不可评估/保守退出，不能事后删除亏损机会。

`tick_size(t)` 必须来自 point-in-time instrument metadata；当前从相邻盘口差推断只可诊断。方向死区不按全样本标签分位数决定，也不与盈利阈值混用。主任务为 return regression / conditional quantiles `{0.1,0.5,0.9}`，direction 是配套校准任务，flat 必须保留。

对于每个 p、h，在 t 固定障碍 `a_h(t)=b_h(t)=max(2×epsilon[p](t), C_rt_est(t)+5bp)`；C_rt_est 仅用 t 时已知信息和预注册成本假设，未来实际成本只用于评估。障碍参数不随未来波动调整：

```text
U_p = P_p(t) × (1 + a_h/10,000); D_p = P_p(t) × (1 − b_h/10,000)
tau_U = inf{u ∈ (t,t+h] : P_p(u) >= U_p}
tau_D = inf{u ∈ (t,t+h] : P_p(u) <= D_p}
touch_up/down = 是否在截止前触及各自障碍（两个可同时为真）
first_touch = up_first | down_first | none | ambiguous
touch_time = (tau − t, interval_resolution)，没触及为右删失 h，不是零秒
```

这定义**市场价格路径**，不定义我的订单是否成交。对 last 的秒 OHLC，可用未来各秒 high/low 得到观察到的极值和触及区间；同秒同时碰上下障碍时先后为 `ambiguous`，分类不得猜顺序，经济回放按 adverse-first。无序列完整性或窗口中有 unknown gap 时，“没观察到触及”不能标成 true none，须 censor。mid/mark 的稀疏采样极值只是 `observed_mfe/mae` 下界，不能假装完整 MFE/MAE 或连续 barrier path。

| Horizon | 端点标签（各 p 分别计算） | 路径范围 | 当前 flow 能力 | 新数据验收要求 |
| --- | --- | --- | --- | --- |
| 5s | `ret/dir_{mid,last,mark}_5s`，P(t)→P(t+5s) | 完整未来 5 个 1s 桶；touch 截止 t+5s | last 仅满足新鲜度/完整性子集可探索；10s book 不支持严格 5s mid path；mark 缺失 | 原始 trades+可验证 L2/ticker、≤1s freshness；秒内触价顺序需事件流 |
| 10s | `ret/dir_{mid,last,mark}_10s` | 未来 10 个桶 | 两张附近 REST 图并非精确 10s 返回；只能独立描述稀疏观察 | 同上；不可把 10s 平均间隔当固定 horizon |
| 30s | `ret/dir_{mid,last,mark}_30s` | 未来 30 个桶 | 可做 last 与 `mid_sampled_15s`；mid path 仍只给观察值 | 完整 source/availability、缺失统计、执行成本独立 |
| 60s / 1m | `ret/dir_{mid,last,mark}_60s`，主 horizon | 未来 60 个桶 | 首轮主问题仍受 strict-mid 门阻挡；可比较探索性替代标签 | 严格端点与完整路径分开验收 |
| 300s / 5m | `ret/dir_{mid,last,mark}_300s` | 未来 300 个桶 | 端点探针更合理，但重叠严重、独立样本少；HAJIMI stale-last 特别突出 | 最长标签成熟后方能分割；不因 h 长就放宽 provenance |

多步预测输出为这五个 horizon 的向量与各自 mask；没有承诺精确未来 tick 轨迹。5s 的 prediction horizon 不要求每 5s 开平仓；后续可以只改善既有、独立有边缘策略的进入时机。

### 2.3 Mark、funding 和 settlement 必须分开

本地源是 Bybit `linear`；采集器没有保存 instrument 元数据，导入时还须核验各 symbol 的 contract type 和有效期。对于目标中的永续合约，“未来 5m 收盘”不是到期结算，也不是 prediction-market 二元 settlement。

对全部五个 h 增加以下**待补采**标签，当前均 missing：

| 标签 | 定义 | 需要的数据 |
| --- | --- | --- |
| `mark_ret_h` / `mark_dir_h` / `mark_touch_h` | 按上文 P_mark 公式，区别于 last 和 mid | 带时间与 snapshot/delta 状态的 mark ticker |
| `basis_h` / `basis_change_h` | B(u)=10,000×(P_mid(u)/P_mark(u)−1)；预测 B(t+h) 与 B(t+h)−B(t) | 同时可用且足够新鲜的 mid、mark；可另列 mark/index basis |
| `funding_event_in_h` | t 时已公布的 nextFundingTime 是否落在 `(t,t+h]`，是已知条件，不冒充预测任务 | 当时的 funding schedule；不能硬编码 8h |
| `funding_realized_h` | 区间内实际 funding 现金流，固定符号持仓 q，`cost_USDT=Σ sign×q×mark(T_f)×rate_realized(T_f)` | 实际 funding rate、mark、event time；预计费率只可作 t 时特征 |
| `funding_settlement_basis` | 在下一次已知 funding 时点 T_f 的 mark/last/basis 及实际资金费；只有 T_f≤t+h 才纳入该 h 的条件任务 | 记录各自价格，不把 funding 时点当合约到期；无事件的行 event-mask=false |
| `realized_close_pnl_h` | 用 t 后可成交入场及 t+h 后退出的实际/模拟 cashflows 算净值 | latency、spread、深度、手续费、成交量与资金费；不是 mark-return 的别名 |

官方 ticker 提供 `markPrice/indexPrice/openInterest/fundingRate/nextFundingTime`，并具有 snapshot/delta 语义；缺字段可能表示未变化，需在有效 session 内重建，而非跨断线盲目 ffill。[Bybit Ticker](https://bybit-exchange.github.io/docs/v5/websocket/public/ticker)

若未来研究交割合约或二元事件，须另有 instrument/official settlement/outcome contract，落在对应 Monday 市场模块；本包不以 last/mark 伪造官方结算标签。标签成熟时间为所有所需终点/路径/结算信息到达时间的最大值；未知成熟时间不得通过训练与 holdout 边界。

## 3. Feature schema / flow 字段与真实队列模型缺口

### 3.1 类型、单位与 schema

每行主键建议 `(dataset_revision, venue, symbol, session_id, decision_time)`；必含 `source_sha256, source_path/line, feature_available_time_max, history_start, quality_flags`。base quantity、USDT notional、price、bp、秒数分别命名；不默认跨币数量可比较。

| 特征组 | 实际源字段 | 派生特征与窗口 | 缺失/限制 |
| --- | --- | --- | --- |
| 1s / 5s 主动流 | buyTo/sellTo、buyVol/sellVol、buyN/sellN/tradesN、tradesTo、o/h/l/c/vwap | signed notional delta、delta/total、trade intensity、均笔额、过去 1/5s return/range、价格冲击 proxy | total=0 时 ratio=null+mask；未知缺秒不能设零；不用存量 cvdClean |
| 5m / 15m / 30m 背景 | 完成的成交秒与有效报价 | L∈{300,900,1800}s 的累计 delta、VWAP 距离、return、realized vol、成交额分布、方向持续性、近期/背景强度比 | warmup 不足则 abstain；scaler/winsorization 仅 train；分币归一化 |
| 最近可见 book | bid1/ask1、bids/asks、bidNotional5/askNotional5、spreadBp | spread、L1/L5/L20 base 与 notional imbalance、深度斜率、microprice−mid、目标金额穿透深度 | flow 只有约10s状态；任何1s/5s输出带 book_age 和 new_snapshot_count，不伪造新报价 |
| book 差分 | 两次真实快照 | 按绝对价格对齐的 displayed depth delta / 实际Δt | 名称为 snapshot-depth-change，不能称 event OFI、撤单量或队列流失速度；价格层消失不证明撤单 |
| 静态 L2 摘要（探索可选） | mid/micro/microSkewBp、bid1Sz/ask1Sz、imb1/5/20 | 1s/5s 状态统计、5m/15m/30m聚合，与 flow-only 做配对消融 | 需固定在相同时间子集上比较；imb定义与 flow不同；时钟/断线未通过前不进入 strict 集 |
| L2 动态 proxy | addsQty/cancQty/fillQty、addB*/cancB*、topCancQty | 仅数据质量审计或单独 proxy 实验 | 禁止称真实 cancel/fill 标签；不能反推出 MBO |
| 路径位置/流量冲击 | 过去 OHLC/VWAP/flow | 过去区间分位、距已知极值、单位名义额位移、吸收 proxy | 从OHLC均分到价格桶仅估计；不宣称精确足迹/冰山；不用全日事后 value area |
| 流事件 | big、liq | 当前只允许合格来源的 unsigned liquidation count/size 诊断 | big 隔离；liq side缺失、bankruptcy价格不能混入last；没消息与掉线区别未知 |
| 数据质量 | 当前源不完整，导入新建 | trade_age、book_age、各源mask、gap length、connected/sequence_verified、reset flag | 质量特征不能掩盖数据门；模型不得通过学会缺失模式“修复”交易证据 |
| 待补采 | ticker/instrument/funding | basis、ΔOI、funding、time_to_funding、tick/lot/min-notional与有效期 | 现有三重门 OI 裁判无法从 flow 重建；不能设 OI=0 或挪用别币 |

microprice 明确定义为 `(ask1×bid1_size + bid1×ask1_size)/(bid1_size+ask1_size)`；imbalance `I_k=(Σbid_qty−Σask_qty)/(Σbid_qty+Σask_qty)`，另有同式 notional 版本。重算时保留分母与 mask，不能同时把相同比值的多个单调变换视为独立证据。

序列输入建议采用多尺度表示：最近 60s 的 1s token、最近 5m 的 5s token、最近 15m 的 15s token、最近 30m 的 30s token；所有桶右边界不晚于 t，末桶未完成则不用。重叠区间只是同一历史的不同表示，不增加 N。另做 5m-only / 15m-only / 30m-only 受预算控制消融；最长实际特征依赖决定 purge，不能按 token 数决定。

### 3.2 真正 queue 模型的数据缺口

需要原始、可重放的 L2 snapshot/delta（交易所时间、接收时间、`u/seq/cts`、连接/恢复事件）、逐笔 trade ID、taker side、price/size、instrument rules，才能检验 1s/5s 的**队列总量动态**。官方公共 book 是价格层聚合，不提供本人的队列优先级；snapshot/delta 的重置语义必须严格实现。[Bybit Orderbook](https://bybit-exchange.github.io/docs/v5/websocket/public/orderbook)，[Bybit Trade](https://bybit-exchange.github.io/docs/v5/websocket/public/trade)

**精确个人 queue position** 还需要可用的 MBO/order-id priority 或自有订单的 accepted/amend/cancel/fill 回报、同价优先级规则和延迟。公共 MBP 加逐笔也只能得到在假设下的队列估计，隐藏量和事件合并仍不可识别。本包不通过真实下单获取这些标签，不订阅私有账户流，不假造 FIFO 真值。未来补采属于单独的数据工作，研究结果中必须保留 estimation bounds。

## 4. Model ladder / baselines → GBDT 与浅序列 → DL → RL

| 阶段 | 模型与输入 | 升级条件 / 当前结论 |
| --- | --- | --- |
| B0 | return=0、价格持久性、train-only 类别先验、no-trade、同频随机 side/entry | 所有模型强制对照；识别 stale-price 猜零优势 |
| B1 | 仅历史价格的 momentum/reversal、单一 signed flow、imbalance/microprice 线性式；Ridge、logistic、浅 CART | 复用 Monday 已有 Ridge/CART；先≤32个预登记特征，不搜索 S1–S5 最优规则 |
| M1 | GBDT（depth≤3，≤200树的初始范围），带 lag 的 Ridge、小 MLP / 单层小 GRU/TCN | 先胜 B1 的 OOS predictive loss 和配对净收益，再讨论复杂度；GBDT 若无现有纯Rust实现，单独提依赖设计，不引入 Python/LightGBM 子进程 |
| D1 | Burn 的小型 causal TCN/GRU，多尺度 flow/context encoder；多h回归/分位数/方向头 | 新高质量多日数据、M1通过后进入；loss按target mask、train-only尺度归一；不以 raw loss 小就判收敛 |
| D2 | 有足够原始 LOB 数据时才考虑 DeepLOB 风格空间卷积+时间模型；Transformer只在有额外收益证据后测试 | 现有10s图和21.6h摘要不具备入场条件，不先堆大模型 |
| R1 | offline RL / contextual bandit 的受约束执行研究 | 固定上游交易任务、方向、数量和deadline；只学何时/如何执行，不授予方向开仓或风险预算权 |

DeepLOB 是 LOB 空间卷积与时间依赖模型的参考，原论文使用的市场与数据规模不能外推为本组三币收益证据。[DeepLOB 原论文](https://arxiv.org/abs/1808.03668)

监督模型训练须报告 train/validation loss、梯度/参数有限性、target scale、naive loss、学习曲线、初始化稳定性、推理一致性和实际时延。early stop 只看 inner validation；outer OOS 与 sealed holdout 不参与更新次数选择。多任务归一尺度仅来自训练区间；以 raw bp 输出评估。训练、Burnpack reload、portable inference 应使用相同 ordered features 与缺失策略。

RL 的 state 可以含冻结预测、合格 L2、剩余数量、剩余时间、模拟 inventory；action 为 wait / passive price offset / bounded marketable slice / cancel-replace；reward 为相对 arrival benchmark 的 execution shortfall 加 fees、inventory 与 deadline penalty。必须包含未成交、partial fill、adverse selection、queue uncertainty 和状态转移时延，基线为 immediate taker、固定 passive、TWAP/POV。无 action log 与支持覆盖时不能假称可靠 off-policy evaluation；模拟训练只能报告 simulator-bound 结果。[Double Deep Q-Learning for Optimal Execution](https://arxiv.org/abs/1812.06600)

当前数据不足以校准这样的模拟器，**RL阶段关闭**。若将来主张 RL 直接选择方向，必须另立可证伪目标、奖励/市场冲击和样本支持论证，先证明监督预测加固定 policy 不能解决；不在本包或 scaffold 中实现。Monday 现有 offline Q-learning 是研究搜索 policy，与本节执行 RL 是两个语义，不能混同或借它绕过 lab-only 边界。

## 5. Evaluation gates / 成本、IS/OOS、purge、基线与最小样本

以下数值是本包提出的**预注册研究门槛**，不是统计定律，也不是已通过结果。任何缺失项返回 `insufficient_data` / `unsupported`；不得降低门槛使当前样本“通过”。

### 5.1 成本和 cashflow

`signals.py:33–39` 留有每边 taker **11bp**、maker **4bp**，三币往返摩擦 **28.5 / 30.1 / 42.3bp** 的历史假设。早期报告用的是 taker 5.5bp，二者不兼容。**本次未查询账户费用或成交账单，11/4bp 不代表当前账户的已验证费率**；保留为审慎场景输入。后续资格依赖当时适用的 symbol/account fee evidence 或明确的、有来源的保守费率上界。[Bybit Fee Rate API](https://bybit-exchange.github.io/docs/v5/account/fee-rate)

```text
C_rt_est_bp(t,Q) = f_entry + f_exit
                + halfspread_entry + expected_halfspread_exit
                + depth_slippage_entry(Q) + expected_depth_slippage_exit(Q)
                + latency/adverse_selection_allowance + funding_cost_bound
```

只用本次 spread 中位数，加 taker 双边22bp，**未计深度、延迟、资金费**，粗场景下 MARS/NIULAI/HAJIMI 的往返成本已约 **27.03 / 28.26 / 42.98bp**。这只是基于样本分布的成本说明，不是每笔当时可知成本或账户报价。若5s预测毛变化只有几bp，即使分类正确也没有独立 round-trip 空间。

经济回放必须以 `entry_time >= decision_time + latency`，在该时点有效 ask 买/bid卖、逐档走量；退出同理。数量固定或由 train-only policy 限定，名义额与base数量不可混淆。只有价格和数量充分的可见深度才可模拟；超出20档不按末档无限成交。手续费按每边真实成交名义额计，不按保证金或杠杆缩小。

固定 q 的 long/short 交易：`net_USDT = sign×q×(P_exit−P_entry) − fees_USDT − signed_funding_cost_USDT`，`net_bp=10,000×net_USDT/(q×P_entry)`。如果执行价已包含 spread/深度穿透，不再重复减同一 spread/slippage；额外延迟或冲击假设单列。收益轨迹按持有数量逐步 mark-to-market 与现金流累计，不把每秒的重叠5s/300s标签相加，也不通过名义杠杆创造 alpha。各 fold/series 初始flat，末尾在本fold内退出并计成本。

预注册三组压力：base、额外非手续费摩擦×1.5、×2；假设latency `{0.25,1,2,5}s` 分别回放，低于原始时钟分辨率的结果只能标 assumption。maker 费用仅用于真正模拟得到的 maker fill；不能把 taker 的同价同量路径简单换成低费率。当前缺合格queue/fill证据，maker策略不得通过资格门。

### 5.2 Split、purge、embargo、污染边界

当前32.8h已用于多次探针和本文设计，全部视为 **development / exposed data**，不能从尾部再命名 sealed holdout。可以按时间 60/20/20 验证管线，但只能叫 plumbing split，不能作为盈利 OOS；不得随机行切分。

后续第一轮预注册日历模板为至少 **42个合格日历日**：前14d IS训练、随后7d inner validation/校准；再14d locked walk-forward OOS，分为4d/5d/5d三fold；末7d全新sealed评估区间。跨币使用相同UTC边界。只有在已授权的研究契约支持相应 sealed evaluation 后才可开封；当前 ML v4 未具备此最终授权路径，见§7。遇到不合格日按冻结规则延长采集，不用删除坏天改善分数。

外层fold按既定 expanding-window refit：只能使用该fold开始前已成熟的标签；上一fold数据可进入后续训练，但模型、阈值、特征族、refit频率不根据观察到的outer分数再修改。每次修改生成新trial并消费新的未来评估数据。walk-forward OOS 已对搜索暴露时必须标 selection-visible，不能代替未暴露终检。

样本依赖区间明确为 `[feature_history_start, max(label_end,label_available_time,simulated_exit_available_time)]`。主保守协议：前一fold训练样本依赖终点必须 **严格早于下一fold首个验证样本的 history_start**；最长context1800s、label300s、预注册latency/close buffer 5s时，决策点间隔至少 **2105s（35m05s）**，按实际availability更晚者扩大。单纯label purge 的下限是300s+buffer，但不足以达到本包完整窗口不重叠的要求。

对任何允许未来样本作训练的辅助 blocked-CV，移除依赖区间与validation相交的训练行，并在validation末尾再 embargo 至少2105s；首选纯向前walk-forward。若引入6h/24h/4h背景或2h持仓的triple-gate，重新按实际最大依赖扩大间隔，不能继续套35m05s。特征标准化、缺失填充参数、PCA、因子方向、筛选和概率校准全部在inner/train内拟合。

### 5.3 最小样本、检验与 pass/fail

| Gate | 预注册门槛 | 当前状态 |
| --- | --- | --- |
| G0 数据正确性 | source/hash/schema完整；无未来join；重复/迟到明确；严格目标源时钟、gap、freshness可验证；所有不可用行及原因可追踪 | 未通过，legacy允许诊断但不能升级 |
| G1 探索性拟合 | ≥7合格日；每target训练至少1,000个相隔≥h的有效锚点；分类每类≥200；history warmup完整 | 未通过；本次不拟合 |
| G2 baseline/ML正式比较 | ≥42日历日及三外层fold；训练有效非重叠锚点：h≤60s每target≥5,000，h=300s≥1,000；outer每fold≥200，合计≥1,000；每分类状态outer合计≥100 | 未通过 |
| G3 策略资格样本 | OOS+sealed合计≥300笔非重叠已结束交易；被保留的symbol×side×policy分支≥100笔；sealed本身≥100笔；评估覆盖≥20个实际交易日，且至少3个按train定义的波动/流动性状态 | 未通过；无交易分支不得用其他币补足 |
| G4 DL升级 | ≥90合格日、多regime；训练≥1,000,000个合格真实行（不计ffill复制）且≥10,000个h分离锚点；B1/M1先通过；所有OOS/交易门仍适用 | 未通过；行数不是充分条件 |
| G5 真实执行能力 | 完整事件回放、latency/fee证据、容量/partial-fill/queue限制披露；按目标金额逐笔验证，不靠更小金额替代 | 未通过；当前只能成本敏感性 |

锚点相隔h只是消除标签直接重叠，**不等于统计独立**。报告 `raw_rows / unique_seconds / valid_targets / h-separated anchors / time-blocks / trading_days / trades`，不混为 N。置信区间按共同UTC日跨币联合block bootstrap，并做≥35m05s及更长相关块的敏感性；实际相关尺度更长时扩大块长。低于足够独立日/块时不报告可信t或显著性，标 insufficient。若估计 N_eff，写明自相关估计方法与误差，不能拿1Hz行数开平方算高t值。

预测门：return 用 MAE/MSE、OOS R²、IC/RankIC、按日IC稳定性；分位数用 pinball loss 和覆盖率；方向用 balanced accuracy、macro-F1、Brier/log-loss、各类混淆；touch用Brier、校准和删失/ambiguous比率。主损失相对**最强简单模型**的配对改进需95%置信区间下界>0，三fold方向一致；任一次复杂度升级另需至少2%的相对损失改善（本研究的实用阈值），避免把极小改进换成高延迟。

经济门：净均值与其95%区间、净中位、胜率、平均赢/平均亏、profit factor、drawdown、tail loss、turnover、capacity、abstain率并列；`expectancy=p_win×avg_win−p_loss×avg_loss` 优先。base净期望95%下界>0，三fold至少2个正且最差fold不得突破预注册回撤约束；×2摩擦压力下净均值仍>0；移除最佳10%交易后净均值>0；配对优于同交易数/持仓/成本约束的随机和最强简单策略。独立round-trip的入场门暂定 `predicted_gross_mean > 2×C_rt_est+5bp`，且校准后的净优势下界>0。该条件不能靠人为把TP设远来通过。

研究回放使用无杠杆标准化1,000 USDT账本、每笔最大100 USDT名义额、单币单仓无加仓；名义额还受当时可见Top5同侧深度5%及前60s成交名义额1%上限约束，低于min-order则不成交。预注册最大组合回撤5%；这只是实验比较口径，**不是修改live风险**。在50/100 USDT分别报告容量敏感性；任何额外3,000 USDT目标需独立验证，不能继承小金额结果。

胜过随机：至少1,000个冻结seed的匹配随机entry/side轨迹，保留相同eligible时刻、方向比例、交易次数、持有分布和不重叠规则；按预先定义regime匹配，使用相同成本/失败退出，比较候选相对零分布。推断检验以跨日/跨币联合block置换或stationary bootstrap保留序列结构，不能IID打乱单秒。所有币/horizon/方向/feature族/参数/seed的尝试登记在同一testing family，用max-statistic或Holm控制family-wise 5%；FDR仅供探索排序。

Monday 的现有adjusted score只是其声明的Gaussian expected-maximum修正，**不能称已经实现完整DSR/PBO**。如增做DSR/PBO须保留全trial收益矩阵并单独验证实现；不把它们当缺少新鲜OOS的补救。[Bailey / López de Prado, Deflated Sharpe Ratio](https://doi.org/10.2139/ssrn.2460551)

三重门候选如果未来被研究，原最低要求“≥100独立信号且净中位>30bp”仍要报告，并同时通过本节更完整的成本、样本和OOS门；不能借 ML 包装放松旧约束。

## 6. Strategy mining / 从 signals 到 rules，控制过拟合

这里将 OF 理解为 **overfitting（过拟合）**。订单流特征仍是研究对象，同时强制 price-only 消融，量化订单流究竟增加了什么。

1. **Freeze hypothesis**：主假设为“流量/盘口在相同历史价格与质量条件下改善60s分布预测，并改善成本后交易选择”。列出 falsifier：只在stale样本有效、只赢弱基线、扣费转负、依赖单一日/币/最佳交易、延迟1–5s后消失，均拒绝。
2. **Feature registry**：每个特征冻结公式、单位、历史范围、source、missing policy、availability与hash；保留 price-only、flow-only、book-only、price+flow+book 四类对照的确切列序。组合使用既有 Factor Bank / search 接口，不另建并行发现系统。
3. **Bounded trials**：建议首轮预算最多24个完整pipeline配置、每个最多3个预登记seed；币/horizon固定输出不是免费多次择优。所有退出阈值、信号方向反转、horizon/context选择都算trial；失败、空结果和人工删选也登记。预算是建议，默认run_enabled=false，没有获批resource/trial grant不启动。
4. **OOF only**：训练段拟合predictor，inner validation只用于特征/模型/校准与策略阈值选择；用cross-fitted OOF预测产生策略候选，不能对训练内预测优化入场规则。outer OOS上的选择可报告但视为已暴露，封存终检才能支持新结论。
5. **Restricted rule grammar**：只允许 `quality gate AND calibrated net-edge threshold AND optional single past-regime filter`，long/short/no-trade分开；除强制 quality gate 外，最多两个条件门和一个退出模板，树深≤2。阈值仅从预登记训练分位点选，优先宽平台；禁止成百上千的“位置×事件×裁判×币×方向×TP/SL”笛卡尔积。
6. **Policy mapping**：默认固定60s持有的taker研究对照，单币单仓，已有持仓不重复开同向；其次才测试基于预测衰减的hysteresis退出。time cap固定≤300s；停止/触价只用当时可见路径，gap/双触按预登记保守规则。成交前超时取消并记未成交；窗口结束不得借下一fold价格平仓。宽止损不得提高名义风险预算。
7. **Ablations / robustness**：同一eligible时间集上移除flow/book/长context，逐币逐方向、时间状态分解；固定延迟与成本压力，参数邻域和leave-one-symbol-out仅使用训练/内层过程。共同时段匹配后仍改善才算L2摘要增量，避免把不同时段误作feature效果。
8. **Selection / failure**：按净优势下界和稳健性先排序，再考虑复杂度、turnover和胜率；保留一个冻结winner或明确 `no_selection`。若所有独立开仓候选都被手续费杀死，结论就是“不成立”；可以另立“固定上游任务的执行择时”研究，但不偷偷延长到2小时或改成maker低费率挽救本轮。
9. **Sealed / readback**：冻结数据、模型、policy、试验账本和成本hash后才请求已有技术契约下的终检；对已打开holdout再调参，该holdout即作废，下一轮要未来新数据。最终交付 prediction/position/cashflow ledger 和失败原因，不交付Live可执行信号。

## 7. Monday landing / Cursor 路径、实验骨架与 No Live

### 7.1 已有能力与真正缺口

以本机 HEAD `577c9ddede29935f2505ec6115e29f9206b481d9` 为只读架构证据，前次已读取 `README.md`、`rust_hft/ARCHITECTURE.md`、`docs/architecture/REPOSITORY_LAYOUT.md`、`docs/architecture/RUST_ONLY_RESEARCH.md`、`rust_hft/alpha-harness/README.md` 和对应 ML/manifest 源码。**未刷新或宣称 GitHub当前main等于此SHA**；Cursor开始时读回remote main与PR head。

已有 `alpha-harness` 的 Campaign / GP / MCTS / Factor Bank、purged walk-forward、Ridge、CART、Burn MLP，以及 `research-core/manifest` 的冻结模型契约；`research-core/ml` 已有 dataset/feature/label hash binding。无需重新发明这些能力。

但现行 CEX public replay writer 的支持范围是 Binance USD-M，当前 supervised v4 终点是 **pre-holdout**；本地 Bybit flow不是只换 `venue` 字符串就能合法送入Campaign的数据。当前canonical L2 replay receipt也明确不证明queue position、market impact或真实capacity。`data-pipelines/adapters/adapter-bybit` 存在并不等于Bybit已获得governed dataset admission。不能以diagnostic `mission run/execute` 绕过这些边界。

### 7.2 提议落点（新增项均为建议，尚未存在）

| 范围 | 提议路径 | 职责 / 边界 |
| --- | --- | --- |
| durable contract | `rust_hft/research-core/manifest/src/sec_orderflow.rs` | 时钟、单位、schema、source quality、label/feature/split/cost/experiment manifest；serde拒绝未知字段 |
| research protocol | `rust_hft/alpha-harness/domain/src/sec_orderflow.rs` | typed experiment与资格状态，复用现有candidate/mission identities；无runtime authority |
| offline import/audit | `rust_hft/alpha-harness/app/src/sec_orderflow.rs` | 用户提供文件的只读导入、统计、封存manifest；入口命名由Cursor结合CLI决定，必须标diagnostic且不能发job |
| transformation/evaluation | `rust_hft/alpha-harness/engine/src/sec_orderflow/{mod,features,labels,splits,costs}.rs` | 确定性变换、target masks、purge、ledger和基线比较；不实现第二套订单/风控 |
| existing model seam | `rust_hft/research-core/ml/src/` 与 `alpha-harness/engine/src/baselines.rs` | 复用训练/推理一致性；P0不新增DL架构或GBDT依赖，P1再评估 |
| tests | 对应现有crate的 `tests/sec_orderflow_*.rs` 或模块测试 | 验证因果与拒绝行为；测试fixture仅为工程回归，绝不当研究市场数据 |
| human contract/example | `docs/plans/2026-09-17-sec-orderflow-ml-research-packet.md`；后续 `docs/research/sec-orderflow-experiment-v1.md` | 完整实验配置、字段字典、限制、可重现命令及负结果模板 |
| future raw acquisition | `rust_hft/tools/collector` 与已有 `rust_hft/data-pipelines/adapters/adapter-bybit` | 另项实现原始L2/trades/ticker及session/sequence；本包不启动、重启或替换采集器 |

不改 `apps/live`、`risk-control`、`execution-gateway`，不增加订单客户端或签名/approval旁路；不把 `atoms.py/signals.py` 复制进Monday生产研究路径。当前JS/Python仅为输入来源与反例证据，后续研究、训练和评估实现全Rust。新增public contract或重要依赖必须在后续任务范围明确后单独审查，骨架不暗中扩大生产接入。

### 7.3 分阶段实施与停点

**P0 — 本次交给 Cursor 的首个可独立验收行为：** typed schema + read-only importer/audit + label/feature/split契约 + deterministic test骨架。输入必须显式提供，不扫描云端不存在的Mac路径；默认只审计，不训练。数据不足时正常终结并输出 `research_eligible=false`。一个writer、一个worktree、一个PR；Monk路由后记录contract/owner/path/branch/base/allowed-files/dependencies于worktree-private登记位置。

**P1 — 单独后续研究实现：** 在合格输入上接入B0/B1和小模型实验、OOF规则映射、成本敏感性；首先保持pre-holdout与research-only。Bybit governed loader/replay、multi-horizon时钟或ML最终holdout需扩展时，另立对应契约，不能放宽现有验证器以适配legacy数据。

**P2 — 数据门通过后：** 纯Rust采集增强、跨日连续性证明、冻结新鲜OOS；之后才按§4逐级测试DL。RL执行模拟另立研究任务，无原始事件/可信fills则继续关闭。

未来CEX云研究仍使用 `mission campaign-freeze → campaign-finalize → mission dispatch submit → generated campaign-execute`；须先支持Bybit及本实验schema并具备签名grant/预算。遵循 `deployment/aliyun/research/README.md#data-flow-review-and-host-lifetime` 的ACK-only大数据/训练边界；Mac只处理现有本地诊断与轻量元数据，不成为云Campaign旁路。远端构建使用 `monday-remote-build`，不在ack-system节点放工具链/缓存。

Monk把包路由给Cursor；本次**没有发送消息、创建PR、推送分支或启动Cloud Agent**。本包也不自动授予未来实现merge或生产训练授权。Cursor完成时按同一handoff字段回传实际身份及检查；文档/骨架完成与研究结果通过是两个不同done criteria。

### 7.4 Experiment skeleton / 提议配置及验收

下面是契约草案，**不是已经存在可执行的CLI配置**；Cursor应与现有typed manifest整合，禁止宽松读取后忽略不认识的门。

```yaml
schema: sec-orderflow-research-v1-proposed
mode: audit_only
run_enabled: false
venue: bybit_linear
symbols: [MARSCOINUSDT, NIULAIUSDT, HAJIMIUSDT]
primary: {symbol: MARSCOINUSDT, horizon_s: 60, price: mid_strict}
decision_grid_ms: 1000
instant_windows_s: [1, 5]
context_windows_s: [300, 900, 1800]
horizons_s: [5, 10, 30, 60, 300]
targets: [return_bp, direction3, quantiles, mfe, mae, touch, first_touch,
          mark_return, basis_change, funding_realized, realized_close_pnl]
freshness_ms: {mid_event: 1000, mid_receive: 1000, last_upper_bound: 1000,
               mark_event: 1000, mark_receive: 1000}
legacy: {availability_quality: unknown, admission: diagnostic_only,
         sampled_mid_max_age_ms: 15000, sampled_mid_min_horizon_s: 30}
missing: {unknown_gap: invalid, prehistory: abstain, missing_target: null}
split: {kind: chronological_nested_walk_forward, initial_train_days: 14,
        inner_validation_days: 7, outer_days: [4, 5, 5], sealed_days: 7,
        full_dependency_gap_floor_s: 2105, holdout_open: false}
budget: {max_pipeline_configs: 24, seeds_per_config: 3, approved_grant: null}
models_initial: [zero, persistence, train_prior, momentum, reversal, ridge, cart]
cost_assumptions_bp: {taker_per_side: 11, maker_per_side: 4,
                     fee_status: historical_assumption_unverified_account}
stress: {extra_friction_multipliers: [1, 1.5, 2], latency_s: [0.25, 1, 2, 5]}
policy: {initial_notional_usdt: 100, per_symbol_max_positions: 1,
         max_hold_s: 300, pyramiding: false, maker_eligible: false}
authority: {orders: false, live_risk_change: false, runtime_resume: false,
            deployment: false, promotion: false, production_training: false}
```

真实执行配置必须补齐并冻结 `dataset_manifest_sha256, source_code_sha, ordered_feature_hash, label_hash, split_hash, cost_hash, trial_family_id, seed, training_config_hash`，不接受placeholder自动放行。当前样本停在audit_only，即使行数超过某crate默认min_rows也不代表研究合格。

必要工程验证（Cursor后续实施）：

1. 同秒两段合并不丢volume；跨小时/跨午夜保留顺序；真正重复trade ID去重与秒分片合并是两个操作。
2. 在未来追加/修改数据，截止t的所有特征、scaler和决策保持不变；首张报价在t之后必须missing，未收盘桶/ATR不输出。
3. gap、断线ready未重置、乱序/重复sequence、未知availability、hash漂移、错币错venue、缺mark/OI/fee都明确拒绝或target mask，绝不用fixture填市场缺失。
4. 每个h的端点、touch、same-second双触、right censor、late label、funding边界、长context purge进行手算反例测试；未来行不影响归一参数。
5. long/short spread、双边fee、部分成交、无法填满、未成交、延迟、资金费、同秒双触、fold末平仓现金流相符；重叠标签不累加为equity。
6. 重跑同manifest输出同digest；训练/重载/推理一致性由后续模型阶段验证；生成报告不等于训练或回放receipt。

最近有用的检查：`cargo test -p <owning-crate> --locked sec_orderflow`，按实际改动选择 `alpha-domain / alpha-engine / alpha-harness / hft-research-manifest / hft-research-ml`；适用时scoped Clippy与`git diff --check`。本次只有Markdown，不跑Rust编译，不创建只断言文案的测试。

后续报告必须包括：输入source identities、所有资格门状态与原因、每h/每币/每类valid数量、数据时钟与freshness分布、全部trial ledger、预测与成本后收益分开、交易/position ledger、bootstrap与多重检验方法、最大回撤和capacity、sealed是否打开、模型与输出hash。负结果是有效终点；没有合格winner时输出`no_selection`，不能生成部署包。

### 7.5 Evidence identities / 审计身份与交付检查

本轮短核对（2026-09-17 09:06:12 CST）：handoff 模板已读取；本机 HEAD 与前次一致；下列7个小型源文件 SHA-256 全部一致；三币 trades/book 的既有首行字段、`data/l2` 目录存在性已核对。没有重跑32.8h全量统计，也没有把截止后新增数据并入评估。前次覆盖表与数据流清单摘要作为历史审计证据保留，并非本轮新建的 governed dataset。

本文中短路径 `collect-flow.js` 等均相对 `/Users/proerror/tools/marscoin-footprint/`；Monday路径相对 `/Users/proerror/Documents/monday/`。Mac文件需由Monk后续提供经manifest约束的离线样本/制品位置给Cursor Cloud；本文仅提供路径与摘要，不宣称已上传数据。

前次记录且本轮短核对一致的源文件 SHA-256：

| 文件 | SHA-256 |
| --- | --- |
| collect-flow.js | `754d7aeea81753f92d76c4be6c9fa0f90a1b239ffb73ebe2404fdab284ecf31b` |
| collect-l2.js | `323f4a2b633a835a01b5e792f231a6421b60310ce941e71c6e82fc5e224f9f50` |
| atoms.py | `70c627d3cc91e3ec01848f6bf267dafc122d7a4b68eaf0ed50a6200c6ae66e85` |
| signals.py | `19d865c93ce9596b335e17564fc8fcb224d58de08de9cf647cb6cada0f501dad` |
| orderflow-strategy-v2.md | `8269194f80ba3b095aceb55a7c530d7c0edf8045965f76ee3f5909a2d993c566` |
| strategy-v2-triple-gate.md | `4106ca998c9f695a27582f950b32d4c44dbfdb55e6502a60be74e4e7e1774210` |
| directionality-findings.md | `9a9a8f2087ef25730d9d08d81ab60624130497d1f7649a72c8d908be3f7459b6` |

覆盖表的输入摘要：对每个stream的选中文件按路径字典序，逐文件拼接截止前原始完整JSONL行（含换行），计算 `{path: 相对marscoin-footprint路径, rows: 选中行数, sha256: 选中原始行的SHA-256}`；将这份列表按JSON键排序、无空格序列化后再SHA-256。摘要用于本次诊断复核，**不是Monday已admit的dataset manifest**；未来应保存完整file manifest与不可变输入副本。

| Symbol | trades 清单摘要 | book 清单摘要 | l2 清单摘要 |
| --- | --- | --- | --- |
| MARSCOINUSDT | `f3f4551e38fbf599ae1e0f37dc721cf27113784f06f6ff661e8e0e33bdf4492a` | `db29030fec40b29dd1e6d1f249af0fdc3c1a2f5888b75561455bf00ca12206a8` | `4e4a7c6f877af5086aff9bb8a1d7dfd0cd17c8654da961a91efd81ee89e06be2` |
| NIULAIUSDT | `abf2475c55d2494375a4c8ae8ddc8120178bb51d673c99b5c8a5ca22ca7c16a2` | `38335a42a70f1951b59b1a7c0f70cdfe067716cdd410557d469b8394e2c3c283` | `8eaecf818186543be718d0d5dd4ea40b4d7befc3940364ddc7e09cf82202b0c8` |
| HAJIMIUSDT | `0104157cd2fcfa52caf6a55c7ef440190bda745b4f74bde1093c8168d258489b` | `f0f72defdf177a0b503d86a20dc3f9eba020e3c6f24c934ad8ffdcfb503f822d` | `2dea89c26d3c21603448606ff3abd941f7953930b4e8f1af5f5099b0cedb8387` |

本次交付路径：

- `/Users/proerror/Documents/monday/docs/plans/2026-09-17-sec-orderflow-ml-research-packet.md`
- `/Users/proerror/tools/marscoin-footprint/reports/2026-09-17-sec-orderflow-ml-research-packet.md`

验收方式：读取两文件，检查顶部十个packet字段、五个horizon、成本/时间拆分/样本数/多重检验门齐全，并比较完整字节与SHA-256。此研究包完成不表示代码合并、数据治理通过、模型训练完成、策略盈利或任何交易权限变化。
