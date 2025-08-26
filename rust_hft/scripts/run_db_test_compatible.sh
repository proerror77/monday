#!/bin/bash

# 兼容版数据库压力测试 - 适配Apple Silicon和非AVX2 CPU
# 
# 功能：
# - 只启动必要服务 (ClickHouse, Redis)
# - 编译兼容版本 (无AVX2/FMA优化)
# - 运行 20+商品 × 15分钟数据库压力测试
# - 生成完整报告

set -e

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

# 显示开始信息
show_start_info() {
    echo -e "${PURPLE}"
    echo "══════════════════════════════════════════════════════════════════"
    echo "🚀 Rust HFT - 20+商品 × 15分钟数据库压力测试 (兼容版)"
    echo "══════════════════════════════════════════════════════════════════"
    echo -e "${NC}"
    echo ""
    echo "⚡ 兼容版 - 适配Apple Silicon/非x86架构，专注于数据库性能测试"
    echo ""
    echo "🔧 修复内容:"
    echo "  • ✅ 修复生命周期错误"
    echo "  • ✅ 修复类型匹配问题"
    echo "  • ✅ 移除不兼容的CPU特性优化"
    echo "  • ✅ 使用标准Release优化"
    echo ""
    echo "📊 测试配置："
    echo "  • 25 个热门交易对 (BTC, ETH, BNB, XRP, ADA...)"
    echo "  • 3 个数据通道 (OrderBook5, Trades, Ticker)"
    echo "  • 15 分钟持续运行"
    echo "  • 预计收集: 1,000,000+ 条市场数据记录"
    echo "  • 服务: ClickHouse + Redis"
    echo ""
    echo "⏱️  预计耗时: ~18-22 分钟"
    echo ""
    log_info "测试将在 3 秒后自动开始..."
    sleep 3
}

# 检测系统架构
detect_system_arch() {
    local arch=$(uname -m)
    local os=$(uname -s)
    
    log_info "检测系统信息:"
    echo "  架构: $arch"
    echo "  操作系统: $os"
    
    if [[ "$arch" == "arm64" ]] || [[ "$arch" == "aarch64" ]]; then
        log_info "检测到 ARM64 架构 (Apple Silicon)"
        ARCH_TYPE="arm64"
    elif [[ "$arch" == "x86_64" ]]; then
        log_info "检测到 x86_64 架构"
        ARCH_TYPE="x86_64"
    else
        log_warn "未知架构: $arch，使用默认配置"
        ARCH_TYPE="unknown"
    fi
    
    echo ""
}

# 创建结果目录
prepare_results_directory() {
    log_step "准备测试结果目录..."
    
    TIMESTAMP=$(date +"%Y%m%d_%H%M%S")
    RESULTS_DIR="./test_results/compatible_db_test_$TIMESTAMP"
    
    mkdir -p "$RESULTS_DIR"
    log_info "结果将保存到: $RESULTS_DIR"
    
    # 记录测试开始时间
    echo "兼容版数据库测试开始时间: $(date)" > "$RESULTS_DIR/test_timeline.log"
    echo "系统架构: $ARCH_TYPE" >> "$RESULTS_DIR/test_timeline.log"
    
    export RESULTS_DIR
}

# 步骤1：启动核心服务
step1_start_core_services() {
    log_step "步骤 1/6: 启动核心服务 (ClickHouse + Redis)..."
    echo "$(date): 开始启动核心服务" >> "$RESULTS_DIR/test_timeline.log"
    
    # 检查Docker是否运行
    if ! docker info > /dev/null 2>&1; then
        log_error "Docker 服务未运行！请先启动 Docker"
        exit 1
    fi
    
    log_info "启动 ClickHouse 和 Redis..."
    
    # 只启动ClickHouse和Redis
    if (cd ops && docker-compose up -d clickhouse redis) > "$RESULTS_DIR/environment_setup.log" 2>&1; then
        log_success "✅ 核心服务启动完成"
    else
        log_error "❌ 核心服务启动失败"
        echo "详细日志请查看: $RESULTS_DIR/environment_setup.log"
        exit 1
    fi
    
    # 等待服务启动
    log_info "等待服务完全启动..."
    sleep 20
    
    # 检查ClickHouse
    local max_attempts=12
    local attempt=1
    
    while [ $attempt -le $max_attempts ]; do
        if curl -s "http://localhost:8123/ping" > /dev/null 2>&1; then
            log_success "✅ ClickHouse 服务就绪"
            break
        else
            log_info "等待 ClickHouse 启动... (尝试 $attempt/$max_attempts)"
            sleep 5
            attempt=$((attempt + 1))
        fi
    done
    
    if [ $attempt -gt $max_attempts ]; then
        log_error "❌ ClickHouse 启动超时！"
        exit 1
    fi
    
    # 检查Redis
    if docker exec hft_redis redis-cli ping > /dev/null 2>&1; then
        log_success "✅ Redis 服务就绪"
    else
        log_warn "⚠️  Redis 可能还在启动中"
    fi
    
    echo "$(date): 核心服务启动完成" >> "$RESULTS_DIR/test_timeline.log"
}

