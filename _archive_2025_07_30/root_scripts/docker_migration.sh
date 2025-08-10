#!/bin/bash

# HFT Docker 環境整理和遷移腳本
# 安全地從現有分散容器遷移到統一的 Docker Compose stack

set -e  # Exit on any error

# Colors and emojis
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
PURPLE='\033[0;35m'
CYAN='\033[0;36m'
NC='\033[0m'

SUCCESS="✅"
ERROR="❌"
WARNING="⚠️"
INFO="ℹ️"
BACKUP="💾"
CLEAN="🧹"
MIGRATE="🔄"

# Configuration
BACKUP_DIR="./docker_backup_$(date +%Y%m%d_%H%M%S)"
COMPOSE_FILE="docker-compose.hft.yml"
ENV_FILE=".env.hft"

print_header() {
    echo -e "${CYAN}================================${NC}"
    echo -e "${CYAN}  HFT Docker 環境整理工具${NC}"
    echo -e "${CYAN}  安全遷移到 Compose Stack${NC}"
    echo -e "${CYAN}================================${NC}"
    echo ""
}

print_success() {
    echo -e "${GREEN}${SUCCESS} $1${NC}"
}

print_error() {
    echo -e "${RED}${ERROR} $1${NC}"
}

print_warning() {
    echo -e "${YELLOW}${WARNING} $1${NC}"
}

print_info() {
    echo -e "${BLUE}${INFO} $1${NC}"
}

# Step 1: 分析現有環境
analyze_current_environment() {
    echo -e "${PURPLE}=== 第一步：分析現有 Docker 環境 ===${NC}"
    echo ""
    
    print_info "檢測當前運行的 HFT 容器..."
    
    # List all HFT containers
    echo "🐳 當前 HFT 容器："
    docker ps --filter "name=hft-" --format "table {{.Names}}\t{{.Status}}\t{{.Ports}}" || true
    echo ""
    
    # Check networks
    echo "🌐 當前 HFT 網絡："
    docker network ls --filter "name=hft" --format "table {{.Name}}\t{{Driver}}\t{{Scope}}" || true
    echo ""
    
    # Check volumes
    echo "💾 當前 HFT 數據卷："
    docker volume ls --filter "name=hft" --format "table {{.Name}}\t{{.Driver}}\t{{.Size}}" 2>/dev/null || docker volume ls | grep hft || true
    echo ""
    
    # Check images
    echo "📦 當前 HFT 鏡像："
    docker images --format "table {{.Repository}}\t{{.Tag}}\t{{.Size}}" | grep -E "(local/hft|hft)" || true
    echo ""
    
    print_success "環境分析完成"
}

# Step 2: 創建備份
create_backup() {
    echo -e "${PURPLE}=== 第二步：創建數據備份 ===${NC}"
    echo ""
    
    print_info "創建備份目錄: $BACKUP_DIR"
    mkdir -p "$BACKUP_DIR"
    
    # Backup databases
    local databases=("hft-master-db:ai" "hft-ops-db:ai" "hft-ml-db:ai")
    
    for db_info in "${databases[@]}"; do
        IFS=':' read -r container db_name <<< "$db_info"
        
        if docker ps --format '{{.Names}}' | grep -q "^${container}$"; then
            print_info "備份數據庫: $container ($db_name)"
            
            # Create SQL dump
            if docker exec "$container" pg_dump -U ai "$db_name" > "$BACKUP_DIR/${container}_backup.sql" 2>/dev/null; then
                print_success "數據庫 $container 備份完成"
            else
                print_warning "數據庫 $container 備份失敗或為空"
                touch "$BACKUP_DIR/${container}_backup.sql"
            fi
        else
            print_warning "容器 $container 未運行，跳過備份"
        fi
    done
    
    # Backup Docker Compose configurations (if any exist)
    if ls docker-compose*.yml >/dev/null 2>&1; then
        print_info "備份現有 Docker Compose 文件..."
        cp docker-compose*.yml "$BACKUP_DIR/" 2>/dev/null || true
    fi
    
    # Backup environment files
    if ls .env* >/dev/null 2>&1; then
        print_info "備份環境變數文件..."
        cp .env* "$BACKUP_DIR/" 2>/dev/null || true
    fi
    
    # Create system info snapshot
    cat > "$BACKUP_DIR/system_info.txt" << EOF
HFT Docker Environment Backup - $(date)
=====================================

=== Running Containers ===
$(docker ps --filter "name=hft-" --format "table {{.Names}}\t{{.Image}}\t{{.Status}}\t{{.Ports}}")

=== Networks ===
$(docker network ls --filter "name=hft")

=== Volumes ===
$(docker volume ls | grep hft)

=== Images ===
$(docker images | grep -E "(local/hft|hft)")

=== Disk Usage ===
$(docker system df)
EOF
    
    print_success "備份創建完成: $BACKUP_DIR"
}

