#!/bin/bash

# 🚀 完整的 HFT 歷史資料工作流程測試腳本
# 
# 流程：下載 → 入庫 → 訓練
# 
# 使用方法：
# chmod +x test_complete_workflow.sh
# ./test_complete_workflow.sh

set -e  # 遇到錯誤時退出

# 顏色設定
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# 日誌函數
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

# 配置參數
SYMBOL="SOLUSDT"
START_DATE="2024-07-01"
END_DATE="2024-07-03"
DATA_TYPE="depth"
OUTPUT_DIR="./test_historical_data"
CLICKHOUSE_URL="http://localhost:8123"
DATABASE="hft_db"
USERNAME="hft_user"
PASSWORD="hft_password"

# 清理函數
cleanup() {
    log_info "清理測試數據..."
    rm -rf "$OUTPUT_DIR"
    log_success "清理完成"
}

# 檢查 Docker 環境
check_docker() {
    log_info "檢查 Docker 環境..."
    
    if ! command -v docker &> /dev/null; then
        log_error "Docker 未安裝"
        exit 1
    fi
    
    if ! command -v docker-compose &> /dev/null; then
        log_error "Docker Compose 未安裝"
        exit 1
    fi
    
    log_success "Docker 環境檢查通過"
}

# 啟動 ClickHouse
start_clickhouse() {
    log_info "啟動 ClickHouse 服務..."
    
    # 檢查是否已經運行
    if docker-compose ps | grep -q "hft_clickhouse.*Up"; then
        log_warning "ClickHouse 已經在運行"
    else
        docker-compose up -d clickhouse
        
        # 等待服務啟動
        log_info "等待 ClickHouse 啟動..."
        sleep 10
        
        # 檢查健康狀態
        for i in {1..30}; do
            if curl -s "${CLICKHOUSE_URL}/ping" > /dev/null; then
                log_success "ClickHouse 啟動成功"
                break
            fi
            
            if [ $i -eq 30 ]; then
                log_error "ClickHouse 啟動超時"
                exit 1
            fi
            
            sleep 2
        done
    fi
}

# 檢查 Rust 環境
check_rust() {
    log_info "檢查 Rust 環境..."
    
    if ! command -v cargo &> /dev/null; then
        log_error "Rust/Cargo 未安裝"
        exit 1
    fi
    
    # 檢查依賴
    log_info "檢查項目依賴..."
    cargo check --examples
    
    log_success "Rust 環境檢查通過"
}

# 步驟 1: 下載歷史資料並直接導入 ClickHouse
download_and_import_data() {
    log_info "步驟 1: 下載歷史資料並直接導入 ClickHouse..."
    log_info "Symbol: $SYMBOL"
    log_info "Date Range: $START_DATE to $END_DATE"
    log_info "Data Type: $DATA_TYPE"
    log_info "ClickHouse URL: $CLICKHOUSE_URL"
    
    # 清理舊數據
    rm -rf "$OUTPUT_DIR"
    
    # 執行下載並直接導入 ClickHouse
    cargo run --example download_historical_lob -- \
        --symbol "$SYMBOL" \
        --start-date "$START_DATE" \
        --end-date "$END_DATE" \
        --data-type "$DATA_TYPE" \
        --output-dir "$OUTPUT_DIR" \
        --clickhouse-url "$CLICKHOUSE_URL" \
        --clickhouse-db "$DATABASE" \
        --clickhouse-user "$USERNAME" \
        --clickhouse-password "$PASSWORD" \
        --batch-size 5000 \
        --max-concurrent 3 \
        --max-retries 2
    
    # 驗證導入結果
    log_info "驗證 ClickHouse 導入結果..."
    local count=$(curl -s "${CLICKHOUSE_URL}/" --data "SELECT count() FROM ${DATABASE}.lob_depth WHERE symbol = '${SYMBOL}'" 2>/dev/null || echo "0")
    
    if [ "$count" != "0" ]; then
        log_success "歷史資料下載並導入完成，共 $count 條記錄"
    else
        log_warning "未找到導入的數據，可能該日期範圍內沒有數據"
        log_info "嘗試下載更大的日期範圍..."
        
        # 擴大日期範圍重試
        cargo run --example download_historical_lob -- \
            --symbol "$SYMBOL" \
            --start-date "2024-06-01" \
            --end-date "2024-06-03" \
            --data-type "$DATA_TYPE" \
            --output-dir "$OUTPUT_DIR" \
            --clickhouse-url "$CLICKHOUSE_URL" \
            --clickhouse-db "$DATABASE" \
            --clickhouse-user "$USERNAME" \
            --clickhouse-password "$PASSWORD" \
            --batch-size 5000 \
            --max-concurrent 3 \
            --max-retries 2
        
        local retry_count=$(curl -s "${CLICKHOUSE_URL}/" --data "SELECT count() FROM ${DATABASE}.lob_depth WHERE symbol = '${SYMBOL}'" 2>/dev/null || echo "0")
        
        if [ "$retry_count" != "0" ]; then
            log_success "擴大範圍後下載成功，導入 $retry_count 條記錄"
        else
            log_error "歷史資料下載和導入失敗"
            return 1
        fi
    fi
}

