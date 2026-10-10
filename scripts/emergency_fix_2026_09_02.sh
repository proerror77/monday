#!/bin/bash
# 立即修复：排除 2026-09-02T01:00 问题时间段
# 适用于当前失败的 SOL 序列研究或市场编码器研究

set -e

echo "=== Monday 研究任务紧急修复 ==="
echo "问题：2026-09-02T01:00 行情段重放校验失败"
echo "原因：该段在关机收尾时被标记为 replay_unsafe"
echo ""

# 问题时间段
PROBLEM_TIME="2026-09-02T01:00:00Z"
PROBLEM_DATE="2026-09-02"
PROBLEM_HOUR=1

echo "🔍 步骤 1: 诊断问题段"
echo ""
echo "如果可以访问 OSS，运行以下命令检查："
echo ""
cat << 'EOF'
# 下载并检查该 manifest
manifest_path="/lake/raw/venue=binance_usdm/date=2026-09-02/hour=01/manifest.json"

# 使用 ossutil 或 kubectl
ossutil cp oss://monday-research${manifest_path} /tmp/problem_manifest.json
jq '{
  date, hour, symbols,
  has_replay_safe_checkpoint,
  all_symbols_bridged,
  all_stream_coverage_verified,
  venue_depth_complete,
  replay_safe: (
    .has_replay_safe_checkpoint == true and
    .all_symbols_bridged == true and
    .all_stream_coverage_verified == true and
    .venue_depth_complete == false
  )
}' /tmp/problem_manifest.json
EOF

echo ""
echo "---"
echo ""
echo "🛠️ 步骤 2: 修复选项"
echo ""
echo "选项 A：排除该小时（推荐，最快）"
echo "  - 调整训练窗口，跳过 2026-09-02T01:00"
echo "  - 保持其他配置不变"
echo "  - 预计 1 小时内恢复"
echo ""
echo "选项 B：修复该段数据"
echo "  - 如果数据实际完整，修补 manifest 标志"
echo "  - 或者重新采集该小时"
echo "  - 需要 1-3 天"
echo ""

read -p "选择修复方案 (A/B): " choice

case $choice in
  [Aa]*)
    echo ""
    echo "✅ 使用选项 A：排除问题时间段"
    echo ""

    echo "=== 针对 SequenceStudyV1 的修改 ==="
    echo ""
    cat << 'EOF'
// 在 rust_hft/alpha-harness/domain/src/sequence_study.rs
// 或者在生成 study 配置的代码中

// 原始时间窗口（假设）
// 2026-09-01T00:00:00Z - 2026-09-03T23:59:59Z

// 修改为排除 2026-09-02T01:00 的窗口
// 方法1：缩短窗口，在问题时间前结束
fold.train.end_ms = 1725235199000;  // 2026-09-02T00:59:59Z

// 方法2：从问题时间后开始
fold.train.history_start_ms = 1725238800000;  // 2026-09-02T02:00:00Z

// 方法3：拆分为两个独立窗口
// 窗口1: 2026-09-01T00:00 - 2026-09-02T00:59
// 窗口2: 2026-09-02T02:00 - 2026-09-03T23:59
EOF

    echo ""
    echo "=== 针对 MarketEncoderStudyV1 的修改 ==="
    echo ""
    cat << 'EOF'
// 在 rust_hft/alpha-harness/domain/src/market_encoder_study.rs

// 调整 fold 的训练数据范围
pub fn adjust_fold_for_unsafe_segment(fold: &mut MarketEncoderFoldV1) {
    // 如果训练窗口包含 2026-09-02T01:00
    let problem_start_ms = 1725235200000;  // 2026-09-02T01:00:00
    let problem_end_ms = 1725238799999;    // 2026-09-02T01:59:59

    if fold.train.view.start_ms <= problem_start_ms
       && fold.train.view.end_ms >= problem_end_ms {
        // 缩短训练窗口到问题时间前
        fold.train.view.end_ms = problem_start_ms - 1;

        println!("⚠️  Adjusted fold to exclude unsafe segment 2026-09-02T01:00");
    }
}
EOF

    echo ""
    echo "=== 通用 SQL 排除逻辑（如果使用 DuckDB/ClickHouse）==="
    echo ""
    cat << 'EOF'
