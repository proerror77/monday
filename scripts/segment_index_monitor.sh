#!/bin/bash
# Segment Index Health Check and Monitoring
#
# 用途：
# 1. 定期检查索引健康状况
# 2. 生成监控报告
# 3. 发送告警（如果需要）
#
# 用法：
#   ./segment_index_monitor.sh [options]
#
# 选项：
#   --index PATH          索引文件路径（默认：/lake/output/metadata/segments.parquet）
#   --alert-threshold N   告警阈值：安全率低于 N% 时告警（默认：95）
#   --output-dir PATH     报告输出目录（默认：/tmp）
#   --json                输出 JSON 格式（用于集成监控系统）

set -e

# 默认配置
INDEX_PATH="${SEGMENT_INDEX_PATH:-/lake/output/metadata/segments.parquet}"
ALERT_THRESHOLD=95
OUTPUT_DIR="/tmp"
OUTPUT_JSON=false

# 解析参数
while [[ $# -gt 0 ]]; do
    case $1 in
        --index)
            INDEX_PATH="$2"
            shift 2
            ;;
        --alert-threshold)
            ALERT_THRESHOLD="$2"
            shift 2
            ;;
        --output-dir)
            OUTPUT_DIR="$2"
            shift 2
            ;;
        --json)
            OUTPUT_JSON=true
            shift
            ;;
        *)
            echo "Unknown option: $1"
            exit 1
            ;;
    esac
done

# 检查依赖
if ! command -v duckdb &> /dev/null; then
    echo "Error: duckdb not found"
    exit 1
fi

if [ ! -f "$INDEX_PATH" ]; then
    echo "Error: Index file not found: $INDEX_PATH"
    exit 1
fi

# 生成时间戳
TIMESTAMP=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
REPORT_FILE="${OUTPUT_DIR}/segment_index_health_${TIMESTAMP}.txt"

# 初始化报告
if [ "$OUTPUT_JSON" = false ]; then
    cat > "$REPORT_FILE" << EOF
=================================================
Segment Index Health Report
=================================================
Generated: $TIMESTAMP
Index: $INDEX_PATH

EOF
fi

# 1. 基本统计
echo "Collecting basic statistics..." >&2

