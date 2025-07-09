#!/bin/bash

# Worktree重构脚本：从复杂结构重构为简化的3环境架构
# 目标：monday-rust-hft/ + monday-agno-framework/ + monday-integration/

set -euo pipefail

# 颜色定义
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
RED='\033[0;31m'
MAGENTA='\033[0;35m'
CYAN='\033[0;36m'
NC='\033[0m'

# 基础路径
BASE_DIR="/Users/shihsonic/Documents"
MAIN_PROJECT="$BASE_DIR/monday"

# 日志函数
log_header() {
    echo -e "\n${BLUE}========================================${NC}"
    echo -e "${BLUE}$1${NC}"
    echo -e "${BLUE}========================================${NC}"
}

log_step() {
    echo -e "\n${CYAN}→ $1${NC}"
}

log_success() {
    echo -e "${GREEN}✓ $1${NC}"
}

log_warning() {
    echo -e "${YELLOW}⚠ $1${NC}"
}

log_error() {
    echo -e "${RED}✗ $1${NC}"
}

# 检查当前worktree状态
check_current_status() {
    log_header "📊 检查当前Worktree状态"
    
    cd "$MAIN_PROJECT"
    echo "当前Worktree列表："
    git worktree list
    
    echo -e "\n当前目录结构："
    ls -la "$BASE_DIR" | grep monday
}

# 备份当前配置
backup_current_config() {
    log_header "💾 备份当前配置"
    
    local backup_dir="$BASE_DIR/monday-worktree-backup-$(date +%Y%m%d_%H%M%S)"
    mkdir -p "$backup_dir"
    
    log_step "备份worktree配置和关键文件"
    
    # 备份各环境的CLAUDE.md和核心脚本
    for dir in monday-*-dev monday-integration-testing monday-production; do
        if [[ -d "$BASE_DIR/$dir" ]]; then
            local backup_sub="$backup_dir/$dir"
            mkdir -p "$backup_sub"
            
            # 备份CLAUDE.md
            [[ -f "$BASE_DIR/$dir/CLAUDE.md" ]] && cp "$BASE_DIR/$dir/CLAUDE.md" "$backup_sub/"
            
            # 备份shell脚本
            find "$BASE_DIR/$dir" -name "*.sh" -type f -exec cp {} "$backup_sub/" \; 2>/dev/null || true
            
            # 备份配置文件
            find "$BASE_DIR/$dir" -name "*.yaml" -o -name "*.yml" -o -name "*.toml" | head -10 | xargs -I {} cp {} "$backup_sub/" 2>/dev/null || true
            
            log_success "已备份 $dir"
        fi
    done
    
    # 备份workspace manager
    [[ -f "$BASE_DIR/monday-workspace-manager.sh" ]] && cp "$BASE_DIR/monday-workspace-manager.sh" "$backup_dir/"
    
    # 备份VS Code工作区
    [[ -f "$BASE_DIR/monday-hft-workspace.code-workspace" ]] && cp "$BASE_DIR/monday-hft-workspace.code-workspace" "$backup_dir/"
    
    log_success "配置备份完成: $backup_dir"
    echo "$backup_dir" > "$MAIN_PROJECT/.worktree_backup_path"
}

# 创建新的分支结构
create_new_branches() {
    log_header "🌿 创建新的分支结构"
    
    cd "$MAIN_PROJECT"
    
    # 确保在develop分支上
    git checkout develop
    
    # 创建新的功能分支
    local branches=(
        "feature/rust-hft-unified"
        "feature/agno-framework-unified" 
        "feature/integration-unified"
    )
    
    for branch in "${branches[@]}"; do
        log_step "创建分支: $branch"
        
        if git show-ref --quiet --heads "$branch"; then
            log_warning "分支 $branch 已存在，跳过创建"
        else
            git checkout -b "$branch" develop
            log_success "已创建分支: $branch"
        fi
    done
    
    git checkout develop
}

# 移除旧的worktree
remove_old_worktrees() {
    log_header "🗑️ 移除旧的Worktree"
    
    cd "$MAIN_PROJECT"
    
    # 要移除的旧worktree
    local old_worktrees=(
        "monday-rust-core-dev"
        "monday-rust-engine-dev" 
        "monday-integrations-dev"
        "monday-ml-dev"
        "monday-integration-testing"
        "monday-production"
        "monday-agno-dev"
        "monday-rust-hft-dev"
    )
    
    for worktree in "${old_worktrees[@]}"; do
        local worktree_path="$BASE_DIR/$worktree"
        
        if [[ -d "$worktree_path" ]]; then
            log_step "移除worktree: $worktree"
            
            # 检查是否有未提交的更改
            if [[ -d "$worktree_path/.git" ]]; then
                cd "$worktree_path"
                if [[ -n "$(git status --porcelain)" ]]; then
                    log_warning "$worktree 有未提交的更改，跳过删除"
                    continue
                fi
                cd "$MAIN_PROJECT"
            fi
            
            # 移除worktree
            git worktree remove "$worktree_path" --force || true
            
            # 如果目录仍然存在，手动删除
            if [[ -d "$worktree_path" ]]; then
                rm -rf "$worktree_path"
            fi
            
            log_success "已移除: $worktree"
        fi
    done
}

