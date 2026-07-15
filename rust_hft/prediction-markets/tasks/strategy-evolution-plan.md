# PM 5-Minute Directional Strategy — Evolution Plan

## 当前状态 (2026-04-11)

### 已完成的修复
- Live 订单精度修复（price round_dp(2), quantity trunc(2), aggressive_ticks=0）
- 订单被拒后触发 cooldown（on_reject 回调）
- 余额不足时暂停 5 分钟（balance_exhausted_until）
- Claimer 退避逻辑（relayer 429 → 30min, no gas → 10min, 默认间隔 60s→300s）
- Live 已停止，等待充值 MATIC 赎回 ~$130

### 数据源清单

| 数据源 | 表名 | 频率 | 状态 |
|--------|------|------|------|
| Binance Spot Price | binance_price_ticks | ~1/秒 | 稳定 |
| Binance LOB 20档 | binance_lob_ticks | 不稳定 | **需修复采集器** |
| Deribit IV | deribit_iv_ticks | ~29/秒 | 稳定，BTC+ETH |
| PM Midpoint Quote | clob_quote_ticks (ploy_runner_live) | 每5秒 | 稳定 |
| PM Orderbook | clob_orderbook_snapshots | ~6/秒 | 稳定 |
| PM Event Metadata | pm_market_metadata | 事件级 | 稳定 |
| PM Settlement | pm_token_settlements | 事件级 | 稳定 |
| Chainlink Price | chainlink_price_ticks | 实时 | 稳定 |
| Binance aggTrades | **不存在** | — | **需要新建采集** |

---

## 策略版本

### V1 — Baseline（旧参数）
- **逻辑**: log-normal 动量模型，EWMA 单一波动率
- **参数**: 6 symbols（含 BTC），max_entry=0.85，window 60-240s
- **回测 (6天)**: 3564 笔，$11,001 盈利，累计资本效率 41%
- **状态**: 仅保留配置用于回测对比，不跑 dry-run

### V2 — Tightened（收窄参数）
- **逻辑**: 同 V1，只改参数
- **参数**: 5 symbols（去 BTC），max_entry=0.55，window 90-180s
- **回测 (6天)**: 1090 笔，$9,748 盈利，累计资本效率 119%
- **状态**: 待部署 dry-run

### V3 — Multi-Vol + Price Structure（多波动率 + 价格结构）
- **逻辑**: V2 参数 + 多波动率估计器 + 贝叶斯价格结构调整
- **新增**:
  - ReturnBuffer: 300s 滚动窗口的 tick 级 log return
  - Realized Variance: sum(r²)/T
  - Parkinson Range: ln(H/L)²/(4·ln2·T)
  - sigma_horizon = max(EWMA, RV, Parkinson)
  - Gate 4: drift speed/acceleration/consistency → odds-ratio 贝叶斯更新
- **回测 (6天)**: 560 笔，$7,832 盈利，累计资本效率 186%
- **状态**: 代码已实现，待部署 dry-run

### V4 — Mean Reversion（均值回归 + 反转检测）— **设计中**
- **核心假设**: 79% 的 5 分钟事件有反转，最佳进场在前 60 秒
- **逻辑**: 价格偏离 S0 后，检测反转信号，买入反方向 token（便宜时买入）
- **进场**: 不是固定时间窗口，而是反转信号触发
- **出场**: 不一定等结算，可以提前 take-profit（需参数优化）
- **反转信号**:
  - 信号 A: drift acceleration 变号（ReturnBuffer，已有）
  - 信号 B: LOB OBI 方向翻转（需稳定 LOB 数据）
  - 信号 C: PM 定价滞后于币安（mispricing 检测）
  - 贝叶斯组合: P(反转) = prior × L(A) × L(B) × L(C)
- **参数优化**: take_profit, stop_loss, max_hold_secs 用 TPE/网格搜索
- **状态**: 设计阶段，等 LOB 采集器修复 + 数据积累

---

## V3 改进方向：本地订单簿趋势检测

V3 目前只用价格动量，没有利用 LOB 的结构信息。改进方向：

