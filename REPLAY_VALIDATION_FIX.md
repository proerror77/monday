# 重放校验失败修复方案

## 问题诊断

**错误**: "source segment is not a fully replayable market-tape segment"  
**位置**: `rust_hft/tools/collector/src/research_inventory.rs:175`  
**阶段**: 数据准备阶段（约57分钟后，目录扫描完成，开始校验行情段）

## 根本原因

某个行情段的 `manifest.json` 不满足以下**全部**重放安全条件：

```rust
// 必须同时满足这4个条件
has_replay_safe_checkpoint == true       // 有安全检查点
all_symbols_bridged == true              // 所有交易对已桥接
all_stream_coverage_verified == true     // 流覆盖已验证
venue_depth_complete == false            // 场所深度不完整（这是正确状态）
```

任何一个不满足都会触发校验失败。

---

## 🔍 诊断步骤

### 1. 定位问题行情段

使用诊断脚本扫描 OSS 存储中的所有 manifest：

```bash
# 如果可以挂载 OSS 到本地
/tmp/diagnose_replay_manifest.sh /path/to/mounted/oss/raw

# 或者先下载一部分 manifest 检查
mkdir -p /tmp/manifests
# 下载目标时间窗口的 manifest 文件
# 然后运行诊断
/tmp/diagnose_replay_manifest.sh /tmp/manifests
```

### 2. 手动检查单个文件

```bash
# 检查某个具体的 manifest
manifest_path="/path/to/manifest.json"

echo "=== 重放校验结果 ==="
jq '{
  has_replay_safe_checkpoint,
  all_symbols_bridged,
  all_stream_coverage_verified,
  venue_depth_complete,
  date,
  hour,
  symbols
}' "$manifest_path"

# 快速判断
if jq -e '
  .has_replay_safe_checkpoint == true and
  .all_symbols_bridged == true and
  .all_stream_coverage_verified == true and
  .venue_depth_complete == false
' "$manifest_path" > /dev/null 2>&1; then
  echo "✅ 可重放"
else
  echo "❌ 不可重放"
fi
```

---

## 🛠️ 修复方案

### 方案 A：排除问题行情段（推荐）

**适用场景**: 只有少数几个小时的行情段有问题

#### 步骤：

1. **识别问题时间段**
   ```bash
   # 从诊断输出中找到问题行情段的 date/hour
   # 例如: 2025-01-15/13
   ```

2. **调整训练窗口**
   
   修改研究配置，跳过问题时间段：
   
   ```rust
   // 在 sequence_study 或 market_encoder_study 配置中
   // 调整 SequenceViewV1 的时间边界
   
   // 原始窗口: 2025-01-14 00:00 - 2025-01-17 23:59
   // 如果 2025-01-15/13 有问题
   
   // 方案1: 拆分为两个窗口
   // 窗口1: 2025-01-14 00:00 - 2025-01-15 12:59
   // 窗口2: 2025-01-15 14:00 - 2025-01-17 23:59
   
   // 方案2: 直接跳过该天
   // 窗口: 2025-01-14 00:00 - 2025-01-14 23:59 + 2025-01-16 00:00 - 2025-01-17 23:59
   ```

3. **更新 Campaign 配置**
   ```bash
   # 在 Monday 中重新生成训练请求
   monday campaign finalize --study <study_id> \
     --exclude-hours "2025-01-15T13"
   ```

---

### 方案 B：修复行情段元数据（需要权限）

**适用场景**: 问题广泛，或确认数据本身是完整的但标志错误

#### 选项 1: 重新采集

如果有实时采集能力，可以重新采集该时间段：

```bash
# 重启 binance-lob-archiver 补录
monday collector replay \
  --start "2025-01-15T13:00:00Z" \
  --end "2025-01-15T13:59:59Z" \
  --symbols SOLUSDT
```

#### 选项 2: 修补 manifest（谨慎）

**⚠️ 警告**: 仅在确认数据实际完整时使用