# Step 3: 驗證新配置
verify_new_configuration() {
    echo -e "${PURPLE}=== 第三步：驗證新配置 ===${NC}"
    echo ""
    
    # Check if compose file exists
    if [[ ! -f "$COMPOSE_FILE" ]]; then
        print_error "Docker Compose 文件 '$COMPOSE_FILE' 不存在"
        print_info "請先運行以下命令創建配置文件："
        echo "  1. 確保 docker-compose.hft.yml 文件存在"
        echo "  2. 設置 .env.hft 環境變數"
        exit 1
    fi
    
    # Validate compose file
    print_info "驗證 Docker Compose 配置..."
    if docker compose -f "$COMPOSE_FILE" config >/dev/null 2>&1; then
        print_success "Docker Compose 配置有效"
    else
        print_error "Docker Compose 配置無效"
        echo "請檢查 $COMPOSE_FILE 文件"
        exit 1
    fi
    
    # Check environment file
    if [[ -f "$ENV_FILE" ]]; then
        print_success "環境變數文件存在: $ENV_FILE"
        
        # Check for required API keys
        if grep -q "your_.*_key_here" "$ENV_FILE"; then
            print_warning "請更新 $ENV_FILE 中的 API 密鑰"
        fi
    else
        print_warning "環境變數文件 '$ENV_FILE' 不存在"
        print_info "將使用默認配置"
    fi
}

# Step 4: 安全停止現有服務
stop_existing_services() {
    echo -e "${PURPLE}=== 第四步：安全停止現有服務 ===${NC}"
    echo ""
    
    print_info "逐步停止現有 HFT 容器..."
    
    # Stop containers in reverse dependency order
    local containers_to_stop=(
        "hft-master-ui" "hft-master-api" "hft-ops-ui" "hft-ops-api" 
        "hft-ml-ui" "hft-ml-api" "hft-master-db" "hft-ops-db" "hft-ml-db"
    )
    
    for container in "${containers_to_stop[@]}"; do
        if docker ps --format '{{.Names}}' | grep -q "^${container}$"; then
            print_info "停止容器: $container"
            if docker stop "$container" >/dev/null 2>&1; then
                print_success "容器 $container 已停止"
            else
                print_warning "停止容器 $container 失敗"
            fi
        else
            print_info "容器 $container 已經停止或不存在"
        fi
    done
    
    # Keep infrastructure running (Redis, ClickHouse) if they exist
    print_info "保持基礎設施服務運行 (Redis, ClickHouse)"
}

# Step 5: 清理舊資源
cleanup_old_resources() {
    echo -e "${PURPLE}=== 第五步：清理舊 Docker 資源 ===${NC}"
    echo ""
    
    read -p "是否要清理停止的容器? (y/N): " -n 1 -r
    echo
    if [[ $REPLY =~ ^[Yy]$ ]]; then
        print_info "移除停止的 HFT 容器..."
        
        # Remove stopped containers
        local stopped_containers=$(docker ps -a --filter "name=hft-" --filter "status=exited" --format "{{.Names}}")
        if [[ -n "$stopped_containers" ]]; then
            echo "$stopped_containers" | xargs docker rm
            print_success "已清理停止的容器"
        else
            print_info "沒有停止的容器需要清理"
        fi
    fi
    
    read -p "是否要清理未使用的網絡? (y/N): " -n 1 -r
    echo
    if [[ $REPLY =~ ^[Yy]$ ]]; then
        print_info "清理未使用的 HFT 網絡..."
        
        # List HFT networks that might be unused
        local hft_networks=$(docker network ls --filter "name=hft" --format "{{.Name}}")
        for network in $hft_networks; do
            # Check if network is in use
            if [[ $(docker network inspect "$network" --format '{{len .Containers}}') -eq 0 ]]; then
                print_info "移除未使用的網絡: $network"
                docker network rm "$network" 2>/dev/null || print_warning "無法移除網絡 $network"
            fi
        done
    fi
    
    read -p "是否要清理未使用的鏡像? (y/N): " -n 1 -r
    echo
    if [[ $REPLY =~ ^[Yy]$ ]]; then
        print_info "清理懸掛鏡像..."
        docker image prune -f
        print_success "懸掛鏡像清理完成"
    fi
}

# Step 6: 部署新 Stack
deploy_new_stack() {
    echo -e "${PURPLE}=== 第六步：部署新 Docker Compose Stack ===${NC}"
    echo ""
    
    print_info "啟動新的 HFT Docker Compose Stack..."
    
    # Start infrastructure first
    print_info "啟動基礎設施服務..."
    if ./hft-compose.sh up infra >/dev/null 2>&1; then
        print_success "基礎設施服務啟動成功"
    else
        print_warning "基礎設施服務啟動可能有問題，繼續嘗試..."
    fi
    
    # Wait a moment for infrastructure
    sleep 5
    
    # Start all services
    print_info "啟動所有 HFT 服務..."
    if ./hft-compose.sh up >/dev/null 2>&1; then
        print_success "HFT Docker Compose Stack 部署成功"
    else
        print_error "Stack 部署失敗，請檢查日誌"
        echo "使用以下命令查看詳細信息："
        echo "  ./hft-compose.sh logs"
        return 1
    fi
    
    # Wait for services to start
    print_info "等待服務完全啟動..."
    sleep 10
    
    # Verify deployment
    print_info "驗證部署狀態..."
    ./hft-compose.sh status
}

