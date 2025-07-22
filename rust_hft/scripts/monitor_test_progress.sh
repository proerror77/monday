#!/bin/bash

# UltraThink 智能测试监控脚本
# 实时监控数据库压力测试进度，提供深度分析

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

# 智能分析函数
analyze_performance_trend() {
    local table_name="hft.market_data_15min"
    
    echo -e "${BLUE}🧠 UltraThink 性能趋势分析${NC}"
    echo "=================================="
    
    # 查询过去5分钟的数据趋势
    local trend_data=$(curl -s "http://localhost:8123/" -d "
        SELECT 
            toStartOfMinute(toDateTime(timestamp / 1000000)) as minute,
            count() as records_per_minute,
            uniq(symbol) as unique_symbols,
            avg(processing_latency_us) as avg_latency
        FROM $table_name 
        WHERE timestamp >= (toUnixTimestamp(now()) - 300) * 1000000
        GROUP BY minute 
        ORDER BY minute DESC 
        LIMIT 5
    " 2>/dev/null)
    
    if [ -n "$trend_data" ]; then
        echo "📊 过去5分钟性能趋势:"
        echo "$trend_data"
        echo ""
        
        # 分析趋势
        local latest_rate=$(echo "$trend_data" | head -1 | awk '{print $2}')
        if [ -n "$latest_rate" ] && [ "$latest_rate" -gt 0 ]; then
            if [ "$latest_rate" -gt 5000 ]; then
                echo -e "${GREEN}🚀 性能状态: 优秀 (${latest_rate} records/min)${NC}"
            elif [ "$latest_rate" -gt 2000 ]; then
                echo -e "${YELLOW}⚡ 性能状态: 良好 (${latest_rate} records/min)${NC}"
            else
                echo -e "${RED}⚠️  性能状态: 需注意 (${latest_rate} records/min)${NC}"
            fi
        fi
    else
        echo "⏳ 等待数据accumulate..."
    fi
    
    echo ""
}

analyze_connection_health() {
    echo -e "${CYAN}🔗 连接健康度分析${NC}"
    echo "======================="
    
    # 检查Docker容器状态
    echo "📦 服务容器状态:"
    docker-compose ps clickhouse redis | grep -E "(clickhouse|redis|Up|Exit)"
    echo ""
    
    # 检查ClickHouse连接
    if curl -s "http://localhost:8123/ping" > /dev/null; then
        echo -e "${GREEN}✅ ClickHouse: 连接正常${NC}"
        
        # 查询活跃连接数
        local connections=$(curl -s "http://localhost:8123/" -d "SELECT value FROM system.metrics WHERE metric = 'TCPConnection'" | tr -d '\n')
        echo "   活跃连接数: $connections"
        
        # 查询查询执行统计
        local queries=$(curl -s "http://localhost:8123/" -d "SELECT value FROM system.metrics WHERE metric = 'Query'" | tr -d '\n')
        echo "   正在执行查询: $queries"
        
    else
        echo -e "${RED}❌ ClickHouse: 连接异常${NC}"
    fi
    
    # 检查Redis连接
    if docker exec hft_redis redis-cli ping >/dev/null 2>&1; then
        echo -e "${GREEN}✅ Redis: 连接正常${NC}"
        local redis_memory=$(docker exec hft_redis redis-cli info memory | grep used_memory_human | cut -d: -f2 | tr -d '\r')
        echo "   内存使用: $redis_memory"
    else
        echo -e "${RED}❌ Redis: 连接异常${NC}"
    fi
    
    echo ""
}

analyze_data_quality() {
    local table_name="hft.market_data_15min"
    
    echo -e "${PURPLE}📊 数据质量分析${NC}"
    echo "=================="
    
    # 检查数据一致性
    local symbol_distribution=$(curl -s "http://localhost:8123/" -d "
        SELECT symbol, count() as count 
        FROM $table_name 
        GROUP BY symbol 
        ORDER BY count DESC 
        LIMIT 5
    " 2>/dev/null)
    
    if [ -n "$symbol_distribution" ]; then
        echo "🎯 热门商品数据分布 (前5个):"
        echo "$symbol_distribution"
        echo ""
        
        # 分析数据均匀性
        local max_count=$(echo "$symbol_distribution" | head -1 | awk '{print $2}')
        local min_count=$(echo "$symbol_distribution" | tail -1 | awk '{print $2}')
        
        if [ -n "$max_count" ] && [ -n "$min_count" ] && [ "$min_count" -gt 0 ]; then
            local balance_ratio=$(echo "scale=2; $min_count / $max_count" | bc)
            local balance_percent=$(echo "scale=1; $balance_ratio * 100" | bc)
            
            if (( $(echo "$balance_percent > 80" | bc -l) )); then
                echo -e "${GREEN}⚖️  数据均衡度: 优秀 (${balance_percent}%)${NC}"
            elif (( $(echo "$balance_percent > 50" | bc -l) )); then
                echo -e "${YELLOW}⚖️  数据均衡度: 良好 (${balance_percent}%)${NC}"
            else
                echo -e "${RED}⚖️  数据均衡度: 需注意 (${balance_percent}%)${NC}"
            fi
        fi
    fi
    
    # 检查数据延迟分布
    local latency_stats=$(curl -s "http://localhost:8123/" -d "
        SELECT 
            quantile(0.5)(processing_latency_us) as p50,
            quantile(0.95)(processing_latency_us) as p95,
            quantile(0.99)(processing_latency_us) as p99
        FROM $table_name 
        WHERE timestamp >= (toUnixTimestamp(now()) - 300) * 1000000
    " 2>/dev/null)
    
    if [ -n "$latency_stats" ]; then
        echo "⚡ 延迟分布 (过去5分钟):"
        echo "$latency_stats"
    fi
    
    echo ""
}

predict_final_results() {
    local table_name="hft.market_data_15min"
    
    echo -e "${WHITE}🔮 UltraThink 结果预测${NC}"
    echo "========================="
    
    # 获取当前数据统计
    local current_stats=$(curl -s "http://localhost:8123/" -d "
        SELECT 
            count() as total_records,
            uniq(symbol) as unique_symbols,
            (max(timestamp) - min(timestamp)) / 1000000 as duration_seconds,
            count() / ((max(timestamp) - min(timestamp)) / 1000000) as avg_rate
        FROM $table_name
    " 2>/dev/null)
    
    if [ -n "$current_stats" ]; then
        local total_records=$(echo "$current_stats" | awk '{print $1}')
        local duration_secs=$(echo "$current_stats" | awk '{print $3}')
        local avg_rate=$(echo "$current_stats" | awk '{print $4}')
        
        if [ -n "$total_records" ] && [ "$total_records" -gt 0 ]; then
            echo "📈 当前统计:"
            echo "   已收集记录: $total_records 条"
            echo "   运行时长: ${duration_secs} 秒"
            echo "   平均速率: ${avg_rate} records/s"
            echo ""
            
            # 预测最终结果
            local remaining_time=$((900 - duration_secs))  # 15分钟 = 900秒
            if [ "$remaining_time" -gt 0 ]; then
                local predicted_total=$(echo "$total_records + ($avg_rate * $remaining_time)" | bc)
                local progress_percent=$(echo "scale=1; ($duration_secs / 900) * 100" | bc)
                
                echo "🔮 预测结果:"
                echo "   测试进度: ${progress_percent}%"
                echo "   剩余时间: ${remaining_time} 秒"
                echo "   预计最终记录数: ${predicted_total} 条"
                
                # 成功概率评估
                if (( $(echo "$avg_rate > 5000" | bc -l) )); then
                    echo -e "   ${GREEN}🎯 成功概率: 95%+ (性能优秀)${NC}"
                elif (( $(echo "$avg_rate > 2000" | bc -l) )); then
                    echo -e "   ${YELLOW}🎯 成功概率: 80%+ (性能良好)${NC}"
                else
                    echo -e "   ${RED}🎯 成功概率: 60%+ (需要关注)${NC}"
                fi
            else
                echo -e "${GREEN}✅ 测试已完成！${NC}"
            fi
        fi
    else
        echo "⏳ 等待测试开始..."
    fi
    
    echo ""
}

show_system_resources() {
    echo -e "${YELLOW}💻 系统资源状态${NC}"
    echo "=================="
    
    # CPU使用率
    if command -v top > /dev/null 2>&1; then
        local cpu_usage=$(top -l 1 -n 0 | grep "CPU usage" | awk '{print $3}' | sed 's/%//')
        if [ -n "$cpu_usage" ]; then
            echo "🖥️  CPU使用率: ${cpu_usage}%"
        fi
    fi
    
    # 内存使用
    if command -v free > /dev/null 2>&1; then
        free -h | grep "Mem:"
    elif command -v vm_stat > /dev/null 2>&1; then
        local mem_pressure=$(memory_pressure 2>/dev/null | grep "System-wide memory free percentage" | awk '{print $5}' | sed 's/%//' || echo "unknown")
        if [ "$mem_pressure" != "unknown" ]; then
            echo "💾 内存可用: ${mem_pressure}%"
        fi
    fi
    
    # 磁盘使用
    local disk_usage=$(df -h . | tail -1 | awk '{print $5}' | sed 's/%//')
    local disk_avail=$(df -h . | tail -1 | awk '{print $4}')
    echo "💿 磁盘使用: ${disk_usage}% (可用: ${disk_avail})"
    
    # Docker 资源使用
    echo ""
    echo "🐳 Docker 容器资源使用:"
    docker stats --no-stream --format "table {{.Name}}\t{{.CPUPerc}}\t{{.MemUsage}}" | head -3
    
    echo ""
}

# 主监控循环
main_monitor() {
    echo -e "${PURPLE}"
    echo "🧠 UltraThink 智能测试监控系统启动"
    echo "====================================="
    echo -e "${NC}"
    
    local monitor_interval=30  # 30秒监控间隔
    local start_time=$(date +%s)
    
    while true; do
        clear
        
        local current_time=$(date +%s)
        local elapsed=$((current_time - start_time))
        local elapsed_min=$((elapsed / 60))
        local elapsed_sec=$((elapsed % 60))
        
        echo -e "${WHITE}🕒 监控时长: ${elapsed_min}分${elapsed_sec}秒${NC}"
        echo "======================================"
        echo ""
        
        # 执行各项分析
        analyze_connection_health
        analyze_performance_trend
        analyze_data_quality
        predict_final_results
        show_system_resources
        
        echo -e "${CYAN}⏳ 下次更新: ${monitor_interval}秒后 (按Ctrl+C停止监控)${NC}"
        echo ""
        
        sleep $monitor_interval
    done
}

# 单次分析模式
single_analysis() {
    echo -e "${PURPLE}🧠 UltraThink 单次分析报告${NC}"
    echo "=========================="
    echo ""
    
    analyze_connection_health
    analyze_performance_trend
    analyze_data_quality
    predict_final_results
    show_system_resources
}

# 参数处理
case "${1:-monitor}" in
    "monitor")
        main_monitor
        ;;
    "once")
        single_analysis
        ;;
    *)
        echo "用法: $0 [monitor|once]"
        echo "  monitor: 持续监控模式 (默认)"
        echo "  once: 单次分析模式"
        ;;
esac