# 步骤2：编译兼容版本
step2_build_compatible() {
    log_step "步骤 2/6: 编译兼容版本 (无CPU特性优化)..."
    echo "$(date): 开始编译项目" >> "$RESULTS_DIR/test_timeline.log"
    
    log_info "清理之前的构建..."
    cargo clean > "$RESULTS_DIR/build.log" 2>&1
    
    log_info "编译兼容版本 (使用标准Release优化)..."
    
    # 根据架构选择编译参数
    if [[ "$ARCH_TYPE" == "arm64" ]]; then
        log_info "使用 ARM64 优化参数"
        RUSTFLAGS="-C target-cpu=native" \
        cargo build --release --example comprehensive_db_stress_test >> "$RESULTS_DIR/build.log" 2>&1
    elif [[ "$ARCH_TYPE" == "x86_64" ]]; then
        log_info "使用 x86_64 标准优化 (跳过AVX2/FMA)"
        cargo build --release --example comprehensive_db_stress_test >> "$RESULTS_DIR/build.log" 2>&1
    else
        log_info "使用通用优化参数"
        cargo build --release --example comprehensive_db_stress_test >> "$RESULTS_DIR/build.log" 2>&1
    fi
    
    if [ $? -eq 0 ]; then
        log_success "✅ 编译完成"
        
        # 检查编译警告
        local warnings=$(grep -c "warning:" "$RESULTS_DIR/build.log" 2>/dev/null || echo "0")
        if [ "$warnings" -gt 0 ]; then
            log_info "编译过程中有 $warnings 个警告，但编译成功"
        fi
    else
        log_error "❌ 编译失败"
        echo ""
        echo "🔍 编译错误分析:"
        tail -20 "$RESULTS_DIR/build.log"
        echo ""
        echo "详细日志请查看: $RESULTS_DIR/build.log"
        exit 1
    fi
    
    echo "$(date): 项目编译完成" >> "$RESULTS_DIR/test_timeline.log"
}

# 步骤3：准备数据库
step3_prepare_database() {
    log_step "步骤 3/6: 准备 ClickHouse 数据库..."
    echo "$(date): 开始数据库准备" >> "$RESULTS_DIR/test_timeline.log"
    
    # 检查并创建数据库
    log_info "检查 HFT 数据库..."
    
    local db_exists=$(curl -s "http://localhost:8123/" -d "SELECT name FROM system.databases WHERE name = 'hft'" | tr -d '\n')
    
    if [ "$db_exists" = "hft" ]; then
        log_success "✅ HFT 数据库已存在"
    else
        log_info "创建 HFT 数据库..."
        curl -s "http://localhost:8123/" -d "CREATE DATABASE IF NOT EXISTS hft"
        log_success "✅ HFT 数据库创建完成"
    fi
    
    # 检查ClickHouse版本和状态
    local ch_version=$(curl -s "http://localhost:8123/" -d "SELECT version()")
    log_info "ClickHouse 版本: $ch_version"
    
    # 清理旧测试数据（如果存在）
    log_info "清理旧测试数据..."
    curl -s "http://localhost:8123/" -d "DROP TABLE IF EXISTS hft.market_data_15min" > /dev/null 2>&1
    
    # 记录系统信息
    {
        echo "=== 兼容版数据库测试系统信息 ==="
        echo "测试开始时间: $(date)"
        echo "主机名: $(hostname)"
        echo "操作系统: $(uname -a)"
        echo "系统架构: $ARCH_TYPE"
        echo "Rust 版本: $(rustc --version)"
        echo "Cargo 版本: $(cargo --version)"
        echo "ClickHouse 版本: $ch_version"
        echo ""
        
        if command -v lscpu > /dev/null 2>&1; then
            echo "=== CPU 信息 ==="
            lscpu | head -10
            echo ""
        elif command -v sysctl > /dev/null 2>&1; then
            echo "=== CPU 信息 (macOS) ==="
            sysctl -n machdep.cpu.brand_string 2>/dev/null || echo "CPU信息不可用"
            echo "CPU 核心数: $(sysctl -n hw.ncpu 2>/dev/null || echo "未知")"
            echo ""
        fi
        
        if command -v free > /dev/null 2>&1; then
            echo "=== 内存信息 ==="
            free -h
            echo ""
        elif command -v vm_stat > /dev/null 2>&1; then
            echo "=== 内存信息 (macOS) ==="
            vm_stat | head -5
            echo ""
        fi
        
        echo "=== 磁盘空间 ==="
        df -h . | head -2
        echo ""
        
        echo "=== 核心服务状态 ==="
        docker-compose ps clickhouse redis
        echo ""
        
    } > "$RESULTS_DIR/system_info.txt"
    
    echo "$(date): 数据库准备完成" >> "$RESULTS_DIR/test_timeline.log"
}

