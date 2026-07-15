---
status: ready
priority: p1
issue_id: "002"
tags: [execution, datastore, optimistic-locking]
dependencies: ["001"]
---

# Phase 1: EngineStore 版本冲突改为显式错误类型

将 `Result<bool>` 的乐观锁结果语义升级为编译器可强制处理的错误模型。

## Problem Statement

当前 `EngineStore.update_cycle_*` 以 `Ok(false)` 表示版本冲突。该设计容易被调用方遗漏处理，导致“更新失败但流程继续”。

## Findings

- 版本冲突是核心一致性事件，不应作为普通布尔值返回。
- 新调用方若仅处理 `Err`，会漏掉 `Ok(false)`。
- 需要编译器层面的强约束，避免后续回归。

## Proposed Solutions

### Option 1: 引入 `VersionConflict` 显式错误（推荐）

**Approach:** 将 `update_cycle_*` 返回改为 `Result<(), StoreError>`，版本冲突返回 `Err(StoreError::VersionConflict { ... })`。

**Pros:**
- 编译期强约束
- 语义清晰
- 调用方处理路径统一

**Cons:**
- 需要批量改签名与调用点

**Effort:** 4-8 小时

**Risk:** Medium

---

### Option 2: 保留 `bool`，加 lint/包装器

**Approach:** 保持原签名，在调用侧统一 helper 强制检查。

**Pros:**
- 改动面较小

**Cons:**
- 约束仍靠约定，不够刚性

**Effort:** 2-4 小时

**Risk:** Medium

## Recommended Action

执行 Option 1。按“接口 -> 适配器 -> 调用方 -> 测试”顺序迁移，保证每一步可编译。

## Technical Details

**主要影响文件：**
- `src/strategy/execution/engine_store.rs`
- `src/adapters/postgres.rs`
- `src/strategy/execution/engine.rs`
- `src/error.rs`（或新增 store error 模块）

**迁移要求：**
- 所有版本冲突统一走 `abort + halt` 安全分支
- MockStore 与单测语义同步更新

## Resources

- GitHub Issue: #36 https://github.com/proerror77/ploy/issues/36
- `migrations/005_idempotency_and_security.sql`
- `src/strategy/execution/engine.rs`
- `src/adapters/postgres.rs`

## Acceptance Criteria

- [ ] `update_cycle_*` 不再返回 `Result<bool>`
- [ ] 版本冲突统一为显式错误类型
- [ ] 所有调用方显式处理版本冲突
- [ ] engine 与 store 相关测试通过

## Work Log

### 2026-03-02 - Issue 初始化

**By:** Codex

**Actions:**
- 将 Phase 1 拆解为可执行 issue
- 明确错误模型目标与迁移路径

**Learnings:**
- 一致性问题最稳妥的修复是类型系统强约束

## Notes

- 本 issue 可先于 Phase 3 完成，是后续风控整合的基础。
