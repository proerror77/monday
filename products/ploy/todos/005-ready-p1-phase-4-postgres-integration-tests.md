---
status: ready
priority: p1
issue_id: "005"
tags: [testing, postgres, integration, consistency]
dependencies: ["002"]
---

# Phase 4: 增加真实 Postgres 集成测试

为 cycle 乐观锁、并发冲突、恢复路径建立真实数据库回归测试。

## Problem Statement

当前执行引擎测试主要依赖 MockStore，无法覆盖真实 Postgres 下的版本冲突与并发行为。

## Findings

- 单元测试可验证逻辑分支，但无法替代真实事务/锁语义。
- 版本冲突与恢复路径属于高风险一致性场景，必须有集成测试兜底。
- CI 若无数据库测试门禁，后续改动易引入回归。

## Proposed Solutions

### Option 1: 使用临时 Postgres（推荐）

**Approach:** 使用 testcontainers 或等效机制启动临时 Postgres，执行 `engine_store` / `engine` 集成测试。

**Pros:**
- 结果贴近生产
- 能覆盖并发竞争与恢复路径

**Cons:**
- 测试运行时间增加
- CI 环境配置更复杂

**Effort:** 1-2 天

**Risk:** Medium

---

### Option 2: 仅强化 MockStore

**Approach:** 继续扩展 Mock 行为模拟冲突场景。

**Pros:**
- 实施快

**Cons:**
- 仍与真实数据库语义存在差距

**Effort:** 0.5-1 天

**Risk:** Medium

## Recommended Action

执行 Option 1。先落地最小关键场景，再逐步扩展到恢复与故障注入。

## Technical Details

**建议测试场景：**
- 正常 Leg1 -> Leg2 完成路径
- `update_cycle_state` 版本冲突
- `update_cycle_leg1` / `update_cycle_leg2` 版本冲突
- 并发更新竞争（一个成功、一个冲突）
- 冲突后 `abort + halt` 恢复路径

**建议位置：**
- `tests/integration/engine_store_pg.rs`
- `tests/integration/engine_conflict_pg.rs`

## Resources

- GitHub Issue: #39 https://github.com/proerror77/ploy/issues/39
- `src/adapters/postgres.rs`
- `src/strategy/execution/engine_store.rs`
- `migrations/005_idempotency_and_security.sql`

## Acceptance Criteria

- [ ] 至少覆盖 5 个关键 Postgres 一致性场景
- [ ] 本地与 CI 可重复执行
- [ ] 失败时输出足够诊断信息（期望版本/实际版本）
- [ ] 与 Mock 单测形成互补，不重复

## Work Log

### 2026-03-02 - Issue 初始化

**By:** Codex

**Actions:**
- 创建 Phase 4 issue
- 明确集成测试范围与优先场景

**Learnings:**
- 一致性回归要依赖真实数据库语义验证

## Notes

- 建议先在 nightly/optional job 启用，再提升为必过门禁。