# 步骤4：运行数据库压力测试
step4_run_stress_test() {
    log_step "步骤 4/6: 运行 20+商品 × 15分钟数据库压力测试..."
    echo "$(date): 开始数据库压力测试" >> "$RESULTS_DIR/test_timeline.log"
    
    log_info "🚀 启动完整数据库压力测试..."
    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "⏳ 测试进行中 (15分钟)，实时输出如下："
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo ""
    
    # 设置环境变量
    export RUST_LOG=info
    export RUST_BACKTRACE=1
    
    # 运行测试，同时输出到控制台和文件
    ./target/release/examples/comprehensive_db_stress_test 2>&1 | tee "$RESULTS_DIR/stress_test.log"
    
    local test_exit_code=${PIPESTATUS[0]}
    
    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    
    if [ $test_exit_code -eq 0 ]; then
        log_success "✅ 数据库压力测试完成"
    else
        log_error "❌ 数据库压力测试失败 (退出码: $test_exit_code)"
        echo "详细日志请查看: $RESULTS_DIR/stress_test.log"
        
        # 尝试提供一些调试信息
        echo ""
        echo "🔍 最近的日志信息:"
        tail -10 "$RESULTS_DIR/stress_test.log" 2>/dev/null || echo "无法读取测试日志"
        
        exit 1
    fi
    
    echo "$(date): 数据库压力测试完成" >> "$RESULTS_DIR/test_timeline.log"
}

# 步骤5：数据验证
step5_verify_data() {
    log_step "步骤 5/6: 验证写入的数据..."
    echo "$(date): 开始数据验证" >> "$RESULTS_DIR/test_timeline.log"
    
    log_info "查询写入的数据统计..."
    
    # 查询总记录数
    local total_records=$(curl -s "http://localhost:8123/" -d "SELECT count() FROM hft.market_data_15min" 2>/dev/null | tr -d '\n')
    if [ -n "$total_records" ] && [ "$total_records" -gt 0 ]; then
        log_info "📊 总记录数: $total_records"
    else
        log_warn "⚠️  未检测到数据或查询失败"
        total_records="0"
    fi
    
    # 查询按商品统计
    if [ "$total_records" -gt 0 ]; then
        log_info "📈 按商品统计 (前10个):"
        curl -s "http://localhost:8123/" -d "
            SELECT symbol, count() as record_count, 
                   min(timestamp) as first_record, 
                   max(timestamp) as last_record
            FROM hft.market_data_15min 
            GROUP BY symbol 
            ORDER BY record_count DESC 
            LIMIT 10
        " > "$RESULTS_DIR/data_by_symbol.txt" 2>/dev/null
        
        if [ -s "$RESULTS_DIR/data_by_symbol.txt" ]; then
            cat "$RESULTS_DIR/data_by_symbol.txt"
        else
            echo "数据查询失败或无数据"
        fi
        
        # 查询按通道统计
        log_info "📡 按通道统计:"
        curl -s "http://localhost:8123/" -d "
            SELECT channel, count() as record_count
            FROM hft.market_data_15min 
            GROUP BY channel 
            ORDER BY record_count DESC
        " > "$RESULTS_DIR/data_by_channel.txt" 2>/dev/null
        
        if [ -s "$RESULTS_DIR/data_by_channel.txt" ]; then
            cat "$RESULTS_DIR/data_by_channel.txt"
        else
            echo "通道统计查询失败"
        fi
        
        # 查询数据库大小
        log_info "💾 数据库大小:"
        curl -s "http://localhost:8123/" -d "
            SELECT 
                formatReadableSize(sum(data_compressed_bytes)) as compressed_size,
                formatReadableSize(sum(data_uncompressed_bytes)) as uncompressed_size,
                count() as total_parts
            FROM system.parts 
            WHERE database = 'hft' AND table = 'market_data_15min'
        " > "$RESULTS_DIR/database_size.txt" 2>/dev/null
        
        if [ -s "$RESULTS_DIR/database_size.txt" ]; then
            cat "$RESULTS_DIR/database_size.txt"
        else
            echo "数据库大小查询失败"
        fi
    fi
    
    log_success "✅ 数据验证完成"
    echo "$(date): 数据验证完成" >> "$RESULTS_DIR/test_timeline.log"
}

