#!/bin/bash

# 🚀 HFT服务启动脚本
# 用于启动和管理ClickHouse、Redis等基础服务

set -e

echo "🚀 === HFT基础服务管理 ==="
echo

# 函数：检查Docker是否运行
check_docker() {
    if ! docker info >/dev/null 2>&1; then
        echo "❌ Docker未运行，请先启动Docker"
        exit 1
    fi
    echo "✅ Docker运行正常"
}

# 函数：启动基础服务 (ClickHouse + Redis)
start_basic() {
    echo "📊 启动基础服务 (ClickHouse + Redis)..."
    docker-compose up -d clickhouse redis
    
    echo "⏳ 等待服务启动..."
    sleep 10
    
    # 等待ClickHouse就绪
    echo "🔍 检查ClickHouse状态..."
    for i in {1..30}; do
        if curl -s "http://localhost:8123/ping" | grep -q "Ok"; then
            echo "✅ ClickHouse已就绪"
            break
        fi
        echo "⏳ 等待ClickHouse启动... ($i/30)"
        sleep 2
    done
    
    # 检查Redis状态
    echo "🔍 检查Redis状态..."
    if redis-cli ping >/dev/null 2>&1; then
        echo "✅ Redis已就绪"
    else
        echo "❌ Redis启动失败"
    fi
}

# 函数：启动完整服务 (包括监控)
start_full() {
    echo "📊 启动完整服务 (包括PostgreSQL和Grafana)..."
    docker-compose --profile full --profile monitoring up -d
    
    echo "⏳ 等待所有服务启动..."
    sleep 20
    
    echo "🌐 服务访问地址："
    echo "   - ClickHouse HTTP: http://localhost:8123"
    echo "   - ClickHouse Play: http://localhost:8123/play"
    echo "   - Redis: localhost:6379" 
    echo "   - PostgreSQL: localhost:5432"
    echo "   - Grafana: http://localhost:3000 (admin/hft_dashboard_password)"
}

# 函数：停止所有服务
stop_services() {
    echo "🛑 停止所有服务..."
    docker-compose --profile full --profile monitoring down
    echo "✅ 所有服务已停止"
}

# 函数：查看服务状态
status_services() {
    echo "📋 服务状态："
    docker-compose ps
    echo
    
    echo "🔍 连接测试："
    # ClickHouse测试
    if curl -s "http://localhost:8123/ping" | grep -q "Ok"; then
        echo "✅ ClickHouse: 运行正常"
    else
        echo "❌ ClickHouse: 无法连接"
    fi
    
    # Redis测试
    if redis-cli ping >/dev/null 2>&1; then
        echo "✅ Redis: 运行正常"
    elif docker exec hft_redis redis-cli ping >/dev/null 2>&1; then
        echo "✅ Redis: 运行正常 (容器内)"
    else
        echo "❌ Redis: 无法连接"
    fi
    
    echo
    echo "💾 磁盘使用："
    du -sh clickhouse/data redis/data 2>/dev/null || echo "数据目录为空"
}

# 函数：查看日志
logs_services() {
    echo "📝 最近的服务日志："
    echo
    echo "=== ClickHouse日志 ==="
    docker logs --tail 20 hft_clickhouse 2>/dev/null || echo "ClickHouse未运行"
    echo
    echo "=== Redis日志 ==="
    docker logs --tail 20 hft_redis 2>/dev/null || echo "Redis未运行"
}

# 函数：重置数据
reset_data() {
    echo "⚠️  警告：这将删除所有数据！"
    read -p "确认重置数据？(yes/NO): " confirm
    if [ "$confirm" = "yes" ]; then
        echo "🧹 停止服务并清理数据..."
        docker-compose down -v
        sudo rm -rf clickhouse/data/* clickhouse/logs/* redis/data/*
        echo "✅ 数据已重置"
    else
        echo "❌ 操作已取消"
    fi
}

# 函数：运行测试
test_integration() {
    echo "🧪 运行集成测试..."
    
    # 检查服务状态
    if ! curl -s "http://localhost:8123/ping" | grep -q "Ok"; then
        echo "❌ ClickHouse未运行，请先启动服务"
        exit 1
    fi
    
    # 测试ClickHouse
    echo "📊 测试ClickHouse连接..."
    docker exec hft_clickhouse clickhouse-client --user hft_user --password hft_password --query "SELECT 'ClickHouse连接正常' as status"
    
    # 测试数据库
    echo "📋 检查数据库和表..."
    docker exec hft_clickhouse clickhouse-client --user hft_user --password hft_password --database hft_db --query "SHOW TABLES"
    
    # 测试插入和查询
    echo "💾 测试数据插入..."
    docker exec hft_clickhouse clickhouse-client --user hft_user --password hft_password --database hft_db --query "SELECT count() FROM lob_depth"
    
    echo "✅ 集成测试完成"
}

# 主函数
main() {
    check_docker
    
    case "${1:-help}" in
        "start"|"up")
            start_basic
            ;;
        "start-full"|"up-full")
            start_full
            ;;
        "stop"|"down")
            stop_services
            ;;
        "status"|"ps")
            status_services
            ;;
        "logs")
            logs_services
            ;;
        "reset")
            reset_data
            ;;
        "test")
            test_integration
            ;;
        "restart")
            stop_services
            sleep 3
            start_basic
            ;;
        "help"|*)
            echo "使用方法: $0 [命令]"
            echo
            echo "命令："
            echo "  start, up       启动基础服务 (ClickHouse + Redis)"
            echo "  start-full      启动完整服务 (包括PostgreSQL + Grafana)"
            echo "  stop, down      停止所有服务"
            echo "  restart         重启服务"
            echo "  status, ps      查看服务状态"
            echo "  logs            查看服务日志"
            echo "  test            运行集成测试"
            echo "  reset           重置所有数据 (危险操作)"
            echo "  help            显示此帮助信息"
            echo
            echo "示例："
            echo "  $0 start        # 启动ClickHouse和Redis"
            echo "  $0 status       # 查看服务状态"
            echo "  $0 test         # 测试服务连接"
            ;;
    esac
}

# 运行主函数
main "$@"