# 创建新的worktree结构
create_new_worktrees() {
    log_header "🏗️ 创建新的Worktree结构"
    
    cd "$MAIN_PROJECT"
    
    # 新的worktree配置
    declare -A new_worktrees=(
        ["monday-rust-hft"]="feature/rust-hft-unified"
        ["monday-agno-framework"]="feature/agno-framework-unified"
        ["monday-integration"]="feature/integration-unified"
    )
    
    for worktree_name in "${!new_worktrees[@]}"; do
        local branch="${new_worktrees[$worktree_name]}"
        local worktree_path="$BASE_DIR/$worktree_name"
        
        log_step "创建worktree: $worktree_name ($branch)"
        
        # 创建worktree
        git worktree add "$worktree_path" "$branch"
        
        log_success "已创建: $worktree_name"
    done
}

# 从备份中恢复关键配置
restore_key_configs() {
    log_header "🔄 恢复和整合关键配置"
    
    local backup_path
    if [[ -f "$MAIN_PROJECT/.worktree_backup_path" ]]; then
        backup_path=$(cat "$MAIN_PROJECT/.worktree_backup_path")
    else
        log_error "找不到备份路径"
        return 1
    fi
    
    # monday-rust-hft: 整合Rust相关配置
    log_step "配置 monday-rust-hft 环境"
    local rust_hft_dir="$BASE_DIR/monday-rust-hft"
    
    # 创建基础目录结构
    mkdir -p "$rust_hft_dir"/{config,scripts,docs,target}
    
    # 从备份中恢复Rust相关配置
    for source_dir in monday-rust-core-dev monday-rust-engine-dev monday-integrations-dev; do
        if [[ -d "$backup_path/$source_dir" ]]; then
            # 复制CLAUDE.md (合并内容)
            if [[ -f "$backup_path/$source_dir/CLAUDE.md" ]]; then
                cat "$backup_path/$source_dir/CLAUDE.md" >> "$rust_hft_dir/docs/CLAUDE_merged.md"
                echo -e "\n---\n" >> "$rust_hft_dir/docs/CLAUDE_merged.md"
            fi
            
            # 复制脚本文件
            find "$backup_path/$source_dir" -name "*.sh" -exec cp {} "$rust_hft_dir/scripts/" \; 2>/dev/null || true
        fi
    done
    
    # monday-agno-framework: 整合ML和Agent相关配置
    log_step "配置 monday-agno-framework 环境"
    local agno_dir="$BASE_DIR/monday-agno-framework"
    
    mkdir -p "$agno_dir"/{agents,pipelines,models,config,tests,docs}
    
    # 从备份中恢复ML和Agent相关配置
    for source_dir in monday-ml-dev monday-agno-dev; do
        if [[ -d "$backup_path/$source_dir" ]]; then
            if [[ -f "$backup_path/$source_dir/CLAUDE.md" ]]; then
                cat "$backup_path/$source_dir/CLAUDE.md" >> "$agno_dir/docs/CLAUDE_merged.md"
                echo -e "\n---\n" >> "$agno_dir/docs/CLAUDE_merged.md"
            fi
            
            find "$backup_path/$source_dir" -name "*.sh" -exec cp {} "$agno_dir/scripts/" \; 2>/dev/null || true
        fi
    done
    
    # monday-integration: 整合测试相关配置
    log_step "配置 monday-integration 环境"
    local integration_dir="$BASE_DIR/monday-integration"
    
    mkdir -p "$integration_dir"/{tests,config,scripts,reports,logs}
    
    # 从备份中恢复测试相关配置
    for source_dir in monday-integration-testing monday-integration; do
        if [[ -d "$backup_path/$source_dir" ]]; then
            if [[ -f "$backup_path/$source_dir/CLAUDE.md" ]]; then
                cat "$backup_path/$source_dir/CLAUDE.md" >> "$integration_dir/docs/CLAUDE_merged.md"
                echo -e "\n---\n" >> "$integration_dir/docs/CLAUDE_merged.md"
            fi
            
            find "$backup_path/$source_dir" -name "*.sh" -exec cp {} "$integration_dir/scripts/" \; 2>/dev/null || true
        fi
    done
    
    log_success "关键配置恢复完成"
}