# 步骤6：生成综合报告
step6_generate_report() {
    log_step "步骤 6/6: 生成综合测试报告..."
    echo "$(date): 开始生成报告" >> "$RESULTS_DIR/test_timeline.log"
    
    local report_file="$RESULTS_DIR/compatible_db_test_report.md"
    local total_records=$(curl -s "http://localhost:8123/" -d "SELECT count() FROM hft.market_data_15min" 2>/dev/null | tr -d '\n')
    
    if [ -z "$total_records" ]; then
        total_records="查询失败"
    fi
    
    {
        echo "# 20+商品 × 15分钟数据库压力测试报告 (兼容版)"
        echo ""
        echo "**测试时间**: $(date)"
        echo "**结果目录**: $RESULTS_DIR"
        echo "**系统架构**: $ARCH_TYPE"
        echo "**总记录数**: $total_records"
        echo ""
        echo "## 📊 测试概况"
        echo ""
        echo "### 测试配置"
        echo "- **测试时长**: 15 分钟"
        echo "- **目标商品**: 25 个热门交易对"
        echo "- **数据通道**: OrderBook5, Trades, Ticker"
        echo "- **数据库**: ClickHouse (hft.market_data_15min)"
        echo "- **批量大小**: 500 记录/批次"
        echo "- **并发写入**: 4 个写入器"
        echo "- **服务模式**: 兼容版 (仅ClickHouse + Redis)"
        echo "- **优化级别**: 标准Release优化 (无AVX2/FMA)"
        echo ""
        
        echo "### 系统信息"
        if [ -f "$RESULTS_DIR/system_info.txt" ]; then
            echo '```'
            head -15 "$RESULTS_DIR/system_info.txt"
            echo '```'
            echo ""
        fi
        
        echo "### 测试结果"
        
        # 提取关键指标
        if [ -f "$RESULTS_DIR/stress_test.log" ]; then
            echo "#### 性能指标"
            echo '```'
            grep -A 15 "详细性能统计" "$RESULTS_DIR/stress_test.log" | tail -15 | head -20 || echo "性能统计未找到"
            echo '```'
            echo ""
            
            echo "#### 自动化分析"
            echo '```'
            grep -A 20 "自动化性能分析和优化建议" "$RESULTS_DIR/stress_test.log" | tail -20 || echo "优化建议未找到"
            echo '```'
            echo ""
        fi
        
        echo "### 数据验证结果"
        echo ""
        echo "- **总记录数**: $total_records"
        echo ""
        
        if [ -f "$RESULTS_DIR/data_by_symbol.txt" ] && [ -s "$RESULTS_DIR/data_by_symbol.txt" ]; then
            echo "#### 按商品分布"
            echo '```'
            cat "$RESULTS_DIR/data_by_symbol.txt"
            echo '```'
            echo ""
        fi
        
        if [ -f "$RESULTS_DIR/data_by_channel.txt" ] && [ -s "$RESULTS_DIR/data_by_channel.txt" ]; then
            echo "#### 按通道分布"
            echo '```'
            cat "$RESULTS_DIR/data_by_channel.txt"
            echo '```'
            echo ""
        fi
        
        if [ -f "$RESULTS_DIR/database_size.txt" ] && [ -s "$RESULTS_DIR/database_size.txt" ]; then
            echo "#### 数据库存储信息"
            echo '```'
            cat "$RESULTS_DIR/database_size.txt"
            echo '```'
            echo ""
        fi
        
        echo "---"
        echo "**兼容版测试报告生成时间**: $(date)"
        
    } > "$report_file"
    
    log_success "✅ 综合报告生成完成"
    echo "$(date): 报告生成完成" >> "$RESULTS_DIR/test_timeline.log"
}

