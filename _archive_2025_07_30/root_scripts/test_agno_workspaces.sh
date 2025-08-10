#!/bin/bash

# 測試標準化 Agno Workspaces
# ===========================

set -e

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log_info() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

log_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $1"
}

log_warning() {
    echo -e "${YELLOW}[WARNING]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

echo "🧪 測試標準化 Agno Workspaces"
echo "============================="
echo ""

# 測試每個 workspace 的配置
workspaces=("ops_workspace" "ml_workspace" "master_workspace")

for ws in "${workspaces[@]}"; do
    log_info "測試 $ws..."
    
    if [ -d "$ws" ]; then
        cd "$ws"
        
        # 檢查必要文件
        if [ -f "workspace/dev_resources.py" ]; then
            log_success "✅ $ws: dev_resources.py 存在"
        else
            log_error "❌ $ws: dev_resources.py 缺失"
        fi
        
        if [ -f "workspace/settings.py" ]; then
            log_success "✅ $ws: settings.py 存在"
        else
            log_error "❌ $ws: settings.py 缺失"
        fi
        
        if [ -f "workspace/secrets/dev_app_secrets.yml" ]; then
            log_success "✅ $ws: secrets 配置存在"
        else
            log_error "❌ $ws: secrets 配置缺失"
        fi
        
        if [ -f "api/main.py" ]; then
            log_success "✅ $ws: API 端點存在"
        else
            log_error "❌ $ws: API 端點缺失"
        fi
        
        # 測試 ag ws 命令
        log_info "設置 $ws 為活動工作區..."
        ag set > /dev/null 2>&1
        
        log_info "測試 $ws dry run..."
        if ag ws up --dry-run > /dev/null 2>&1; then
            log_success "✅ $ws: ag ws up --dry-run 成功"
        else
            log_warning "⚠️ $ws: ag ws up --dry-run 失敗"
        fi
        
        cd ..
        echo ""
    else
        log_error "❌ $ws 目錄不存在"
    fi
done

echo "🎉 Agno Workspaces 標準化測試完成！"
echo ""
echo "📋 下一步："
echo "1. 檢查 Docker 是否運行"
echo "2. 啟動基礎設施 (Redis + ClickHouse)"
echo "3. 使用冷啟動腳本啟動完整系統"
echo ""
echo "🚀 啟動命令："
echo "   ./cold_start_system.sh"