-- 查询时自动排除
SELECT * FROM market_segments
WHERE symbol = 'SOLUSDT'
  AND start_ns <= ?
  AND end_ns >= ?
  AND replay_safe = TRUE
  AND NOT (date = '2026-09-02' AND hour = 1);  -- 排除问题小时
EOF

    echo ""
    echo "=== 快速验证脚本 ==="
    echo ""
    cat << 'VERIFY'
#!/bin/bash
# 验证调整后的时间窗口

start_ms=1725148800000  # 调整后的开始时间
end_ms=1725321599000    # 调整后的结束时间

problem_start=1725235200000
problem_end=1725238799999

if [ $start_ms -ge $problem_start ] && [ $start_ms -le $problem_end ]; then
    echo "❌ 错误：开始时间仍在问题区间内"
    exit 1
fi

if [ $end_ms -ge $problem_start ] && [ $end_ms -le $problem_end ]; then
    echo "❌ 错误：结束时间仍在问题区间内"
    exit 1
fi

if [ $start_ms -lt $problem_start ] && [ $end_ms -gt $problem_end ]; then
    echo "❌ 错误：时间窗口包含问题区间"
    exit 1
fi

echo "✅ 时间窗口已正确排除问题时段"
VERIFY

    echo ""
    echo "---"
    echo ""
    echo "📋 下一步操作："
    echo "1. 根据你的研究类型（SequenceStudy 或 MarketEncoder）应用上述修改"
    echo "2. 重新生成 Campaign 配置"
    echo "3. 提交到 ACK 集群"
    echo "4. 验证准备阶段能顺利通过"
    echo ""
    echo "预计恢复时间：1-2 小时"
    ;;

  [Bb]*)
    echo ""
    echo "⚠️ 使用选项 B：修复该段数据"
    echo ""
    echo "=== 修补 Manifest 脚本 ==="
    echo ""
    cat << 'EOF'
#!/bin/bash
# 谨慎使用：仅在确认数据实际完整时使用

manifest_path="/tmp/problem_manifest.json"

# 1. 下载 manifest
ossutil cp oss://monday-research/lake/raw/venue=binance_usdm/date=2026-09-02/hour=01/manifest.json $manifest_path

# 2. 备份
cp $manifest_path ${manifest_path}.backup

# 3. 修改标志
jq '.has_replay_safe_checkpoint = true |
    .all_symbols_bridged = true |
    .all_stream_coverage_verified = true |
    .venue_depth_complete = false' \
  $manifest_path > ${manifest_path}.fixed

# 4. 验证
echo "修改前："
jq '{has_replay_safe_checkpoint, all_symbols_bridged, all_stream_coverage_verified, venue_depth_complete}' ${manifest_path}.backup

echo "修改后："
jq '{has_replay_safe_checkpoint, all_symbols_bridged, all_stream_coverage_verified, venue_depth_complete}' ${manifest_path}.fixed

# 5. 确认并上传
read -p "确认上传修改后的 manifest？(yes/no): " confirm
if [ "$confirm" = "yes" ]; then
    ossutil cp ${manifest_path}.fixed oss://monday-research/lake/raw/venue=binance_usdm/date=2026-09-02/hour=01/manifest.json
    echo "✅ 已上传修改后的 manifest"
else
    echo "❌ 已取消"
fi
EOF

    echo ""
    echo "⚠️ 警告："
    echo "- 修补标志会影响数据完整性校验"
    echo "- 只有在确认数据实际完整但标志错误时使用"
    echo "- 建议先联系数据团队确认"
    echo ""
    echo "预计时间：半天（如果数据确实完整）"
    ;;

  *)
    echo "无效选择"
    exit 1
    ;;
esac

echo ""
echo "=== 完成 ==="
