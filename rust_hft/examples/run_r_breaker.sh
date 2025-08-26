#!/bin/bash

# R-Breaker Live Trading 運行腳本
# 提供多種運行模式和參數組合

set -e

# 顏色定義
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# 打印帶顏色的消息
print_info() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

print_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $1"
}

print_warning() {
    echo -e "${YELLOW}[WARNING]${NC} $1"
}

print_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

# 檢查 Rust 環境
check_rust() {
    if ! command -v cargo &> /dev/null; then
        print_error "Cargo 未找到，請安裝 Rust 工具鏈"
        exit 1
    fi
    print_success "Rust 環境檢查完成"
}

# 編譯項目
build_project() {
    print_info "編譯 R-Breaker 交易系統..."
    if cargo build --release --example r_breaker_live_trading; then
        print_success "編譯完成"
    else
        print_error "編譯失敗"
        exit 1
    fi
}

# 顯示幫助信息
show_help() {
    echo -e "${BLUE}R-Breaker Live Trading System${NC}"
    echo ""
    echo "使用方法:"
    echo "  $0 [模式] [選項]"
    echo ""
    echo "運行模式:"
    echo "  demo     - 快速演示模式 (5分鐘)"
    echo "  test     - 測試模式 (30分鐘)"
    echo "  live     - 實時交易模式 (60分鐘)"
    echo "  custom   - 自定義參數模式"
    echo ""
    echo "選項:"
    echo "  --symbol SYMBOL     - 交易對 (默認: BTCUSDT)"
    echo "  --capital AMOUNT    - 初始資金 (默認: 10000)"
    echo "  --duration MINUTES  - 運行時間分鐘 (默認: 60)"
    echo "  --sensitivity FLOAT - 敏感度係數 (默認: 0.35)"
    echo "  --stop-loss FLOAT   - 止損比例 (默認: 0.02)"
    echo "  --take-profit FLOAT - 止盈比例 (默認: 0.04)"
    echo "  --verbose           - 詳細輸出"
    echo "  --simulation        - 模擬模式"
    echo ""
    echo "示例:"
    echo "  $0 demo"
    echo "  $0 live --symbol ETHUSDT --capital 50000"
    echo "  $0 custom --duration 120 --sensitivity 0.4 --verbose"
}

# 運行演示模式
run_demo() {
    print_info "啟動 R-Breaker 演示模式..."
    print_warning "演示模式將運行 5 分鐘，使用模擬交易"
    
    cargo run --release --example r_breaker_live_trading -- \
        --symbol BTCUSDT \
        --capital 10000 \
        --duration 5 \
        --sensitivity 0.35 \
        --stop-loss 0.02 \
        --take-profit 0.04 \
        --simulation \
        --verbose
}

# 運行測試模式
run_test() {
    print_info "啟動 R-Breaker 測試模式..."
    print_warning "測試模式將運行 30 分鐘，使用模擬交易"
    
    cargo run --release --example r_breaker_live_trading -- \
        --symbol BTCUSDT \
        --capital 10000 \
        --duration 30 \
        --sensitivity 0.35 \
        --stop-loss 0.02 \
        --take-profit 0.04 \
        --simulation \
        --verbose
}

# 運行實時模式
run_live() {
    print_info "啟動 R-Breaker 實時交易模式..."
    print_warning "實時模式將運行 60 分鐘"
    
    # 額外參數處理
    SYMBOL="BTCUSDT"
    CAPITAL="10000"
    DURATION="60"
    SENSITIVITY="0.35"
    STOP_LOSS="0.02"
    TAKE_PROFIT="0.04"
    VERBOSE=""
    SIMULATION="--simulation"  # 默認使用模擬模式，安全起見
    
    # 解析額外參數
    shift # 移除 'live' 參數
    while [[ $# -gt 0 ]]; do
        case $1 in
            --symbol)
                SYMBOL="$2"
                shift 2
                ;;
            --capital)
                CAPITAL="$2"
                shift 2
                ;;
            --duration)
                DURATION="$2"
                shift 2
                ;;
            --sensitivity)
                SENSITIVITY="$2"
                shift 2
                ;;
            --stop-loss)
                STOP_LOSS="$2"
                shift 2
                ;;
            --take-profit)
                TAKE_PROFIT="$2"
                shift 2
                ;;
            --verbose)
                VERBOSE="--verbose"
                shift
                ;;
            --no-simulation)
                SIMULATION=""
                print_warning "已關閉模擬模式，將進行真實交易！"
                ;;
            *)
                print_error "未知參數: $1"
                exit 1
                ;;
        esac
    done
    
    print_info "交易參數:"
    print_info "  交易對: $SYMBOL"
    print_info "  初始資金: $CAPITAL"
    print_info "  運行時間: $DURATION 分鐘"
    print_info "  敏感度: $SENSITIVITY"
    print_info "  止損: $STOP_LOSS"
    print_info "  止盈: $TAKE_PROFIT"
    
    if [[ -n "$SIMULATION" ]]; then
        print_info "  模式: 模擬交易"
    else
        print_warning "  模式: 真實交易"
        echo ""
        print_warning "您即將開始真實交易！"
        read -p "確認繼續？(y/N): " -n 1 -r
        echo
        if [[ ! $REPLY =~ ^[Yy]$ ]]; then
            print_info "已取消"
            exit 0
        fi
    fi
    
    cargo run --release --example r_breaker_live_trading -- \
        --symbol "$SYMBOL" \
        --capital "$CAPITAL" \
        --duration "$DURATION" \
        --sensitivity "$SENSITIVITY" \
        --stop-loss "$STOP_LOSS" \
        --take-profit "$TAKE_PROFIT" \
        $VERBOSE \
        $SIMULATION
}

