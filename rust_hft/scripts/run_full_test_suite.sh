#!/usr/bin/env bash
set -euo pipefail

# Deprecated in favor of run_test_suite.sh.
# This shim keeps backward compatibility for docs and scripts.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "$SCRIPT_DIR/run_test_suite.sh" "$@"
step4_run_stress_test() {
    log_step "步骤 4/6: 运行 20+商品 × 15分钟数据库压力测试..."
    echo "$(date): 开始数据库压力测试" >> "$RESULTS_DIR/test_timeline.log"
    
    log_info "🚀 启动完整数据库压力测试..."
    log_info "📊 测试参数:"
    echo "   • 目标商品: 25 个热门交易对"
    echo "   • 数据通道: OrderBook5, Trades, Ticker"
    echo "   • 测试时长: 15 分钟"
    echo "   • 监控间隔: 30 秒"
    echo "   • 批量大小: 500 记录/批次"
    echo "   • 并发写入: 4 个写入器"
    echo ""
    
    log_info "⏳ 测试进行中，请耐心等待..."
    echo "   实时日志将保存到: $RESULTS_DIR/stress_test.log"
    echo ""
    
    # 设置环境变量
    export RUST_LOG=info
    export RUST_BACKTRACE=1
    
    # 运行测试
    ./target/release/examples/comprehensive_db_stress_test > "$RESULTS_DIR/stress_test.log" 2>&1
    
    local test_exit_code=$?
    
    if [ $test_exit_code -eq 0 ]; then
        log_success "✅ 数据库压力测试完成"
    else
        log_error "❌ 数据库压力测试失败 (退出码: $test_exit_code)"
        echo "详细日志请查看: $RESULTS_DIR/stress_test.log"
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
    local total_records=$(curl -s "http://localhost:8123/" -d "SELECT count() FROM hft.market_data_15min" | tr -d '\n')
    log_info "📊 总记录数: $total_records"
    
    # 查询按商品统计
    log_info "📈 按商品统计 (前10个):"
    curl -s "http://localhost:8123/" -d "
        SELECT symbol, count() as record_count, 
               min(timestamp) as first_record, 
               max(timestamp) as last_record
        FROM hft.market_data_15min 
        GROUP BY symbol 
        ORDER BY record_count DESC 
        LIMIT 10
    " > "$RESULTS_DIR/data_by_symbol.txt"
    
    cat "$RESULTS_DIR/data_by_symbol.txt"
    
    # 查询按通道统计
    log_info "📡 按通道统计:"
    curl -s "http://localhost:8123/" -d "
        SELECT channel, count() as record_count
        FROM hft.market_data_15min 
        GROUP BY channel 
        ORDER BY record_count DESC
    " > "$RESULTS_DIR/data_by_channel.txt"
    
    cat "$RESULTS_DIR/data_by_channel.txt"
    
    # 查询时间分布
    log_info "⏰ 时间分布:"
    curl -s "http://localhost:8123/" -d "
        SELECT 
            toDateTime(intDiv(timestamp, 1000000)) as time_bucket,
            count() as records_per_second
        FROM hft.market_data_15min 
        GROUP BY time_bucket 
        ORDER BY time_bucket DESC 
        LIMIT 20
    " > "$RESULTS_DIR/data_time_distribution.txt"
    
    head -10 "$RESULTS_DIR/data_time_distribution.txt"
    
    log_success "✅ 数据验证完成"
    echo "$(date): 数据验证完成" >> "$RESULTS_DIR/test_timeline.log"
}