# 创建统一的CLAUDE.md文档
create_unified_documentation() {
    log_header "📚 创建统一的CLAUDE.md文档"
    
    # monday-rust-hft CLAUDE.md
    cat > "$BASE_DIR/monday-rust-hft/CLAUDE.md" << 'EOF'
# Rust HFT 核心引擎开发环境

## 🦀 **专用环境说明**

此 Worktree (`monday-rust-hft`) 专门用于 **Rust HFT核心引擎开发**，专注于超低延迟交易系统的核心组件。

- **分支**: `feature/rust-hft-unified`
- **专注领域**: 高性能OrderBook、策略引擎、风险管理、市场数据处理
- **开发语言**: Rust + PyO3 (Python绑定)
- **核心目标**: <1μs 决策延迟，零分配算法

## 🏗️ **项目结构**

```
rust_hft/                     # Rust核心代码
├── src/
│   ├── core/                 # 核心组件
│   │   ├── orderbook.rs      # 高性能订单簿
│   │   ├── strategy.rs       # 策略引擎
│   │   └── risk_manager.rs   # 风险管理
│   ├── integrations/         # 外部集成
│   │   ├── bitget_connector.rs # Bitget API
│   │   └── dynamic_bitget_connector.rs
│   ├── ml/                   # ML推理
│   │   ├── inference.rs      # 模型推理
│   │   └── features.rs       # 特征提取
│   └── python_bindings/      # PyO3绑定
│       └── mod.rs            # Python接口
├── benches/                  # 性能基准测试
├── examples/                 # 使用示例
└── Cargo.toml               # 依赖配置

agno_integration/            # Agno集成
├── config/                  # 配置文件
├── scripts/                 # 开发脚本
└── docs/                    # 文档
```

## 🔧 **开发工具**

### 性能优化
```bash
# 性能基准测试
cargo bench --bench decision_latency
cargo bench --bench orderbook_performance

# 内存分析
cargo build --release
valgrind --tool=memcheck ./target/release/rust_hft

# SIMD优化检查
objdump -d target/release/rust_hft | grep -i "simd\|avx\|sse"
```

### PyO3开发
```bash
# 编译Python绑定
maturin develop

# 测试Python集成
python -c "import rust_hft; print(rust_hft.create_orderbook('BTCUSDT'))"
```

## 🎯 **核心指标**

- **决策延迟**: P99 < 1μs
- **吞吐量**: > 100,000 updates/s
- **内存使用**: 零分配热路径
- **CPU效率**: < 80% 单核使用率

EOF

    # monday-agno-framework CLAUDE.md
    cat > "$BASE_DIR/monday-agno-framework/CLAUDE.md" << 'EOF'
# Agno Framework 智能代理开发环境

## 🧠 **专用环境说明**

此 Worktree (`monday-agno-framework`) 专门用于 **Agno智能代理框架开发**，专注于AI驱动的交易决策系统。

- **分支**: `feature/agno-framework-unified`
- **专注领域**: 智能代理、ML流水线、策略开发、AI决策
- **开发语言**: Python + 集成Rust HFT引擎
- **核心目标**: 智能化交易决策，自动化ML生命周期

## 🏗️ **项目结构**

```
agents/                      # 7个智能代理
├── supervisor_agent.py      # 全局调度代理
├── trade_agent.py          # 交易执行代理  
├── risk_agent.py           # 风险控制代理
├── monitor_agent.py        # 系统监控代理
├── train_agent.py          # 模型训练代理
├── config_agent.py         # 配置管理代理
└── chat_agent.py           # 用户交互代理

pipelines/                   # ML流水线
├── data_processing/         # 数据处理
├── feature_engineering/     # 特征工程
├── model_training/          # 模型训练
├── model_evaluation/        # 模型评估
└── deployment/              # 模型部署

strategies/                  # 交易策略
├── momentum/                # 动量策略
├── mean_reversion/          # 均值回归
├── arbitrage/               # 套利策略
└── ml_based/                # ML策略

config/                      # 配置管理
├── agent_config.yaml       # 代理配置
├── strategy_config.yaml    # 策略配置
└── ml_config.yaml          # ML配置
```

## 🤖 **Agent开发**

### 使用Rust HFT功能
```python
import rust_hft

class TradeAgent:
    def __init__(self):
        # 创建Rust OrderBook实例
        self.orderbook_handle = rust_hft.create_orderbook("BTCUSDT")
        
    def process_market_data(self, bids, asks):
        # 使用Rust高性能处理
        rust_hft.update_orderbook(self.orderbook_handle, bids, asks)
        best_bid, best_ask = rust_hft.get_best_bid_ask(self.orderbook_handle)
        return best_bid, best_ask
        
    def generate_signal(self, features):
        # 使用Rust计算交易信号
        signal = rust_hft.calculate_strategy_signal(self.orderbook_handle, features)
        return signal
```

### ML流水线开发
```bash
# 训练新模型
python pipelines/train_btc_lstm.py

# 评估模型性能
python pipelines/evaluate_model.py --model btc_lstm_v1.2

# 部署模型（蓝绿部署）
python pipelines/deploy_model.py --model btc_lstm_v1.2 --strategy blue_green
```

EOF

    # monday-integration CLAUDE.md
    cat > "$BASE_DIR/monday-integration/CLAUDE.md" << 'EOF'
# 集成测试环境

## 🧪 **专用环境说明**

此 Worktree (`monday-integration`) 专门用于 **集成测试和系统验证**，确保Rust HFT引擎和Agno Framework的协同工作。

- **分支**: `feature/integration-unified`
- **专注领域**: 端到端测试、性能验证、系统集成
- **测试类型**: 集成测试、性能测试、压力测试、故障恢复测试
- **核心目标**: 确保系统稳定性和性能要求

## 🏗️ **测试结构**

```
tests/                       # 测试套件
├── unit/                    # 单元测试
├── integration/             # 集成测试
├── performance/             # 性能测试
├── e2e/                     # 端到端测试
└── stress/                  # 压力测试

config/                      # 测试配置
├── test_environments.yaml  # 测试环境配置
├── performance_targets.yaml # 性能目标
└── test_data.yaml          # 测试数据

scripts/                     # 测试脚本
├── run_full_test_suite.sh  # 完整测试套件
├── performance_benchmark.sh # 性能基准测试
└── docker_test_env.sh      # Docker测试环境
```

## 🚀 **测试执行**

### 集成测试
```bash
# Rust-Python集成测试
python tests/integration/test_rust_python_ffi.py

# 端到端交易流程测试
python tests/e2e/test_trading_flow.py

# Agent协同测试
python tests/integration/test_agent_coordination.py
```

### 性能测试
```bash
# 延迟基准测试
./scripts/performance_benchmark.sh --test latency --duration 300

# 吞吐量测试
./scripts/performance_benchmark.sh --test throughput --duration 600

# 压力测试
./scripts/performance_benchmark.sh --test stress --duration 1800
```

EOF

    log_success "统一CLAUDE.md文档创建完成"
}

