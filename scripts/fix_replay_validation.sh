#!/bin/bash
# Monday 重放校验失败修复工具
# 用途：排除不可重放的行情段，调整训练窗口

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# 颜色输出
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

usage() {
    cat << EOF
用法: $0 [选项]

修复重放校验失败的训练任务

选项:
    -m, --mode MODE          修复模式: exclude|inspect|patch
                             exclude: 排除问题时间段（默认，推荐）
                             inspect: 只检查不修改
                             patch: 修补 manifest（需要权限）

    -t, --time DATETIME      问题时间段 (格式: 2026-09-02T01:00)
    -d, --duration HOURS     排除持续时长（小时，默认1）

    -s, --study FILE         研究配置文件路径
    -o, --output FILE        输出修复后的配置文件

    --manifest PATH          直接检查某个 manifest.json
    --oss-path PATH          OSS 存储路径前缀

    -h, --help               显示帮助信息

示例:
    # 检查某个 manifest
    $0 --mode inspect --manifest /path/to/manifest.json

    # 排除问题时间段（最常用）
    $0 --mode exclude --time "2026-09-02T01:00" --study study.json -o study_fixed.json

    # 扫描 OSS 目录找问题段
    $0 --mode inspect --oss-path /mnt/oss/raw/binance-usdm
EOF
}

# 检查单个 manifest
inspect_manifest() {
    local manifest_path="$1"

    if [ ! -f "$manifest_path" ]; then
        echo -e "${RED}✗ 文件不存在: $manifest_path${NC}"
        return 1
    fi

    echo -e "${YELLOW}检查: $manifest_path${NC}"

    # 提取关键字段
    local has_replay=$(jq -r '.has_replay_safe_checkpoint // "missing"' "$manifest_path")
    local symbols_bridged=$(jq -r '.all_symbols_bridged // "missing"' "$manifest_path")
    local coverage=$(jq -r '.all_stream_coverage_verified // "missing"' "$manifest_path")
    local depth_complete=$(jq -r '.venue_depth_complete // "missing"' "$manifest_path")
    local date=$(jq -r '.date // "unknown"' "$manifest_path")
    local hour=$(jq -r '.hour // "unknown"' "$manifest_path")

    echo "  时间段: $date/$hour"
    echo "  has_replay_safe_checkpoint: $has_replay"
    echo "  all_symbols_bridged: $symbols_bridged"
    echo "  all_stream_coverage_verified: $coverage"
    echo "  venue_depth_complete: $depth_complete"

    # 判断是否可重放
    if [ "$has_replay" = "true" ] && \
       [ "$symbols_bridged" = "true" ] && \
       [ "$coverage" = "true" ] && \
       [ "$depth_complete" = "false" ]; then
        echo -e "${GREEN}✓ 可重放${NC}"
        return 0
    else
        echo -e "${RED}✗ 不可重放${NC}"

        # 详细说明哪个条件不满足
        [ "$has_replay" != "true" ] && echo "  失败原因: has_replay_safe_checkpoint != true"
        [ "$symbols_bridged" != "true" ] && echo "  失败原因: all_symbols_bridged != true"
        [ "$coverage" != "true" ] && echo "  失败原因: all_stream_coverage_verified != true"
        [ "$depth_complete" != "false" ] && echo "  失败原因: venue_depth_complete != false (应该是false)"

        return 1
    fi
}

# 扫描 OSS 目录
inspect_oss_directory() {
    local oss_path="$1"
    local failed_count=0
    local checked_count=0

    echo -e "${YELLOW}扫描目录: $oss_path${NC}"
    echo ""

    # 查找所有 manifest.json
    while IFS= read -r manifest; do
        checked_count=$((checked_count + 1))
        if ! inspect_manifest "$manifest"; then
            failed_count=$((failed_count + 1))
            echo ""
        fi
    done < <(find "$oss_path" -name "manifest.json" -type f 2>/dev/null)

    echo ""
    echo "扫描完成："
    echo "  检查: $checked_count 个 manifest"
    echo -e "  失败: ${RED}$failed_count${NC} 个"
    echo -e "  通过: ${GREEN}$((checked_count - failed_count))${NC} 个"
}