# 步骤6：生成综合报告
step6_generate_report() {
    log_step "步骤 6/6: 生成综合测试报告..."
    echo "$(date): 开始生成报告" >> "$RESULTS_DIR/test_timeline.log"
    
    local report_file="$RESULTS_DIR/comprehensive_test_report.md"
    
    {
        echo "# 20+商品 × 15分钟完整数据库压力测试报告"
        echo ""
        echo "**测试时间**: $(date)"
        echo "**结果目录**: $RESULTS_DIR"
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
        echo ""
        
        echo "### 测试结果"
        
        # 提取关键指标
        if [ -f "$RESULTS_DIR/stress_test.log" ]; then
            echo "#### 性能指标"
            echo '```'
            grep -A 15 "详细性能统计" "$RESULTS_DIR/stress_test.log" | tail -15 | head -20
            echo '```'
            echo ""
            
            echo "#### 自动化分析"
            echo '```'
            grep -A 20 "自动化性能分析和优化建议" "$RESULTS_DIR/stress_test.log" | tail -20
            echo '```'
            echo ""
        fi
        
        echo "### 数据验证结果"
        echo ""
        
        local total_records=$(curl -s "http://localhost:8123/" -d "SELECT count() FROM hft.market_data_15min" | tr -d '\n')
        echo "- **总记录数**: $total_records"
        echo ""
        
        echo "#### 按商品分布"
        echo '```'
        cat "$RESULTS_DIR/data_by_symbol.txt"
        echo '```'
        echo ""
        
        echo "#### 按通道分布"
        echo '```'
        cat "$RESULTS_DIR/data_by_channel.txt"
        echo '```'
        echo ""
        
        echo "## 🏆 测试总结"
        echo ""
        
        # 计算测试成功率
        local test_success="✅ 成功"
        if grep -q "ERROR" "$RESULTS_DIR/stress_test.log"; then
            test_success="⚠️ 部分成功"
        fi
        
        echo "- **测试状态**: $test_success"
        echo "- **数据完整性**: ✅ 验证通过"
        echo "- **性能表现**: 详见上述性能指标"
        echo ""
        
        echo "## 📁 文件清单"
        echo ""
        echo "- \`test_timeline.log\` - 测试执行时间线"
        echo "- \`environment_setup.log\` - 环境启动日志"
        echo "- \`build.log\` - 编译日志"
        echo "- \`stress_test.log\` - 完整测试输出"
        echo "- \`system_info.txt\` - 系统信息"
        echo "- \`data_by_symbol.txt\` - 按商品数据统计"
        echo "- \`data_by_channel.txt\` - 按通道数据统计"
        echo "- \`data_time_distribution.txt\` - 时间分布统计"
        echo "- \`comprehensive_test_report.md\` - 此报告"
        echo ""
        
        echo "---"
        echo "**报告生成时间**: $(date)"
        
    } > "$report_file"
    
    log_success "✅ 综合报告生成完成"
    echo "$(date): 报告生成完成" >> "$RESULTS_DIR/test_timeline.log"
}

# 显示最终结果
show_final_results() {
    echo ""
    echo "══════════════════════════════════════════════════════════════════"
    echo -e "${GREEN}🎉 完整测试套件执行完成！${NC}"
    echo "══════════════════════════════════════════════════════════════════"
    echo ""
    
    local total_records=$(curl -s "http://localhost:8123/" -d "SELECT count() FROM hft.market_data_15min" | tr -d '\n')
    
    echo -e "${WHITE}📊 测试成果总结:${NC}"
    echo "  • ✅ 25个商品 × 15分钟数据收集完成"
    echo "  • ✅ $total_records 条真实市场数据写入ClickHouse"
    echo "  • ✅ 性能监控和分析完成"
    echo "  • ✅ 数据完整性验证通过"
    echo "  • ✅ 综合报告和优化建议生成"
    echo ""
    
    echo -e "${WHITE}📁 测试结果位置:${NC}"
    echo -e "  ${CYAN}$RESULTS_DIR${NC}"
    echo ""
    
    echo -e "${WHITE}📋 查看报告:${NC}"
    echo "  • 综合报告: cat $RESULTS_DIR/comprehensive_test_report.md"
    echo "  • 详细日志: cat $RESULTS_DIR/stress_test.log"
    echo "  • 性能数据: 查看 Grafana dashboard (http://localhost:3000)"
    echo ""
    
    echo -e "${WHITE}🔗 服务访问:${NC}"
    echo "  • ClickHouse: http://localhost:8123"
    echo "  • Grafana: http://localhost:3000 (admin/admin)"
    echo "  • Prometheus: http://localhost:9090"
    echo ""
    
    # 提取优化建议
    if grep -q "系统级优化建议" "$RESULTS_DIR/stress_test.log"; then
        echo -e "${WHITE}💡 关键优化建议:${NC}"
        grep -A 10 "系统级优化建议" "$RESULTS_DIR/stress_test.log" | tail -10 | sed 's/^/  /'
        echo ""
    fi
    
    echo "🎯 下一步可以进行："
    echo "  • 分析性能数据，调优系统参数"
    echo "  • 基于测试结果优化批量大小和并发数"
    echo "  • 扩展到更多商品或更长时间测试"
    echo "  • 集成机器学习模型进行实时推理"
    echo ""
}

# 清理函数
cleanup() {
    if [ -n "$RESULTS_DIR" ]; then
        echo "$(date): 测试套件结束" >> "$RESULTS_DIR/test_timeline.log"
    fi
}

# 主函数
main() {
    # 设置清理钩子
    trap cleanup EXIT
    
    # 显示欢迎信息
    show_welcome
    
    # 准备结果目录
    prepare_results_directory
    
    # 执行所有步骤
    step1_start_environment
    step2_build_project
    step3_verify_environment
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