# 步驟 2: 執行歷史資料訓練
train_with_historical() {
    log_info "步驟 2: 執行歷史資料訓練..."
    
    # 檢查資料庫中是否有數據
    local count=$(curl -s "${CLICKHOUSE_URL}/" --data "SELECT count() FROM ${DATABASE}.lob_depth WHERE symbol = '${SYMBOL}'" 2>/dev/null || echo "0")
    
    if [ "$count" = "0" ]; then
        log_error "資料庫中沒有 $SYMBOL 的數據"
        return 1
    fi
    
    log_info "開始訓練，使用 $count 條歷史記錄"
    
    # 執行訓練
    cargo run --example train_with_historical -- \
        --symbol "$SYMBOL" \
        --clickhouse-url "$CLICKHOUSE_URL" \
        --historical-days 7 \
        --pretrain-batch-size 256 \
        --pretrain-epochs 5 \
        --pretrain-lr 0.001 \
        --feature-dim 32 \
        --hidden-dim 64 \
        --model-save-path "./test_models"
    
    log_success "歷史資料訓練完成"
    
    # 檢查模型文件
    if [ -d "./test_models" ] && [ "$(ls -A ./test_models)" ]; then
        log_info "模型文件："
        ls -la ./test_models/
    fi
}

# 生成測試報告
generate_report() {
    log_info "生成測試報告..."
    
    local report_file="workflow_test_report_$(date +%Y%m%d_%H%M%S).md"
    
    cat > "$report_file" << EOF
# HFT 歷史資料工作流程測試報告

**測試時間**: $(date)
**測試配置**:
- Symbol: $SYMBOL
- Date Range: $START_DATE to $END_DATE
- Data Type: $DATA_TYPE

## 測試結果

### 1. 歷史資料下載
EOF
    
    if [ -d "$OUTPUT_DIR" ] && [ "$(ls -A $OUTPUT_DIR)" ]; then
        local file_count=$(find "$OUTPUT_DIR" -name "*.jsonl" | wc -l)
        echo "✅ 成功下載 $file_count 個文件" >> "$report_file"
        
        echo -e "\n**文件清單**:" >> "$report_file"
        find "$OUTPUT_DIR" -name "*.jsonl" -exec basename {} \; | sed 's/^/- /' >> "$report_file"
    else
        echo "❌ 歷史資料下載失敗" >> "$report_file"
    fi
    
    cat >> "$report_file" << EOF

### 2. ClickHouse 資料導入
EOF
    
    local db_count=$(curl -s "${CLICKHOUSE_URL}/" --data "SELECT count() FROM ${DATABASE}.lob_depth WHERE symbol = '${SYMBOL}'" 2>/dev/null || echo "0")
    if [ "$db_count" != "0" ]; then
        echo "✅ 成功導入 $db_count 條記錄" >> "$report_file"
    else
        echo "❌ 資料導入失敗或無數據" >> "$report_file"
    fi
    
    cat >> "$report_file" << EOF

### 3. 歷史資料訓練
EOF
    
    if [ -d "./test_models" ] && [ "$(ls -A ./test_models)" ]; then
        echo "✅ 訓練完成，模型已保存" >> "$report_file"
        echo -e "\n**模型文件**:" >> "$report_file"
        ls -la ./test_models/ | tail -n +2 | sed 's/^/- /' >> "$report_file"
    else
        echo "❌ 訓練失敗或模型未保存" >> "$report_file"
    fi
    
    cat >> "$report_file" << EOF

## 系統信息
- Docker版本: $(docker --version)
- Rust版本: $(rustc --version)
- ClickHouse狀態: $(curl -s "${CLICKHOUSE_URL}/ping" > /dev/null && echo "運行中" || echo "未運行")

## 資源使用
- 磁盤使用: $(du -sh "$OUTPUT_DIR" 2>/dev/null || echo "N/A")
- 模型文件大小: $(du -sh "./test_models" 2>/dev/null || echo "N/A")

## 建議
1. 定期清理測試數據以節省磁盤空間
2. 根據實際需求調整批次大小和並發數
3. 考慮使用 GPU 加速訓練過程
EOF
    
    log_success "測試報告已生成: $report_file"
}

# 主函數
main() {
    echo "🚀 HFT 歷史資料工作流程測試"
    echo "=============================="
    
    # 設置錯誤處理
    trap cleanup EXIT
    
    # 執行測試步驟
    check_docker
    check_rust
    start_clickhouse
    
    log_info "開始完整工作流程測試..."
    
    # 執行主要步驟
    if download_and_import_data; then
        if train_with_historical; then
            log_success "🎉 完整工作流程測試成功！"
        else
            log_error "訓練步驟失敗"
            exit 1
        fi
    else
        log_error "下載和導入步驟失敗"
        exit 1
    fi
    
    # 生成報告
    generate_report
    
    log_info "測試完成！"
}

# 解析命令行參數
while [[ $# -gt 0 ]]; do
    case $1 in
        --symbol)
            SYMBOL="$2"
            shift 2
            ;;
        --start-date)
            START_DATE="$2"
            shift 2
            ;;
        --end-date)
            END_DATE="$2"
            shift 2
            ;;
        --data-type)
            DATA_TYPE="$2"
            shift 2
            ;;
        --output-dir)
            OUTPUT_DIR="$2"
            shift 2
            ;;
        --help)
            echo "用法: $0 [選項]"
            echo "選項:"
            echo "  --symbol SYMBOL           交易對符號 (預設: SOLUSDT)"
            echo "  --start-date DATE         開始日期 (預設: 2024-07-01)"
            echo "  --end-date DATE          結束日期 (預設: 2024-07-03)"
            echo "  --data-type TYPE         資料類型 (預設: depth)"
            echo "  --output-dir DIR         輸出目錄 (預設: ./test_historical_data)"
            echo "  --help                   顯示此幫助信息"
            exit 0
            ;;
        *)
            log_error "未知參數: $1"
            exit 1
            ;;
    esac
done

# 運行主函數
main "$@"