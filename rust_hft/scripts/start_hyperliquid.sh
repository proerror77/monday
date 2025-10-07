#!/bin/bash

# Hyperliquid 交易系统启动脚本
# 使用方法: ./scripts/start_hyperliquid.sh [paper|live]

set -e

# 颜色定义
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# 默认模式
MODE=${1:-paper}

echo -e "${BLUE}🚀 Hyperliquid 交易系统启动脚本${NC}"
echo -e "${BLUE}模式: ${MODE}${NC}"

# 检查模式参数
if [ "$MODE" != "paper" ] && [ "$MODE" != "live" ]; then
    echo -e "${RED}❌ 错误: 模式参数必须是 'paper' 或 'live'${NC}"
    echo "使用方法: $0 [paper|live]"
    exit 1
fi

# 检查配置文件
CONFIG_FILE="config/hyperliquid_${MODE}.yaml"
if [ ! -f "$CONFIG_FILE" ]; then
    echo -e "${RED}❌ 错误: 配置文件不存在: $CONFIG_FILE${NC}"
    exit 1
fi

echo -e "${GREEN}✅ 配置文件检查通过: $CONFIG_FILE${NC}"

# Live 模式警告
if [ "$MODE" = "live" ]; then
    echo -e "${RED}⚠️  警告: 您即将启动 Live 模式！${NC}"
    echo -e "${RED}   这将使用真实资金进行交易！${NC}"
    echo -e "${YELLOW}   请确保您已经：${NC}"
    echo -e "${YELLOW}   1. 正确配置了私钥${NC}"
    echo -e "${YELLOW}   2. 设置了合理的风控参数${NC}"
    echo -e "${YELLOW}   3. 充分测试了策略${NC}"
    echo ""
    read -p "是否继续？(yes/no): " confirm
    if [ "$confirm" != "yes" ]; then
        echo -e "${YELLOW}❌ 用户取消操作${NC}"
        exit 0
    fi
fi

# 创建日志目录
mkdir -p logs

# 检查私钥配置（Live 模式）
if [ "$MODE" = "live" ]; then
    echo -e "${BLUE}🔐 检查私钥配置...${NC}"
    if grep -q "YOUR_64_CHAR_HEX_PRIVATE_KEY_HERE" "$CONFIG_FILE"; then
        echo -e "${RED}❌ 错误: 请先在配置文件中设置您的私钥${NC}"
        echo -e "${YELLOW}编辑文件: $CONFIG_FILE${NC}"
        echo -e "${YELLOW}将 'YOUR_64_CHAR_HEX_PRIVATE_KEY_HERE' 替换为您的实际私钥${NC}"
        exit 1
    fi
    echo -e "${GREEN}✅ 私钥配置检查通过${NC}"
fi

# 编译项目
echo -e "${BLUE}🔨 编译项目...${NC}"
if ! cargo check -p hft-execution-adapter-hyperliquid; then
    echo -e "${RED}❌ 编译失败${NC}"
    exit 1
fi
echo -e "${GREEN}✅ 编译成功${NC}"

# 运行测试
echo -e "${BLUE}🧪 运行测试...${NC}"
if ! cargo test -p hft-execution-adapter-hyperliquid; then
    echo -e "${RED}❌ 测试失败${NC}"
    exit 1
fi
echo -e "${GREEN}✅ 测试通过${NC}"

# 显示启动信息
echo ""
echo -e "${GREEN}🎯 启动参数:${NC}"
echo -e "  模式: ${MODE}"
echo -e "  配置: ${CONFIG_FILE}"
echo -e "  日志: logs/hyperliquid_${MODE}.log"
echo ""

# 模拟启动（实际项目中这里会启动真实的交易程序）
echo -e "${BLUE}📈 启动交易系统...${NC}"
echo -e "${YELLOW}注意: 这是演示脚本，实际使用时请替换为您的交易程序启动命令${NC}"

# 示例：运行演示程序
if [ "$MODE" = "paper" ]; then
    echo -e "${BLUE}运行 Paper 模式演示...${NC}"
    # cargo run --example hyperliquid_demo
    echo -e "${GREEN}✅ Paper 模式演示完成${NC}"
else
    echo -e "${RED}Live 模式启动已准备就绪${NC}"
    echo -e "${YELLOW}请手动启动您的交易程序${NC}"
fi

echo ""
echo -e "${GREEN}🎉 启动流程完成！${NC}"
echo ""
echo -e "${BLUE}💡 有用的命令:${NC}"
echo -e "  查看日志: tail -f logs/hyperliquid_${MODE}.log"
echo -e "  停止程序: Ctrl+C"
echo -e "  编辑配置: vim $CONFIG_FILE"