# 清理旧文件
cleanup_old_files() {
    log_header "🧹 清理旧文件"
    
    # 删除旧的workspace manager
    [[ -f "$BASE_DIR/monday-workspace-manager.sh" ]] && rm "$BASE_DIR/monday-workspace-manager.sh"
    
    # 删除旧的VS Code工作区
    [[ -f "$BASE_DIR/monday-hft-workspace.code-workspace" ]] && rm "$BASE_DIR/monday-hft-workspace.code-workspace"
    
    log_success "旧文件清理完成"
}

# 显示最终状态
show_final_status() {
    log_header "🎉 重构完成"
    
    echo -e "${GREEN}新的Worktree结构：${NC}"
    cd "$MAIN_PROJECT"
    git worktree list
    
    echo -e "\n${CYAN}目录结构：${NC}"
    ls -la "$BASE_DIR" | grep monday
    
    echo -e "\n${YELLOW}下一步：${NC}"
    echo "1. 设置PyO3绑定 (运行: setup-pyo3-bindings.sh)"
    echo "2. 创建新的workspace manager"
    echo "3. 更新VS Code工作区配置"
    echo "4. 验证集成功能"
    
    log_success "Worktree重构完成！"
}

# 主函数
main() {
    log_header "🚀 开始Worktree重构"
    
    echo -e "${YELLOW}重构计划：${NC}"
    echo "• 当前: 8-9个复杂worktree → 目标: 3个统一worktree"
    echo "• monday-rust-hft: Rust核心引擎 + PyO3绑定"
    echo "• monday-agno-framework: Python智能代理框架"  
    echo "• monday-integration: 集成测试环境"
    echo ""
    
    read -p "是否继续重构？ (y/N): " -n 1 -r
    echo
    if [[ ! $REPLY =~ ^[Yy]$ ]]; then
        echo "重构已取消"
        exit 0
    fi
    
    check_current_status
    backup_current_config
    create_new_branches
    remove_old_worktrees
    create_new_worktrees
    restore_key_configs
    create_unified_documentation
    cleanup_old_files
    show_final_status
}

# 错误处理
error_handler() {
    log_error "重构过程中发生错误"
    echo "如需恢复，请查看备份目录: $(cat "$MAIN_PROJECT/.worktree_backup_path" 2>/dev/null || echo "备份路径未找到")"
    exit 1
}

trap error_handler ERR

# 执行主函数
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    main "$@"
fi