### 本地 LOB 摘要（需稳定 LOB 数据后实现）
```
每收到一个 LOB tick:
  更新:
    - 各价位累积量变化（加单/撤单检测）
    - 关键价位检测（大单堆积 = 支撑/阻力）
    - 吸收量（某价位被反复吃掉又补回来 = 有人防守）
    - OBI 变化率（不是水平值，是方向翻转）

趋势确认:
    - S0 上方 ask 被吃掉 → 阻力突破 → 趋势向上
    - S0 下方 bid 堆积 → 支撑形成 → 下跌空间有限
    - bid 快速撤单 → 支撑消失 → 加速下跌
```

### 需要的数据改进
1. **修复 LOB 采集器稳定性**（当前有些天只有几百条）
2. **加 aggTrades 采集**（真正的吃单方向，比 LOB 快照更直接）
3. **LOB 数据积累 1-2 周**后做因子验证

---

## 关键数据发现

### 反转模式 (108 个事件样本)
- 79% 有反转（价格先往一个方向走，然后反转）
- 21% 是纯趋势（一路涨或一路跌）
- 最佳进场时机：50% 在前 60 秒，21% 在 60-120 秒

### 因子预测力
| 因子 | 与 5 分钟方向的相关系数 | 判定 |
|------|----------------------|------|
| 价格动量（当前策略核心） | 最强 | 保留 |
| OBI 水平 | 0.015 | 无效 |
| OBI 变化率 | 0.039 | 极弱，但作为反转确认可能有用 |
| OBI 反向（5000 样本） | -0.21 | 反向指标，需更多数据验证 |
| IV 水平 vs 波动幅度 | 0.21 | 只预测幅度不预测方向 |
| IV Skew | 0.01 | 无效 |

### PM CLOB 流动性
- 真实 spread: 约 2-3 cents（不是 midpoint 合成的 0.5 cents）
- 中途出场成本: 约 2-3%
- V4 提前出场可行，但需要反转幅度 > spread 成本

### 资本占用
- 峰值同时持仓: 6-7 笔
- 峰值资金占用: $150-175
- $500 账户完全够用

---

## 实施路径

### Phase 1: 基础设施（系统重构期间）
- [ ] 修复 LOB 采集器稳定性
- [ ] 新建 aggTrades 采集器
- [ ] 创建 V1/V2/V3 配置文件 + systemd 服务
- [ ] 部署 V2+V3 dry-run 并行运行

### Phase 2: 数据积累（1-2 周）
- [ ] 确认 LOB 数据连续稳定
- [ ] 积累 aggTrades 数据
- [ ] 用新数据验证 OBI 变化率的反转预测力
- [ ] 用新数据验证 aggTrades 的方向预测力

### Phase 3: V3 改进
- [ ] 实现本地 LOB 摘要（关键价位、吸收量、OBI 变化率）
- [ ] 加入 LOB 趋势确认作为贝叶斯似然
- [ ] 回测验证 → dry-run → live

### Phase 4: V4 实现
- [ ] 实现反转检测逻辑（Phase 1+2 的两阶段进场）
- [ ] 实现提前出场逻辑（GTC 限价卖单）
- [ ] 参数优化（take_profit, stop_loss, max_hold_secs）
- [ ] 回测验证 → dry-run → live

### Phase 5: 贝叶斯框架统一
- [ ] 统一 V3（趋势）和 V4（反转）的贝叶斯后验计算
- [ ] 根据市场状态自动切换策略（趋势 vs 反转）
- [ ] 完整的多因子贝叶斯模型

---

## 配置文件清单

```
config/strategies/
  02-pm5d.v1-dryrun.toml    # V1 baseline dry-run
  02-pm5d.v1-live.toml      # V1 baseline live
  02-pm5d.v2-dryrun.toml    # V2 tightened dry-run
  02-pm5d.v2-live.toml      # V2 tightened live
  02-pm5d.v3-dryrun.toml    # V3 multi-vol dry-run
  02-pm5d.v3-live.toml      # V3 multi-vol live
  02-pm5d.v4-dryrun.toml    # V4 mean-reversion dry-run (future)
  02-pm5d.v4-live.toml      # V4 mean-reversion live (future)
```