# 显示最终结果
show_final_results() {
    echo ""
    echo "══════════════════════════════════════════════════════════════════"
    echo -e "${GREEN}🎉 兼容版数据库压力测试完成！${NC}"
    echo "══════════════════════════════════════════════════════════════════"
    echo ""
    
    local total_records=$(curl -s "http://localhost:8123/" -d "SELECT count() FROM hft.market_data_15min" 2>/dev/null | tr -d '\n')
    if [ -z "$total_records" ]; then
        total_records="查询失败"
    fi
    
    echo -e "${WHITE}📊 测试成果总结:${NC}"
    echo "  • ✅ 25个商品 × 15分钟数据收集尝试完成"
    echo "  • ✅ $total_records 条数据写入ClickHouse"
    echo "  • ✅ 兼容性问题修复完成"
    echo "  • ✅ 综合报告和系统信息收集完成"
    echo ""
    
    echo -e "${WHITE}📁 测试结果位置:${NC}"
    echo -e "  ${CYAN}$RESULTS_DIR${NC}"
    echo ""
    
    echo -e "${WHITE}📋 查看报告:${NC}"
    echo "  • 综合报告: cat $RESULTS_DIR/compatible_db_test_report.md"
    echo "  • 详细日志: cat $RESULTS_DIR/stress_test.log"
    echo "  • 系统信息: cat $RESULTS_DIR/system_info.txt"
    echo "  • 数据统计: ls $RESULTS_DIR/*.txt"
    echo ""
    
    echo -e "${WHITE}🔗 服务访问:${NC}"
    echo "  • ClickHouse: http://localhost:8123"
    echo "  • Redis: localhost:6379"
    echo ""
    
    # 提取关键性能指标
    if [ -f "$RESULTS_DIR/stress_test.log" ]; then
        echo -e "${WHITE}📈 关键性能指标:${NC}"
        
        # 提取消息率
        local msg_rate=$(grep "msg/s" "$RESULTS_DIR/stress_test.log" | tail -1 | grep -o '[0-9.]\+ msg/s' | head -1 2>/dev/null)
        if [ -n "$msg_rate" ]; then
            echo "  • 消息处理率: $msg_rate"
        fi
        
        echo ""
    fi
    
    echo "🎯 测试总结："
    if [ "$total_records" != "查询失败" ] && [ "$total_records" -gt 100000 ]; then
        echo "  • ✅ 测试成功！收集了大量真实市场数据"
    elif [ "$total_records" != "查询失败" ] && [ "$total_records" -gt 0 ]; then
        echo "  • ⚠️  测试部分成功，建议检查网络连接和优化参数"
    else
        echo "  • ❌ 测试可能遇到问题，请查看详细日志"
    fi
    
    echo ""
    echo "🔧 下一步建议："
    echo "  • 分析详细报告，了解系统性能表现"
    echo "  • 根据系统架构优化配置参数"
    echo "  • 如需更高性能，考虑使用专用的x86_64服务器"
    echo "  • 可以尝试增加测试时长或商品数量"
    echo ""
}

# 清理函数
cleanup() {
    if [ -n "$RESULTS_DIR" ]; then
        echo "$(date): 兼容版数据库测试结束" >> "$RESULTS_DIR/test_timeline.log"
    fi
}

# 主函数
main() {
    # 设置清理钩子
    trap cleanup EXIT
    
    # 显示开始信息
    show_start_info
    
    # 检测系统架构
    detect_system_arch
    
    # 准备结果目录
    prepare_results_directory
    
    # 执行所有步骤
    step1_start_core_services
    step2_build_compatible
    step3_prepare_database
    step4_run_stress_test
    step5_verify_data
    step6_generate_report
    
    # 显示最终结果
    show_final_results
}

# 检查是否从正确的目录运行
if [ ! -f "Cargo.toml" ] || [ ! -f "docker-compose.yml" ]; then
    log_error "请从项目根目录运行此脚本"
    exit 1
fi

# 运行主函数
main "$@"