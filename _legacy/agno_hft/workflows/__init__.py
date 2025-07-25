"""
Agno Workflows v2 高頻交易工作流系統
====================================

基於 https://docs.agno.com/workflows_2/types_of_workflows 實現的
完整 HFT 交易工作流系統。

核心特性：
- 🧠 5個專業化 AI Agents
- 🔧 5個專業化工具集  
- 📊 2個端到端工作流
- ✅ 4個業務評估器
- 🎛️ 統一工作流管理器

架構組件：
1. **專業化 Agents**：
   - DataAnalyst: 數據分析專家
   - ModelTrainer: 模型訓練專家  
   - TradingStrategist: 交易策略專家
   - RiskManager: 風險管理專家
   - SystemMonitor: 系統監控專家

2. **專業化工具集**：
   - RustHFTTools: Rust 引擎交互
   - DataProcessingTools: 數據處理分析
   - ModelManagementTools: ML 模型管理
   - TradingTools: 交易執行管理
   - MonitoringTools: 系統監控告警

3. **業務工作流**：
   - TrainingWorkflow: TLOB 模型訓練工作流
   - TradingWorkflow: 智能交易執行工作流

4. **智能評估器**：
   - DataQualityEvaluator: 數據質量評估
   - ModelPerformanceEvaluator: 模型性能評估
   - TradingOpportunityEvaluator: 交易機會評估
   - RiskLevelEvaluator: 風險水平評估

使用示例：
```python
from workflows import workflow_manager

# 執行模型訓練工作流
training_result = await workflow_manager.execute_training_workflow(
    "BTCUSDT", 
    {"epochs": 100, "batch_size": 128}
)

# 執行交易工作流
trading_result = await workflow_manager.execute_trading_workflow(
    "BTCUSDT",
    {"prediction_class": 2, "confidence": 0.75}
)

# 監控系統健康
health_result = await workflow_manager.monitor_system_health()
```

版本：2.0 (基於 Agno Workflows v2)
更新：2025-07-22
"""

from .workflow_manager import workflow_manager, WorkflowManager

# 導入所有 Agents
from .agents import (
    DataAnalyst,
    ModelTrainer, 
    TradingStrategist,
    RiskManager,
    SystemMonitor
)

# 導入所有工具集
from .tools import (
    RustHFTTools,
    DataProcessingTools,
    ModelManagementTools,
    TradingTools,
    MonitoringTools
)

# 導入工作流定義 (使用新架構)
from .workflows_v2 import (
    ModelDevelopmentWorkflow as TrainingWorkflow,
    OperationalWorkflow as TradingWorkflow
)

# 導入評估器
from .evaluators import (
    DataQualityEvaluator,
    ModelPerformanceEvaluator,
    TradingOpportunityEvaluator,
    RiskLevelEvaluator
)

__version__ = "2.0.0"
__agno_version__ = "v2"

__all__ = [
    # 工作流管理器
    'workflow_manager',
    'WorkflowManager',
    
    # 專業化 Agents
    'DataAnalyst',
    'ModelTrainer', 
    'TradingStrategist',
    'RiskManager',
    'SystemMonitor',
    
    # 工具集
    'RustHFTTools',
    'DataProcessingTools', 
    'ModelManagementTools',
    'TradingTools',
    'MonitoringTools',
    
    # 工作流定義 (兼容舊版 API)
    'TrainingWorkflow',
    'TradingWorkflow',
    
    # 評估器
    'DataQualityEvaluator',
    'ModelPerformanceEvaluator',
    'TradingOpportunityEvaluator', 
    'RiskLevelEvaluator'
]

# 系統信息
def get_system_info():
    """獲取工作流系統信息"""
    return {
        "name": "Agno HFT Workflows",
        "version": __version__,
        "agno_version": __agno_version__,
        "agents": 5,
        "tools": 5,
        "workflows": 2,
        "evaluators": 4,
        "architecture": "Agno Workflows v2",
        "features": [
            "專業化 AI Agents",
            "智能工作流編排", 
            "條件化評估器",
            "統一工具集成",
            "異步並行處理",
            "實時監控告警"
        ]
    }

# 快速開始函數
async def quick_start_demo():
    """快速開始演示"""
    from .workflow_manager import demo_workflow_execution
    await demo_workflow_execution()