# 運行自定義模式
run_custom() {
    print_info "啟動 R-Breaker 自定義模式..."
    
    # 默認參數
    SYMBOL="BTCUSDT"
    CAPITAL="10000"
    DURATION="60"
    SENSITIVITY="0.35"
    STOP_LOSS="0.02"
    TAKE_PROFIT="0.04"
    MAX_POSITION="0.1"
    VERBOSE=""
    SIMULATION="--simulation"
    
    # 解析參數
    shift # 移除 'custom' 參數
    while [[ $# -gt 0 ]]; do
        case $1 in
            --symbol)
                SYMBOL="$2"
                shift 2
                ;;
            --capital)
                CAPITAL="$2"
                shift 2
                ;;
            --duration)
                DURATION="$2"
                shift 2
                ;;
            --sensitivity)
                SENSITIVITY="$2"
                shift 2
                ;;
            --stop-loss)
                STOP_LOSS="$2"
                shift 2
                ;;
            --take-profit)
                TAKE_PROFIT="$2"
                shift 2
                ;;
            --max-position-ratio)
                MAX_POSITION="$2"
                shift 2
                ;;
            --verbose)
                VERBOSE="--verbose"
                shift
                ;;
            --simulation)
                SIMULATION="--simulation"
                shift
                ;;
            --no-simulation)
                SIMULATION=""
                shift
                ;;
            *)
                print_error "未知參數: $1"
                show_help
                exit 1
                ;;
        esac
    done
    
    cargo run --release --example r_breaker_live_trading -- \
        --symbol "$SYMBOL" \
        --capital "$CAPITAL" \
        --duration "$DURATION" \
        --sensitivity "$SENSITIVITY" \
        --stop-loss "$STOP_LOSS" \
        --take-profit "$TAKE_PROFIT" \
        --max-position-ratio "$MAX_POSITION" \
        $VERBOSE \
        $SIMULATION
}

# 預設配置運行
run_preset() {
    local preset=$1
    shift
    
    case $preset in
        "conservative")
            print_info "使用保守配置..."
            cargo run --release --example r_breaker_live_trading -- \
                --sensitivity 0.3 \
                --stop-loss 0.015 \
                --take-profit 0.03 \
                --max-position-ratio 0.08 \
                --simulation \
                --verbose \
                "$@"
            ;;
        "aggressive")
            print_info "使用激進配置..."
            cargo run --release --example r_breaker_live_trading -- \
                --sensitivity 0.4 \
                --stop-loss 0.025 \
                --take-profit 0.05 \
                --max-position-ratio 0.15 \
                --simulation \
                --verbose \
                "$@"
            ;;
        "balanced")
            print_info "使用平衡配置..."
            cargo run --release --example r_breaker_live_trading -- \
                --sensitivity 0.35 \
                --stop-loss 0.02 \
                --take-profit 0.04 \
                --max-position-ratio 0.1 \
                --simulation \
                --verbose \
                "$@"
            ;;
        *)
            print_error "未知預設配置: $preset"
            print_info "可用預設: conservative, aggressive, balanced"
            exit 1
            ;;
    esac
}

# 主函數
main() {
    echo -e "${BLUE}=====================================${NC}"
    echo -e "${BLUE}  R-Breaker Live Trading System  ${NC}"
    echo -e "${BLUE}=====================================${NC}"
    echo ""
    
    # 檢查 Rust 環境
    check_rust
    
    # 編譯項目
    build_project
    
    # 解析命令行參數
    if [[ $# -eq 0 ]]; then
        show_help
        exit 0
    fi
    
    case $1 in
        "demo")
            run_demo
            ;;
        "test")
            run_test
            ;;
        "live")
            run_live "$@"
            ;;
        "custom")
            run_custom "$@"
            ;;
        "conservative"|"aggressive"|"balanced")
            run_preset "$@"
            ;;
        "help"|"--help"|"-h")
            show_help
            ;;
        *)
            print_error "未知模式: $1"
            show_help
            exit 1
            ;;
    esac
}

# 運行主函數
main "$@"