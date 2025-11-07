#!/bin/bash

# WLFI弹性持仓策略训练启动脚本

echo "=========================================="
echo "WLFI弹性持仓策略训练系统"
echo "=========================================="

# 检查Python环境
if ! command -v python3 &> /dev/null; then
    echo "错误: 未找到python3"
    exit 1
fi

# 检查必要的依赖
echo "检查依赖包..."
python3 -c "import torch, pandas, numpy, clickhouse_connect, redis" 2>/dev/null
if [ $? -ne 0 ]; then
    echo "错误: 缺少必要的依赖包"
    echo "请安装: pip install torch pandas numpy clickhouse-connect redis matplotlib seaborn PyYAML"
    exit 1
fi

# 检查ClickHouse连接
echo "检查ClickHouse连接..."
python3 -c "
import clickhouse_connect
try:
    client = clickhouse_connect.get_client(host='localhost', port=8123)
    client.command('SELECT 1')
    print('ClickHouse连接正常')
except Exception as e:
    print(f'ClickHouse连接失败: {e}')
    exit(1)
" || exit 1

# 检查配置文件
CONFIG_FILE="training_config.yaml"
if [ ! -f "$CONFIG_FILE" ]; then
    echo "创建默认配置文件..."
    python3 run_training.py --create-config
fi

echo "开始训练流程..."
echo ""

# 选择训练模式
echo "请选择训练模式:"
echo "1) 完整训练流程 (特征准备 + 模型训练 + 回测验证)"
echo "2) 只准备特征数据"
echo "3) 只训练模型" 
echo "4) 只运行回测验证"
echo ""

read -p "请输入选择 (1-4): " choice

case $choice in
    1)
        echo "运行完整训练流程..."
        python3 run_training.py --config $CONFIG_FILE
        ;;
    2)
        echo "准备特征数据..."
        python3 run_training.py --config $CONFIG_FILE --step 1
        ;;
    3)
        echo "训练模型..."
        python3 run_training.py --config $CONFIG_FILE --step 2
        ;;
    4)
        echo "回测验证..."
        python3 run_training.py --config $CONFIG_FILE --step 3
        ;;
    *)
        echo "无效选择，退出"
        exit 1
        ;;
esac

# 检查执行结果
if [ $? -eq 0 ]; then
    echo ""
    echo "=========================================="
    echo "训练完成! 查看结果:"
    echo "- 模型文件: ./models/"
    echo "- 训练日志: ./logs/"
    echo "- 回测图表: ./plots/"
    echo "=========================================="
else
    echo ""
    echo "=========================================="
    echo "训练失败，请检查日志文件"
    echo "=========================================="
    exit 1
fi