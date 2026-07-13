---
status: ready
priority: p1
issue_id: "001"
tags: [architecture, cleanup, dead-code]
dependencies: []
---

# Phase 0: 清理死代码与禁用路径

删除已死代码与已禁用运行路径，降低认知负担并收紧生产主链路。

## Problem Statement

当前仓库同时保留多条历史路径（部分未被调用、部分已硬禁 live），增加维护成本和误用风险。

## Findings

- `src/strategy/orchestrator.rs` 在当前代码库无实例化与调用。
- `OrderPlatform` 路径已明确禁止 live runtime（提示使用 coordinator runtime）。
- 旧抽象残留导致阅读时难以快速识别“真实生产路径”。

## Proposed Solutions

### Option 1: 直接删除死代码（推荐）

**Approach:** 删除确认无引用模块与对应导出，编译器兜底校验全局无残留调用。

**Pros:**
- 立即降低复杂度
- 风险最小、改动可审计

**Cons:**
- 若存在隐式依赖，需要快速回滚

**Effort:** 2-4 小时

**Risk:** Low

---

### Option 2: 保留代码，仅标记 deprecated

**Approach:** 先加 `deprecated` 与文档说明，延后删除。

**Pros:**
- 过渡平滑

**Cons:**
- 复杂度不下降
- 误用风险仍在

**Effort:** 1-2 小时

**Risk:** Medium

## Recommended Action

执行 Option 1。分小步提交删除无引用模块与导出；每步运行 `cargo build` + 关键测试，确保行为无变化。

## Technical Details

**候选清理范围（以最终引用检查为准）：**
- `src/strategy/orchestrator.rs`
- `src/platform/platform.rs`（如确认仅 legacy 且无生产引用）
- `platform` 侧未使用的旧 agent trait/实现

**约束：**
- 不改生产行为
- 不引入接口破坏给当前 coordinator 路径

## Resources

- GitHub Issue: #35 https://github.com/proerror77/ploy/issues/35
- 架构审查结论（本轮对话）
- `src/main_modes/platform_mode.rs`
- `src/coordinator/bootstrap/*`

## Acceptance Criteria

- [ ] 删除目标死代码并通过 `cargo build`
- [ ] 生产主路径（coordinator）编译与启动流程不变
- [ ] `pub use` 与模块导出无悬挂引用
- [ ] 变更说明补充到文档/issue 记录

## Work Log

### 2026-03-02 - Issue 初始化

**By:** Codex

**Actions:**
- 基于 Phase 迁移计划创建 issue
- 明确 Phase 0 目标、范围、验收标准

**Learnings:**
- 先清理死代码可以显著降低后续 Phase 的改动风险

## Notes

- 本 issue 聚焦“减法”；不做功能增强。