STATS=$(duckdb -json -c "
SELECT
    COUNT(*) as total_segments,
    COUNT(DISTINCT symbol) as unique_symbols,
    COUNT(DISTINCT date) as unique_dates,
    MIN(date) as first_date,
    MAX(date) as last_date,
    SUM(verified_bytes) / 1024 / 1024 / 1024.0 as total_gb,
    SUM(CASE WHEN replay_safe THEN 1 ELSE 0 END) as safe_segments,
    ROUND(100.0 * SUM(CASE WHEN replay_safe THEN 1 ELSE 0 END) / COUNT(*), 2) as safe_percentage
FROM read_parquet('$INDEX_PATH')
")

TOTAL=$(echo "$STATS" | jq -r '.[0].total_segments')
SAFE=$(echo "$STATS" | jq -r '.[0].safe_segments')
SAFE_PCT=$(echo "$STATS" | jq -r '.[0].safe_percentage')

if [ "$OUTPUT_JSON" = false ]; then
    cat >> "$REPORT_FILE" << EOF
## Basic Statistics

Total segments: $TOTAL
Safe segments: $SAFE
Safety rate: $SAFE_PCT%
Unique symbols: $(echo "$STATS" | jq -r '.[0].unique_symbols')
Date range: $(echo "$STATS" | jq -r '.[0].first_date') to $(echo "$STATS" | jq -r '.[0].last_date')
Total data: $(echo "$STATS" | jq -r '.[0].total_gb') GB

EOF
fi

# 2. 每日安全率
echo "Analyzing daily safety rates..." >&2

DAILY_SAFETY=$(duckdb -json -c "
SELECT
    date,
    COUNT(*) as total,
    SUM(CASE WHEN replay_safe THEN 1 ELSE 0 END) as safe,
    ROUND(100.0 * SUM(CASE WHEN replay_safe THEN 1 ELSE 0 END) / COUNT(*), 2) as safe_pct
FROM read_parquet('$INDEX_PATH')
GROUP BY date
ORDER BY date DESC
LIMIT 30
")

if [ "$OUTPUT_JSON" = false ]; then
    echo "## Daily Safety Rates (Last 30 Days)" >> "$REPORT_FILE"
    echo "" >> "$REPORT_FILE"
    echo "$DAILY_SAFETY" | jq -r '.[] | "\(.date)  \(.total) segments  \(.safe) safe  \(.safe_pct)%"' >> "$REPORT_FILE"
    echo "" >> "$REPORT_FILE"
fi

# 3. 检查问题段
echo "Checking for problematic segments..." >&2

PROBLEMS=$(duckdb -json -c "
SELECT
    date,
    hour,
    symbol,
    CASE
        WHEN NOT has_replay_safe_checkpoint THEN 'missing_checkpoint'
        WHEN NOT all_symbols_bridged THEN 'not_bridged'
        WHEN NOT all_stream_coverage_verified THEN 'coverage_unverified'
        WHEN venue_depth_complete THEN 'wrong_depth_flag'
        ELSE 'unknown'
    END as issue
FROM read_parquet('$INDEX_PATH')
WHERE replay_safe = FALSE
  AND date >= CURRENT_DATE - INTERVAL 7 DAYS
ORDER BY date DESC, hour DESC
LIMIT 100
")

PROBLEM_COUNT=$(echo "$PROBLEMS" | jq 'length')

if [ "$OUTPUT_JSON" = false ]; then
    cat >> "$REPORT_FILE" << EOF
## Problematic Segments (Last 7 Days)

Found $PROBLEM_COUNT unsafe segments.

EOF

    if [ "$PROBLEM_COUNT" -gt 0 ]; then
        echo "$PROBLEMS" | jq -r '.[] | "\(.date) \(.hour):00  \(.symbol)  \(.issue)"' | head -20 >> "$REPORT_FILE"
        if [ "$PROBLEM_COUNT" -gt 20 ]; then
            echo "... and $((PROBLEM_COUNT - 20)) more" >> "$REPORT_FILE"
        fi
    fi
    echo "" >> "$REPORT_FILE"
fi

# 4. 索引增长趋势
echo "Analyzing growth trends..." >&2

RECENT_INGESTION=$(duckdb -json -c "
SELECT
    DATE_TRUNC('day', TO_TIMESTAMP(ingested_at_ms / 1000)) as ingestion_day,
    COUNT(*) as segments_added
FROM read_parquet('$INDEX_PATH')
WHERE ingested_at_ms > EXTRACT(EPOCH FROM CURRENT_TIMESTAMP - INTERVAL 7 DAYS) * 1000
GROUP BY ingestion_day
ORDER BY ingestion_day DESC
")

if [ "$OUTPUT_JSON" = false ]; then
    echo "## Index Growth (Last 7 Days)" >> "$REPORT_FILE"
    echo "" >> "$REPORT_FILE"
    echo "$RECENT_INGESTION" | jq -r '.[] | "\(.ingestion_day)  +\(.segments_added) segments"' >> "$REPORT_FILE"
    echo "" >> "$REPORT_FILE"
fi

# 5. 生成健康状态
HEALTH_STATUS="HEALTHY"
ALERTS=()

# 检查安全率
if (( $(echo "$SAFE_PCT < $ALERT_THRESHOLD" | bc -l) )); then
    HEALTH_STATUS="WARNING"
    ALERTS+=("Safety rate ($SAFE_PCT%) below threshold ($ALERT_THRESHOLD%)")
fi

# 检查最近的问题段
RECENT_PROBLEMS=$(duckdb -c "
SELECT COUNT(*)
FROM read_parquet('$INDEX_PATH')
WHERE replay_safe = FALSE
  AND date >= CURRENT_DATE - INTERVAL 1 DAYS
" 2>/dev/null || echo "0")

if [ "$RECENT_PROBLEMS" -gt 10 ]; then
    HEALTH_STATUS="WARNING"
    ALERTS+=("$RECENT_PROBLEMS problematic segments in the last 24 hours")
fi

# 检查索引是否在更新
LATEST_INGESTION=$(duckdb -c "
SELECT MAX(ingested_at_ms) / 1000
FROM read_parquet('$INDEX_PATH')
" 2>/dev/null || echo "0")

CURRENT_TIME=$(date +%s)
TIME_DIFF=$((CURRENT_TIME - LATEST_INGESTION))

if [ "$TIME_DIFF" -gt 7200 ]; then  # 2 hours
    HEALTH_STATUS="WARNING"
    ALERTS+=("No new segments ingested in the last 2 hours")
fi

# 输出结果
if [ "$OUTPUT_JSON" = true ]; then
    # JSON 输出（用于监控系统集成）
    cat << EOF
{
  "timestamp": "$TIMESTAMP",
  "index_path": "$INDEX_PATH",
  "health_status": "$HEALTH_STATUS",
  "statistics": $(echo "$STATS" | jq '.[0]'),
  "alerts": $(printf '%s\n' "${ALERTS[@]}" | jq -R . | jq -s .),
  "problem_count": $PROBLEM_COUNT,
  "recent_problems": $RECENT_PROBLEMS,
  "daily_safety": $DAILY_SAFETY
}
EOF
else
    # 文本报告
    cat >> "$REPORT_FILE" << EOF
## Health Status

Overall: $HEALTH_STATUS

EOF

    if [ ${#ALERTS[@]} -gt 0 ]; then
        echo "### Alerts" >> "$REPORT_FILE"
        for alert in "${ALERTS[@]}"; do
            echo "⚠️  $alert" >> "$REPORT_FILE"
        done
        echo "" >> "$REPORT_FILE"
    fi

    cat >> "$REPORT_FILE" << EOF
## Recommendations

EOF

    if (( $(echo "$SAFE_PCT < 95" | bc -l) )); then
        cat >> "$REPORT_FILE" << EOF
1. Investigate problematic segments (see list above)
2. Check collector logs for data quality issues
3. Consider re-collecting affected time periods
EOF
    fi

    if [ "$RECENT_PROBLEMS" -gt 10 ]; then
        cat >> "$REPORT_FILE" << EOF
4. Recent spike in problematic segments detected
5. Review collector configuration and network stability
EOF
    fi

    if [ "$TIME_DIFF" -gt 7200 ]; then
        cat >> "$REPORT_FILE" << EOF
6. Index appears stale - verify collector is running
7. Check SEGMENT_INDEX_PATH environment variable
EOF
    fi

    if [ ${#ALERTS[@]} -eq 0 ]; then
        echo "✓ No issues detected. Index is healthy." >> "$REPORT_FILE"
    fi

    cat >> "$REPORT_FILE" << EOF

=================================================
End of Report
=================================================
EOF

    # 显示报告
    cat "$REPORT_FILE"
    echo ""
    echo "Report saved to: $REPORT_FILE"
fi

# 返回状态码
if [ "$HEALTH_STATUS" = "WARNING" ]; then
    exit 1
else
    exit 0
fi