```bash
#!/bin/bash
# 修补单个 manifest
manifest="path/to/manifest.json"

# 备份原文件
cp "$manifest" "${manifest}.backup"

# 修改标志
jq '.has_replay_safe_checkpoint = true |
    .all_symbols_bridged = true |
    .all_stream_coverage_verified = true |
    .venue_depth_complete = false' \
  "$manifest" > "${manifest}.fixed"

# 验证修改
diff "${manifest}.backup" "${manifest}.fixed"

# 确认后替换
mv "${manifest}.fixed" "$manifest"
```

**必须验证的前提条件**:
- 数据文件实际存在且完整
- 没有序列缺口（sequence gap）
- 所有交易对确实有数据
- 流类型覆盖完整（depth@100ms, bookTicker）

---

### 方案 C：放宽校验条件（不推荐）

**仅用于调试，不可用于生产**

修改 `research_inventory.rs` 临时降低标准：

```rust
// 在 rust_hft/tools/collector/src/research_inventory.rs
// 临时注释严格校验（仅用于定位其他问题）

let flags_ok = manifest
    .get("has_replay_safe_checkpoint")
    .and_then(Value::as_bool)
    .unwrap_or(false)  // 改为 true 会跳过检查（危险）
    // ... 其他条件
```

**⚠️ 这会破坏研究完整性，仅用于诊断！**

---

## ✅ 推荐执行流程

基于当前情况（首小时准备失败），建议：

### 立即行动：

1. **定位问题小时**
   ```bash
   # Luna 应该已经保存了失败日志
   # 从日志中提取失败的具体 manifest 路径
   # 或者扫描请求窗口内的所有 manifest
   ```

2. **评估影响范围**
   ```bash
   # 如果只有1-2个小时有问题 → 方案A（排除）
   # 如果>10%的小时有问题 → 需要重新评估数据源质量
   ```

3. **调整并重试**
   ```bash
   # 更新训练窗口配置，排除问题时间段
   # 重新提交 Campaign
   ```

### 长期改进：

1. **预检查流程**
   
   在 Campaign 提交前运行预检：
   ```bash
   monday data validate \
     --start <start_time> \
     --end <end_time> \
     --check-replay-flags
   ```

2. **采集质量监控**
   
   监控 collector 输出的 `replay_safe` 标志：
   ```bash
   # 定期检查最近采集的数据
   monday collector status --check-replay-safety
   ```

3. **自动修复管线**
   
   对于已知的暂时性问题（如临时连接中断），建立补录机制。

---

## 📋 检查清单

在重新启动训练前确认：

- [ ] 已定位具体的问题行情段（date/hour）
- [ ] 已确认问题原因（缺失标志 vs 实际数据问题）
- [ ] 已更新训练时间窗口配置（如使用方案A）
- [ ] 配置更新已通过 `validate()` 检查
- [ ] 累计预算和截止时间未变
- [ ] 保留了原失败记录用于审计

---

## 🔗 相关代码位置

- 校验逻辑: `rust_hft/tools/collector/src/research_inventory.rs:161-176`
- Manifest 生成: `rust_hft/tools/collector/src/bin/binance-lob-archiver.rs`
- 重放标志设置: `segment.mark_replay_unsafe()` 调用点
- Campaign 准备入口: `rust_hft/alpha-harness/app/src/mission_campaign/sequence/worker.rs`

---

## 💡 下一步建议

**立即执行**:
1. 让 Luna 读取失败任务的完整错误日志，提取失败的具体 manifest 路径
2. 下载该 manifest 检查哪个标志位失败
3. 决定使用方案 A（排除）还是方案 B（修复）

**如果需要快速恢复**:
- 使用方案 A 排除 1-2 个问题小时
- 保持其他配置不变
- 预计可在 1 小时内重新启动

**如果需要彻底修复**:
- 诊断数据源问题（采集器配置、网络稳定性）
- 可能需要重新采集部分数据
- 预计需要 1-3 天
