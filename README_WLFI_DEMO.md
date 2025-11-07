# WLFI 完整交易分析系统 Demo

这是一个完整的端到端 WLFI 交易数据分析系统，包含数据查询、清洗、特征工程、机器学习模型训练（XGBoost、GRU、Transformer）以及策略回测功能。

## 🚀 主要功能

### 1. 数据收集与处理
- **ClickHouse 云数据库连接**：自动连接到您的 ClickHouse 云实例
- **多类型市场数据**：支持 L1 BBO、L2 订单簿、交易数据
- **智能数据清洗**：异常值检测、缺失值处理、数据验证
- **高级特征工程**：技术指标、时间特征、市场微结构特征

### 2. 机器学习模型
- **XGBoost 模型**：传统机器学习方法，支持超参数优化
- **GRU 深度学习模型**：循环神经网络，适合时间序列
- **Transformer 模型**：注意力机制，捕捉长期依赖
- **稳定性优化**：自动调整学习率，确保模型稳定性

### 3. 策略回测
- **动量策略**：基于价格动量的交易策略
- **均值回归策略**：基于布林带的均值回归策略  
- **完整回测框架**：包含滑点、手续费、风险控制
- **性能指标**：夏普比率、最大回撤、胜率等

### 4. 报告生成
- **模型性能报告**：详细的模型评估指标
- **回测性能报告**：完整的策略表现分析
- **数据质量报告**：数据完整性和质量评估
- **可视化图表**：训练曲线、回测表现图
- **执行总结**：高层次的结果汇总

## 📦 安装依赖

```bash
pip install -r requirements.txt
```

## 🏃‍♂️ 快速开始

### 方法一：使用运行脚本（推荐）

```bash
# 基本用法
./run_wlfi_demo.sh <your_clickhouse_password>

# 指定交易对和时间范围
./run_wlfi_demo.sh <your_clickhouse_password> WLFI 168

# 参数说明：
# 参数1: ClickHouse 数据库密码（必需）
# 参数2: 交易对符号（可选，默认 WLFI）
# 参数3: 历史数据小时数（可选，默认 168 小时 = 1周）
```

### 方法二：直接运行 Python 脚本

```bash
python3 wlfi_complete_demo.py \
    --password <your_clickhouse_password> \
    --symbol WLFI \
    --hours 168 \
    --host "https://ivigyu08to.ap-northeast-1.aws.clickhouse.cloud:8443"
```

## 📊 输出文件说明

运行完成后，系统会生成以下文件：

### 数据文件
- `raw_*_data.csv` - 原始市场数据
- `processed_*_data.csv` - 处理后的特征数据

### 模型文件
- `xgboost_wlfi_model.joblib` - 训练好的 XGBoost 模型
- `gru_wlfi_model.pth` - 训练好的 GRU 模型
- `transformer_wlfi_model.pth` - 训练好的 Transformer 模型

### 报告文件
- `executive_summary_*.txt` - 📋 **执行总结**（最重要）
- `model_performance_report_*.txt` - 模型性能报告
- `backtest_*_*.txt` - 策略回测报告
- `data_quality_report_*.txt` - 数据质量报告

### 可视化文件
- `*_training_history_*.png` - 模型训练曲线
- `backtest_*_*.png` - 回测性能图表

### 日志文件
- `wlfi_demo_*.log` - 详细执行日志

## 🔧 系统架构

```
┌─────────────────┐    ┌──────────────────┐    ┌─────────────────┐
│   ClickHouse    │───▶│  Data Processing │───▶│  Feature Eng.   │
│   Cloud DB      │    │   & Cleaning     │    │   & Labels      │
└─────────────────┘    └──────────────────┘    └─────────────────┘
                                                          │
┌─────────────────┐    ┌──────────────────┐    ┌─────────────────┐
│   Backtesting   │◀───│  Model Training  │◀───│   Sequences     │
│   & Reports     │    │  (XGBoost/DL)    │    │   Creation      │
└─────────────────┘    └──────────────────┘    └─────────────────┘
```

## 🎯 核心特性

### 数据处理优化
- **时间序列处理**：专门优化的时间序列数据处理流程
- **多步长预测**：可调整的预测时间窗口
- **自动特征选择**：智能选择相关特征
- **数据质量控制**：全面的数据验证和清理

### 模型稳定性增强
- **学习率自适应**：自动调整学习率以提高稳定性
- **早停机制**：防止过拟合
- **梯度裁剪**：深度学习模型的梯度稳定
- **交叉验证**：时间序列专用的验证方法

### 回测系统完善
- **真实交易成本**：包含滑点和手续费
- **风险管理**：仓位限制和风险控制
- **多策略支持**：可扩展的策略框架
- **详细性能分析**：全面的回测指标

## 🚨 注意事项

1. **数据要求**：确保 ClickHouse 中有足够的 WLFI 历史数据
2. **计算资源**：深度学习模型需要较多内存和计算时间
3. **网络连接**：需要稳定的网络连接到 ClickHouse 云服务
4. **Python 环境**：推荐使用 Python 3.8 或更高版本

## 🔍 故障排除

### 连接问题
```bash
# 测试 ClickHouse 连接
curl --user 'default:<your_password>' \
  --data-binary 'SELECT 1' \
  https://ivigyu08to.ap-northeast-1.aws.clickhouse.cloud:8443
```

### 依赖问题
```bash
# 检查关键依赖
python3 -c "import pandas, numpy, xgboost, torch, sklearn"
```

### 内存不足
- 减少 `--hours` 参数值
- 降低深度学习模型的 `batch_size`
- 使用更小的 `sequence_length`

## 📈 性能基准

在典型的 1 周 WLFI 数据上：
- **数据处理**：~2-5 分钟
- **XGBoost 训练**：~1-3 分钟
- **深度学习训练**：~5-15 分钟
- **回测执行**：~1-2 分钟

## 🤝 扩展开发

系统采用模块化设计，易于扩展：

- `db/clickhouse_client.py` - 数据库连接层
- `utils/data_preprocessing.py` - 数据处理层
- `models/` - 机器学习模型层
- `utils/backtesting.py` - 回测引擎层

可以轻松添加：
- 新的数据源
- 新的特征工程方法
- 新的机器学习模型
- 新的交易策略

## 📞 支持

如遇问题，请检查：
1. 生成的日志文件 `wlfi_demo_*.log`
2. 执行总结文件中的错误信息
3. 确认 ClickHouse 连接和数据可用性

---

**祝您使用愉快！** 🎉