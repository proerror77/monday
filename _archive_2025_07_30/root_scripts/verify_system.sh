#!/bin/bash
# 
# HFT 系統狀態驗證腳本
# ===================

set -euo pipefail

# 顏色定義
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log_info() { echo -e "${BLUE}[INFO]${NC} $1"; }
log_success() { echo -e "${GREEN}[SUCCESS]${NC} $1"; }
log_warning() { echo -e "${YELLOW}[WARNING]${NC} $1"; }
log_error() { echo -e "${RED}[ERROR]${NC} $1"; }

echo "🔍 HFT 系統狀態驗證"
echo "=================="
echo

# 1. 檢查基礎設施
log_info "檢查基礎設施狀態..."

# Redis
if redis-cli ping >/dev/null 2>&1; then
    log_success "✅ Redis: 正常運行"
else
    log_error "❌ Redis: 無法連接"
fi

# ClickHouse
if curl -s http://localhost:8123/ping >/dev/null 2>&1; then
    log_success "✅ ClickHouse: 正常運行"
else
    log_error "❌ ClickHouse: 無法連接"
fi

# 2. 檢查 ClickHouse 認證
log_info "驗證 ClickHouse 認證配置..."
if docker exec hft-clickhouse clickhouse-client --user=hft_user --password=hft_password --query "SELECT 'auth_ok'" >/dev/null 2>&1; then
    log_success "✅ ClickHouse 認證: 配置正確"
else
    log_error "❌ ClickHouse 認證: 配置有問題"
fi

# 3. 檢查數據庫和表
log_info "檢查數據庫結構..."
if docker exec hft-clickhouse clickhouse-client --user=hft_user --password=hft_password --database=hft --query "SHOW TABLES" >/dev/null 2>&1; then
    table_count=$(docker exec hft-clickhouse clickhouse-client --user=hft_user --password=hft_password --database=hft --query "SHOW TABLES" | wc -l)
    log_success "✅ HFT 數據庫: 可訪問 ($table_count 個表)"
else
    log_error "❌ HFT 數據庫: 無法訪問"
fi

# 4. 檢查 Rust 進程
log_info "檢查 Rust HFT 核心..."
if pgrep -f "market_data_collector" >/dev/null 2>&1; then
    rust_pid=$(pgrep -f "market_data_collector")
    log_success "✅ Rust HFT 核心: 運行中 (PID: $rust_pid)"
else
    log_warning "⚠️ Rust HFT 核心: 未運行"
fi

# 5. 檢查 UI 端點
log_info "檢查 UI 服務可用性..."

ports=(8501 8502 8503)
names=("Ops Dashboard" "ML Dashboard" "Master Dashboard")

for i in "${!ports[@]}"; do
    port=${ports[$i]}
    name=${names[$i]}
    
    if curl -s -f "http://localhost:$port/" >/dev/null 2>&1; then
        log_success "✅ $name: 可訪問 (http://localhost:$port)"
    else
        log_warning "⚠️ $name: 無法訪問 (http://localhost:$port)"
    fi
done

# 6. 檢查 Docker 容器
log_info "檢查 Docker 容器狀態..."
docker_containers=$(docker ps --format "table {{.Names}}\t{{.Status}}" | grep -E "hft-" || true)
if [ -n "$docker_containers" ]; then
    echo "$docker_containers"
else
    log_warning "⚠️ 沒有找到運行中的 HFT Docker 容器"
fi

echo
echo "📊 系統摘要"
echo "=========="

# 計算健康分數
score=0
total=6

if redis-cli ping >/dev/null 2>&1; then ((score++)); fi
if curl -s http://localhost:8123/ping >/dev/null 2>&1; then ((score++)); fi
if docker exec hft-clickhouse clickhouse-client --user=hft_user --password=hft_password --query "SELECT 1" >/dev/null 2>&1; then ((score++)); fi
if pgrep -f "market_data_collector" >/dev/null 2>&1; then ((score++)); fi

ui_available=0
for port in 8501 8502 8503; do
    if curl -s -f "http://localhost:$port/" >/dev/null 2>&1; then
        ((ui_available++))
    fi
done

if [ $ui_available -gt 0 ]; then ((score++)); fi
if [ -n "$(docker ps -q --filter name=hft-)" ]; then ((score++)); fi

percentage=$((score * 100 / total))

if [ $percentage -ge 80 ]; then
    log_success "🎉 系統健康度: $percentage% ($score/$total) - 良好"
elif [ $percentage -ge 60 ]; then
    log_warning "⚠️ 系統健康度: $percentage% ($score/$total) - 一般"
else
    log_error "❌ 系統健康度: $percentage% ($score/$total) - 需要修復"
fi

echo
echo "🔧 可用服務地址:"
echo "• Master Dashboard: http://localhost:8503"
echo "• Ops Dashboard:    http://localhost:8501"
echo "• ML Dashboard:     http://localhost:8502"
echo
echo "📝 日誌文件位置: logs/ 目錄"