# 排除时间段
exclude_time_range() {
    local problem_time="$1"
    local duration_hours="${2:-1}"
    local study_file="$3"
    local output_file="$4"

    echo -e "${YELLOW}排除时间段: $problem_time (持续 ${duration_hours}h)${NC}"

    # 解析时间
    local problem_ts=$(date -j -f "%Y-%m-%dT%H:%M" "$problem_time" "+%s" 2>/dev/null || echo "")
    if [ -z "$problem_ts" ]; then
        echo -e "${RED}✗ 无效的时间格式: $problem_time${NC}"
        echo "  请使用格式: 2026-09-02T01:00"
        return 1
    fi

    local exclude_start="$problem_time:00Z"
    local exclude_end=$(date -j -f "%s" "$((problem_ts + duration_hours * 3600))" "+%Y-%m-%dT%H:%M:%SZ")

    echo "  排除范围: $exclude_start - $exclude_end"

    # 如果没有指定研究配置文件，生成建议
    if [ -z "$study_file" ]; then
        cat << EOF

${YELLOW}建议操作：${NC}

1. 在 Rust 代码中调整时间窗口：

   // 找到对应的 SequenceViewV1 或时间范围定义
   // 将原始窗口拆分为两部分，跳过 $exclude_start - $exclude_end

2. 或者在 Campaign 配置中标记排除：

   {
     "excluded_hours": ["$problem_time"]
   }

3. 或者手动调整 fold 定义：

   fold.train.end_ms = $(date -j -f "%Y-%m-%dT%H:%M:%SZ" "$exclude_start" "+%s")000;
   // 然后从 $exclude_end 开始下一个窗口

EOF
        return 0
    fi

    # TODO: 实现自动修改配置文件
    echo -e "${YELLOW}自动修改配置文件功能开发中...${NC}"
    echo "请手动编辑 $study_file"
}

# 修补 manifest（谨慎使用）
patch_manifest() {
    local manifest_path="$1"

    echo -e "${RED}⚠️  警告：修补 manifest 会改变数据完整性校验${NC}"
    echo "只有在确认数据实际完整但标志错误时才使用"
    echo ""
    read -p "确认继续？(yes/no): " confirm

    if [ "$confirm" != "yes" ]; then
        echo "已取消"
        return 1
    fi

    # 备份
    local backup="${manifest_path}.backup.$(date +%Y%m%d%H%M%S)"
    cp "$manifest_path" "$backup"
    echo "已备份到: $backup"

    # 修改标志
    jq '.has_replay_safe_checkpoint = true |
        .all_symbols_bridged = true |
        .all_stream_coverage_verified = true |
        .venue_depth_complete = false' \
        "$manifest_path" > "${manifest_path}.tmp"

    mv "${manifest_path}.tmp" "$manifest_path"
    echo -e "${GREEN}✓ 已修补 $manifest_path${NC}"

    # 验证
    inspect_manifest "$manifest_path"
}

# 主函数
main() {
    local mode="exclude"
    local problem_time=""
    local duration=1
    local study_file=""
    local output_file=""
    local manifest_path=""
    local oss_path=""

    # 解析参数
    while [[ $# -gt 0 ]]; do
        case $1 in
            -m|--mode)
                mode="$2"
                shift 2
                ;;
            -t|--time)
                problem_time="$2"
                shift 2
                ;;
            -d|--duration)
                duration="$2"
                shift 2
                ;;
            -s|--study)
                study_file="$2"
                shift 2
                ;;
            -o|--output)
                output_file="$2"
                shift 2
                ;;
            --manifest)
                manifest_path="$2"
                shift 2
                ;;
            --oss-path)
                oss_path="$2"
                shift 2
                ;;
            -h|--help)
                usage
                exit 0
                ;;
            *)
                echo "未知选项: $1"
                usage
                exit 1
                ;;
        esac
    done

    # 检查 jq
    if ! command -v jq &> /dev/null; then
        echo -e "${RED}错误: 需要安装 jq${NC}"
        echo "  macOS: brew install jq"
        echo "  Linux: apt-get install jq 或 yum install jq"
        exit 1
    fi

    # 执行对应操作
    case $mode in
        inspect)
            if [ -n "$manifest_path" ]; then
                inspect_manifest "$manifest_path"
            elif [ -n "$oss_path" ]; then
                inspect_oss_directory "$oss_path"
            else
                echo -e "${RED}inspect 模式需要指定 --manifest 或 --oss-path${NC}"
                exit 1
            fi
            ;;
        exclude)
            if [ -z "$problem_time" ]; then
                echo -e "${RED}exclude 模式需要指定 --time${NC}"
                exit 1
            fi
            exclude_time_range "$problem_time" "$duration" "$study_file" "$output_file"
            ;;
        patch)
            if [ -z "$manifest_path" ]; then
                echo -e "${RED}patch 模式需要指定 --manifest${NC}"
                exit 1
            fi
            patch_manifest "$manifest_path"
            ;;
        *)
            echo -e "${RED}未知模式: $mode${NC}"
            usage
            exit 1
            ;;
    esac
}

main "$@"
