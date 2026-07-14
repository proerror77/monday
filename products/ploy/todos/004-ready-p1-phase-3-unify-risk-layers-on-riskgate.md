---
status: ready
priority: p1
issue_id: "004"
tags: [risk, architecture, coordinator, strategy]
dependencies: ["002", "003"]
---

# Phase 3: 统一风控层到 RiskGate

收敛多层分散风控，建立统一风险入口与统一观测视图。

## Problem Statement

当前风险逻辑分散在策略级与平台级多个模块，缺乏统一决策与统一指标口径，增加行为不一致风险。

## Findings

- 策略级 `RiskManager` 与平台级 `RiskGate` 职责重叠。
- `ValidationChain` 与执行前检查路径未完全统一。
- 单策略模式与 coordinator 模式风险语义存在潜在分叉。

## Proposed Solutions

### Option 1: 统一到 `RiskGate`（推荐）

**Approach:** 逐步将策略级风险状态、失败计数、日内限制等并入 `RiskGate`，`StrategyEngine` 改为调用统一入口。

**Pros:**
- 风险决策单一来源
- 观测与审计更一致
- 降低策略/平台行为漂移

**Cons:**
- 变更面大，需要分阶段验证

**Effort:** 1-2 周

**Risk:** High

---

### Option 2: 保留双层，仅做桥接

**Approach:** 维持现结构，新增适配层对齐部分指标。

**Pros:**
- 迁移成本较低

**Cons:**
- 架构复杂度仍高
- 长期维护成本大

**Effort:** 3-5 天

**Risk:** Medium

## Recommended Action

执行 Option 1，采用分阶段迁移并以行为对齐测试作为门禁，避免一次性切换。

## Technical Details

**主要影响范围：**
- `src/strategy/risk_mgmt/*`
- `src/platform/risk.rs`
- `src/strategy/execution/engine.rs`
- coordinator 下单与风控编排路径

**迁移要求：**
- 定义统一风险状态模型
- 定义统一错误/阻断原因枚举
- 提供单策略与平台模式共享的风险观测输出

## Resources

- GitHub Issue: #38 https://github.com/proerror77/ploy/issues/38
- `src/strategy/risk_mgmt/risk.rs`
- `src/strategy/risk_mgmt/validation.rs`
- `src/platform/risk.rs`

## Acceptance Criteria

- [ ] `StrategyEngine` 与 coordinator 使用统一风险入口
- [ ] 风险状态、阻断原因、指标口径统一
- [ ] 关键风险场景回归测试通过
- [ ] 旧风险路径完成下线或明确兼容边界

## Work Log

### 2026-03-02 - Issue 初始化

**By:** Codex

**Actions:**
- 创建 Phase 3 issue
- 明确统一风控目标与高风险迁移约束

**Learnings:**
- 风控统一必须强依赖阶段化验证与可回滚策略

## Notes

- Phase 3 是高风险项，建议单独工作流与验收窗口。
