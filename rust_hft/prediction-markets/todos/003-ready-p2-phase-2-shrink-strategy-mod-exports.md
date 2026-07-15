---
status: ready
priority: p2
issue_id: "003"
tags: [architecture, api-surface, strategy]
dependencies: ["002"]
---

# Phase 2: 收敛 strategy/mod.rs 导出面

将 `strategy` 根模块从“大平铺导出”调整为子域化导出，减少耦合与误用。

## Problem Statement

`strategy/mod.rs` 当前承载过多 `pub use`，把核心抽象、执行实现、策略实现混在同一层，导致导入边界模糊、维护成本高。

## Findings

- 导出面过大，模块职责不清晰。
- 调用方可能绕过预期边界直接依赖实现细节。
- 重构成本可控，Rust 编译器能提供高质量迁移反馈。

## Proposed Solutions

### Option 1: 按子域重组导出（推荐）

**Approach:** 新增/强化分层导出（如 `core`、`execution`、`risk`、`impls`），逐步缩减根导出。

**Pros:**
- 结构清晰
- 降低误用
- 为后续统一架构做准备

**Cons:**
- 需要修改调用方 `use` 路径

**Effort:** 4-8 小时

**Risk:** Medium

---

### Option 2: 保持现状，仅加文档

**Approach:** 不改代码，只补充导入规范。

**Pros:**
- 短期成本低

**Cons:**
- 结构债务继续累积

**Effort:** 1-2 小时

**Risk:** Medium

## Recommended Action

执行 Option 1，采用渐进迁移：先引入子域导出，再分批替换调用方，最后收敛根层导出。

## Technical Details

**主要影响文件：**
- `src/strategy/mod.rs`
- `src/strategy/execution/mod.rs`
- 依赖 `crate::strategy::*` 的调用方

**迁移原则：**
- 先兼容、后收敛（避免一次性破坏）
- 每批次变更后跑构建与关键测试

## Resources

- GitHub Issue: #37 https://github.com/proerror77/ploy/issues/37
- `src/strategy/mod.rs`
- `src/lib.rs` 对外模块暴露

## Acceptance Criteria

- [ ] 根模块导出面显著缩小
- [ ] 子域导出层次清晰且文档化
- [ ] 全量编译通过，调用方导入路径完成迁移
- [ ] 不引入行为变化

## Work Log

### 2026-03-02 - Issue 初始化

**By:** Codex

**Actions:**
- 创建 Phase 2 issue
- 明确导出面收敛目标与迁移策略

**Learnings:**
- 分层导出属于“低风险高收益”的架构优化

## Notes

- 优先保证兼容，再做最终裁剪。