# Step 7: 恢復數據（如需要）
restore_data() {
    echo -e "${PURPLE}=== 第七步：數據恢復確認 ===${NC}"
    echo ""
    
    read -p "是否需要恢復備份的數據庫數據? (y/N): " -n 1 -r
    echo
    if [[ $REPLY =~ ^[Yy]$ ]]; then
        print_info "恢復數據庫數據..."
        
        local databases=("hft-master-db:ai" "hft-ops-db:ai" "hft-ml-db:ai")
        
        for db_info in "${databases[@]}"; do
            IFS=':' read -r container db_name <<< "$db_info"
            backup_file="$BACKUP_DIR/${container}_backup.sql"
            
            if [[ -f "$backup_file" ]] && [[ -s "$backup_file" ]]; then
                print_info "恢復數據庫: $container"
                
                # Wait for container to be ready
                print_info "等待數據庫容器就緒..."
                sleep 5
                
                if docker exec -i "$container" psql -U ai "$db_name" < "$backup_file" >/dev/null 2>&1; then
                    print_success "數據庫 $container 恢復成功"
                else
                    print_warning "數據庫 $container 恢復失敗，可能是空數據庫"
                fi
            else
                print_info "跳過 $container (無備份數據或備份為空)"
            fi
        done
    else
        print_info "跳過數據恢復，使用全新數據庫"
    fi
}

# Step 8: 驗證遷移結果
verify_migration() {
    echo -e "${PURPLE}=== 第八步：驗證遷移結果 ===${NC}"
    echo ""
    
    print_info "執行遷移後驗證..."
    
    # Check service health
    if ./hft-compose.sh health >/dev/null 2>&1; then
        print_success "所有服務健康檢查通過"
    else
        print_warning "部分服務可能存在問題，請檢查狀態"
    fi
    
    # Show access URLs
    echo ""
    echo -e "${GREEN}🎉 遷移完成！服務訪問地址：${NC}"
    echo -e "${GREEN}🎯 HFT Master Control: http://localhost:8504${NC}"
    echo -e "${GREEN}⚡ HFT Operations: http://localhost:8503${NC}"
    echo -e "${GREEN}🧠 HFT ML Workspace: http://localhost:8502${NC}"
    echo ""
    
    # Show management commands
    echo -e "${BLUE}💡 常用管理命令：${NC}"
    echo "  ./hft-compose.sh status    # 查看狀態"
    echo "  ./hft-compose.sh logs      # 查看日誌"
    echo "  ./hft-compose.sh restart   # 重啟服務"
    echo "  ./hft-compose.sh health    # 健康檢查"
    echo ""
    
    print_success "Docker 環境整理和遷移完成！"
}

# Main migration function
run_migration() {
    print_header
    
    print_info "開始 HFT Docker 環境整理和遷移流程..."
    echo ""
    
    # Confirm before proceeding
    read -p "這將停止現有容器並遷移到新的 Docker Compose stack。是否繼續? (y/N): " -n 1 -r
    echo
    if [[ ! $REPLY =~ ^[Yy]$ ]]; then
        print_info "操作已取消"
        exit 0
    fi
    
    # Execute migration steps
    analyze_current_environment
    create_backup
    verify_new_configuration
    stop_existing_services
    cleanup_old_resources
    deploy_new_stack
    restore_data
    verify_migration
    
    echo ""
    print_success "🎉 HFT Docker 環境整理完成！"
    print_info "備份文件保存在: $BACKUP_DIR"
}

# Helper functions for individual operations
case "${1:-migrate}" in
    "analyze")
        analyze_current_environment
        ;;
    "backup")
        create_backup
        ;;
    "cleanup")
        cleanup_old_resources
        ;;
    "migrate"|"")
        run_migration
        ;;
    "help")
        echo "HFT Docker 環境整理工具"
        echo ""
        echo "用法: $0 [command]"
        echo ""
        echo "Commands:"
        echo "  migrate  - 執行完整遷移流程 (默認)"
        echo "  analyze  - 僅分析當前環境"
        echo "  backup   - 僅創建備份"
        echo "  cleanup  - 僅清理舊資源"
        echo "  help     - 顯示幫助"
        ;;
    *)
        print_error "未知命令: $1"
        echo "使用 '$0 help' 查看可用命令"
        exit 1
        ;;
esac