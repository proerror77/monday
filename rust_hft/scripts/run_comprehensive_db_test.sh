#!/bin/bash

# 20+商品 × 15分钟完整数据库压力测试运行脚本
# 
# 功能：
# - 自动检查ClickHouse服务状态
# - 编译优化版本
# - 运行完整压力测试
# - 生成性能报告
# - 提供优化建议

set -e  # 遇到错误立即退出

# 颜色定义
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
PURPLE='\033[0;35m'
CYAN='\033[0;36m'
WHITE='\033[1;37m'
NC='\033[0m' # No Color

# 日志函数
log_info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

log_step() {
    echo -e "${BLUE}[STEP]${NC} $1"
}

log_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $1"
}

# 检查ClickHouse服务状态
check_clickhouse() {
    log_step "检查 ClickHouse 服务状态..."
    
    # 检查ClickHouse是否运行
    if curl -s "http://localhost:8123/" > /dev/null 2>&1; then
        log_success "ClickHouse 服务运行正常 ✓"
        
        # 显示ClickHouse版本信息
        CH_VERSION=$(curl -s "http://localhost:8123/" || echo "未知版本")
        log_info "ClickHouse 版本: $CH_VERSION"
    else
        log_error "ClickHouse 服务未运行！"
        log_info "请先启动 ClickHouse 服务："
        echo "  方法1: docker run -d -p 8123:8123 clickhouse/clickhouse-server"
        echo "  方法2: brew services start clickhouse (如果通过brew安装)"
        echo "  方法3: systemctl start clickhouse-server (Linux系统)"
        exit 1
    fi
}

# 检查系统资源
check_system_resources() {
    log_step "检查系统资源..."
    
    # 检查可用内存
    if command -v free > /dev/null 2>&1; then
        AVAILABLE_MEM=$(free -m | awk 'NR==2{printf "%.1f", $7/1024}')
        log_info "可用内存: ${AVAILABLE_MEM}GB"
        
        if (( $(echo "$AVAILABLE_MEM < 2.0" | bc -l) )); then
            log_warn "可用内存较少，可能影响测试性能"
        fi
    elif command -v vm_stat > /dev/null 2>&1; then
        # macOS 系统
        FREE_PAGES=$(vm_stat | grep "Pages free" | awk '{print $3}' | sed 's/\.//')
        AVAILABLE_MEM=$(echo "scale=1; $FREE_PAGES * 4096 / 1024 / 1024 / 1024" | bc)
        log_info "可用内存: ${AVAILABLE_MEM}GB (macOS)"
    fi
    
    # 检查CPU核心数
    CPU_CORES=$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo "未知")
    log_info "CPU 核心数: $CPU_CORES"
    
    # 检查磁盘空间
    AVAILABLE_DISK=$(df -h . | awk 'NR==2 {print $4}')
    log_info "当前目录可用磁盘空间: $AVAILABLE_DISK"
}

# 编译优化版本
build_release() {
    log_step "编译 Release 优化版本..."
    
    # 清理之前的构建
    cargo clean
    
    # 编译 Release 版本
    log_info "正在编译高性能版本（启用所有优化）..."
    RUSTFLAGS="-C target-cpu=native -C target-feature=+avx2,+fma" \
    cargo build --release --example comprehensive_db_stress_test
    
    if [ $? -eq 0 ]; then
        log_success "编译完成 ✓"
    else
        log_error "编译失败！"
        exit 1
    fi
}

# 创建结果目录
prepare_output_dirs() {
    log_step "准备输出目录..."
    
    TIMESTAMP=$(date +"%Y%m%d_%H%M%S")
    RESULTS_DIR="./test_results/comprehensive_db_test_$TIMESTAMP"
    
    mkdir -p "$RESULTS_DIR"
    log_info "结果将保存到: $RESULTS_DIR"
    
    # 保存系统信息
    {
        echo "=== 系统信息 ==="
        echo "测试时间: $(date)"
        echo "主机名: $(hostname)"
        echo "操作系统: $(uname -a)"
        echo "Rust 版本: $(rustc --version)"
        echo "Cargo 版本: $(cargo --version)"
        echo ""
        
        if command -v lscpu > /dev/null 2>&1; then
            echo "=== CPU 信息 ==="
            lscpu
            echo ""
        fi
        
        if command -v free > /dev/null 2>&1; then
            echo "=== 内存信息 ==="
            free -h
            echo ""
        fi
        
        echo "=== 磁盘空间 ==="
        df -h
        echo ""
        
    } > "$RESULTS_DIR/system_info.txt"
}

# 运行完整测试
run_comprehensive_test() {
    log_step "启动 20+商品 × 15分钟完整数据库压力测试..."
    
    log_info "测试配置:"
    echo "  • 测试时长: 15 分钟"
    echo "  • 目标商品: 25 个热门交易对"
    echo "  • 数据通道: OrderBook5, Trades, Ticker"
    echo "  • 数据库: ClickHouse (localhost:8123) - 使用现有 hft 数据库"
    echo "  • 批量写入: 500 记录/批次, 4 个并发写入器"
    echo ""
    
    log_warn "注意: 这是一个长时间运行的测试，请确保："
    echo "  • 网络连接稳定"
    echo "  • ClickHouse 服务正常运行"
    echo "  • 有足够的磁盘空间存储数据"
    echo "  • 不要中断测试进程"
    echo ""
    
    read -p "确认开始测试吗？(y/N): " -n 1 -r
    echo
    if [[ ! $REPLY =~ ^[Yy]$ ]]; then
        log_info "测试已取消"
        exit 0
    fi
    
    log_step "开始执行测试..."
    echo "=================================================================================="
    
    # 设置环境变量
    export RUST_LOG=info
    export RUST_BACKTRACE=1
    
    # 运行测试，同时输出到文件和控制台
    ./target/release/examples/comprehensive_db_stress_test 2>&1 | tee "$RESULTS_DIR/test_output.log"
    
    TEST_EXIT_CODE=${PIPESTATUS[0]}
    
    echo "=================================================================================="
    
    if [ $TEST_EXIT_CODE -eq 0 ]; then
        log_success "测试成功完成！"
    else
        log_error "测试异常退出，退出码: $TEST_EXIT_CODE"
    fi
    
    return $TEST_EXIT_CODE
}

# 生成测试总结报告
generate_summary_report() {
    log_step "生成测试总结报告..."
    
    local output_file="$RESULTS_DIR/test_summary.txt"
    
    {
        echo "=================================================================="
        echo "20+商品 × 15分钟完整数据库压力测试 - 总结报告"
        echo "=================================================================="
        echo "测试时间: $(date)"
        echo "测试结果目录: $RESULTS_DIR"
        echo ""
        
        echo "=== 测试配置 ==="
        echo "• 测试时长: 15 分钟"
        echo "• 目标商品数: 25 个"
        echo "• 测试通道: OrderBook5, Trades, Ticker"
        echo "• ClickHouse URL: http://localhost:8123"
        echo "• 数据库: hft_stress_test.market_data_15min"
        echo "• 批量大小: 500 记录/批次"
        echo "• 并发写入器: 4 个"
        echo ""
        
        echo "=== 关键性能指标提取 ==="
        
        # 从测试输出中提取关键指标
        if [ -f "$RESULTS_DIR/test_output.log" ]; then
            echo "从测试日志提取关键指标..."
            
            # 提取最终统计数据
            grep -A 20 "详细性能统计" "$RESULTS_DIR/test_output.log" | tail -20
            echo ""
            
            # 提取优化建议
            echo "=== 自动化优化建议 ==="
            grep -A 30 "自动化性能分析和优化建议" "$RESULTS_DIR/test_output.log" | tail -30
            echo ""
            
        else
            echo "测试输出文件未找到"
        fi
        
        echo "=== 文件清单 ==="
        echo "• system_info.txt - 系统信息"
        echo "• test_output.log - 完整测试输出"
        echo "• test_summary.txt - 此总结报告"
        echo ""
        
        echo "=================================================================="
        
    } > "$output_file"
    
    log_success "总结报告已生成: $output_file"
}

# 清理函数
cleanup() {
    log_info "清理临时文件..."
    # 可以在这里添加清理逻辑
}

# 主函数
main() {
    echo -e "${PURPLE}"
    echo "=================================================================="
    echo "🚀 Rust HFT - 20+商品 × 15分钟完整数据库压力测试"
    echo "=================================================================="
    echo -e "${NC}"
    
    # 设置清理钩子
    trap cleanup EXIT
    
    # 检查先决条件
    check_clickhouse
    check_system_resources
    
    # 准备环境
    prepare_output_dirs
    build_release
    
    # 运行测试
    if run_comprehensive_test; then
        generate_summary_report
        
        echo ""
        log_success "🎉 完整数据库压力测试成功完成！"
        echo ""
        echo -e "${WHITE}测试结果已保存到: ${CYAN}$RESULTS_DIR${NC}"
        echo ""
        echo "查看结果："
        echo "  • 完整输出: cat $RESULTS_DIR/test_output.log"
        echo "  • 测试总结: cat $RESULTS_DIR/test_summary.txt"
        echo "  • 系统信息: cat $RESULTS_DIR/system_info.txt"
        echo ""
        
    else
        log_error "测试失败，请检查日志文件：$RESULTS_DIR/test_output.log"
        exit 1
    fi
}

# 检查是否从正确的目录运行
if [ ! -f "Cargo.toml" ]; then
    log_error "请从 Rust HFT 项目根目录运行此脚本"
    exit 1
fi

# 运行主函数
main "$@"